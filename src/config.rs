//! Settings that survive not having a shell.
//!
//! Lucida read its credentials from the environment alone for its first two
//! versions, which is correct for a CLI and quietly broken for an MCP server.
//! A GUI-launched application on macOS does not inherit a login shell's
//! environment, so a `GEMINI_API_KEY` exported in `~/.zshenv` is invisible to
//! Claude Code launched from the Dock — and therefore to every server it spawns.
//! The failure is confusing rather than obvious: the same binary works perfectly
//! from a terminal.
//!
//! The usual workarounds are both worse than a file. `launchctl setenv` exports
//! the secret to *every* process in the login session and does not survive a
//! reboot; putting the value in the MCP client's own config hardcodes a
//! credential into a JSON file that tends to get shared.
//!
//! So: an optional file of `KEY=value` lines, holding values for exactly the
//! variables Lucida already documents.
//!
//! # Which wins
//!
//! The file, and this reverses the original rule — see [`var`] for why. In
//! short: a file entry is an explicit statement about Lucida, a shell export is
//! ambient and applies to everything, and the specific one should win.
//!
//! # Why not TOML
//!
//! Because the keys *are* environment variable names, and any other format would
//! invent a second vocabulary for the same seven settings — `gemini.api_key` in
//! a file and `GEMINI_API_KEY` in the environment, with a mapping table to keep in
//! sync. It also keeps the parser to a few lines and adds no dependency, which
//! is the same reasoning that kept a JSON-RPC crate out of `mcp.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Every setting Lucida will read from a config file, for `lucida config` and
/// for the template it writes.
pub const KNOWN_KEYS: &[(&str, &str)] = &[
    ("GEMINI_API_KEY", "Gemini API key — images and Veo video"),
    ("BFL_API_KEY", "Black Forest Labs API key (hosted FLUX)"),
    ("STABILITY_API_KEY", "Stability AI developer platform key"),
    ("OPENAI_API_KEY", "OpenAI API key"),
    ("LUCIDA_COMFYUI_URL", "Where ComfyUI is listening"),
    ("LUCIDA_COMFYUI_AUTH", "ComfyUI credentials, if it is fenced"),
    ("LUCIDA_COMFYUI_CA", "PEM certificate for a private CA"),
];

/// Names Lucida used to read and no longer does, with what replaced each.
///
/// A rename cannot simply delete the old name. Someone who exported
/// `GOOGLE_API_KEY` did nothing wrong, and dropping it silently turns a working
/// setup into "no API key found" — a message that sends them to check a key that
/// is present and correct. So the retired name is still *recognised*, purely to
/// say what happened, and never used as a credential.
///
/// `GOOGLE_API_KEY` was dropped in favour of `GEMINI_API_KEY` because everything
/// Lucida reaches on Google is the Gemini API — images and Veo alike — and after
/// Imagen's shutdown on 2026-08-17 there is nothing left that "Google" named
/// more accurately. One name, and it matches the one Google's own documentation
/// uses.
pub const RETIRED_KEYS: &[(&str, &str)] = &[("GOOGLE_API_KEY", "GEMINI_API_KEY")];

/// Whether `name` is a retired spelling, and what to use instead.
pub fn replacement_for(name: &str) -> Option<&'static str> {
    RETIRED_KEYS
        .iter()
        .find(|(old, _)| *old == name)
        .map(|(_, new)| *new)
}

/// A retired name this process can still see, from either source.
///
/// Reported by `lucida config` and by the credential error, so the diagnosis is
/// "you have the old name" rather than "you have no key".
pub fn retired_in_use() -> Vec<(&'static str, &'static str)> {
    RETIRED_KEYS
        .iter()
        .filter(|(old, _)| origin(old).is_some())
        .copied()
        .collect()
}

/// Setting names present in the config file, whether or not Lucida knows them.
///
/// Exists so `lucida config` can surface a key it does *not* recognise. A
/// mistyped name is otherwise perfectly silent: the file looks right, the value
/// is there, and the tool simply never reads it.
pub fn keys_in_file() -> Vec<String> {
    let mut names: Vec<String> = loaded().values.keys().cloned().collect();
    names.sort();
    names
}

struct Loaded {
    path: Option<PathBuf>,
    values: HashMap<String, String>,
}

static LOADED: OnceLock<Loaded> = OnceLock::new();

