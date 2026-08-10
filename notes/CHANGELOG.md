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

#### Added
- **`.github/workflows/claude-fix.yml` — the review loop is closed** (`11574ca`). The reviewer only
  ever commented; nothing acted on those comments. Adding the `claude-fix` label to a PR now runs
  a fix pass that reads the inline review comments (they are **not** in `gh pr view` — it fetches
  `/pulls/N/comments`), commits fixes, replies with what it changed and what it deliberately did
  not, and removes the label.
  - **Label-gated on purpose.** Firing on `pull_request_review` submitted oscillates: fix pushes →
    `synchronize` → reviewer posts → fix pushes → …, each lap billed at full model rates. The
    vendor ships no loop guard and does not document which token it pushes with, so GitHub's own
    recursion protection can't be relied on either. One label, one pass; re-label to repeat.
  - Load-bearing details: `contents: write` (this job pushes, the reviewer only comments);
    checkout `ref: head.ref` + `repository: head.repo.full_name` (the default checkout is a
    detached merge commit, which cannot be pushed from); `fetch-depth: 0`; and
    `cancel-in-progress: false`, since cancelling mid-pass can leave edits applied but uncommitted.

#### Fixed
- **`.github/workflows/claude-review.yml` — the automated PR reviewer could not post** (`bb8809a`).
  It ran to completion and delivered nothing. Three independent gaps, each a silent no-op on
  its own, plus a wrong secret name:
  - **No `permissions:` block.** This repo's default workflow token is read-only
    (`default_workflow_permissions: "read"`), so the job analyzed the diff, billed the tokens,
    and had no right to comment. Added `contents: read` / `pull-requests: write` /
    `id-token: write`.
  - **No `--allowedTools`.** Permissions grant the TOKEN the right; `--allowedTools` grants the
    AGENT the tool. Without the inline-comment MCP tool and the `gh` allowlist there was no
    mechanism to post at all.
  - **The prompt never said to post to GitHub** — added `REPO` / `PR NUMBER` context and
    explicit `gh pr comment` / `create_inline_comment` instructions.
  - **The secret is the org-level `CLAUDE_OAUTH_TOKEN`**, not `CLAUDE_CODE_OAUTH_TOKEN`. The
    action's *input* name stays `claude_code_oauth_token`, so the two deliberately disagree;
    the file carries a comment on that line because it reads as a typo.
- Dropped `--effort xhigh` — not a documented `claude_args` flag, an unrecognized flag fails the
  run outright, and Claude Code already defaults to xhigh effort on capable models.
- Replaced the severity filter. "Ignore superficial style or formatting nitpicks" is followed
  *literally*: the model finds the bugs, then declines to report anything below the bar, so
  precision looks excellent while real findings vanish. Now a concrete bar (anything that could
  cause incorrect behavior, a test failure, a security weakness, or a misleading result) with
  explicit severity, and an instruction not to filter past it.
- Softened "security exploits" to "security vulnerabilities" (`claude-fable-5` runs classifiers
  over cybersecurity content and can return `stop_reason: "refusal"`), added a `concurrency`
  group so rapid pushes cancel superseded billed runs, and pinned checkout `@v6`.

---

## v1.0.1 — released 2026-08-09

**Packaging only. No behaviour changed, and the compiled binary is the same as v1.0.0's.**

Cut so that a published crate and a git tag are the same tree. v1.0.0 was tagged before the
crates.io metadata existed, and publishing 1.0.0 afterwards would have meant a package whose
`Cargo.toml` did not match the tag it claimed to be — identical code, but a discrepancy of
exactly the kind this project spends its time refusing elsewhere. A patch release costs one
workflow run and removes the footnote entirely.

#### Added

- **crates.io package metadata** (`b8bb83a`) — `repository`, `homepage`, `readme`, five
  `keywords` and three `categories`. None of it is needed to build, which is why nothing ever
  warned: the package would have published with a description and nothing else — no link to
  the source, and no category or keyword anyone could have found it under.
- **`exclude`** (`b8bb83a`), stating what ships rather than inheriting whatever is tracked. It
  keeps out `video.mp4` — a 4.9 MB test render committed by accident in `d37737c` — and the
  internal review notes. Roughly a tenth of the crate would otherwise have been a video of
  fog. `notes/CHANGELOG.md` stays; a product review does not belong in a package. Result: 43
  files, 273 KiB compressed, verified to compile from the packaged sources rather than from
  the working tree.

**Still tracked in the repository:** `video.mp4` itself. Excluding it from the package is not
the same as removing it from git, and the second is the owner's call.

---

## v1.0.0 — released 2026-08-09

**Video became a substitution, and the tests grew a second layer.** The version number is the
claim worth reading: the surfaces this tool is used through — the `ImageProvider` and
`VideoProvider` traits, the MCP tool set, the four exit codes, the `--json` documents, the
ledger format and the config names — are ones callers can now build against and expect to
survive. Not that the work is finished.

