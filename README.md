# Lucida

Generate and edit images and video — as a CLI, or as an MCP server so coding
agents can make their own assets. Images come from Google Gemini, a local
ComfyUI, hosted FLUX, Stability AI or OpenAI; video comes from Veo.

Named for the [camera lucida](https://en.wikipedia.org/wiki/Camera_lucida), the
optical device that let artists trace what they saw onto paper.

```console
$ lucida generate "a minimalist e-ink reading icon, flat vector, single weight" \
    --out public/icon.png --aspect 1:1

$ lucida edit hero.png "replace the background with a warmer gradient"

$ lucida generate "a brass astrolabe on dark slate" \
    --provider comfyui --seed 12345 --negative "text, watermark"

$ lucida video "a red maple leaf drifting down against white" --out clip.mp4

$ lucida generate "a brass astrolabe on dark slate" --provider bfl --seed 7

$ lucida models --provider bfl
```

One static binary. No Python, no virtualenv, no dependency resolution at startup
— which matters most in the MCP case, where the server is launched and killed
constantly and any stray write to stdout corrupts the protocol.

## Install

```console
cargo install --git https://github.com/Artificial-Humanity/Lucida
```

Or take a prebuilt binary from the
[latest release](https://github.com/Artificial-Humanity/Lucida/releases/latest):

- **macOS** — `lucida-<version>-macos-universal`, a universal binary covering both
  Apple Silicon and Intel. It is unsigned, so on first run macOS will refuse it;
  clear the quarantine attribute with
  `xattr -d com.apple.quarantine lucida-*-macos-universal`.
- **Linux** — `lucida-<version>-x86_64-linux-musl`, statically linked with no libc
  or OpenSSL dependency. Verified running on Debian 11 and Alpine, including TLS
  with no system CA store.

Or build it locally:

```console
git clone https://github.com/Artificial-Humanity/Lucida
cd Lucida && cargo build --release
# binary at target/release/lucida
```

## Configuration

Every setting is an environment variable, and every one of them can also live in
a config file for when there is no shell to read the environment from:

```console
$ lucida config --init
$ lucida config
Config file: /home/you/.config/lucida/config.env

Settings visible to this process:
  GOOGLE_API_KEY         set (config file)    Google API key
  LUCIDA_COMFYUI_URL     not set              Where ComfyUI is listening
  …
```

The file is plain `KEY=value` lines using the same names as the environment
variables, so there is no second vocabulary to learn. A leading `export` and
surrounding quotes are both accepted, which means a fragment of a shell profile
can be pasted straight in. Lines are ignored after `#`.

**The environment always wins.** The file is consulted only when a variable is
unset or empty, so adding one cannot change the behaviour of a setup that
already works.

Looked for in order: `$LUCIDA_CONFIG` (a file path, wins outright), then
`$XDG_CONFIG_HOME/lucida/config.env` or `~/.config/lucida/config.env`, then on
macOS `~/Library/Application Support/lucida/config.env`. `--init` creates it
mode 600 and never overwrites an existing file; Lucida warns if the file it
reads is readable by other users.

`lucida config` prints only *whether* each setting is present and where it came
from — never a value — so its output is safe to paste into a bug report.

## Providers

Five, and the differences are not only quality:

| | `google` | `comfyui` | `bfl` | `stability` | `openai` |
|---|---|---|---|---|---|
| Models | Gemini | Local (Flux.2) | Hosted FLUX | Stable Image | gpt-image |
| Credential | API key, billing | None | API key, paid | API key, paid | API key, paid |
| Cost | Per image | Free | ~3 credits | ~2 credits | Per image |
| Speed | Seconds | **Minutes** | Seconds | Seconds | ~15-20s |
| Aspect ratio | 10 named | Any, /16px | Any, /32px | 9 named (a *different* 9) | 3 named, or free on `gpt-image-2` |
| Output size | Tiers | Pixels | Pixels | **Not adjustable** | Only `gpt-image-2` |
| Seed | **No** | Yes | Yes | Yes | **No** |
| Negative prompt | No | **Yes** | No | **Yes** | **No** |
| Editing | Yes | Yes | Yes | Not yet | Yes |
| **Mask** | No | No | No | No | **Yes (advisory)** |
| Steps / guidance | No | Yes | `flux-2-flex`, `flux-dev` | No | No |
| Output carries | SynthID + C2PA | **Nothing** | C2PA only | C2PA only | C2PA only |

**Only `openai` accepts a mask, and it is advisory rather than binding.** Asking
for a change inside a box and measuring the rest of the picture: `gpt-image-2`
concentrated 4.5× more change inside than outside (10.9/255 elsewhere), while
`gpt-image-1.5` managed 2.0× and lost an object nowhere near the mask. If pixels
outside the mask must survive, composite the result back over the original.

Two of them use *named* aspect ratios and **disagree about which** — Google has
`4:3`, Stability has `9:21`, neither has the other's. That is why the list is
published per provider rather than shared.

**Capabilities vary per *model* on `bfl`**, which no other provider does — `steps`
and `guidance` exist on `flux-2-flex` and `flux-dev` and nowhere else in the
family. `lucida models --provider bfl` marks which is which.

Note the pattern that is easy to get backwards: the **local** lane is the one
with a negative prompt, because ComfyUI builds the graph and can wire negative
conditioning itself. No hosted FLUX endpoint exposes one.

The provider is inferred from the model id, so `--model klein` reaches ComfyUI
and `--model banana` reaches Google. `--provider` overrides it, and also picks
the default model, so `--provider comfyui` alone works.

**Parameters are never silently dropped.** Asking Google for a seed is an error
naming the provider that has one, not an image that quietly ignored you:

```console
$ lucida generate "a test" --seed 42
error: `google` has no concept of a seed, so `--seed` cannot be honoured.

Google never exposes one, which means results there are not reproducible by any
means. Use the `comfyui` provider (a local model, e.g. `--model klein`) when you
need to render the same image twice.
```

`lucida models --provider <name>` prints that provider's full capability list
alongside the models it can actually see.

## Setup

### Google

Set an API key from [Google AI Studio](https://aistudio.google.com/apikey):

```sh
export GOOGLE_API_KEY="..."     # GEMINI_API_KEY also works
```

> **Billing is required.** Image generation has **no free tier**. A free-tier key
> authenticates fine and can list models, but every generation returns
> `HTTP 429 … limit: 0` — meaning no quota exists at all, not that a quota was
> used up. Waiting will not help; enable billing at
> [aistudio.google.com/billing](https://aistudio.google.com/billing). `lucida`
> detects this specific case and says so rather than reporting a generic rate
> limit.

Check your key without spending anything — listing models is free:

```console
$ lucida models
```

### ComfyUI

Point Lucida at a running [ComfyUI](https://github.com/comfyanonymous/ComfyUI)
if it is not on the default `http://127.0.0.1:8188`:

```sh
export LUCIDA_COMFYUI_URL="http://127.0.0.1:8188"
```

**Remote servers work.** The URL is used verbatim, so `https://` and a
path-prefixed reverse proxy (`https://host/comfyui/`) both work:

```sh
export LUCIDA_COMFYUI_URL="https://comfyui.example.internal"
export LUCIDA_COMFYUI_AUTH="user:password"    # if it is fenced
```

| Variable | Purpose |
|---|---|
| `LUCIDA_COMFYUI_URL` | Where the server is. Any scheme, host, port and path prefix |
| `LUCIDA_COMFYUI_AUTH` | Credentials, sent on every request including the image download |
| `LUCIDA_COMFYUI_CA` | Path to a PEM certificate, to trust a private CA |

`LUCIDA_COMFYUI_AUTH` accepts whichever form you have to hand:

- `user:password` — encoded as Basic. This is the reverse-proxy `basic_auth` case.
- `Bearer eyJ…` or `Basic dXNl…` — a complete header value, used as is.
- anything else — treated as a bare token and sent as `Bearer`.

Credentials embedded in the URL (`https://user:pass@host`) work too, and an
explicit `LUCIDA_COMFYUI_AUTH` beats them. Either way the password is stripped
out of the URL before it can reach an error message.

**On certificates:** Lucida trusts the bundled Mozilla root set and never reads
the system CA store — that is what lets the static musl binary do TLS on a host
with no certificates installed. A publicly trusted certificate therefore needs no
configuration at all, including one issued by Let's Encrypt for an internal-only
hostname via a DNS-01 challenge, which is a common and good setup. Only a genuinely
private CA needs `LUCIDA_COMFYUI_CA`. There is no flag to skip verification.

Lucida builds the workflow graph itself and submits it to ComfyUI's `/prompt`
API, so nothing needs importing into the UI. It asks the server which model files
exist rather than assuming any particular install, and `--model klein` resolves
to whatever Flux.2 diffusion model, text encoder and VAE that server reports.

Images move over HTTP in both directions — uploaded with `/upload/image`, results
fetched with `/view` — rather than through the filesystem, so a ComfyUI in a
container or on another machine works with no shared mount.

Editing works here too:

```console
$ lucida edit hero.png "make the sky overcast" --provider comfyui
```

The source is uploaded, encoded, and attached to the conditioning. Repeating
`--ref` chains additional images, each adding to what the model is conditioned
on.

> **An edit keeps the source's aspect ratio, not its pixel dimensions.** The
> result is normalised to roughly one megapixel — the resolution Flux.2 actually
> works at — so a `1024×576` source comes back `1360×768`. It can upscale as
> readily as downscale. Aspect is preserved to within about half a percent,
> the drift coming from rounding onto the 16-pixel latent grid.
>
> This matters because `lucida edit` overwrites its input by default: editing a
> file in place can change its dimensions. Pass `--out` to write elsewhere, or
> `--aspect`/`--size` to state the geometry you want. Lucida prints the size it
> actually wrote, so this is never silent.
>
> Exact preservation is deliberately not offered: a 12 MP photograph is not
> something this model can render, so *some* normalisation is unavoidable and a
> rule that silently applies only sometimes would be worse than one that always
> applies.

Requires a Flux.2 checkpoint — diffusion model, text encoder and VAE — installed
where ComfyUI can see it. `lucida models --provider comfyui` lists what it found,
and a missing file produces a message naming the files the server actually has
rather than a validation dump.

> **Renders are slow, and the first one is slowest.** Measured on a Radeon 8060S
> (gfx1151), 1024×1024 at 20 steps with Flux.2 Klein 9B: **~270 seconds** to
> generate, **~460 seconds** to edit. Much of that is loading 33 GB of weights,
> and if ComfyUI runs with `--cache-none` every render pays it rather than just
> the first. Editing costs more on top because the encoded source adds tokens for
> the model to attend over.

### Black Forest Labs

A key from [dashboard.bfl.ai](https://dashboard.bfl.ai). This is a paid API and
every render costs credits, so `lucida models --provider bfl` checks the key
against the free `/v1/credits` endpoint and reports the balance before you spend
anything.

```console
$ lucida config --set BFL_API_KEY     # reads stdin; stays out of shell history
$ lucida models --provider bfl
Key is valid. Remaining credits: 1000
```

Measured: a 1024×576 `flux-2-pro` render took **6 seconds and 3 credits**; the
same picture edited cost 4.5. Compare ~270 seconds for the local lane, free.

Lucida prints the cost of each job before waiting for it, because this is the one
provider where a typo in a loop spends real money.

> **A malformed key comes back as HTTP 422, not 401.** Lucida recognises it
> anyway — read by status code alone it looks like a rejected parameter, which
> sends you to inspect your prompt.

## Use as an MCP server

Register it once, for every project:

```console
claude mcp add --scope user lucida -- /path/to/lucida mcp
```

Verify with `claude mcp list`, which should report `✔ Connected`.

> **If the client was launched from the Dock, Finder or Spotlight, it has no
> shell environment** — and neither does any MCP server it spawns. A
> `GOOGLE_API_KEY` exported in `~/.zshenv` is genuinely invisible, even though
> the same binary works perfectly from a terminal. Use a config file, which is
> what it is for:
>
> ```console
> lucida config --init      # writes ~/.config/lucida/config.env, mode 600
> lucida config             # shows what a process can actually see
> ```
>
> Passing `--env GOOGLE_API_KEY='${GOOGLE_API_KEY}'` to `claude mcp add` does
> **not** fix this: the reference is expanded from the client's own environment,
> which is exactly the empty one. `launchctl setenv` does work, but exports the
> secret to every process in your login session and does not survive a reboot.

Four tools are exposed:

| Tool | Purpose |
|---|---|
| `generate_image` | Prompt to image, or image editing via `reference_images` |
| `image_providers` | Reports which providers are reachable and what each supports |
| `start_video` | Begins a Veo render, returns an operation id immediately |
| `check_video` | Polls that operation; downloads it once finished |

**The schema is deliberately generic, and `image_providers` is why.** Version 0.1
advertised Google's ten aspect ratios and its `1K`/`2K`/`4K` sizes as hard enums.
Those are not facts about image generation, they are facts about Google — and an
agent reads a schema and believes it. Since one server here serves both providers,
chosen per call from the model id, there is no single "configured provider" whose
schema could be published. So parameter descriptions name which providers honour
them, `image_providers` reports live capabilities on request, and a parameter the
chosen provider cannot honour comes back as an error naming one that can. Nothing
is silently ignored.

Video is split in two deliberately. A Veo render takes minutes — long enough that
a single blocking tool call would likely hit the client's timeout and abandon a
render you had already paid for. Starting and polling separately keeps every call
fast, and because the operation id is just a string, a render started by an agent
can be recovered from the shell with `lucida check <operation>` even if the agent
session dies.

Failures come back as tool content rather than protocol errors, so the agent
reads the message and adapts instead of the call simply dying.

## Models

The default is `gemini-3.1-flash-image`. Google's codename for this family is
**Nano Banana**, which appears nowhere in the API, so `lucida` accepts both:

| Alias | Model | Notes |
|---|---|---|
| `banana`, `flash` | `gemini-3.1-flash-image` | Nano Banana 2 — the default |
| `banana-pro`, `pro` | `gemini-3-pro-image` | Nano Banana Pro — best quality |
| `banana-lite`, `lite` | `gemini-3.1-flash-lite-image` | Cheapest |
| `banana-1` | `gemini-2.5-flash-image` | Legacy; retires 2026-10-02 |

Any unrecognised value passes straight through, so a new model id works the day
it ships.

**Imagen is not supported, deliberately.** It uses a different endpoint, and the
whole family shuts down on **2026-08-17**. `lucida` detects an Imagen id and
explains rather than failing obscurely. Note that `imagen-3.0-*` ids found in
older blog posts and generated code are already retired and return 404.

## Video

```console
$ lucida video "a red maple leaf drifting down against white" \
    --out clip.mp4 --aspect 16:9 --model veo-lite

$ lucida video "slow push in, the light shifts to gold" --image still.jpg
```

| Alias | Model |
|---|---|
| `veo`, `veo-fast` | `veo-3.1-fast-generate-preview` — the default |
| `veo-standard` | `veo-3.1-generate-preview` |
| `veo-lite` | `veo-3.1-lite-generate-preview` — cheapest |

Video is billed **per second of output** and is far more expensive than images,
which is why the fast model is the default rather than the standard one.

Measured behaviour, all on `veo-lite`:

| | Result |
|---|---|
| Clip length | 8 seconds, 24 fps — not currently adjustable |
| Default resolution | 1280×720 |
| `--resolution 1080p` | 1920×1080, ~4× the file size, ~50% longer to render |
| `--aspect 9:16` | 720×1280 vertical |
| Audio | **Every clip carries an AAC audio track** — Veo generates sound, not just picture |
| Render time | 35–95 seconds for the above |

Two things worth knowing before you spend money on a long render:

- **Veo generates audio.** If you want a silent asset — a looping background, a
  hero animation — strip the track afterwards; there is no flag to suppress it.
- **With `--image`, the source aspect wins.** Animating a 1:1 still while asking
  for `--aspect 16:9` returns a 16:9 frame with the square image *pillarboxed
  inside black bars*, not cropped or extended. Match `--aspect` to your source,
  or crop the still first.

Unlike images, Veo runs as a long-running operation: `lucida` starts the render,
polls with backoff while reporting elapsed time, then downloads the result. A
short clip takes roughly a minute. Passing `--image` animates an existing still
instead of generating from text alone.

If a wait is interrupted, nothing is lost — the render continues server-side and
can be collected later by operation id:

```console
$ lucida check models/veo-3.1-lite-generate-preview/operations/abc123 -o clip.mp4
```

## Options

`--aspect` takes `W:H`. Google accepts only `1:1`, `2:3`, `3:2`, `3:4`, `4:3`,
`4:5`, `5:4`, `9:16`, `16:9`, `21:9`; ComfyUI accepts any ratio. `--size` takes
either a tier (`1K`, `2K`, `4K`) or a pixel count for the long edge — Google
rounds to the nearest tier, ComfyUI uses the number directly, rounded to the
16-pixel grid latent models require. Omit either to let the provider choose.

So `--aspect 16:9 --size 1K` means "1K on the long edge" everywhere, and becomes
`aspectRatio: 16:9, imageSize: 1K` on Google and `1024×576` on ComfyUI.

These apply to ComfyUI only, and are rejected with an explanation elsewhere:

| Flag | Meaning |
|---|---|
| `--seed` | Renders the same image again. The seed used is always reported, so an unpinned render can still be repeated |
| `--negative` | What to keep out of the picture |
| `--steps` | Sampling steps, default 20 |
| `--guidance` | How closely to follow the prompt, default 5 |

`--seed` is genuinely deterministic here, not approximately so: two separate runs
of the same prompt, model and seed produced **byte-identical** PNGs. That is the
one thing Google cannot offer at any price.

`generate` writes to `image.png` by default; `edit` overwrites its input unless
given `--out`. Parent directories are created as needed. The written path is
printed to stdout alone, so it composes:

```console
$ open "$(lucida generate "a sunset" -o /tmp/x.png)"
```

**Extensions are corrected to match reality.** Gemini picks its own output format
— usually JPEG regardless of what you asked for — so `-o icon.png` that returns
JPEG is written as `icon.jpg`, with a note on stderr. A file named `.png` holding
JPEG bytes passes unnoticed until something downstream rejects it. Since the real
path is what goes to stdout, `$(lucida generate …)` stays correct either way.

## Troubleshooting

**`IMAGE_RECITATION` — the model returns no image.** This filter fires when the
output would too closely reproduce training data, and it catches *simple* prompts
more readily than elaborate ones, which is the opposite of what most people
expect. "A small blue circle centered on white" was refused; "a weathered brass
compass on a folded nautical chart, warm window light, shallow depth of field"
went straight through.

The reason is that a terse prompt has one obvious rendering while a described
scene has many. Requests like "a simple flat icon" are the most likely to hit it.
Add material, lighting, composition, or style detail rather than reaching for
another model.

**`HTTP 429 … limit: 0`.** Not throttling — no image quota exists on the project.
See [Setup](#setup); waiting will not help.

**`negativePrompt isn't supported by this model`.** `veo-lite` rejects negative
prompts outright. Use `veo` or `veo-standard`, or drop the flag.

**`could not reach ComfyUI`.** The server has to be running, and Lucida looks at
`http://127.0.0.1:8188` unless `LUCIDA_COMFYUI_URL` says otherwise. Note that a
hostname that resolves elsewhere on your network may not resolve for Lucida —
prefer the host and port you can `curl`. Short hostnames are a common culprit:
a name may exist only as a fully qualified one.

**`HTTP 401`.** The server, or a proxy in front of it, wants credentials. Set
`LUCIDA_COMFYUI_AUTH`. The message distinguishes "none were sent" from "the ones
you sent were rejected", so it tells you which half to fix.

**`TLS verification failed`.** The server answered — this is a certificate
problem, not an unreachable host, and restarting the server will not help. Either
the certificate is issued by a private CA (point `LUCIDA_COMFYUI_CA` at it) or it
does not match the hostname you used. The underlying error is appended and names
which.

**A ComfyUI render appears to hang.** Give it several minutes before assuming
otherwise; elapsed time is reported every 30 seconds. On ROCm specifically,
ComfyUI needs `--disable-mmap` or diffusion models hang on load rather than
failing, and a cold MIOpen kernel database can add close to an hour on first use
— persist it across restarts. A local provider inherits its host's quirks.

**`ComfyUI has no diffusion model for the flux2 family`.** The checkpoint is not
installed, or not where that server looks. The message lists what it does have;
`lucida models --provider comfyui` prints the same list.

**The MCP server connects, but every generation says no API key found.** The
client was launched as a GUI application, so it inherited no shell environment
and passed that empty environment to the server. Run `lucida config` to confirm,
then `lucida config --init` and put the key in the file. See
[Use as an MCP server](#use-as-an-mcp-server).

**The MCP tools do not appear.** Claude Code reads its server list at startup, so
a newly added server is invisible to the session that added it — restart, then
check `claude mcp list` reports `✔ Connected`. Note that "connected" only proves
the binary launched and answered a handshake; it says nothing about the API key,
because listing tools never calls the API. The first real generation is what
proves credentials.

**The written file has a different extension than requested.** Intended — see
[Options](#options).

## Watermarking

**Provenance is per-provider, and the difference is real rather than claimed —
both halves were verified in the raw bytes.**

| Provider | Output carries | Survives a re-encode? |
|---|---|---|
| `google` (images and video) | SynthID watermark + C2PA manifest | **Yes** — SynthID is in the pixels |
| `bfl` | C2PA manifest only, no pixel watermark | No — metadata only |
| `stability` | C2PA manifest only, no pixel watermark | No — metadata only |
| `openai` | C2PA manifest only, no pixel watermark | No — metadata only |
| `comfyui` | Nothing | — |

That middle row is the one worth reading twice. Hosted FLUX **is** marked: a
signed C2PA manifest naming `Black Forest Labs API` as claim generator, `FLUX.2`
as software agent, and asserting `digitalSourceType: trainedAlgorithmicMedia`.
But it carries no pixel watermark, so re-encoding the file removes the disclosure
entirely — which is precisely what SynthID is designed to prevent. Marked and
*removably* marked are different claims, and both differ from unmarked.

Lucida reports which you got: `lucida models --provider <name>` lists it, the MCP
`generate_image` result states it per render, and `image_providers` reports it
for both. That is cheaper than the way we established it in the first place,
which was grepping the bytes.

The rest of this section concerns Google.

Everything Google generates — images and video alike — is marked as AI-generated.
There is no opt-out, on any tier, including paid API access.

Two different things get conflated, which is why people reasonably believe
otherwise:

- **The visible "sparkle" glyph** is a consumer Gemini *app* feature, and paid
  app tiers remove it. It is not applied to API output at all. So these files
  genuinely look clean.
- **Invisible provenance** rides along regardless: an embedded
  [SynthID](https://deepmind.google/technologies/synthid/) watermark plus a
  [C2PA](https://c2pa.org/) manifest asserting
  `digitalSourceType: trainedAlgorithmicMedia`, the IPTC code for AI-generated
  media.

Looking clean is not the same as being unmarked. Verified directly against this
tool's own paid-tier output — both the JPEG and the MP4 contain `C2PA`,
`SynthID`, and `trainedAlgorithmicMedia` strings in the raw bytes:

```console
$ grep -aoiE "c2pa|synthid|trainedAlgorithmicMedia" output.jpg | sort -u
```

Re-encoding an image (opening and re-exporting it) strips the C2PA metadata; the
SynthID watermark is designed to survive that. Treat "watermark-free" claims as
false, including any `add_watermark` config field you may encounter — that
belongs to Vertex AI and does nothing here.

**The local lane is the exception.** A Flux.2 render from ComfyUI contains no
SynthID string and no C2PA manifest, checked exactly the same way. If you need
genuinely unmarked output, that is the only provider here that produces it.

## Roadmap

[ROADMAP.md](ROADMAP.md) covers the providers still to come — hosted Flux,
OpenAI, Firefly, and why Midjourney is deliberately excluded — plus the pieces
this second provider left open, editing on the local lane chief among them.

## Licence

Apache-2.0.
