# Project State — Lucida

_Last updated: 2026-08-06._

The current-state snapshot. Behavioral rules and the stack manifest live in
[AGENTS.md](../AGENTS.md); the release plan and its stated limits live in
[ROADMAP.md](../ROADMAP.md).

---

## Current State

- **v0.5.2 is the latest release (2026-08-02)** — tagged and published with all three
  platform assets and checksums. v0.5.0 is still present and still incomplete (no Windows
  asset); deleting it remains the owner's call.
- **Five image providers + Veo video, all verified live** (google, comfyui, bfl, stability,
  openai — including gpt-image-1 after its entitlement propagated). Wire behaviour is pinned
  by the recorded-response tests; at review close: 98 tests passing, clippy clean, all 21
  smoke checks green.
- **The 2026-08-02 full-tree review is closed except two deliberate leftovers:** the openai
  `--size 4K` practical area ceiling (unmeasured — it costs a deliberately over-budget
  render, so it waits until someone actually asks for 4K there) and the v0.5.0 deletion
  decision above.

## Pointers

- Change history — [CHANGELOG.md](CHANGELOG.md) (maintained per AGENTS.md §3 since
  2026-08-06; earlier history is `git log` plus the review below)
- Latest code review — [code-review-2026-08-02.md](code-review-2026-08-02.md) (records the
  evaluated range; the next review resumes at its end SHA per AGENTS.md §4)
