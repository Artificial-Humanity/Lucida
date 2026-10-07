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

    // The Lemonade lane needs no credential, and its default address is a
    // loopback port where a real server may be listening on the machine running
    // the suite. Pointed at nothing here, in the one constructor every process
    // test goes through, so no test can open that lane by forgetting a line: a
    // test that wants a server overrides this with a stand-in of its own.
    cmd.env("LUCIDA_LEMONADE_URL", NOWHERE);
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

#[test]
fn a_model_that_needs_a_reference_is_refused_before_the_missing_key() {
    // `gen4_image_turbo` cannot render from text. That objection is real with or
    // without a key, so it must win over "no Runway API key found" — and exit 2,
    // since no retry can succeed.
    let sandbox = Sandbox::new("turbo-noref");
    run(lucida(&sandbox).args(["generate", "x", "--model", "gen4_image_turbo"]))
        .says("reference image")
        .never_says("no Runway API key")
        .exits(2);
}

#[test]
fn the_canary_can_find_where_googles_video_half_starts() {
    // scripts/canary.sh cuts `models --provider google` at this line to check
    // only the image default. Reworded, the cut finds nothing, the Veo aliases'
    // own `(default)` stays in, and that check passes whatever is listed.
    let sandbox = Sandbox::new("google-video-half");
    run(lucida(&sandbox).args(["models", "--provider", "google"]))
        .says("Video models available");
}

#[test]
fn the_canary_can_find_which_credentials_are_set() {
    // scripts/canary.sh decides which providers to probe by grepping
    // `lucida config` for `^ +NAME +set`. Reworded or re-columned, the grep
    // finds nothing, every keyed provider is skipped, and the canary used to
    // report "no drift detected" having probed none of them. It now refuses a
    // run that probed nothing, but a layout change should fail here first.
    //
    // Both directions are pinned: a key that is set must match, and one that is
    // not must not — `not set` would otherwise read as set and send the canary
    // after a provider it has no credential for.
    let sandbox = Sandbox::new("canary-config-layout");
    sandbox.write_config("GEMINI_API_KEY=cli-test-value\n");

    let shown = run(lucida(&sandbox).arg("config"));
    shown.exits(0);

    // `^ +NAME +set`, by hand: this crate has no regex dependency.
    let canary_sees_set = |name: &str| {
        shown.stdout.lines().any(|line| {
            let indented = line.trim_start_matches(' ');
            line.len() > indented.len()
                && indented
                    .strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix(' '))
                    .is_some_and(|rest| rest.trim_start_matches(' ').starts_with("set"))
        })
    };

    assert!(
        canary_sees_set("GEMINI_API_KEY"),
        "the canary's `^ +GEMINI_API_KEY +set` no longer matches a key that is set:\n{}",
        shown.stdout
    );
    assert!(
        !canary_sees_set("BFL_API_KEY"),
        "the canary's `^ +BFL_API_KEY +set` now matches a key that is NOT set:\n{}",
        shown.stdout
    );
}

