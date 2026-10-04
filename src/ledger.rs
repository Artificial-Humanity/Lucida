//! What was generated, and what became of it.
//!
//! Until this file existed, Lucida remembered nothing. Every render was a
//! transaction with no receipt: no prompt→file trail, no way to list the video
//! operations still in flight, no history, and therefore nowhere for cost to
//! accumulate even in principle. The seed — the one value that makes a render
//! repeatable — was reported as a line on stderr that scrolls away.
//!
//! That is survivable at a terminal, where a human watches each render. It is
//! not survivable for the customer this tool is actually built for. An agent
//! starts a Veo render, hands back an operation id, and its session ends; the id
//! now exists in a transcript nobody will read again, and a paid render is
//! unreachable. `lucida ops` is the answer, and this is what makes it possible.
//!
//! # Shape
//!
//! One JSON object per line, appended, in the config directory. Not a database,
//! not TOML, not a directory of sidecars:
//!
//! - **Append-only** means a write is one syscall and two processes can hold the
//!   file open without coordinating. `O_APPEND` writes of this size do not
//!   interleave on any platform Lucida ships to — *provided the line really is
//!   one `write`*. It was not: `writeln!` of a `serde_json::Value` streams the
//!   value token by token into an unbuffered file, which `strace` shows as a
//!   dozen syscalls per record, so four MCP workers finishing together could
//!   splice their lines into each other and lose both — a paid Veo operation id
//!   among them. [`append`] serialises the whole line first and writes it once.
//! - **One object per line** means a truncated or garbled line costs that line
//!   and nothing else. A JSON *array* would be corrupt as a whole, which for a
//!   record of things you have paid for is the wrong failure.
//! - **Beside `config.env`** because that directory already exists, is already
//!   found on every platform by [`crate::config::search_paths`], and is already
//!   where Lucida keeps things that outlive a process.
//!
//! # It never fails a render
//!
//! Every function here swallows its own errors. A render that has been paid for
//! and written to disk must not be reported as a failure because a log line
//! could not be appended — the ledger is a convenience, and the image is the
//! product. Failures go to stderr once and are otherwise dropped.

use crate::clock;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;

/// Roughly how much history to keep, in bytes.
///
/// A cap rather than unbounded, because nothing else would ever delete this and
/// an agent generating assets in a loop writes a line per render forever. Two
/// megabytes is on the order of ten thousand entries — far more history than
/// anyone will read, and small enough to be beneath notice on any disk.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The smallest share of the lines a prune must drop to be worth rewriting the
/// file for, as a divisor: one in four.
const MIN_PRUNE_SHARE: usize = 4;

/// What a record is about.
pub const IMAGE: &str = "image";
pub const VIDEO: &str = "video";

/// Where the record ended up.
pub const DONE: &str = "done";
/// The provider reported the render as finally failed — rejected, filtered,
/// expired. Retires an operation exactly as `done` does, because there is
/// nothing left to collect; unlike `done` it carries no file, only the reason.
pub const FAILED: &str = "failed";
/// A video render handed back an operation id and nothing has collected it yet.
pub const STARTED: &str = "started";
/// An image the provider returned — and so billed — whose file could not be
/// written. Recorded rather than dropped, because the spend is real whether or
/// not the bytes reached the disk, and the budget is summed from this file.
pub const UNSAVED: &str = "unsaved";
/// An image the provider billed for whose wait ended without it: cancelled,
/// out of time, or a poll or download that failed. Recorded for the same reason
/// as `unsaved` — the spend is real and the budget is summed from this file —
/// and carrying no `operation`, so `lucida ops` never lists it: an image wait
/// does not outlive the call that started it, and there is nothing to collect.
pub const ABANDONED: &str = "abandoned";

/// The ledger file, or `None` if it is switched off or has nowhere to live.
///
/// Beside the config file rather than in a directory of its own: `LUCIDA_CONFIG`
/// may name a file anywhere, and putting the ledger next to whichever config is
/// actually in use keeps "where does Lucida keep its state" a single answer.
///
/// Never under `cfg(test)`. The config path a unit test resolves is the
/// developer's real one, so a render a test reached — and one did, billed —
/// landed in the real ledger and counted against the real budget. Tests of the
/// file itself pass their own path to [`append`] and its neighbours.
pub fn path() -> Option<PathBuf> {
    if cfg!(test) || disabled() {
        return None;
    }
    let config = crate::config::preferred_path()?;
    Some(config.with_file_name("renders.jsonl"))
}

