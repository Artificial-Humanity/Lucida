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
# An isolated HOME, so a config file on the machine running these checks cannot
# supply the key and turn the assertion into a no-op. Shared with the config
# checks below.
sandbox=$(mktemp -d)
trap 'rm -rf "$sandbox"' EXIT

# Compared by directory NAME rather than full path. Under Git Bash on Windows,
# `mktemp -d` yields a Unix-style /tmp/tmp.XXXX while the native binary
# correctly prints C:\Users\RUNNER~1\AppData\Local\Temp\tmp.XXXX — two
# spellings of one directory, which a substring match on the full path calls a
# failure. The unique name appears in both.
sandbox_name=$(basename "$sandbox")

out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin "$BIN" models 2>&1 || true)
case "$out" in
  *"no API key found"*) pass "missing key reports cleanly" ;;
  *panicked*)           fail "panicked without a key: $out" ;;
  *)                    fail "unexpected no-key output: $out" ;;
esac

# The message must point at the fix for the case that actually bites: a process
# with no shell environment.
case "$out" in
  *"lucida config"*) pass "no-key message names the config file" ;;
  *)                 fail "no-key message gives no way forward: $out" ;;
esac

# --- config file resolution -----------------------------------------------
# The regression that matters here is subtle: an MCP server launched by a GUI
# application has no shell environment, and until 0.3.0 that made an exported key
# invisible with no way to recover. `env -i` reproduces exactly that.
out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin "$BIN" config --init 2>&1)
case "$out" in
  *"$sandbox_name"*config.env*) pass "config --init writes into a bare HOME" ;;
  *)                       fail "config --init: $out" ;;
esac

printf 'GOOGLE_API_KEY=smoke-test-value\n' >> "$sandbox/.config/lucida/config.env"

# The key must be visible with NO environment whatsoever.
out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin "$BIN" config 2>&1)
case "$out" in
  *"GOOGLE_API_KEY"*"set (config file)"*) pass "config file is read with no environment" ;;
  *) fail "config file was not picked up: $out" ;;
esac

# …and the environment must still take precedence over it.
out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin GOOGLE_API_KEY=x "$BIN" config 2>&1)
case "$out" in
  *"GOOGLE_API_KEY"*"set (environment)"*) pass "environment beats the config file" ;;
  *) fail "config file overrode the environment: $out" ;;
esac

# Values must never be printed — this output gets pasted into bug reports.
case "$out" in
  *smoke-test-value*) fail "config printed a secret value" ;;
  *)                  pass "config never prints values" ;;
esac

# --- capability guards ----------------------------------------------------
# Runnable with no credentials and no server, which is the point: whether Google
# has a seed is not a fact about your API key. If this ever starts reporting a
# missing key instead, the check has been moved back behind client construction
# and the message has become useless.
out=$(env -u GOOGLE_API_KEY -u GEMINI_API_KEY "$BIN" generate "x" --seed 1 2>&1 || true)
case "$out" in
  *"no concept of a seed"*comfyui*) pass "unsupported seed names a provider that has one" ;;
  *"no API key found"*)             fail "capability check ran after credentials: $out" ;;
  *)                                fail "unexpected --seed output: $out" ;;
esac

out=$(env -u GOOGLE_API_KEY -u GEMINI_API_KEY "$BIN" generate "x" --aspect 7:3 2>&1 || true)
case "$out" in
  *"supports only these aspect ratios"*) pass "unsupported aspect ratio is rejected" ;;
  *)                                     fail "unexpected --aspect output: $out" ;;
esac

# --- a provider that is not there -----------------------------------------
# The likeliest failure for the local lane by a wide margin, and it must name the
# server and the variable rather than surfacing a raw connection error.
out=$(LUCIDA_COMFYUI_URL="http://127.0.0.1:1" "$BIN" models --provider comfyui 2>&1 || true)
case "$out" in
  *"could not reach ComfyUI"*LUCIDA_COMFYUI_URL*) pass "unreachable ComfyUI explains itself" ;;
  *panicked*)                                     fail "panicked on unreachable server: $out" ;;
  *)                                              fail "unexpected unreachable output: $out" ;;
esac

# --- the MCP stdio transport ----------------------------------------------
# Worth testing separately from the CLI: framing can break in ways no ordinary
# command would reveal, and on Windows line endings are the plausible culprit.
handshake='{"jsonrpc":"2.0","id":1,"method":"tools/list"}'

reply=$(printf '%s\n' "$handshake" | "$BIN" mcp 2>/dev/null)
case "$reply" in
  *generate_image*image_providers*start_video*check_video*)
    pass "MCP tools/list (LF input)" ;;
  *) fail "MCP tools/list did not list all four tools: $reply" ;;
esac

# Every provider must be reachable through the schema, not merely implemented.
#
# Checked by NAME rather than by phrase. The previous version grepped for the
# literal "comfyui only" — true until a second provider gained a negative prompt
# and the text correctly became "comfyui and stability only", at which point the
# check failed on an improvement. Names are what an agent selects on, and they
# are what a new provider must not be missing from.
for provider in google comfyui bfl stability; do
  case "$reply" in
    *"\"$provider\""*) pass "schema offers provider: $provider" ;;
    *) fail "provider $provider is missing from the MCP schema" ;;
  esac
done

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