#[test]
fn a_provider_of_both_media_lists_both() {
    // `runway` was a video provider only, and `lucida models --provider runway`
    // listed its video models. Now that it renders images too, the image lane
    // must not hide the video one that command has always shown.
    let sandbox = Sandbox::new("runway-both");
    run(lucida(&sandbox).args(["models", "--provider", "runway"]))
        .says("gen4_image")
        .says("gen4.5")
        .never_says("panicked");
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

#[test]
fn a_count_of_zero_is_refused_before_anything_runs() {
    // `--count 0` used to succeed, rendering nothing and exiting 0 — which a
    // script reads as a batch that worked.
    let sandbox = Sandbox::new("count-zero");
    run(lucida(&sandbox).args(["generate", "x", "--provider", "comfyui", "--count", "0"]))
        .exits(2)
        .says("--count");
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

// --- `--json` on a command with no document is refused -----------------------
//
// `out.rs` promises one JSON object on stdout whatever happens. These five print
// prose with `println!`, so `--json` used to produce a stream that was not JSON
// at all and a caller's parser failed on the first word. A flag that cannot be
// honoured is refused, and before any work: `config --init` writes a file, and
// `update` goes to the network.

#[test]
fn json_is_refused_by_a_command_with_no_json_document() {
    let sandbox = Sandbox::new("json-refused");
    for (command, extra) in [
        ("models", &["--provider", "comfyui"][..]),
        ("config", &[][..]),
        ("skill", &[][..]),
        ("setup", &["--dry-run"][..]),
        ("update", &["--check"][..]),
    ] {
        let mut args = vec!["--json", command];
        args.extend_from_slice(extra);
        let out = run(lucida(&sandbox).args(&args));
        out.exits(2);

        let doc: serde_json::Value = serde_json::from_str(out.stdout.trim())
            .unwrap_or_else(|e| panic!("`{}` wrote no JSON document ({e}):\n{}", out.argv, out.stdout));
        assert_eq!(doc["ok"], false, "{}", out.argv);
        assert_eq!(doc["refused"], true, "{}", out.argv);
        assert_eq!(doc["exit_code"], 2, "{}", out.argv);
        let message = doc["error"].as_str().unwrap_or_default();
        assert!(
            message.contains(&format!("`lucida {command}`")),
            "the refusal does not name the command: {message}"
        );
    }
}

#[test]
fn a_refused_json_command_does_none_of_its_work() {
    // `config --init` writes the config file; if the refusal came after it, the
    // caller would be told "refused, nothing done" about a file that now exists.
    let sandbox = Sandbox::new("json-no-work");
    run(lucida(&sandbox).args(["--json", "config", "--init"])).exits(2);
    assert!(
        !sandbox.config_file().exists(),
        "`config --init --json` was refused after writing the config file"
    );
}

#[test]
fn json_still_works_where_there_is_a_document() {
    // The refusal is a list of the five that have none, not a rule against the
    // flag: the commands that do have a document must keep it.
    let sandbox = Sandbox::new("json-kept");
    for command in [&["ops"][..], &["history"][..]] {
        let mut args = vec!["--json"];
        args.extend_from_slice(command);
        run(lucida(&sandbox).args(&args)).exits(0);
    }
}

// --- a closed pipe is not a crash ---------------------------------------------

#[test]
fn skill_into_a_closed_pipe_is_not_a_panic() {
    // `lucida skill | head -1` closes the pipe after one line. The reader is
    // dropped before the child can write, so the write fails with a broken pipe;
    // the old `print!` turned that into a panic and exit 101.
    let sandbox = Sandbox::new("skill-pipe");
    let mut child = lucida(&sandbox)
        .arg("skill")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not run lucida");
    drop(child.stdout.take());
    let done = child.wait_with_output().expect("the process never finished");
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert_eq!(done.status.code(), Some(0), "a closed pipe ended it:\n{stderr}");
    assert!(!stderr.contains("panicked"), "a closed pipe panicked:\n{stderr}");
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
fn a_dry_run_reports_every_resolved_image_parameter() {
    // "Every resolved parameter" is what the flag's help promises, and a field
    // missing here is a parameter a caller cannot confirm before paying.
    let sandbox = Sandbox::new("dry-image-fields");
    let out = run(lucida(&sandbox).args([
        "generate",
        "x",
        "--provider",
        "comfyui",
        "--negative",
        "blurry",
        "--steps",
        "12",
        "--guidance",
        "4.5",
        "--workflow",
        "graph.json",
        "--dry-run",
        "--json",
    ]));
    out.exits(0);
    let doc: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(doc["negative_prompt"], "blurry");
    assert_eq!(doc["steps"], 12);
    assert_eq!(doc["guidance"], 4.5);
    assert_eq!(doc["workflow"], "graph.json");

    // A mask only means something on an edit, so it is checked on one.
    let edit = run(lucida(&sandbox).args([
        "edit",
        "photo.png",
        "x",
        "--provider",
        "comfyui",
        "--mask",
        "mask.png",
        "--dry-run",
        "--json",
    ]));
    edit.exits(0);
    let doc: serde_json::Value = serde_json::from_str(edit.stdout.trim()).unwrap();
    assert_eq!(doc["mask"], "mask.png");
    assert_eq!(doc["references"], serde_json::json!(["photo.png"]));

    // And a field nobody set is present and null, so the document has one shape.
    let bare = run(lucida(&sandbox).args(["generate", "x", "--provider", "comfyui", "--dry-run", "--json"]));
    let doc: serde_json::Value = serde_json::from_str(bare.stdout.trim()).unwrap();
    for field in ["negative_prompt", "mask", "workflow", "steps", "guidance"] {
        assert!(doc.get(field).is_some_and(|v| v.is_null()), "`{field}` is absent or set: {doc}");
    }
}

/// `--guidance 7.1` is reported as 7.1, not as the f64 its f32 widens to.
///
/// `json!` on an `f32` widens it first, and the widened value is
/// 7.099999904632568 — true to the bits and not what anyone typed, in the one
/// document whose job is to show a caller what they asked for.
#[test]
fn a_dry_run_reports_guidance_as_it_was_written() {
    let sandbox = Sandbox::new("dry-guidance");
    let out = run(lucida(&sandbox).args([
        "generate", "x", "--provider", "comfyui", "--guidance", "7.1", "--dry-run", "--json",
    ]));
    out.exits(0).says("\"guidance\":7.1").never_says("7.0999");
}

#[test]
fn a_dry_run_reports_a_video_resolution_and_negative_prompt() {
    let sandbox = Sandbox::new("dry-video-fields");
    let out = run(lucida(&sandbox).args([
        "video",
        "x",
        "--provider",
        "google",
        "--resolution",
        "720p",
        "--negative",
        "text",
        "--dry-run",
        "--json",
    ]));
    out.exits(0);
    let doc: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(doc["resolution"], "720p");
    assert_eq!(doc["negative_prompt"], "text");
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

// --- a budget that cannot be enforced refuses -------------------------------
//
// Each of these used to remove the cap without a word: the value was read as no
// budget at all, and every render went through. Driven as dry runs, because a
// dry run refuses exactly what a real run would and sends nothing.

#[test]
fn an_unreadable_budget_refuses_a_paid_render_and_names_the_value() {
    for written in ["$5", "5 USD", "NaN", "inf", "-1"] {
        let sandbox = Sandbox::new("budget-unreadable");
        run(lucida(&sandbox)
            .env("LUCIDA_BUDGET", written)
            .args(["generate", "x", "--provider", "google", "--dry-run"]))
        .exits(2)
        .says(&format!("`{written}`"))
        .says("such as `5`");
    }
}

#[test]
fn an_unreadable_budget_never_refuses_the_local_lane() {
    let sandbox = Sandbox::new("budget-unreadable-free");
    run(lucida(&sandbox)
        .env("LUCIDA_BUDGET", "$5")
        .args(["generate", "x", "--provider", "comfyui", "--dry-run"]))
    .exits(0);
}

#[test]
fn config_flags_an_unreadable_budget() {
    let sandbox = Sandbox::new("budget-config");
    sandbox.write_config("LUCIDA_BUDGET=$5\n");
    run(lucida(&sandbox).arg("config"))
        .says("LUCIDA_BUDGET  (`$5`")
        .says("such as `5`");
}

/// `budget_usd` is `null` for an unreadable budget as well as for none, and
/// read alone that says "no cap" while every paid render is refused. The
/// problem field is what tells the two apart.
#[test]
fn history_json_names_an_unreadable_budget() {
    let history = |budget: Option<&str>| {
        let sandbox = Sandbox::new("budget-history");
        let mut cmd = lucida(&sandbox);
        if let Some(budget) = budget {
            cmd.env("LUCIDA_BUDGET", budget);
        }
        let out = run(cmd.args(["--json", "history"]));
        out.exits(0);
        serde_json::from_str::<serde_json::Value>(out.stdout.trim()).unwrap()
    };

    let unreadable = history(Some("$5"));
    assert!(unreadable["budget_usd"].is_null(), "{unreadable}");
    let problem = unreadable["budget_problem"].as_str().unwrap_or_default();
    assert!(problem.contains("`$5`") && problem.contains("such as `5`"), "{unreadable}");

    let readable = history(Some("5"));
    assert_eq!(readable["budget_usd"], 5.0);
    assert!(readable["budget_problem"].is_null(), "{readable}");

    let unset = history(None);
    assert!(unset["budget_usd"].is_null() && unset["budget_problem"].is_null(), "{unset}");
}

#[test]
fn a_budget_with_the_ledger_off_refuses_a_paid_render() {
    // The budget is counted from the ledger, so with the ledger off nothing
    // spent is ever counted and the cap could never be reached.
    let sandbox = Sandbox::new("budget-no-ledger");
    run(lucida(&sandbox)
        .env("LUCIDA_BUDGET", "5")
        .env("LUCIDA_NO_LEDGER", "1")
        .args(["video", "x", "--provider", "google", "--dry-run"]))
    .exits(2)
    .says("LUCIDA_BUDGET")
    .says("LUCIDA_NO_LEDGER");

    run(lucida(&sandbox)
        .env("LUCIDA_BUDGET", "5")
        .env("LUCIDA_NO_LEDGER", "1")
        .args(["generate", "x", "--provider", "comfyui", "--dry-run"]))
    .exits(0);
}

/// With no home directory the ledger has nowhere to live, so nothing spent is
/// counted — the same hole as `LUCIDA_NO_LEDGER`, refused the same way, and
/// named for what it is.
#[test]
fn a_budget_with_nowhere_to_keep_the_ledger_refuses_a_paid_render() {
    let sandbox = Sandbox::new("budget-homeless");
    let homeless = || {
        let mut cmd = lucida(&sandbox);
        cmd.env_remove("HOME").env_remove("USERPROFILE").env("LUCIDA_BUDGET", "5");
        cmd
    };
    run(homeless().args(["generate", "x", "--provider", "google", "--dry-run"]))
        .exits(2)
        .says("nowhere to live")
        .never_says("LUCIDA_NO_LEDGER");

    run(homeless().args(["generate", "x", "--provider", "comfyui", "--dry-run"])).exits(0);
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
fn ops_names_the_provider_that_started_each_render() {
    let sandbox = Sandbox::new("ops-provider");
    sandbox.write_config("");
    let at = 1_700_000_000;
    let started = |provider: Option<&str>, operation: &str| {
        let mut entry = serde_json::json!({
            "at": at, "kind": "video", "status": "started",
            "model": "m", "prompt": "p", "operation": operation,
        });
        if let Some(provider) = provider {
            entry["provider"] = provider.into();
        }
        format!("{entry}\n")
    };
    let runway = "4f1a2b3c-0000-4000-8000-000000000000";
    let legacy = "5f1a2b3c-0000-4000-8000-000000000000";
    let failed = "6f1a2b3c-0000-4000-8000-000000000000";
    let ledger = [
        started(Some("runway"), runway),
        // Before the provider was recorded: no field, so the bare command.
        started(None, legacy),
        // Retired by a terminal failure: must not be listed at all.
        started(Some("runway"), failed),
        format!(
            "{}\n",
            serde_json::json!({
                "at": at + 1, "kind": "video", "status": "failed",
                "provider": "runway", "operation": failed, "error": "moderation",
            })
        ),
    ]
    .concat();
    fs::write(sandbox.config_file().with_file_name("renders.jsonl"), ledger).unwrap();

    let ops = run(lucida(&sandbox).arg("ops"));
    ops.says(&format!("lucida check --provider runway {runway}"))
        .says(&format!("lucida check {legacy}"))
        .never_says(&format!("--provider runway {legacy}"))
        .never_says(failed);
}

/// An image whose wait was abandoned after it was billed is spend `history`
/// counts and a render `ops` never lists: there is nothing to collect.
#[test]
fn an_abandoned_image_is_counted_and_never_listed() {
    let sandbox = Sandbox::new("ledger-abandoned");
    sandbox.write_config("");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let entry = serde_json::json!({
        "at": now, "kind": "image", "status": "abandoned", "provider": "runway",
        "model": "gen4_image", "prompt": "p", "handle": "img-7", "estimated_usd": 0.08,
        "error": "gave up after 10 minutes",
    });
    fs::write(sandbox.config_file().with_file_name("renders.jsonl"), format!("{entry}\n")).unwrap();

    let history = run(lucida(&sandbox).args(["--json", "history"]));
    let document: serde_json::Value = serde_json::from_str(history.stdout.trim()).unwrap();
    assert_eq!(document["estimated_usd_24h"], 0.08, "{document}");

    run(lucida(&sandbox).arg("history")).says("abandoned").says("img-7");
    run(lucida(&sandbox).arg("ops"))
        .says("No video renders are waiting")
        .never_says("img-7");
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

    // A server that never started has no CR in its output either. Require the
    // response, and a clean exit, so there is something for the check to see.
    out.exits(0).says("generate_image");
    assert!(
        !out.stdout.contains('\r'),
        "the server emitted CR in its output"
    );
}

#[test]
fn a_notification_draws_no_response() {
    // An empty stdout is also what a server that failed to start produces. So a
    // `ping` follows the notification: its answer proves the server was reading,
    // and, being the only line, that the notification before it drew none.
    let sandbox = Sandbox::new("mcp-notify");
    let out = mcp(
        &sandbox,
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
         {\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n",
    );

    out.exits(0);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected only the ping's reply, so the notification drew none:\n{}",
        out.stdout
    );
    let reply: serde_json::Value =
        serde_json::from_str(lines[0]).expect("the one reply must be a JSON document");
    assert_eq!(reply["id"], 7, "the reply is not the ping's: {reply}");
}

#[test]
fn lucida_config_names_the_file_in_use() {
    // The process-level half of the `src/config.rs` unit test for the same
    // rule: that unit test cannot set the variable without racing its
    // neighbours, so the proof that it is read at all lives here, where each
    // run has an environment of its own.
    let sandbox = Sandbox::new("config-explicit");
    let file = sandbox.dir.join("elsewhere.env");
    fs::write(&file, "OPENAI_API_KEY=sk-not-a-real-key\n").unwrap();

    let out = run(lucida(&sandbox).env("LUCIDA_CONFIG", &file).arg("config"));

    out.exits(0)
        .says(&format!("Config file: {}", file.display()))
        .says("set (config file)");
}

/// The replies of one `lucida mcp` session, one JSON document per line.
///
/// Waits for `expected` replies before hanging up. A `tools/call` runs on a
/// worker, and closing stdin discards calls that have not started — so the
/// plain `mcp` helper, which closes it at once, would race the very call under
/// test. Bounded, so a missing reply fails the assertion rather than hanging.
fn mcp_replies(sandbox: &Sandbox, script: &str, expected: usize) -> Vec<serde_json::Value> {
    use std::io::BufRead;

    let mut child = lucida(sandbox)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("could not start `lucida mcp`");

    let mut pipe = child.stdin.take().unwrap();
    pipe.write_all(script.as_bytes()).expect("could not write stdin");

    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let mut replies = Vec::new();
    while replies.len() < expected {
        match receiver.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(line) => replies
                .push(serde_json::from_str(&line).expect("the server wrote a line that is not JSON")),
            Err(_) => break,
        }
    }

    drop(pipe);
    child.wait().expect("`lucida mcp` did not exit after its input closed");
    replies
}

#[test]
fn a_line_that_is_not_json_gets_a_parse_error_with_a_null_id() {
    // The line used to be logged to stderr and dropped, so a client that sent
    // one malformed request waited for an answer for as long as it cared to.
    let sandbox = Sandbox::new("mcp-parse-error");
    let replies = mcp_replies(&sandbox, "this is not json\n", 1);

    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0]["error"]["code"], -32700, "{}", replies[0]);
    assert!(replies[0]["id"].is_null(), "{}", replies[0]);
    assert!(replies[0].get("id").is_some(), "the id must be present, as null: {}", replies[0]);
}

#[test]
fn the_error_code_says_what_was_wrong() {
    let sandbox = Sandbox::new("mcp-codes");
    let replies = mcp_replies(
        &sandbox,
        concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#, "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"paint_a_fresco","arguments":{}}}"#, "\n",
        ),
        2,
    );

    let code_of = |id: u64| {
        replies
            .iter()
            .find(|reply| reply["id"] == id)
            .unwrap_or_else(|| panic!("no reply to {id}: {replies:?}"))["error"]["code"]
            .clone()
    };
    assert_eq!(code_of(1), -32601, "an unknown method is method-not-found");
    assert_eq!(code_of(2), -32602, "an unknown tool is invalid params");
}

#[test]
fn a_misspelt_argument_is_refused_and_nothing_runs() {
    // `reference_image`, singular, used to be ignored: the call rendered a
    // fresh generation and reported success. The output path is absolute and
    // inside the sandbox so the test can see whether anything was written.
    let sandbox = Sandbox::new("mcp-unknown-argument");
    let output = sandbox.dir.join("fox.png");
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "generate_image", "arguments": {
            "prompt": "a fox",
            "output_path": output,
            "reference_image": "photo.png"
        } }
    });
    let replies = mcp_replies(&sandbox, &format!("{call}\n"), 1);

    let result = &replies[0]["result"];
    assert_eq!(result["isError"], true, "{}", replies[0]);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("`reference_image`"), "must name the key: {text}");
    assert!(text.contains("`reference_images`"), "must list what is accepted: {text}");
    assert!(text.contains("nothing has been run"), "{text}");
    assert!(!output.exists(), "a refused call must not write an image");
}

