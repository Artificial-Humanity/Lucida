# Project State — Lucida

_Last updated: 2026-08-09._

The current-state snapshot. Behavioral rules and the stack manifest live in
[AGENTS.md](../AGENTS.md); the release plan and its stated limits live in
[ROADMAP.md](../ROADMAP.md).

---

## Current State

- **v0.10.0 is the latest release (2026-08-09)** — *dependable unattended use*, the second
  tranche of the [2026-08-09 product review](product-review-20260809.md). **All three of the
  review's structural findings are now closed.** The MCP server dispatches tool calls on a
  pool of four and answers `ping`/`tools/list` from the reading thread, so a long render no
  longer makes it deaf; `notifications/cancelled` is honoured, having been *unreachable*
  because a cancellation carries no id of its own. Renders are recorded in an append-only
  ledger, read by `lucida ops`, `lucida history` and the new `list_operations` MCP tool — so
  a Veo render whose session ended can still be collected. Cost is stated before it is spent
  and `LUCIDA_BUDGET` refuses over it. Four outcomes now have four exit codes (**2 = refused,
  do not retry**), with `--json` on every subcommand. `scripts/canary.sh` probes all five
  providers live, free by construction, from a weekly ai-lab-0 cron. 201 tests, up from 163.
- **v0.9.2 (2026-08-09)** was the review's first tranche — *keep the existing promises*. Two
  structural findings: **a wrongly-typed MCP argument is refused rather than silently
  dropped** (the worst case turned an *edit* into a fresh generation and reported success),
  and **a paid Veo render can no longer be lost from the CLI**. Plus connect timeouts, atomic
  image writes, self-dating shutdown notices, and a description that matches the tool.
- **v0.9.1 (2026-08-07)** was the 2026-08-06 code review, closed, and nothing else: no new
  capability, one structural change (mask semantics became a value) and a set of fixes. It is
  the first release the changelog covers.
- **v0.9.0 (2026-08-03) closed the v0.6.0 → v0.9.0 run** (17 commits, through `f5ecf17`):
  self-update, one-line install on all three platforms, `lucida setup` wiring itself into
  Claude, the binary carrying its own skill, a **binding** mask on the local lane,
  Lucida-scoped keys beating shell exports, and CI on all three platforms.
- **v0.9.0 through v0.10.0 are the published releases** (pruned 2026-08-07) — the eleven releases
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
  openai). Wire behaviour is pinned by the recorded-response tests; as of 2026-08-09:
  **201 tests** passing (163 at v0.9.2), clippy clean at `-D warnings`, all smoke checks green
  — and **CI green on all three platforms**. Windows keeps earning that lane: the atomic-write
  refactor left `config.rs` with an unused import that only exists off Unix, and a ledger test
  used a path that is unwritable on Linux and an ordinary directory on Windows. Local clippy
  could see neither.
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
- **Decided 2026-08-09, and it shapes the roadmap:** public adoption *is* a goal, and the
  review is being addressed in full — all three tranches, expansion axes included. So the
  positioning work is live (description and topics done; crates.io once the ledger and video
  APIs settle).
- **Phase 3's video work is DONE, on `main`, unreleased.** The `VideoProvider` trait landed
  and video now has **three providers**: Veo, Runway (its own gen4 models only) and Kling
  (direct — all eight versions, three quality tiers). `--duration` answers the ROADMAP's open
  question: 8 s is **not** a hard limit, Veo does 4, 6 and 8. 220 tests.
- **Aggregators remain declined, twice reaffirmed.** Runway's fronted catalogue (Kling, Veo,
  Seedance, Hailuo, Grok, Gemini) was excluded by the owner on 2026-08-09; fal.ai was probed
  and declined the same day, because the models it fronts that we want — Kling, Veo, FLUX —
  are all now reached *directly*, so it would only add a worse second path to lanes we own.
  Two facts about fal are in the changelog for whoever revisits it: it authenticates with
  `Authorization: Key` rather than `Bearer`, and it queues a request **without validating**,
  so AGENTS.md §2's free-validation-error probe — how every provider here was characterised
  safely — does not work against it.
- **Left in Phase 3:** Lyria audio (same Gemini key, no new account) and Stability's edit
  endpoints. **Owner 2026-08-09: no further providers for now.**

## Pointers

- Change history — [CHANGELOG.md](CHANGELOG.md) (maintained per AGENTS.md §3 since
  2026-08-06; v0.9.1 is the first version it covers). History before that is **`git log` and
  the thirteen tags** — the release notes below v0.9.0 were destroyed by the 2026-08-07 prune,
  so the tags are now the only pointer into that range.
- Download counts destroyed by the prune —
  [release-downloads-at-prune-20260807.md](release-downloads-at-prune-20260807.md)
- Latest code review — [code-review-20260806-151318.md](code-review-20260806-151318.md)
  (records the evaluated range; the next review resumes at its end SHA per AGENTS.md §4)
- Latest product review — [product-review-20260809.md](product-review-20260809.md). Not part
  of the §4 cycle and it moves no pointer; it is the plan of record for v0.9.2 onward, and
  each finding it lists is closed by its own commit.
