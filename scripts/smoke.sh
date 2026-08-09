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

# ...and it must still say what the provider can do. Whether Google has a seed is
# not a fact about your credentials, and this command used to return before
# printing the table the moment a client could not be built — so the answer that
# needed no key was the one you could not get without one.
case "$out" in
  *"This provider supports:"*) pass "capabilities print without a key" ;;
  *)                           fail "no capability table without a key: $out" ;;
esac
case "$out" in
  *"output carries"*) pass "provenance prints without a key" ;;
  *)                  fail "no provenance line without a key: $out" ;;
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

printf 'GEMINI_API_KEY=smoke-test-value\n' >> "$sandbox/.config/lucida/config.env"

# The key must be visible with NO environment whatsoever.
out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin "$BIN" config 2>&1)
case "$out" in
  *"GEMINI_API_KEY"*"set (config file)"*) pass "config file is read with no environment" ;;
  *) fail "config file was not picked up: $out" ;;
esac

# …and it must beat the environment, which is what makes a key scoped to Lucida
# reachable at all when the shell already exports a broader one. This assertion
# was the other way round through v0.5.2; see config::var for why it changed.
out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin GEMINI_API_KEY=x "$BIN" config 2>&1)
case "$out" in
  *"GEMINI_API_KEY"*"set (config file)"*) pass "config file beats the environment" ;;
  *) fail "environment overrode the config file: $out" ;;
esac

# The losing source must be named, not merely out-ranked. Someone reading this
# output is usually asking why the key they exported is not being used.
case "$out" in
  *"not used — the config file wins"*) pass "the shadowed environment value is reported" ;;
  *) fail "config did not report the shadowed environment value: $out" ;;
esac

# A renamed setting must be diagnosed, not merely absent. Someone holding
# GOOGLE_API_KEY has a key that is present and correct, so "no API key found"
# would send them to check the one thing that is not wrong.
#
# A separate HOME with no config file, because the sandbox above now holds a
# GEMINI_API_KEY — and the notice is deliberately silent once the replacement
# is set. Reusing that sandbox would assert the opposite of the behaviour.
migration=$(mktemp -d)
out=$(env -i HOME="$migration" PATH=/usr/bin:/bin GOOGLE_API_KEY=x "$BIN" config 2>&1)
case "$out" in
  *"no longer read"*GEMINI_API_KEY*) pass "a retired key name names its replacement" ;;
  *) fail "retired GOOGLE_API_KEY was not reported: $out" ;;
esac

# …and once the replacement is set, the migration is over and the old name is
# just another variable Lucida does not read. A permanent notice about a
# non-problem is one people learn to skip past.
out=$(env -i HOME="$migration" PATH=/usr/bin:/bin GOOGLE_API_KEY=x GEMINI_API_KEY=y "$BIN" config 2>&1)
case "$out" in
  *"no longer read"*) fail "the retired name was still reported after migrating: $out" ;;
  *) pass "a completed migration says nothing about the old name" ;;
esac
rm -rf "$migration"

# …and setting it must be refused rather than written, since a written value
# nothing reads is the silent drop this design exists to refuse.
out=$(echo v | env -i HOME="$sandbox" PATH=/usr/bin:/bin "$BIN" config --set GOOGLE_API_KEY 2>&1 || true)
case "$out" in
  *"no longer read"*"--set GEMINI_API_KEY"*) pass "config --set refuses a retired name" ;;
  *) fail "config --set accepted a retired name: $out" ;;
esac

# --remove exists so changing a key does not mean remembering where the file is.
out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin "$BIN" config --remove GEMINI_API_KEY 2>&1)
case "$out" in
  *"Removed GEMINI_API_KEY"*) pass "config --remove deletes a setting" ;;
  *) fail "config --remove: $out" ;;
esac

out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin "$BIN" config 2>&1)
case "$out" in
  *"GEMINI_API_KEY"*"not set"*) pass "the removed setting is gone" ;;
  *) fail "the setting survived removal: $out" ;;
esac

# Removing what was never there is idempotent, but never silent — it is a typo
# often enough to be worth saying out loud.
out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin "$BIN" config --remove GEMINI_API_KEY 2>&1)
case "$out" in
  *"nothing to remove"*) pass "removing an absent setting says so" ;;
  *) fail "unexpected --remove output: $out" ;;
esac

# Restored, so the checks below still have a file to read.
printf 'GEMINI_API_KEY=smoke-test-value\n' >> "$sandbox/.config/lucida/config.env"
out=$(env -i HOME="$sandbox" PATH=/usr/bin:/bin GEMINI_API_KEY=x "$BIN" config 2>&1)

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
out=$(env -u GEMINI_API_KEY "$BIN" generate "x" --seed 1 2>&1 || true)
case "$out" in
  *"no concept of a seed"*comfyui*) pass "unsupported seed names a provider that has one" ;;
  *"no API key found"*)             fail "capability check ran after credentials: $out" ;;
  *)                                fail "unexpected --seed output: $out" ;;
esac

out=$(env -u GEMINI_API_KEY "$BIN" generate "x" --aspect 7:3 2>&1 || true)
case "$out" in
  *"supports only these aspect ratios"*) pass "unsupported aspect ratio is rejected" ;;
  *)                                     fail "unexpected --aspect output: $out" ;;
esac

# What a mask guarantees, out of the binary that shipped rather than out of a
# unit test. v0.9.0 made one provider's mask binding and six of the seven
# surfaces describing it went on saying "advisory" — including the probe an agent
# is told to believe. Every one of those surfaces now reads the same enum, so
# checking one through a fresh process checks the mechanism.
out=$(env -u GEMINI_API_KEY "$BIN" generate "x" --mask m.png 2>&1 || true)
case "$out" in
  *comfyui*binding*) pass "a refused mask names the provider whose mask binds" ;;
  *advisory*)        fail "the mask refusal offers only an advisory mask: $out" ;;
  *)                 fail "unexpected --mask output: $out" ;;
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

# --- the render ledger ------------------------------------------------------
# A render that has been paid for must be findable afterwards, and the ledger is
# the only thing that makes that true across sessions. Checked out of the shipped
# binary because the file's location is resolved at runtime from the config
# search path, which no unit test exercises.
ledger_home=$(mktemp -d)

out=$(env -i HOME="$ledger_home" PATH=/usr/bin:/bin "$BIN" ops 2>&1 || true)
case "$out" in
  *"No video renders are waiting"*) pass "ops reports an empty ledger" ;;
  *)                                fail "unexpected empty-ledger output: $out" ;;
esac

out=$(env -i HOME="$ledger_home" PATH=/usr/bin:/bin "$BIN" config 2>&1 || true)
case "$out" in
  # Named out loud because this file records prompts, and someone who does not
  # want them on disk should not have to find it first to learn it exists.
  *"Render ledger:"*) pass "config names the ledger" ;;
  *)                  fail "config does not mention the ledger: $out" ;;
esac

out=$(env -i HOME="$ledger_home" PATH=/usr/bin:/bin LUCIDA_NO_LEDGER=1 "$BIN" ops 2>&1 || true)
case "$out" in
  *"LUCIDA_NO_LEDGER"*) pass "the ledger can be switched off" ;;
  *)                    fail "LUCIDA_NO_LEDGER was ignored: $out" ;;
esac
rm -rf "$ledger_home"

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
for provider in google comfyui bfl stability openai; do
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
