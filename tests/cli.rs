//! The shipped binary, as a black box.
//!
//! Everything in `src/**/tests` compiles *inside* the crate: it can reach
//! private functions, which is what makes it good at logic and blind to
//! everything a user actually touches. Exit codes, `--json` on stdout alone,
//! the config search path resolved from a real environment, JSON-RPC framing
//! over a real pipe — none of those exist until there is a process.
//!
//! Those assertions lived in `scripts/smoke.sh` and ran only in CI, in bash,
//! after a separate release build. So the layer most likely to break for
//! someone installing Lucida was the one layer `cargo test` did not cover, and
//! the one a developer could not run before committing. They live here now, and
//! `scripts/smoke.sh` runs *this file* against the shipped artifact.
//!
//! ## Which binary
//!
//! `LUCIDA_TEST_BIN` if set, otherwise the one cargo just built. That is what
//! lets the release workflow point these same assertions at the musl static
//! build and the fused universal binary — artifacts `cargo test` never produces
//! and which have their own ways of being broken.
//!
//! ## Two rules for anything added here
//!
//! **Nothing may reach a provider.** Every command below is a refusal, a local
//! read, or a connection to a port chosen because nothing is listening on it. A
//! test that renders costs money on every machine that ever runs it.
//!
//! **Every process gets its own empty environment.** `env_clear`, a private
//! `HOME`, and no credentials — so a key on the developer's machine cannot turn
//! an assertion into a no-op, and the assertions run identically on the laptop
//! and on CI. `env -i` is also the exact condition the config file exists for: a
//! GUI-launched client inherits no shell environment and passes that emptiness
//! to the MCP server it spawns.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// The binary under test — cargo's, unless something points elsewhere.
fn binary() -> PathBuf {
    match std::env::var_os("LUCIDA_TEST_BIN") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_BIN_EXE_lucida")),
    }
}

/// A private `HOME` for one test, removed when it ends.
///
/// Tests run in parallel threads of one process, so a shared directory would
/// have them writing each other's config file. The name carries the test's own
/// label to make a leaked directory attributable, plus pid and nanoseconds
/// because two runs can overlap — the same construction `comfy::unique_upload_name`
/// uses, and for the same reason.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Sandbox {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.subsec_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("lucida-cli-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).expect("could not create a sandbox directory");
        Sandbox { dir }
    }

    /// Where `config::search_paths` will look first, on every platform.
    ///
    /// With `XDG_CONFIG_HOME` unset the first entry falls back to
    /// `home()/.config`, and `home()` resolves from `HOME` or `USERPROFILE` —
    /// so this one spelling is correct on Linux, macOS and Windows alike.
    fn config_file(&self) -> PathBuf {
        self.dir.join(".config").join("lucida").join("config.env")
    }

    fn write_config(&self, contents: &str) {
        let path = self.config_file();
        fs::create_dir_all(path.parent().unwrap()).expect("could not create the config directory");
        fs::write(&path, contents).expect("could not write the config file");
    }

    /// The unique component of the directory name.
    ///
    /// Compared instead of the full path because Git Bash on Windows hands the
    /// test a Unix-style `/tmp/...` while the native binary correctly prints
    /// `C:\Users\...\Temp\...` — two spellings of one directory, which a
    /// substring match on the whole path calls a failure.
    fn name(&self) -> String {
        self.dir.file_name().unwrap().to_string_lossy().into_owned()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A command with no inherited environment beyond what the OS needs to run a
/// process at all.
///
/// Not a bare `env_clear()`: Windows needs `SYSTEMROOT` before a socket can be
/// opened, and one test below deliberately opens one. Restoring the minimum
/// keeps the isolation while leaving the platform functional — nothing in the
/// restored set is anything Lucida reads.
fn lucida(sandbox: &Sandbox) -> Command {
    let mut cmd = Command::new(binary());
    cmd.env_clear();

    for key in ["PATH", "SYSTEMROOT", "SystemRoot", "TEMP", "TMP", "COMSPEC"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }

    // Both spellings, because `config::home()` reads `HOME` first and
    // `USERPROFILE` only where there is none — and a test that set just one
    // would pass on one platform for a reason unrelated to what it asserts.
    cmd.env("HOME", &sandbox.dir)
        .env("USERPROFILE", &sandbox.dir);
    cmd
}

/// What a finished process said, in a shape that makes a failure legible.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    argv: String,
}

