//! `lucida setup` — wiring Lucida into Claude Code and the Claude desktop app.
//!
//! # Claude only, deliberately
//!
//! An earlier sketch covered every MCP client on the machine. It was dropped
//! because it amounted to a table of {client × scope → directory} for
//! third-party tools that change independently, that Lucida cannot probe at
//! runtime, and that CI cannot test — the same shape of hand-written list that
//! went stale five times in one review here. Other clients get a documented
//! shape in the README instead: run `lucida mcp` as a stdio server, and
//! `lucida skill` for the skill. That form names no directory, so it cannot rot.
//!
//! # Nothing here is guessed
//!
//! Every mechanism was verified before it was written:
//!
//! - `claude mcp add --scope local|user|project`, from that command's own help.
//! - The desktop app reads `mcpServers` from `claude_desktop_config.json` — a
//!   server added that way appears in its Connectors list badged "Local dev".
//! - Skills live at `~/.claude/skills/<name>/SKILL.md`, or `.claude/skills/`
//!   in a project.
//!
//! **A target that is not detected is skipped, never created.** The desktop
//! config path differs per platform and only macOS was verified here, so setup
//! merges into a file that already exists and otherwise reports the app as
//! absent. That makes a wrong path harmless — it cannot write somewhere strange,
//! it can only fail to find something.

use crate::skill;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Where an installation applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every project on this machine.
    User,
    /// One project directory.
    Project(PathBuf),
}

/// One thing setup would do, and can describe before doing it.
#[derive(Debug)]
enum Step {
    /// Register the MCP server through Claude Code's own CLI, which owns the
    /// file format so Lucida does not have to know it.
    ClaudeCodeMcp { scope: &'static str },
    /// Merge into the desktop app's config, preserving everything else in it.
    DesktopMcp { path: PathBuf },
    /// Write the skill where a client will find it.
    Skill { path: PathBuf },
    /// Something already correct. Reported rather than silently skipped, since
    /// "nothing happened" and "nothing needed to happen" read the same otherwise.
    AlreadyDone { what: String },
}

/// Why the desktop app gets an instruction instead of a file.
///
/// Its skills are uploaded through the UI and held in the app's own store —
/// verified by watching an upload: the only file that appeared was under
/// `local-agent-mode-sessions/skills-plugin/<session>/<session>/skills/`,
/// alongside every other skill including Anthropic's, beside a generated
/// manifest, with the app's Local Storage and IndexedDB written in the same
/// second. That directory is a working copy the app extracts per session, so a
/// file written into it is gone by the next one.
///
/// Automating this would therefore mean writing to a database the app owns, to
/// save one drag-and-drop. Printing the path is the honest trade.
const DESKTOP_SKILL_NOTE: &str = "\nThe Claude app keeps skills in its own store rather than on disk, so \
     this one is\nadded once by hand: Settings → Skills → Add → Upload a skill, and choose";

impl Step {
    fn describe(&self) -> String {
        match self {
            Step::ClaudeCodeMcp { scope } => {
                format!("Claude Code   register MCP server (--scope {scope})")
            }
            Step::DesktopMcp { path } => {
                format!("Claude app    add mcpServers.lucida to {}", tilde(path))
            }
            Step::Skill { path } => format!("skill         write {}", tilde(path)),
            Step::AlreadyDone { what } => format!("already done  {what}"),
        }
    }