/// Whether the user has switched the ledger off.
///
/// Worth having, and worth `lucida config` reporting: this file records
/// **prompts**, which are the most personal thing Lucida handles, and someone
/// who does not want them on disk should not have to discover the file first.
pub fn disabled() -> bool {
    crate::config::var("LUCIDA_NO_LEDGER").is_some()
}

/// Appends one record. Best-effort by construction — see the module note.
pub fn record(entry: Value) {
    let Some(path) = path() else { return };
    if let Err(e) = append(&path, &entry) {
        // Once, on stderr, and never again: a ledger that reports its own
        // failures on every render is worse than one that quietly stops working,
        // because the noise lands on top of output someone is trying to read.
        eprintln!("note: could not write the render ledger ({}): {e:#}", path.display());
    }
}

/// A finished image, or one that was billed and could not be saved.
///
/// `unsaved` is the write's error when the file never landed. The entry is
/// written either way: once the provider has returned the image it has been
/// billed, and an entry that waited for the write left a full disk or an
/// unwritable path with a paid render the budget never counted.
pub fn image(
    provider: &str,
    model: &str,
    prompt: &str,
    path: &str,
    seed: Option<u64>,
    estimated_usd: f64,
    unsaved: Option<&anyhow::Error>,
) {
    record(image_entry(provider, model, prompt, path, seed, estimated_usd, unsaved));
}

fn image_entry(
    provider: &str,
    model: &str,
    prompt: &str,
    path: &str,
    seed: Option<u64>,
    estimated_usd: f64,
    unsaved: Option<&anyhow::Error>,
) -> Value {
    let mut entry = json!({
        "at": clock::now(),
        "kind": IMAGE,
        "status": if unsaved.is_some() { UNSAVED } else { DONE },
        "provider": provider,
        "model": model,
        "prompt": prompt,
        // Where it was written, or for an unsaved image where it was meant to be.
        "path": path,
        "seed": seed,
        // An estimate, never a charge — the provider's invoice is the authority.
        // Recorded per entry rather than summed anywhere, so the rolling budget
        // window is derived from the log like everything else here.
        "estimated_usd": estimated_usd,
    });
    if let Some(error) = unsaved {
        entry["error"] = json!(summarise(&format!("{error:#}")));
    }
    entry
}

/// An image billed and never returned, from the marker its provider put on the
/// error.
///
/// Without it a BFL or Runway render whose wait was cancelled or ran out wrote
/// nothing: an MCP client that hung up mid-render and retried was billed twice
/// and counted once.
pub fn abandoned_image(
    provider: &str,
    model: &str,
    prompt: &str,
    abandoned: &crate::provider::Abandoned,
    estimated_usd: f64,
) {
    record(abandoned_entry(provider, model, prompt, abandoned, estimated_usd));
}

pub(crate) fn abandoned_entry(
    provider: &str,
    model: &str,
    prompt: &str,
    abandoned: &crate::provider::Abandoned,
    estimated_usd: f64,
) -> Value {
    json!({
        "at": clock::now(),
        "kind": IMAGE,
        "status": ABANDONED,
        "provider": provider,
        "model": model,
        "prompt": prompt,
        // BFL's polling URL or Runway's task id — the one handle on what was
        // paid for. Deliberately not `operation`: that field is what `ops`
        // lists and what `done` retires, and an image has neither.
        "handle": abandoned.handle,
        "estimated_usd": estimated_usd,
        "error": summarise(&abandoned.to_string()),
    })
}

/// A video render that has been started and not yet collected.
///
/// The entry `lucida ops` is built on, and the reason this module exists: the
/// operation id is the only way back to a render that is already being billed.
pub fn video_started(
    provider: &str,
    model: &str,
    prompt: &str,
    operation: &str,
    estimated_usd: f64,
) {
    record(started_entry(provider, model, prompt, operation, estimated_usd));
}

fn started_entry(
    provider: &str,
    model: &str,
    prompt: &str,
    operation: &str,
    estimated_usd: f64,
) -> Value {
    json!({
        "at": clock::now(),
        "kind": VIDEO,
        "status": STARTED,
        // The backend that was actually used. This was the literal "google" for
        // every render, so a Runway or Kling operation showed up in `lucida ops`
        // and `history` as Veo.
        "provider": provider,
        "model": model,
        "prompt": prompt,
        "operation": operation,
        // Charged at the moment the render starts, not when it is collected —
        // which is why the estimate rides on this entry rather than on the
        // `done` one. A render started and never collected still cost money.
        "estimated_usd": estimated_usd,
    })
}

