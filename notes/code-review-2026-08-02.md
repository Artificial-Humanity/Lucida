# Full code review — 2026-08-02

**Scope.** Every line of `src/` (11 modules, ~6,600 lines including tests),
`scripts/smoke.sh`, `Cargo.toml`, `README.md`, and `ROADMAP.md`. Read in full,
not sampled. Every finding below was verified against the built binary before
being claimed — the review that produced this file also produced two commits,
and the tree state it describes is:

| Commit | Subject |
|---|---|
| `9608758` | Let ComfyUI render a workflow of your own, without letting options go silent *(pre-review)* |
| `df92572` | Record the wire, so verifying five providers stops costing money |
| `d55492b` | Review sweep: the fifth provider was missing from every hand-written list |

**State at close:** 89 tests passing, `cargo clippy --all-targets` clean,
`scripts/smoke.sh` all 21 checks passing, release build warning-free.

---

## 1. The headline finding

**Everything generated stayed true; everything hand-written rotted.**

When the fifth provider (openai) landed, no generated artifact missed it: the
`provider` enum in the MCP schema, `provider_summary()`, `providers_where()`,
the capabilities probe. But *every* hand-maintained list silently stayed at
three or four. This is not one bug — it is one failure mode surfacing in seven
places, and the codebase's own doc comments predicted it (`mcp.rs` opens with
the story of the hand-written schema that claimed "Two providers" while
listing three). The systemic fix applied in `d55492b` was, wherever the shape
allowed, to replace the hand-written text with text computed from
`Backend::ALL` — so a sixth provider *cannot* be missing.

The instances, each confirmed against the live binary before fixing:

### 1.1 `mcp.rs` — the schema said "Four providers are available"

The tool description's opening count was a hand-written word directly above a
generated provider list that contained five entries. An agent reads the count
and believes it. **Fix:** the count is now `Backend::ALL.len()`, formatted
into the description. Verified via `tools/list` against the built binary:
"5 providers are available…".

### 1.2 `mcp.rs` — the `model` parameter's defaults omitted openai

"Defaults to … on google, `klein` on comfyui, flux-2-pro on bfl, core on
stability." — nothing for openai. **Fix:** generated from
`Backend::ALL` × `Backend::default_model()`, producing
`google → gemini-3.1-flash-image, comfyui → klein, bfl → flux-2-pro,
stability → core, openai → gpt-image-2`.

### 1.3 `mcp.rs` — `aspect_ratio` and `size` descriptions omitted openai entirely

The aspect description named google's list, stability's list, and "comfyui and
bfl accept any ratio" — openai unmentioned, despite having the most unusual
geometry model of all (gpt-image-2 free on a 16-grid; its siblings exactly
three fixed sizes). The `size` description likewise. The `seed` description
said only "google exposes none" when openai also has no seed. **Fix:** all
three descriptions now state the openai behaviour.

### 1.4 `provider.rs` — `Backend::parse` error named four providers

`unknown provider `x`. Known providers: google, comfyui, bfl, stability` —
openai parseable but unlisted. **Fix:** the list is now generated from
`Backend::ALL`. Verified: the error now names all five.

### 1.5 `provider.rs` — the reference-image refusal said "Both providers"

`Capabilities::check`'s message for a provider that cannot edit still read
"Both providers currently support editing … use `google` or `comfyui`" —
written when there were two providers, wrong since the third. Reachable in
practice: stability cannot edit, and `bfl` with a non-FLUX.2 model cannot.
**Fix:** the list of editors is computed from
`capabilities_for(backend, default_model)` at refusal time, same pattern as
`mcp.rs`'s `providers_where`.

### 1.6 `main.rs` — CLI help text stuck at three providers

Five separate strings:

- module doc: "one of three providers"
- `about` / `long_about`: named Google, ComfyUI, FLUX only; no
  `STABILITY_API_KEY` / `OPENAI_API_KEY` mention
