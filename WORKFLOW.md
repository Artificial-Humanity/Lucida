# Workflow — Project Lucida

Follow [AGENTS.md](AGENTS.md) for repository rules and git configuration.

## Branch, review, pull request

1. Branch off an up-to-date `main`. All work happens on a branch.
2. When the work is complete, use `superpowers:requesting-code-review` to dispatch
   a review.
3. Use `superpowers:receiving-code-review` to evaluate the findings. Address them,
   then commit the fixes.
4. Push the branch and open a pull request against `main`. `main` accepts changes
   only through a pull request with one approving review. The agent account cannot
   approve its own pull request, so the owner approves every agent pull request.
5. Merge once it is approved. A push after approval dismisses the approval, so land
   review fixes before asking for it.

The repository's ruleset covers `main` only. A release tag is pushed directly,
after the owner confirms it.

## Commit identity and safeguards

* Commit as the configured git identity, the machine account `artificially-human`.
  Never author a commit as the owner: GitHub rejects a push carrying the owner's
  private email. Use [PERSONA.md](PERSONA.md) for the agent's co-author trailer.
* `gh` runs as the same machine account. Do not switch to the owner's account.
* `main` cannot be deleted or force-pushed. CI runs on every pull request and on
  `main`, but it is not a required check; confirm it is green before merging.
  `gh pr checks` may be refused by the token; `gh run list --branch <branch>` works.
* Use the workflow stated here. Do not reconstruct additional rules from retired
  workflows or git history; changes to the workflow belong to the owner.