/// A video that has been downloaded, which is what retires an operation from
/// `lucida ops`.
pub fn video_done(provider: &str, operation: &str, path: &str) {
    record(done_entry(provider, operation, path));
}

fn done_entry(provider: &str, operation: &str, path: &str) -> Value {
    json!({
        "at": clock::now(),
        "kind": VIDEO,
        "status": DONE,
        "provider": provider,
        "operation": operation,
        "path": path,
    })
}

/// A video the provider has said it will never deliver.
///
/// Without this a render that failed or expired stayed in `lucida ops` forever,
/// listed as waiting to be collected and answering every `check` with the same
/// error. [`note_failure`] decides what counts as final; this is the record.
fn failed_entry(provider: &str, operation: &str, error: &str) -> Value {
    json!({
        "at": clock::now(),
        "kind": VIDEO,
        "status": FAILED,
        "provider": provider,
        "operation": operation,
        "error": summarise(error),
    })
}

/// Retires `operation` if `error` is a failure the provider reported as final.
///
/// Only [`crate::video::TerminalFailure`] counts. A transport error, a 5xx or a
/// deadline says nothing about the render, which may well still finish, and
/// retiring it on one would hide a paid render that `lucida ops` exists to find.
pub fn note_failure(provider: &str, operation: &str, error: &anyhow::Error) {
    if let Some(entry) = terminal_entry(provider, operation, error) {
        record(entry);
    }
}

/// The `failed` record for `error`, or `None` when the error is not final.
/// Split from [`note_failure`] so the decision can be tested without writing to
/// the machine's real ledger.
fn terminal_entry(provider: &str, operation: &str, error: &anyhow::Error) -> Option<Value> {
    let failure = error.downcast_ref::<crate::video::TerminalFailure>()?;
    Some(failed_entry(provider, operation, &failure.0))
}

/// The first line of an error, bounded: the ledger is read line by line and
/// capped by size, and a provider's failure body can run to pages.
fn summarise(error: &str) -> String {
    const LIMIT: usize = 300;
    let first = error.lines().next().unwrap_or("").trim();
    match first.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{}…", &first[..cut]),
        None => first.to_string(),
    }
}

/// The provider a ledger entry recorded, if it can be trusted.
///
/// Entries written before the provider was real all say `google`, whatever
/// started them. A `google` entry whose operation id is not Veo-shaped is one of
/// those, and returning it would make `lucida ops` print a `--provider google`
/// command that is wrong for the render in front of it, where the plain
/// `lucida check <id>` it printed before infers the right one. So that case
/// reads as unrecorded. No entry without a `provider` field gets one invented.
pub fn recorded_provider(entry: &Value) -> Option<&str> {
    let provider = entry["provider"].as_str()?;
    if provider == "google" {
        let operation = entry["operation"].as_str().unwrap_or_default();
        if crate::provider::infer_video_backend_from_operation(operation)
            != crate::provider::VideoBackend::Google
        {
            return None;
        }
    }
    Some(provider)
}

/// Every record, oldest first. An unreadable file reads as no history rather
/// than as an error, for the same reason writes are best-effort.
pub fn entries() -> Vec<Value> {
    let Some(path) = path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Operations with a `done` or `failed` record: nothing is left to collect.
fn retired(all: &[Value]) -> std::collections::HashSet<String> {
    all.iter()
        .filter(|e| e["status"] == DONE || e["status"] == FAILED)
        .filter_map(|e| e["operation"].as_str().map(str::to_string))
        .collect()
}

/// Video operations that were started and never collected.
///
/// Computed from the log rather than stored as state, so there is nothing to go
/// out of sync: an operation is outstanding exactly when it has a `started`
/// record and no `done` or `failed` one. A render collected from a different
/// shell, or by an agent, therefore disappears from here without anything
/// having to be told.
pub fn outstanding() -> Vec<Value> {
    outstanding_from(entries())
}

fn outstanding_from(all: Vec<Value>) -> Vec<Value> {
    let collected = retired(&all);

    let mut seen = std::collections::HashSet::new();
    all.into_iter()
        .filter(|e| e["status"] == STARTED)
        .filter(|e| {
            e["operation"]
                .as_str()
                .is_some_and(|op| !collected.contains(op) && seen.insert(op.to_string()))
        })
        .collect()
}

fn append(path: &std::path::Path, entry: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Checked before the write rather than after, so a prune that does free
    // space frees it before this record lands. The cap is a target, not a
    // guarantee: what may be dropped is limited to what the spend window and
    // outstanding renders do not need, and when that is little the file stays
    // over the cap (see `prune`).
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        prune(path);
    }

    // The whole record, built before the file is touched. `writeln!(file, "{entry}")`
    // looked like one write and was a dozen: serde_json's `Display` streams token
    // by token into an unbuffered `File`, and `O_APPEND` only keeps *each syscall*
    // whole. Concurrent writers interleaved mid-record, both lines failed to parse
    // and `entries()` dropped them — a paid operation id and its spend, gone.
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');

    let mut options = std::fs::OpenOptions::new();
    // Readable too, for one byte: see the torn-tail check below.
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        // The ledger holds prompts. Applies only when this call creates the
        // file, so a file whose permissions the user chose is left alone.
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;

    // A crash mid-write leaves a last line with no newline, and the next record
    // would be joined onto it, costing both. Starting on a fresh line costs only
    // the torn one — and an empty line, which reads as nothing.
    if ends_mid_line(&mut file) {
        line.insert(0, '\n');
    }

    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Whether the file is non-empty and its last byte is not a newline.
///
/// An unreadable tail reads as "fine": a stray blank line is cheaper than a
/// ledger that refuses to record because it could not look first.
fn ends_mid_line(file: &mut std::fs::File) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1))
        .and_then(|_| file.read_exact(&mut last))
        .is_ok()
        && last[0] != b'\n'
}

