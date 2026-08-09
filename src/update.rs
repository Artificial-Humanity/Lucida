//! `lucida update` — replacing the running binary with the latest release.
//!
//! # Why this is not "just download and overwrite"
//!
//! Lucida arrives three ways — `cargo install --git`, a prebuilt release binary,
//! or a local `cargo build` — and **the update for one is wrong for another**.
//! Overwriting a `cargo install` binary with a downloaded release leaves cargo
//! believing it manages a file it no longer built, so the next
//! `cargo install --git` silently reverts the update. So the install source is
//! detected and each gets its own answer.
//!
//! The detection is deliberately conservative: a binary living in a cargo bin
//! directory came from cargo, and anything else is treated as a plain download.
//! That direction of error is the safe one — a downloaded binary in an unusual
//! place is still self-replaceable, while a cargo-managed binary must never be
//! written over.
//!
//! **Nothing here assumes a Rust toolchain.** cargo is only ever involved for
//! someone whose binary is already sitting in a cargo directory, which is to say
//! someone who used cargo to put it there. Everyone else gets a self-contained
//! download and replace: no compiler, no toolchain, nothing but the binary
//! replacing itself.
//!
//! Both paths end in an installed update rather than in advice. A cargo-managed
//! copy is rebuilt by running cargo — pinned to the release tag, so what gets
//! installed is the version that was just offered rather than whatever `main`
//! has become.
//!
//! # Nothing updates itself
//!
//! Installing is always user-triggered: `lucida update`, or whatever the user
//! automates around it. There is no path in this file that replaces a binary
//! without being asked to, and that is not squeamishness — it is the same rule
//! the rest of the codebase follows. Lucida refuses to drop a `--seed` it cannot
//! honour; a program that swapped itself for a different version unattended
//! would be committing the largest version of that sin available to it. v0.6.0
//! reversed config precedence and retired a setting name. Applying that to a
//! working machine at 3am, with nobody present to read why, is not a service.
//!
//! [`notify_if_due`] is the concession: at most once a day, on an interactive
//! terminal, it prints one line saying a newer release exists. It installs
//! nothing. See its own notes for the guards, which matter more than the check.

use crate::config;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where releases are published. A field on [`Updater`] so tests can aim it at
/// `testserver`, the same way every provider client does.
const RELEASES_API: &str = "https://api.github.com/repos/Artificial-Humanity/Lucida/releases/latest";

/// The releases page, for the cases this cannot resolve itself.
const RELEASES_PAGE: &str = "https://github.com/Artificial-Humanity/Lucida/releases/latest";

/// The repository, for the `cargo install` line.
const REPO: &str = "https://github.com/Artificial-Humanity/Lucida";

/// GitHub's API rejects a request with no User-Agent, with a message that does
/// not mention the header — so it is set explicitly rather than left to whatever
/// the HTTP client defaults to.
const USER_AGENT: &str = concat!("lucida/", env!("CARGO_PKG_VERSION"));

pub struct Updater {
    http: reqwest::blocking::Client,
    api: String,
}

/// Only the fields that are used. GitHub returns a great deal more, and naming
/// the rest would make an unrelated addition to their API a deserialisation
/// failure here.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// What `lucida update` should do once it knows both version numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Report only. Never prompts, never installs.
    Check,
    /// Report, then ask before replacing anything. The default.
    Ask,
    /// Report and install without asking — `--yes`, for automation.
    Yes,
}