#[test]
fn every_tool_schema_closes_its_properties() {
    let sandbox = Sandbox::new("mcp-closed-schemas");
    let replies = mcp_replies(&sandbox, &format!("{TOOLS_LIST}\n"), 1);

    let tools = replies[0]["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    for tool in tools {
        assert_eq!(
            tool["inputSchema"]["additionalProperties"], false,
            "{} does not say it refuses unknown arguments",
            tool["name"]
        );
    }
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

// --- default provider preference -------------------------------------------
//
// These are process tests rather than unit tests because the thing under test
// is a *process* fact: a setting resolved from the environment or a config
// file, on a run that named neither provider nor model. `env_clear` and a
// private HOME are exactly the conditions that make the question meaningful —
// a stray key on the developer's machine would otherwise decide the answer.

#[test]
fn a_preference_picks_the_first_provider_you_hold_a_key_for() {
    let sandbox = Sandbox::new("pref-first");
    let out = run(lucida(&sandbox)
        .env("BFL_API_KEY", "k")
        .env("LUCIDA_IMAGE_PROVIDERS", "bfl,google")
        .args(["generate", "x", "--dry-run", "--json"]));

    out.exits(0)
        .says("\"provider\":\"bfl\"")
        .says("\"provider_source\":\"LUCIDA_IMAGE_PROVIDERS\"");
}

#[test]
fn a_preference_skips_the_providers_you_have_no_key_for() {
    let sandbox = Sandbox::new("pref-skip");
    let out = run(lucida(&sandbox)
        .env("BFL_API_KEY", "k")
        .env("LUCIDA_IMAGE_PROVIDERS", "openai,stability,bfl")
        .args(["generate", "x", "--dry-run", "--json"]));

    // Third in the list, and it says so: the position is the evidence that the
    // first two were considered and passed over rather than never read.
    out.exits(0)
        .says("\"provider\":\"bfl\"")
        .says("choice 3 of 3");
}

/// The rule the whole design turns on, pinned so it cannot be relaxed quietly.
///
/// Every provider the user listed is unusable, and they *do* hold a google key.
/// A fallback chain would render at google. This must refuse instead: google is
/// not on their list, and spending money at a provider nobody named is the
/// outcome the preference order exists to avoid (ROADMAP § 6, constraint 2).
#[test]
fn a_preference_refuses_rather_than_falling_back_to_an_unlisted_provider() {
    let sandbox = Sandbox::new("pref-nofallback");
    let out = run(lucida(&sandbox)
        .env("GEMINI_API_KEY", "g")
        .env("LUCIDA_IMAGE_PROVIDERS", "openai,stability")
        .args(["generate", "x", "--dry-run", "--json"]));

    out.says("no provider in LUCIDA_IMAGE_PROVIDERS has a credential")
        .says("openai needs OPENAI_API_KEY")
        // Names what they *can* reach, so the refusal is one move from fixed.
        .says("You do have credentials for google")
        // and did not quietly render there.
        .never_says("\"provider\":\"google\"");
    // A refusal, not a failure: nothing was sent, and retrying cannot succeed
    // until the configuration changes — which is what exit 2 tells a wrapper.
    out.exits(2);
}

#[test]
fn an_unknown_name_in_the_preference_refuses_rather_than_being_skipped() {
    // Skipping a typo would hand the render to whatever came next — which here
    // is a real provider, so it would have looked like it worked. Exit 2,
    // because nothing was sent and the list has to change before a retry can
    // succeed; both media share the function, so both are driven.
    for (setting, key, list, command) in [
        ("LUCIDA_IMAGE_PROVIDERS", "BFL_API_KEY", "bflx,bfl", &["generate", "x", "--dry-run", "--json"][..]),
        ("LUCIDA_VIDEO_PROVIDERS", "RUNWAY_API_KEY", "runwayx,runway", &["video", "x", "--dry-run", "--json"][..]),
    ] {
        let sandbox = Sandbox::new("pref-typo");
        let out = run(lucida(&sandbox).env(key, "k").env(setting, list).args(command));
        let typo = list.split(',').next().unwrap();
        out.exits(2)
            .says(&format!("{setting} lists `{typo}`"))
            .says("not a provider")
            .never_says("\"provider\":\"bfl\"")
            .never_says("\"provider\":\"runway\"");
    }
}

#[test]
fn a_preference_with_no_entries_is_refused_rather_than_read_as_unset() {
    // `, ,` is set, and says nothing. Reading it as unset sent the render to the
    // built-in default and reported "no preference set" about a setting that was
    // set — the substitution the preference exists to prevent, one step earlier.
    for (setting, command) in [
        ("LUCIDA_IMAGE_PROVIDERS", &["generate", "x", "--dry-run"][..]),
        ("LUCIDA_VIDEO_PROVIDERS", &["video", "x", "--dry-run"][..]),
    ] {
        let sandbox = Sandbox::new("pref-empty");
        let out = run(lucida(&sandbox).env(setting, ", ,").args(command));
        out.exits(2).says(setting).says("no provider").never_says("no preference set");
    }
}

/// Blank is unset, and only blank: the line between this and the refusal above.
///
/// `config::var` drops an empty or whitespace-only value before the preference
/// is ever parsed, so `"  "` is the built-in default and says so. `", ,"` is
/// not blank — it holds commas — and is refused. A change to either side moves
/// the line, which is why both are pinned here and in README.md.
#[test]
fn a_blank_preference_counts_as_unset() {
    for (setting, key, command) in [
        ("LUCIDA_IMAGE_PROVIDERS", "GEMINI_API_KEY", &["generate", "x", "--dry-run"][..]),
        ("LUCIDA_VIDEO_PROVIDERS", "GEMINI_API_KEY", &["video", "x", "--dry-run"][..]),
    ] {
        let sandbox = Sandbox::new("pref-blank");
        let out = run(lucida(&sandbox).env(key, "g").env(setting, "  ").args(command));
        out.exits(0).says("no preference set");
    }
}

#[test]
fn naming_a_provider_beats_the_preference_and_announces_nothing() {
    let sandbox = Sandbox::new("pref-explicit");
    let out = run(lucida(&sandbox)
        .env("STABILITY_API_KEY", "s")
        .env("LUCIDA_IMAGE_PROVIDERS", "bfl")
        .args([
            "generate",
            "x",
            "--provider",
            "stability",
            "--dry-run",
            "--json",
        ]));

    // Nothing was defaulted, so there is nothing to report. Narrating a choice
    // back to the person who just made it is noise, and `provider_source` being
    // null is how a caller tells "I chose this" from "it was chosen for me".
    out.exits(0)
        .says("\"provider\":\"stability\"")
        .says("\"provider_source\":null")
        .never_says("Provider: ");
}

#[test]
fn no_preference_leaves_the_built_in_default_exactly_as_it_was() {
    let sandbox = Sandbox::new("pref-none");
    let out = run(lucida(&sandbox)
        .env("GEMINI_API_KEY", "g")
        .args(["generate", "x", "--dry-run", "--json"]));

    out.exits(0)
        .says("\"provider\":\"google\"")
        .says("\"provider_source\":\"built-in\"");
}

#[test]
fn video_has_its_own_preference_list() {
    let sandbox = Sandbox::new("pref-video");
    let out = run(lucida(&sandbox)
        .env("RUNWAY_API_KEY", "r")
        .env("LUCIDA_VIDEO_PROVIDERS", "google,runway")
        .args(["video", "x", "--dry-run", "--json"]));

    // Also pins the restructure this needed: the video path used to bake in a
    // default *model* before choosing a provider, so a preference could never
    // have been consulted. Landing on runway proves the order was reversed.
    out.exits(0)
        .says("\"provider\":\"runway\"")
        .says("\"provider_source\":\"LUCIDA_VIDEO_PROVIDERS\"");
}

/// The `(default)` marker in the tool description follows the actual default.
///
/// It was a literal comparison against one provider, which was true for as long
/// as the default could not move. A preference list moves it, and this text is
/// the thing an agent reasons from when it decides whether to name a provider at
/// all — so a stale marker here does not merely misinform, it tells the agent
/// the render is going somewhere it is not.
#[test]
fn the_default_marker_follows_the_preference() {
    let sandbox = Sandbox::new("mcp-default-marker");
    let out = run_with_stdin(
        lucida(&sandbox)
            .arg("mcp")
            .env("BFL_API_KEY", "k")
            .env("LUCIDA_IMAGE_PROVIDERS", "bfl,google"),
        &format!("{TOOLS_LIST}\n"),
    );

    out.says("- bfl (default)")
        // and google, in the same image list, is no longer marked.
        .says("- google: Highest quality");
}

/// A preference nothing can satisfy must not stop the server answering.
///
/// Resolution refuses in that state, and `tools/list` runs it to place the
/// marker. Propagating that refusal would take the whole listing down over a
/// setting — so nothing is marked, which is also the honest answer: until the
/// list is fixed, no provider is the default.
#[test]
fn an_unsatisfiable_preference_still_lists_the_tools() {
    let sandbox = Sandbox::new("mcp-default-none");
    let out = run_with_stdin(
        lucida(&sandbox).arg("mcp").env("LUCIDA_IMAGE_PROVIDERS", "openai"),
        &format!("{TOOLS_LIST}\n"),
    );

    out.says("generate_image")
        // No image provider is marked, because none resolves.
        .says("- google: Highest quality")
        .never_says("- google (default): Highest quality");
}

/// A typed-out retired video id warns before the render is attempted.
///
/// The image list annotates a retired id where it is displayed. Video has no
/// such list — every alias points at a current model — so a retired id can only
/// arrive by being typed, and nothing was telling the person who typed it.
#[test]
fn a_retired_video_model_says_so_before_it_is_sent() {
    let sandbox = Sandbox::new("veo-retired");
    let out = run(lucida(&sandbox).args([
        "video",
        "x",
        "--model",
        "veo-3.0-generate-001",
        "--dry-run",
    ]));

    out.says("retired 2026-06-30").says("expect this to fail");
}

#[test]
fn a_current_video_model_carries_no_such_warning() {
    let sandbox = Sandbox::new("veo-current");
    let out = run(lucida(&sandbox).args(["video", "x", "--model", "veo-fast", "--dry-run"]));

    // The warning's absence means nothing from a command that did not run, so
    // require the dry run's own output first: it exits 0 and names the model
    // that `veo-fast` resolves to.
    out.exits(0)
        .says("Dry run")
        .says("veo-3.1-fast-generate-preview")
        .never_says("retired")
        .never_says("expect this to fail");
}

// --- the Lemonade lane ------------------------------------------------------
//
// `lucida()` already points LUCIDA_LEMONADE_URL at port 1, where nothing
// listens; `lemonade()` is the spelling a test of this lane uses, and a test
// that needs a server overrides the URL with a stand-in this file starts itself.
// The lane's default is a loopback port where a real Lemonade may be running, so
// a test that left it unset could render on the machine running the suite. The
// stand-in is not a provider — nothing leaves this process — which keeps the
// file's first rule.

/// Nothing listens here.
const NOWHERE: &str = "http://127.0.0.1:1/v1";

const LEMONADE_MODELS: &str = r#"{"object":"list","data":[{"id":"Flux-2-Klein-4B-TheNoise","object":"model","labels":["image","edit"],"recipe_options":{"cfg_scale":1.0,"height":1024,"steps":4,"width":1024}}]}"#;

/// A 64x64 black PNG.
const TINY_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAIklEQVR4nO3BAQ0AAADCoPdPbQ8HFAAAAAAAAAAAAAAA8G4wQAABiwCo9wAAAABJRU5ErkJggg==";

fn lemonade_image() -> String {
    format!(r#"{{"created":1,"data":[{{"b64_json":"{TINY_PNG_B64}"}}]}}"#)
}

/// A command for a test of the Lemonade lane: [`lucida`], with the server
/// address set explicitly so the test says where it points.
fn lemonade(sandbox: &Sandbox) -> Command {
    let mut cmd = lucida(sandbox);
    cmd.env("LUCIDA_LEMONADE_URL", NOWHERE);
    cmd
}

/// A stand-in Lemonade on a loopback port: one scripted JSON reply per
/// connection, in order. Returns its `/v1` base and a handle that yields each
/// request as it arrived (head, blank line, body). Gives up on a connection
/// that does not come within 20 s, so a broken binary fails the test rather
/// than hanging it.
fn fake_lemonade(replies: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    fake_lemonade_answering(replies.into_iter().map(|body| (200, body)).collect())
}

/// [`fake_lemonade`], with each reply's HTTP status scripted too.
fn fake_lemonade_answering(replies: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{BufRead, BufReader, Read};
    use std::time::{Duration, Instant};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binding the stand-in");
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();

    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, reply) in replies {
            let deadline = Instant::now() + Duration::from_secs(20);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break Some(stream),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break None,
                }
            };
            let Some(stream) = stream else { break };
            stream.set_nonblocking(false).unwrap();

            let mut reader = BufReader::new(stream);
            let mut head = String::new();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            seen.push(format!("{head}\r\n{}", String::from_utf8_lossy(&body)));

            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 {status} Scripted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
        seen
    });
    (base, handle)
}