/// Looks up a setting: the config file first, then the environment.
///
/// **The file wins, and that reverses the rule this shipped with.** Through
/// v0.5.2 the environment won and the file was a fallback, on the reasoning that
/// introducing a config file must not change the behaviour of a setup that
/// already worked. That was a sound migration property, and it cost more than it
/// protected.
///
/// The case it made unreachable: a shell exports `OPENAI_API_KEY` for general
/// use, and you want Lucida to use a *different* key — a fine-grained one scoped
/// to this tool. There was no way to say so. `config --set` wrote the value,
/// reported success, and every render went on using the ambient key. Not merely
/// awkward: impossible, and silently so, which is the failure mode this codebase
/// refuses everywhere else.
///
/// A file entry is an explicit statement about Lucida specifically; a shell
/// export is ambient and applies to everything that reads that name. The
/// specific one should win. The one-off override survives as
/// `LUCIDA_CONFIG=other.env lucida …`, which names a whole file and wins
/// outright — see [`search_paths`].
///
/// An empty environment variable counts as absent. Exporting `GEMINI_API_KEY=`
/// is how a shell profile reports "I meant to set this", and treating it as a
/// real value produces an authentication error instead of a useful one. `parse`
/// drops empty file values for the same reason.
pub fn var(name: &str) -> Option<String> {
    if let Some(value) = loaded().values.get(name) {
        return Some(value.clone());
    }

    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Where a setting's value is coming from.
///
/// Exists so `lucida config` can report the *loser* as well as the winner. "Set
/// in both" and "set in one" resolve to the same value but not to the same
/// situation, and the whole class of bug here is about which source a process
/// actually reaches.
#[derive(Debug, PartialEq, Eq)]
pub enum Origin {
    /// In the file only.
    File,
    /// In the environment only.
    Environment,
    /// In both. The file is used; the environment value is not.
    FileOverridingEnvironment,
}

/// Which source is supplying `name`, if any.
pub fn origin(name: &str) -> Option<Origin> {
    origin_of(
        loaded().values.contains_key(name),
        std::env::var(name).is_ok_and(|v| !v.trim().is_empty()),
    )
}

/// The precedence table itself, separated from where the two answers come from.
///
/// Pulled out because `loaded()` is a process-wide `OnceLock` — the first test
/// to touch it fixes it for every other — so the resolution rule would otherwise
/// only be checkable from `scripts/smoke.sh`, in a fresh process. It is checked
/// there too, end to end; this pins the table itself.
fn origin_of(in_file: bool, in_env: bool) -> Option<Origin> {
    match (in_file, in_env) {
        (true, true) => Some(Origin::FileOverridingEnvironment),
        (true, false) => Some(Origin::File),
        (false, true) => Some(Origin::Environment),
        (false, false) => None,
    }
}

fn loaded() -> &'static Loaded {
    LOADED.get_or_init(|| {
        for path in search_paths() {
            if path.is_file() {
                let values = match std::fs::read_to_string(&path) {
                    Ok(text) => parse(&text),
                    Err(e) => {
                        // Never fatal: a config file is an optional convenience,
                        // and the environment may well hold everything needed.
                        eprintln!("warning: could not read {}: {e}", path.display());
                        continue;
                    }
                };
                warn_if_readable_by_others(&path);
                return Loaded {
                    path: Some(path),
                    values,
                };
            }
        }

        Loaded {
            path: None,
            values: HashMap::new(),
        }
    })
}

/// The file actually in use, if any.
pub fn source() -> Option<&'static Path> {
    loaded().path.as_deref()
}

/// Where a config file is looked for, in order.
///
/// `LUCIDA_CONFIG` names a file directly and wins outright, which is what makes
/// the whole thing testable and lets a launcher point at a managed location.
pub fn search_paths() -> Vec<PathBuf> {
    if let Some(explicit) = std::env::var("LUCIDA_CONFIG")
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        return vec![PathBuf::from(explicit)];
    }

    let mut paths = Vec::new();

    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home().map(|home| home.join(".config")))
    {
        paths.push(base.join("lucida").join("config.env"));
    }

    // Checked second rather than first even on macOS: someone with a dotfiles
    // repo expects `~/.config` to work everywhere, and the native location is
    // the more discoverable fallback rather than the more likely one.
    #[cfg(target_os = "macos")]
    if let Some(home) = home() {
        paths.push(
            home.join("Library")
                .join("Application Support")
                .join("lucida")
                .join("config.env"),
        );
    }

    paths
}

/// The preferred path, for messages that tell someone where to put a key.
pub fn preferred_path() -> Option<PathBuf> {
    search_paths().into_iter().next()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Parses `KEY=value` lines.
///
/// Deliberately forgiving in the two ways that matter in practice: a leading
/// `export` is accepted, so a fragment of a shell profile can be copied or
/// symlinked straight in, and surrounding quotes are stripped, because a key
/// pasted from documentation usually arrives wearing them.
fn parse(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        if key.is_empty() {
            continue;
        }

        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);

        if !value.is_empty() {
            values.insert(key.to_string(), value.to_string());
        }
    }

    values
}

/// Says something once if the file holding an API key is readable by others.
///
/// A warning rather than a refusal: it is the user's machine and their call, and
/// a tool that stops working over a permission bit is a tool people route
/// around.
#[cfg(unix)]
fn warn_if_readable_by_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Ok(metadata) = std::fs::metadata(path) {
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            eprintln!(
                "warning: {} is readable by other users (mode {:o}). It may hold an \
                 API key — consider `chmod 600 {}`.",
                path.display(),
                mode & 0o777,
                path.display()
            );
        }
    }
}