/// Asks before replacing the running binary.
///
/// Refusing without a terminal is the point rather than an inconvenience: a
/// scripted `lucida update` that silently installed would be exactly the
/// unattended self-replacement this design does not do. Automation says so out
/// loud with `--yes`, which puts the decision back in a human's hands — the
/// person who wrote the script.
fn confirm() -> Result<bool> {
    use std::io::{BufRead, Write};

    if !std::io::stdin().is_terminal() {
        bail!(
            "a newer version is available, but there is no terminal to confirm at.\n\n\
             Run `lucida update --yes` to install without asking, or \
             `lucida update --check` to report only."
        );
    }

    // To stderr, so stdout carries the report and nothing else — the same
    // division `config --set` uses.
    eprint!("A newer version is available. Would you like to update? [y/N] ");
    std::io::stderr().flush().ok();

    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("reading your answer")?;

    // Anything that is not clearly yes is no, including a bare Enter. The
    // asymmetry is deliberate: the cost of a spurious "no" is running the
    // command again.
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// How this copy of Lucida got here, and therefore how it is updated.
#[derive(Debug, PartialEq, Eq)]
pub enum Install {
    /// Under a cargo bin directory: cargo owns it, so cargo must replace it.
    Cargo,
    /// A downloaded release binary, or anything else: replaceable in place.
    Standalone,
}

impl Updater {
    pub fn new() -> Result<Self> {
        Self::with_timeout(Duration::from_secs(120))
    }

    /// A short timeout for the background notice, a long one for a download.
    fn with_timeout(timeout: Duration) -> Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .connect_timeout(crate::retry::CONNECT_TIMEOUT)
                .build()
                .context("building HTTP client")?,
            api: RELEASES_API.to_string(),
        })
    }

    /// Reports both versions, then acts according to `mode`.
    ///
    /// Both numbers are always printed, including when they match. "You have
    /// the latest version" is more convincing next to the two figures it is a
    /// claim about, and it saves the follow-up question of what the latest
    /// actually is.
    pub fn run(&self, mode: Mode) -> Result<()> {
        let current = env!("CARGO_PKG_VERSION");
        let release = self.latest()?;
        let latest = release.tag_name.trim_start_matches('v');

        println!("Current version    {current}");
        println!("Available version  {latest}");
        println!();

        if !is_newer(latest, current)? {
            println!("You have the latest version of Lucida.");
            return Ok(());
        }

        match mode {
            Mode::Check => {
                println!("A newer version is available. Run `lucida update` to install it.");
                return Ok(());
            }
            Mode::Ask if !confirm()? => {
                println!("Not updated.");
                return Ok(());
            }
            _ => {}
        }

        let exe = std::env::current_exe().context("finding the running binary")?;
        // Resolved, because a symlink on PATH is common and the file to replace
        // is the target rather than the link.
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

        match install_kind(&exe) {
            Install::Cargo => reinstall_with_cargo(&exe, &release.tag_name),
            Install::Standalone => self.replace(&exe, &release, latest),
        }
    }

    fn latest(&self) -> Result<Release> {
        let response = self
            .http
            .get(&self.api)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/vnd.github+json")
            .send()
            .with_context(|| format!("asking {} for the latest release", self.api))?;

        let status = response.status();
        if !status.is_success() {
            // 403 here is nearly always the unauthenticated rate limit, which
            // resets on its own — worth saying, because "403" alone reads like a
            // permissions problem with no way forward.
            let hint = if status.as_u16() == 403 {
                "\n\nGitHub rate-limits unauthenticated requests by IP; this \
                 usually clears within the hour. Meanwhile the releases page \
                 has the binaries."
            } else {
                ""
            };
            bail!("could not check for updates: HTTP {status}{hint}\n\n{RELEASES_PAGE}");
        }

        response.json().context("reading the release description")
    }

    fn replace(&self, exe: &Path, release: &Release, version: &str) -> Result<()> {
        let wanted = asset_name(version)?;
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == wanted)
            .ok_or_else(|| {
                let available: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
                anyhow!(
                    "release {version} has no asset named `{wanted}` for this platform.\n\n\
                     It published: {}\n\n\
                     Download one by hand from {RELEASES_PAGE}",
                    available.join(", ")
                )
            })?;

        // Checked before spending a download on a file that cannot be installed.
        // A binary in /usr/local/bin owned by root is the common case, and the
        // useful message names the path rather than reporting errno 13 after a
        // 7 MB transfer.
        let dir = exe.parent().unwrap_or_else(|| Path::new("."));
        writable(dir, exe)?;

        println!("Downloading {}…", asset.name);
        let bytes = self.download(&asset.browser_download_url)?;

        // The published checksum, when the release carries one. It proves the
        // transfer arrived intact — a truncated or mangled download — and does
        // NOT prove provenance: GitHub serves the binary and the checksum over
        // the same connection, so a compromised repository supplies a matching
        // pair. Provenance needs a signature made elsewhere, which is the
        // code-signing item on the roadmap.
        if let Some(sums) = release.assets.iter().find(|a| a.name == format!("{wanted}.sha256")) {
            let published = self.download(&sums.browser_download_url)?;
            verify(&bytes, &String::from_utf8_lossy(&published))?;
            println!("Checksum verified.");
        } else {
            println!("No published checksum for this asset; skipping verification.");
        }

        install_over(exe, dir, &bytes)?;

        println!("Updated to {version}: {}", exe.display());
        Ok(())
    }

    fn download(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(url)
            .header("User-Agent", USER_AGENT)
            .send()
            .with_context(|| format!("downloading {url}"))?;

        if !response.status().is_success() {
            bail!("downloading {url}: HTTP {}", response.status());
        }

        Ok(response.bytes().context("reading the download")?.to_vec())
    }
}