/// Drops old history when the file outgrows its cap.
///
/// Up to the oldest half of the lines rather than one, so pruning happens rarely
/// instead of on every write once the cap is reached. It used to drop *exactly*
/// the oldest half, which under bulk use reached into the spend window and
/// deleted entries the budget is computed from, and into video starts that were
/// still waiting to be collected. Now it drops, oldest first, only what neither
/// of those needs; if that is less than half, or nothing, the file stays over
/// the cap, which is best-effort as it always was.
///
/// A prune that could drop only a sliver is skipped too: [`append`] calls this
/// on every write once the file is over the cap, so one that rewrote the whole
/// file to shed a line or two would turn every later append into a full read,
/// parse and atomic rewrite — and each rewrite is a window in which a concurrent
/// append, perhaps a `started` operation id, is lost. A rewrite has to be worth
/// that, so it happens only when it drops at least [`MIN_PRUNE_SHARE`] of the
/// lines; until then the file simply runs over the cap.
///
/// Written atomically, and best-effort: a concurrent append during the rewrite
/// could be lost, which is a real race and an acceptable one — the alternative
/// is a lock file, and a lock file that outlives a crash would stop the ledger
/// recording anything at all. Losing a line of history is recoverable; refusing
/// to record is not.
fn prune(path: &std::path::Path) {
    prune_at(path, clock::now());
}

