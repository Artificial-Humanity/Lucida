#!/usr/bin/env bash
#
# Post-build checks, shared by every platform in the release workflow so the
# three jobs cannot drift apart.
#
# Deliberately does NOT use `set -e`. An earlier version relied on it and one
# assertion silently stopped asserting: `cmd | grep -q x && echo ok` looks like a
# check, but bash does not apply `set -e` to a command failing on the left of
# `&&`, so a false assertion just skipped its echo and the build stayed green.
# `pipefail` made that worse — `lucida` exits non-zero on the very error being
# asserted, so the pipeline reported failure even when the assertion held.
#
# Every check here is therefore explicit: capture output, compare, call fail().
#
# Usage: smoke.sh <path-to-binary>

BIN=${1:?usage: smoke.sh <path-to-binary>}
failures=0

pass() { printf '  ok   %s\n' "$1"; }
fail() { printf '  FAIL %s\n' "$1"; failures=$((failures + 1)); }

# --- it runs at all -------------------------------------------------------
version=$("$BIN" --version 2>&1)
case "$version" in
  lucida\ *) pass "runs: $version" ;;
  *)         fail "--version printed: $version" ;;
esac

"$BIN" --help >/dev/null 2>&1 && pass "--help" || fail "--help exited non-zero"

# --- missing credentials fail cleanly, rather than panicking ---------------
# `|| true` because a non-zero exit is the correct behaviour here; the exit code
# is not what is under test, the message is.
out=$(env -u GOOGLE_API_KEY -u GEMINI_API_KEY "$BIN" models 2>&1 || true)
case "$out" in
  *"no API key found"*) pass "missing key reports cleanly" ;;
  *panicked*)           fail "panicked without a key: $out" ;;
  *)                    fail "unexpected no-key output: $out" ;;
esac

# --- the MCP stdio transport ----------------------------------------------
# Worth testing separately from the CLI: framing can break in ways no ordinary
# command would reveal, and on Windows line endings are the plausible culprit.
handshake='{"jsonrpc":"2.0","id":1,"method":"tools/list"}'

reply=$(printf '%s\n' "$handshake" | "$BIN" mcp 2>/dev/null)
case "$reply" in
  *generate_image*start_video*check_video*) pass "MCP tools/list (LF input)" ;;
  *) fail "MCP tools/list did not list all three tools: $reply" ;;
esac

# A client on Windows may terminate requests with CRLF.
reply=$(printf '%s\r\n' "$handshake" | "$BIN" mcp 2>/dev/null)
case "$reply" in
  *generate_image*) pass "MCP tools/list (CRLF input)" ;;
  *) fail "CRLF-terminated request was not parsed: $reply" ;;
esac

# And the server must never emit CR itself: newline framing is part of the
# JSON-RPC contract, so a "helpful" CRLF would corrupt the stream.
if printf '%s\n' "$handshake" | "$BIN" mcp 2>/dev/null | od -c | grep -q '\\r'; then
  fail "server emitted CR in its output"
else
  pass "server output is LF-only"
fi

# A notification carries no id and must draw no reply at all.
reply=$(printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}' | "$BIN" mcp 2>/dev/null)
if [ -z "$reply" ]; then
  pass "notification correctly draws no response"
else
  fail "replied to a notification: $reply"
fi

# --- verdict --------------------------------------------------------------
if [ "$failures" -gt 0 ]; then
  printf '\n%d check(s) failed\n' "$failures"
  exit 1
fi
printf '\nall checks passed\n'