/// How long a check is good for. A day, because the thing being reported
/// changes at most that often and a notice that appears every run is one people
/// learn to scroll past.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Setting that turns the notice off entirely. Listed in `config::KNOWN_KEYS`,
/// so `lucida config` reports it like any other setting rather than making it
/// folklore.
pub const OPT_OUT: &str = "LUCIDA_NO_UPDATE_CHECK";

/// One line, at most once a day, saying a newer release exists. Installs
/// nothing.
///
/// **Every guard here matters more than the check does**, because the failure
/// modes of a version check are all about where it runs rather than what it
/// finds:
///
/// - **Never in `mcp` mode.** The server is spawned and killed constantly by its
///   client, so a check per launch is a network round trip per launch — putting
///   back exactly the startup cost that shipping one static binary removed.
/// - **Only when stderr is a terminal.** That is the test for "a human is
///   watching", and it is stderr rather than stdout precisely because
///   `$(lucida generate …)` captures stdout while a person still reads stderr.
///   Scripts, pipelines and agents see nothing.
/// - **Printed to stderr, never stdout.** stdout carries the written path and
///   nothing else; that is load-bearing enough elsewhere to be worth restating.
/// - **After the command, not before.** The work is not delayed by a network
///   call, and a short timeout means a slow GitHub costs a few seconds at exit
///   rather than blocking a render.
/// - **Silent on every failure.** No network, rate-limited, unparseable
///   response, unwritable cache — none of that is worth a word. A notice that
///   could not be fetched is not news.
pub fn notify_if_due(current: &str) {
    if config::var(OPT_OUT).is_some() {
        return;
    }

    if !std::io::stderr().is_terminal() {
        return;
    }

    let Some(stamp) = stamp_path() else { return };
    if !is_due(last_checked(&stamp), SystemTime::now()) {
        return;
    }

    // Written before the request, not after: a GitHub that is down or
    // rate-limiting should cost one attempt a day, not one per invocation.
    record_check(&stamp);

    let Ok(updater) = Updater::with_timeout(Duration::from_secs(5)) else {
        return;
    };
    let Ok(release) = updater.latest() else { return };

    let latest = release.tag_name.trim_start_matches('v');
    if is_newer(latest, current).unwrap_or(false) {
        eprintln!(
            "note: lucida {latest} is available (this is {current}). \
             Run `lucida update`, or set {OPT_OUT}=1 to stop checking."
        );
    }
}

