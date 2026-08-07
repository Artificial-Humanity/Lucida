# Project State — Lucida

_Last updated: 2026-08-07._

The current-state snapshot. Behavioral rules and the stack manifest live in
[AGENTS.md](../AGENTS.md); the release plan and its stated limits live in
[ROADMAP.md](../ROADMAP.md).

---

## Current State

- **v0.9.1 is the latest release (2026-08-07)** — the 2026-08-06 code review, closed, and
  nothing else: no new capability, one structural change (mask semantics became a value) and
  a set of fixes. It is the first release the changelog covers.
- **v0.9.0 (2026-08-03) closed the v0.6.0 → v0.9.0 run** (17 commits, through `f5ecf17`):
  self-update, one-line install on all three platforms, `lucida setup` wiring itself into
  Claude, the binary carrying its own skill, a **binding** mask on the local lane,
  Lucida-scoped keys beating shell exports, and CI on all three platforms.
- **Only v0.9.0 and v0.9.1 are published releases (pruned 2026-08-07)** — the eleven releases
  from v0.1.0 through v0.8.0 were deleted, taking the incomplete v0.5.0 (no Windows asset)
  with them. The baseline is deliberate: `lucida update` arrived in v0.7.0, so nothing below
  it could update itself at all, and every surviving release self-updates by both paths. **The
  thirteen git tags were kept** — `cargo install --tag` resolves against tags, not releases,
  and they are the history the deleted release notes used to carry. CI is unaffected: its
  pinned-version check derives the second-newest non-draft release (`05e9aac`) rather than
  hardcoding one, and now resolves v0.9.0. GitHub destroys a release's download counters with
  the release, so they were captured first:
  [release-downloads-at-prune-20260807.md](release-downloads-at-prune-20260807.md).
- **Five image providers + Veo video, all verified live** (google, comfyui, bfl, stability,
  openai). Wire behaviour is pinned by the recorded-response tests; as of 2026-08-06:
  **138 tests** passing (125 at the review close), clippy clean at `-D warnings`, all smoke
  checks green — and **CI green on all three platforms at `aa55e95`**, which is what puts the
  Windows fixes below on a real Windows runner rather than on a pure-function test alone.
- **The v0.6→v0.9 range was reviewed 2026-08-06 and every finding is now closed**
  (`83680b5`, `1c49c47`, `05e9aac` — see the pointer below for what each one was).
  The headline fix is structural: **whether a mask binds is a value, not prose.**
  `Capabilities.mask` is a `MaskSupport { No, Advisory, Binding }`, so `lucida models`, both
  MCP prose sites, the `image_providers` probe and the capability refusal are generated from
  it; the two surfaces that cannot be generated — the `--mask` clap literal and the shipped
  skill — name no provider and claim no semantics, and tests enforce both. Also closed: the
  two Windows path gaps, atomic config writes, the `mcpServers` panic, and the three
  in-passing items.
- **Carried forward, deliberately:** the openai `--size 4K` area ceiling (unmeasured — it
  costs an over-budget render). The v0.5.0 deletion decision is closed — see the prune above.

## Pointers

- Change history — [CHANGELOG.md](CHANGELOG.md) (maintained per AGENTS.md §3 since
  2026-08-06; v0.9.1 is the first version it covers). History before that is **`git log` and
  the thirteen tags** — the release notes below v0.9.0 were destroyed by the 2026-08-07 prune,
  so the tags are now the only pointer into that range.
- Download counts destroyed by the prune —
  [release-downloads-at-prune-20260807.md](release-downloads-at-prune-20260807.md)
- Latest code review — [code-review-20260806-151318.md](code-review-20260806-151318.md)
  (records the evaluated range; the next review resumes at its end SHA per AGENTS.md §4)