#[test]
fn lemonade_without_a_model_is_refused_before_any_request() {
    let sandbox = Sandbox::new("lemonade-no-model");
    run(lemonade(&sandbox).args(["generate", "x", "--provider", "lemonade"]))
        .exits(2)
        .says("LUCIDA_LEMONADE_MODEL")
        .says("lucida models --provider lemonade")
        .never_says("not reachable");
}

#[test]
fn a_lemonade_cased_id_is_refused_rather_than_billed_at_bfl() {
    let sandbox = Sandbox::new("lemonade-casing");
    run(lemonade(&sandbox).args(["generate", "x", "--model", "Flux-2-Klein-4B"]))
        .exits(2)
        .says("--provider lemonade")
        .says("--provider bfl")
        .never_says("BFL_API_KEY");
}

#[test]
fn an_explicit_provider_bfl_is_not_refused_for_its_casing() {
    // `--provider bfl` is a decision, so the casing guard on the inferred route
    // stays out of it. With no BFL key anywhere the render stops at the
    // credential — nothing is sent, nothing is spent — and the dry run plans it.
    let sandbox = Sandbox::new("lemonade-casing-explicit-bfl");
    let casing = "every BFL model id is lower-case";
    run(lemonade(&sandbox).args(["generate", "x", "--provider", "bfl", "--model", "Flux-2-Klein-4B"]))
        .says("BFL_API_KEY")
        .never_says(casing);

    let out = run(lemonade(&sandbox).args([
        "generate", "x", "--provider", "bfl", "--model", "Flux-2-Klein-4B", "--dry-run", "--json",
    ]));
    out.exits(0).never_says(casing);
    let plan: serde_json::Value = serde_json::from_str(out.stdout.trim()).expect("one JSON document");
    assert_eq!(plan["provider"], "bfl");
}