- `--provider` help: "google, comfyui or bfl"
- `models --provider` help: same three
- `--negative` help: "(comfyui only — no FLUX or Gemini model takes one)" —
  **wrong**, stability honours a negative prompt (verified by rendering, per
  `stability.rs`'s own capability comments)
- `--seed` help: "(comfyui and bfl; google has none)" — stability has a seed;
  openai does not and was unmentioned

**Fix:** all updated to the five-provider reality. These could not be
generated (clap attribute strings must be literals), so they remain a known
drift surface — see §5.1.

### 1.7 `scripts/smoke.sh` — the provider-name loop checked four

```sh
for provider in google comfyui bfl stability; do   # openai missing
```

This is the sharpest instance, because the comment directly above it says the
check exists so that "a new provider must not be missing from" the schema —
and the check itself was missing the new provider. The check designed to catch
the drift had drifted. **Fix:** `openai` added. The deeper lesson is recorded
in §5.1: a checklist that must be updated by hand is itself a hand-written
list.

---

## 2. Other defects found and fixed (`d55492b`)

### 2.1 `provider.rs` — `--model core` was inferred as Google and 404'd there

`infer_backend` consulted `MODEL_ALIASES` for all providers but
`KNOWN_MODELS` only for bfl and openai. Stability's endpoints (`core`,
`ultra`, `sd3`) are its model ids; `ultra` and `sd3` happened to also be alias
keys, `core` did not. So `lucida generate x --model core` fell through to the
Google default and sent "core" to Gemini — a confusing 404 against the wrong
provider. **Fix:** `stability::KNOWN_MODELS` is now consulted, with a
regression test (`backends_are_inferred_from_the_model_id` asserts `core` →
Stability). Note the behavioural consequence, accepted deliberately: `--model
core` now routes to a **paid** API instead of erroring — which is what asking
for `core` means.

### 2.2 `genai.rs` — a user-facing message named the binary's old name

The 404 branch of `explain_error` said "Run `mediagen models`…". `mediagen`
was the project's pre-rename name; a user pasting that command gets
"command not found". **Fix:** `lucida models`. (Found by grepping for the old
name across the tree; this was the only survivor.)

### 2.3 `mcp.rs` — unknown methods answered `-32603` instead of `-32601`

All dispatch errors were reported as `-32603` (internal error). JSON-RPC
reserves `-32601` for *method not found*, and MCP clients probe optional
surface (`resources/list`, `prompts/list`) expecting exactly that code — a
client is entitled to treat `-32603` on a probe as a server fault. **Fix:**
errors whose message begins `unknown method` map to `-32601`; everything else
remains `-32603`. Verified: `resources/list` now answers
`{"error":{"code":-32601,…}}`.

### 2.4 `mcp.rs` — the capabilities probe omitted three capabilities

`describe_providers()` (the `image_providers` tool) reported aspect, seed,
negative prompt, references, steps, guidance, provenance — but not `size`,
`mask`, or `workflow`. The generate_image schema explicitly sends agents to
this probe to check mask support, and the probe never mentioned masks.
**Fix:** all three added; mask reports `accepted (advisory)` to match the
`lucida models` wording, keeping the measured caveat attached to the claim.

### 2.5 `mcp.rs` — `generate_image` duplicated `Backend::default_model`

A hand-written five-arm `match` mapping backends to default models, identical
in intent to `Backend::default_model()`, which already exists and is already
tested. Two copies of a mapping is a drift bomb. **Fix:** the match is now a
call.

### 2.6 `main.rs` — the `video` subcommand skipped extension correction

`lucida check` corrects the output extension against the actual MIME type;
`lucida video` did not, so `lucida video -o clip.avi` wrote MP4 bytes into a
file named `.avi`. It also printed size as integer MB
(`bytes.len() / 1_048_576`), so any clip under a megabyte reported "0 MB".
**Fix:** the video path now applies `correct_extension(&out, "video/mp4")`
and prints `{:.1} MB`, matching `check`.

### 2.7 Clippy — five `collapsible_if` warnings

Four pre-existing (`bfl.rs` credits, `mcp.rs` commentary, `main.rs` commentary
and `write_image` parent-dir) plus one in the new `testserver.rs`. All
rewritten as let-chains (edition 2024). `cargo clippy --all-targets` is now
clean, which matters mostly so that the *next* warning is visible.

