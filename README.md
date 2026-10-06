# Lucida

Generate and edit images and video — as a standalone CLI or as a Model Context Protocol (MCP) server for coding agents.

* **Images:** Google Gemini, local ComfyUI, Black Forest Labs (FLUX), Stability AI, OpenAI, Runway.
* **Video:** Google Veo, Runway, Kling.

## Contents

- [Install](#install)
- [Usage](#usage)
- [Configuration](#configuration)
  - [Settings reference](#settings-reference)
  - [Configuration resolution](#configuration-resolution)
- [Providers](#providers)
- [Commands](#commands)
  - [Configuration commands](#configuration-commands)
  - [Render commands](#render-commands)
  - [Geometry and output](#geometry-and-output)
  - [Custom ComfyUI workflows](#custom-comfyui-workflows)
- [MCP server](#mcp-server)
  - [Setup](#setup)
  - [Available tools](#available-tools)
  - [Scripting and exit codes](#scripting-and-exit-codes)
  - [Budget and ledger](#budget-and-ledger)
  - [Agent skill](#agent-skill)
- [Troubleshooting](#troubleshooting)
- [License](#license)

## Install

### Linux & macOS

```console
curl -fsSL https://raw.githubusercontent.com/Artificial-Humanity/Lucida/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/Artificial-Humanity/Lucida/main/install.ps1 | iex
```

The script installs a static binary to `~/.local/bin` (or `%LOCALAPPDATA%\Programs\lucida`). Set `LUCIDA_INSTALL_DIR` to override the destination or `LUCIDA_VERSION=vX.Y.Z` to pin a release.

Prebuilt binaries with SHA256 checksums are also available on [Releases](https://github.com/Artificial-Humanity/Lucida/releases/latest).

From v1.1.0 the macOS binary is signed with an Apple Developer ID and notarized, so a
download from that page opens without a Gatekeeper warning. Because a standalone
executable cannot carry a stapled notarization ticket, macOS checks it with Apple
**online** on first run — silent with a network, and a machine with none can still
refuse it. Binaries installed by the script above were never subject to that check:
macOS marks quarantine from browsers, not from `curl`.

### Building from source

From [crates.io](https://crates.io/crates/lucida):

```console
cargo install lucida --locked
```

From the repository, which builds whatever `main` is:

```console
cargo install --git https://github.com/Artificial-Humanity/Lucida
```

Or from a local checkout:

```console
cargo build --release
```

The compiled binary will be at `target/release/lucida`.

### Updating

```console
lucida update
```

Flags:
* `--check`: Check if an update is available without installing.
* `--yes`: Install the update without prompting.

Set `LUCIDA_NO_UPDATE_CHECK=1` to disable the daily automatic update notice.

## Usage

```console
# Generate an image
lucida generate "a minimalist vector logo for an audio engine" --out logo.png --aspect 1:1

# Generate an image with explicit dimensions and provider
lucida generate "abstract gradient mesh, deep indigo into amber" \
  --out og-image.png --aspect 16:9 --size 1200 --provider google

# Edit an image
lucida edit og-image.png "warm the background tones" --out og-warm.png

# Edit with a mask
lucida edit product.png "replace label text" --mask label-mask.png --provider comfyui

# Video generation (Veo, Runway, Kling)
lucida video "a red maple leaf drifting down against white" --out leaf.mp4 --duration 5

# Asynchronous video generation
lucida video "waves crashing against a harbour wall at dusk" --no-wait
lucida check <operation-id> --out clip.mp4

# Inspect model capabilities for a provider
lucida models --provider comfyui

# Run interactive MCP setup for Claude Code and Claude Desktop
lucida setup
```

## Configuration

Configuration settings can be set as environment variables or stored in a `KEY=value` configuration file.

```console
lucida config --set GEMINI_API_KEY      # prompts securely; or pipe via stdin
lucida config --remove GEMINI_API_KEY   # removes key from config file
lucida config                           # displays active configuration sources
```

### Settings reference

<!-- SETTINGS TABLE: checked against config::KNOWN_KEYS by a test. -->

| Setting | Value | Description |
|---|---|---|
| `GEMINI_API_KEY` | key | Google Gemini for images and Veo for video |
| `BFL_API_KEY` | key | Black Forest Labs (hosted FLUX) |
| `STABILITY_API_KEY` | key | Stability AI developer platform |
| `OPENAI_API_KEY` | key | OpenAI API |
| `RUNWAY_API_KEY` | key | Runway Gen-4 images and video |
| `KLINGAI_API_KEY` | key | Kling video (single API key) |
| `LUCIDA_COMFYUI_URL` | URL | ComfyUI base URL (default: `http://127.0.0.1:8188`) |
| `LUCIDA_COMFYUI_AUTH` | credentials | ComfyUI authentication (`user:password`, `Bearer …`, or `Basic …`) |
| `LUCIDA_COMFYUI_CA` | path | Path to PEM file for a private CA |
| `LUCIDA_IMAGE_PROVIDERS` | ordered list | Comma-separated image provider preference (e.g. `bfl,google`) |
| `LUCIDA_VIDEO_PROVIDERS` | ordered list | Comma-separated video provider preference (e.g. `runway,google`) |
| `LUCIDA_NO_UPDATE_CHECK` | flag | Set to any non-empty value to disable daily update notices |
| `LUCIDA_NO_LEDGER` | flag | Set to any non-empty value to disable recording renders in the ledger |
| `LUCIDA_BUDGET` | USD | Rolling 24-hour spending cap in USD (refuses renders exceeding limit) |

### Configuration resolution

Lucida checks configuration files in the following order:

1. `$LUCIDA_CONFIG` (explicit file path).
2. `$XDG_CONFIG_HOME/lucida/config.env` or `~/.config/lucida/config.env`.
3. `~/Library/Application Support/lucida/config.env` (macOS).

Values defined in a configuration file take precedence over environment variables.

## Providers

| Medium | Supported Providers |
|---|---|
| **Images** | `google` (Gemini), `comfyui` (local Flux/SD), `bfl` (FLUX), `stability`, `openai`, `runway` (Gen-4) |
| **Video** | `google` (Veo), `runway`, `kling` |

* **Provider resolution:** Selected with `--provider <name>`. If omitted, Lucida infers the provider from `--model` or falls back to the order defined in `LUCIDA_IMAGE_PROVIDERS` / `LUCIDA_VIDEO_PROVIDERS`.
* **Capability inspection:** Run `lucida models --provider <name>` to view accessible models, supported parameters, and remaining account balances.
* **Mask handling:** `comfyui` performs pixel-binding compositing (unmasked pixels remain byte-identical). `openai` treats masks as advisory guidance.

## Commands

Use `--help` on any command to view supported options. Pass `--json` for structured machine output.

<h3 id="configuration-commands">Configuration commands</h3>

| Command | Description |
|---|---|
| `lucida config` | Displays active settings, config file path, and ledger location |
| `lucida config --set <NAME>` | Writes setting to config file (prompts securely or accepts stdin) |
| `lucida config --remove <NAME>` | Deletes setting from config file |
| `lucida config --init` | Writes default template config file (mode 0600) |

<h3 id="render-commands">Render commands</h3>

| Command | Description |
|---|---|
| `lucida generate <prompt>` | Generates an image from a prompt |
| `lucida edit <image> <prompt>` | Edits an image (overwrites input unless `--out` is specified) |
| `lucida video <prompt>` | Renders video via Veo, Runway, or Kling |
| `lucida check <operation-id>` | Checks status and downloads completed video render |
| `lucida ops` | Lists pending asynchronous video operations |
| `lucida history` | Displays render history and running spend total (`-n` limits count) |
| `lucida models` | Lists reachable models and features for a provider |
| `lucida setup` | Configures Lucida MCP server in Claude Code and Claude Desktop |
| `lucida skill` | Emits agent skill definition to stdout |
| `lucida update` | Updates binary to the latest release |
| `lucida mcp` | Starts Model Context Protocol server over stdio |

### Geometry and output

* **Aspect and size:** `--aspect` accepts `W:H` (e.g. `1:1`, `16:9`). `--size` accepts tiers (`1K`, `2K`, `4K`) or pixel counts for the long edge.
* **Path piping:** Written output path is emitted to stdout on completion for script piping:
  ```console
  open "$(lucida generate "dawn over the mountains" -o /tmp/dawn.png)"
  ```
* **Extension correction:** If output format differs from the requested extension (e.g. JPEG bytes returned for `.png`), Lucida corrects the filename extension and reports it on stderr.

### Custom ComfyUI workflows

Pass custom ComfyUI API workflow JSON files using `--workflow`:

```console
lucida generate "a brass astrolabe" --provider comfyui --workflow workflow_api.json
```

Workflows use parameter replacement tokens:

| Token | Replaced by |
|---|---|
| `%prompt%`, `%negative%` | Prompt and `--negative` |
| `%width%`, `%height%` | `--aspect` and `--size` |
| `%seed%`, `%steps%`, `%cfg%` | `--seed`, `--steps`, `--guidance` |

Tokens must exist in the workflow file for corresponding CLI flags to be applied.

## MCP server

### Setup

Register Lucida with Claude Code and Claude Desktop:

```console
lucida setup
```

Options:
* `--project [DIR]`: Scope registration to a specific project directory instead of user-wide.
* `--dry-run`: Output configuration actions without modifying files.
* `--yes`: Apply changes without confirmation prompts.

For other MCP clients, configure `lucida mcp` as a stdio server:

```json
{
  "command": "lucida",
  "args": ["mcp"]
}
```

### Available tools

| Tool | Purpose |
|---|---|
| `generate_image` | Generates or edits images with optional reference images |
| `image_providers` | Reports active image providers and capabilities |
| `video_providers` | Reports active video providers, capabilities, and credits |
| `start_video` | Initiates asynchronous video render; returns operation ID |
| `check_video` | Polls render status and retrieves completed video |
| `list_operations` | Lists tracked in-flight video operations |

### Scripting and exit codes

The `--json` flag emits a single JSON response object to stdout. Exit codes indicate operation status:

| Code | Status | Description |
|---|---|---|
| `0` | Success | Operation completed successfully |
| `1` | Error | Execution failure |
| `2` | Refused | Pre-flight refusal (unsupported capability or budget limit exceeded) |
| `3` | Pending | Asynchronous task in progress: `lucida check`, or `lucida video` when its 15-minute wait runs out with the render still going (already billed — collect it with `lucida check`, do not re-run) |

### Budget and ledger

* **Budget enforcement:** `LUCIDA_BUDGET` sets a rolling 24-hour spending cap in USD. Requests exceeding the cap fail immediately with exit code `2`.
  The value must be a plain number (`5`, `2.50`). One that is not (`$5`, `5 USD`), or a budget set together with `LUCIDA_NO_LEDGER` or where the ledger has nowhere to live (no home or config directory — the ledger is where spend is counted), refuses every paid render with exit code `2` rather than being ignored. Free renders are never refused. The cap is enforced across concurrent MCP calls in one process, but not between separate `lucida` processes.
* **Dry runs:** `--dry-run` performs full pre-flight validation and cost calculation without dispatching requests or spending balance.
* **Ledger tracking:** Completed and pending operations are recorded to a JSON ledger adjacent to the configuration file. Inspected via `lucida history` or `lucida ops`. Set `LUCIDA_NO_LEDGER=1` to disable recording.

### Agent skill

Export the agent skill for compatible clients:

```console
lucida skill > ~/.claude/skills/lucida/SKILL.md
```

## Troubleshooting

* **Missing API keys in GUI clients:** GUI applications do not inherit shell exports. Set keys in the configuration file using `lucida config --set <KEY>`.
* **MCP tools not detected:** Restart Claude Code or Claude Desktop after running `lucida setup` so server definitions are reloaded.
* **ComfyUI connection failures:** Verify the server is running and accessible at `http://127.0.0.1:8188` or set `LUCIDA_COMFYUI_URL`.
* **ComfyUI 401 Unauthorized:** Set `LUCIDA_COMFYUI_AUTH` with appropriate HTTP credentials.
* **Private TLS failures:** Specify the path to your internal certificate authority bundle via `LUCIDA_COMFYUI_CA`.
* **Unrecognised configuration keys:** Check `lucida config` output. Unknown variables in the configuration file are ignored.

## License

[Apache-2.0](LICENSE)