#[test]
fn an_mcp_call_with_a_lemonade_cased_id_and_no_provider_is_refused() {
    let sandbox = Sandbox::new("mcp-lemonade-casing");
    let output = sandbox.dir.join("klein.png");
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "generate_image", "arguments": {
            "prompt": "a fox",
            "output_path": output,
            "model": "Flux-2-Klein-4B"
        } }
    });
    let replies = mcp_replies(&sandbox, &format!("{call}\n"), 1);

    let result = &replies[0]["result"];
    assert_eq!(result["isError"], true, "{}", replies[0]);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("every BFL model id is lower-case"), "{text}");
    assert!(text.contains(r#"provider: "lemonade""#), "the MCP spelling of the remedy: {text}");
    assert!(!text.contains("BFL_API_KEY"), "refused for the key, not the casing: {text}");
    assert!(!output.exists(), "a refused call must not write an image");
}

#[test]
fn a_lemonade_dry_run_touches_no_network_and_costs_nothing() {
    let sandbox = Sandbox::new("lemonade-dry-run");
    let out = run(lemonade(&sandbox).args([
        "generate", "x", "--provider", "lemonade", "--model", "Flux-2-Klein-4B", "--dry-run", "--json",
    ]));
    out.exits(0).never_says("not reachable");
    let plan: serde_json::Value = serde_json::from_str(out.stdout.trim()).expect("one JSON document");
    assert_eq!(plan["provider"], "lemonade");
    assert_eq!(plan["model"], "Flux-2-Klein-4B");
    assert_eq!(plan["estimated_usd"], 0.0);
}

/// A PNG signature and an IHDR claiming `width` x `height` — all Lucida reads
/// to learn an image's size.
fn png_header(path: &Path, width: u32, height: u32) {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&width.to_be_bytes());
    png.extend_from_slice(&height.to_be_bytes());
    fs::write(path, png).expect("could not write the source image");
}