impl Run {
    /// Both streams, for assertions that do not care which one carried the
    /// message. Where the split *is* the point — `--json` putting nothing but a
    /// document on stdout — the test reads `stdout` directly.
    fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    #[track_caller]
    fn says(&self, needle: &str) -> &Run {
        assert!(
            self.output().contains(needle),
            "expected {needle:?} in the output of `{}`\n--- exit {} ---\n{}",
            self.argv,
            self.code,
            self.output()
        );
        self
    }

    #[track_caller]
    fn never_says(&self, needle: &str) -> &Run {
        assert!(
            !self.output().contains(needle),
            "{needle:?} should not appear in the output of `{}`\n--- exit {} ---\n{}",
            self.argv,
            self.code,
            self.output()
        );
        self
    }

    #[track_caller]
    fn exits(&self, want: i32) -> &Run {
        assert_eq!(
            self.code,
            want,
            "`{}` exited {} rather than {want}\n{}",
            self.argv,
            self.code,
            self.output()
        );
        self
    }
}

fn run(cmd: &mut Command) -> Run {
    run_with_stdin(cmd, "")
}

fn run_with_stdin(cmd: &mut Command, stdin: &str) -> Run {
    let argv = format!(
        "lucida {}",
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    );

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", binary().display()));

    // Closed by the drop, which is what tells the MCP server its input has
    // ended and lets it exit rather than blocking this test forever.
    {
        let mut pipe = child.stdin.take().expect("stdin was piped");
        match pipe.write_all(stdin.as_bytes()) {
            Ok(()) => {}

            // The child exited without reading its input, and for some of these
            // commands that *is* the behaviour under test: `config --set` with a
            // retired name refuses before it ever prompts, so it can be gone
            // before this write lands. Whether it wins that race depends on
            // scheduling, which is why this passed locally and on two of three
            // CI platforms before failing on the third.
            //
            // Not a blanket ignore. Anything other than a closed pipe is a fault
            // in this harness and still panics — and the exit code and output
            // are collected below either way, so a child that wrongly ignored
            // its input is still caught by the assertion that follows.
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}

            Err(e) => panic!("could not write stdin: {e}"),
        }
    }

    let done = child
        .wait_with_output()
        .expect("the process never finished");
    Run {
        // A signalled process has no code. Reported as -1 rather than unwrapped
        // so a crash shows up as a failed assertion with the output attached,
        // instead of as a panic inside the harness.
        code: done.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&done.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&done.stderr).into_owned(),
        argv,
    }
}

// --- it runs at all ---------------------------------------------------------

#[test]
fn it_runs_and_reports_the_version_it_was_built_from() {
    let sandbox = Sandbox::new("version");
    let out = run(lucida(&sandbox).arg("--version"));

    out.exits(0);
    assert_eq!(
        out.stdout.trim(),
        format!("lucida {}", env!("CARGO_PKG_VERSION")),
        "the binary under test is not the one this checkout describes"
    );
}

#[test]
fn help_succeeds() {
    let sandbox = Sandbox::new("help");
    run(lucida(&sandbox).arg("--help")).exits(0);
}

// --- missing credentials ----------------------------------------------------

#[test]
fn a_missing_key_explains_itself_rather_than_panicking() {
    let sandbox = Sandbox::new("nokey");
    let out = run(lucida(&sandbox).arg("models"));

    out.says("no API key found")
        // The message has to point at the fix for the case that actually bites:
        // a process with no shell environment, which is this one.
        .says("lucida config")
        .never_says("panicked");
}

