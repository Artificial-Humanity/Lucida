# Roadmap

Lucida speaks to two providers: Google, for images via Gemini and video via Veo,
and a local ComfyUI for images. This records where it goes next and, more
usefully, what has to be true first.

Nothing below the "Done" section is committed work. Items are ordered by what
unblocks what, not by enthusiasm.

---

## 0. Done

### The provider abstraction

`ImageProvider` in `provider.rs` — `generate`, `capabilities`, `list_models` —
with Google and ComfyUI behind it. Resolved as planned, by all three routes:

1. **Normalized what genuinely maps.** `Aspect` holds a `W:H` pair and `Size`
   holds a long edge in pixels. `ImageRequest::pixels` resolves them onto a
   provider's grid; Google translates back to its named ratios and tier names.
   `--aspect 16:9 --size 1K` means the same thing on both and becomes `1024×576`
   locally.
2. **Declared capabilities and failed early.** `Capabilities::check` runs before
   anything is spent, and every message names a provider that supports what was
   asked for. This carried more weight than expected: it is what lets the local
   lane add seed, negative prompt, steps and guidance without those parameters
   silently evaporating when the request goes to Google.
3. **No typed escape hatch was needed.** The union stayed small enough that a
   flat `ImageRequest` with capability-guarded optional fields is honest. Revisit
   if a provider arrives with a parameter that genuinely has no analogue —
   OpenAI's mask is the likeliest candidate, and it is a shape problem rather
   than a naming one.

Provider selection is inferred from the model id, with `--provider` overriding
and also supplying the model default.

### The config file

Anticipated here as "a config file for keys once there is more than one to
hold". It arrived for a different and better reason, which is worth recording
because the original framing would have deferred it indefinitely.

**Reading credentials from the environment alone is correct for a CLI and
quietly broken for an MCP server.** A GUI-launched application on macOS — from
the Dock, Finder or Spotlight — inherits no login shell, so a key exported in
`~/.zshenv` is invisible to it and to every server it spawns. The same binary
works perfectly from a terminal, which makes the failure look like anything
except what it is.

Neither obvious workaround is good. `launchctl setenv` exports the secret to
every process in the login session and does not survive a reboot. Passing
`--env GOOGLE_API_KEY='${GOOGLE_API_KEY}'` to `claude mcp add` does not work at
all, because the reference is expanded from the client's own environment — the
empty one. The README previously recommended that, and was wrong.

So: an optional file of `KEY=value` lines at `~/.config/lucida/config.env`
(with the native macOS location as a fallback, and `LUCIDA_CONFIG` overriding
both), read only when a variable is unset. Deliberately not TOML — the keys
*are* environment variable names, and any other format would invent a second
vocabulary for the same settings plus a mapping to keep in sync. It also keeps
the parser to a few lines and adds no dependency, the same reasoning that kept a
JSON-RPC crate out of `mcp.rs`.

`lucida config` reports what the *running process* can see and where each
setting came from, never a value. That distinction is the whole diagnostic: the
answer differs between a terminal and a GUI-launched server, and no other tool
can tell you which one you are looking at.

**The general lesson, which applies to every provider still to come:** a
credential mechanism has to work in the environment the program actually runs
in, and for an MCP server that is not a shell.

**The MCP schema question was decided in favour of a generic schema plus a
capabilities probe.** The deciding argument was not on the original list: a
single MCP server serves *both* providers, chosen per call from the model id, so
there is no one "configured provider" whose schema could be regenerated. The
enums for `aspect_ratio` and `size` are gone — a test now fails if either comes
back — parameter descriptions name which providers honour them, and a new
`image_providers` tool reports live capabilities. Unsupported parameters return
an error as tool content, so the model can read it and retry.

### ComfyUI

Text-to-image against Flux.2, verified end to end on a Radeon 8060S (gfx1151):
1024×1024, 20 steps, ~270 seconds cold. Lucida builds the API-format graph
itself, so nothing needs importing into the UI, and it asks `/object_info` which
model files exist rather than hardcoding this machine's install.

