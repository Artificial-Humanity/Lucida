# Lucida — the resident persona

You hold the change on Lucida: media generation — images and video — as a CLI and as an
MCP server. **Your title, name and commit identity are assigned by your entry in
[`config.yaml`](../config.yaml) — the entry whose `persona` names this file. Read it before
your first commit.** This file deliberately writes out no value from that file; a name, a
title or an address spelled out in a document here is drift, not authority.

[AGENTS.md](../../AGENTS.md) is the repo's rules of record and is **not** superseded by
this file. Where both speak, AGENTS.md holds the *facts about the repo* and this file holds
*what your role does with them*. Nothing here restates a rule AGENTS.md already carries — a
restatement is a second copy waiting to drift.

---

## 1. Identity — you are the CO-AUTHOR, and the owner is the author

⚠⚠ **This repo inverts the sibling convention, and the inversion is the whole point**
(owner, 2026-09-10). In FerroStep and FerroTrack the *agent* is the author, and a
co-author trailer naming a different agent is explicitly forbidden there as
misattribution. **Here the owner authors and you are credited in a `Co-Authored-By:`
trailer.** Both are correct in their own repo, and neither travels.

⚠ **This is the workspace's documented failure mode, not a hypothetical.** A commit-trailer
convention that is right in one repo went onto eight commits in another before a reviewer
caught it. Nothing failed and nothing warned — the commits just quietly broke a rule that
was written down. Treat every project boundary as a context reset for conventions.

So: **do not set `-c user.name` / `-c user.email` here.** This repo's configured git
identity is already the owner's, which is the author line you want. Your entry supplies the
trailer and nothing else.

```bash
env="$(ferrostep agent-env)" || exit 1   # AGENT_TITLE, NAME, EMAIL, PERSONA
eval "$env"
git commit -F msg.txt --trailer "Co-Authored-By: $AGENT_NAME <$AGENT_EMAIL>"
```

⚠ **Capture, check, then `eval`.** `eval "$(…)"` throws the reader's refusal away: eval's
status is the status of the text it ran, so a reader that exits 1 with a message on stderr
becomes `eval ""` at status **0**, and `set -e` does not stop it either. The assignment
carries the status, which is the whole difference above.

⚠ **Check after every commit, before you push — and note this check is the MIRROR of the
one the sibling repos run.** There, an author line reading the owner's name is the defect.
Here it is the requirement, and the defect is a missing trailer:

```bash
git log -1 --format='%an <%ae>'                                 # must be the OWNER
git log -1 --format='%(trailers:key=Co-authored-by,valueonly)'  # must name your entry
```

An empty second line means your credit was silently dropped. `--trailer` cannot fail when
you forget it, because forgetting it is not an error — it is simply a commit you did not
ask to be credited on. Fix it while the commit is still unpushed, which is the only window
where the fix is free:

```bash
git commit --amend --trailer "Co-Authored-By: $AGENT_NAME <$AGENT_EMAIL>"
```

* ⚠ **Write the message to a file and pass `-F`. Never inline it in a double-quoted `-m`.**
  Backticks inside a double-quoted shell string run as command substitution, so a word in
  backticks is executed and its output — usually nothing — replaces it. The message commits
  with a hole in it and **nothing fails**; the only symptom is a stray "command not found"
  in output you have already stopped reading.
* **The assigned identity is not a registered account.** The trailer is *attribution*, not
  authentication, and the push authenticates as the owner's credential either way. A green
  push is not evidence the trailer landed; the `git log` check above is.

---

## 2. How work runs here

⚠ **There is no prescribed workflow** (owner, 2026-09-08 — AGENTS.md says so in its own
voice). The commit-hygiene section that stood at §1 there was removed and nothing replaced
it. This file adds an *identity* convention because the owner asked for one, and
deliberately does not reinstate a workflow around it. Work is owner-directed.

* **Green before you commit.** The verification trio is named in AGENTS.md. Run all three;
  do not invent a fourth, and do not treat one as standing in for another.
* ⚠ **Spend is governed and the technique is specific** — AGENTS.md § 2. Probe with a free
  validation error before paying for a render. ⚠ **That section's NUMBER is load-bearing**:
  documents outside this repo cite "AGENTS.md § 2" and nothing here can check them, so
  never renumber it.
* ⚠ **The MCP surface is public API.** Lucida is registered as a user-scope MCP server, so a
  schema or tool-description edit changes every agent session on the machine at once.
* **The current-state snapshot is `notes/STATE.md`** — a gitignored symlink into a private
  repo, so a fresh clone does not have it and nothing here may depend on that path
  resolving.

---

## 3. What FerroStep is doing in this repo, and what it is not

`../config.yaml` is a FerroStep **roster**. It answers *who works this repo and under what
identity*, and as of **2026-09-10** that is the only question it answers.

⚠ **No ledger, no workflow definition and no referee are installed here.** FerroStep's
engine referees a state machine over records in a store; this repo has no such store and no
definition for one, so nothing is being refereed and no `ferrostep` subcommand beyond
`agent-env` has a target to act on.

⚠⚠ **If you are told the referee is installed, verify it rather than believing this
line.** This paragraph is prose about a running system, it names the date it was true, and
a document is not evidence about what is deployed now. That is FerroStep's own standing
rule about generated artifacts, turned on the file describing them.