Three things earned it. **Video stopped being one hardcoded lane**: Veo, Runway and Kling
behind a `VideoProvider` trait, so video is a substitution the way images have been since
v0.4. **Scope got a boundary** rather than a backlog: audio was declined on the day it was
cheapest to add, which is the only day a scope decision means anything. And **the layer a
user actually touches is now tested by `cargo test`** rather than by a shell script in CI —
which immediately turned up a bash assertion that had been unreachable for months and three
MCP `provider` enums that were hand-written literals, the one place a stale list makes a
working provider *unreachable* rather than merely undocumented.

Also in this release: `--dry-run` on every paid command, a `video_providers` probe the shipped
skill had been promising but not delivering, guidance to ask the user which provider and model
to use when the choice is theirs, and a `lucida config` that no longer reports an enforced
spending cap as ignored.

Released from `main` with the verification trio green (**255 tests**, up from 201 at v0.10.0;
`cargo clippy --all-targets -- -D warnings` clean; all `scripts/smoke.sh` checks against the
release build) and CI green on all three platforms at `11409e1`.

Not in scope and deliberately so: audio (see `ROADMAP.md` § "Audio — out of scope"), further
providers (owner, 2026-08-09), and publishing to crates.io — which remains available and
unrequested.

### 2026-08-09 — The README, and the setting it was right to omit

Owner: the README had gone stale. It had — and auditing it found a live defect underneath.
**255 tests**, up from 252.

#### Fixed

- **`LUCIDA_BUDGET` was missing from `config::KNOWN_KEYS`** (`1bcac73`) while `spend::budget`
  read it through `var` like every other setting. A budget set in the config file was
  therefore **fully enforced**, and `lucida config` filed it under *"in the config file but
  not recognised (ignored — check the spelling)"* — the one command whose job is to report
  what is in effect, saying the spending cap was not. The direction of that lie is what makes
  it serious: someone who believes the message concludes they have no cap, or deletes the line
  that was protecting them.
- **`LUCIDA_CONFIG` was documented as a config-file setting** (`1bcac73`), which it cannot be
  — it names the file the settings live in. It and the retired `GOOGLE_API_KEY` moved to a
  separate table that says why.
- **Two clap help strings** (`1bcac73`) — `video` still said "with Veo" across three
  providers, `models` said "image models" after it learned to answer for video.

#### Added

- **`every_setting_read_is_a_known_key`** (`1bcac73`) — walks `src/` and asserts every name
  passed to `config::var` appears in `KNOWN_KEYS`. Directory-walked rather than listed, so a
  new module is covered without anyone remembering; a second hand-written list would only
  move the problem.
- **`the_readme_lists_every_setting`** (`1bcac73`) — the settings table checked against
  `KNOWN_KEYS` in **both** directions. A missing row is a setting nobody knows about; a
  surplus row is a setting that does not exist, which costs somebody an afternoon. Its first
  version passed by slicing an empty string, which is the failure this whole cycle keeps
  turning up: an assertion that cannot fail reads as coverage.
- **`every_readme_link_points_at_a_heading_that_exists`** (`1bcac73`) — resolves all 26
  anchors against the headings, implementing GitHub's slug rule. A broken anchor is silent on
  GitHub, so nothing else would ever report it.

#### Changed

- **README rewritten** (`1bcac73`) — a table of contents; the command table split so
  **configuration comes first and separately**, carrying the exhaustive `config --set` list;
  video described as three providers rather than Veo; `--dry-run`, `--count`, `--duration`,
  `--mode` and `--image` documented; the MCP tool table corrected from four tools to six; and
  the per-credential coverage principle stated where a reader meets the provider list.

### 2026-08-09 — The binary gets tested like everything else

Owner's observation, and it was a fair one: `cargo test` covered every layer except the one a
user touches. **252 tests, up from 221** — 222 unit and a new 30-test binary-level suite.

#### Added

- **`tests/cli.rs`** (`3558134`) — the shipped binary as a black box: exit codes, `--json`
  alone on stdout, the config search path resolved from a real environment, the capability
  refusals, the ledger's runtime-resolved location, and JSON-RPC framing over a real pipe.
  Every process gets `env_clear` and a private `HOME`, so a credential on the developer's
  machine cannot turn an assertion into a no-op and the suite runs identically on a laptop
  and on CI. Nothing reaches a provider; the one socket opened points at a port chosen
  because nothing is listening on it.
- **`LUCIDA_TEST_BIN`** (`3558134`) — points that file at a binary other than cargo's, which
  is how the release workflow now covers the musl-static and lipo-fused artifacts to the same
  depth as a debug build. Verified to be honoured rather than decorative.

#### Changed