fn is_due(last: Option<SystemTime>, now: SystemTime) -> bool {
    match last {
        // A clock that has moved backwards makes the elapsed time an error
        // rather than a small number; treating that as "due" checks once too
        // often, which is the harmless direction.
        Some(last) => now.duration_since(last).map_or(true, |d| d >= CHECK_INTERVAL),
        None => true,
    }
}

/// A cache path, not a config path — this is a timestamp Lucida wrote, not a
/// setting anyone edits, and putting it beside the API keys would invite
/// treating the config directory as scratch space.
fn stamp_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Caches"))
    } else if cfg!(target_os = "windows") {
        // `%LOCALAPPDATA%` is where Windows keeps per-machine state that need not
        // roam, which is exactly what a timestamp is — it worked in
        // `%USERPROFILE%\.cache` too, since the directory is created either way,
        // but a dotfile cache directory is a convention from another platform.
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| home().map(|h| h.join(".cache")))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".cache")))
    };

    Some(base?.join("lucida").join("last-update-check"))
}

fn last_checked(path: &Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(path).ok()?;
    let secs: u64 = text.trim().parse().ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

fn record_check(path: &Path) {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, now.as_secs().to_string());
}

/// The `cargo install` arguments for a given release tag.
///
/// **`--tag` rather than the default branch, which is the part worth getting
/// right.** `cargo install --git <url>` builds whatever `main` happens to be,
/// so an update that had just announced "0.7.0 is available" would install
/// something else — main with whatever has landed since — and then report a
/// version the release page has never heard of. The tag installs the release
/// that was actually offered.
fn cargo_args(tag: &str) -> Vec<String> {
    ["install", "--git", REPO, "--tag", tag, "--force"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Rebuilds a cargo-installed copy, by running cargo.
///
/// Earlier this only printed the command, on the reasoning that `cargo install`
/// takes minutes and wants its own output on the terminal. The second half of
/// that is right and the conclusion did not follow: a child process inherits
/// this terminal by default, so cargo's progress and any compile error arrive
/// exactly as they would if it had been typed. What printing actually achieved
/// was making the user copy a line — a dead end at the moment they had just
/// said yes.
///
/// Falling back to printing when cargo cannot be found is still right, and is
/// the only case where a command is handed over rather than run.
fn reinstall_with_cargo(exe: &Path, tag: &str) -> Result<()> {
    let args = cargo_args(tag);
    let printable = format!("cargo {}", args.join(" "));

    // Respects CARGO, which is set when this is itself invoked from cargo, and
    // names the toolchain's own binary rather than whatever is first on PATH.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    println!(
        "\nThis copy was installed by cargo ({}), so cargo replaces it — which \
         means building from source, and that takes a few minutes.\n\n  {printable}\n",
        exe.display()
    );

    // Inheriting stdio, which is the whole point: cargo's output is the
    // progress report, and its compile errors are the diagnosis.
    match std::process::Command::new(&cargo).args(&args).status() {
        Ok(status) if status.success() => {
            println!("\nUpdated to {tag}.");
            Ok(())
        }
        Ok(status) => bail!(
            "`{printable}` exited with {status}, so nothing was replaced — the \
             copy you are running is untouched.\n\n\
             cargo's own output above says why."
        ),
        Err(e) => bail!(
            "could not run cargo ({e}), so this copy cannot be rebuilt here.\n\n\
             Run it yourself where cargo is available:\n\n  {printable}"
        ),
    }
}

/// Whether `exe` lives under a cargo bin directory.
///
/// `CARGO_HOME` first, since a non-default one is exactly the case a hardcoded
/// `~/.cargo` would get wrong.
pub fn install_kind(exe: &Path) -> Install {
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".cargo")))
        .map(|home| home.join("bin"));

    match cargo_bin {
        Some(bin) if exe.starts_with(&bin) => Install::Cargo,
        _ => Install::Standalone,
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The release asset for the platform this was built for.
///
/// Built from `std::env::consts` rather than a lookup table of every target,
/// and an unrecognised platform is an error naming the releases page rather
/// than a guess that downloads the wrong architecture.
pub fn asset_name(version: &str) -> Result<String> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);

    match (os, arch) {
        // One universal binary covers both Apple architectures, which is why
        // arch is not consulted here.
        ("macos", _) => Ok(format!("lucida-{version}-macos-universal")),
        ("linux", "x86_64") => Ok(format!("lucida-{version}-x86_64-linux-musl")),
        ("windows", "x86_64") => Ok(format!("lucida-{version}-x86_64-windows.exe")),
        _ => bail!(
            "no release binary is published for {os}/{arch}.\n\n\
             Build from source with `cargo build --release`, or see {RELEASES_PAGE}"
        ),
    }
}