**Determinism is real, and was measured rather than assumed.** Two separate runs
at seed 12345 produced byte-identical PNGs. Worth stating precisely, because
"supports a seed" is a claim several hosted providers make while returning
something merely similar — and because reproducibility is the single capability
Google cannot offer at any price.

Two choices worth recording:

- **`CFGGuider` rather than the `BasicGuider` + `FluxGuidance` pair** the
  text-to-image blueprint uses. `CFGGuider` takes both positive and negative
  conditioning, which is what makes the negative prompt a real input here rather
  than a parameter accepted and ignored.
- **Results are fetched over `/view`, not read off disk.** A ComfyUI in a
  container or on another host works with no shared mount. Worth noting the
  hostname trap that prompted it: `comfyui.ai-lab-0` does not resolve on the
  machine ComfyUI runs on, while `localhost:8188` answers — the short name has no
  record at all, only the fully qualified one does.

**Remote servers are supported**, added immediately after the first version
because "does this work over https" turned out to have three separate answers.
TLS worked already, since the URL is used verbatim; authentication did not exist
at all; and a private CA was rejected. All three now handled —
`LUCIDA_COMFYUI_AUTH` (Basic, Bearer or `user:pass`, applied to every request
including the `/view` download) and `LUCIDA_COMFYUI_CA`. Verified against
a live TLS endpoint with a real Let's Encrypt certificate, and against test
servers for the 401, wrong-credentials, private-CA and self-signed cases.

Two details worth keeping:

- **Credentials are stripped out of the base URL at construction.** The base URL
  is printed in error messages, and `https://user:pass@host` would otherwise put
  a password into terminal scrollback and pasted bug reports.
- **A 401 used to be reported as "ComfyUI has no `UNETLoader` node — this build
  may be too old".** Every non-200 from `/object_info` was read as a missing
  node. That is the exact inverse of the truth, and the kind of message that
  sends someone to upgrade a server that was only ever refusing them. Refusals
  are now diagnosed separately, and told apart by whether credentials were sent.
- **No `--insecure` flag**, deliberately. `LUCIDA_COMFYUI_CA` covers the honest
  case; disabling verification wholesale is a different and worse thing, and is
  not offered.

**Confirmed: local output carries no provenance marking.** No SynthID string, no
C2PA manifest, checked the same way we checked Google's. The README's flat
watermarking claim is now per-provider.

**Still open on this lane**, in the order they matter:

- **Editing.** `references` is declared `false` and rejected with an
  explanation. It needs an uploaded image (`/upload/image`, multipart) and a
  reference-conditioning graph — `VAEEncode` into `ReferenceLatent`, which the
  Klein edit blueprint already shows. The nodes are all present on the server;
  this is unfinished, not blocked.
- **`--workflow` override.** Lucida ships one opinionated graph and no way to
  supply your own. The compromise named below — maintained templates plus an
  override — is still the right one; only the first half exists.
- **Non-Flux model families.** The graph hardcodes Flux.2's node types
  (`Flux2Scheduler`, `EmptyFlux2LatentImage`, `CLIPLoader type=flux2`). A
  different family needs a different graph, which is the `--workflow` work again
  from the other end.
- **Inpainting.** Mask-based editing is reachable locally via the Flux.1 Fill
  blueprint, and would strain `references: Vec<String>` in the same way OpenAI
  would. Still the cheapest place to discover that gap.

---

## 2. Providers

| Provider | Appeal | Main obstacle |
|---|---|---|
| ~~Local (ComfyUI)~~ | — | **Done; see §0** |
| **Flux (Black Forest Labs)** | Nearest substitute on quality and cost; same models, hosted | Weight licensing differs sharply per model |
| **OpenAI** | Documented REST API, mask-based editing | None significant, but a one-off — shares little with the others |
| **Adobe Firefly** | Licensed training data and enterprise indemnification | Entitlements and credential complexity |
| **Midjourney** | Distinctive aesthetic | No official general API; unofficial ones breach ToS |

### Local — ComfyUI (delivered; what the bet was worth)

Implemented — see §0 for what shipped and what is still open. Kept here because
the argument for going second-to-ComfyUI was the load-bearing decision, and it is
worth recording whether it held.

