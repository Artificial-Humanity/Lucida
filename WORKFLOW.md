# Workflow — Project Lucida

Follow [AGENTS.md](AGENTS.md) for repository rules.

## Branch, review, pull request

1. Branch off an up-to-date `main`. All work happens on a branch.
2. When the work is complete, use `superpowers:requesting-code-review` to dispatch
   a review.
3. Use `superpowers:receiving-code-review` to evaluate the findings. Address them,
   then commit the fixes.
4. Push the branch and open a pull request against `main`, with
   `.github/PULL_REQUEST_TEMPLATE.md` as the body (`gh pr create --body-file`).
   `main` accepts changes only through a pull request with one approving review.
   The agent account cannot approve its own pull request, so the owner approves
   every agent pull request.
5. Merge once it is approved and CI is green (`gh run list --branch <branch>`).
   A push after approval dismisses the approval, so land review fixes before
   asking for it.

The repository's ruleset covers `main` only; release tags are not subject to it.

## Commit identity and safeguards

* Commit as the configured git identity, the machine account `artificially-human`.
  Never author a commit as the owner. Use [PERSONA.md](PERSONA.md) for the
  agent's co-author trailer.
* `gh` runs as the same machine account. Do not switch to the owner's account.
* `main` cannot be deleted or force-pushed. CI runs on every pull request and on
  `main`, but it is not a required check. `gh pr checks` may be refused by the
  token; `gh run list --branch <branch>` works.
* Use the workflow stated here. Do not reconstruct additional rules from retired
  workflows or git history; changes to the workflow belong to the owner.
