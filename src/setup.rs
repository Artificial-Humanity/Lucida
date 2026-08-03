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

    let steps = plan(&scope, &exe)?;

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

    // Only worth saying where the app is actually installed, and only for a
    // user-scope run, since the desktop app has no notion of a project.
    if matches!(scope, Scope::User) && desktop_config().is_some() {
        println!("{DESKTOP_SKILL_NOTE}\n  {}", tilde(&skill_path(&scope)));
    }

    Ok(())
}

/// What would be done, without doing any of it.
///
/// Built entirely from what is present: a client that is not installed
/// contributes no steps rather than an error, because "set this up wherever it
/// applies" is the request, and a machine with only one of the two is normal.
fn plan(scope: &Scope, exe: &Path) -> Result<Vec<Step>> {
    let mut steps = Vec::new();

    if which("claude").is_some() {
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

        steps.push(Step::Skill {
            path: skill_path(scope),
        });
    }

    // The desktop app has no notion of a project, so it is only touched for a
    // user-scope run. Saying so beats silently ignoring the flag.
    if let Scope::User = scope
        && let Some(path) = desktop_config()
    {
        if desktop_has_lucida(&path, exe)? {
            steps.push(Step::AlreadyDone {
                what: "the Claude app already registers this binary".into(),
            });
        } else {
            steps.push(Step::DesktopMcp { path });
        }
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

    config["mcpServers"]["lucida"] = json!({
        "command": exe.to_string_lossy(),
        "args": ["mcp"],
    });

    let mut body = serde_json::to_string_pretty(&config)?;
    body.push('\n');
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
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

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(program);
        candidate.is_file().then_some(candidate)
    })
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

    #[test]
    fn detection_reports_absence_rather_than_inventing_a_path() {
        // A machine without the app must yield None, never a path that setup
        // would then create. This is what makes the unverified Windows and
        // Linux locations safe to carry.
        assert!(which("a-program-that-is-not-installed-anywhere").is_none());
    }
}
