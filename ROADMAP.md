# Roadmap

Lucida speaks to three providers: Google (images via Gemini, video via Veo), a
local ComfyUI, and hosted FLUX from Black Forest Labs. This records where it goes
next and, more usefully, what has to be true first.

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
both). Deliberately not TOML — the keys
*are* environment variable names, and any other format would invent a second
vocabulary for the same settings plus a mapping to keep in sync. It also keeps
the parser to a few lines and adds no dependency, the same reasoning that kept a
JSON-RPC crate out of `mcp.rs`.

`lucida config` reports what the *running process* can see and where each
setting came from, never a value. That distinction is the whole diagnostic: the
answer differs between a terminal and a GUI-launched server, and no other tool
can tell you which one you are looking at.

**Precedence was reversed after v0.5.2, and the original rule was wrong for a
reason worth recording.** The file shipped as a *fallback*: the environment won,
and the file was read only for names the environment did not answer. The
argument was migration safety — introducing a config file must not change the
behaviour of a setup that already works — and as a migration property it was
sound. What it cost was not visible until someone asked for it.

A shell exporting `OPENAI_API_KEY` for general use made a Lucida-scoped key
**unreachable**. Not awkward: impossible. `config --set` would write the value,
report `Added OPENAI_API_KEY in …`, and every render would go on using the
ambient key, because the ambient key won by rule. Fine-grained credentials — one
key per tool, scoped and revocable independently — is an ordinary way to hold
API keys, and the design had no answer for it.

Worse, the failure was *silent*, in a codebase whose entire argument is that
silent drops are the thing to refuse. `--workflow` rejects an option whose token
is missing; an unsupported parameter is an error naming a provider that has one.
The config layer quietly discarded what it had just confirmed writing.

The rule now: **a file entry is an explicit statement about Lucida; a shell
export is ambient and applies to everything that reads that name. The specific
one wins.** The per-invocation escape survives untouched, because
`LUCIDA_CONFIG=other.env` names a whole file and outranks both. And `lucida
config` now reports the loser as well as the winner — a setting present in both
is listed under "Also set in this environment, and not used", since someone
reading that output is usually asking exactly why their exported key is being
ignored.

**One credential, one name.** `GOOGLE_API_KEY` was accepted alongside
`GEMINI_API_KEY` from the start, on the reasoning that the Google SDKs look for
both. Two spellings for one credential turned out to teach nobody which was
canonical — a key in either worked, so nothing ever corrected a wrong guess —
while costing an entry in `KNOWN_KEYS`, a special case in the template
generator, and a clause in every message that mentioned it. `GEMINI_API_KEY` is
now the only one read: everything Lucida reaches on Google is the Gemini API,
images and Veo alike, and after Imagen's shutdown on 2026-08-17 nothing is left
that "Google" named more accurately.

**Retiring a name is not the same as deleting it**, which is the part worth
keeping. Someone who exported `GOOGLE_API_KEY` did nothing wrong, and simply
dropping it turns a working setup into "no API key found" — a message that sends
them to check the one thing that is not wrong. So a retired name stays
*recognised* and never used: `lucida config` lists it under "Set, but no longer
read", the credential error names the rename instead of reporting an absence,
and `config --set` refuses to write it rather than filing a value nothing reads.
`RETIRED_KEYS` is a table, so the next rename costs one line.

**`config --remove` arrived with it**, and the reason is worth recording because
it is not symmetry for its own sake: changing a key otherwise meant remembering
where the config file lives, which is exactly the knowledge `lucida config`
exists to spare you. It edits the file *in use* rather than the preferred
location — a stale value can sit in a file further down the search order, or in
one named by `LUCIDA_CONFIG`, and removing from anywhere else would report
success and change nothing. It also states what answers *next*: with the file no
longer supplying a value, an environment variable that was being shadowed
becomes the credential, and that is said at the moment of removal.

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

### Editing on the local lane

Implemented. The source is uploaded with `/upload/image` (multipart, over HTTP
rather than by writing into the server's input directory, so a remote server
still works), scaled onto the latent grid, VAE-encoded, and attached to the
conditioning with `ReferenceLatent`. Repeating `--ref` chains further images.

