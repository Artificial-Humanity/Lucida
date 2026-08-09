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

## Working section — unreleased

### 2026-08-09 — the product review's second tranche, in progress

Towards v0.10, "dependable unattended use". The review's **third and last structural finding
closes here**, so all three are now closed. 186 tests, up from 163 at v0.9.2.

Still open for v0.10: cost visibility and a budget guard, `--json` + meaningful exit codes,
`--count` batching, and the live canary (which the owner has ruled runs from an ai-lab-0 cron
rather than GitHub Actions secrets, with the workflow kept `workflow_dispatch`-only).

#### Added

- **`src/ledger.rs`, `lucida ops`, `lucida history`, and the `list_operations` MCP tool**
  (`b2daf7f`) — an append-only JSONL record of every render, beside `config.env`. Lucida
  remembered nothing before this: no prompt→file trail, no way to list video operations in
  flight, no history, and nowhere for cost to accumulate even in principle. The failure it
  closes is specific — an agent starts a Veo render, hands back an operation id, its session
  ends, and minutes of billed output become unreachable because the id lives only in a
  transcript. Outstanding operations are **derived** (a `started` record with no `done` one),
  so a render collected anywhere drops off the list with nothing told. Writes never fail a
  render; the file prunes its oldest half at 2 MB; `LUCIDA_NO_LEDGER` switches it off and
  `lucida config` names the path either way, because it records prompts. **`list_operations`
  is a deliberate addition to the user-scope MCP surface** (AGENTS.md, Integration
  Dependencies).
- **`src/clock.rs`** (`b2daf7f`) — the date arithmetic moves out of `provider.rs` and gains
  the inverse conversion, so a ledger timestamp is stored as a number and formatted for a
  human rather than frozen into somebody's idea of a format. `stamp` needed `div_euclid`:
  written with `/`, 1969 came out as 1970.
- **`src/cancel.rs`** (`e52cbd5`) — a `Token` (an `Arc<AtomicBool>`), `with()` to install one
  for the duration of a call, and `check()` for poll loops to call between sleeps. Ambient to
  the thread rather than a parameter on `ImageProvider::generate`: threading it through the
  trait would be paid for by five implementors, the CLI and every future provider to serve one
  caller, and the CLI has a user with a Ctrl-C. Wired into the poll loops in `comfy.rs`,
  `bfl.rs` and `video.rs`.
- **A worker pool on the MCP server** (`e52cbd5`) — four workers, an `InFlight` registry keyed
  by request id, a `Mutex`-guarded writer, and `guarded()` catching a panicking tool call.
  `serve` splits into `run(reader, out, handle)` so the behaviour can be asserted.
- **`Clients` in `src/setup.rs`** (`bda08fe`) — client presence as a parameter rather than a
  probe, so `plan()` is testable in all four combinations.
- **`Client::unique_upload_name`** (`bda08fe`) — pid + counter + clock, with the source
  basename kept in the middle.
- **`genai::mime_of`** (`e85f662`) — sniff first, extension only for what the sniffer does not
  know (GIF), PNG as the last resort.
- **A `verify` job in `release.yml` and an `audit` job in `ci.yml`** (`e85f662`).

#### Fixed

- **The MCP server no longer blocks its stdio loop for the whole render** (`e52cbd5`) — the
  review's first structural finding. `dispatch` ran to completion before the next line was
  read, so a ComfyUI render held the loop for up to 1800 s: `ping` went unanswered (read as a
  dead server), a second call queued invisibly, and `notifications/cancelled` was unreachable
  — it carries no id of its own, so the early return for id-less messages dropped the one
  message whose purpose is stopping a paid render. Cancellation is cooperative and lands only
  where there is a poll loop; a single blocking POST runs to completion, and the message says
  the charge stands.
- **Capabilities print without a credential** (`4751e9f`) — both `lucida models` and
  `image_providers` opened a client first and returned before the table when one could not be
  built, contradicting `capabilities_for`'s own doc comment. `ImageProvider::capabilities()` is
  removed from the trait and all five implementors, since asking a *client* what its provider
  supports is the coupling itself.
- **ComfyUI upload collisions** (`bda08fe`) — two callers editing different files both named
  `image.png` overwrote each other's upload between the upload and the render, silently, each
  getting an edit of the other's picture.
- **Provenance on CLI renders** (`bda08fe`) — reported by MCP on every render, by the CLI only
  in `lucida models`.
- **`lucida setup` named a skill file it never wrote** (`bda08fe`) — with the Claude app and no
  Claude Code CLI, the skill step was never planned but the note telling you to upload it still
  printed.
- **`Client::generate_video` becomes `await_video`** (`b2daf7f`) — the CLI starts and waits in
  two visible steps, because the operation id has to exist where it can be written down and a
  single blocking call hid it inside itself.
- **Release tags could ship without the suite having run** (`e85f662`) — the three build jobs
  smoke-tested their own binary; `cargo test` ran only on `main`. Also `--locked` everywhere,
  `rust-version = "1.85"`, and the genai MIME guess replaced by `sniff_mime`.

#### Changed

