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
//!   interleave on any platform Lucida ships to.
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

/// What a record is about.
pub const IMAGE: &str = "image";
pub const VIDEO: &str = "video";

/// Where the record ended up.
pub const DONE: &str = "done";
/// A video render handed back an operation id and nothing has collected it yet.
pub const STARTED: &str = "started";

/// The ledger file, or `None` if it is switched off or has nowhere to live.
///
/// Beside the config file rather than in a directory of its own: `LUCIDA_CONFIG`
/// may name a file anywhere, and putting the ledger next to whichever config is
/// actually in use keeps "where does Lucida keep its state" a single answer.
pub fn path() -> Option<PathBuf> {
    if disabled() {
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

/// A finished image.
pub fn image(provider: &str, model: &str, prompt: &str, path: &str, seed: Option<u64>) {
    record(json!({
        "at": clock::now(),
        "kind": IMAGE,
        "status": DONE,
        "provider": provider,
        "model": model,
        "prompt": prompt,
        "path": path,
        "seed": seed,
    }));
}

/// A video render that has been started and not yet collected.
///
/// The entry `lucida ops` is built on, and the reason this module exists: the
/// operation id is the only way back to a render that is already being billed.
pub fn video_started(model: &str, prompt: &str, operation: &str) {
    record(json!({
        "at": clock::now(),
        "kind": VIDEO,
        "status": STARTED,
        "provider": "google",
        "model": model,
        "prompt": prompt,
        "operation": operation,
    }));
}

/// A video that has been downloaded, which is what retires an operation from
/// `lucida ops`.
pub fn video_done(operation: &str, path: &str) {
    record(json!({
        "at": clock::now(),
        "kind": VIDEO,
        "status": DONE,
        "provider": "google",
        "operation": operation,
        "path": path,
    }));
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

/// Video operations that were started and never collected.
///
/// Computed from the log rather than stored as state, so there is nothing to go
/// out of sync: an operation is outstanding exactly when it has a `started`
/// record and no `done` one. A render collected from a different shell, or by an
/// agent, therefore disappears from here without anything having to be told.
pub fn outstanding() -> Vec<Value> {
    let all = entries();

    let collected: std::collections::HashSet<String> = all
        .iter()
        .filter(|e| e["status"] == DONE)
        .filter_map(|e| e["operation"].as_str().map(str::to_string))
        .collect();

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

    // Checked before the write rather than after, so the file is never larger
    // than the cap plus one line.
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        prune(path);
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{entry}")?;
    Ok(())
}

/// Drops the oldest half when the file outgrows its cap.
///
/// Half rather than one line, so pruning happens rarely instead of on every
/// write once the cap is reached. Written atomically, and best-effort: a
/// concurrent append during the rewrite could be lost, which is a real race and
/// an acceptable one — the alternative is a lock file, and a lock file that
/// outlives a crash would stop the ledger recording anything at all. Losing a
/// line of history is recoverable; refusing to record is not.
fn prune(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    let keep = lines.split_at(lines.len() / 2).1.join("\n");
    let _ = crate::write_atomically(path, format!("{keep}\n").as_bytes(), false);
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

    /// Outstanding operations are derived, not stored, so a render collected
    /// anywhere — another shell, an agent, `lucida check` — drops off the list
    /// without anything having to be told.
    #[test]
    fn a_collected_render_is_no_longer_outstanding() {
        let all = [
            json!({ "kind": VIDEO, "status": STARTED, "operation": "operations/a" }),
            json!({ "kind": VIDEO, "status": STARTED, "operation": "operations/b" }),
            json!({ "kind": VIDEO, "status": DONE, "operation": "operations/a" }),
        ];

        let collected: std::collections::HashSet<String> = all
            .iter()
            .filter(|e| e["status"] == DONE)
            .filter_map(|e| e["operation"].as_str().map(str::to_string))
            .collect();
        let open: Vec<&Value> = all
            .iter()
            .filter(|e| e["status"] == STARTED)
            .filter(|e| !collected.contains(e["operation"].as_str().unwrap()))
            .collect();

        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["operation"], "operations/b");
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

    /// A ledger write must never be the thing that fails a render that has
    /// already been paid for. Pointed at a path that cannot exist, `record`
    /// returns normally.
    #[test]
    fn a_broken_ledger_path_does_not_propagate() {
        let impossible = std::path::Path::new("/proc/self/mem/nope/renders.jsonl");
        assert!(append(impossible, &json!({ "n": 1 })).is_err());
        // `record` is what callers use, and it swallows exactly that error.
    }
}