/// Whether the new binary can actually be installed, checked before downloading.
fn writable(dir: &Path, exe: &Path) -> Result<()> {
    // Tested by writing, because permission bits do not answer the question on
    // their own: a directory can be mode 755 and still unwritable to this user,
    // and on Windows the bits mean something else entirely.
    let probe = dir.join(".lucida-update-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => bail!(
            "cannot write to {} ({e}), so {} cannot be replaced.\n\n\
             Re-run with permission to write there, or download the new binary \
             from {RELEASES_PAGE} and put it in place yourself.",
            dir.display(),
            exe.display()
        ),
    }
}

/// Puts `bytes` at `exe`, replacing what is there.
///
/// Written next to the target rather than in a temp directory, so the final step
/// is a rename within one filesystem — atomic, and with no copy across devices
/// that could leave a half-written binary at the destination.
fn install_over(exe: &Path, dir: &Path, bytes: &[u8]) -> Result<()> {
    let staged = dir.join(".lucida-update-staged");
    std::fs::write(&staged, bytes).with_context(|| format!("writing {}", staged.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .context("making the new binary executable")?;
    }

    // Windows refuses to replace a file that is being executed, but it does
    // allow renaming one — so the running binary is moved aside first and the
    // replacement takes its name. The displaced file cannot be deleted while it
    // runs; it is cleaned up on the next update instead.
    #[cfg(windows)]
    {
        let displaced = dir.join(".lucida-update-old.exe");
        let _ = std::fs::remove_file(&displaced);
        std::fs::rename(exe, &displaced).with_context(|| {
            format!("moving the running binary aside: {}", exe.display())
        })?;
        if let Err(e) = std::fs::rename(&staged, exe) {
            // Put it back, so a failure here does not leave the machine with no
            // lucida at all.
            let _ = std::fs::rename(&displaced, exe);
            return Err(e).with_context(|| format!("installing over {}", exe.display()));
        }
    }

    #[cfg(not(windows))]
    std::fs::rename(&staged, exe)
        .with_context(|| format!("installing over {}", exe.display()))?;

    Ok(())
}

/// Compares dotted versions numerically.
///
/// Numerically rather than as strings, because `0.10.0` sorts before `0.9.0`
/// lexicographically — the classic way a version check quietly stops offering
/// updates once a minor number reaches double digits.
fn is_newer(candidate: &str, current: &str) -> Result<bool> {
    Ok(parts(candidate)? > parts(current)?)
}

fn parts(version: &str) -> Result<(u64, u64, u64)> {
    // A pre-release suffix is ignored rather than rejected, so a tag like
    // `0.7.0-rc1` still compares by its numbers instead of failing the check.
    let core = version.trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);

    let mut fields = core.split('.').map(str::parse::<u64>);
    let mut next = || -> Result<u64> {
        fields
            .next()
            .transpose()
            .ok()
            .flatten()
            .ok_or_else(|| anyhow!("`{version}` is not a version this can compare"))
    };

    Ok((next()?, next()?, next()?))
}

