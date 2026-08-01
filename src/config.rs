//! Settings that survive not having a shell.
//!
//! Lucida read its credentials from the environment alone for its first two
//! versions, which is correct for a CLI and quietly broken for an MCP server.
//! A GUI-launched application on macOS does not inherit a login shell's
//! environment, so a `GOOGLE_API_KEY` exported in `~/.zshenv` is invisible to
//! Claude Code launched from the Dock — and therefore to every server it spawns.
//! The failure is confusing rather than obvious: the same binary works perfectly
//! from a terminal.
//!
//! The usual workarounds are both worse than a file. `launchctl setenv` exports
//! the secret to *every* process in the login session and does not survive a
//! reboot; putting the value in the MCP client's own config hardcodes a
//! credential into a JSON file that tends to get shared.
//!
//! So: an optional file of `KEY=value` lines, holding defaults for exactly the
//! variables Lucida already documents. The environment still wins when it has an
//! answer, which means every existing setup behaves identically and this is
//! purely a fallback.
//!
//! # Why not TOML
//!
//! Because the keys *are* environment variable names, and any other format would
//! invent a second vocabulary for the same six settings — `google.api_key` in a
//! file and `GOOGLE_API_KEY` in the environment, with a mapping table to keep in
//! sync. It also keeps the parser to a few lines and adds no dependency, which
//! is the same reasoning that kept a JSON-RPC crate out of `mcp.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Every setting Lucida will read from a config file, for `lucida config` and
/// for the template it writes.
pub const KNOWN_KEYS: &[(&str, &str)] = &[
    ("GOOGLE_API_KEY", "Google API key (GEMINI_API_KEY also accepted)"),
    ("GEMINI_API_KEY", "Alternative spelling of the above"),
    ("LUCIDA_COMFYUI_URL", "Where ComfyUI is listening"),
    ("LUCIDA_COMFYUI_AUTH", "ComfyUI credentials, if it is fenced"),
    ("LUCIDA_COMFYUI_CA", "PEM certificate for a private CA"),
];

struct Loaded {
    path: Option<PathBuf>,
    values: HashMap<String, String>,
}

static LOADED: OnceLock<Loaded> = OnceLock::new();

/// Looks up a setting: the environment first, then the config file.
///
/// An empty environment variable counts as absent. Exporting `GOOGLE_API_KEY=`
/// is how a shell profile reports "I meant to set this", and treating it as a
/// real value produces an authentication error instead of a useful one.
pub fn var(name: &str) -> Option<String> {
    if let Some(value) = std::env::var(name).ok().filter(|v| !v.trim().is_empty()) {
        return Some(value);
    }

    loaded().values.get(name).cloned()
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
         # Read only when the corresponding environment variable is unset, so this\n\
         # never overrides a shell that already knows the answer. It exists mainly\n\
         # for GUI-launched MCP clients, which do not inherit a login shell's\n\
         # environment — the case where an exported key is invisible.\n\
         #\n\
         # Keep this file private: chmod 600\n\n",
    );

    for (key, purpose) in KNOWN_KEYS {
        // GEMINI_API_KEY is only an alias; offering both invites setting both.
        if *key == "GEMINI_API_KEY" {
            continue;
        }
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
        assert!(text.contains("#GOOGLE_API_KEY="));
        // The alias may be *mentioned* — knowing it is accepted is useful — but
        // offering a second slot for the same credential invites setting both.
        assert!(!text.contains("#GEMINI_API_KEY="));

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
