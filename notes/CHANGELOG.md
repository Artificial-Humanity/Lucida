# Changelog

This document tracks technical changes, refactoring milestones, and build-system adjustments
for Lucida.

> **Maintenance:** Required by [AGENTS.md](../AGENTS.md) §3, which is the rule of record —
> append an entry **after committing** code work (source, build config, dependency
> manifests; docs-only commits are exempt), always carrying the short 7-character commit
> SHA. The log is append-only within a release cycle: entries are pruned **only** when a new
> version is tagged, at which point they collect under that version's heading. New entries
> go at the top under the current date, using the `Added` / `Changed` / `Fixed` / `Removed`
> structure.
>
> Maintained since **2026-08-06**, when the changelog + code-review cycle (AGENTS.md §3–§4)
> was adopted here to match the convention Prosodia already runs. History before that date
> lives in `git log` (the 2026-08-02 full-tree review, which narrated everything through
> v0.5.2, is in git history — superseded reviews are deleted per §4).

---

## 2026-08-06

Everything below closes a finding from the 2026-08-06 code review, which evaluated
`069f72d`…`f5ecf17` (v0.6.0 → v0.9.0). Verified on Linux at the end of the run: 138 tests
(from 125), `cargo clippy --all-targets -- -D warnings` clean, all `scripts/smoke.sh`
checks passing against the release build.

### Added

- **`MaskSupport { No, Advisory, Binding }` in `src/provider.rs`** (`83680b5`) — with
  `kind()`, `guarantee()`, `describe()`, plus `mask_providers()`,
  `mask_accepting_providers()` and `mask_semantics()`, which builds the cross-provider
  paragraph from `Backend::ALL`. `Capabilities.mask` changes from `bool` to this type; all
  five providers declare a variant. Closes **HN1**.
- **`config::write_replacing()`** (`1c49c47`) — stage-beside-then-rename, the pattern
  `update.rs`'s `install_over` already used, now applied to `config.env` and the Claude
  desktop config. Restricts the staged file *before* the rename, closing the window where
  a file was world-readable at the moment it first held a key. `restrict_permissions`
  moves out of `main.rs` as `config::restrict_to_owner`. Closes **LN1**.
- **`config::home_from()` and `setup::candidate_names()`** (`1c49c47`) — pure functions
  split out so both Windows path rules are pinned per platform from any platform, which
  is where they needed testing: the Windows CI lane runs under git-bash and cannot
  represent either case.
- **Six tests and one smoke check** (`83680b5`) — `MaskSupport` coverage of every masking
  provider; the mask refusal naming the binding provider; a ban on both semantics words in
  `SKILL.md`; a ban on `Backend::ALL`'s names and both semantics words in the `--mask` clap
  help, read back through `CommandFactory`; a refusal of any tagline claiming exclusive
  masking; and `smoke.sh` checking the refusal out of the shipped binary.

### Changed

- **Every mask surface is generated from the capability** (`83680b5`) — `lucida models`,
  both `mcp.rs` prose sites, the `image_providers` probe and the `Capabilities::check`
  refusal. The refusal names `comfyui` first, since a mask that binds is the better remedy;
  the old text offered only `openai`. The two surfaces that cannot be generated — the clap
  literal and `SKILL.md` — now name no provider and claim no semantics, only where the
  answer lives.
- **`provider_summary()` reports masking with its kind** (`83680b5`), and openai's tagline
  drops "the ONLY provider that can mask an edit to part of an image" — an eighth wrong
  surface the review had not found, printed by the MCP schema beside a computed capability
  list. `README.md` drops its provider count for the same reason.
- **`config::search_paths()` resolves on stock Windows** (`1c49c47`) — `home()` falls back
  to `USERPROFILE` like its two siblings, and `%APPDATA%\lucida\config.env` joins the list
  second (the reasoning that already puts `~/.config` ahead of the macOS location). The
  "could not determine a config location" message names all three variables. Closes **MN1**.
- **`setup::which()` consults `PATHEXT`** (`1c49c47`) — so `claude.exe` and `claude.cmd`
  are found, with the conventional four extensions as a fallback for an unset value.
  Closes **MN2**.
- **`merge_desktop_config` refuses a malformed `mcpServers`** (`1c49c47`) rather than
  panicking through serde_json's `IndexMut`, and leaves the file untouched. Closes **LN2**.
- **The update stamp uses `%LOCALAPPDATA%` on Windows** (`05e9aac`) instead of
  `%USERPROFILE%\.cache`; its test names each platform's location rather than matching
  `"ache"`, which `%LOCALAPPDATA%` would have failed.
- **`.github/workflows/ci.yml` derives its version pin** (`05e9aac`) — the second-newest
  non-draft release, rather than a hardcoded `v0.7.0` that made the job depend on one
  release surviving forever while pruning old ones is already contemplated. Verified
  against the live repo: resolves `v0.8.0`, installs it, version check passes.

### Removed

- **The unreachable `"GEMINI_API_KEY is set but empty"` branch** in `src/genai.rs`
  (`05e9aac`) — `config::var` filters blank values from both sources, so `from_env` cannot
  see one. An unreachable branch reads as evidence the case can happen. Closes **LN3**.

---

_Entries above this line open the log; earlier history is `git log`._
