# Roadmap

Lucida currently speaks to exactly one provider: Google, for images via Gemini
and video via Veo. That was the right place to start and is the wrong place to
stop. This records where it goes next and, more usefully, what has to be true
first.

Nothing here is committed work. Items are ordered by what unblocks what, not by
enthusiasm.

---

## 1. The prerequisite: a provider abstraction

Every provider item below is blocked on the same piece of work, so it comes
first.

Today `genai::Client` is Google's REST client, and `main.rs`, `mcp.rs` and
`video.rs` call it directly. A second provider means introducing a trait —
roughly `generate(&ImageRequest) -> GeneratedImage`, plus a `capabilities()`
describing what the provider actually supports — and moving the Google
implementation behind it.

The hard part is not the trait. It is that **providers disagree about what an
image request even is**, and the current `ImageRequest` quietly encodes Google's
answer:

| | Google | Others |
|---|---|---|
| Aspect ratio | 10 named ratios | OpenAI takes explicit pixel sizes; ComfyUI takes width/height nodes |
| Resolution | `1K`/`2K`/`4K` | Mostly pixel dimensions |
| Editing | Reference images in the prompt | OpenAI uses an explicit mask; ComfyUI uses an inpainting graph |
| Output format | The model decides (usually JPEG) | Often selectable |
| Negative prompt | Video only, and not on every model | First-class on Flux and local models |
| Seed / determinism | Not exposed | Central to Flux and local workflows |

Three ways to resolve that, in rough order of preference:

1. **Normalize what genuinely maps.** Aspect ratio and resolution can be
   expressed as target dimensions and translated per provider. Most of the
   surface fits here.
2. **Declare capabilities and fail early.** `capabilities()` lets the CLI reject
   `--seed` against a provider that has no such concept, with a message naming
   one that does — the same shape as the existing `veo-lite` negative-prompt
   guard, which is the pattern worth copying.
3. **Allow a typed escape hatch** for genuinely provider-specific parameters,
   rather than pretending a union type is a common interface.

**The MCP schema is the constraint that matters most.** `generate_image` today
advertises Google's aspect-ratio enum and `1K`/`2K`/`4K` sizes. An agent reads
that schema and believes it. Whatever the abstraction becomes, the tool
description has to stay honest about what the *selected* provider can do, or the
model will confidently pass parameters that get silently dropped. Options are a
generic schema plus a capabilities probe, or regenerating the schema per
configured provider. This needs deciding before the second provider lands, not
after.

Provider selection should probably be inferred from the model id where possible
(the alias table already does this) with an explicit `--provider` override, and
a config file for keys once there is more than one to hold.

---

## 2. Providers

| Provider | Appeal | Main obstacle |
|---|---|---|
| **OpenAI** | Documented REST API, mask-based editing | None significant — the natural second provider |
| **Flux (Black Forest Labs)** | Hosted API *and* open weights; seeds and negative prompts | Weight licensing differs sharply per model |
| **Local (ComfyUI et al.)** | No per-image cost, no provenance marking, full control | Graph-based, not prompt-based; a different interface shape |
| **Adobe Firefly** | Licensed training data and enterprise indemnification | Entitlements and credential complexity |
| **Midjourney** | Distinctive aesthetic | No official general API; unofficial ones breach ToS |

### OpenAI

The most straightforward addition and the best first test of the abstraction,
precisely because it is *similar but not identical* — a shakedown that will
expose sloppy assumptions without fighting the design.

Notable differences to handle: explicit pixel sizes rather than named ratios,
mask-based inpainting rather than reference-image conditioning, and a quality
parameter with different semantics. Editing is the piece most likely to strain
the current `ImageRequest`, since `references: Vec<String>` cannot express "this
region of this image."

### Flux — Black Forest Labs

Interesting twice over: a hosted API, and open weights runnable locally, which
makes it the natural bridge to the local lane.

**Licensing needs checking per model, not per vendor.** The FLUX family has
shipped under materially different terms — some permissive, some
non-commercial-only. That distinction is exactly the "low fence" question the
studio already has a position on, and it applies to the *weights*, not the API.
Using the hosted API is a commercial-terms question; running the weights locally
is a licence question. Resolve both before this ships, and record the answer here
rather than in someone's memory.

