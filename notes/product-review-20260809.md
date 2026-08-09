# Product Review — 2026-08-09

**Reviewer:** Ziggy (agent session). **Scope:** the product, not the code line-by-line — where
Lucida should go, what it is missing, and what would make it more reliable. Based on a full
source read (v0.9.1, all 15 `src/*.rs`, workflows, installers), the docs
(README/ROADMAP/STATE/SKILL), and a market scan of the August-2026 provider landscape.
This is not an AGENTS.md §4 code review and does not move the review-cycle pointer.

---

## The verdict

The product thesis — *asset generation for coding agents, one static binary, five providers
behind one honest interface, nothing silently dropped* — is sound, differentiated, and mostly
delivered. The capability-truth-in-code architecture (`Capabilities::check` refusing before
money is spent, `MaskSupport` generating every surface, the skill that bans itself from
stating facts) is the right moat.

The market moved toward the niche and thereby crowded it: **every vendor now ships its own
MCP server** — fal's (Mar 2026) fronts 1,000+ models, Replicate's (Feb 2026) 50,000+, and
Comfy Org shipped an official ComfyUI MCP on 2026-06-30 that overlaps the local lane
directly. Raw model access is now a commodity. What none of them offer is Lucida's actual
position: multi-provider neutrality on the **user's own first-party keys**, a uniform
request model with honest refusals, an unmarked local lane, no markup, no middleman, one
binary. That is the position to defend.

But there is a gap between the promise and the implementation in exactly the places an
*agent* (the primary customer) hits them. The three findings below each contradict the
product's own stated design; fix them before adding anything.

## Three structural reliability gaps

### 1. The MCP server blocks its stdio loop for the whole render

`serve()` (`src/mcp.rs:55-98`) is a synchronous loop; `dispatch` runs to completion before
the next line is read. A ComfyUI render holds the loop for up to its 1800 s deadline
(`src/comfy.rs:626`). During that time a client `ping` goes unanswered (clients using ping
as a liveness probe conclude the server is dead), `notifications/cancelled` cannot be
honoured — there is no cancellation path at all, so a client's cancel is a no-op that keeps
billing — and a second tool call queues behind the first. For a tool whose headline use case
is "agents make their own assets," head-of-line blocking is the difference between a tool
agents use and a tool agents learn to avoid.

### 2. Silent parameter drops on the MCP surface

Every optional argument is read with a typed accessor that returns `None` on a JSON type
mismatch, and `None` means "not requested" (`src/mcp.rs:469-497`). Worst case:
`"reference_images": "photo.png"` (string, not array) — `.as_array()` yields `None` and
**the edit silently becomes a fresh generation**, reported as success. A stringified `seed`
is silently dropped (render unreproducible); same for `size`, `steps`, `guidance`,
`provider`. The care exists one line away (`steps` uses `u32::try_from` with an explicit
error and a test), so this is an oversight, not a philosophy — but it is the product's one
broken promise, on its most important surface.

### 3. A paid render can be lost irrecoverably from the CLI

`generate_video` (`src/video.rs:57-62`) never prints the operation id — the only path that
shows it is the 15-minute-deadline branch. One transient poll failure (a single 502, a
dropped connection, a laptop sleep, a Ctrl-C) aborts the wait, and the user has no id to
give `lucida check`. Minutes of billed Veo output, gone. There are **zero retries anywhere
in the codebase** — every `.send()` is a single attempt — so BFL and ComfyUI polls share the
fragility, and BFL's signed download URL (expires ~10 min) is not printed on failure either.
The MCP surface got this right (`start_video` returns the id immediately); the CLI never
did. Related: there is no `lucida video --no-wait`.

## Dated items

- **2026-08-17 (imminent):** Imagen's shutdown is referenced future-tense in six places
  (`src/genai.rs:20,81,203,208`, `src/config.rs:65`, `src/main.rs:806`). Defaults are safe
  (`gemini-3.1-flash-image` is current); the prose goes stale on the 17th.
- **2026-12-01:** OpenAI shuts down `gpt-image-1.5`, `gpt-image-1-mini`,
  `chatgpt-image-latest` — all three sit in `KNOWN_MODELS` (`src/openai.rs:92-97`) and are
  printed by `lucida models`. Default `gpt-image-2` is current.
- **Veo 3.1 changed the contract:** durations 4/6/8 s (ROADMAP's "is 8 s a hard limit?" is
  answered — no), resolutions 720p/1080p/4K (8 s-only at the higher tiers), three price
  tiers ($0.05–0.60/s), reference images and video *extension* on the standard tier. Lucida
  pins the right model ids but exposes none of the new knobs; `--duration` alone is a cheap
  real win. All Veo 3.1 ids are still `-preview` and can churn without a deprecation page.