#[test]
fn capabilities_print_without_a_credential() {
    let sandbox = Sandbox::new("caps-nokey");
    let out = run(lucida(&sandbox).arg("models"));

    // Whether Google has a seed is not a fact about your credentials. This
    // command used to return the moment a client could not be built, so the one
    // answer that needed no key was the one you could not get without one.
    out.says("This provider supports:").says("output carries");
}

// --- config file resolution -------------------------------------------------

#[test]
fn config_init_writes_into_a_bare_home() {
    let sandbox = Sandbox::new("init");
    let out = run(lucida(&sandbox).args(["config", "--init"]));

    out.says(&sandbox.name()).says("config.env");
    assert!(
        sandbox.config_file().exists(),
        "config --init reported a path it did not create"
    );
}

#[test]
fn the_config_file_is_read_with_no_environment_at_all() {
    // The regression this guards is subtle and was real until 0.3.0: an MCP
    // server launched by a GUI application has no shell environment, so an
    // exported key was invisible with no way to recover.
    let sandbox = Sandbox::new("file-read");
    sandbox.write_config("GEMINI_API_KEY=cli-test-value\n");

    run(lucida(&sandbox).arg("config"))
        .says("GEMINI_API_KEY")
        .says("set (config file)");
}

#[test]
fn the_config_file_beats_the_environment_and_names_what_it_beat() {
    let sandbox = Sandbox::new("file-wins");
    sandbox.write_config("GEMINI_API_KEY=cli-test-value\n");

    // Precedence this way round is what makes a key scoped to Lucida reachable
    // at all when the shell already exports a broader one. It was the other way
    // through v0.5.2; see `config::var` for why it changed.
    run(lucida(&sandbox)
        .env("GEMINI_API_KEY", "from-the-environment")
        .arg("config"))
    .says("set (config file)")
    // The losing source must be named rather than merely out-ranked.
    // Whoever is reading this output is usually asking why the key they
    // exported is not being used.
    .says("not used — the config file wins");
}

#[test]
fn config_never_prints_a_value() {
    let sandbox = Sandbox::new("no-values");
    sandbox.write_config("GEMINI_API_KEY=cli-test-value\n");

    // This output is meant to be safe to paste into a bug report.
    run(lucida(&sandbox)
        .env("GEMINI_API_KEY", "from-the-environment")
        .arg("config"))
    .never_says("cli-test-value")
    .never_says("from-the-environment");
}

// --- a renamed setting ------------------------------------------------------

#[test]
fn a_retired_key_name_names_its_replacement() {
    // Someone holding GOOGLE_API_KEY has a key that is present and correct, so
    // "no API key found" would send them to check the one thing not wrong.
    let sandbox = Sandbox::new("retired");
    run(lucida(&sandbox).env("GOOGLE_API_KEY", "x").arg("config"))
        .says("no longer read")
        .says("GEMINI_API_KEY");
}

#[test]
fn a_completed_migration_says_nothing_about_the_old_name() {
    // A permanent notice about a non-problem is one people learn to skip past.
    let sandbox = Sandbox::new("migrated");
    run(lucida(&sandbox)
        .env("GOOGLE_API_KEY", "x")
        .env("GEMINI_API_KEY", "y")
        .arg("config"))
    .never_says("no longer read");
}

#[test]
fn config_set_refuses_a_retired_name() {
    // Writing a value nothing reads is exactly the silent drop this product
    // exists to refuse.
    let sandbox = Sandbox::new("set-retired");
    run_with_stdin(
        lucida(&sandbox).args(["config", "--set", "GOOGLE_API_KEY"]),
        "v\n",
    )
    .says("no longer read")
    .says("--set GEMINI_API_KEY");
}

// --- removing a setting -----------------------------------------------------