---

## 3. Open findings — need an owner decision or a live probe

Ordered by how much they matter. None were fixed, each for a stated reason.

### 3.1 `comfy.rs` — `--workflow` silently ignores an explicit `--model`

```
lucida generate "x" --provider comfyui --workflow f.json --model klein
```

The model is dropped without a word — a supplied workflow names its own
checkpoints, so `generate` builds an empty `Checkpoint` and never reads
`req.model`. That is the *silent-drop sin*, arriving through the door built to
let callers past the defaults, and it is the same shape of failure `--workflow`
itself was designed to refuse (`a graph with no %seed% cannot honour --seed`).

**Why not fixed here:** `main.rs::into_request` fills in the provider default
before the request reaches the provider, so by the time `comfy::generate` runs
it cannot distinguish "the user typed `--model klein`" from "klein is the
default". Refusing the combination properly means carrying
`Option<String>` for the model deeper into `ImageRequest`, or refusing in
`into_request`/`mcp::generate_image` where explicitness is still visible.
That is a plumbing decision worth making deliberately, not in a review sweep.

**Recommendation:** refuse `--workflow` + explicit `--model` at the two entry
points (CLI and MCP), where `self.model: Option<String>` still exists. Small,
honest, and consistent with the token-refusal design. The same reasoning
applies to `--ref` + `--workflow`, which *is* already refused — in
`comfy::generate` — so the refusal for model should cite that precedent.

### 3.2 `openai.rs` — edits never send `output_format`

`generate_fresh` requests `"output_format": "png"` explicitly; `edit()` sends
no such field yet the result is still reported as `image/png`. If the edits
endpoint's default is ever not PNG (or differs per model), the extension and
MIME reported to the user are wrong.

**Why not fixed here:** OpenAI **rejects** unknown parameters (measured — it
is the reassuring property the module header records). Adding `output_format`
to the multipart form without probing risks breaking every edit if the edits
endpoint happens not to accept it. This needs one live request against
`/images/edits`, which costs money and was out of scope for a review pass.

**Recommendation:** next paid session, probe once; then either send the field
or document that edits are PNG by default with a source.

### 3.3 `stability.rs` — the sd3 variants are unreachable

`SD3_VARIANTS` lists four (`sd3.5-large`, `-large-turbo`, `-medium`,
`-flash`), but `generate` hardcodes `SD3_VARIANTS[0]`. There is no spelling
that reaches the others: `--model sd3.5-flash` is not an alias, so it passes
through as a URL path → `/v2beta/stable-image/generate/sd3.5-flash` → 404.
The cheapest and fastest variants are advertised in the source and
unreachable from the CLI.

**Recommendation:** accept `sd3.5-*` as model ids that resolve to endpoint
`sd3` + `model` field. Small change; needs a naming decision (does
`lucida models --provider stability` then list 3 endpoints or 6 spellings?).

### 3.4 `stability.rs` — the seed the API chose may be recoverable

The code comments say "the API does not report the seed it chose", and
`GeneratedImage.seed` echoes only a requested seed. Stability's v2beta
responses reportedly carry a `seed` **response header**. If true, an unpinned
render becomes reproducible — the exact property the seed field exists for.

**Status: unverified either way.** The original probing sessions did not look
at response headers. One free… no — one *cheap* render with header capture
settles it. If the header exists, read it and report it; if not, add a
comment saying headers were checked, so the question stays answered.

### 3.5 `masked.rs` — non-ASCII input is mangled

`read_masked` reads bytes and pushes `byte as char` — a Latin-1
reinterpretation. Pasting a value containing `é` (UTF-8 `0xC3 0xA9`) stores
`Ã©`. Related: two asterisks are printed for that character (one per byte),
but backspace pops one `char` and erases one asterisk, desynchronising the
display. API keys are ASCII in practice, so this is low priority — but the
prompt is generic (`Value for {name}:`) and will eventually be reused for
something that isn't a key.