**Standing fix, not reactive patching: a scheduled live canary.** A weekly CI cron running
the free-validation-error probes (AGENTS.md §2's 7-cents technique) against each provider,
failing on drift. It closes the stated limit of the recorded-response tests — "a recording
proves Lucida still speaks yesterday's protocol" — on a schedule instead of on a user's
failed render.

## What's missing — one feature closes four gaps

**A render ledger.** Nothing records what was generated: no prompt→file→seed→model→provider→
cost trail, no way to list in-flight video operations, no history. A small append-only
ledger (or per-image sidecar) gives, in one file: `lucida ops` (resumable operations without
hand-kept ids), render history, reproducibility (the reported seed is currently a stderr
note that scrolls away), and the substrate for cost tracking — which ROADMAP §3 admits is
entirely unbuilt.

**Cost visibility and a budget guard.** BFL echoes cost after submit; nothing estimates
before, nothing accumulates, nothing caps. For unattended agent use, a session spend cap
plus per-render cost in the tool result would let the skill's "decide parameters before
iterating" advice be enforced rather than hoped.

**Parity/ergonomics list**, in value order:

| Gap | Note |
|---|---|
| Capabilities need credentials in practice | `lucida models` and `image_providers` abort before printing the static capability table when `open()`/`list_models` fails (`src/main.rs:791-863`, `src/mcp.rs:568-574`) — contradicting `src/provider.rs:401-404` and the skill's "believe the probe." `capabilities_for` is pure; print it regardless. |
| No `connect_timeout` on any client | A blackholed host hangs for the full 300 s timeout; `image_providers` probes all five providers sequentially — up to ~20 min inside a tool advertised as "Cheap to call. Spends nothing." |
| Non-atomic image writes over the user's original | `lucida edit` defaults its output to the input path, then truncating-writes it (`src/main.rs:354`, `1070-1085`). Disk-full mid-write destroys the source. `config::write_replacing` is the house pattern; images never got it. |
| Batch/variants | No `-n`, no seed sweep; OpenAI hardcodes `"n": 1`. Agents want three candidates to pick from. |
| `--json` + exit codes | Everything exits 0/1; `lucida check` exits 0 with empty stdout while pending — scripts cannot tell pending from done from failed. |
| Provenance on CLI renders | MCP reports the marking on every render; the CLI only in `lucida models`. |
| ComfyUI upload collisions | Uploads use the source basename with `overwrite=true` — two concurrent Lucidas editing different `image.png`s silently swap pixels. A nonce fixes it. |
| `lucida setup` edge case | With the Claude app but no Claude Code CLI, setup names a skill file it never wrote (`src/setup.rs:139-141` vs `151-195`); `plan()` has no tests. |

## Where to go — ranked expansion axes

1. **Video depth (highest conviction).** Video is where the market moved and Lucida is
   thinnest — one provider, hardcoded to the genai client, no trait (`Backend` is
   image-only). Sora's API is being **removed 2026-09-24**, which clears the field; official
   metered APIs: Runway (self-serve, ~$0.05–0.12/s), Kling (official dev platform,
   per-second), Luma, MiniMax — all submit/poll/download, the shape built three times
   already. Extract a `VideoProvider` trait; add one non-Google video provider (the real
   substitution, as BFL was for images). Expose Veo 3.1 duration/resolution; consider
   extension. Watch FLUX 3 (2026-07-23 announcement: 20 s video with audio, gated now,
   open-weight Dev promised late 2026) — when it opens, the existing BFL integration
   becomes a video lane almost for free.
2. **Audio (cheapest expansion, completes the name).** The Gemini API already integrated now
   serves **Lyria 3** (`lyria-3-clip-preview` 30 s clips, `lyria-3-pro-preview` full songs)
   — same key, same client conventions. ElevenLabs has metered SFX (~$0.02/effect) and Music
   with API-level inpainting as the dedicated-provider option later. Suno/Udio remain
   closed, so the no-subscription rule excludes nothing wanted.
3. **Finish editing on providers already integrated.** Stability's edit endpoints
   (inpaint/erase/outpaint/remove-background/upscale) were deferred because `ImageRequest`
   couldn't express a mask — that premise died when OpenAI forced the mask field, so
   `stability.rs:105-110` is stale-by-design. BFL fill/expand (still FLUX.1-era endpoints)
   and klein LoRA finetuning are open surface. Upscale and background removal are the
   "asset finishing" verbs coding agents need.
4. **The aggregator question — no, or not yet.** One fal integration adds
   Kling/Wan/Seedance/Hailuo in a stroke, but cuts against everything differentiating:
   capabilities become unknowable per-model, provenance becomes undocumented, pricing gains
   a markup, and fal already has an official MCP. If demand appears, the precedent is
   `--workflow`: a deliberately fenced escape hatch whose reduced guarantees are stated.
5. **Vector assets — a real hole.** Icons and logos want SVG; no current provider emits it.
   Recraft has a metered API with native SVG output — the first provider that would be added
   for a *capability* rather than a model family. Onboarding rule applies (studied
   interface first).

## Positioning and hygiene

The GitHub repo description still reads "Generate and edit images with Google's Gemini
models" — four providers and all of video ago. It is the one capability surface no test
guards, and currently the only pitch a visitor sees (0 stars, 0 issues, 84 downloads at the
prune, a chunk of which are CI's own installer jobs). Decide whether public adoption is a
goal. If yes: fix the description, add topics, consider the crates.io publish once the
ledger/video-trait API settles (name still free; after v1.0). If no — legitimate; the tool
earns its keep internally — signing/SmartScreen and installer polish stay deprioritized.

Build hygiene for the next cycle: `rust-version` in Cargo.toml (edition 2024 needs ≥1.85;
older toolchains get a raw compile error via the cargo-reinstall update path), CI runs
without `--locked` despite a committed lockfile, `release.yml` does not gate on the test
suite (a tag ships having run only smoke), no `cargo audit`/`deny`, and the genai lane still
guesses MIME from file extension (`src/genai.rs:317-327`) after `sniff_mime` was built to
end exactly that.

## Suggested sequence

1. **v0.9.2 — keep the existing promises** (days): MCP type-mismatch refusals, print the
   Veo op id + retry transient polls, connect timeouts, atomic image writes, Imagen prose
   made date-proof, GitHub description.
2. **v0.10 — dependable unattended use** (weeks): MCP off-loop dispatch + cancellation,
   `lucida video --no-wait`, the render ledger + `lucida ops`, `--json` + exit codes,
   credential-free capability printing, live canary cron.
3. **v0.11+ — expand on conviction:** `VideoProvider` trait + Veo 3.1 knobs + one second
   video provider; Lyria audio; Stability edit endpoints. Then reassess Recraft/SVG and the
   crates.io/v1.0 moment.

The short version: the design philosophy is the product. The next release should make the
implementation as honest as the philosophy everywhere an agent touches it; the six months
after that are video, audio, and a ledger that makes every render accountable.

---

## Appendix — market facts the recommendations rest on (as of 2026-08-09)

Confidence: [official] = vendor docs fetched directly; [secondary] = consistent trade
press; [unverified] as marked.

- **Google:** `imagen-4.0-*` shut down 2026-08-17; replacement `gemini-3.1-flash-image`
  [official]. Current lineup: 3.1-flash-image (~$0.067/1K image), 3.1-flash-lite-image,
  3-pro-image (~$0.134), 2.5-flash-image legacy. Veo 3.0/2.0 past earliest-shutdown
  (2026-06-30); current `veo-3.1-{,fast-,lite-}generate-preview`, 4/6/8 s, 720p/1080p/4K,
  native audio, extension on standard; $0.05–0.60/s by tier [official].
- **OpenAI:** `gpt-image-2` current (2026-04-21); `gpt-image-1.5`/`-mini`/
  `chatgpt-image-latest` shut down 2026-12-01; dall-e-2/3 already dead (2026-05-12). Mask
  is prompt-based guidance, per docs, verbatim: "may not follow its exact shape with
  complete precision" [official]. **Sora Videos API removed 2026-09-24, no successor
  named** [official].
- **BFL:** FLUX.2 pro/flex/max/klein; fill/expand remain FLUX.1-era endpoints; FLUX.2 Erase
  exists; klein LoRA finetuning in public beta; **no negative prompt in FLUX.2 by design**
  (guidance-distilled) [official]. FLUX 3 announced 2026-07-23: image+video+audio, 20 s HD
  video with audio, gated early access; open-weight Dev "later in 2026" [official].
- **Stability:** API shape unchanged — generate (Core/Ultra/SD3.5), edit
  (erase/inpaint/outpaint/remove-bg/search-replace/recolor/relight), upscale
  (2/40/60 credits) [official]. "SD4 launched Apr 2026" claims are **unverified, likely
  false** — absent from Stability's own news feed. Company tilting enterprise/audio
  (Stable Audio 3.0, 2026-05-20).
- **Video APIs, official + metered:** Runway Dev (self-serve, credits at $0.01, Gen-4.5
  ~$0.12/s); Kling developer platform (Kling 3.0 ≈ $0.084–0.168/s, failed tasks free);
  Luma (Ray3.2 in API since 2026-06); Pika "API Club" (2026-08-04, $10/mo + wholesale,
  70+ models — new, unproven); MiniMax/Hailuo 2.3 (per-second); Alibaba Wan 2.5–2.7 via
  Model Studio (needs Alibaba Cloud account).
- **Aggregators:** fal.ai (600–1,000+ models, ~$400 M annualized, day-0 partner access,
  official MCP at mcp.fal.ai); Replicate (**acquired by Cloudflare**, closed early 2026;
  API unchanged so far; official MCP); Together AI (LLM-first, weakest fit). Aggregator
  provenance marking (SynthID/C2PA passthrough) is **undocumented — unknown**.
- **Competing tools:** official Comfy Org MCP (2026-06-30, Comfy Cloud-backed, "no local
  GPU required"); community ComfyUI MCPs; Google's official `nanobanana` Gemini CLI
  extension (no official Veo extension found); Hugging Face MCP (Spaces as tools); ImgMCP
  (own credit billing). No official OpenAI image MCP found.
- **Audio:** Gemini API now serves Lyria 3 (`lyria-3-clip-preview`, `lyria-3-pro-preview`)
  [official]; ElevenLabs SFX ~$0.0194/effect, Eleven Music v2 with API-level inpainting
  (Music API GA status partially conflicting — verify before committing); Stable Audio 3.0
  open-weight; **Suno and Udio have no official public API**.