    fn is_work(&self) -> bool {
        !matches!(self, Step::AlreadyDone { .. })
    }
}

pub fn run(scope: Scope, dry_run: bool, assume_yes: bool) -> Result<()> {
    let exe = std::env::current_exe().context("finding this binary")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

    let clients = Clients::detect(&scope);
    let steps = plan(&scope, &exe, &clients)?;

    println!("Lucida  {}", exe.display());
    println!(
        "Scope   {}\n",
        match &scope {
            Scope::User => "user — every project on this machine".to_string(),
            Scope::Project(dir) => format!("project — {}", dir.display()),
        }
    );

    for step in &steps {
        println!("  {}", step.describe());
    }

    if !steps.iter().any(Step::is_work) {
        println!("\nNothing to do.");
        return Ok(());
    }

    if dry_run {
        println!("\n--dry-run, so nothing was changed.");
        return Ok(());
    }

    if !assume_yes && !confirm()? {
        println!("Nothing was changed.");
        return Ok(());
    }

    println!();
    for step in &steps {
        apply(step, &exe, &scope)?;
    }

    println!(
        "\nRestart Claude Code and the Claude app to pick this up — both read \
         their server lists at startup."
    );

    // Only worth saying where the app is actually installed. `Clients::detect`
    // already handles the project-scope case by reporting no desktop app at all,
    // so this reads one fact rather than re-deriving it — and it now names a file
    // the plan above genuinely wrote, which for a while it did not.
    if clients.desktop.is_some() {
        println!("{DESKTOP_SKILL_NOTE}\n  {}", tilde(&skill_path(&scope)));
    }

    Ok(())
}

/// What would be done, without doing any of it.
///
/// Built entirely from what is present: a client that is not installed
/// contributes no steps rather than an error, because "set this up wherever it
/// applies" is the request, and a machine with only one of the two is normal.
/// Which of the two clients this machine has.
///
/// A parameter rather than something `plan` looks up, because the four
/// combinations are exactly what needed testing and three of them cannot be
/// produced on the machine running the tests. `plan` had no tests at all, which
/// is how it shipped naming a file it never wrote.
pub struct Clients {
    pub claude_code: bool,
    /// The desktop app's config file, if it is there.
    pub desktop: Option<PathBuf>,
}

impl Clients {
    fn detect(scope: &Scope) -> Self {
        Self {
            claude_code: which("claude").is_some(),
            // The desktop app has no notion of a project, so a project-scope run
            // does not touch it. Saying so beats silently ignoring the flag.
            desktop: match scope {
                Scope::User => desktop_config(),
                Scope::Project(_) => None,
            },
        }
    }
}

fn plan(scope: &Scope, exe: &Path, clients: &Clients) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    let has_claude_code = clients.claude_code;

    if has_claude_code {
        let scope_flag = match scope {
            Scope::User => "user",
            Scope::Project(_) => "project",
        };
        if claude_code_has_lucida(exe) {
            steps.push(Step::AlreadyDone {
                what: "Claude Code already registers this binary".into(),
            });
        } else {
            steps.push(Step::ClaudeCodeMcp { scope: scope_flag });
        }
    }

    let desktop = clients.desktop.clone();
    if let Some(path) = &desktop {
        if desktop_has_lucida(path, exe)? {
            steps.push(Step::AlreadyDone {
                what: "the Claude app already registers this binary".into(),
            });
        } else {
            steps.push(Step::DesktopMcp { path: path.clone() });
        }
    }

    // Written for *either* client, which is the fix for a gap that only showed
    // up on a machine with one of them.
    //
    // The skill used to be planned inside the Claude Code branch alone, while
    // the closing note that tells you where to find it for the desktop app was
    // printed on `desktop_config().is_some()`. With the Claude app installed and
    // no Claude Code CLI — a perfectly ordinary machine — setup finished by
    // naming a file it had never written, and the instruction it gave you was to
    // go and upload that file.
    if has_claude_code || desktop.is_some() {
        steps.push(Step::Skill {
            path: skill_path(scope),
        });
    }

    if steps.is_empty() {
        bail!(
            "found neither Claude Code nor the Claude app on this machine.\n\n\
             Any MCP client can run Lucida as a stdio server — see the README \
             for the shape, and `lucida skill` prints the skill."
        );
    }

    Ok(steps)
}

fn apply(step: &Step, exe: &Path, scope: &Scope) -> Result<()> {
    match step {
        Step::AlreadyDone { .. } => Ok(()),

        Step::ClaudeCodeMcp { scope: flag } => {
            // Claude Code's own CLI writes it, because the tool that owns a
            // config format should be the one to edit it — the same reasoning
            // that has `lucida update` run cargo rather than overwrite a
            // cargo-managed binary.
            let mut cmd = std::process::Command::new("claude");
            cmd.args(["mcp", "add", "--scope", flag, "lucida", "--"]);
            cmd.arg(exe).arg("mcp");

            if let Scope::Project(dir) = scope {
                cmd.current_dir(dir);
            }

            let status = cmd.status().context("running `claude mcp add`")?;
            if !status.success() {
                bail!("`claude mcp add` exited with {status}");
            }
            println!("  registered with Claude Code");
            Ok(())
        }

        Step::DesktopMcp { path } => {
            merge_desktop_config(path, exe)?;
            println!("  added to {}", tilde(path));
            Ok(())
        }

        Step::Skill { path } => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(path, skill::SKILL)
                .with_context(|| format!("writing {}", path.display()))?;
            println!("  wrote {}", tilde(path));
            Ok(())
        }
    }
}