#[test]
fn config_remove_deletes_a_setting_and_says_so_when_there_was_none() {
    // `--remove` exists so changing a key does not mean remembering where the
    // file is.
    let sandbox = Sandbox::new("remove");
    sandbox.write_config("GEMINI_API_KEY=cli-test-value\n");

    run(lucida(&sandbox).args(["config", "--remove", "GEMINI_API_KEY"]))
        .says("Removed GEMINI_API_KEY");

    run(lucida(&sandbox).arg("config"))
        .says("GEMINI_API_KEY")
        .says("not set");

    // Idempotent, but never silent — it is a typo often enough to be worth
    // saying out loud.
    run(lucida(&sandbox).args(["config", "--remove", "GEMINI_API_KEY"])).says("nothing to remove");
}

// --- capability guards ------------------------------------------------------
//
// Runnable with no credentials and no server, which is the point. If any of
// these ever starts reporting a missing key instead, the check has moved back
// behind client construction and the message has become useless.

#[test]
fn an_unsupported_seed_names_a_provider_that_has_one() {
    let sandbox = Sandbox::new("seed");
    run(lucida(&sandbox).args(["generate", "x", "--seed", "1"]))
        .says("no concept of a seed")
        .says("comfyui")
        .never_says("no API key found");
}

#[test]
fn an_unsupported_aspect_ratio_is_rejected() {
    let sandbox = Sandbox::new("aspect");
    run(lucida(&sandbox).args(["generate", "x", "--aspect", "7:3"]))
        .says("supports only these aspect ratios");
}

#[test]
fn a_refused_mask_names_the_provider_whose_mask_binds() {
    // v0.9.0 made one provider's mask binding and six of the seven surfaces
    // describing it went on saying "advisory" — including the probe an agent is
    // told to believe. Every surface now reads one enum, so checking a single
    // one through a fresh process checks the mechanism.
    //
    // "advisory" appearing here is correct rather than a regression: the
    // refusal describes both kinds so the caller can choose, and the difference
    // is usually the reason to prefer one. What must not happen is a refusal
    // that offers only the weaker guarantee — so the assertion is that the
    // binding one is named, not that the word "advisory" is absent.
    //
    // The bash version of this check could not tell those apart. It was an
    // ordered `case` whose "only advisory" arm sat *after* the arm that
    // matched, so it had been unreachable for as long as both words appeared.
    let sandbox = Sandbox::new("mask");
    run(lucida(&sandbox).args(["generate", "x", "--mask", "m.png"]))
        .says("comfyui")
        .says("the mask is binding");
}

#[test]
fn a_seeded_batch_is_refused() {
    // `--seed` pins one image and `--count` asks for several: together they
    // render the same picture N times and bill for each.
    let sandbox = Sandbox::new("seeded-batch");
    run(lucida(&sandbox).args([
        "generate",
        "x",
        "--provider",
        "comfyui",
        "--seed",
        "5",
        "--count",
        "3",
    ]))
    .exits(2)
    .says("same picture");
}

// --- a provider that is not there -------------------------------------------

#[test]
fn an_unreachable_comfyui_explains_itself() {
    // The likeliest failure for the local lane by a wide margin. Port 1 is
    // chosen because nothing listens there — no provider is contacted, and the
    // connection is refused immediately rather than timing out.
    let sandbox = Sandbox::new("comfy-down");
    run(lucida(&sandbox)
        .env("LUCIDA_COMFYUI_URL", "http://127.0.0.1:1")
        .args(["models", "--provider", "comfyui"]))
    .says("could not reach ComfyUI")
    .says("LUCIDA_COMFYUI_URL")
    .never_says("panicked");
}

// --- exit codes and --json --------------------------------------------------

#[test]
fn a_capability_refusal_exits_2_and_an_ordinary_error_exits_1() {
    // Everything used to exit 0 or 1, collapsing outcomes a caller has to tell
    // apart. A refusal is not a failure: retrying it cannot succeed, so a
    // wrapper needs to see a different number.
    let sandbox = Sandbox::new("codes");

    run(lucida(&sandbox).args(["generate", "x", "--provider", "google", "--seed", "5"])).exits(2);
    run(lucida(&sandbox).args(["generate", "x", "--provider", "nonsense"])).exits(1);
}

