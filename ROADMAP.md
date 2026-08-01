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
| **Local (ComfyUI et al.)** | Already installed and working; free, unmarked, no credential | Graph-based, not prompt-based; a different interface shape |
| **Flux (Black Forest Labs)** | Nearest substitute on quality and cost; same models, hosted | Weight licensing differs sharply per model |
| **OpenAI** | Documented REST API, mask-based editing | None significant, but a one-off — shares little with the others |
| **Adobe Firefly** | Licensed training data and enterprise indemnification | Entitlements and credential complexity |
| **Midjourney** | Distinctive aesthetic | No official general API; unofficial ones breach ToS |

### Local — ComfyUI and friends

**Start here.** Not because it is the tidiest entry point — it is the least
tidy — but because it is already on the machine, already working, and costs
nothing to iterate against.

`ai-lab-0` carries a complete **FLUX.2 Klein 9B** stack under
`/data/services/comfyui/models`: the 17 GB diffusion model, its 16 GB Qwen3 text
encoder, and the VAE. A 1024×1024 render succeeded on the gfx1151 on 2026-07-19.
The riskiest unknown for this lane — whether Flux runs at all on this hardware,
given the ROCm history — is therefore already answered. It does.

That removes every barrier that usually delays a second provider. No credential
to obtain, no billing to enable, no per-image cost while the abstraction is being
reshaped daily, and no rate limit to design around. The work can start tonight
and be thrown away twice without anyone caring.

**The apparent objection is actually the main argument.** ComfyUI is the least
representative provider here — a workflow graph rather than a prompt call — so
designing the trait against it first looks like a way to end up with a
graph-shaped abstraction. But Google is *already implemented*. Building the trait
against Google plus ComfyUI means designing against the two most dissimilar
providers on the list, which is exactly the pair most likely to produce an
abstraction that survives the rest. Google plus OpenAI would be two variations on
one shape, and would flatter a design that had not been tested.

**Confirmed: local output carries no provenance marking.** The existing Klein
render contains no SynthID string and no C2PA manifest, checked the same way we
checked Google's. This is the one lane producing genuinely unmarked images — a
real difference rather than a claimed one, and the reason the README's current
flat statement about watermarking will need qualifying.

The obstacle is real: ComfyUI's API takes a **workflow graph**, not a prompt. A
provider implementation would hold template workflows and substitute nodes —
prompt text, dimensions, seed, checkpoint — then poll for completion. That is
closer to the Veo start/poll pattern than to the image path, and the existing
long-running-operation handling is a reasonable model for it.

The template question is smaller than it looks, because **ComfyUI already ships
them**. `ComfyUI/blueprints/` currently holds six Flux workflows:

```
Text to Image     (Flux.1 Dev / Flux.1 Krea Dev / Flux.2 Dev)
Image Edit        (Flux.2 Dev / Flux.2 Klein 4B)
Image Inpainting  (Flux.1 Fill Dev)
```

The graphs do not need authoring, only adapting — note the Klein blueprint
targets the 4B while the installed model is the 9B, so node parameters need
reconciling rather than copying. Still far less work than starting from a blank
graph.

Worth noticing what the inpainting blueprint implies: **mask-based editing is
reachable through the local lane**, not only through OpenAI. The structural gap
in `references: Vec<String>` — which cannot express "this region of this image" —
can therefore be discovered without adding a provider purely to find it.

Worth deciding early whether Lucida ships opinionated default workflows or
requires the user to supply their own. Shipping defaults is friendlier and ages
badly; requiring them is honest and unwelcoming. A small set of maintained
templates plus a `--workflow` override is probably the compromise.

Also note the ROCm caveats already documented for that machine — a local provider
inherits its host's quirks, and "it hangs" will be the first bug report.

### Flux — Black Forest Labs (hosted)

**Second, and cheap once local is done.** By this point Flux is already
understood: same model family, same parameter model — seed, steps, guidance,
negative prompt. The hosted API becomes a second *transport* for a provider
already integrated, rather than a first encounter with a new vendor and a new
shape at once.

It is also the provider genuinely worth being able to switch *to*. Closest to
Google on quality and cost, which is what makes it the real substitution
candidate rather than merely another supported name — and an abstraction is only
proven by a real substitution.

Between them, local and hosted Flux force the capability question that Google
alone never raises. Flux exposes parameters Google simply lacks, seeds above all:
Google never surfaces one, so nothing in Lucida can currently express "give me
that result again." Determinism is not a parameter that bolts on afterwards, and
meeting it early is why this pair belongs ahead of tidier options.

**Licensing needs checking per model, not per vendor.** The FLUX family has
shipped under materially different terms — some permissive, some
non-commercial-only. That distinction is exactly the "low fence" question the
studio already has a position on, and it applies to the *weights*, not the API.
Running Klein locally is a licence question; calling the hosted API is a
commercial-terms question. They can have different answers. Resolve both and
record them here rather than in someone's memory.

### OpenAI

Demoted from second to third, not dismissed. The reasoning is only about
ordering: it is a one-off. Its parameter model shares little with Flux, with the
local lane, or with Google, so implementing it teaches the abstraction less per
unit of work than Flux does.

It still earns a place. Mask-based inpainting is the one editing model nothing
else here uses, and it is the piece most likely to strain `ImageRequest` —
`references: Vec<String>` cannot express "this region of this image." That is a
structural gap worth discovering deliberately rather than late. Also handles
explicit pixel sizes rather than named ratios, and a quality parameter with its
own semantics.

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