/// Checks bytes against a `sha256sum`-style line: the hex digest, then the
/// filename.
fn verify(bytes: &[u8], published: &str) -> Result<()> {
    use sha2::{Digest, Sha256};

    let expected = published
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("the published checksum file was empty"))?
        .to_ascii_lowercase();

    let actual = format!("{:x}", Sha256::digest(bytes));

    if actual != expected {
        bail!(
            "the download does not match its published checksum, so it was not \
             installed.\n\n  expected {expected}\n  got      {actual}\n\n\
             Retry, and if it persists take the binary from {RELEASES_PAGE}"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, serve};

    #[test]
    fn versions_compare_numerically_not_as_text() {
        assert!(is_newer("0.7.0", "0.6.0").unwrap());
        assert!(is_newer("1.0.0", "0.9.9").unwrap());
        assert!(!is_newer("0.6.0", "0.6.0").unwrap());
        assert!(!is_newer("0.5.9", "0.6.0").unwrap());

        // The one a string comparison gets wrong, and the reason this is not a
        // string comparison.
        assert!(is_newer("0.10.0", "0.9.0").unwrap());
        assert!(!is_newer("0.9.0", "0.10.0").unwrap());
    }

    #[test]
    fn a_leading_v_and_a_prerelease_suffix_are_tolerated() {
        assert!(is_newer("v0.7.0", "0.6.0").unwrap());
        assert!(is_newer("0.7.0-rc1", "0.6.0").unwrap());
        assert!(parts("not-a-version").is_err());
    }

    #[test]
    fn the_asset_name_matches_what_the_release_workflow_publishes() {
        // Pinned against .github/workflows/release.yml. If that file renames an
        // asset, this test is what says so — the updater would otherwise fail
        // only on a user's machine, at the moment they tried to update.
        let name = asset_name("0.6.0").unwrap();
        assert!(name.starts_with("lucida-0.6.0-"), "{name}");
        match std::env::consts::OS {
            "macos" => assert_eq!(name, "lucida-0.6.0-macos-universal"),
            "linux" => assert_eq!(name, "lucida-0.6.0-x86_64-linux-musl"),
            "windows" => assert_eq!(name, "lucida-0.6.0-x86_64-windows.exe"),
            other => panic!("untested platform {other}"),
        }
    }

    #[test]
    fn a_check_is_due_once_a_day_and_survives_a_backwards_clock() {
        let now = SystemTime::now();

        assert!(is_due(None, now), "a machine that has never checked is due");
        assert!(is_due(Some(now - CHECK_INTERVAL), now));
        assert!(is_due(Some(now - CHECK_INTERVAL * 3), now));
        assert!(!is_due(Some(now), now), "twice in a row is not due");
        assert!(!is_due(Some(now - Duration::from_secs(60)), now));

        // A stamp in the future — a clock that moved backwards, or a file
        // copied between machines — makes the elapsed time an error rather
        // than a small number. Checking once too often is the harmless
        // direction; never checking again is not.
        assert!(is_due(Some(now + CHECK_INTERVAL), now));
    }

    #[test]
    fn the_stamp_is_a_cache_path_not_a_config_path() {
        // Beside the API keys would invite treating the config directory as
        // scratch space, and this is a timestamp Lucida wrote rather than a
        // setting anyone edits.
        let Some(path) = stamp_path() else { return };
        let text = path.to_string_lossy();
        assert!(text.contains("lucida"), "{text}");
        assert!(!text.contains("config.env"), "{text}");

        // Each platform's own location for machine-local state a tool generated.
        // Windows keeps that in %LOCALAPPDATA% rather than in a dotfile cache
        // directory borrowed from another platform — with `.cache` still accepted
        // there, since that is where the fallback lands when the variable is
        // unset.
        let accepted: &[&str] = if cfg!(target_os = "macos") {
            &["Caches"]
        } else if cfg!(target_os = "windows") {
            &["Local", "cache"]
        } else {
            &["cache"]
        };
        assert!(
            accepted.iter().any(|marker| text.contains(marker)),
            "not under this platform's cache location (wanted one of {accepted:?}): {text}"
        );
    }

    #[test]
    fn the_cargo_reinstall_is_pinned_to_the_release_tag() {
        let args = cargo_args("v0.7.0");
        assert_eq!(
            args,
            vec!["install", "--git", REPO, "--tag", "v0.7.0", "--force"]
        );

        // Without --tag, cargo builds the default branch — so an update that
        // announced 0.7.0 would install whatever main had become, then report a
        // version the release page has never heard of.
        assert!(args.contains(&"--tag".to_string()));
        assert!(args.contains(&"--force".to_string()));
    }

    #[test]
    fn a_cargo_installed_binary_is_recognised() {
        let home = PathBuf::from("/tmp/cargo-home-fixture");
        unsafe { std::env::set_var("CARGO_HOME", &home) };

        assert_eq!(install_kind(&home.join("bin/lucida")), Install::Cargo);
        assert_eq!(
            install_kind(Path::new("/usr/local/bin/lucida")),
            Install::Standalone
        );

        unsafe { std::env::remove_var("CARGO_HOME") };
    }

    #[test]
    fn a_checksum_mismatch_refuses_the_download() {
        // Vector for the empty string, so the expectation is checkable against
        // any sha256 implementation rather than one this test generated.
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify(b"", &format!("{empty}  lucida")).is_ok());
        assert!(verify(b"", &format!("{empty}  lucida").to_uppercase()).is_ok());

        let wrong = verify(b"different bytes", &format!("{empty}  lucida"));
        let message = wrong.unwrap_err().to_string();
        assert!(message.contains("does not match"), "{message}");
        assert!(message.contains("was not installed"), "{message}");
    }

    #[test]
    fn checking_reports_a_newer_release_without_installing() {
        let body = r#"{"tag_name":"v99.0.0","assets":[
            {"name":"lucida-99.0.0-macos-universal",
             "browser_download_url":"{{server}}/download"}]}"#;
        let server = serve(vec![Reply::json(body)]);

        let updater = Updater {
            http: reqwest::blocking::Client::new(),
            api: format!("{}/releases/latest", server.url()),
        };
        updater.run(Mode::Check).unwrap();

        let requests = server.finish();
        assert_eq!(requests.len(), 1, "a check must not download anything");
        // GitHub answers a request with no User-Agent with a message that does
        // not mention the header, so this is worth pinning.
        assert_eq!(requests[0].header("user-agent"), Some(USER_AGENT));
        assert_eq!(requests[0].header("accept"), Some("application/vnd.github+json"));
    }

    #[test]
    fn asking_with_no_terminal_refuses_rather_than_installing() {
        // The test harness has no terminal on stdin, which is the same
        // situation as a cron job or a CI step — and the one where installing
        // without being asked would be exactly the unattended self-replacement
        // this design does not do.
        let body = r#"{"tag_name":"v99.0.0","assets":[
            {"name":"lucida-99.0.0-macos-universal",
             "browser_download_url":"{{server}}/download"}]}"#;
        let server = serve(vec![Reply::json(body)]);

        let updater = Updater {
            http: reqwest::blocking::Client::new(),
            api: format!("{}/releases/latest", server.url()),
        };

        let message = updater.run(Mode::Ask).unwrap_err().to_string();
        assert!(message.contains("no terminal to confirm at"), "{message}");
        assert!(message.contains("--yes"), "{message}");

        let requests = server.finish();
        assert_eq!(
            requests.len(),
            1,
            "it must refuse before downloading anything"
        );
    }

    #[test]
    fn an_unavailable_release_api_names_the_releases_page() {
        let server = serve(vec![Reply::status(403, r#"{"message":"rate limit"}"#)]);
        let updater = Updater {
            http: reqwest::blocking::Client::new(),
            api: format!("{}/releases/latest", server.url()),
        };

        let message = updater.run(Mode::Check).unwrap_err().to_string();
        assert!(message.contains("rate-limit"), "{message}");
        assert!(message.contains(RELEASES_PAGE), "{message}");
        server.finish();
    }
}
