#!/usr/bin/env bash
#
# Live drift detection: does each provider still speak the protocol Lucida
# expects, today?
#
# This closes a limit the recorded-response tests state about themselves. A
# recording proves Lucida still speaks *yesterday's* protocol, not that the
# provider still does — so every wire test in the suite passes on the day an API
# changes, and the first thing that notices is somebody's failed render. This
# script asks the providers instead, whenever it is run; see WHERE THIS RUNS.
#
# Usage: canary.sh <path-to-binary>
#
# WHERE THIS RUNS: by hand, with no scheduled job. Run it before every release
# and after changing a provider lane.
#
# ai-lab-0 is the natural place to run it, because of credential geography
# rather than convenience — the provider keys
# already live on that machine, and putting a second copy into GitHub Actions
# secrets would double the number of places they exist for no gain. The workflow
# in .github/workflows/canary.yml is therefore `workflow_dispatch` only: it can
# be run by hand when someone has reason to, and it never runs itself.
#
# WHAT IT COSTS: nothing, and that is a property rather than an aspiration.
# Every probe is one of two kinds:
#
#   1. A free endpoint — the model list or credit balance behind `lucida models`.
#      Exercises the base URL, the credential header and the response parsing.
#   2. A render request naming a model that does not exist. The provider must
#      reject it, which proves the render endpoint is still where we think it is
#      and still fails in the shape `explain_error` reads. A model that does not
#      exist cannot be rendered, so this cannot bill.
#
# A SUCCESSFUL RENDER HERE IS A FAILURE. If a nonsense model id ever comes back
# 200, either the provider stopped validating or Lucida sent something other than
# what was asked for, and both mean money was spent by a script whose whole
# contract is that it spends none. That case is reported loudly rather than
# passed over — and it is detected by a file appearing, not by an exit code, so
# it cannot be missed by a probe that happens to exit non-zero afterwards.
#
# A PASS NEEDS POSITIVE EVIDENCE. A probe passes only when the output carries the
# thing it was looking for: the model list for a free endpoint, the provider's
# own unknown-model rejection for a render probe. Anything else is a failure
# that shows what came back. The first version of this script passed on "did not
# match any failure I thought of", which made DNS failure, connection refused, a
# timeout, a moved endpoint (404) and a 5xx all read as "rejected the unknown
# model, as expected" — a canary that is green because nothing was looked at.
#
# A provider whose key is absent is SKIPPED, not failed. This is meant to be
# runnable on a laptop with two keys as well as on the machine that has them all.
# But a run in which NOTHING was probed is not a clean run, and exits non-zero.

BIN=${1:?usage: canary.sh <path-to-binary>}
failures=0
skipped=0
passed=0
# Passes that needed no credential (ComfyUI). They are real, but they do not show
# that any keyed provider was looked at, which is what the floor below asks.
unkeyed_passed=0

pass() { printf '  ok    %s\n' "$1"; passed=$((passed + 1)); }
fail() { printf '  DRIFT %s\n' "$1"; failures=$((failures + 1)); }
skip() { printf '  --    %s\n' "$1"; skipped=$((skipped + 1)); }

printf 'Lucida canary — %s\n' "$(date -u '+%Y-%m-%d %H:%M UTC')"
printf 'binary: %s (%s)\n\n' "$BIN" "$("$BIN" --version 2>&1)"

# Which providers have a credential to probe with.
#
# Asked of the binary rather than read from the environment, and that is the
# whole point of `lucida config`: a key may live in the config file instead, and
# on the machine this is meant to run on it usually does. Checking `$GEMINI_API_KEY`
# here would skip a provider that is perfectly reachable and report "no drift"
# for a lane nothing looked at — a canary that quietly stops watching is worse
# than no canary. `config` prints presence and source, never a value.
#
# comfyui needs no credential: it is either listening or it is not, and "not" is
# a state of the world rather than drift.
#
# A failing `config` is a finding, not silence: with the output discarded, every
# keyed provider read as keyless, was skipped, and the run reported "no drift"
# having looked at nothing.
settings=$("$BIN" config 2>/dev/null)
config_status=$?
if [ "$config_status" -ne 0 ]; then
  fail "config — \`lucida config\` exited $config_status, so no credential can be found and nothing can be probed"
  settings=""