**The bet.** ComfyUI is the least representative provider on this list — a
workflow graph rather than a prompt call — so designing the trait against it
first looked like a way to end up with a graph-shaped abstraction. The counter
was that Google is *already implemented*, so Google plus ComfyUI means designing
against the two most dissimilar providers available, which is the pair most likely
to produce an abstraction that survives the rest. Google plus OpenAI would have
been two variations on one shape, flattering a design that had not been tested.

**It held, and for a sharper reason than expected.** The dissimilarity did not
land where predicted. The graph-versus-prompt gap turned out to be shallow —
graph construction is confined to one private method and never reaches the trait,
because the *call pattern* (submit, poll, download) was already familiar from
Veo. What genuinely strained the design was the parameter mismatch: seed and
negative prompt exist locally and simply do not exist on Google. That is what
forced `capabilities()` to be real rather than decorative, and it is what would
have been missed by a tidier second provider.

The one prediction that was wrong in a useful direction: **the shipped blueprints
were not the shortcut they looked like.** They are subgraph definitions in UI
format, not the API format `/prompt` accepts, and the Flux.2 Klein blueprint
targets the 4B while the installed model is the 9B. They were far more valuable
as documentation of which node types and parameters Flux.2 wants than as graphs
to adapt — reading them, then building the graph in Rust, was cheaper than
converting them.

Two facts confirmed rather than assumed: local output carries no SynthID and no
C2PA manifest, and Flux really does run on the gfx1151. The ROCm caveats hold —
`--disable-mmap` is mandatory, and "it hangs" is indeed the first thing anyone
will report, which is why elapsed time is now printed every 30 seconds.

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

The capability question this pair was meant to force has already been answered by
the local half: seed, negative prompt, steps and guidance all exist there and
none exist on Google, so `Capabilities` is load-bearing and determinism did not
have to bolt on afterwards. Hosted Flux therefore inherits a design that already
expects it, and the useful thing it tests is different — whether a provider can
be swapped for another *with the same parameters* and a different transport.
That is the substitution the abstraction is actually for, and it remains
unproven.

**Licensing needs checking per model, not per vendor, and is still unresolved.**
The FLUX family has shipped under materially different terms — some permissive,
some non-commercial-only. That distinction is exactly the "low fence" question the
studio already has a position on, and it applies to the *weights*, not the API.
Running Klein locally is a licence question; calling the hosted API is a
commercial-terms question. They can have different answers.

**This now matters more than it did**, because the local lane shipped without
resolving it. Nothing in Lucida depends on the answer — it holds no weights and
bundles no model — but anything published from a Klein render does. Resolve both
and record them here rather than in someone's memory.

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

**Provenance stopped being uniform — handled.** `Provenance` is part of
`Capabilities`, the README's flat claim is now a per-provider table, and Lucida
reports the marking on every render rather than leaving users to grep the bytes
as we did. Each new provider has to state its own, which is the point: the type
makes omitting it impossible rather than merely impolite.

**Speed stopped being uniform — partly handled.** This was not on the original
list and should have been. Google answers in seconds; a cold local render took
270. The gap is large enough to change how a caller behaves, so the MCP tool
description says so and elapsed time is reported during a render. Still open: an
agent has no way to ask "how long will this take" before committing, and the
answer varies by two orders of magnitude.

**Cost stops being uniform.** Per-image pricing varies by an order of magnitude
and local generation is free at the margin. Anything that estimates or warns
about spend has to be provider-aware. Nothing estimates spend yet, so this is
still ahead.

**Failure modes multiply — confirmed, on schedule.** ComfyUI brought its own
catalogue immediately: a server that is not running, a model file that is not
installed, a graph rejected in validation with the useful sentence four levels
down in the JSON. Each got a dedicated message, and the troubleshooting section
grew as predicted. Expect this to continue faster than the feature list.

**Testing gets harder — now the sharpest open problem.** The unit tests cover
what can be checked without a network: request normalization, capability
rejection, schema honesty, error formatting. Everything else was verified by
hand against a live server, and a single verification cycle on the local lane
costs 2–5 minutes of wall clock. That is affordable for one provider and will not
be for three. Recorded-response testing is needed before the next one, or
coverage will quietly become aspirational.

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