#[test]
fn json_reports_a_refusal_as_one_document() {
    // One object on stdout whatever happens, including on failure — a caller
    // parsing output should not have to switch parsers depending on the outcome.
    let sandbox = Sandbox::new("json-refusal");
    let out = run(lucida(&sandbox).args([
        "--json",
        "generate",
        "x",
        "--provider",
        "google",
        "--seed",
        "5",
    ]));

    out.exits(2);
    assert!(
        out.stdout.contains("\"refused\":true"),
        "no refusal document on stdout:\n{}",
        out.stdout
    );
}

#[test]
fn json_writes_json_alone_to_stdout() {
    // Prose on stdout is prose in the parser's input.
    let sandbox = Sandbox::new("json-clean");
    let out = run(lucida(&sandbox).args(["--json", "ops"]));
    let doc = out.stdout.trim();

    assert!(
        doc.starts_with('{') && doc.ends_with('}'),
        "stdout was not a bare JSON object:\n{doc}"
    );
}

// --- a dry run sends nothing ------------------------------------------------

#[test]
fn a_dry_run_reports_its_plan() {
    // The flag exists because confirming "does --provider X use X's own model?"
    // used to require a render, and the answer cost money three separate times.
    let sandbox = Sandbox::new("dry-plan");
    run(lucida(&sandbox).args([
        "generate",
        "x",
        "--provider",
        "comfyui",
        "--dry-run",
        "--json",
    ]))
    .says("\"status\":\"dry-run\"");
}

#[test]
fn a_dry_run_still_refuses_what_a_real_run_would() {
    // Otherwise it is a different code path that happens to be free, and it
    // confirms nothing about the run you were about to pay for.
    let sandbox = Sandbox::new("dry-refusal");
    run(lucida(&sandbox).args([
        "generate",
        "x",
        "--provider",
        "google",
        "--seed",
        "5",
        "--dry-run",
    ]))
    .exits(2);
}

// --- the render ledger ------------------------------------------------------
//
// Checked out of a real process because the ledger's location is resolved at
// runtime from the config search path, which no unit test exercises.

#[test]
fn ops_reports_an_empty_ledger() {
    let sandbox = Sandbox::new("ops-empty");
    run(lucida(&sandbox).arg("ops")).says("No video renders are waiting");
}

#[test]
fn config_names_the_ledger_and_it_can_be_switched_off() {
    let sandbox = Sandbox::new("ledger-visible");

    // Named out loud because this file records prompts, and someone who does
    // not want them on disk should not have to find it first to learn it exists.
    run(lucida(&sandbox).arg("config")).says("Render ledger:");

    run(lucida(&sandbox).env("LUCIDA_NO_LEDGER", "1").arg("ops")).says("LUCIDA_NO_LEDGER");
}

// --- the MCP stdio transport ------------------------------------------------
//
// Worth exercising separately from the CLI: framing breaks in ways no ordinary
// command would reveal, and line endings are the plausible culprit on Windows.

const TOOLS_LIST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

fn mcp(sandbox: &Sandbox, request: &str) -> Run {
    run_with_stdin(lucida(sandbox).arg("mcp"), request)
}

#[test]
fn tools_list_answers_over_stdio() {
    let sandbox = Sandbox::new("mcp-list");
    let out = mcp(&sandbox, &format!("{TOOLS_LIST}\n"));

    for tool in [
        "generate_image",
        "image_providers",
        "start_video",
        "check_video",
        "video_providers",
        "list_operations",
    ] {
        assert!(
            out.stdout.contains(tool),
            "{tool} is missing from tools/list:\n{}",
            out.stdout
        );
    }
}

