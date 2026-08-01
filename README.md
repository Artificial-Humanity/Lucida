# lucida

Generate and edit images with Google's Gemini image models — as a CLI, or as an
MCP server so coding agents can make their own assets.

Named for the [camera lucida](https://en.wikipedia.org/wiki/Camera_lucida), the
optical device that let artists trace what they saw onto paper.

```console
$ lucida generate "a minimalist e-ink reading icon, flat vector, single weight" \
    --out public/icon.png --aspect 1:1

$ lucida edit hero.png "replace the background with transparent alpha"

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

## Options

Aspect ratios: `1:1`, `2:3`, `3:2`, `3:4`, `4:3`, `4:5`, `5:4`, `9:16`, `16:9`,
`21:9`. Sizes: `1K`, `2K`, `4K`. Omit either to let the model choose.

`generate` writes to `image.png` by default; `edit` overwrites its input unless
given `--out`. Parent directories are created as needed. The written path is
printed to stdout alone, so it composes:

```console
$ open "$(lucida generate "a sunset" -o /tmp/x.png)"
```

## Watermarking

Every image from every Gemini image model carries an invisible
[SynthID](https://deepmind.google/technologies/synthid/) watermark. There is no
opt-out, on any tier.

The visible "sparkle" glyph is a consumer Gemini app feature and is *not* applied
to API output — so these images look clean, which is not the same as being
watermark-free. Any provenance detector will still identify them as generated.
Claims to the contrary are wrong, including a `add_watermark` field you may see
on some shared config types; it belongs to Vertex AI and does nothing here.

## Licence

Apache-2.0.
