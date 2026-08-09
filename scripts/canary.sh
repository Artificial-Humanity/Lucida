#!/usr/bin/env bash
#
# Live drift detection: does each provider still speak the protocol Lucida
# expects, today?
#
# This closes a limit the recorded-response tests state about themselves. A
# recording proves Lucida still speaks *yesterday's* protocol, not that the
# provider still does — so every wire test in the suite passes on the day an API
# changes, and the first thing that notices is somebody's failed render. This
# script asks the providers instead, on a schedule.
#
# Usage: canary.sh <path-to-binary>
#
# WHERE THIS RUNS: an ai-lab-0 cron, weekly. Owner's call, 2026-08-09, and the
# reason is credential geography rather than convenience — the five provider keys
# already live on that machine, and putting a second copy into GitHub Actions
# secrets would double the number of places they exist for no gain. The workflow
# in .github/workflows/canary.yml is therefore `workflow_dispatch` only: it can
# be run by hand when someone has reason to, and it never runs itself.
#
#   # crontab on ai-lab-0
#   17 6 * * 1 /path/to/lucida-repo/scripts/canary.sh /usr/local/bin/lucida
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
# passed over.
#
# A provider whose key is absent is SKIPPED, not failed. This is meant to be
# runnable on a laptop with two keys as well as on the machine that has five.

BIN=${1:?usage: canary.sh <path-to-binary>}
failures=0
skipped=0

pass() { printf '  ok    %s\n' "$1"; }
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
settings=$("$BIN" config 2>/dev/null)

have_key() {
  case "$1" in
    comfyui)   return 0 ;;
    google)    name=GEMINI_API_KEY ;;
    bfl)       name=BFL_API_KEY ;;
    stability) name=STABILITY_API_KEY ;;
    openai)    name=OPENAI_API_KEY ;;
    runway)    name=RUNWAYML_API_SECRET ;;
    *)         return 1 ;;
  esac
  printf '%s' "$settings" | grep -qE "^ +$name +set"
}

# --- 1. the free endpoints --------------------------------------------------
# `lucida models` reaches each provider's list-or-balance endpoint, which costs
# nothing and is the fastest way to learn that a key has been revoked, a base URL
# has moved, or a response shape has changed.

printf 'Free endpoints (model lists and balances):\n'
for provider in google comfyui bfl stability openai runway; do
  if ! have_key "$provider"; then
    skip "$provider — no credential in this environment"
    continue
  fi

  out=$("$BIN" models --provider "$provider" 2>&1)
  case "$out" in
    *"NOT reachable"*|*"cannot be used right now"*)
      # ComfyUI being off is an ordinary state of the world, not drift.
      if [ "$provider" = comfyui ]; then
        skip "comfyui — not listening"
      else
        fail "$provider — $(printf '%s' "$out" | head -2 | tr '\n' ' ')"
      fi
      ;;
    *"This provider supports:"*)
      pass "$provider — reachable, capability table intact"
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
probe() {
  provider=$1
  model=$2

  if ! have_key "$provider"; then
    skip "$provider — no credential in this environment"
    return
  fi

  out=$("$BIN" generate "canary probe, never rendered" \
          --provider "$provider" --model "$model" \
          --out /dev/null 2>&1)
  code=$?

  case "$code" in
    0)
      # The one outcome that must never happen: it means something rendered.
      fail "$provider — a NONEXISTENT MODEL RENDERED. This spent money. Investigate before running again."
      ;;
    2)
      # A capability refusal, which means Lucida declined before reaching the
      # provider — so this probe learned nothing about the provider at all.
      skip "$provider — refused locally, probe never left the machine"
      ;;
    *)
      case "$out" in
        *"key was rejected"*|*"401"*|*"403"*)
          fail "$provider — the credential is no longer accepted"
          ;;
        *)
          pass "$provider — rejected the unknown model, as expected"
          ;;
      esac
      ;;
  esac
}

probe google    "gemini-3.1-flash-image-canary-does-not-exist"
probe bfl       "flux-2-pro-canary-does-not-exist"
probe stability "core-canary-does-not-exist"
probe openai    "gpt-image-canary-does-not-exist"

# Runway is a *video* provider, so the image probe above does not reach it. Its
# free endpoint is the balance, which `lucida models` already calls — and that
# alone exercises the base URL, the Bearer header and the mandatory
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
if [ "$failures" -eq 0 ]; then
  printf 'no drift detected (%s probe(s) skipped)\n' "$skipped"
  exit 0
fi

printf '%s drift finding(s). A provider changed under us — read the lines marked\n' "$failures"
printf 'DRIFT above, then check the recorded-response tests that cover that lane:\n'
printf 'they will still be passing, which is exactly the gap this script exists for.\n'
exit 1