- **`scripts/smoke.sh` stopped being a second, weaker test suite** (`3558134`) — it checks
  what packaging can break (does the artifact load, does it know its own version) and then
  runs `tests/cli.rs` against it. One copy of each assertion, and a test in that file asserts
  the delegation still exists so the two cannot silently fork again.
- **All three MCP `provider` enums are generated** (`3558134`) from `Backend::ALL` /
  `VideoBackend::ALL`. They were literals, one of them directly above a `model` description
  that is generated *because the hand-written version of itself had omitted openai*. This is
  the worst place in the product for a stale list: a well-behaved client validates against a
  JSON Schema `enum` before sending, so a provider missing there is **unreachable** rather
  than undocumented — the call never arrives, and none of the carefully-written refusals get
  the chance to name it. Nothing would have reported it, because the code behind the missing
  provider works perfectly. `every_provider_is_selectable_through_the_schema` covers all three
  surfaces, including `check_video`, where forgetting one strands an operation.

#### Fixed

- **A mask assertion that had stopped asserting** (`3558134`) — an ordered bash `case` whose
  "offers only an advisory mask" arm sat *after* the arm that matched, and so was unreachable
  for as long as both words appeared in the message. Rewritten as the property it meant: the
  refusal must name the provider whose mask *binds*. This is the failure mode that motivated
  the move — a `#[test]` cannot fail this way, and `set -e` never protected against it.
- **A wire assertion that could not fail** (`3558134`) — the new provider check matched names
  against the whole `--help` output, and `--seed` names openai too, so deleting openai from
  the `--provider` list left it green. Scoped to the one flag's entry and confirmed to bite by
  breaking it deliberately.

### 2026-08-09 — Phase 3: video becomes a substitution

The product review's expansion axis #1, and the first work since v0.10.0. Video went from one
hardcoded lane to three providers behind a trait. 220 tests, up from 201 at v0.10.0.

#### Added

- **`VideoProvider`, `VideoBackend`, `VideoCapabilities`, `DurationSupport`** (`294ddcd`) —
  the video twin of the image abstraction: `start`/`poll`, `VideoBackend::ALL`,
  `video_capabilities_for`, and a `check` that refuses before a client exists.
  `VideoCapabilities` is deliberately *not* a reuse of `Capabilities` — no mask, no steps, no
  guidance, and a duration images have no concept of, so sharing one struct would mean half
  the fields being meaningless per request.
- **`src/runway.rs` — Gen-4 video** (`294ddcd`) — Runway's own `gen4_turbo`/`gen4`/`gen4.5`
  **only**. Its endpoint also fronts Kling, Veo, Seedance, Hailuo, Grok and Gemini, which
  would make Lucida an aggregator front-end; `veo3.1` there is a second, worse path to a lane
  already reached directly. Owner's call 2026-08-09, pinned by a test.
- **`src/kling.rs` — Kling, direct** (`d37737c`) — all eight model versions and three quality
  tiers, measured rather than assumed (8 models × 5 ratios × 3 tiers, uniform). Reached on
  Kling's own API, deliberately not through Runway's passthrough.
- **`--duration`, `--seed`, `--mode`, `--provider` on `video` and `check`** (`294ddcd`,
  `d37737c`) — `--duration` answers the ROADMAP's open "is 8 s a hard limit?" with a no: Veo
  does 4, 6 and 8. `mode` is a quality tier, a concept only Kling has.
- **`lucida models --provider <video>`** (`294ddcd`, `d37737c`) — live credit balances for
  Runway and Kling, and the capability table whether or not a key is present.

#### Changed

- **The MCP video schema is generated from `VideoBackend::ALL`** (`294ddcd`) — it said
  "Google only" and "output carries a SynthID watermark and a C2PA manifest", both false the
  instant a second provider landed.
- **The blocking wait loop moves out of Veo into one shared helper** (`294ddcd`) — waiting is
  a front-end decision, not a provider one, so both lanes get the same backoff, deadline and
  cancellation check rather than each reinventing them.
- **Per-second pricing multiplies by clip length** (`294ddcd`) — a rate without a duration is
  not a price, and a 2-second test is not a 10-second render.
- **`RUNWAYML_API_SECRET` → `RUNWAY_API_KEY`** (`d37737c`) — owner's call: every other
  credential is `<PROVIDER>_API_KEY`, and house consistency across seven keys beats matching
  one vendor's spelling. No retirement entry owed; Runway never appeared in a release.

#### Fixed

- **`--provider <video>` with no model sent Veo's model id to the other provider**
  (`d37737c`) — a clap `default_value` made "unspecified" indistinguishable from "explicitly
  the Veo default". `ImageOptions::into_request` had already solved this for images and says
  so in a comment; video repeated the mistake anyway. Two tests pin it.