/// The providers `generate_image` offers, read off the wire.
fn schema_providers(out: &Run) -> Vec<String> {
    let doc: serde_json::Value =
        serde_json::from_str(out.stdout.trim()).expect("tools/list must return one JSON document");

    let tools = doc["result"]["tools"]
        .as_array()
        .expect("tools/list must return an array of tools");
    let image = tools
        .iter()
        .find(|t| t["name"] == "generate_image")
        .expect("generate_image must be listed");

    image["inputSchema"]["properties"]["provider"]["enum"]
        .as_array()
        .expect("provider must arrive as a closed set")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_provider_the_schema_offers_is_named_in_the_help_text() {
    // Deliberately not a list of provider names. `src/mcp.rs` asserts the schema
    // against `Backend::ALL`, which it can see and this file cannot — so
    // repeating the names here would only add a sixth place to forget one.
    //
    // What this can do from outside is hold two *different* surfaces against
    // each other. The schema's enum is generated; the `--provider` help text is
    // a hand-written doc comment in `src/main.rs`, and hand-written provider
    // lists in this repository have gone stale every single time. So the
    // generated set is the expectation and the prose has to keep up with it.
    let sandbox = Sandbox::new("mcp-providers");
    let offered = schema_providers(&mcp(&sandbox, &format!("{TOOLS_LIST}\n")));

    assert!(
        !offered.is_empty(),
        "the schema offered no providers at all"
    );

    let help = run(lucida(&sandbox).args(["generate", "--help"]));
    let entry = flag_help(&help.stdout, "--provider <PROVIDER>");

    for provider in &offered {
        assert!(
            entry.contains(provider),
            "`{provider}` can be selected over MCP but the `--provider` help does \
             not name it — the hand-written list in src/main.rs has gone stale.\n\
             the enum offers {offered:?}, and --provider says:\n{entry}"
        );
    }
}

/// The help text belonging to one flag, and nothing else.
///
/// Scoped deliberately. Matching the whole `--help` output is what the first
/// version of the above did, and it could not fail: `--seed` mentions openai
/// too ("google and openai have none"), so deleting openai from the `--provider`
/// list left the assertion passing on an unrelated line. An assertion that
/// cannot fail is worse than no assertion, because it reads as coverage.
fn flag_help<'a>(help: &'a str, flag: &str) -> &'a str {
    let start = help
        .find(flag)
        .unwrap_or_else(|| panic!("`{flag}` is not in the help output:\n{help}"));
    let rest = &help[start + flag.len()..];

    // clap separates entries with a blank line.
    match rest.find("\n\n") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

#[test]
fn a_crlf_terminated_request_is_parsed() {
    // A client on Windows may terminate requests with CRLF.
    let sandbox = Sandbox::new("mcp-crlf");
    mcp(&sandbox, &format!("{TOOLS_LIST}\r\n")).says("generate_image");
}

#[test]
fn the_server_never_emits_a_carriage_return() {
    // Newline framing is part of the JSON-RPC contract, so a "helpful" CRLF
    // would corrupt the stream for every client.
    let sandbox = Sandbox::new("mcp-lf");
    let out = mcp(&sandbox, &format!("{TOOLS_LIST}\n"));

    assert!(
        !out.stdout.contains('\r'),
        "the server emitted CR in its output"
    );
}

#[test]
fn a_notification_draws_no_response() {
    let sandbox = Sandbox::new("mcp-notify");
    let out = mcp(
        &sandbox,
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    );

    assert!(
        out.stdout.trim().is_empty(),
        "replied to a notification:\n{}",
        out.stdout
    );
}

// --- the file this replaced -------------------------------------------------

#[test]
fn the_smoke_script_delegates_here_rather_than_asserting_twice() {
    // These assertions are only worth moving if there is exactly one copy of
    // them. `scripts/smoke.sh` runs the release artifact through this file; if
    // someone re-adds bash assertions there, the two copies drift and the one
    // that runs less often is the one that goes stale.
    //
    // Read from the manifest directory so this holds wherever the tests run.
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/smoke.sh");
    let body = fs::read_to_string(&script).expect("scripts/smoke.sh should exist");

    assert!(
        body.contains("--test cli"),
        "smoke.sh no longer runs this file against the shipped artifact"
    );
    assert!(
        body.contains("LUCIDA_TEST_BIN"),
        "smoke.sh runs the tests without pointing them at the artifact it was given"
    );
}
