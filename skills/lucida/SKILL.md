---
name: lucida
description: Generating and editing images and video with Lucida — choosing a provider, iterating on a render, and the outcomes that differ from what was asked for. Use when creating image or video assets through the lucida MCP server or CLI, or when a render came back wrong and the next attempt needs to be different rather than repeated.
---

# Lucida

Lucida generates and edits images, and generates video, as an MCP server
(`generate_image`, `image_providers`, `start_video`, `check_video`,
`list_operations`) or a CLI
(`lucida generate|edit|video|check|ops|history|models|config`).

**This file deliberately contains no capability facts.** Which provider takes a
seed, a mask, a negative prompt or a given aspect ratio changes as providers ship
and Lucida adds them — so those live in one place that is always current:

- MCP: call `image_providers`.
- CLI: run `lucida models --provider <name>`.

Anything here that contradicts those is out of date; believe the probe.

## The one rule that changes how you work

**Nothing is silently dropped.** A parameter the chosen provider cannot honour
comes back as an error naming a provider that *can*, not a render that quietly
ignored you. This is unusual and it is worth relying on:

- Do not defensively strip parameters you are unsure about. Ask for what you
  want; if it cannot be honoured you get told, before anything is spent.
- Do not treat such an error as failure. It is a routing hint — the message
  names the provider to switch to.
- Do not pre-filter by consulting a table you remember. Let the call fail and
  read the answer, or probe first if you are about to spend money in a loop.

## Choosing a provider

Ask what the job actually requires, then probe. In rough order of how often it
decides the answer:

1. **Must the same picture be reproducible?** Then you need a provider with a
   seed, and you must record the seed you were given — every render reports the
   one it used, including when you did not pin one.
2. **Is this running unattended, or is someone waiting?** Providers differ by
   roughly two orders of magnitude in latency. One lane is local and slow enough
   that a caller will assume it has hung; the hosted ones return in seconds. If
   you start a slow render on someone's behalf, say so up front.
3. **Does it cost money?** Most providers bill per render; one is free at the
   margin because it runs locally. A retry loop against a paid provider spends
   real money each turn — decide the parameters before iterating, not during.
   Every render now reports its expected cost, so this is a number you can act
   on rather than a rule of thumb. If a budget is set, a render that would
   exceed it is refused before anything is sent; switch to the local lane rather
   than retrying, since retrying cannot succeed.
4. **Does the output need to be free of provenance marking, or carry it?**
   Providers differ, and Lucida reports what each render carried.

`--provider` picks one explicitly; otherwise it is inferred from the model id.

## Iterating on an image

Prompt quality dominates everything else here. Detailed prompts work
considerably better than terse ones — and terse prompts are also the ones most
likely to be refused by a safety filter, which is the opposite of most people's
intuition: a described scene has many renderings, a bare phrase has one.

When a render is close but wrong:

- **Wrong content, right composition** → `edit` the result rather than
  regenerating. Regenerating throws away what worked.
- **Right content, wrong framing** → regenerate with explicit geometry rather
  than editing, since an edit inherits the source's shape.
- **Wrong style entirely** → change the prompt, not the parameters. Sampler
  settings are a much weaker lever than wording.

Editing a picture repeatedly compounds artefacts. Past two or three rounds,
fold the accumulated intent into one prompt and generate fresh.

## Outcomes that differ from the request

These are the things that surprise callers. Each is reported by Lucida rather
than hidden, so read what comes back rather than assuming the request was met.

- **An edit is normalised to the resolution the model works at.** Aspect
  survives; exact pixel dimensions generally do not, and it can upscale as
  readily as downscale. If exact geometry matters, state it, and check the size
  reported back.
- **`lucida edit` overwrites its input by default.** Pass an explicit output
  path when the original matters. Via MCP, `generate_image` with
  `reference_images` writes wherever you tell it.
- **The written path is authoritative, not the one you asked for.** File
  extensions are corrected to match the bytes actually returned, so a request
  for `.png` may be written as `.jpg`. The CLI prints the real path on stdout
  alone, which is why `$(lucida generate …)` composes; the MCP result reports it
  too. Use what comes back when referencing the file afterwards.
- **What a mask guarantees differs by provider, so read the capability report
  before acting on one.** On some, the masked region is only where the change is
  *concentrated*, and pixels outside it move as well; on others Lucida
  guarantees they do not. Compositing the result back over the original is the
  remedy in the first case and a way to degrade an exact render in the second —
  so check rather than assume. The mask entry in the capability report states
  which kind you have.
- **A local render can take minutes and reports elapsed time while it works.**
  It has not hung.
- **A slow render does not block the server.** Other tool calls run alongside it
  — up to four at once — and cancelling a request actually stops the waiting.
  Cancellation does not refund a render already submitted to a paid provider: the
  charge is incurred when the provider starts work, not when the result is read.

## Video

Video is split into `start_video` and `check_video` because a render takes long
enough that one blocking call would likely hit a client timeout and abandon
something already paid for. Start, then poll.

The operation id is just a string, so a render started by an agent can be
collected later from the shell with `lucida check <operation>` — including after
the agent session ends. Hand the id back to the user when you start a render you
may not be around to finish.

**If you have lost an operation id, call `list_operations`.** Every started render
is recorded, so an id from an earlier session — yours or someone else's — is still
there. Do not start a second render because the first one's id is no longer in
your context; that pays twice for the same shot.

Video bills per second of output and is far more expensive than images. Confirm
before starting one unless you have been asked for it directly.

## When credentials fail

`lucida config` reports what the running process can see and where each setting
came from, never a value — so it is safe to include in a bug report, and it is
the first thing to run when a key "is set" but nothing works.

The failure worth recognising: a GUI-launched client inherits no shell
environment and passes that empty environment to any MCP server it spawns, so a
key exported in a shell profile is genuinely invisible. The config file exists
for exactly that case; `lucida config --set NAME` fills it in without the value
touching shell history.

If a setting is present in both the config file and the environment, the file
wins, and `lucida config` names the shadowed one.