/// Adds `mcpServers.lucida`, keeping every other key exactly as it was.
///
/// Read, modify, write — never a fresh file. The desktop config holds unrelated
/// settings, and a tool that rewrites someone's preferences to add one server
/// has done considerably more than it was asked to.
fn merge_desktop_config(path: &Path, exe: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut config: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?
    };

    if !config.is_object() {
        bail!("{} does not contain a JSON object", path.display());
    }

    // Indexing auto-creates objects through `Null`, which is what makes the
    // missing-`mcpServers` case need no handling at all — but serde_json's
    // `IndexMut` *panics* on a key that is present and not an object. So
    // `"mcpServers": []` in someone's config would abort the process, from inside
    // the one function whose stated promise is to treat that file gently. The
    // not-an-object case above already gets a sentence; this deserves the same.
    match config.get("mcpServers") {
        None | Some(Value::Null) => {}
        Some(value) if value.is_object() => {}
        Some(other) => bail!(
            "{}'s `mcpServers` is {} rather than an object, so Lucida cannot be \
             added to it without discarding what is there.\n\n\
             Correct or remove that key and run `lucida setup` again.",
            path.display(),
            json_kind(other)
        ),
    }

    config["mcpServers"]["lucida"] = json!({
        "command": exe.to_string_lossy(),
        "args": ["mcp"],
    });

    let mut body = serde_json::to_string_pretty(&config)?;
    body.push('\n');

    // Staged and renamed rather than truncated in place. This file is not
    // Lucida's — it holds other servers' registrations and unrelated preferences
    // — and a crash or a full disk partway through a truncating write leaves
    // someone with none of it. Not private: it is the app's own config, and
    // nothing Lucida writes into it is a secret.
    crate::config::write_replacing(path, &body, false)
}

/// What a JSON value is, for a message about the wrong kind of thing being there.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn claude_code_has_lucida(exe: &Path) -> bool {
    std::process::Command::new("claude")
        .args(["mcp", "get", "lucida"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| String::from_utf8_lossy(&out.stdout).contains(&*exe.to_string_lossy()))
}

fn desktop_has_lucida(path: &Path, exe: &Path) -> Result<bool> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        return Ok(false);
    };
    Ok(config["mcpServers"]["lucida"]["command"].as_str() == Some(&exe.to_string_lossy()))
}

/// The desktop app's config, only if it is actually there.
///
/// Only the macOS location was verified directly; the other two are the
/// platform-conventional ones. That asymmetry is safe because a path that does
/// not exist means "the app is not installed" rather than "create this" — a
/// wrong guess can only fail to find something.
fn desktop_config() -> Option<PathBuf> {
    let path = if cfg!(target_os = "macos") {
        home()?.join("Library/Application Support/Claude/claude_desktop_config.json")
    } else if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA")?).join("Claude/claude_desktop_config.json")
    } else {
        home()?.join(".config/Claude/claude_desktop_config.json")
    };

    path.is_file().then_some(path)
}

