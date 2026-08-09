#!/usr/bin/env bash
#
# Post-build checks against a binary that has already been built and packaged.
#
# Usage: smoke.sh <path-to-binary>
#
# WHAT THIS IS FOR. The release workflow produces artifacts `cargo test` never
# builds: a statically linked musl binary, a lipo-fused universal binary for the
# Mac, and an MSVC build for Windows. Each has its own ways of being broken that
# a debug build on Linux cannot show — a static link that cannot resolve a name,
# a fuse that produced a file no loader will run, a release profile whose LTO
# miscompiled something. So the packaged artifact has to be run, not just built.
#
# WHAT THIS IS NOT. It is no longer a second, weaker copy of the test suite.
# Every behavioural assertion — exit codes, `--json` on stdout alone, the config
# search path, the capability refusals, JSON-RPC framing — lives in
# `tests/cli.rs` and runs under `cargo test` on every push. This script points
# that same file at the artifact it was handed, so there is one set of
# assertions covering both the binary a developer just built and the binary a
# user will download.
#
# It used to hold about fifty assertions written in bash. They were good checks
# in the wrong place: they could not run before a commit, they needed a release
# build first, and one of them silently stopped asserting for a while because
# `cmd | grep -q x && echo ok` is not a check — bash does not apply `set -e` to
# the left of `&&`, so a false assertion just skipped its echo and CI stayed
# green. A `#[test]` cannot fail that way.
#
# Deliberately no `set -e`, for the reason above: every check here is explicit.

BIN=${1:?usage: smoke.sh <path-to-binary>}
failures=0

pass() { printf '  ok   %s\n' "$1"; }
fail() { printf '  FAIL %s\n' "$1"; failures=$((failures + 1)); }

# Absolute, because the delegation below runs cargo from the repository root
# while the artifact is usually named relative to wherever the caller stood.
case "$BIN" in
  /*|[A-Za-z]:[\\/]*) ;;
  *) BIN="$PWD/$BIN" ;;
esac

if [ ! -x "$BIN" ] && [ ! -f "$BIN" ]; then
  printf 'no such binary: %s\n' "$BIN"
  exit 1
fi

# --- the artifact itself ----------------------------------------------------
# Everything a packaging step can break, and nothing that `tests/cli.rs` already
# covers. If the file will not load or cannot print its own version, the run
# below would fail thirty times over with a less useful message.

version=$("$BIN" --version 2>&1)
case "$version" in
  lucida\ *) pass "runs: $version" ;;
  *)         fail "--version printed: $version" ;;
esac

"$BIN" --help >/dev/null 2>&1 && pass "--help" || fail "--help exited non-zero"

if [ "$failures" -gt 0 ]; then
  printf '\nthe artifact does not run; not attempting the behaviour suite\n'
  exit 1
fi

# --- the behaviour suite, against this artifact -----------------------------
# `tests/cli.rs` honours LUCIDA_TEST_BIN, so the assertions that ran against the
# debug build in CI run again here against the packaged one.
#
# This does build the crate once more (an integration test depends on the bin
# target whether or not it ends up using it). That is a minute of a release job,
# in exchange for the release artifact being covered to the same depth as
# everything else rather than by a handful of shell greps.

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if ! command -v cargo >/dev/null 2>&1; then
  printf '\ncargo is not on PATH, so the behaviour suite cannot run against this\n'
  printf 'artifact. That is a gap, not a pass — install a toolchain, or run\n'
  printf '`cargo test` separately and treat the artifact as unverified.\n'
  exit 1
fi

printf '\nBehaviour suite (tests/cli.rs against this binary):\n'
if (cd "$repo" && LUCIDA_TEST_BIN="$BIN" cargo test --locked --test cli); then
  pass "tests/cli.rs passes against the packaged binary"
else
  fail "tests/cli.rs failed against the packaged binary"
fi

# --- verdict ----------------------------------------------------------------
if [ "$failures" -gt 0 ]; then
  printf '\n%d check(s) failed\n' "$failures"
  exit 1
fi
printf '\nall checks passed\n'