- **Two bugs found by the new tests rather than by reading** (`e52cbd5`) — the worker loop was
  written `while let Ok(job) = receiver.lock().unwrap().recv()`, which holds the scrutinee's
  `MutexGuard` for the whole body, so four workers behaved exactly like one loop
  (`tool_calls_run_concurrently_rather_than_queueing` failed with "only 1 of 4 calls ran at
  once"); and a panicking tool call silently cost a worker, so after four the server would
  accept calls and answer none while still passing `ping`.

---

## v0.9.2 — released 2026-08-09

The first tranche of the 2026-08-09 product review: the release the review asked for by name,
"keep the existing promises". Nothing new is offered — every item is the implementation
failing to do something the product already claims, and two of the three structural findings
close here (the third, the MCP server blocking its stdio loop, is v0.10 work).

Released from `main` with the verification trio green (163 tests, up from 138; `cargo clippy
--all-targets -- -D warnings` clean; all `scripts/smoke.sh` checks) and CI green on all three
platforms at `8e48b10`.

### 2026-08-09 — the 2026-08-09 product review, first tranche

Everything below closes a finding in [product-review-20260809.md](product-review-20260809.md),
and every one of them is the implementation failing to keep a promise the product already
makes. No new capability; the review's own framing is "keep the existing promises". 163 tests,
up from 138.

#### Added

- **`src/retry.rs`** (`e39a6fd`) — `send_idempotent()`, up to three attempts with exponential
  backoff, honouring `Retry-After` (delta-seconds form) and capping what that header can ask
  for. Transient means a connection that never opened, a timeout, a 429, or a 5xx; a 4xx
  returns at once. Takes a closure rather than a `RequestBuilder`, since a builder is consumed
  by `send` and `try_clone` gives up on exactly the bodies worth retrying, and returns
  `reqwest`'s own `Result` so every caller keeps the error mapping it had. Six tests, driven by
  `testserver` with the backoff parameterised so the suite does not spend it.
- **`retry::CONNECT_TIMEOUT`** (`7fcc22f`) — five seconds, applied by all eleven HTTP clients.
- **`video::resume_notice()` and `lucida video --no-wait`** (`63fdd92`) — the operation id and
  the `lucida check` line that collects it, printed the moment a render starts; and the CLI
  equivalent of what `start_video` has always done on the MCP surface.
- **`provider::RETIREMENTS`, `retirement_note()`, `unix_time()`** (`92da839`) — one declared
  table of announced shutdown dates, with the tense computed against today. `unix_time` is
  eleven lines of `days_from_civil` rather than a date dependency; an unparseable date reads as
  future, so a typo can only ever understate.
- **`write_atomically()` at the crate root** (`a5e6b84`) — the staging mechanism lifted out of
  `config::write_replacing`, which is now three lines that call it.
- **`Backend::product_name()`** (`f09cdd5`) — the prose spelling of a provider (`FLUX`, not
  `bfl`), deliberately `#[cfg(test)]`: its only job is to be the list a shopfront surface can
  be measured against.
- **Typed MCP argument accessors** (`ec43ba9`) — `optional()`, `opt_str`, `opt_string`,
  `opt_u64`, `opt_f64`, `opt_str_array`, `req_str` in `src/mcp.rs`.

#### Fixed

- **A wrongly-typed MCP argument is refused rather than dropped** (`ec43ba9`) — the review's
  second structural finding. `as_str`/`as_array`/`as_u64` answer `None` for a type mismatch
  exactly as for a missing value, and `None` meant "not requested", so
  `"reference_images": "photo.png"` turned an **edit into a fresh generation, reported as
  success**; a stringified `seed` was dropped, losing reproducibility. Array *elements* are
  checked too, naming the index. Seven tests, none needing credentials.
- **A paid Veo render can no longer be lost from the CLI** (`63fdd92`, `e39a6fd`) — the third
  structural finding. The operation id was printed only in the deadline branch, so a 502, a
  Ctrl-C, a closed laptop lost it; and nothing retried, so one transient poll failure ended the
  wait. Both halves closed.
- **`connect_timeout` on every client** (`7fcc22f`) — without it reqwest falls back to the full
  request timeout for the handshake, so a blackholed host hung for 300 s and the
  `image_providers` probe, which walks five providers in sequence, had a worst case of roughly
  twenty minutes inside a tool advertised as spending nothing.
- **Image writes are atomic** (`a5e6b84`) — `lucida edit` defaults its output to its own input,
  so a failed truncating write destroyed the user's original *and* the edit. The staging name
  gains a counter beside the pid, because two writes can now be in flight within one process.
- **Shutdown dates no longer go stale in prose** (`92da839`) — Imagen's 2026-08-17 date was
  future-tense in five places eight days before it; the three OpenAI ids retiring 2026-12-01
  were recorded nowhere and listed like current models.
- **The shopfront names what the tool does** (`f09cdd5`) — the repository description said
  "Generate and edit images with Google's Gemini models" through four providers and all of
  video; the `--help` banner omitted video. Both fixed, description aligned with
  `Cargo.toml`'s, seven topics added.
- **BFL and OpenAI print the signed download URL on a failed download** (`e39a6fd`) — the
  render is finished and billed at that point and the URL expires in about ten minutes, so the
  message was the only possible recovery and did not contain it.

#### Changed

- **Three tests now read the source or the schema rather than trusting prose**
  (`7fcc22f`, `f09cdd5`) — every `Client::builder()` chain must reach `.build()` with a connect
  timeout; the package description and `--help` banner must name every `Backend::ALL` entry and
  video. Both surfaces are ones nothing can generate, which is why they rotted.

---

## v0.9.1 — released 2026-08-07

The whole version is the 2026-08-06 code review, closed. Nothing new was added to the tool:
every item below fixes something the review found, and the headline one is structural rather
than a correction — whether a mask binds is a value now, not prose repeated on eight
surfaces.

Released from `main` with the verification trio green on this machine (138 tests, up from
125; `cargo clippy --all-targets -- -D warnings` clean; all `scripts/smoke.sh` checks against
the release build) and CI green on all three platforms.

### 2026-08-06 — closing the review of `069f72d`…`f5ecf17` (v0.6.0 → v0.9.0)

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

_v0.9.1 is the first version this log covers. Everything before it is `git log` and the
release notes on GitHub — the log opened 2026-08-06 with no backfill._