fn prune_at(path: &std::path::Path, now: i64) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let parsed: Vec<Option<Value>> = lines
        .iter()
        .map(|l| serde_json::from_str(l).ok())
        .collect();

    let values: Vec<Value> = parsed.iter().flatten().cloned().collect();
    let retired = retired(&values);
    let cutoff = now - crate::spend::WINDOW_SECONDS;

    // Entries the spend window is computed from, and starts nobody has collected.
    // A line that does not parse is neither: it has no timestamp to protect and
    // no operation id to lose.
    let protected = |entry: &Option<Value>| -> bool {
        let Some(e) = entry else { return false };
        let young = e["at"].as_i64().unwrap_or(0) >= cutoff;
        let waiting = e["status"] == STARTED
            && e["operation"].as_str().is_some_and(|op| !retired.contains(op));
        young || waiting
    };

    let budget = lines.len() / 2;
    let mut drop = vec![false; lines.len()];
    let mut dropped = 0;
    for (i, entry) in parsed.iter().enumerate() {
        if dropped == budget {
            break;
        }
        if !protected(entry) {
            drop[i] = true;
            dropped += 1;
        }
    }

    // A `started` line that survives must keep the record that retired it, or the
    // operation would reappear in `lucida ops` as waiting.
    let kept_starts: std::collections::HashSet<&str> = parsed
        .iter()
        .zip(&drop)
        .filter(|(_, d)| !**d)
        .filter_map(|(e, _)| e.as_ref())
        .filter(|e| e["status"] == STARTED)
        .filter_map(|e| e["operation"].as_str())
        .collect();
    for (entry, d) in parsed.iter().zip(drop.iter_mut()) {
        let Some(e) = entry else { continue };
        let retires = e["status"] == DONE || e["status"] == FAILED;
        if *d && retires && e["operation"].as_str().is_some_and(|op| kept_starts.contains(op)) {
            *d = false;
        }
    }

    // Counted after the un-drop pass above, which can only shrink it.
    let dropping = drop.iter().filter(|d| **d).count();
    if dropping == 0 || dropping < lines.len() / MIN_PRUNE_SHARE {
        return;
    }

    let keep: Vec<&str> = lines
        .iter()
        .zip(&drop)
        .filter(|(_, d)| !**d)
        .map(|(l, _)| *l)
        .collect();
    // Private: the ledger holds prompts, and the rewrite would otherwise replace
    // a 0600 file with one at the umask's default.
    let _ = crate::write_atomically(path, format!("{}\n", keep.join("\n")).as_bytes(), true);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ledger's own path is environment-dependent, so these drive the pure
    /// parts directly against a temporary file.
    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);

        // A counter, not the clock: two tests starting in the same second shared
        // a directory, and the first one's cleanup deleted the second one's file
        // out from under it.
        let dir = std::env::temp_dir().join(format!(
            "lucida-ledger-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("renders.jsonl")
    }

    fn read(path: &std::path::Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[test]
    fn entries_append_one_line_each() {
        let path = temp();
        for n in 0..3 {
            append(&path, &json!({ "n": n })).unwrap();
        }
        let written = read(&path);
        assert_eq!(written.len(), 3);
        assert_eq!(written[0]["n"], 0, "oldest must be first");
        assert_eq!(written[2]["n"], 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A garbled line costs that line and nothing else — the reason this is
    /// JSONL rather than one JSON array, since an array would be corrupt whole.
    #[test]
    fn a_damaged_line_does_not_cost_the_history_around_it() {
        let path = temp();
        std::fs::write(
            &path,
            "{\"n\":1}\nthis is not json\n{\"n\":2}\n{\"n\":3}\n",
        )
        .unwrap();

        let survived = read(&path);
        assert_eq!(survived.len(), 3);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// An image that was billed and could not be written still carries its
    /// spend, because the budget is summed from these entries and the provider
    /// charged for the render whatever happened to the disk afterwards.
    #[test]
    fn an_unsaved_image_still_records_its_spend() {
        let error = anyhow::anyhow!("writing out.png: No space left on device");
        let entry = image_entry("google", "m", "p", "out.png", None, 0.134, Some(&error));
        assert_eq!(entry["status"], UNSAVED);
        assert_eq!(entry["estimated_usd"], 0.134);
        assert_eq!(entry["path"], "out.png");
        assert!(entry["error"].as_str().unwrap().contains("No space left"), "{entry}");

        let saved = image_entry("google", "m", "p", "out.png", None, 0.134, None);
        assert_eq!(saved["status"], DONE);
        assert!(saved.get("error").is_none(), "{saved}");
    }

    /// Outstanding operations are derived, not stored, so a render collected
    /// anywhere — another shell, an agent, `lucida check` — drops off the list
    /// without anything having to be told.
    #[test]
    fn a_collected_render_is_no_longer_outstanding() {
        let open = outstanding_from(vec![
            json!({ "kind": VIDEO, "status": STARTED, "operation": "operations/a" }),
            json!({ "kind": VIDEO, "status": STARTED, "operation": "operations/b" }),
            json!({ "kind": VIDEO, "status": DONE, "operation": "operations/a" }),
        ]);

        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["operation"], "operations/b");
    }

    /// A render the provider reported as finally failed has nothing left to
    /// collect, so it leaves the list exactly as a collected one does. It used
    /// to stay there forever, answering every `check` with the same error.
    /// An abandoned image carries no `operation`, so `ops` has nothing to list:
    /// an image wait does not outlive its call, and nothing is left to collect.
    #[test]
    fn an_abandoned_image_is_never_outstanding() {
        let abandoned = json!({
            "kind": IMAGE, "status": ABANDONED, "handle": "img-7", "estimated_usd": 0.08,
        });
        let open = outstanding_from(vec![
            abandoned,
            json!({ "kind": VIDEO, "status": STARTED, "operation": "operations/a" }),
        ]);
        assert_eq!(open.len(), 1, "{open:?}");
        assert_eq!(open[0]["operation"], "operations/a");
    }

    #[test]
    fn a_failed_render_is_no_longer_outstanding() {
        let open = outstanding_from(vec![
            json!({ "kind": VIDEO, "status": STARTED, "operation": "operations/a" }),
            json!({ "kind": VIDEO, "status": STARTED, "operation": "operations/b" }),
            json!({ "kind": VIDEO, "status": FAILED, "operation": "operations/a", "error": "x" }),
        ]);

        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["operation"], "operations/b");
    }

    #[test]
    fn the_entries_carry_the_provider_that_was_used() {
        let started = started_entry("runway", "gen4_turbo", "a fox", "4f1a2b3c", 0.5);
        assert_eq!(started["provider"], "runway");
        assert_eq!(started["status"], STARTED);

        assert_eq!(done_entry("kling", "915468728228253726", "/tmp/x.mp4")["provider"], "kling");
    }

    /// Entries from before the provider was recorded say `google` for everything,
    /// so a `google` entry on an id Veo would never issue is not evidence, and a
    /// missing field stays missing.
    #[test]
    fn a_provider_is_trusted_only_when_it_can_be_true() {
        let entry = |provider: Value, operation: &str| {
            json!({ "kind": VIDEO, "status": STARTED, "provider": provider, "operation": operation })
        };
        let uuid = "4f1a2b3c-0000-4000-8000-000000000000";

        assert_eq!(recorded_provider(&entry(json!("runway"), uuid)), Some("runway"));
        assert_eq!(recorded_provider(&entry(json!("google"), "operations/abc")), Some("google"));
        assert_eq!(
            recorded_provider(&entry(json!("google"), uuid)),
            None,
            "a legacy Runway entry, mislabelled google"
        );
        assert_eq!(
            recorded_provider(&json!({ "kind": VIDEO, "operation": "operations/abc" })),
            None,
            "no field, no answer"
        );
    }

    /// The failure's reason is kept, but only its first line and a bounded
    /// length: a provider's error body can run to pages.
    #[test]
    fn an_error_summary_is_one_bounded_line() {
        assert_eq!(summarise("the render failed: nope\nlong detail"), "the render failed: nope");
        let long = "é".repeat(1000);
        let summary = summarise(&long);
        assert!(summary.chars().count() <= 301, "{}", summary.chars().count());
        assert!(summary.ends_with('…'));
    }

    /// Pruning keeps the newest half. The oldest entries are the ones nobody
    /// wants, and dropping half at a time means this runs rarely rather than on
    /// every write once the cap is reached.
    #[test]
    fn pruning_keeps_the_newest_half() {
        let path = temp();
        for n in 0..10 {
            append(&path, &json!({ "n": n })).unwrap();
        }
        prune(&path);

        let left = read(&path);
        assert_eq!(left.len(), 5);
        assert_eq!(left[0]["n"], 5, "the newest half must survive");
        assert_eq!(left[4]["n"], 9);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// What `note_failure` writes, against a temporary file: nothing for an error
    /// that is not final, and exactly one record that retires the operation for
    /// one that is — including when `check` has wrapped it in context on the way
    /// up, as `poll` does.
    #[test]
    fn only_a_terminal_error_leaves_a_failed_record() {
        use anyhow::Context;
        let note = |path: &std::path::Path, error: &anyhow::Error| {
            if let Some(entry) = terminal_entry("runway", "op-1", error) {
                append(path, &entry).unwrap();
            }
        };

        let path = temp();
        let transport = anyhow::anyhow!("connection reset").context("polling the Runway task");
        let deadline = anyhow::anyhow!("gave up after 15 minutes");
        note(&path, &transport);
        note(&path, &deadline);
        assert!(read(&path).is_empty(), "a non-final error wrote a record");

        let terminal: anyhow::Error = Err::<(), _>(crate::video::terminal("the render failed: moderation"))
            .context("polling the render")
            .unwrap_err();
        note(&path, &terminal);

        let written = read(&path);
        assert_eq!(written.len(), 1, "{written:?}");
        assert_eq!(written[0]["status"], FAILED);
        assert_eq!(written[0]["provider"], "runway");
        assert_eq!(written[0]["operation"], "op-1");
        assert_eq!(written[0]["error"], "the render failed: moderation");

        let open = outstanding_from(vec![
            json!({ "kind": VIDEO, "status": STARTED, "operation": "op-1" }),
            written[0].clone(),
        ]);
        assert!(open.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The line is one `write`, and so lands whole however many writers there
    /// are. `writeln!` of the value streamed it token by token: four MCP workers
    /// finishing together spliced their lines into each other, both failed to
    /// parse, and a paid operation id was gone.
    #[test]
    fn two_appends_are_two_whole_lines() {
        let path = temp();
        append(&path, &json!({ "operation": "operations/a" })).unwrap();
        append(&path, &json!({ "operation": "operations/b" })).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text:?}");
        for (line, want) in lines.iter().zip(["operations/a", "operations/b"]) {
            let parsed: Value = serde_json::from_str(line).expect("a line that does not parse");
            assert_eq!(parsed["operation"], want);
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn concurrent_appends_never_splice_each_other() {
        let path = temp();
        let writers: Vec<_> = (0..8)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for n in 0..50 {
                        // Long enough that a record cannot be one lucky buffer.
                        let entry = json!({ "w": w, "n": n, "prompt": "x".repeat(2000) });
                        append(&path, &entry).unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        // Blank lines are allowed: a writer that glimpses another's record
        // half-written sees no newline yet and starts on a fresh line, which
        // costs an empty line and nothing else.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().filter(|l| !l.is_empty()).count(), 400);
        assert_eq!(read(&path).len(), 400, "a line was spliced and dropped");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A crash mid-write leaves a last line with no newline. The next record
    /// must start on its own line, or it is joined onto the stump and both are
    /// lost; as it is, only the torn one is.
    #[test]
    fn a_torn_last_line_costs_only_itself() {
        let path = temp();
        std::fs::write(&path, "{\"n\":1}\n{\"n\":2,\"pro").unwrap();

        append(&path, &json!({ "n": 3 })).unwrap();

        let survived = read(&path);
        assert_eq!(survived.len(), 2, "{survived:?}");
        assert_eq!(survived[0]["n"], 1);
        assert_eq!(survived[1]["n"], 3, "the new record was joined onto the stump");

        // And an intact file gets no stray blank line.
        append(&path, &json!({ "n": 4 })).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("\n\n"), "{text:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    const DAY: i64 = 24 * 60 * 60;

    /// Writes `entries` one per line and prunes as of `now`.
    fn pruned(entries: &[Value], now: i64) -> Vec<Value> {
        let path = temp();
        let text: String = entries.iter().map(|e| format!("{e}\n")).collect();
        std::fs::write(&path, text).unwrap();
        prune_at(&path, now);
        let left = read(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        left
    }

    /// The spend window is computed from the ledger, so pruning into it would
    /// quietly raise the budget. Half the lines are *not* dropped when the other
    /// half is too young to lose.
    #[test]
    fn pruning_never_drops_an_entry_inside_the_spend_window() {
        let now = 10 * DAY;
        let mut entries: Vec<Value> = (0..4)
            .map(|n| json!({ "at": now - 5 * DAY + n, "n": n, "estimated_usd": 1.0 }))
            .collect();
        entries.extend((4..10).map(|n| json!({ "at": now - 60 + n, "n": n, "estimated_usd": 1.0 })));

        let left = pruned(&entries, now);

        // Ten lines: up to five may go, but only the four old ones are old.
        let kept: Vec<i64> = left.iter().map(|e| e["n"].as_i64().unwrap()).collect();
        assert_eq!(kept, vec![4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn pruning_never_drops_a_video_still_waiting_to_be_collected() {
        let now = 10 * DAY;
        let old = now - 5 * DAY;
        let entries = vec![
            json!({ "at": old, "kind": VIDEO, "status": STARTED, "operation": "operations/waiting" }),
            json!({ "at": old + 1, "kind": VIDEO, "status": STARTED, "operation": "operations/collected" }),
            json!({ "at": old + 2, "kind": VIDEO, "status": DONE, "operation": "operations/collected" }),
            json!({ "at": old + 3, "kind": VIDEO, "status": STARTED, "operation": "operations/failed" }),
            json!({ "at": old + 4, "kind": VIDEO, "status": FAILED, "operation": "operations/failed" }),
            json!({ "at": old + 5, "n": 5 }),
            json!({ "at": old + 6, "n": 6 }),
            json!({ "at": old + 7, "n": 7 }),
        ];

        let left = pruned(&entries, now);

        assert!(
            left.iter().any(|e| e["operation"] == "operations/waiting"),
            "an outstanding render was pruned: {left:?}"
        );
        // Oldest first, and only what is allowed: of the four that may go (half
        // of eight) the waiting start is spared and the next four go instead.
        assert!(!left.iter().any(|e| e["operation"] == "operations/collected"));
        assert_eq!(left.len(), 4, "{left:?}");
        assert_eq!(left[0]["operation"], "operations/waiting");
    }

    /// A retiring record outlives nothing it should not: if the start line is
    /// kept, the `done` that retired it is kept with it, or the operation would
    /// reappear in `lucida ops` as waiting.
    #[test]
    fn a_start_that_survives_keeps_the_record_that_retired_it() {
        let now = 10 * DAY;
        let old = now - 5 * DAY;
        let entries = vec![
            json!({ "at": old, "n": 0 }),
            json!({ "at": old + 1, "kind": VIDEO, "status": DONE, "operation": "operations/a" }),
            // Out of order, as two processes' clocks can leave a file.
            json!({ "at": old + 9, "kind": VIDEO, "status": STARTED, "operation": "operations/a" }),
            json!({ "at": now, "n": 3 }),
        ];

        let left = pruned(&entries, now);

        let open = outstanding_from(left);
        assert!(open.is_empty(), "a retired operation came back: {open:?}");
    }

    /// If nothing may be dropped the file is left alone — over the cap, as the
    /// module note says, rather than rewritten smaller at the cost of history
    /// that is still needed.
    #[test]
    fn pruning_leaves_the_file_alone_when_everything_is_needed() {
        let now = 10 * DAY;
        let entries: Vec<Value> = (0..6).map(|n| json!({ "at": now - n, "n": n })).collect();
        assert_eq!(pruned(&entries, now).len(), 6);
    }

    /// Over the cap with almost nothing old enough to drop, `append` calls this on
    /// every write. A rewrite for one line would make each of those appends a
    /// full read-parse-rewrite and a chance to lose a concurrent record, so the
    /// file is left exactly as it is until a prune is worth doing.
    #[test]
    fn a_prune_that_could_drop_one_line_leaves_the_file_untouched() {
        let now = 10 * DAY;
        let path = temp();
        let mut entries = vec![json!({ "at": now - 5 * DAY, "n": 0 })];
        entries.extend((1..12).map(|n| json!({ "at": now - n, "n": n })));
        let text: String = entries.iter().map(|e| format!("{e}\n")).collect();
        std::fs::write(&path, &text).unwrap();

        prune_at(&path, now);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The same file once enough of it has aged out does get rewritten.
    #[test]
    fn a_prune_that_drops_a_quarter_is_worth_the_rewrite() {
        let now = 10 * DAY;
        let mut entries: Vec<Value> =
            (0..3).map(|n| json!({ "at": now - 5 * DAY + n, "n": n })).collect();
        entries.extend((3..12).map(|n| json!({ "at": now - n, "n": n })));
        assert_eq!(pruned(&entries, now).len(), 9);
    }

    #[cfg(unix)]
    mod permissions {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn mode(path: &std::path::Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        /// The ledger holds prompts, and umask would make it group-readable.
        #[test]
        fn a_new_ledger_is_private() {
            let path = temp();
            append(&path, &json!({ "n": 1 })).unwrap();
            assert_eq!(mode(&path), 0o600);
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }

        /// Permissions the user chose are theirs: appending must not tighten
        /// (or loosen) a file it did not create.
        #[test]
        fn an_existing_ledger_keeps_the_permissions_it_has() {
            let path = temp();
            std::fs::write(&path, "{\"n\":0}\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

            append(&path, &json!({ "n": 1 })).unwrap();

            assert_eq!(mode(&path), 0o644);
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }

        /// The rewrite replaces the file, so it has to bring the privacy with it.
        #[test]
        fn a_pruned_ledger_is_private_again() {
            let path = temp();
            let lines: String = (0..10).map(|n| format!("{{\"at\":1,\"n\":{n}}}\n")).collect();
            std::fs::write(&path, lines).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

            prune_at(&path, 10 * DAY);

            assert_eq!(read(&path).len(), 5);
            assert_eq!(mode(&path), 0o600);
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }

    /// A ledger write must never be the thing that fails a render that has
    /// already been paid for.
    ///
    /// The unwritable path is a *directory* standing where the file belongs,
    /// which no platform will open for appending. An earlier version used
    /// `/proc/self/mem/...`, which is unwritable on Linux and merely an ordinary
    /// relative path on Windows — where `create_dir_all` obligingly made it and
    /// the assertion failed on the CI lane that exists for exactly this.
    #[test]
    fn a_ledger_write_that_cannot_succeed_is_still_not_an_error_for_the_caller() {
        let path = temp();
        std::fs::create_dir_all(&path).unwrap();

        assert!(append(&path, &json!({ "n": 1 })).is_err());

        // `record` is deliberately NOT called here. It resolves the *real*
        // ledger path from the config search path, so a test that calls it
        // appends junk to whichever ledger belongs to the machine running the
        // suite — which this test did, and which showed up as three `{"n":1}`
        // lines in the developer's own history. A test must not write outside
        // its temporary directory. What `record` adds over `append` is one
        // `if let Err` that discards, and that is visible by reading it.

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