elif ! printf '%s' "$settings" | grep -qE '^ +[A-Z_]+ +(set|not set)'; then
  # The column layout is what `have_key` greps. If it changed, every provider
  # would read as keyless; tests/cli.rs pins the layout from the other side.
  fail "config — its output no longer has the \`NAME  set|not set\` rows this script reads"
fi

have_key() {
  case "$1" in
    comfyui)   return 0 ;;
    google)    name=GEMINI_API_KEY ;;
    bfl)       name=BFL_API_KEY ;;
    stability) name=STABILITY_API_KEY ;;
    openai)    name=OPENAI_API_KEY ;;
    runway)    name=RUNWAY_API_KEY ;;
    kling)     name=KLINGAI_API_KEY ;;
    *)         return 1 ;;
  esac
  printf '%s' "$settings" | grep -qE "^ +$name +set"
}

# --- 1. the free endpoints --------------------------------------------------
# `lucida models` reaches each provider's list-or-balance endpoint, which costs
# nothing and is the fastest way to learn that a key has been revoked, a base URL
# has moved, or a response shape has changed.

printf 'Free endpoints (model lists and balances):\n'
for provider in google comfyui bfl stability openai runway kling; do
  if ! have_key "$provider"; then
    skip "$provider — no credential in this environment"
    continue
  fi

  out=$("$BIN" models --provider "$provider" 2>&1)
  case "$out" in
    *"no resource pack on this account"*)
      # See the kling balance check below.
      skip "$provider — key accepted, but the account has no resource pack to read"
      ;;
    # `did not answer` is what `lucida models` prints when a client exists and
    # the provider failed to reply (DNS, refused, timeout, a bad status).
    # `cannot be used right now` is the no-client case. The capability table is
    # printed after either, so it proves nothing about reachability and is not
    # looked for here.
    *"did not answer"*|*"NOT reachable"*|*"cannot be used right now"*)
      # ComfyUI being off is an ordinary state of the world, not drift.
      if [ "$provider" = comfyui ]; then
        skip "comfyui — not listening"
      else
        # From the line that names the failure on: the retry chatter and the
        # `== Images ==` banner above it say nothing about why.
        fail "$provider — $(printf '%s' "$out" | grep -A2 -E 'did not answer|NOT reachable|cannot be used right now' | head -c 200 | tr '\n' ' ')"
      fi
      ;;
    # The positive lines: printed only when the listing (or, for Kling, the
    # balance read) succeeded.
    *"Image models available to"*|*"Remaining units"*)
      pass "$provider — reachable, answered with its models"
      [ "$provider" = comfyui ] && unkeyed_passed=$((unkeyed_passed + 1))
      ;;
    *)
      fail "$provider — unrecognised output: $(printf '%s' "$out" | head -c 200)"
      ;;
  esac
done

# --- 2. the render endpoints, without rendering -----------------------------
# A model id that cannot exist. The provider must refuse it; a refusal proves the
# endpoint is still there and still speaks the error shape Lucida parses.

printf '\nRender endpoints (rejected by a model id that cannot exist):\n'

