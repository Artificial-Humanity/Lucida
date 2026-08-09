# Project State — Lucida

_Last updated: 2026-08-09._

The current-state snapshot. Behavioral rules and the stack manifest live in
[AGENTS.md](../AGENTS.md); the release plan and its stated limits live in
[ROADMAP.md](../ROADMAP.md).

---

## Current State

- **v1.0.0 is the latest release (2026-08-09)** — *video became a substitution, and the tests
  grew a second layer*. The version number is a claim about **surfaces**, not about being
  finished: the `ImageProvider` and `VideoProvider` traits, the six MCP tools, the four exit
  codes, the `--json` documents, the ledger format and the config names are what callers may
  now build against. Three things earned it — video behind a trait (Veo, Runway, Kling) so it
  substitutes the way images have since v0.4; a **scope boundary** rather than a backlog, with
  audio declined on the day it was cheapest to add; and the layer a user touches moved into
  `cargo test`, which immediately found an unreachable bash assertion and three hand-written
  MCP `provider` enums. 255 tests, CI green on all three platforms at `11409e1`.
  **crates.io waits on the Apple signing certificate** (owner, 2026-08-09) — a positioning
  call, not a technical dependency: `cargo install` compiles from source, so a published crate
  ships no binary for Gatekeeper to evaluate. Signing affects only a browser download from the
  releases page, plus SmartScreen. Noted in [ROADMAP.md](../ROADMAP.md) § 4 so that if org
  enrollment stalls, the coupling is re-taken on purpose rather than by default.
- **v0.10.0 (2026-08-09)** — *dependable unattended use*, the second
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
- **v0.9.0 through v1.0.0 are the published releases** (pruned 2026-08-07) — the eleven releases
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
  **255 tests** passing (163 at v0.9.2), clippy clean at `-D warnings`, all smoke checks green
  — and **CI green on all three platforms**. Windows keeps earning that lane: the atomic-write
  refactor left `config.rs` with an unused import that only exists off Unix, and a ledger test
  used a path that is unwritable on Linux and an ordinary directory on Windows. Local clippy
  could see neither.
- **Tests come in two layers now** (`3558134`), because `cargo test` had covered everything
  except the layer a user touches. Unit tests stay inside the file they test; anything that
  only exists once there is a *process* lives in `tests/cli.rs`, which drives the binary as a
  black box. `scripts/smoke.sh` is no longer a weaker second suite — it checks what packaging
  can break and then runs `tests/cli.rs` against the artifact via `LUCIDA_TEST_BIN`, so the
  musl-static and universal builds get the full depth. The move paid for itself immediately:
  one bash assertion had been **unreachable** for as long as both its words appeared (an
  ordered `case` with the failing arm second), and all three MCP `provider` enums turned out
  to be hand-written literals — the one place a stale list makes a working provider
  *unreachable*, since a client validates against a JSON Schema `enum` before sending.
- **The README is a checked surface now** (`1bcac73`), not just prose. Rewritten for currency
  (six MCP tools not four, three video providers not one, a TOC, and the command table split
  so configuration comes first with the exhaustive `config --set` list) — and the audit found
  a defect under it. **`LUCIDA_BUDGET` was enforced but not in `KNOWN_KEYS`**, so `lucida
  config` reported the spending cap as *"ignored — check the spelling"* while it was in force.
  Three tests now hold the ground: every name read through `config::var` must be a known key
  (scanned from `src/`, so a new module is covered automatically), the README's settings table
  must match `KNOWN_KEYS` in both directions, and every anchor link must resolve — a broken
  one is silent on GitHub.
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
  positioning work is live (description and topics done; the ledger and video APIs settled at
  v1.0.0, so crates.io is now gated only on the signing certificate above).
- **Phase 3's video work shipped in v1.0.0.** The `VideoProvider` trait landed
  and video now has **three providers**: Veo, Runway (its own gen4 models only) and Kling
  (direct — all eight versions, three quality tiers). `--duration` answers the ROADMAP's open
  question: 8 s is **not** a hard limit, Veo does 4, 6 and 8. 220 tests.
- **The aggregator decisions below are SUPERSEDED in principle** (owner, 2026-08-09, after
  they were made): coverage is per-credential, not global — see AGENTS.md. A user holding only
  a Runway or only a fal subscription should be able to use its full width, so "reachable
  another way" is not a reason to exclude a lane. Restricting Runway to `gen4` and declining
  fal both rest on an argument that reasoned from a keyring holding every direct key. **Not
  yet acted on** — owner 2026-08-09: no further providers for the time being — but the next
  session should treat the exclusions as open questions rather than settled ones.
- **Aggregators are declined as built, for now.** Runway's fronted catalogue (Kling, Veo,
  Seedance, Hailuo, Grok, Gemini) was excluded by the owner on 2026-08-09; fal.ai was probed
  and declined the same day, because the models it fronts that we want — Kling, Veo, FLUX —
  are all now reached *directly*, so it would only add a worse second path to lanes we own.
  Two facts about fal are in the changelog for whoever revisits it: it authenticates with
  `Authorization: Key` rather than `Bearer`, and it queues a request **without validating**,
  so AGENTS.md §2's free-validation-error probe — how every provider here was characterised
  safely — does not work against it.
- **Left in Phase 3:** Stability's edit endpoints. **Owner 2026-08-09: no further providers
  for now.**
- **Audio is out of scope** — owner 2026-08-09, *"this starts to drift into Swiss army MCP."*
  Lyria was the cheapest expansion on the board and that is precisely why the decision is
  worth having in writing: both model ids were confirmed live on the existing Gemini key, on
  plain `generateContent` (simpler than any video lane), with Gemini TTS beside them on the
  same credential. **Cheapness is a temptation, not a reason.** Lucida is images and video.
  A **boundary, not a rejection**: the likely home is a separate tool, because the coverage
  principle applied to audio means ElevenLabs and its peers — a provider portfolio the size of
  Lucida's, wanting a vocabulary of voices and takes rather than aspect ratios and masks.
  Reasoning in [ROADMAP.md](../ROADMAP.md) § "Audio — out of scope"; reopen as its own
  project, not as a Lucida phase.

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