- **Kling answers 200 with a non-zero `code` for some refusals** (`d37737c`) — checking
  `is_success()` alone read a refusal as a submission and returned a task id that did not
  exist.
- **`lucida models` marked the default model per-provider** (`ef9fb14`, carried from v0.10.0
  work) — now generated for video providers too.

#### Deliberately not built

- **fal.ai.** Probed and declined 2026-08-09. It is the aggregator the product review named,
  and it fronts Kling, Veo and FLUX — all three of which Lucida now reaches *directly*, so it
  would add a worse second path to lanes we already own. Two operational facts recorded for
  whoever revisits this: fal authenticates with `Authorization: Key`, not `Bearer`; and it
  accepts a request into its queue **without validating**, failing at execution instead — so
  AGENTS.md §2's free-validation-error probe, which is how every provider here was
  characterised safely, does not work against it.

---

## v0.10.0 — released 2026-08-09


**Dependable unattended use** — the second tranche of the 2026-08-09 product review, and with
it the review's **third and last structural finding**, so all three are closed. The theme is
the one the review named: everything an agent needs in order to be left alone with this tool.
The MCP server answers while it renders and honours a cancellation; renders are remembered, so
one that has been paid for can be found again; cost is stated before it is spent and a budget
can refuse it; four outcomes are four exit codes, with `--json` for callers that parse; and a
weekly canary asks the providers whether they still speak the protocol, rather than waiting
for a user's failed render to find out.

Released from `main` with the verification trio green (201 tests, up from 163 at v0.9.2;
`cargo clippy --all-targets -- -D warnings` clean; all `scripts/smoke.sh` checks against the
release build) and CI green on all three platforms at `a8cb7b1`.

### 2026-08-09 — the product review's second tranche

Towards v0.10, "dependable unattended use". The review's **third and last structural finding
closes here**, so all three are now closed.

**Everything planned for v0.10 is now on `main`.** 201 tests, up from 163 at v0.9.2.

#### Added

- **`src/spend.rs` — cost visibility and `LUCIDA_BUDGET`** (`489dfcf`, `a6779a8`) — a price
  per provider and model, reported on every render (CLI *and* the MCP tool result, where the
  model that has to act on it can read it), recorded in the ledger, and enforced before a
  client exists. **The table is small on purpose**: only rates verified against a provider's
  published pricing appear as prices, each carrying the date checked; everything else is
  `Unverified`, a refusal to guess rather than an oversight, counted against a budget at a
  stated ceiling described as an assumed upper bound. The window is a **rolling 24 hours over
  the ledger**, not a session — a CLI invocation is one render, so a per-session cap guards
  nothing there, while an MCP server can run for a week and never reset.
- **`src/out.rs` — four exit codes and a global `--json`** (`804df50`) — 0 done, 1 failed,
  **2 refused**, 3 pending. 2 earns its own code because a refusal is an answer rather than a
  failure and retrying it cannot succeed; it is carried as an error type so it travels the
  existing `?` paths, tagged at `Capabilities::check` and `spend::check` as a whole rather
  than per `bail!`. `--json` emits one object on stdout **including on failure**, so a caller
  never switches parsers by outcome. `run` returns the exit code rather than `()`.
- **`scripts/canary.sh` and a `workflow_dispatch`-only workflow** (`ef9fb14`) — live drift
  detection, closing the limit the recorded-response tests state about themselves. Free by
  construction: model lists and balances, plus render requests naming a model that cannot
  exist. **A successful render inside the canary is reported as a failure.** Runs from a
  weekly ai-lab-0 cron (owner, 2026-08-09) so the five keys gain no second home.
- **`--count N` on `generate` and `edit`** (`a6779a8`) — N candidates as `name-1`, `name-2`;
  a single render keeps its given name. `--seed` with `--count` is refused, since together
  they render one picture N times and bill for each.
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
- **Three cost bugs, all found by running it rather than reading it** (`489dfcf`, `a6779a8`)
  — a **free** render was refused once the day's spend passed the cap, because `spent + 0.0
  <= budget` is false, which declined the very lane the refusal message points at; a batch
  called `check` once per image, so all N asked "can I afford one more?" and all N said yes
  (three images at $0.134 went through a $0.20 budget and rendered all three, about forty
  cents); and `price_for` matched the raw model string, so the documented alias `banana-pro`
  priced as `Unverified` and counted at the ceiling.
- **`lucida models` marked the default model per-provider** (`ef9fb14`) — only google and bfl
  ever got the branch, so openai and stability listed their default indistinguishably from
  everything else. Generated from `Backend::default_model()` now, with a test. Found by the
  canary, which was checking something else.
- **A ledger test wrote to the developer's real ledger** (`a6779a8`) — `record()` resolves the
  live config path, so calling it from a test appends junk to whichever machine runs the
  suite. It did.
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