**Recommendation:** accumulate bytes in a `Vec<u8>`, validate UTF-8 at the
end; print one asterisk per *character* by only printing on UTF-8 boundary
bytes. ~15 lines. Not urgent.

### 3.6 `openai.rs` — `implied_aspect` guesses MIME from the extension

A reference whose name doesn't end `.png` is assumed JPEG. A `.webp` source
therefore fails dimension-reading, silently falls back to `None`, and the
edit is sent `auto` — precisely the reshaping behaviour `implied_aspect`
exists to prevent, restored for one file type. `image_dimensions` in
`main.rs` supports PNG and JPEG only, so the fix has two halves (sniff the
magic bytes instead of the name; optionally add WebP's `VP8X`/`VP8 `/`VP8L`
header parsing).

### 3.7 Minor, recorded for completeness

- **`bfl.rs`**: poll-loop errors call `explain_error(status, body,
  "get_result")`, so a 404 mid-poll would say ``no such endpoint as
  `get_result` `` and suggest `lucida models` — misleading advice for an
  expired job. The `Task not found` status branch usually catches this first,
  which is why it has never been seen.
- **`resolve_model` casing** is inconsistent across providers: stability and
  openai lowercase unknown ids on passthrough; bfl and genai preserve them.
  Harmless today (all live ids are lowercase); worth unifying whenever one of
  those files is next open.
- **`mcp.rs`**: `steps` arrives as `u64` and is cast `as u32` — a
  pathological value wraps silently instead of erroring. The provider would
  reject the wrapped value anyway.
- **`provider.rs`**: `Size::parse` accepts 16–16384, and openai's
  `area_dimensions` squares a `--size 4K` scale into a ~16 MP budget that the
  API will likely refuse. Capability-true (`size: true` for gpt-image-2) but
  the practical ceiling is unmeasured.

---

## 4. Recorded-response testing (`df92572`) — what was built and why it holds

The last of the four agreed roadmap items, and the review's precondition: it
is what made several findings *checkable* rather than anecdotal.

### 4.1 Design

`src/testserver.rs`, compiled only under `cfg(test)`, no new dependency
(~250 lines of `TcpListener`, same reasoning that kept a JSON-RPC crate out
of `mcp.rs`):

- **Responses are scripted, not simulated.** A test passes replies transcribed
  from real provider sessions; the server plays them back in order. No
  routing, no cleverness — request-order changes surface as assertion
  failures on the recorded requests.
- **Requests are recorded whole**: method, path+query, headers
  (case-insensitive lookup), body bytes (sized *and* chunked
  transfer-encoding both handled, since which one a multipart form gets is a
  reqwest implementation detail).
- **`{{server}}` substitution** in reply bodies lets a recording hand back
  polling/download URLs that point at the test server — which is how "the
  client follows the URL the API returned, verbatim" became testable.
- **Deadline on accept** (15 s, non-blocking poll): a test whose code makes
  fewer requests than scripted fails in seconds instead of hanging the suite.

Each provider client grew a `base: String` field (`API_ROOT` in production);
tests construct clients directly — possible because the tests live in each
module's own `mod tests`, so no test-only public surface was added except
`genai::Client::recorded` (`pub(crate)`, `cfg(test)`), which `video.rs`
needs because it shares genai's client and cannot reach its fields.

### 4.2 What the recordings pin

The highest-value assertions are the **deliberate asymmetries** that
documentation flattens and that were previously guarded by nothing but
memory:

| Claim | Test |
|---|---|
| BFL's signed download URL carries **no** credential; submit/poll carry `x-key` | `bfl::the_signed_download_url_never_receives_the_api_key` |
| BFL polls the URL the API returned **verbatim** (regional delegation) | same test |
| Veo's download URL **requires** the key — the opposite of BFL, both measured | `video::the_video_download_carries_the_key_because_veo_urls_require_it` |
| ComfyUI credentials ride on **every** request including `/view` — the round trip that used to be the one to fail behind a proxy | `comfy::a_render_carries_credentials_on_every_request_including_the_download` |
| Google's key travels as `x-goog-api-key`, never `?key=` in the URL | `genai::the_key_travels_as_a_header_and_never_in_the_url` |
| An OpenAI edit sends its source's implied size, never `auto` (the mini-reshaped-a-square regression) | `openai::an_edit_is_multipart_with_the_sources_implied_size` |
| OpenAI's URL-response fallback downloads without the key | `openai::a_url_response_is_downloaded_without_the_key` |
| Stability is one multipart round trip, every field present **by name** — necessary because that API silently ignores unknown fields | `stability::the_render_is_one_multipart_request_returning_raw_bytes` |
| `sd3` names its variant on the wire | `stability::the_sd3_endpoint_names_its_variant` |
| ComfyUI uploads land as `subfolder/name` in `LoadImage` | `comfy::an_uploaded_reference_is_named_with_its_subfolder` |
| A custom `--workflow` is submitted as substituted, with **zero** `/object_info` calls (works on installs without Flux.2 files) | `comfy::a_custom_workflow_is_submitted_without_resolving_models` |
| Both BFL moderation states are terminal; no further polling | `bfl::a_moderated_prompt_stops_the_poll_with_a_clear_verdict` |
| Veo's lite negative-prompt guard runs **before** any HTTP (aimed at a dead port, still answers correctly) | `video::lite_rejects_a_negative_prompt_before_spending_a_round_trip` |

18 new tests total (71 → 89), 15 of them wire-backed. The custom-workflow
render path is now exercised end to end without a GPU — the live render
through a custom graph is still owed once VRAM frees up, as is re-testing
`gpt-image-1` (allowlisted on project `proj_NnBcP5SB8RSD4vUUnH9DIka6` yet
still 403; its three siblings work).

### 4.3 The stated limit

A recording proves Lucida still speaks **yesterday's** protocol, not that the
provider still does. Live verification is owed once per new provider or
changed endpoint; the recordings reduce it from *every change* to *once*.
This is written into ROADMAP §3 so nobody mistakes green tests for live
compatibility.

### 4.4 A latent race, fixed in passing

`comfy.rs`'s `workflow_file` test helper keyed its temp directory on
**content length**, so all tests sharing the `MINIMAL` fixture shared one
directory — and each deletes it on exit while siblings may still be reading.
Pre-existing with two tests, first *observed* when a third joined (one
intermittent failure, then unreproducible — the classic signature). Now keyed
on `(pid, atomic counter)`.

---

## 5. Standing observations

### 5.1 Hand-written provider lists that remain, knowingly

These could not be generated and will rot again when provider six lands.
Recorded here so the next provider's checklist starts from it:

- `main.rs` clap help strings (attribute literals): `--provider`,
  `--negative`, `--seed`, `--steps`, `--guidance`, `--mask`, `--workflow`,
  `about`/`long_about`, `models --provider`.
- `mcp.rs` parameter descriptions for `aspect_ratio` / `size` /
  `negative_prompt` / `seed` / `steps` / `guidance` (partly interpolated, but
  the prose naming providers is manual).
- `README.md` prose and tables.
- `provider.rs` `Capabilities::check` remedy texts (the negative-prompt
  remedy match, `no_sampler`).
- `scripts/smoke.sh` — now correct, but the loop is still a hand-typed list.
  A future option: have the binary print `Backend::ALL` (e.g. `lucida models
  --list-providers`) and iterate over that, so the smoke test cannot drift.

### 5.2 What was checked and found sound

Worth recording so future reviews need not re-derive it:

- **`config.rs`**: parser handles `export`, quotes, `=` in values, comments;
  empty env var counts as unset (deliberate); `OnceLock` caching is safe
  because `config --set` exits immediately after writing; 0o600 enforced on
  create, warning (not refusal) on loose modes — with rationale.
- **`masked.rs`**: the `Restore` drop-guard + `ISIG`-off design is correct;
  Ctrl-C arrives as `0x03`, restores, exits 130. The non-ASCII issue (§3.5)
  is the only defect.