# Where the probes are told to write. A probe that works writes a file here, and
# a file here is the one unambiguous sign that something rendered.
#
# This was `--out /dev/null`, which cannot work: Lucida corrects the extension
# (`/dev/null.png`) and stages an atomic write beside the target, which a
# non-root user cannot do in /dev. So a render that SUCCEEDED failed afterwards
# with exit 1 and was reported as "rejected the unknown model, as expected" — a
# billed render, passed.
scratch=$(mktemp -d) || { echo "canary: cannot create a scratch directory" >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT

# True when the directory holds anything at all.
produced_output() { [ -n "$(ls -A "$1" 2>/dev/null)" ]; }

# probe <provider> <model> <rejection text>
#
# The rejection text is what that provider's unknown-model failure says in
# Lucida's wording (each `explain_error` in src/<provider>.rs), so a pass means
# the provider answered *and said no for the reason we expect*. A 404 on a moved
# endpoint says the same thing for the providers that put the model in the URL,
# which is why those texts name the model id as well.
probe() {
  provider=$1
  model=$2
  expected=$3

  if ! have_key "$provider"; then
    skip "$provider — no credential in this environment"
    return
  fi

  dir="$scratch/$provider"
  mkdir -p "$dir"
  out=$("$BIN" generate "canary probe, never rendered" \
          --provider "$provider" --model "$model" \
          --out "$dir/probe.png" 2>&1)
  code=$?

  # Checked first and whatever the exit code was.
  if produced_output "$dir"; then
    fail "$provider — a NONEXISTENT MODEL RENDERED. This spent money. Investigate before running again."
    return
  fi

  case "$code" in
    0)
      # Exit 0 with no file is no better: Lucida said it rendered.
      fail "$provider — a NONEXISTENT MODEL RENDERED (exit 0, no file). This may have spent money. Investigate before running again."
      ;;
    2)
      # A capability refusal, which means Lucida declined before reaching the
      # provider — so this probe learned nothing about the provider at all.
      skip "$provider — refused locally, probe never left the machine"
      ;;
    *)
      case "$out" in
        *"key was rejected"*|*"key was not accepted"*|*"HTTP 401"*|*"HTTP 403"*)
          fail "$provider — the credential is no longer accepted"
          ;;
        *"$expected"*)
          pass "$provider — rejected the unknown model, as expected"
          ;;
        *)
          fail "$provider — not the rejection expected (\"$expected\"): $(printf '%s' "$out" | head -c 200 | tr '\n' ' ')"
          ;;
      esac
      ;;
  esac
}

# Expected texts, from the explain_error of each provider:
#   google    404  "no such model"                       (src/genai.rs)
#   bfl       404  "no such endpoint as `<model>`"       (src/bfl.rs)
#   stability 404  "no such endpoint as `<model>`"       (src/stability.rs)
#   openai    400  OpenAI's own "does not exist"         (src/openai.rs, measured:
#                  an id that is not in the catalogue says so, where an id that
#                  exists but is not enabled says "does not have access" as a 403)
probe google    "gemini-3.1-flash-image-canary-does-not-exist" "no such model"
probe bfl       "flux-2-pro-canary-does-not-exist"             "no such endpoint as \`flux-2-pro-canary-does-not-exist\`"
probe stability "core-canary-does-not-exist"                   "no such endpoint as \`core-canary-does-not-exist\`"
probe openai    "gpt-image-canary-does-not-exist"              "does not exist"

# Runway renders images too, but `probe` cannot reach it: Lucida refuses a model
# that is not one of Runway's own before anything is sent, because the endpoint
# fronts other companies' models. So this probe names a real model and carries
# two violations Lucida passes on and the endpoint rejects for free: a seed one
# past its measured ceiling (4294967295) and a prompt one past its 1000-character
# limit. Either alone blocks a render, so both limits would have to move at once
# for it to spend — unlike the probes above, it is free by measurement rather
# than by construction. If it ever renders, that is reported as the failure it is.
if have_key runway; then
  long_prompt=$(printf 'canary probe, never rendered %.0s' $(seq 1 40))
  mkdir -p "$scratch/runway"
  out=$("$BIN" generate "$long_prompt" \
          --provider runway --model gen4_image --seed 4294967296 \
          --out "$scratch/runway/probe.png" 2>&1)
  code=$?
  if produced_output "$scratch/runway"; then
    fail "runway — A PROBE RENDERED. This spent money. Investigate before running again."
  else
    case "$code:$out" in
      0:*) fail "runway — A PROBE RENDERED (exit 0, no file). This may have spent money. Investigate before running again." ;;
      2:*) skip "runway — refused locally, probe never left the machine" ;;
      *"4294967295"*) pass "runway — rejected the out-of-range seed, as expected" ;;
      *"key was rejected"*|*"rejected the key"*|*"HTTP 401"*|*"HTTP 403"*) fail "runway — the credential is no longer accepted" ;;
      *) fail "runway — unrecognised rejection: $(printf '%s' "$out" | head -c 200 | tr '\n' ' ')" ;;
    esac
  fi