#[cfg(not(unix))]
fn warn_if_readable_by_others(_path: &Path) {}

/// A starter file, written by `lucida config --init`.
pub fn template() -> String {
    let mut text = String::from(
        "# Lucida settings.\n\
         #\n\
         # A value here takes precedence over the same name in the environment,\n\
         # so this is where a key scoped to Lucida goes when your shell already\n\
         # exports a broader one. It is also the only place a GUI-launched MCP\n\
         # client can find a key at all, since it inherits no shell.\n\
         #\n\
         # `lucida config` reports which source each setting is coming from.\n\
         #\n\
         # Keep this file private: chmod 600\n\n",
    );

    // Every known key, and only known keys — a retired name has no slot here,
    // since offering one would invite writing a value nothing reads.
    for (key, purpose) in KNOWN_KEYS {
        text.push_str(&format!("# {purpose}\n#{key}=\n\n"));
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_forms_a_shell_profile_produces() {
        let values = parse(
            "# a comment\n\
             \n\
             GOOGLE_API_KEY=plain\n\
             export LUCIDA_COMFYUI_URL=\"https://host:8188\"\n\
             	LUCIDA_COMFYUI_AUTH = 'bob:hunter2'  \n\
             MALFORMED\n\
             EMPTY=\n",
        );

        assert_eq!(values.get("GOOGLE_API_KEY").unwrap(), "plain");
        // `export` stripped, quotes stripped.
        assert_eq!(values.get("LUCIDA_COMFYUI_URL").unwrap(), "https://host:8188");
        assert_eq!(values.get("LUCIDA_COMFYUI_AUTH").unwrap(), "bob:hunter2");
        // A line with no `=` is skipped rather than fatal.
        assert!(!values.contains_key("MALFORMED"));
        // An empty value is the same as absent, so a later fallback still runs.
        assert!(!values.contains_key("EMPTY"));
    }

    #[test]
    fn a_retired_name_reports_its_replacement() {
        assert_eq!(replacement_for("GOOGLE_API_KEY"), Some("GEMINI_API_KEY"));
        assert_eq!(replacement_for("GEMINI_API_KEY"), None);
        assert_eq!(replacement_for("BFL_API_KEY"), None);

        // A retired name must not also be a live one, or `config --set` would
        // refuse to write a setting Lucida actually reads.
        for (retired, _) in RETIRED_KEYS {
            assert!(
                !KNOWN_KEYS.iter().any(|(known, _)| known == retired),
                "{retired} is both retired and current"
            );
        }
    }

    #[test]
    fn the_file_outranks_the_environment() {
        // Reversed after v0.5.2. The case that forced it: a shell exporting a broad
        // key made a Lucida-scoped one unreachable, since the ambient value won
        // and `config --set` reported success anyway.
        assert_eq!(
            origin_of(true, true),
            Some(Origin::FileOverridingEnvironment)
        );
        assert_eq!(origin_of(true, false), Some(Origin::File));
        assert_eq!(origin_of(false, true), Some(Origin::Environment));
        assert_eq!(origin_of(false, false), None);
    }

    #[test]
    fn a_value_containing_equals_survives() {
        // Base64 and tokens routinely end in `=`.
        let values = parse("LUCIDA_COMFYUI_AUTH=Basic dXNlcjpwdw==\n");
        assert_eq!(
            values.get("LUCIDA_COMFYUI_AUTH").unwrap(),
            "Basic dXNlcjpwdw=="
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        assert!(parse("# GOOGLE_API_KEY=nope\n\n   \n").is_empty());
    }

    #[test]
    fn the_template_only_offers_the_canonical_key_name() {
        let text = template();
        assert!(text.contains("#GEMINI_API_KEY="));

        // A retired name gets no slot. Offering one would invite writing a
        // value nothing reads — and a template is the worst place to learn that,
        // since it looks like the tool suggesting the name itself.
        for (retired, _) in RETIRED_KEYS {
            assert!(
                !text.contains(&format!("#{retired}=")),
                "the template offers the retired name {retired}"
            );
        }

        // Every line is inert until uncommented, so writing the file changes
        // nothing about how Lucida behaves.
        for line in text.lines().filter(|l| l.contains('=')) {
            assert!(
                line.trim_start().starts_with('#'),
                "template line is live: {line}"
            );
        }
    }

    /// The template must survive a round trip through the parser as a no-op —
    /// if a commented line ever parsed as a real one, `config --init` would
    /// silently blank out settings the environment was providing.
    #[test]
    fn the_template_parses_to_nothing() {
        assert!(parse(&template()).is_empty());
    }

    #[test]
    fn an_explicit_config_path_wins_outright() {
        // Uses the real environment, so pick a name nothing else sets.
        unsafe { std::env::set_var("LUCIDA_CONFIG", "/tmp/lucida-test-config.env") };
        let paths = search_paths();
        unsafe { std::env::remove_var("LUCIDA_CONFIG") };

        assert_eq!(paths, vec![PathBuf::from("/tmp/lucida-test-config.env")]);
    }
}
