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
//! **Nothing here assumes a Rust toolchain.** A cargo command is only ever
//! printed to someone whose binary is already sitting in a cargo directory,
//! which is to say someone who used cargo to put it there. Everyone else gets a
//! self-contained download and replace: no compiler, no toolchain, nothing but
//! the binary replacing itself.
//!
//! # No automatic checks
//!
//! Nothing calls this on startup, and nothing should. The MCP server is spawned
//! and killed constantly by its client, so a version check at startup would be a
//! network round trip per launch — and a network call nobody asked for is a
//! surprise regardless of its cost. `lucida update` and `lucida update --check`
//! are both explicit.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

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
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .context("building HTTP client")?,
            api: RELEASES_API.to_string(),
        })
    }

    /// Reports what is available, and if `apply` is set, installs it.
    pub fn run(&self, apply: bool) -> Result<()> {
        let current = env!("CARGO_PKG_VERSION");
        let release = self.latest()?;
        let latest = release.tag_name.trim_start_matches('v');

        if !is_newer(latest, current)? {
            println!("lucida {current} is the latest release.");
            return Ok(());
        }

        println!("lucida {latest} is available (this is {current}).");

        if !apply {
            println!("\nRun `lucida update` to install it.");
            return Ok(());
        }

        let exe = std::env::current_exe().context("finding the running binary")?;
        // Resolved, because a symlink on PATH is common and the file to replace
        // is the target rather than the link.
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

        match install_kind(&exe) {
            // Printed rather than run. `cargo install` can take minutes and
            // wants its own output on the terminal; wrapping it would hide a
            // compile error behind a spinner.
            Install::Cargo => {
                println!(
                    "\nThis copy was installed by cargo ({}), so cargo should \
                     replace it:\n\n  cargo install --git {REPO} --force",
                    exe.display()
                );
                Ok(())
            }
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
        updater.run(false).unwrap();

        let requests = server.finish();
        assert_eq!(requests.len(), 1, "a check must not download anything");
        // GitHub answers a request with no User-Agent with a message that does
        // not mention the header, so this is worth pinning.
        assert_eq!(requests[0].header("user-agent"), Some(USER_AGENT));
        assert_eq!(requests[0].header("accept"), Some("application/vnd.github+json"));
    }

    #[test]
    fn an_unavailable_release_api_names_the_releases_page() {
        let server = serve(vec![Reply::status(403, r#"{"message":"rate limit"}"#)]);
        let updater = Updater {
            http: reqwest::blocking::Client::new(),
            api: format!("{}/releases/latest", server.url()),
        };

        let message = updater.run(false).unwrap_err().to_string();
        assert!(message.contains("rate-limit"), "{message}");
        assert!(message.contains(RELEASES_PAGE), "{message}");
        server.finish();
    }
}
