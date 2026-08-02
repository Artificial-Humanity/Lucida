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

$ lucida models --provider bfl
```

One static binary. No Python, no virtualenv, no dependency resolution at startup
— which matters most in the MCP case, where the server is launched and killed
constantly and any stray write to stdout corrupts the protocol.

**What this file deliberately does not cover:** what each provider charges, how
fast it is, which models it offers today, or what provenance marking its output
carries. Those belong to the providers, they change without notice, and a copy
of them here would be wrong before it was stale. `lucida models --provider
<name>` asks the provider and prints the live answer; pricing and policy are
worth reading from the provider directly.

## Install

```console
cargo install --git https://github.com/Artificial-Humanity/Lucida
```

Or take a prebuilt binary from the
[latest release](https://github.com/Artificial-Humanity/Lucida/releases/latest):

- **macOS** — `lucida-<version>-macos-universal`, covering Apple Silicon and
  Intel. It is unsigned, so first run needs
  `xattr -d com.apple.quarantine lucida-*-macos-universal`.
- **Linux** — `lucida-<version>-x86_64-linux-musl`, statically linked with no
  libc or OpenSSL dependency.
- **Windows** — `lucida-<version>-x86_64-windows.exe`.

Or build it: `git clone …` then `cargo build --release`, binary at
`target/release/lucida`.

## Configuration

Every setting is an environment variable, and every one can also live in a
config file for when there is no shell to read the environment from.

```console
$ lucida config --set BFL_API_KEY       # prompts, masked; or reads a pipe
$ lucida config                         # what this process sees, and from where
```

| Variable | For |
|---|---|
| `GOOGLE_API_KEY` | Google — images via Gemini, video via Veo. `GEMINI_API_KEY` is accepted as an alias |
| `BFL_API_KEY` | Black Forest Labs — hosted FLUX |
| `STABILITY_API_KEY` | Stability AI |
| `OPENAI_API_KEY` | OpenAI |
| `LUCIDA_COMFYUI_URL` | Where ComfyUI is listening. Defaults to `http://127.0.0.1:8188`; no credential needed |
| `LUCIDA_COMFYUI_AUTH` | ComfyUI credentials, if it is fenced. `user:password`, a complete `Bearer …` / `Basic …` header, or a bare token. Sent on every request including the image download |
| `LUCIDA_COMFYUI_CA` | Path to a PEM certificate, to trust a private CA |
| `LUCIDA_CONFIG` | Path to the config file, overriding where it is looked for |

You only need the ones for providers you actually use. Keys come from each
provider's own dashboard.

**`lucida config`** prints whether each setting is present and where it came
from — never a value — so its output is safe to paste into a bug report.

**`lucida config --set NAME`** writes one setting into the config file. At a
terminal it prompts and shows asterisks rather than the value; given a pipe it
reads stdin, so the key never enters shell history:

```console
$ pbpaste | lucida config --set OPENAI_API_KEY
```

**`lucida config --init`** writes a starter file, mode 600, and prints its path.
It never overwrites an existing one.

The file is plain `KEY=value` lines using the same names as the environment
variables, so there is no second vocabulary to learn. A leading `export` and
surrounding quotes are both accepted, so a fragment of a shell profile can be
pasted straight in. Text after `#` is ignored.

**The config file wins.** A value there takes precedence over the same name in
the environment, which is what lets you give Lucida a key of its own when your
shell already exports a broader one — a fine-grained key scoped to this tool
rather than the general-purpose one in your profile. `lucida config` names any
setting that exists in both, so a shadowed export is reported rather than left
to be discovered.

For a one-off, name a different file: `LUCIDA_CONFIG=alt.env lucida …` wins
outright over both.

It is looked for in order: `$LUCIDA_CONFIG` (a file path, wins outright), then
`$XDG_CONFIG_HOME/lucida/config.env` or `~/.config/lucida/config.env`, then on
macOS `~/Library/Application Support/lucida/config.env`. Lucida warns if the
file it reads is readable by other users.