#[test]
fn a_lemonade_dry_run_refuses_an_edit_whose_source_is_over_the_ceiling() {
    // The render refuses this before it asks the server anything; a dry run
    // promises every refusal a real run would make, so it refuses it too.
    // LUCIDA_LEMONADE_URL is port 1: a request would read "not reachable".
    let sandbox = Sandbox::new("lemonade-dry-run-ceiling");
    let huge = sandbox.dir.join("huge.png");
    png_header(&huge, 4000, 3000);
    run(lemonade(&sandbox)
        .args(["edit"])
        .arg(&huge)
        .args(["x", "--provider", "lemonade", "--model", "M", "--dry-run"]))
    .exits(2)
    .says("4000x3000")
    .says("--size 2048")
    .never_says("not reachable")
    .never_says("Dry run");

    // The remedy it names passes the same dry run.
    run(lemonade(&sandbox)
        .args(["edit"])
        .arg(&huge)
        .args(["x", "--provider", "lemonade", "--model", "M", "--size", "2048", "--dry-run"]))
    .exits(0)
    .never_says("not reachable");
}

#[test]
fn a_lemonade_mask_is_refused_naming_the_lane_whose_mask_binds() {
    let sandbox = Sandbox::new("lemonade-mask");
    run(lemonade(&sandbox).args([
        "edit", "in.png", "x", "--provider", "lemonade", "--model", "M", "--mask", "m.png",
    ]))
    .exits(2)
    .says("comfyui")
    .never_says("not reachable");
}

