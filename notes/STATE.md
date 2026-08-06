# Project State — Lucida

_Last updated: 2026-08-06._

The current-state snapshot. Behavioral rules and the stack manifest live in
[AGENTS.md](../AGENTS.md); the release plan and its stated limits live in
[ROADMAP.md](../ROADMAP.md).

---

## Current State

- **v0.9.0 is the latest release (2026-08-03)** — the v0.6.0 → v0.9.0 run (17 commits,
  through `f5ecf17`): self-update, one-line install on all three platforms, `lucida setup`
  wiring itself into Claude, the binary carrying its own skill, a **binding** mask on the
  local lane, Lucida-scoped keys beating shell exports, and CI on all three platforms.
  v0.5.0 is still present and still incomplete (no Windows asset); deleting it remains the
  owner's call.
- **Five image providers + Veo video, all verified live** (google, comfyui, bfl, stability,
  openai). Wire behaviour is pinned by the recorded-response tests; at the 2026-08-06
  review close: 125 tests passing, clippy clean at `-D warnings`, all smoke checks green.
- **The v0.6→v0.9 range was reviewed 2026-08-06** (see the pointer below). Headline
  finding, open: v0.9.0's binding ComfyUI mask is still described as "advisory" on every
  agent-facing surface — CLI `--mask` help, `lucida models`, both MCP prose sites, the
  `image_providers` probe, the capability refusal, and the shipped skill; only README.md
  was updated. Two Medium Windows gaps (config-file `home()` reads only `HOME`; `setup`'s
  `which()` misses `.exe`/`.cmd`). Carried forward, deliberately: the openai `--size 4K`
  area ceiling (unmeasured — costs an over-budget render) and the v0.5.0 deletion decision.

## Pointers

- Change history — [CHANGELOG.md](CHANGELOG.md) (maintained per AGENTS.md §3 since
  2026-08-06; earlier history is `git log` and the current review)
- Latest code review — [code-review-20260806-151318.md](code-review-20260806-151318.md)
  (records the evaluated range; the next review resumes at its end SHA per AGENTS.md §4)