> **A GUI-launched application has no shell environment**, and neither does any
> MCP server it spawns — so a key exported in `~/.zshenv` is genuinely invisible
> to a client started from the Dock, Finder or Spotlight, even though the same
> binary works perfectly from a terminal. This is what the config file is for.
> Note that passing `--env GOOGLE_API_KEY='${GOOGLE_API_KEY}'` to `claude mcp
> add` does **not** fix it: the reference expands from the client's own
> environment, which is exactly the empty one.

## Providers

Five: `google`, `comfyui`, `bfl`, `stability` and `openai`.

The provider is inferred from the model id, so `--model klein` reaches ComfyUI
and `--model banana` reaches Google. `--provider` overrides it and supplies that
provider's default model, so `--provider comfyui` alone works.

They do not support the same things — seeds, negative prompts, masks, sampler
settings and aspect ratios all vary, and on some providers they vary per model.
Rather than reproduce that here, ask:

```console
$ lucida models --provider comfyui
```

which lists the models that provider can currently reach, the aliases Lucida
accepts for them, and exactly what it can be asked for.

**Nothing is silently dropped.** A parameter the chosen provider cannot honour
is an error naming one that can, not an image that quietly ignored you:

```console
$ lucida generate "a test" --seed 42
error: `google` has no concept of a seed, so `--seed` cannot be honoured.

Google never exposes one, which means results there are not reproducible by any
means. Use the `comfyui` provider (a local model, e.g. `--model klein`) when you
need to render the same image twice.
```

ComfyUI is the one provider with no credential and no per-image cost, and the
one that takes minutes rather than seconds. Lucida builds the workflow graph
itself and submits it to `/prompt`, so nothing needs importing into the UI, and
it asks the server which model files exist rather than assuming an install.
Images move over HTTP in both directions rather than through the filesystem, so
a ComfyUI in a container or on another machine works with no shared mount.

## Commands

| Command | What it does |
|---|---|
| `lucida generate <prompt>` | Prompt to image. Writes `image.png` unless `--out` |
| `lucida edit <image> <prompt>` | Edits an existing image. **Overwrites its input** unless `--out` |
| `lucida video <prompt>` | Renders with Veo. Takes minutes, and bills per second of output |
| `lucida check <operation>` | Resumes a video render by operation id — after a timeout, or from a different shell |
| `lucida models` | What a provider can reach, and what it can be asked for |
| `lucida config` | What settings this process can see |
| `lucida mcp` | Runs as an MCP server over stdio |

`--help` on any of them lists its options and notes which providers honour each.

**Geometry.** `--aspect` takes `W:H`; `--size` takes a tier (`1K`, `2K`, `4K`)
or a pixel count for the long edge. Both are resolved onto whatever grid the
provider uses, so `--aspect 16:9 --size 1K` means the same thing everywhere and
becomes `1024×576` on ComfyUI. Omit either to let the provider choose.

**Output.** Parent directories are created as needed, and the written path is
printed to stdout alone, so it composes:

```console
$ open "$(lucida generate "a sunset" -o /tmp/x.png)"
```

Extensions are corrected to match the bytes actually returned: `-o icon.png`
that comes back as JPEG is written as `icon.jpg`, with a note on stderr. A file
named `.png` holding JPEG bytes passes unnoticed until something downstream
rejects it. Since the real path is what goes to stdout, `$(lucida generate …)`
stays correct either way.

An edit is normalised to the resolution the model works at rather than kept at
the source's exact pixel dimensions, so aspect survives but size may not.
Lucida prints the size it actually wrote; pass `--aspect` or `--size` to state
the geometry you want.

### Your own ComfyUI workflow

Lucida builds a Flux.2 graph by default. To render something else — a different
model family, a ControlNet, an upscaler — supply a graph of your own in
ComfyUI's **API format** (Workflow → Export (API), not the editor format with
`nodes` and `links` arrays):

```console
$ lucida generate "a brass astrolabe" --provider comfyui --workflow mine.json
```

Mark where values belong with tokens:

| Token | Filled from |
|---|---|
| `%prompt%` `%negative%` | the prompt and `--negative` |
| `%width%` `%height%` | `--aspect` and `--size` |
| `%seed%` `%steps%` `%cfg%` | `--seed`, `--steps`, `--guidance` |

**A token the file omits means that option cannot be honoured, and Lucida
refuses it rather than ignoring it.** A graph with no `%seed%` would render
happily and drop `--seed` on the floor — the silent failure this tool exists to
prevent, arriving through the one door built to let you past its defaults. So
the tokens present in your file are the capability set for that render.

