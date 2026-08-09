# AGENTS — Lucida

This is the entry point for any agent or developer working on Lucida (media generation —
images and video — as a CLI and as an MCP server). This is an independent GitHub repo.
Internal engineering notes — changelog, current state, the active code review — live in
[notes/](notes/). Before starting work, read [notes/STATE.md](notes/STATE.md) for the
current state of the project.

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
  review (§5.1) — clap help strings, MCP parameter prose, `README.md`, remedy texts,
  `scripts/smoke.sh`. Add to that list the **GitHub repository description and topics**,
  which live outside the repo entirely and so cannot be tested from it: they read
  "Generate and edit images with Google's Gemini models" for four providers and all of
  video. The description is kept in step with `Cargo.toml`'s, and a test
  (`the_shopfront_names_every_provider_and_video`) holds that one and the `--help`
  banner against `Backend::ALL` — but the GitHub copy is updated by hand, with
  `gh repo edit`, and nothing will remind you.
* **Provenance:** hosted-provider output carries SynthID and/or C2PA marks; local ComfyUI is
  the only unmarked lane.
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

---

## Integration Dependencies

* Lucida is registered as a **user-scope MCP server**, so `generate_image` /
  `image_providers` / `start_video` / `check_video` are available in every project on this
  machine. A change to the MCP surface changes every agent session's tooling — treat schema
  and tool-description edits as public API.
* A recording proves Lucida still speaks **yesterday's** protocol, not that the provider
  still does: live verification is owed once per new provider or changed endpoint
  (ROADMAP §3). Since 2026-08-09 that limit also has a standing answer —
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

* **Canonical root marker files → `UPPERCASE`** (`SCREAMING_SNAKE_CASE` if multi-word): `README.md`, `LICENSE`, `CONTRIBUTING.md`, `CHANGELOG.md`, `ROADMAP.md`, `AGENTS.md`. Keep this set small and curated.
* **Top-level anchor docs → `UPPERCASE`, single word preferred:** `ARCHITECTURE.md`, `STATE.md`.
* **All other docs & notes → `lowercase-kebab-case.md`:** e.g. `open-decisions.md`, `code-review-findings.md`. This is the rule for everything in `notes/`.
* **Source code → the language's own convention:** Rust `snake_case.rs`, Swift `PascalCase.swift`, Kotlin `PascalCase.kt`.
* **Never** let case be the only difference between two paths, and always reference files with their exact case.

---

## System Operational Mandates

### 1. Commit Hygiene

* **Pull before push, every time.** The Mac and `ai-lab-0` (and their agent sessions) commit to
  the same `main` branch concurrently: run `git pull --rebase` as the first step of any
  commit-and-push sequence. If the tree holds the owner's uncommitted local edits, fetch and
  check ahead/behind instead of forcing a rebase.

### 2. Paid-API Spend Discipline

* Hosted providers bill per render. **Probe with a free validation error before paying for a
  render**: send a deliberately invalid parameter value — the rejection names the parameter
  and lists its accepted values without rendering or billing (the entire 2026-08-02 probe
  batch cost about 7 cents this way). Modest verification spend is authorized; bulk renders
  are an owner call.

### 3. Changelog Maintenance Requirement

* The project changelog lives at [notes/CHANGELOG.md](notes/CHANGELOG.md). Append a detailed chronological entry describing all technical modifications, refactoring milestones, and build-system changes **after committing** the corresponding work.
* **Scope: code work only.** Changelog entries are required for source, build-config, and dependency-manifest changes (`src/`, `scripts/`, `Cargo.toml`/`Cargo.lock`, `.github/workflows/`). They are **not** required for docs-only commits (`notes/`, `*.md`, comments-only changes).
* Every entry must be accompanied by the short 7-character commit SHA associated with the work.
* **The changelog is append-only across a release cycle.** Do not prune, rewrite, or remove historical entries. Entries are pruned/rolled over **only** when we tag and release a new version of the overall project — at which point the released entries are collected under that version's heading and the working section is reset for the next cycle.
* New entries go at the top under the current date, following the existing `Added` / `Changed` / `Fixed` / `Removed` structure.

### 4. Code Review Execution Standards

* **Scope: code work only.** Code reviews cover the same code changes that warrant changelog entries (see §3) — source, build config, and dependency manifests. Docs-only commits are out of scope and need no review.
* **A review is a report, not a fix pass.** Assume the deliverable is the findings document alone: the reviewing agent takes on fixes only when the owner explicitly asks it to, never as a rider on the review itself.
* **The review itself never warrants a changelog entry.** Review documents live in `notes/`, and writing, replacing, or deleting one is docs-only work under §3; the changelog material is the code commits that later close the findings.
* When performing a code review, cross-reference the changelog and corresponding commits.
* Create a review document matching the format `notes/code-review-[year][month][day]-[hhmmss].md`. Begin the document with the first evaluated short commit SHA, and end with the last evaluated commit SHA.
* Determine the range of commits to review by starting with the commit immediately following the end SHA of the *previous* code review. If no prior review exists, use all commits from the previous and current day.
* Once the new code review document has been written, delete the previous one to keep only the latest review active.
* Repoint the **Latest code review** pointer in [notes/STATE.md](notes/STATE.md) to the new document (only the link target changes; the surrounding line is phrased generically) so a session can find the current review without globbing the folder.
