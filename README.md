# Lucida

Generate and edit images and video — as a CLI, or as an MCP server so coding
agents can make their own assets. Images come from Google Gemini, a local
ComfyUI, hosted FLUX, Stability AI or OpenAI; video comes from Veo, Runway or Kling.

## Contents

- [Install](#install) · [Updating](#updating)
- [Usage](#usage)
- [Configuration](#configuration)
  - [Every setting](#every-setting) — the complete list `config --set` writes
  - [Where settings come from](#where-settings-come-from)
- [Providers](#providers)
- [Commands](#commands)
  - [Configuration](#configuration-commands)
  - [Generating and collecting renders](#generating-and-collecting-renders)
  - [Geometry and output](#geometry-and-output)
  - [Your own ComfyUI workflow](#your-own-comfyui-workflow)
- [Use as an MCP server](#use-as-an-mcp-server)
  - [Other clients](#other-clients) · [Tools](#tools) · [Scripting](#scripting)
  - [Cost, budget and the ledger](#cost-budget-and-the-ledger)
  - [The skill](#the-skill)
- [Troubleshooting](#troubleshooting)
- [Roadmap](#roadmap) · [Licence](#licence)

## Install

```console
curl -fsSL https://raw.githubusercontent.com/Artificial-Humanity/Lucida/main/install.sh | sh
```

Windows, in PowerShell:

```console
irm https://raw.githubusercontent.com/Artificial-Humanity/Lucida/main/install.ps1 | iex
```

That picks the binary for your platform, verifies its published checksum,
installs it to `~/.local/bin` (or `%LOCALAPPDATA%\Programs\lucida`), and tells
you if that directory is not on your PATH. `LUCIDA_INSTALL_DIR` chooses
somewhere else; `LUCIDA_VERSION=v0.10.0` pins a version instead of taking the
latest. Both are read by the installer only — they are not Lucida settings and
do not belong in the config file. Nothing needs a toolchain, and nothing needs
sudo.

Or download it first and read it:

```console
curl -fsSL https://raw.githubusercontent.com/Artificial-Humanity/Lucida/main/install.sh -o install.sh
less install.sh && sh install.sh
```

<details>
<summary>Other ways in</summary>

Prebuilt binaries are on the
[releases page](https://github.com/Artificial-Humanity/Lucida/releases/latest),
each with a `.sha256` beside it: `lucida-<version>-macos-universal` (Apple
Silicon and Intel), `lucida-<version>-x86_64-linux-musl` (static, no libc or
OpenSSL), and `lucida-<version>-x86_64-windows.exe`. A binary downloaded in a
browser on macOS carries the quarantine attribute and needs
`xattr -d com.apple.quarantine lucida-*` on first run — one the installer avoids,
since curl does not set it.

With Rust installed:

```console
cargo install --git https://github.com/Artificial-Humanity/Lucida
```

Or from a clone: `cargo build --release`, binary at `target/release/lucida`.

</details>

### Updating

```console
$ lucida update
Current version    0.9.0
Available version  0.9.1

A newer version is available. Would you like to update? [y/N]
```

```console
$ lucida update
Current version    0.9.1
Available version  0.9.1

You have the latest version of Lucida.
```

`--check` reports without offering to install; `--yes` installs without asking,
for automation. Without a terminal to answer at, `lucida update` refuses rather
than assuming yes — a scripted update that installed silently would be the one
thing this deliberately does not do.

It knows how it was installed, and either way it finishes the job rather than
handing you a command. A downloaded binary replaces itself — verifying the
published checksum, then swapping atomically — with no toolchain involved. A
cargo-installed copy is rebuilt by running cargo, pinned to the release tag so
you get the version you were just offered rather than whatever `main` has
become; cargo's own output goes straight to your terminal, since that build
takes a few minutes and its errors are the diagnosis.

The two are kept apart because overwriting a cargo-managed binary would leave
cargo believing it manages a file it did not build, and the next
`cargo install` would silently revert the update.

**Nothing ever updates itself.** At most once a day, on an interactive terminal,
Lucida prints one line noting that a newer release exists — and installs
nothing. Set `LUCIDA_NO_UPDATE_CHECK=1` to silence it. The check never runs in
`mcp` mode or when output is not a terminal, so scripts, pipelines and agents
see nothing and pay no network round trip.

## Usage

```console
$ lucida generate "a minimalist e-ink reading icon, flat vector, single weight" \
    --out public/icon.png --aspect 1:1

$ lucida generate "abstract gradient mesh, deep indigo into amber" \
    --out public/og-image.png --aspect 16:9 --size 1200

$ lucida edit public/og-image.png "warm the background" --out public/og-warm.png

$ lucida edit product.png "replace the label text" \
    --mask label-area.png --provider comfyui

$ lucida generate "a brass astrolabe on dark slate" \
    --provider comfyui --seed 12345 --negative "text, watermark"

$ lucida generate "a wordmark for a note-taking app" --count 3   # icon-1.png…
$ lucida video "a slow push through fog" --provider kling --dry-run   # costs nothing

$ lucida video "a red maple leaf drifting down against white" --out clip.mp4
$ lucida video "waves against a harbour wall at dusk" --no-wait
$ lucida video "a slow push through fog" --provider runway --duration 6
$ lucida check operations/xyz --out clip.mp4

$ lucida models --provider comfyui

$ lucida setup
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

Named for the [camera lucida](https://en.wikipedia.org/wiki/Camera_lucida), the
optical device that let artists trace what they saw onto paper.

## Configuration

Every setting is an environment variable, and every one can also live in a
config file for when there is no shell to read the environment from.

```console
$ lucida config --set BFL_API_KEY       # prompts, masked; or reads a pipe
$ lucida config --remove BFL_API_KEY    # clears it again
$ lucida config                         # what this process sees, and from where
```

### Every setting

The complete list. These are exactly the names `lucida config --set <NAME>`
writes and `lucida config` reports — the table is checked against the same
constant the binary reads, and a test fails the build if the two drift apart.

You only need the ones for providers you actually use. Keys come from each
provider's own dashboard.

<!-- SETTINGS TABLE: checked against config::KNOWN_KEYS by a test. -->

| `lucida config --set …` | Value | For |
|---|---|---|
| `GEMINI_API_KEY` | key | Google — images via Gemini, video via Veo. One key covers both. Image generation needs billing enabled on the project behind it; a free-tier key reports a quota of zero |
| `BFL_API_KEY` | key | Black Forest Labs — hosted FLUX. Bills per image |
| `STABILITY_API_KEY` | key | Stability AI developer platform |
| `OPENAI_API_KEY` | key | OpenAI. Model access is granted per project |
| `RUNWAY_API_KEY` | key | Runway — Gen-4 video. Runway's own docs call this `RUNWAYML_API_SECRET`; Lucida uses the house `<PROVIDER>_API_KEY` spelling |
| `KLINGAI_API_KEY` | key | Kling — video. The single-key scheme, **not** a legacy AccessKey/SecretKey pair |
| `LUCIDA_COMFYUI_URL` | URL | Where ComfyUI is listening. Defaults to `http://127.0.0.1:8188`; no credential needed |
| `LUCIDA_COMFYUI_AUTH` | `user:password`, a complete `Bearer …` / `Basic …` header, or a bare token | ComfyUI credentials, if it is fenced. Sent on every request including the image download |
| `LUCIDA_COMFYUI_CA` | path to a PEM file | Trust a private CA. Only needed for a genuinely private certificate — see [Troubleshooting](#troubleshooting) |
| `LUCIDA_NO_UPDATE_CHECK` | any non-empty value | Silences the daily "a newer release exists" notice |
| `LUCIDA_NO_LEDGER` | any non-empty value | Stops recording renders. The ledger stores your prompts; `lucida config` says where it lives either way |
| `LUCIDA_BUDGET` | dollars, e.g. `25` | Estimated spend allowed in a rolling 24 hours. A render that would exceed it is refused before anything is sent |

Two more names are recognised but are **not** config-file settings:

| Variable | Why it is not in the table above |
|---|---|
| `LUCIDA_CONFIG` | Names the config file, so it cannot be a line inside it. Environment only, and it wins outright over every other location |
| `GOOGLE_API_KEY` | **Retired.** Recognised only so `lucida config` can say what replaced it, and `--set` refuses it by name. Never used as a credential |

`--set` will write a name outside that table rather than refusing it, but nothing
reads it: `lucida config` lists any such line under *"in the config file but not
recognised"*, so a typo is reported rather than silently obeyed.

> **`GOOGLE_API_KEY` was renamed to `GEMINI_API_KEY`** and is no longer read.
> Everything Lucida reaches on Google is the Gemini API — images and Veo alike —
> so one name covers both. If you have the old one and not the new one, `lucida
> config` says so by name rather than reporting a missing key, and `lucida
> config --remove GOOGLE_API_KEY` clears it from the file. Once `GEMINI_API_KEY`
> is set the migration is done and nothing mentions the old name again, whether
> or not a stale export is still lying around.

### Where settings come from

The four `lucida config` invocations are in the
[configuration command table](#configuration-commands). What they do:

**`lucida config`** prints whether each setting is present and where it came
from — never a value — so its output is safe to paste into a bug report.

**`lucida config --set NAME`** writes one setting into the config file. At a
terminal it prompts and shows asterisks rather than the value; given a pipe it
reads stdin, so the key never enters shell history:

```console
$ pbpaste | lucida config --set OPENAI_API_KEY
```

**`lucida config --remove NAME`** deletes one setting from whichever file is in
use, so changing a key does not mean remembering where the file lives. If the
same name is also in your environment, it says so — that value is what applies
once the file no longer answers.

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
> Note that passing `--env GEMINI_API_KEY='${GEMINI_API_KEY}'` to `claude mcp
> add` does **not** fix it: the reference expands from the client's own
> environment, which is exactly the empty one.

## Providers

**Images:** `google`, `comfyui`, `bfl`, `stability`, `openai`.
**Video:** `google` (Veo), `runway`, `kling`.

They are separate sets, and `--provider` means the one belonging to the command
you are running — `google` is Gemini under `generate` and Veo under `video`.

The provider is inferred from the model id, so `--model klein` reaches ComfyUI
and `--model banana` reaches Google. `--provider` overrides it and supplies that
provider's default model, so `--provider comfyui` alone works. `lucida check`
infers the provider from the shape of the operation id, so resuming a render
needs nothing but the id.

Providers exist so that a **subset** of subscriptions still buys the full width
of what you pay for. Nothing here assumes you hold all of them: every command
works with one key, `lucida models` says what that one key reaches, and a
capability refusal names alternatives without requiring you to have them.

They do not support the same things — seeds, negative prompts, masks, sampler
settings and aspect ratios all vary, and on some providers they vary per model.
Rather than reproduce that here, ask:

```console
$ lucida models --provider comfyui
```

which lists the models that provider can currently reach, the aliases Lucida
accepts for them, and exactly what it can be asked for.

**A mask does not mean the same thing on every provider**, and `lucida models`
reports which kind you have. On
`openai` a mask is *advisory* — it concentrates the change without confining it,
so pixels outside can still move, and anything needing them preserved has to
composite the result back itself. On `comfyui` it is **binding**: Lucida builds
that graph, so it composites the render through the mask before returning it,
and every pixel outside comes back byte-identical. Measured, not asserted.

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

`--help` on any of them lists its options and notes which providers honour each.
`--json` works on every one of them.

<h3 id="configuration-commands">Configuration</h3>

Everything that reads or writes a setting. `<NAME>` is one of the twelve in
[Every setting](#every-setting).

| Command | What it does |
|---|---|
| `lucida config` | Every setting, whether it is present, and which source it came from — never a value. Also prints the config file path, the ledger path, and any name in the file that Lucida does not read |
| `lucida config --set <NAME>` | Writes one setting into the config file. Prompts with masked input at a terminal; reads stdin from a pipe, so the value never enters shell history. Refuses a retired name, naming its replacement |
| `lucida config --remove <NAME>` | Deletes one setting from whichever file is in use. Says so if the same name is also in the environment, since that value is what applies afterwards |
| `lucida config --init` | Writes a starter config file, mode 600, and prints its path. Never overwrites an existing one |

<h3 id="generating-and-collecting-renders">Generating and collecting renders</h3>

| Command | What it does |
|---|---|
| `lucida generate <prompt>` | Prompt to image. Writes `image.png` unless `--out`. `--count N` renders a batch; `--dry-run` prints the plan and its cost without sending anything |
| `lucida edit <image> <prompt>` | Edits an existing image. **Overwrites its input** unless `--out`. `--mask` concentrates the change; what that guarantees differs per provider |
| `lucida video <prompt>` | Renders with Veo, Runway or Kling. Takes minutes and bills per second, so `--duration` is the flag that decides the bill. `--no-wait` prints the operation id and returns; `--mode` picks a quality tier where the provider has one; `--image` animates a still |
| `lucida check <operation>` | Resumes a video render by operation id — after a timeout, an interruption, or from a different shell. Infers the provider from the id |
| `lucida ops` | Video renders started and never collected, each with the command that finishes it |
| `lucida history` | Recent renders — prompt, provider, file, seed — and the running spend total. `-n` limits how many |
| `lucida models` | What a provider can reach and what it can be asked for. Answers for video providers too, including remaining credits |
| `lucida setup` | Wires Lucida into Claude Code and the Claude app. `--dry-run` stops after the plan |
| `lucida skill` | Prints the agent skill, for a client's skills directory |
| `lucida update` | Replaces this binary with the latest release; `--check` only reports |
| `lucida mcp` | Runs as an MCP server over stdio |

### Geometry and output

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

```console
$ lucida setup
Lucida  /Users/you/.local/bin/lucida
Scope   user — every project on this machine

  Claude Code   register MCP server (--scope user)
  skill         write ~/.claude/skills/lucida/SKILL.md
  Claude app    add mcpServers.lucida to ~/Library/…/claude_desktop_config.json

Apply this? [y/N]
```

It shows the plan before touching anything, and the first line names the binary
it will register — which matters, because it registers whichever copy you ran.
`--project [DIR]` scopes it to one project instead, `--dry-run` stops after the
plan, and `--yes` skips the question.

Claude Code's own CLI does the registering, because the tool that owns a config
format should be the one to write it. Restart both afterwards; each reads its
server list at startup.

**The Claude app's skills are uploaded, not files on disk**, so `setup` prints
the path and leaves that one step to you: Settings → Skills → Add → Upload a
skill. Everything else it does itself.

### Other clients

Any MCP client can run Lucida as a stdio server. Most take a command and
arguments:

```json
{ "command": "/path/to/lucida", "args": ["mcp"] }
```

and `lucida skill` prints the skill for clients that support them. Deliberately
no per-client instructions here: directory layouts are theirs to change, and a
list of them would be stale before it was useful.

### Tools

Six are exposed:

| Tool | Purpose |
|---|---|
| `generate_image` | Prompt to image, or image editing via `reference_images` |
| `image_providers` | Which image providers are reachable, and what each supports |
| `video_providers` | The same for video, including remaining credits where a provider exposes a balance |
| `start_video` | Begins a render on Veo, Runway or Kling; returns an operation id immediately |
| `check_video` | Polls that operation; downloads it once finished |
| `list_operations` | Every started render, so a lost operation id can be recovered instead of paid for twice |

**The schema is deliberately generic, and `image_providers` is why.** Version
0.1 advertised Google's aspect ratios and size tiers as hard enums. Those are
not facts about image generation, they are facts about Google — and an agent
reads a schema and believes it. Since one server serves every provider, chosen
per call from the model id, there is no single configured provider whose schema
could be published. So parameter descriptions name which providers honour them,
`image_providers` reports live capabilities on request, and a parameter the
chosen provider cannot honour comes back as an error naming one that can.

Video is split in two deliberately. A render takes minutes — long enough that a
single blocking call would likely hit the client's timeout and abandon a render
you had already paid for. Starting and polling separately keeps every call fast,
and because the operation id is just a string, a render started by an agent can
be recovered from the shell with `lucida check <operation>` even if the agent
session dies. `list_operations` is the same list, for when the id is gone.

Failures come back as tool content rather than protocol errors, so the agent
reads the message and adapts instead of the call simply dying.

The recorded-response tests prove Lucida still speaks *yesterday's* protocol, not
that the providers still do — so `scripts/canary.sh` asks them, on a weekly cron.
Every probe is free by construction: a model list, a balance, or a render request
naming a model that cannot exist. A render that *succeeds* there is reported as a
failure, since it would mean money was spent by a script whose whole contract is
that it spends none.

<h3 id="scripting">Scripting: <code>--json</code> and exit codes</h3>

`--json` works on any subcommand and puts one object on stdout — including on
failure, so a caller never has to switch parsers depending on the outcome. Human
prose stays on stderr. Exit codes tell four outcomes apart rather than two:

| code | meaning |
|---|---|
| 0 | done |
| 1 | something went wrong |
| 2 | refused before anything was spent — a capability or a budget said no |
| 3 | still working; ask again later |

**2 is worth its own code.** A refusal is an answer, not a failure: the request
was understood and declined before the money moved, and the message names what to
do instead — so a wrapper that retries on 1 should not retry on 2. And `lucida
check` used to report "still rendering" with the same 0 as "finished and
written", which a polling script cannot tell apart.

### Cost, budget and the ledger

Every render says what it is expected to cost, before it happens and again
afterwards, and `LUCIDA_BUDGET` turns that into a cap: a render that would take
the last 24 hours past it is refused before anything is sent, in the same voice
as a capability refusal and pointing at the local lane, which costs nothing.
Prices are estimates from published rates, each carrying the date it was checked
— a provider whose rate is not verified here is counted at a stated upper bound
rather than guessed at, and the provider's own invoice is always the authority.
`lucida history` shows the running total.

**`--dry-run` answers "what would you send?" without sending it** — the resolved
provider and model, every parameter, and the estimated cost. It runs after every
capability and budget check, so a refusal still refuses and still exits 2: it is
a rehearsal rather than a cheaper path that happens to be free. Use it before a
batch or a long clip, where the number you are choosing is the bill.

Every render is written to a ledger — one JSON object per line, beside the config
file — so a render that has been paid for can be found again afterwards. That is
what `lucida ops` reads: an agent starts a Veo render, hands back an operation id
and its session ends, and without somewhere to read the id back from, minutes of
billed output are unreachable. The `list_operations` MCP tool is the same list.
The ledger records your prompts; `LUCIDA_NO_LEDGER=1` switches it off, and
`lucida config` says where it is either way.

Tool calls run off the read loop, up to four at a time. The thread reading stdin
answers `initialize`, `ping` and `tools/list` itself and never waits on a render,
so a client using ping as a liveness probe gets an answer while a 20-minute
ComfyUI job is still going, and a second call does not queue behind the first.
`notifications/cancelled` stops the waiting where there is a poll loop to stop —
ComfyUI, BFL, Veo. It does not undo a charge: a paid provider bills when it
starts work, not when the result is read.

### The skill

[`skills/lucida/SKILL.md`](skills/lucida/SKILL.md) carries what a tool schema
structurally cannot: how to choose a provider for a job, how to iterate when a
render is close but wrong, and which outcomes differ from what was asked for. It
states no capabilities at all — those come from `image_providers` at call time,
so the skill cannot go stale as providers change. Tests enforce that: naming a
provider, a model family, or a count of providers fails the build.

The binary carries it, so you do not need the repository:

```console
$ lucida skill > ~/.claude/skills/lucida/SKILL.md
```

It is compiled in with `include_str!`, which means the file in the repository is
the file that ships and it updates with `lucida update` — you cannot be running
one version while holding another version's skill.

It prints rather than installing because skill directories differ by client —
`~/.claude/skills`, a project's `.agents/skills`, others — and Lucida has no way
to know which you mean. Redirecting puts that choice where it belongs.

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
[Geometry and output](#geometry-and-output).

**`lucida config` says a setting is "not recognised".** That name is not one
Lucida reads — check it against [Every setting](#every-setting). The message is
the only report you get, since a name nothing reads cannot fail any other way.

## Roadmap

[ROADMAP.md](ROADMAP.md) records what is built, what is next, and the decisions
behind both — including which providers were ruled out and why.

## Licence

Apache-2.0.
