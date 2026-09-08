# AGENTS — Lucida

This is the entry point for any agent or developer working on Lucida (media generation —
images and video — as a CLI and as an MCP server). This is an independent GitHub repo.
Public documentation lives in [docs/](docs/), which holds the roadmap. The current-state
snapshot is `STATE.md` in this project's **private** working notes, reachable in a checkout
of the umbrella workspace at `notes/STATE.md` (a gitignored symlink) and deliberately not
published — so it is named here rather than linked. Read it before starting work.

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
* **Anchor docs in `docs/` → `UPPERCASE`, single word preferred:** `ROADMAP.md`, `ARCHITECTURE.md`.
* **All other documents → `lowercase-kebab-case.md`:** e.g. `open-decisions.md`. This is the rule for anything in `docs/` that is not one of the anchors above.
* **Source code → the language's own convention:** Rust `snake_case.rs`, Swift `PascalCase.swift`, Kotlin `PascalCase.kt`.
* **Never** let case be the only difference between two paths, and always reference files with their exact case.

---

## System Operational Mandates

⚠ **There is no prescribed workflow here** (owner, 2026-09-08). The commit-hygiene
section that stood at §1 is gone, and nothing replaces it. The number below is left at
**2** deliberately: `docs/ROADMAP.md` and three internal documents — the state snapshot,
the code review and the product review — all cite "AGENTS.md §2", so the number is an
identifier those citations depend on rather than a position in a list. Renumbering it
would silently falsify every one of them, including the ones outside this repo where
nothing here can check them.

### 2. Paid-API Spend Discipline

* Hosted providers bill per render. **Probe with a free validation error before paying for a
  render**: send a deliberately invalid parameter value — the rejection names the parameter
  and lists its accepted values without rendering or billing (the entire 2026-08-02 probe
  batch cost about 7 cents this way). Modest verification spend is authorized; bulk renders
  are an owner call.
