# AGENTS — Lucida

This is the entry point for any agent or developer working on Lucida (media generation —
images and video — as a CLI and as an MCP server). This is an independent GitHub repo.
Public documentation — the current state and the roadmap — lives in [docs/](docs/).
Before starting work, read [docs/STATE.md](docs/STATE.md) for the current state of the
project.

---

## Core Stack Matrix

* **Language Ecosystem:** Rust, one static binary, zero runtime dependencies. The
  no-new-dependency posture is deliberate and extends to test infrastructure: JSON-RPC is
  hand-rolled in `src/mcp.rs`, and wire behaviour is pinned by the recorded-response test
  server in `src/testserver.rs` (scripted replies transcribed from real provider sessions).
* **Image providers (five):** Google Gemini (`genai.rs`), local ComfyUI (`comfy.rs`), Black
  Forest Labs hosted FLUX (`bfl.rs`), Stability AI (`stability.rs`), OpenAI (`openai.rs`).
  Video is Veo (`video.rs`, sharing genai's client).
* **Capability truth lives in code, not prose.** `Backend::ALL` and the capabilities tables
  generate provider lists wherever the shape allows. The 2026-08-02 review's headline
  finding: every generated list stayed true while every hand-written one rotted. When
  provider six lands, start from the known hand-written drift surfaces recorded in that
  review (§5.1) — clap help strings, MCP parameter prose, `README.md`, remedy texts.
  The MCP `provider` enums came off that list on 2026-08-09: all three were literals, and an
  `enum` is the worst place for one, because a well-behaved client *validates against it* — a
  provider missing there is unreachable rather than undocumented, the call never arrives, and
  no refusal message gets the chance to name it. They generate from `Backend::ALL` /
  `VideoBackend::ALL` now, and `tests/cli.rs` holds the surviving hand-written list (the
  `--provider` clap doc comment) against the generated set over the wire.
  Add to that list the **GitHub repository description and topics**,
  which live outside the repo entirely and so cannot be tested from it: they read
  "Generate and edit images with Google's Gemini models" for four providers and all of
  video. The description is kept in step with `Cargo.toml`'s, and a test
  (`the_shopfront_names_every_provider_and_video`) holds that one and the `--help`
  banner against `Backend::ALL` — but the GitHub copy is updated by hand, with
  `gh repo edit`, and nothing will remind you.
* **Provenance:** hosted-provider output carries SynthID and/or C2PA marks; local ComfyUI is
  the only unmarked lane.
* **Width is per-provider too** (owner, 2026-08-09). Lucida covers image generation *and*
  video generation, and **each provider should be as completely represented as possible across
  both**. A provider present for one medium and absent for the other is a coverage gap, not a
  finished integration: as of v1.0.1, runway and kling are video-only here while both offer
  image generation, and openai is image-only. The live matrix and the ordered work list are in
  `docs/ROADMAP.md` § 5. This widens providers that already exist, which is **not** what the
  2026-08-09 pause on new providers covers — the owner clarified the pause meant new
  *providers*, not new endpoints.

  One caveat that section records and this rule does not override: **an API with a published
  removal date is a countdown, not coverage.** OpenAI's Sora is the live case.
* **Coverage is per-credential, not global** (owner, 2026-08-09). The point of many providers
  is not one entry per model family — it is that someone holding a *subset* of these
  subscriptions can use the **full width of what they pay for**. So "that model is reachable
  another way" is never on its own a reason to leave a lane unexposed: it is a fact about
  whichever keyring the decision was made on, and a user with only a Runway or only a fal
  subscription has a different one. This overturns the argument used on 2026-08-09 to restrict
  Runway to its own `gen4` models and to decline fal — an argument that reasoned from a
  machine holding every direct key.

  What it does **not** overturn is why aggregated lanes are harder: capabilities are not
  knowable per model, provenance passthrough is undocumented, and pricing carries a margin.
  The answer to those is honest labelling rather than exclusion, and the vocabulary already
  exists — `Provenance::Unverified` says nobody has checked, `Price::Unverified` says the rate
  is not confirmed. An aggregated model may ship carrying weaker claims, provided the claims
  it carries are true.
* **Verification trio:** `cargo test`, `cargo clippy --all-targets` (kept warning-free so
  the next warning is visible), and `scripts/smoke.sh` — all three green before tagging a
  release. A release ships three platform assets with checksums (macOS universal, Linux
  musl-static, Windows); a release missing an asset is the v0.5.0 failure mode.
* **Two test layers, one place each.** Unit tests live in `#[cfg(test)] mod tests` inside the
  file they test and can reach private functions. Anything that only exists once there is a
  *process* — exit codes, `--json` alone on stdout, the config search path, JSON-RPC framing —
  goes in `tests/cli.rs`, which drives the binary as a black box with `env_clear` and a private
  `HOME`. Those assertions used to be bash inside `scripts/smoke.sh`, where they could not run
  before a commit and where one of them had silently stopped asserting: an ordered `case` whose
  failing arm sat *after* the arm that matched. `smoke.sh` now runs `tests/cli.rs` against the
  packaged artifact via `LUCIDA_TEST_BIN`, so the musl-static and universal binaries are
  covered to the same depth as the debug build and there is only one copy of each assertion —
  which a test in that file enforces. **Do not add bash assertions back to `smoke.sh`;** it is
  for what packaging can break (does the artifact load, does it know its version).

---

## Integration Dependencies

* Lucida is registered as a **user-scope MCP server**, so `generate_image` /
  `image_providers` / `start_video` / `check_video` are available in every project on this
  machine. A change to the MCP surface changes every agent session's tooling — treat schema
  and tool-description edits as public API.
* A recording proves Lucida still speaks **yesterday's** protocol, not that the provider
  still does: live verification is owed once per new provider or changed endpoint
  (`docs/ROADMAP.md` §3). Since 2026-08-09 that limit also has a standing answer —
  `scripts/canary.sh` probes every provider live and costs nothing by construction (free
  endpoints, plus render requests naming a model that cannot exist). **It runs from a
  weekly cron on ai-lab-0**, where the five keys already live; the GitHub workflow is
  `workflow_dispatch` only, deliberately, so the credentials gain no second home. A
  successful render inside the canary is reported as a *failure* — it would mean money was
  spent by a script whose contract is that it spends none.

---

## File Naming Conventions

Names must be predictable so links resolve on case-sensitive systems (Linux/CI) as well as
case-insensitive macOS/Windows.

* **Canonical root marker files → `UPPERCASE`** (`SCREAMING_SNAKE_CASE` if multi-word): `README.md`, `LICENSE`, `CONTRIBUTING.md`, `AGENTS.md`. Keep this set small and curated.
* **Anchor docs in `docs/` → `UPPERCASE`, single word preferred:** `ROADMAP.md`, `STATE.md`, `ARCHITECTURE.md`.
* **All other documents → `lowercase-kebab-case.md`:** e.g. `open-decisions.md`. This is the rule for anything in `docs/` that is not one of the anchors above.
* **Source code → the language's own convention:** Rust `snake_case.rs`, Swift `PascalCase.swift`, Kotlin `PascalCase.kt`.
* **Never** let case be the only difference between two paths, and always reference files with their exact case.

---

## System Operational Mandates

### 1. Commit Hygiene

* **`main` is PR-only. Do not push to it directly** (owner, 2026-08-10). Branch, push the
  branch, open a PR, and let it merge. This applies to agent sessions exactly as it applies to
  the owner — an agent that "just needs one small fix on `main`" is the case the rule exists
  for. Two reasons it is a rule and not a preference:
  * **The Mac and `ai-lab-0` (and their agent sessions) commit concurrently.** Direct pushes to
    a shared `main` are how two sessions silently interleave half-finished work; a branch is a
    place for work to be incomplete without being everyone's problem.
  * **Nothing reviews a direct push.** `.github/workflows/claude-review.yml` triggers on
    `pull_request`, so work that skips the PR skips the review entirely — the automation
    cannot see a commit that was never proposed.
* **Branch naming**: `<type>/<short-slug>` matching the commit type — `fix/`, `feat/`,
  `docs/`, `chore/`.
* **Work on the branch, commit and push liberally, open the PR only when the work is done**
  (owner, 2026-08-10). Pushing to your own branch is free and is the entire point of having
  one: commit early, commit often, push whenever, and let the branch hold work that is not
  yet finished. What is deliberate is the *timing of the PR*, not the timing of the commits.
  * **When completion is defined, completion opens the PR.** If a `/goal` has been set,
    achieving that goal IS the completion point — open the PR then, without being asked again.
  * **Otherwise the owner calls it.** With no goal set, work, push, and wait: the owner
    acknowledges the completion point and the PR follows from that.
  * **This is also what makes it cheap.** `.github/workflows/claude-review.yml` fires when a
    PR is opened AND on every push to an open one, so a PR opened at the *start* of the work
    bills a full model-rate review of half-finished code on every intermediate push. Opening
    at completion buys exactly one review, of work that is actually ready to be read.
* **Pull before push, every time.** Run `git pull --rebase` as the first step of any
  commit-and-push sequence on your branch, and rebase on `main` before opening the PR. If the
  tree holds the owner's uncommitted local edits, fetch and check ahead/behind instead of
  forcing a rebase.
* **The exception is the owner's, not yours.** If the owner explicitly directs a direct push to
  `main`, that is their call and does not need re-litigating — state the rule once, then do as
  asked. An agent never grants itself the exception.
* ⚠ **A rule in this file is not an enforcement mechanism.** The authority is the branch
  protection on `main`; this section only explains it. If a direct push to `main` ever
  *succeeds*, the protection is missing or was bypassed — report that rather than treating it
  as permission.* **Review feedback is closed with the `claude-fix` label, not by hand-waving.** The review
  workflow only comments; `.github/workflows/claude-fix.yml` is what acts on those comments.
  Add the `claude-fix` label to the PR and the fix agent reads the inline comments, commits
  the fixes, replies, and removes the label. It is label-gated deliberately: firing it
  automatically on every submitted review oscillates (fix pushes → `synchronize` → new review
  → fix pushes), and the vendor ships no loop guard. One label, one pass; re-label to run it
  again. A review comment is an argument, not an order — the fix agent is expected to push
  back in a reply where a finding is wrong, rather than making a change it believes is wrong.


### 2. Paid-API Spend Discipline

* Hosted providers bill per render. **Probe with a free validation error before paying for a
  render**: send a deliberately invalid parameter value — the rejection names the parameter
  and lists its accepted values without rendering or billing (the entire 2026-08-02 probe
  batch cost about 7 cents this way). Modest verification spend is authorized; bulk renders
  are an owner call.