Genuine capability gains: seeds (so a result is reproducible), real negative
prompts, and step/guidance control. These are the parameters the abstraction's
escape hatch exists for.

### Local — ComfyUI and friends

The most architecturally distinct, and the most strategically interesting.

The lab already runs ComfyUI on `ai-lab-0`, so this is integration rather than
provisioning. Attractions: no per-image cost, nothing leaves the network, no
SynthID or C2PA marking, and complete control over models and samplers.

The obstacle is real: ComfyUI's API takes a **workflow graph**, not a prompt. A
provider implementation would hold template workflows and substitute nodes —
prompt text, dimensions, seed, checkpoint — then poll for completion. That is
closer to the Veo start/poll pattern than to the image path, and the existing
long-running-operation handling is a reasonable model for it.

Worth deciding early whether Lucida ships opinionated default workflows or
requires the user to supply their own. Shipping defaults is friendlier and ages
badly; requiring them is honest and unwelcoming. A small set of maintained
templates plus a `--workflow` override is probably the compromise.

Also note the ROCm caveats already documented for that machine — a local provider
inherits its host's quirks, and "it hangs" will be the first bug report.

### Adobe Firefly

The distinguishing feature is provenance and indemnification: trained on licensed
content, with enterprise assurances about commercial use. For a studio publishing
assets under its own name, that is a substantive difference from every other
option here, not a marketing line.

The cost is access complexity — Firefly Services sits behind Adobe's
authentication and entitlement model, which is heavier than an API key in an
environment variable. Worth confirming what a single-developer account can
actually reach before designing anything.

### Midjourney

Aesthetically the most distinctive and practically the most difficult. There has
been no general-availability public API; access has historically run through
Discord, and the third-party "APIs" that appear in search results work by
automating accounts in ways that violate the terms of service.

**Not worth implementing against an unofficial interface.** It would break
without warning, could get an account banned, and would make Lucida complicit in
a ToS breach. Revisit only if an official API becomes generally available. Until
then this entry exists to record the decision, so it does not get re-litigated
every few months.

---

## 3. Cross-cutting consequences

Adding providers changes more than the request path.

**Provenance stops being uniform.** Every Google image carries SynthID and a C2PA
manifest. Locally generated images carry nothing. Others differ. The README makes
a flat claim today that would become false — it needs to become per-provider, and
Lucida should probably be able to report what marking an output carries rather
than leaving users to grep the bytes as we did.

**Cost stops being uniform.** Per-image pricing varies by an order of magnitude
and local generation is free at the margin. Anything that estimates or warns
about spend has to be provider-aware.

**Failure modes multiply.** The troubleshooting section is currently a catalogue
of Google's specific behaviours — free-tier quota reading as throttling,
`IMAGE_RECITATION` punishing simple prompts. Each provider brings its own, and
each is discovered the same way: by hitting it. Expect the section to grow faster
than the feature list.

**Testing gets harder.** Everything shipped so far was verified against the live
API, which costs money and requires credentials. A provider matrix multiplies
that. Some form of recorded-response testing is probably needed before the third
provider, or coverage will quietly become aspirational.

---

## 4. Independent of providers

- **Code signing.** macOS binaries are unsigned, so first run needs the
  quarantine attribute cleared; Windows shows a SmartScreen warning. An Apple
  Developer account is available; this is scheduling, not blocking.
- **crates.io.** Deliberately not published yet. `cargo install --git` works and
  the name is still free. Worth doing once the API surface settles — a published
  crate name is harder to walk back than a repo rename.
- **Video beyond Veo.** Runway, Pika, Kling and Sora occupy the same space. The
  start/poll abstraction already exists and should generalize, but this trails
  images.
- **Untested surface.** `--resolution` and image-to-video are verified;
  `negative_prompt` is verified on fast and standard but the *effect* was never
  A/B'd, only its acceptance. Worth an actual comparison.
- **Clip length.** Veo returns 8 seconds with no parameter to change it. Confirm
  whether that is a hard limit or an undocumented one.