- **`comfy.rs`**: `split_credentials` handles `@` in paths and passwords
  containing `@` (rsplit); `auth_header`'s three-form parse checks
  scheme-shaped input before the colon rule (Basic payloads contain colons);
  `escape_json` covers quotes, backslashes, and control chars; `urlencode`
  correct for the `/view` query; `find_image` searches by shape so
  `--workflow` graphs with arbitrary save nodes still resolve.
- **`provider.rs`**: `pixels()` places the named size on the long edge and
  rounds to the latent grid; `round_to` is a correct round-half-up;
  `infer_backend` ordering (aliases → known models → file extension →
  `flux-` prefix → Google) resolves the local/hosted flux collision, with
  tests.
- **`main.rs`**: `image_dimensions` PNG offset math and the JPEG SOFn marker
  walk (excluding DHT/JPG/DAC) are correct; `correct_extension` treats
  jpg/jpeg as one; `write_image` canonicalises and strips the Windows `\\?\`
  prefix.
- **`mcp.rs`**: notifications (no `id`) draw no reply; nothing but responses
  reaches stdout; errors return as `isError` tool *content* so the model can
  read and retry — all covered by smoke checks.
- **Credential hygiene** end to end: keys never in URLs (Google), never on
  signed downloads (BFL, OpenAI), stripped from printable base URLs
  (ComfyUI), never echoed by `config`, never taken as CLI arguments. Now all
  wire-tested, not just intended.

### 5.3 Release state

v0.5.1 is the latest tag (three platform assets; v0.5.0 shipped without the
Windows asset and can be deleted). The two review commits are pushed but
untagged — nothing in them changes behaviour a release *must* carry, but the
schema/message corrections are user-visible; a v0.5.2 is one command away
when wanted.

---

## 6. Suggested order of follow-up

1. Refuse `--workflow` + explicit `--model` at the CLI/MCP entry points
   (§3.1) — closes the last silent drop.
2. Live probes, batched into one paid session: OpenAI edits `output_format`
   (§3.2), Stability `seed` response header (§3.4), gpt-image-1 entitlement
   retry, one live custom-workflow render on ComfyUI when the GPU frees.
3. sd3 variant spelling (§3.3) — decide, then it is a small change.
4. `masked.rs` UTF-8 (§3.5) and `implied_aspect` MIME sniffing (§3.6) as
   opportunistic fixes next time those files are open.

---

## 7. Follow-up applied — 2026-08-02, same day

Everything code alone could close, is closed. 98 tests (89 → 98), clippy
clean, all smoke checks passing, refusal verified against the release binary.

- **§3.1 CLOSED** — `--workflow` + explicit `--model` refused at both entry
  points (`into_request`, `mcp::generate_image`), where `Option<String>`
  still distinguishes typed from defaulted. The MCP `workflow` description
  now states the rule. Tests on both paths.
- **§3.3 CLOSED** — the four `sd3.5-*` spellings are model ids resolving to
  endpoint `sd3` + `model` field; `sd3` alone still means `sd3.5-large`;
  `infer_backend` routes them. Naming decision taken: `lucida models
  --provider stability` lists 3 endpoints **and** 4 variant spellings —
  discoverability was the point. Wire test pins path + field.
- **§3.5 CLOSED** — `read_masked` accumulates bytes, validates UTF-8 once at
  the end, prints one asterisk per character (lead bytes only), and
  backspace pops a whole character. Helpers unit-tested.
- **§3.6 CLOSED** — `sniff_mime` (magic bytes: PNG/JPEG/WebP) replaces the
  extension guess in `implied_aspect`, and `image_dimensions` learned all
  three WebP layouts (VP8X / VP8 / VP8L). A PNG named `.webp` and a real
  WebP both keep their implied size — tested.
- **§3.7 partly closed** — bfl poll-loop 404 now says the job expired
  instead of suggesting `lucida models`; MCP `steps` uses `try_from` and
  errors instead of wrapping. **Left open, deliberately:** `resolve_model`
  casing (either direction of unification changes live passthrough
  behaviour; still harmless today) and the openai `--size 4K` area ceiling
  (unmeasured — belongs in the paid-probe batch).
- **§3.2, §3.4, gpt-image-1 retry, live workflow render — still owed.**
  They spend money or need the GPU; batch per §6 item 2.

---

## 8. The paid probes and the live render — 2026-08-02, later the same day

Owner freed the GPU and authorized modest spend. All four owed items ran;
every one came back positive. Total spend: two Stability core renders
(~$0.06) and one low-quality gpt-image-1 render (~$0.01) — the OpenAI
`output_format` answer cost nothing (see below).

- **§3.2 CLOSED** — `/images/edits` **does** accept `output_format`, and the
  probe was free: sending `output_format=bogus-value` drew
  `Invalid value: … Supported values are: 'png', 'webp', and 'jpeg'` — a
  validation error that both names the parameter and lists its values,
  without rendering or billing. `edit()` now sends `output_format: png`,
  matching `generate_fresh`, and the edit wire test asserts it.
- **§3.4 CLOSED, and the folklore was wrong** — Stability's success response
  carries `seed: <n>` (and `finish-reason: SUCCESS`) as **response
  headers**. Measured twice: an unpinned core render reported
  `seed: 742048682`, and re-rendering pinned to that value produced
  byte-identical IDAT (pixels; whole files differ only by the C2PA
  instance id, as §"measured rather than read" predicted). `generate()`
  now reads the header and reports it — an unpinned Stability render is
  reproducible after all. `testserver::Reply` grew `with_header` to record
  it.
- **gpt-image-1 CLOSED** — the entitlement has propagated;
  a low-quality generation succeeded. All five `KNOWN_MODELS` are live.
  No code change needed; it was already listed.
- **Live custom-workflow render CLOSED** — a hand-written klein graph
  (renamed save node `keep`, all seven tokens) rendered 512x512 at seed 42
  in 78 s through the release binary. Tokens substituted, zero
  `/object_info` resolution, image found by shape. The recorded-response
  test's claims all held live. ROADMAP updated.

### 8.1 `resolve_model` casing — settled, and the probe was free

The direction was ambiguous only because nobody had asked the APIs. All
four hosted providers reject an uppercase model id, and each rejection
costs nothing because it renders nothing:

| Provider | `--model` as typed | Answer |
|---|---|---|
| stability | `/generate/CORE` | `404 Not Found` (control: `core` reaches parameter validation) |
| bfl | `/v1/FLUX-2-PRO` | `{"detail":"Not Found"}` |
| openai | `GPT-IMAGE-2` | `The model 'GPT-IMAGE-2' does not exist.` |
| google | `GEMINI-3.1-FLASH-IMAGE` | `unexpected model name format` |

No live id on any of them contains uppercase, so lowercasing cannot
mangle a real one and turns `--model CORE` from an unreadable 404 into a
render. `genai.rs` and `bfl.rs` now match `stability.rs`/`openai.rs`;
a test in `provider.rs` pins all four.

**ComfyUI is deliberately excluded, and it is the reason this is not a
blanket rule.** Its ids are checkpoint *filenames* on a case-sensitive
filesystem, so `comfy::resolve` lowercases only for the alias lookup and
pins `model.trim()` — original spelling — for the file, comparing it
exactly against what the server reports. Lowercasing there would break
`--model MyCheckpoint.safetensors`. Different question, different answer.

Remaining open: the openai `--size 4K` practical area ceiling — measuring
it means paying for a deliberately over-budget render, so it waits until
someone actually asks for 4K there.

### 8.2 Shipped

**v0.5.2**, tagged and published the same day, superseding §5.3's "one
command away". All three platform assets built and attached with
checksums, so v0.5.0's missing-Windows-asset failure did not recur.
v0.5.0 itself is still present and still incomplete; deleting it remains
the owner's call.

Total spend for the whole probe batch: **about 7 cents** — two Stability
core renders and one low-quality gpt-image-1 render. Every other question
was answered by a validation error, which renders nothing and bills
nothing. That is the technique worth carrying forward: ask the API in the
way that costs nothing before paying it.