#[test]
fn an_unreachable_lemonade_says_where_it_looked_and_which_setting_chose_it() {
    let sandbox = Sandbox::new("lemonade-down");
    run(lemonade(&sandbox).args(["generate", "x", "--provider", "lemonade", "--model", "M"]))
        .exits(1)
        .says(&format!("not reachable at {NOWHERE}"))
        .says("LUCIDA_LEMONADE_URL")
        .never_says("not a fallback");

    run(lemonade(&sandbox)
        .env("LUCIDA_IMAGE_PROVIDERS", "lemonade,google")
        .env("LUCIDA_LEMONADE_MODEL", "M")
        .args(["generate", "x"]))
    .exits(1)
    .says("not reachable")
    .says("not a fallback");
}

#[test]
fn models_for_an_unreachable_lemonade_still_print_what_it_supports() {
    let sandbox = Sandbox::new("lemonade-models-down");
    run(lemonade(&sandbox).args(["models", "--provider", "lemonade"]))
        .exits(0)
        .says("did not answer")
        .says("This provider supports:")
        .says("at most 2048 pixels");
}

#[test]
fn a_lemonade_render_is_free_seeded_and_recorded() {
    let sandbox = Sandbox::new("lemonade-render");
    let (base, fake) = fake_lemonade(vec![LEMONADE_MODELS.to_string(), lemonade_image()]);
    let destination = sandbox.dir.join("fox.png");

    let out = run(lemonade(&sandbox)
        .env("LUCIDA_LEMONADE_URL", &base)
        .args(["--json", "generate", "a fox", "--provider", "lemonade", "--model", "Flux-2-Klein-4B-TheNoise", "-o"])
        .arg(&destination));
    out.exits(0);
    let seen = fake.join().unwrap();
    assert_eq!(seen.len(), 2, "{seen:?}");

    let doc: serde_json::Value = serde_json::from_str(out.stdout.trim()).expect("one JSON document");
    let image = &doc["images"][0];
    assert_eq!(image["provider"], "lemonade");
    assert_eq!(image["estimated_usd"], 0.0);
    let seed = image["seed"].as_u64().expect("a Lemonade render always reports its seed");
    // The seed reported is the one that went over the wire.
    assert!(seen[1].contains(&format!("\"seed\":{seed}")), "{}", seen[1]);
    assert!(fs::read(&destination).unwrap().starts_with(&[0x89, b'P', b'N', b'G']));

    let ledger = fs::read_to_string(sandbox.config_file().with_file_name("renders.jsonl"))
        .expect("the render left no ledger");
    let entry: serde_json::Value = serde_json::from_str(ledger.lines().last().unwrap()).unwrap();
    assert_eq!(entry["provider"], "lemonade");
    assert_eq!(entry["seed"], seed);
    assert_eq!(entry["estimated_usd"], 0.0);
}