**The wiring worth knowing, because it looks like a bug:** the encoded source
attaches to the *negative* conditioning as well as the positive. The Flux.2 edit
blueprint does this, and the reason is that classifier-free guidance measures a
difference between two branches — if only the positive branch sees the source,
the difference is dominated by "there is an image here" rather than by the
prompt. Both branches denoise the same picture; the prompt is what differs. It is
covered by a test for exactly this reason.

Dimensions come from `GetImageSize` on the server rather than being guessed
client-side, so Lucida never has to decode the image itself. `--aspect` or
`--size` overrides, which is how an edit reframes.

**That does not mean the output matches the input, and assuming it did was
wrong.** `GetImageSize` reads the *scaled* image, and the scale normalises to
about one megapixel — so a 1024x576 source comes back 1360x768, upscaled.
Aspect survives to within 0.4%, the drift being the 16-pixel grid.

The first two edit tests both used a square source, where a 1024x1024 result is
indistinguishable from the default, so the error survived them. It took a
deliberately non-square source to expose it. Worth remembering as a testing
lesson rather than a Flux one: a fixture that matches the default proves nothing
about the code path that computes it.

Exact preservation is not the fix. A 12 MP photograph cannot be rendered by this
model, so some normalisation is unavoidable, and a rule that applies only
sometimes is worse than one that always applies. Instead the size actually
written is now reported — `image_dimensions` reads it back out of the PNG or
JPEG header — so the behaviour is stated rather than discovered. `lucida edit`
overwriting its input by default is what makes stating it necessary.

Measured at ~460-500s against ~270s to generate at the same size and step count,
the difference being the tokens the encoded source adds. Worth knowing before
assuming an edit hangs — it is the same wait again, roughly doubled.

**`references: Vec<String>` survived**, which was the open question. Chained
whole-image references fit it exactly. What it still cannot express is a *mask* —
"this region of this image" — so the structural gap the roadmap predicted is real
but untouched, and inpainting (below) is where it will finally bite.

**`--workflow` override — delivered.** A caller-supplied API-format graph, with
`%prompt% %negative% %seed% %width% %height% %steps% %cfg%` marking where values
go. The design centre: the tokens present in the file *are* the capability set
for that render, so an option with no token to receive it is refused rather
than silently dropped — the failure this whole design exists to prevent,
arriving through the one door built to let callers past the design. The editor
format is named rather than submitted, substituted values are JSON-escaped, and
validation runs before anything is announced or uploaded. Verified against a
recorded server end to end, and live 2026-08-02: a hand-written klein graph
with a renamed save node rendered 512x512 at seed 42 through the release
binary — tokens substituted, image found by shape, seed honoured.

**Still open on this lane**, in the order they matter:

- **Non-Flux model families.** The built-in graph hardcodes Flux.2's node types
  (`Flux2Scheduler`, `EmptyFlux2LatentImage`, `CLIPLoader type=flux2`). A
  different family needs a different graph — reachable today via `--workflow`,
  but not as a maintained template.
### Inpainting on the local lane — DELIVERED, and the premise was wrong

This section used to read: *"Mask-based editing is reachable locally via the
Flux.1 Fill blueprint."* That sentence cost nothing to write and would have cost
a 24 GB download and an unresolved licence decision to act on, because it was
**not true**. The Flux.2 Klein checkpoint already installed does masked
inpainting through `InpaintModelConditioning`, which was settled in one render.

Worth recording as a method rather than a fact: the claim was about a model, and
a claim about a model is testable. Reading blueprints suggested Fill; asking the
server showed `InpaintModelConditioning` and `DifferentialDiffusion` were both
present, and one probe against the installed weights answered it. The habit that
paid here is the same one that found BFL's 422 and Stability's seed header —
ask the thing itself before believing the documentation about it.

**The measurement that shaped the design.** Conditioning alone gives an
*advisory* mask, and the numbers are close to OpenAI's:

