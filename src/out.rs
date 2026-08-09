//! Exit codes and machine-readable output.
//!
//! Everything used to exit 0 or 1, which collapses three outcomes a caller has
//! to tell apart into two. The sharpest case is `lucida check`: while a render
//! is still working it printed a sentence to stderr, wrote nothing to stdout,
//! and exited 0 — indistinguishable, to a script, from a render that had
//! finished and been written. A polling loop built on that either spins forever
//! or gives up on a render it has already paid for.
//!
//! So there are four outcomes, and each has a number:
//!
//! | code | meaning |
//! |---|---|
//! | 0 | done — the thing was produced |
//! | 1 | something went wrong |
//! | 2 | refused before anything was spent — a capability or a budget said no |
//! | 3 | still working; ask again later |
//!
//! **2 is worth the separate code.** A refusal is not a failure, it is an answer
//! — the request was understood and declined before the money moved, and the
//! message names what to do instead. A wrapper that retries on 1 should not
//! retry on 2, because retrying cannot succeed.
//!
//! # `--json`
//!
//! One JSON object on stdout, whatever happens, including on failure — a caller
//! parsing output should not have to switch parsers depending on the outcome.
//! Human prose goes to stderr, so `--json` output stays a single clean document
//! and the two never mix.

use serde_json::{Value, json};
use std::sync::OnceLock;

pub const OK: i32 = 0;
pub const ERROR: i32 = 1;
pub const REFUSED: i32 = 2;
pub const PENDING: i32 = 3;

/// A request that was understood and declined before anything was spent.
///
/// Carried as an error so it travels the existing `?` paths untouched, and
/// recognised by [`code_for`] on the way out. The alternative — a bespoke result
/// type threaded through every call site — would be a large change to say one
/// small thing.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// The exit code an error deserves.
pub fn code_for(error: &anyhow::Error) -> i32 {
    if error.downcast_ref::<Refused>().is_some() {
        REFUSED
    } else {
        ERROR
    }
}

static JSON: OnceLock<bool> = OnceLock::new();

/// Set once, at startup, from `--json`.
///
/// A global rather than a parameter threaded through a dozen printing sites: it
/// is written once before any work begins and only ever read afterwards, which
/// is the one shape where ambient state is honest — every caller wants the same
/// answer and none of them can change it.
pub fn set_json(on: bool) {
    let _ = JSON.set(on);
}

pub fn json() -> bool {
    *JSON.get().unwrap_or(&false)
}

/// Prints the single JSON document, if `--json` was asked for.
pub fn emit(value: Value) {
    if json() {
        println!("{value}");
    }
}

/// The failure document, so a `--json` caller parses one shape either way.
pub fn emit_error(error: &anyhow::Error, code: i32) {
    emit(json!({
        "ok": false,
        "error": format!("{error:#}"),
        // Named as well as numbered, because a wrapper reading this should not
        // have to remember what 2 means to know not to retry.
        "refused": code == REFUSED,
        "exit_code": code,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refusal must be distinguishable from a failure, or the code is
    /// decoration: retrying a refusal cannot succeed, and retrying a failure
    /// often can.
    #[test]
    fn a_refusal_is_not_an_ordinary_error() {
        let refused = anyhow::Error::new(Refused("no seed on google".into()));
        assert_eq!(code_for(&refused), REFUSED);

        let broke = anyhow::anyhow!("the connection dropped");
        assert_eq!(code_for(&broke), ERROR);
    }

    /// A refusal keeps its message. The whole value of one is that it names a
    /// way forward, so wrapping it must not swallow the sentence that does.
    #[test]
    fn a_refusal_carries_its_message_through_context() {
        let refused = anyhow::Error::new(Refused("use comfyui, which is free".into()))
            .context("generating an image");

        assert_eq!(code_for(&refused), REFUSED, "context must not hide the kind");
        assert!(
            format!("{refused:#}").contains("use comfyui"),
            "the way forward was lost: {refused:#}"
        );
    }

    /// The four outcomes have to be four different numbers, which is the entire
    /// point — `check` used to report "still rendering" and "finished and
    /// written" with the same one.
    #[test]
    fn every_outcome_has_its_own_code() {
        let codes = [OK, ERROR, REFUSED, PENDING];
        let unique: std::collections::BTreeSet<i32> = codes.into_iter().collect();
        assert_eq!(unique.len(), codes.len(), "two outcomes share a code");
    }
}