#[test]
fn tools_list_offers_lemonade_and_never_its_placeholder() {
    let sandbox = Sandbox::new("mcp-lemonade");
    let out = mcp(&sandbox, &format!("{TOOLS_LIST}\n"));
    assert!(schema_providers(&out).iter().any(|p| p == "lemonade"), "{}", out.stdout);
    out.never_says("lemonade-model");
}

#[test]
fn the_canary_can_tell_a_listening_lemonade_from_a_silent_one() {
    // scripts/canary.sh passes lemonade on "Image models available to" and
    // skips it only on the lane's own "Lemonade is not reachable at". Both
    // sides are pinned on every platform: from a stand-in that answers, and
    // from port 1, where nothing does. The tests below run the script itself.
    let sandbox = Sandbox::new("canary-lemonade");
    let (base, fake) = fake_lemonade(vec![LEMONADE_MODELS.to_string()]);
    run(lucida(&sandbox)
        .env("LUCIDA_LEMONADE_URL", &base)
        .args(["models", "--provider", "lemonade"]))
    .says("Image models available to the lemonade provider")
    .says("Flux-2-Klein-4B-TheNoise  (image, edit; 1024x1024; 4 steps; cfg 1)");
    fake.join().unwrap();

    run(lemonade(&sandbox).args(["models", "--provider", "lemonade"]))
        .says("did not answer")
        .says(&format!("Lemonade is not reachable at {NOWHERE}"));
}

/// The line scripts/canary.sh prints for lemonade, run against `base` with no
/// credential anywhere: every keyed provider skips, ComfyUI points at port 1,
/// and nothing but the stand-in at `base` is asked anything. Unix only: the
/// script is bash, and a Windows runner's `bash` may not be the one it needs.
#[cfg(unix)]
fn canary_says_of_lemonade(label: &str, base: &str) -> String {
    let sandbox = Sandbox::new(label);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/canary.sh");
    let mut cmd = Command::new("bash");
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &sandbox.dir)
        .env("LUCIDA_LEMONADE_URL", base)
        .env("LUCIDA_COMFYUI_URL", "http://127.0.0.1:1")
        .arg(&script)
        .arg(binary());
    let out = run(&mut cmd);
    let lines: Vec<&str> = out.stdout.lines().filter(|l| l.contains(" lemonade — ")).collect();
    assert_eq!(lines.len(), 1, "one lemonade verdict expected:\n{}", out.stdout);
    lines[0].to_string()
}

#[cfg(unix)]
#[test]
fn the_canary_skips_lemonade_only_when_it_is_not_reachable() {
    // Nothing listening.
    let line = canary_says_of_lemonade("canary-lemonade-refused", NOWHERE);
    assert!(line.starts_with("  --    lemonade — not listening"), "{line}");

    // A gateway up in front of a Lemonade that is not — once per attempt, as
    // the listing is retried.
    let down = r#"{"error":{"message":"Lemonade is not reachable.","type":"invalid_request_error","param":null,"code":"upstream_unreachable"}}"#;
    let (base, fake) = fake_lemonade_answering(vec![(502, down.to_string()); 3]);
    let line = canary_says_of_lemonade("canary-lemonade-502", &base);
    assert_eq!(fake.join().unwrap().len(), 3);
    assert!(line.starts_with("  --    lemonade — not listening"), "{line}");
}

#[cfg(unix)]
#[test]
fn the_canary_fails_lemonade_on_any_other_answer_with_the_reason() {
    // A base without `/v1`: answered, but not by Lemonade's API.
    let (base, fake) = fake_lemonade_answering(vec![(404, r#"{"detail":"Not Found"}"#.to_string())]);
    let line = canary_says_of_lemonade("canary-lemonade-404", &base);
    fake.join().unwrap();
    assert!(line.starts_with("  DRIFT lemonade — "), "{line}");
    assert!(line.contains("HTTP 404"), "the reason is missing: {line}");

    // A listing in which nothing is labelled `image` — what a renamed
    // `labels` field would look like. Not a quiet server: drift.
    let unlabelled = r#"{"object":"list","data":[{"id":"Flux-2-Klein-4B-TheNoise","object":"model","tags":["image","edit"]}]}"#;
    let (base, fake) = fake_lemonade(vec![unlabelled.to_string()]);
    let line = canary_says_of_lemonade("canary-lemonade-unlabelled", &base);
    fake.join().unwrap();
    assert!(line.starts_with("  DRIFT lemonade — "), "{line}");
    assert!(line.contains("No image models visible"), "the reason is missing: {line}");
}

#[cfg(unix)]
#[test]
fn the_canary_passes_lemonade_on_a_listing_with_an_image_model() {
    let (base, fake) = fake_lemonade(vec![LEMONADE_MODELS.to_string()]);
    let line = canary_says_of_lemonade("canary-lemonade-pass", &base);
    let seen = fake.join().unwrap();
    assert!(line.starts_with("  ok    lemonade — "), "{line}");
    // The listing and nothing else: the canary never renders on this lane.
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].starts_with("GET /v1/models "), "{}", seen[0]);
}