| | change inside mask | change outside | verdict |
|---|---|---|---|
| `InpaintModelConditioning` alone | 128.03/255 | 23.84/255 | advisory, 5.4x |
| plus in-graph compositing | 127.47/255 | **0.00/255** | binding |
| through the release binary | 123.82/255 | **0.00 mean, 0.00 max** | binding |

So the local lane now has a capability **no hosted provider here offers**: a
mask that binds. OpenAI's is advisory and the README says so, telling callers to
composite the result themselves. Lucida builds the ComfyUI graph, so it does
that compositing with `ImageCompositeMasked` and returns a guarantee instead of
a caveat. Every pixel outside the mask is byte-identical, max as well as mean.

Two details that would have been silent failures:

- **The mask is scaled to the *scaled* source.** The render happens at roughly a
  megapixel, so a mask cut for the original dimensions composites the change
  into the wrong place. `GetImageSize` on the scaled source drives an
  `ImageScale` on the mask.
- **The alpha convention matches OpenAI's**, verified rather than assumed —
  `LoadImage`'s MASK output reads 255 exactly where the source is transparent.
  One mask file works on both providers with nothing to convert. Checking it
  needed no diffusion at all: `MaskToImage` round-trips in seconds, where
  guessing wrong would have inverted every mask and still rendered happily.

**Still open on this lane:**

- **Non-Flux model families.** The built-in graph hardcodes Flux.2's node types
  (`Flux2Scheduler`, `EmptyFlux2LatentImage`, `CLIPLoader type=flux2`). A
  different family needs a different graph — reachable today via `--workflow`,
  but not as a maintained template. Now the only thing blocking the Mac lane,
  where 32 GB cannot hold Flux.2 Klein but would hold SDXL comfortably.

---

## 2. Providers

| Provider | Appeal | Main obstacle |
|---|---|---|
| ~~Local (ComfyUI)~~ | — | **Done; see §0** |
| ~~Flux (Black Forest Labs)~~ | — | **Done; see below** |
| ~~OpenAI~~ | — | **Done; see below** |
| ~~Stability AI~~ | — | **Done; see below** |
| ~~Adobe Firefly~~ | — | **Ruled out: subscription only** |
| ~~Midjourney~~ | — | **Ruled out: subscription only, and no official API** |

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

### Flux — Black Forest Labs (hosted) — DELIVERED

Implemented and verified against the live API. See the notes below for what it
cost to learn; the section that follows is kept as the original reasoning.

**The substitution worked, and that was the point.** ComfyUI proved the trait
could hold two dissimilar shapes; BFL proves it holds a real substitution — the
same model family, reached a different way. The call pattern is submit / poll /
download for the third time (Veo, ComfyUI, BFL), which is fair evidence it is
the right shape.

**Three predictions in this section were wrong, all in the same direction — they
assumed hosted Flux would be a superset of what we had.**

1. **There is no negative prompt.** Not on any FLUX.2 endpoint, not `flux-dev`,
   not `flux-pro-1.1`. This section confidently listed it. The *local* lane has
   one only because ComfyUI builds the graph and can wire negative conditioning
   itself — so on this axis hosted Flux is less capable than local Flux, the
   reverse of the assumption.
2. **Capabilities vary per model, not per provider.** `steps` and `guidance`
   exist on `flux-2-flex` and `flux-dev` and nowhere else in the family. That is
   a genuinely new axis: until now a provider had one answer for everyone, and
   `capabilities_for` had to grow a model argument.
3. **Provenance was not what anyone would have guessed.** BFL output carries a
   signed C2PA manifest and **no** pixel watermark — a third state, distinct from
   Google (both) and ComfyUI (neither). The practical difference is that a
   re-encode strips C2PA and cannot strip SynthID, so BFL output is marked
   *removably*. Shipping it as `Provenance::Unverified` and checking a real
   render was the right call; the obvious guess (unmarked, like other non-Google
   generators) was wrong.

**Measured:** a 1024x576 `flux-2-pro` render, 6 seconds and 3 credits; the same
picture edited, 9-12 seconds and 4.5 credits. Against ~270s and free locally.
That speed gap is large enough to change how a caller behaves and is now stated
in the tool description.