Values are JSON-escaped on the way in, so a prompt containing quotes cannot
corrupt the graph. A workflow cannot be combined with reference images or an
explicit `--model`, since it names its own checkpoints and Lucida has no way to
know which node an upload belongs to.

## Use as an MCP server

Register it once, for every project:

```console
claude mcp add --scope user lucida -- /path/to/lucida mcp
```

Verify with `claude mcp list`, which should report `✔ Connected`. If keys live
in your shell rather than a config file, read the note under
[Configuration](#configuration) first — it is the failure this setup hits most.

Four tools are exposed:

| Tool | Purpose |
|---|---|
| `generate_image` | Prompt to image, or image editing via `reference_images` |
| `image_providers` | Reports which providers are reachable and what each supports |
| `start_video` | Begins a Veo render, returns an operation id immediately |
| `check_video` | Polls that operation; downloads it once finished |

**The schema is deliberately generic, and `image_providers` is why.** Version
0.1 advertised Google's aspect ratios and size tiers as hard enums. Those are
not facts about image generation, they are facts about Google — and an agent
reads a schema and believes it. Since one server serves every provider, chosen
per call from the model id, there is no single configured provider whose schema
could be published. So parameter descriptions name which providers honour them,
`image_providers` reports live capabilities on request, and a parameter the
chosen provider cannot honour comes back as an error naming one that can.

Video is split in two deliberately. A Veo render takes minutes — long enough
that a single blocking call would likely hit the client's timeout and abandon a
render you had already paid for. Starting and polling separately keeps every
call fast, and because the operation id is just a string, a render started by an
agent can be recovered from the shell with `lucida check <operation>` even if
the agent session dies.

Failures come back as tool content rather than protocol errors, so the agent
reads the message and adapts instead of the call simply dying.

## Troubleshooting

Errors from a provider are reported as the provider gave them, with the status
code — so a quota, billing or model-availability problem is answered by that
provider's dashboard rather than here. The entries below are the ones that are
about Lucida.

**Every generation says no API key found, but the key is set.** The client was
launched as a GUI application and inherited no shell environment. Run `lucida
config` to confirm what the process can actually see, then put the key in the
config file with `lucida config --set`.

**The MCP tools do not appear.** Claude Code reads its server list at startup,
so a newly added server is invisible to the session that added it — restart,
then check `claude mcp list` reports `✔ Connected`. Note that "connected" only
proves the binary launched and answered a handshake; it says nothing about the
API key, because listing tools never calls the API. The first real generation is
what proves credentials.

**`could not reach ComfyUI`.** The server has to be running, and Lucida looks at
`http://127.0.0.1:8188` unless `LUCIDA_COMFYUI_URL` says otherwise. A hostname
that resolves elsewhere on your network may not resolve for Lucida — prefer the
host and port you can `curl`. Short hostnames are a common culprit: a name may
exist only as a fully qualified one.

**`HTTP 401` from ComfyUI.** The server, or a proxy in front of it, wants
credentials. Set `LUCIDA_COMFYUI_AUTH`. The message distinguishes "none were
sent" from "the ones you sent were rejected", so it tells you which half to fix.

**`TLS verification failed`.** The server answered — this is a certificate
problem, not an unreachable host, and restarting the server will not help.
Lucida trusts the bundled Mozilla root set and never reads the system CA store,
which is what lets the static binary do TLS on a host with no certificates
installed. A publicly trusted certificate therefore needs no configuration;
only a genuinely private CA needs `LUCIDA_COMFYUI_CA`. There is no flag to skip
verification.

**A ComfyUI render appears to hang.** Give it several minutes before assuming
otherwise; elapsed time is reported every 30 seconds. A local provider inherits
its host's quirks — on ROCm, for instance, ComfyUI needs `--disable-mmap` or
diffusion models hang on load rather than failing.

**The written file has a different extension than requested.** Intended — see
[Commands](#commands).

## Roadmap

[ROADMAP.md](ROADMAP.md) records what is built, what is next, and the decisions
behind both — including which providers were ruled out and why.

## Licence

Apache-2.0.
