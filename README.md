# lucida

Generate and edit images and video with Google's Gemini and Veo models — as a
CLI, or as an MCP server so coding agents can make their own assets.

Named for the [camera lucida](https://en.wikipedia.org/wiki/Camera_lucida), the
optical device that let artists trace what they saw onto paper.

```console
$ lucida generate "a minimalist e-ink reading icon, flat vector, single weight" \
    --out public/icon.png --aspect 1:1

$ lucida edit hero.png "replace the background with a warmer gradient"

$ lucida video "a red maple leaf drifting down against white" --out clip.mp4

$ lucida models
```

One static binary. No Python, no virtualenv, no dependency resolution at startup
— which matters most in the MCP case, where the server is launched and killed
constantly and any stray write to stdout corrupts the protocol.

## Install

```console
cargo install --git https://github.com/Artificial-Humanity/lucida
```

Or build it locally:

```console
git clone https://github.com/Artificial-Humanity/lucida
cd lucida && cargo build --release
# binary at target/release/lucida
```

## Setup

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

## Use as an MCP server

Register it once, for every project:

```console
claude mcp add --scope user lucida -- /path/to/lucida mcp
```

MCP servers inherit Claude Code's environment, so a `GOOGLE_API_KEY` exported in
`~/.zshenv` is picked up automatically. To be explicit without hardcoding the
secret, reference it instead:

```console
claude mcp add --scope user --env GOOGLE_API_KEY='${GOOGLE_API_KEY}' \
  lucida -- /path/to/lucida mcp
```

Verify with `claude mcp list`, which should report `✔ Connected`.

The server exposes one tool, `generate_image`, taking `prompt`, `output_path`,
and optionally `aspect_ratio`, `size`, `model`, and `reference_images`. Failures
come back as tool content rather than protocol errors, so the agent can read the
message and adjust instead of the call simply dying.

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

Unlike images, Veo runs as a long-running operation: `lucida` starts the render,
polls with backoff while reporting elapsed time, then downloads the result. A
short clip takes roughly a minute. Passing `--image` animates an existing still
instead of generating from text alone.

## Options

Aspect ratios: `1:1`, `2:3`, `3:2`, `3:4`, `4:3`, `4:5`, `5:4`, `9:16`, `16:9`,
`21:9`. Sizes: `1K`, `2K`, `4K`. Omit either to let the model choose.

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

## Watermarking

Everything generated here — images and video alike — is marked as AI-generated.
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

## Licence

Apache-2.0.