fn skill_path(scope: &Scope) -> PathBuf {
    match scope {
        Scope::User => home()
            .unwrap_or_default()
            .join(".claude/skills/lucida/SKILL.md"),
        Scope::Project(dir) => dir.join(".claude/skills/lucida/SKILL.md"),
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Finds `program` on `PATH`, under any name the platform considers executable.
///
/// The bare name alone was enough until Windows became a release platform. There
/// `claude` on `PATH` is `claude.exe` or `claude.cmd` depending on how it was
/// installed, the extensionless file does not exist, and the whole of `setup`
/// then failed quietly: the Claude Code steps and the skill write were skipped,
/// and a machine without the desktop app reported "found neither Claude Code nor
/// the Claude app" while `claude` ran fine in the same terminal.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    // Only Windows makes the extension part of finding a program; elsewhere the
    // name on PATH is the name of the file.
    let pathext = cfg!(target_os = "windows").then(|| std::env::var("PATHEXT").unwrap_or_default());
    let names = candidate_names(program, pathext.as_deref());

    std::env::split_paths(&path).find_map(|dir| {
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// The filenames `program` could have, given `PATHEXT`.
///
/// `None` means a platform that does not use executable extensions. A pure
/// function because that is what makes the Windows answer checkable from Linux —
/// and this is a case where the platform's own CI lane is no help: it runs the
/// smoke script under git-bash, which resolves `claude` the POSIX way.
///
/// `PATHEXT` rather than a hardcoded pair, because it is the list the shell
/// itself consults, and a machine that has added `.ps1` to it means that. The
/// conventional four are a fallback for an unset or empty value, since taking it
/// literally would reproduce the bug this fixes.
fn candidate_names(program: &str, pathext: Option<&str>) -> Vec<String> {
    let Some(pathext) = pathext else {
        return vec![program.to_string()];
    };

    let mut extensions: Vec<String> = pathext
        .split(';')
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if extensions.is_empty() {
        extensions = [".com", ".exe", ".bat", ".cmd"]
            .iter()
            .map(|ext| (*ext).to_string())
            .collect();
    }

    // The bare name first: a PATH entry can hold an extensionless shim, and a
    // file that is there is a better answer than one that might be.
    let mut names = vec![program.to_string()];
    names.extend(extensions.into_iter().map(|ext| match ext.strip_prefix('.') {
        Some(bare) => format!("{program}.{bare}"),
        None => format!("{program}.{ext}"),
    }));
    names
}

/// Paths are printed with `~` because the full ones are long enough to wrap,
/// and a wrapped path is harder to check at a glance than a short one.
fn tilde(path: &Path) -> String {
    match home() {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}

fn confirm() -> Result<bool> {
    use std::io::{BufRead, Write};

    if !std::io::stdin().is_terminal() {
        bail!(
            "there is no terminal to confirm at.\n\n\
             Run `lucida setup --yes` to proceed without asking, or \
             `lucida setup --dry-run` to see the plan only."
        );
    }

    eprint!("\nApply this? [y/N] ");
    std::io::stderr().flush().ok();

    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `plan` had no tests, and this is what that cost.
    ///
    /// The skill was planned inside the Claude Code branch alone, while the
    /// closing note telling you where to upload it for the desktop app was
    /// printed whenever the app's config existed. On a machine with the Claude
    /// app and no Claude Code CLI — perfectly ordinary — setup finished by naming
    /// a file it had never written, and the instruction it gave you was to go and
    /// upload that file.
    ///
    /// All four combinations, three of which cannot be produced on the machine
    /// running this.
    #[test]
    fn a_skill_is_planned_whenever_either_client_is_present() {
        let exe = Path::new("/opt/lucida");
        // A path that does not exist, so `desktop_has_lucida` answers false
        // without this test depending on anything installed here.
        let config = PathBuf::from("/nonexistent/claude_desktop_config.json");

        let cases = [
            (true, true, true, "both clients"),
            (true, false, true, "Claude Code only"),
            (false, true, true, "the Claude app only"),
            (false, false, false, "neither"),
        ];

        for (claude_code, desktop, expect_skill, what) in cases {
            let clients = Clients {
                claude_code,
                desktop: desktop.then(|| config.clone()),
            };
            let planned = plan(&Scope::User, exe, &clients);

            if !claude_code && !desktop {
                assert!(planned.is_err(), "{what}: must refuse, not plan nothing");
                continue;
            }

            let steps = planned.unwrap_or_else(|e| panic!("{what}: {e}"));
            let has_skill = steps.iter().any(|s| matches!(s, Step::Skill { .. }));
            assert_eq!(has_skill, expect_skill, "{what}: skill step wrong");
        }
    }

    /// The desktop app has no notion of a project, so a project-scope run must
    /// not plan against it — including the note that names the skill file.
    #[test]
    fn a_project_scope_run_does_not_reach_for_the_desktop_app() {
        let clients = Clients::detect(&Scope::Project(PathBuf::from(".")));
        assert!(clients.desktop.is_none());
    }

    #[test]
    fn the_skill_lands_where_each_scope_expects_it() {
        let user = skill_path(&Scope::User);
        assert!(user.ends_with(".claude/skills/lucida/SKILL.md"), "{user:?}");

        let project = skill_path(&Scope::Project(PathBuf::from("/tmp/proj")));
        assert_eq!(
            project,
            PathBuf::from("/tmp/proj/.claude/skills/lucida/SKILL.md")
        );
    }

    #[test]
    fn merging_preserves_every_other_setting() {
        // The whole risk of touching someone's app config: the file holds
        // unrelated preferences, and adding a server must not disturb them.
        let dir = std::env::temp_dir().join(format!("lucida-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claude_desktop_config.json");

        std::fs::write(
            &path,
            r#"{"coworkUserFilesPath":"/somewhere","preferences":{"theme":"dark"},
                "mcpServers":{"other":{"command":"/bin/other","args":["mcp"]}}}"#,
        )
        .unwrap();

        merge_desktop_config(&path, Path::new("/usr/local/bin/lucida")).unwrap();

        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["coworkUserFilesPath"], "/somewhere");
        assert_eq!(after["preferences"]["theme"], "dark");
        assert_eq!(after["mcpServers"]["other"]["command"], "/bin/other");
        assert_eq!(
            after["mcpServers"]["lucida"]["command"],
            "/usr/local/bin/lucida"
        );
        assert_eq!(after["mcpServers"]["lucida"]["args"][0], "mcp");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_config_with_no_servers_gains_the_key() {
        let dir = std::env::temp_dir().join(format!("lucida-setup-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claude_desktop_config.json");

        // Exactly the shape found on a real machine before setup ran: the file
        // exists and has no mcpServers key at all.
        std::fs::write(&path, r#"{"preferences":{}}"#).unwrap();
        merge_desktop_config(&path, Path::new("/opt/lucida")).unwrap();

        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["mcpServers"]["lucida"]["command"], "/opt/lucida");
        assert!(after["preferences"].is_object());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An `mcpServers` of the wrong kind is an error, not a panic.
    ///
    /// `config["mcpServers"]["lucida"] = …` reads as total and is not:
    /// serde_json's `IndexMut` creates objects through `Null` but panics on a
    /// value that is present and not an object. A config holding
    /// `"mcpServers": []` — which nothing prevents — would therefore abort
    /// `lucida setup` with a backtrace, from the function documented as treating
    /// that file gently.
    #[test]
    fn a_malformed_server_list_is_refused_rather_than_panicking() {
        let dir = std::env::temp_dir().join(format!("lucida-setup-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        for (label, body) in [
            ("an array", r#"{"mcpServers":[]}"#),
            ("a string", r#"{"mcpServers":"none"}"#),
            ("a number", r#"{"mcpServers":3}"#),
        ] {
            let path = dir.join("claude_desktop_config.json");
            std::fs::write(&path, body).unwrap();

            let error = merge_desktop_config(&path, Path::new("/opt/lucida"))
                .expect_err(&format!("{label} was accepted"))
                .to_string();
            assert!(error.contains(label), "{error}");
            assert!(error.contains("lucida setup"), "must say what to do: {error}");

            // And the file it refused to understand is left exactly as it was.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }

        // `null` is not malformed — it is the shape indexing was relied on to
        // fill in, and it still is.
        let path = dir.join("claude_desktop_config.json");
        std::fs::write(&path, r#"{"mcpServers":null}"#).unwrap();
        merge_desktop_config(&path, Path::new("/opt/lucida")).unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["mcpServers"]["lucida"]["command"], "/opt/lucida");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The rewrite goes through a staged file, and does not leave it behind.
    ///
    /// A rename either happens or does not, which is the point — but a staging
    /// file left in someone's config directory is its own small mess, and this is
    /// the cheap half of the guarantee to check.
    #[test]
    fn merging_leaves_no_staging_file_behind() {
        let dir = std::env::temp_dir().join(format!("lucida-setup-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claude_desktop_config.json");
        std::fs::write(&path, r#"{"preferences":{"theme":"dark"}}"#).unwrap();

        merge_desktop_config(&path, Path::new("/opt/lucida")).unwrap();

        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(left, vec!["claude_desktop_config.json"], "{left:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// What `setup` looks for, pinned per platform on any platform.
    ///
    /// The bug: `claude` on a Windows `PATH` is `claude.exe` or `claude.cmd`, the
    /// bare name is not a file, and so detection failed while `claude` worked in
    /// the same terminal — silently, because an undetected client contributes no
    /// steps by design.
    #[test]
    fn a_program_is_looked_for_under_every_executable_extension() {
        // Unix: one name, the one asked for.
        assert_eq!(candidate_names("claude", None), vec!["claude"]);

        // Windows, with a real PATHEXT: the shell's own list, case-folded,
        // because the files on disk are lowercase.
        let names = candidate_names("claude", Some(".COM;.EXE;.BAT;.CMD;.VBS"));
        assert_eq!(names.first().unwrap(), "claude", "the bare name comes first");
        for expected in ["claude.exe", "claude.cmd", "claude.bat", "claude.vbs"] {
            assert!(names.contains(&expected.to_string()), "{names:?}");
        }

        // An unset or empty PATHEXT falls back rather than reproducing the bug:
        // an empty extension list would leave only the bare name again.
        for empty in ["", "   ", ";;"] {
            let names = candidate_names("claude", Some(empty));
            assert!(names.contains(&"claude.exe".to_string()), "{names:?}");
            assert!(names.contains(&"claude.cmd".to_string()), "{names:?}");
        }

        // A list written without the leading dots still produces one dot each.
        let names = candidate_names("claude", Some("EXE;CMD"));
        assert!(names.contains(&"claude.exe".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("..")), "{names:?}");
    }

    #[test]
    fn detection_reports_absence_rather_than_inventing_a_path() {
        // A machine without the app must yield None, never a path that setup
        // would then create. This is what makes the unverified Windows and
        // Linux locations safe to carry.
        assert!(which("a-program-that-is-not-installed-anywhere").is_none());
    }
}
