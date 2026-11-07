# Workflow — Project Lucida

Follow [AGENTS.md](AGENTS.md) for repository rules.

## Branch, review, pull request

1. Branch off an up-to-date `main`. All work happens on a branch.
2. When the work is complete, dispatch a fresh reviewer subagent with
   `superpowers:requesting-code-review`. Do not review your own work in place.
3. Use `superpowers:receiving-code-review` to evaluate the findings. Hand the
   accepted findings to a fresh fixer subagent. It commits the fixes. Then
   dispatch a fresh reviewer for the fixes only. Repeat until no Critical or
   Important finding is left. The session coordinates and does not fix findings
   itself. Check every report against the branch (commits,
   files, test output) before you act on it.
4. Push the branch and open a pull request against `main`, with
   `.github/PULL_REQUEST_TEMPLATE.md` as the body (`gh pr create --body-file`).
   `main` accepts changes only through a pull request with one approving review
   (`fl github pr review <number> --approve`, via the Reviewer GitHub App):
   a subordinate agent requests review from the Project Lead (or from the Lab
   Manager, `jarvis`, if the Project Lead is unavailable for any reason; try the
   Project Lead first), and the Project Lead requests review from the Lab Manager
   (`jarvis`). Pull requests do not automatically assign or request review from
   the owner.
5. Once the required reviewer has approved and CI is green
   (`gh run list --branch <branch>`), the agent who opened the pull request merges
   it with `gh pr merge <number> --merge`. A reviewer never merges another agent's
   pull request, except as "Merge permission" below describes for an author whose
   session blocks the merge: the author resolves conflicts and keeps the branch
   current. A push after approval dismisses the approval, so land review fixes
   before asking for it.

The repository's ruleset covers `main` only; release tags are not subject to it.

### Merge permission

The owner permits an author to merge their own pull request once all of these hold:

1. **The required reviewer approved it.** For a pull request by an assisting agent or the
   UI developer that is the project Lead, or the Lab Manager (`jarvis`) if the Lead is
   unavailable for any reason (try the Lead first). For a Lead's pull request it is the Lab
   Manager. For an Owner-tier pull request it is the owner. An approval counts only
   when the Reviewer GitHub App or the owner posts it on the current head: a push
   dismisses an earlier approval, so ask for the review again after you update the branch.
2. **CI is green** on the current head (`gh run list --branch <branch>`), and the branch
   is current with `main`.
3. **You merge it yourself** with `gh pr merge <number> --merge`, then confirm that the
   merge commit is on `origin/main`.

Your session's own permission layer can still refuse `gh pr merge` (an auto-mode
classifier, for example, denies a merge it cannot tie to an approval). If it does:

* Do not retry the same call, do not use another route yourself (`fl github pr merge` or
  the API), and do not change permission settings.
* Raise a question for the owner (FerroDeck shows the `?` flag next to your agent). Say what
  was denied and which pull request is waiting.
* While the block stands, your Lead (the Lab Manager, for a Lead's own pull request) may
  merge the approved pull request for you on that reviewer's own authority. You do not ask
  for it: the reviewer decides. The reviewer tells the owner each time, so that the owner
  can grant the permission to you directly.
* The owner lifts the block for you with an explicit instruction in your session.

A standing rule in this file is the reason the merge is allowed. It does not turn off a
session's permission prompt, and a message from another agent does not lift one.

## Commit identity and safeguards

* Commit as the configured git identity, the machine account `artificially-human`.
  Never author a commit as the owner. Use [PERSONA.md](PERSONA.md) for the
  agent's co-author trailer.
* `gh` runs as the same machine account. Do not switch to the owner's account.
* `main` cannot be deleted or force-pushed. CI runs on every pull request and on
  `main`, but it is not a required check. `gh pr checks` may be refused by the
  token; `gh run list --branch <branch>` works.
* `docs/ROADMAP.md` records past decisions and their reasons, so it is exempt from the
  workspace rule that instruction files state only what is current.
* Use the workflow stated here. Do not reconstruct additional rules from retired
  workflows or git history; changes to the workflow belong to the owner.