else
  skip "runway — no credential in this environment"
fi

# Runway's and Kling's balances are free, and `lucida models` reads them. For
# Runway that exercises the base URL, the Bearer header and the mandatory
# X-Runway-Version header, which is the one most likely to be retired under us.
if have_key runway; then
  out=$("$BIN" models --provider runway 2>&1)
  case "$out" in
    *"Remaining credits"*) pass "runway — reachable, version header still accepted" ;;
    *) fail "runway — $(printf '%s' "$out" | head -2 | tr '\n' ' ')" ;;
  esac
else
  skip "runway — no credential in this environment"
fi

if have_key kling; then
  out=$("$BIN" models --provider kling 2>&1)
  case "$out" in
    *"Remaining units"*) pass "kling — reachable, balance readable" ;;
    # Lucida says this only after Kling accepted the signed key and answered
    # with a balance document that holds no resource pack. An empty account is
    # a state of the account, not drift, so it is skipped rather than failed.
    # It is not a pass either: a renamed balance field reads the same way, and
    # with no pack there is nothing to tell the two apart.
    *"no resource pack on this account"*)
      skip "kling — key accepted, but the account has no resource pack to read" ;;
    *) fail "kling — $(printf '%s' "$out" | head -2 | tr '\n' ' ')" ;;
  esac
else
  skip "kling — no credential in this environment"
fi

# --- 3. the models we default to are still offered --------------------------
# A default that has been retired is the failure mode with the longest fuse: it
# works until it does not, and `RETIREMENTS` only knows about the dates somebody
# wrote down.

printf '\nDefaults still listed by the provider:\n'
for provider in google openai; do
  if ! have_key "$provider"; then
    skip "$provider — no credential in this environment"
    continue
  fi

  listed=$("$BIN" models --provider "$provider" 2>&1)
  # Only the image half: a provider of both media lists its video models after,
  # and google's static Veo aliases carry `(default)` of their own — which would
  # pass this check whether or not the image default is still offered.
  listed=${listed%%Video models available*}
  case "$listed" in
    *"(default"*|*"default)"*)
      pass "$provider — its default is present in the live list"
      ;;
    *)
      fail "$provider — the default model is not in the live model list"
      ;;
  esac
done

printf '\n'
if [ "$failures" -eq 0 ] && [ "$((passed - unkeyed_passed))" -eq 0 ]; then
  # Every keyed provider was skipped, so nothing was looked at. "No drift
  # detected" would be true of a run that asked no question. ComfyUI answering
  # does not count: it needs no key, so it is probed on every machine.
  printf 'NOTHING WAS PROBED (%s probe(s) skipped, no keyed provider passed or failed).\n' "$skipped"
  printf 'No provider had a credential this script could find; run `lucida config` and\n'
  printf 'see which keys are set, then run it where at least one is.\n'
  exit 1
fi

if [ "$failures" -eq 0 ]; then
  printf 'no drift detected (%s probe(s) passed, %s skipped)\n' "$passed" "$skipped"
  exit 0
fi

printf '%s drift finding(s). A provider changed under us — read the lines marked\n' "$failures"
printf 'DRIFT above, then check the recorded-response tests that cover that lane:\n'
printf 'they will still be passing, which is exactly the gap this script exists for.\n'
exit 1