**Two bugs the live API found that no amount of reading would have.** A malformed
key returns **422**, not 401, so status-code dispatch reported it as a rejected
parameter and sent the reader to inspect their prompt. And an edit sent with the
default 1024x1024 silently reframed a 16:9 source to square — the edit itself was
good and the composition was destroyed. Dimensions are now omitted for an edit
unless asked for, matching the local lane.

**Still open:** `flux-pro-1.1-ultra` takes `aspect_ratio` rather than
width/height and is untested; the fill/expand and finetune endpoints are not
implemented; and the licensing question below is unchanged and now applies to
both lanes.

### Flux — Black Forest Labs (hosted) — original reasoning

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

### OpenAI — DELIVERED

Implemented and verified live. The prediction that mattered held: **the mask was
the structural gap, and it was real.** `references: Vec<String>` could not
express "this region of this image", so `ImageRequest` grew a mask field — the
one typed addition four other providers never forced.

What the live API corrected, in the direction of less certainty rather than
more: **the mask is advisory, not binding.** Measured by asking for a change
inside a box and comparing the rest of the frame, `gpt-image-2` concentrated
4.5× more change inside the mask than outside and `gpt-image-1.5` only 2.0×,
losing an object nowhere near the mask. So `Capabilities` says the provider
takes a mask; it cannot say the mask is honoured, and the CLI help says
"advisory" rather than implying a guarantee. Anything needing pixels outside the
mask to survive has to composite the result back over the original.

Also confirmed: OpenAI **rejects** unknown parameters, which turned out to be a
free probing technique rather than an obstacle — sending a deliberately invalid
`output_format` drew a validation error naming the parameter and listing its
accepted values, without rendering or billing anything. That answered a question
that had been deferred as needing paid verification. Worth carrying to every
future provider: ask in the way that costs nothing before paying.

### Stability AI — DELIVERED

Implemented and verified live. It never had a section of its own here, which is
recorded rather than quietly fixed: it was added in the same sweep as OpenAI and
the roadmap did not keep up.

Two things it contributed that nothing else had:

1. **A provider whose output size is not adjustable at all.** Every other
   provider takes either tiers or pixels, so `Capabilities` had carried an
   implicit assumption that *some* size control always exists. Stability made
   size a capability like any other, which is the shape the design claimed to
   have and had not yet been forced to prove.
2. **A seed reported in a response header.** The code originally recorded, from
   the documentation, that the API does not report the seed it chose. A later
   probe found `seed` and `finish-reason` as response *headers*, and pinning a
   re-render to the reported value produced byte-identical pixels. So an
   unpinned Stability render is reproducible after all — and the lesson is that
   "the API does not return X" is a claim about where someone looked, not about
   the API, until the headers have been read too.

**Still open:** editing. Stability puts it on separate endpoints
(`edit/inpaint`, `edit/erase`) rather than as parameters to generate, and
`generate` deliberately refuses an edit rather than silently turning it into a
fresh render. The capability table says "not yet" and means it.

### OpenAI — original reasoning

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

### Adobe Firefly — ruled out

**Not implementing.** Firefly has no pay-as-you-go tier: reaching it means an
Adobe subscription, and buying one purely to add a provider is not a trade worth
making. Owner decision, 2026-08-01.

The appeal was real and is worth recording so the decision is understood rather
than merely remembered: Firefly is trained on licensed content and comes with
enterprise indemnification, which for a studio publishing under its own name is a
substantive difference rather than a marketing line. That does not change the
answer. If Adobe ever ships metered access, this is worth reopening — and the
indemnification argument is why it would be.

### Midjourney — ruled out

**Not implementing**, now for two independent reasons, either of which is
sufficient.

The commercial one, and the simpler: subscription only, no metered access. Same
answer as Firefly, same reasoning. Owner decision, 2026-08-01.

The technical one, recorded earlier and unchanged: there has never been a
general-availability public API. Access has run through Discord, and the
third-party "APIs" in search results work by automating accounts in ways that
violate the terms of service. Implementing against one would break without
warning, could get an account banned, and would make Lucida complicit in a ToS
breach.

Revisit only if an official, metered API appears. Both objections would have to
fall, not one.

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

**Testing gets harder — handled, by recording the wire.** The unit tests cover
what can be checked without a network: request normalization, capability
rejection, schema honesty, error formatting. What they could not see was the
wire itself — which URL was called, which request carried the credential, what
the body actually said — and that was exactly where the money-costing bugs
lived. `testserver.rs` (test-only, no dependency, ~200 lines of `TcpListener`)
now replays responses transcribed from the live sessions while recording every
request whole, and each client grew a `base` URL field so tests can aim it at
the recorder.

What earned its keep immediately is that the recordings pin the *deliberate
asymmetries* that documentation would flatten: BFL's signed download URL must
carry **no** credential while Veo's download URL **requires** one; ComfyUI's
credentials must ride on every request *including* `/view`, the round trip that
used to be the one to fail behind a proxy; an OpenAI edit must send its
source's implied size, never `auto`. Each of those was verified by hand once
and then unguarded — now a regression is a red test, not a leaked key or a
reshaped picture.

The limit is stated rather than hidden: a recording proves Lucida still speaks
yesterday's protocol, not that the provider still does. Live verification is
still owed once per new provider or changed endpoint; the recordings make it
once rather than every change.

**The axis this missed entirely was the platform, and it cost a real bug.**
Every test ran offline, deterministically and quickly — and only ever on the
Linux machine they were written on, because the sole workflow ran on a tag and
built releases rather than running the suite. The first `cargo test` on a Mac,
at v0.5.2, failed immediately: `arbitrary_seed` read the clock once per call,
and macOS ticks `SystemTime` at 1 µs where Linux ticks at 1 ns. Measured on the
Mac, 97,543 of 100,000 back-to-back reads returned the *same* instant, so two
renders started in the same microsecond were handed the same seed — the one
property the function exists to provide. Five releases shipped with it.

Three things worth keeping from that:

- **A test can encode a platform assumption without naming one.** Nothing in
  `successive_seeds_differ` mentioned an operating system; it simply relied on
  clock resolution that only one platform has. The rewritten test asserts
  distinctness across a batch of a thousand, which cannot pass by luck anywhere.
- **The fix had to be structural, not statistical.** Stirring the bits harder
  would not have helped: multiplication by an odd constant is a bijection mod
  2^64, so equal inputs stay equal outputs. The clock is now read once and a
  counter supplies the difference between calls.
- **`.github/workflows/ci.yml`** now runs the suite, clippy and the smoke script
  on Linux, macOS and Windows for every push and pull request. Building on three
  platforms was never the same as running on them.

---

## 4. Independent of providers

- **Code signing — deliberately waiting on the organization account.** A
  Developer ID certificate is issued to the team that creates it and **cannot be
  transferred between an individual and an organization account**. Signing under
  a personal account now would mean re-issuing later and shipping a build whose
  signing identity changes underneath users, which reads exactly like the thing
  signing exists to rule out. So this waits on the Artificial Humanity org
  enrollment (D-U-N-S in progress, 2026-08-02). Owner decision.

  **The installer lowered the urgency, which is worth recording so the delay is
  not mistaken for drift.** macOS sets the quarantine attribute from browsers
  and LaunchServices, not from curl — so a binary arriving through `install.sh`
  is never evaluated by Gatekeeper and needs no `xattr -d`. Unsigned now costs a
  warning only on the path where someone downloads from the releases page in a
  browser, and Windows SmartScreen.

  When it does land, two things about a bare CLI binary that catch people out:
  `--options runtime` and `--timestamp` are required for notarization, and
  **`stapler` cannot staple a standalone executable** — only a `.app`, `.pkg` or
  `.dmg`. A notarized bare binary is checked online instead, so shipping a
  `.pkg` is the only way to get an offline ticket. Signing also has to happen
  *after* `lipo`, since fusing strips signatures.
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
