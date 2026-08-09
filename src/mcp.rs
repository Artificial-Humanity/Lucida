//! MCP server over stdio.
//!
//! The protocol is newline-delimited JSON-RPC 2.0, which is small enough that a
//! dependency would cost more than it saves. It also buys the single most useful
//! property for a stdio server: nothing reaches stdout unless this file puts it
//! there. In Python, one stray `print` in any transitive import corrupts the
//! stream and the failure looks like a mysterious handshake error.
//!
//! Diagnostics therefore go to stderr, which Claude Code captures as server logs.
//!
//! # Keeping the schema honest
//!
//! An agent reads a tool schema and believes it, which makes the schema the
//! sharpest constraint on the provider abstraction. Version 0.1 advertised
//! Google's ten named aspect ratios and its `1K`/`2K`/`4K` sizes as hard enums.
//! Those are not facts about image generation; they are facts about Google, and a
//! second provider makes them wrong.
//!
//! Two options were open: regenerate the schema per configured provider, or
//! publish one generic schema alongside a capabilities probe. This file does the
//! second, for a reason specific to how the tool is used: a single server here
//! serves *both* providers, chosen per call from the model id, so there is no one
//! "configured provider" whose schema could be published. Instead:
//!
//! - Parameter descriptions name which providers honour them, rather than
//!   pretending the union is universally available.
//! - `image_providers` reports live capabilities, so an agent can check rather
//!   than guess.
//! - A parameter the chosen provider cannot honour is a loud error naming one
//!   that can — never a silent drop. That error comes back as tool content, so
//!   the model can read it and retry rather than simply failing.

use crate::bfl;
use crate::comfy;
use crate::openai;
use crate::stability;
use crate::genai::{self, DEFAULT_MODEL};
use crate::provider::{
    Aspect, AspectSupport, Backend, ImageProvider, ImageRequest, Size, capabilities_for,
    infer_backend,
};
use crate::provider::{VideoBackend, video_capabilities_for};
use crate::video::{DEFAULT_VIDEO_MODEL, VideoRequest, VideoStatus};
use crate::cancel;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// How many tool calls can be in flight at once.
///
/// Four rather than one, which is what this server effectively had, and rather
/// than unbounded, which would let a client with a loop in it start a hundred
/// paid renders. Four is enough that an agent generating a set of assets is not
/// serialised, and small enough that the bill for a runaway client is bounded by
/// something other than its own good behaviour.
const WORKERS: usize = 4;

/// Requests currently being worked on, so a cancellation can find one.
type InFlight = Arc<Mutex<HashMap<String, cancel::Token>>>;

/// The output stream, held by whichever worker is writing a line.
///
/// Every response goes through this. Two workers finishing at once would
/// otherwise interleave their JSON mid-line and corrupt the stream for good —
/// the protocol has no way to resynchronise.
///
/// Generic only so tests can supply a `Vec<u8>`; in production it is always
/// stdout.
type Out<W> = Arc<Mutex<W>>;

struct Job {
    id: Value,
    params: Value,
    token: cancel::Token,
}

/// The stdio server.
///
/// # Why this is not one loop any more
///
/// It was, and the shape was the problem: `dispatch` ran to completion before
/// the next line was read. A ComfyUI render can hold that for its full
/// 1800-second deadline, and for all of it the server was deaf. A client's
/// `ping` went unanswered, which clients using ping as a liveness probe read as
/// a dead server; a second tool call queued behind the first with no indication
/// it had even been received; and `notifications/cancelled` could not be honoured
/// at all, so a user's cancel was a no-op that kept billing.
///
/// So: the reading thread only ever reads. It answers `initialize`, `ping` and
/// `tools/list` itself — they are pure and instant, and liveness must never
/// depend on a worker being free — and hands `tools/call` to a small pool.
/// Responses are matched by id, which JSON-RPC allows to come back in any order.
pub fn serve() -> Result<()> {
    eprintln!("lucida MCP server ready (default image model: {DEFAULT_MODEL})");
    let stdin = std::io::stdin();
    let out = Arc::new(Mutex::new(std::io::stdout()));
    run(stdin.lock(), out, call_tool)
}

/// The loop itself, over any reader and writer.
///
/// Split from [`serve`] so the property this whole change exists for — a `ping`
/// answered while a render is still going — can be asserted rather than
/// described. `handle` is the tool dispatcher, which in production is always
/// `call_tool`; a test supplies one that is deliberately slow, because there is
/// no other way to hold a worker open on demand.
fn run<R, W, F>(reader: R, out: Out<W>, handle: F) -> Result<()>
where
    R: BufRead,
    W: Write + Send + 'static,
    F: Fn(&Value) -> Result<Value> + Send + Clone + 'static,
{
    let in_flight: InFlight = Arc::new(Mutex::new(HashMap::new()));

    let (sender, receiver) = mpsc::channel::<Job>();
    let receiver = Arc::new(Mutex::new(receiver));

    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let receiver = Arc::clone(&receiver);
            let out = Arc::clone(&out);
            let in_flight = Arc::clone(&in_flight);
            let handle = handle.clone();
            std::thread::spawn(move || {
                loop {
                    // Taken in a statement of its own, and this is load-bearing
                    // rather than stylistic: a `while let Ok(job) =
                    // receiver.lock().unwrap().recv()` keeps the scrutinee's
                    // temporaries — the MutexGuard among them — alive for the
                    // whole body, so every worker would hold the queue lock for
                    // the entire render and four workers would behave exactly
                    // like the one loop this replaces. Written that way first;
                    // `tool_calls_run_concurrently_rather_than_queueing` failed
                    // with "only 1 of 4 calls ran at once".
                    //
                    // Ending the statement here drops the guard, so a worker
                    // holds the lock only while *waiting*, never while working.
                    let job = receiver.lock().unwrap().recv();
                    let Ok(job) = job else { break };

                    let key = job.id.to_string();
                    let result = cancel::with(job.token, || guarded(&handle, &job.params));
                    in_flight.lock().unwrap().remove(&key);
                    respond(&out, &job.id, result);
                }
            })
        })
        .collect();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipping unparseable line: {e}");
                continue;
            }
        };

        let method = request["method"].as_str().unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // Handled before the notification check below, because a cancellation
        // *is* a notification — it carries no id of its own, only the id of the
        // request it is cancelling. That is precisely why it used to be
        // unreachable: the early return dropped it with everything else that had
        // no id, so the one message whose whole purpose is to stop a paid render
        // was the one message guaranteed to be ignored.
        if method == "notifications/cancelled" {
            let target = params["requestId"].to_string();
            if let Some(token) = in_flight.lock().unwrap().get(&target) {
                eprintln!("cancelling request {target}");
                token.cancel();
            }
            continue;
        }

        // No other id means a notification: act on it, but never reply. Replying
        // to a notification is a protocol violation some clients treat as fatal.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };

        if method == "tools/call" {
            let token = cancel::Token::new();
            in_flight
                .lock()
                .unwrap()
                .insert(id.to_string(), token.clone());
            // Send cannot fail while a worker is alive, and if the pool has gone
            // the process is on its way down anyway.
            let _ = sender.send(Job { id, params, token });
            continue;
        }

        respond(&out, &id, dispatch(method, &params));
    }

    // Stdin closed: the client has gone. Ask everything in flight to stop, then
    // let the workers finish the line they are writing. Cancellation is
    // cooperative, so this is quick for anything in a poll loop — and for a
    // single blocking render it waits, which is right: that call is already paid
    // for and its result may still be worth writing to disk.
    for token in in_flight.lock().unwrap().values() {
        token.cancel();
    }
    drop(sender);
    for worker in workers {
        let _ = worker.join();
    }

    Ok(())
}

/// Runs a tool call, turning a panic into an error reply.
///
/// A panicking worker used to be survivable, because there was no worker: the
/// process died and the client saw its server exit. With a pool, an unwinding
/// thread is *quietly* lost — the pool shrinks by one, the request that caused
/// it never gets a reply, and after four the server accepts calls and answers
/// none of them, forever, while still passing `ping`. A hang with a healthy
/// liveness probe is the worst shape a failure can take, so the panic is caught
/// and reported as what it is.
fn guarded<F>(handle: &F, params: &Value) -> Result<Value>
where
    F: Fn(&Value) -> Result<Value>,
{
    let called = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(params)));
    called.unwrap_or_else(|_| {
        anyhow::bail!(
            "the tool panicked. This is a bug in lucida — the server is still \
             running and other tools are unaffected. The panic message is in the \
             server's stderr log."
        )
    })
}

/// Writes one JSON-RPC reply, whole, under the output lock.
fn respond<W: Write>(out: &Out<W>, id: &Value, result: Result<Value>) {
    let response = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(e) => {
            // -32601 is "method not found", which clients may probe for
            // (resources/list, prompts/list); everything else is -32603.
            let code = if e.to_string().starts_with("unknown method") {
                -32601
            } else {
                -32603
            };
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": e.to_string() }
            })
        }
    };

    let mut out = out.lock().unwrap();
    // A failed write means the client's pipe is gone. Nothing useful is left to
    // do about it here, and panicking on a worker would take the process down
    // while other renders are still finishing.
    let _ = writeln!(out, "{response}");
    let _ = out.flush();
}

fn dispatch(method: &str, params: &Value) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "lucida", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": [
                image_schema(),
                providers_schema(),
                start_video_schema(),
                check_video_schema(),
                video_providers_schema(),
                list_operations_schema(),
            ]
        })),
        "tools/call" => call_tool(params),
        other => anyhow::bail!("unknown method: {other}"),
    }
}

/// Describes every provider from its own declared capabilities.
///
/// Generated rather than written, because the hand-written version drifted: by
/// the fourth provider it opened with "Two providers are available", listed
/// three, and omitted the fourth entirely, which existed only in the enum. An
/// agent reads a schema and believes it, so a claim nobody can forget to update
/// is worth more than a better-phrased one that rots.
fn provider_summary() -> String {
    Backend::ALL
        .iter()
        .map(|backend| {
            // The provider's own default, not an empty string: capabilities can
            // depend on the model, and BFL with no model reports no editing.
            let caps = capabilities_for(*backend, backend.default_model());
            let mut notes: Vec<String> = Vec::new();
            if caps.seed {
                notes.push("seed".into());
            }
            if caps.negative_prompt {
                notes.push("negative prompt".into());
            }
            if caps.references {
                notes.push("editing".into());
            }
            if caps.steps {
                notes.push("steps/guidance".into());
            }
            if !caps.size {
                notes.push("NO size control".into());
            }
            // Masking carries its kind, because the kind is what decides between
            // the providers offering it — and because the tagline printed right
            // beside this had openai as "the ONLY provider that can mask" for a
            // release after the local lane started masking better.
            if caps.mask.accepted() {
                notes.push(format!("masks, {}", caps.mask.kind()));
            }
            format!(
                "- {}{}: {} [{}] Output carries: {}.",
                backend.name(),
                if *backend == Backend::Google { " (default)" } else { "" },
                caps.tagline,
                notes.join(", "),
                caps.provenance.describe()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The providers for which `predicate` holds, as prose.
///
/// Generated because these lists are exactly what rots: "google, comfyui and
/// bfl" was correct for three providers and wrong the moment a fourth could
/// edit. A test asserts the editing list against the capabilities, and this is
/// how it stays true rather than merely being corrected each time.
fn providers_where(predicate: fn(&crate::provider::Capabilities) -> bool) -> String {
    let names: Vec<&str> = Backend::ALL
        .iter()
        .filter(|b| predicate(&capabilities_for(**b, b.default_model())))
        .map(|b| b.name())
        .collect();
    crate::provider::join_and(&names)
}

fn image_schema() -> Value {
    json!({
        "name": "generate_image",
        "description": format!(
            "Generate an image and write it to disk. Returns the path written.\n\n\
             {} providers are available and the choice matters — cost, speed, \
             what you can ask for, and what ends up embedded in the file all \
             differ:\n{}\n\n\
             The provider is inferred from the model id; pass `provider` to be \
             explicit. Not every parameter works on every provider, and the ones \
             that do not are a hard error naming one that does — never a silent \
             drop. Call image_providers for live capabilities and which are \
             actually reachable.\n\n\
             Pass reference_images to edit an existing picture ({}); pass mask as \
             well to concentrate the change on part of one ({}, and what that \
             guarantees differs per provider — see the mask parameter).",
            Backend::ALL.len(),
            provider_summary(),
            providers_where(|c| c.references),
            providers_where(|c| c.mask.accepted())
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "What to draw. Detailed prompts work considerably better than terse ones."
                },
                "output_path": {
                    "type": "string",
                    "description": "Where to write the image. Relative paths resolve against the current working directory. Parent directories are created."
                },
                "provider": {
                    "type": "string",
                    "enum": ["google", "comfyui", "bfl", "stability", "openai"],
                    "description": "Which backend to use. Inferred from `model` when omitted, defaulting to google."
                },
                "model": {
                    "type": "string",
                    "description": format!(
                        "Model id or alias. Defaults per provider: {}.",
                        // Generated: the hand-written list omitted openai.
                        Backend::ALL
                            .iter()
                            .map(|b| format!("{} → {}", b.name(), b.default_model()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                "aspect_ratio": {
                    "type": "string",
                    // Deliberately not an enum: the providers with named ratios
                    // disagree about which, and the others take any ratio at all.
                    "description": format!(
                        "W:H, e.g. 16:9. google accepts only: {}. stability accepts a \
                         DIFFERENT nine: {}. comfyui and bfl accept any ratio; on \
                         openai, gpt-image-2 takes any ratio and its siblings only \
                         1:1, 2:3 and 3:2.",
                        genai::ASPECT_RATIOS.join(", "),
                        crate::stability::ASPECT_RATIOS.join(", ")
                    )
                },
                "size": {
                    "type": "string",
                    "description": "Long edge in pixels, or a tier (1K, 2K, 4K). google rounds to a tier; comfyui and bfl use the number; openai's gpt-image-2 scales its pixel budget by it. NOT supported by stability or the other openai models, which render fixed sizes — passing it there is an error."
                },
                "negative_prompt": {
                    "type": "string",
                    "description": "What to keep out of the picture. comfyui and stability only — google's image models and every FLUX endpoint lack the concept, so passing it there is an error rather than a no-op."
                },
                "seed": {
                    "type": "integer",
                    "description": "Renders the same image again. comfyui, bfl and stability; google and openai expose none, so results there cannot be reproduced. comfyui is verified pixel-identical across runs."
                },
                "steps": {
                    "type": "integer",
                    "description": "Sampling steps. comfyui, and on bfl only flux-2-flex and flux-dev."
                },
                "guidance": {
                    "type": "number",
                    "description": "How closely to follow the prompt. comfyui, and on bfl only flux-2-flex and flux-dev."
                },
                "workflow": {
                    "type": "string",
                    "description": "Path to a ComfyUI workflow in API format, rendered instead of the built-in graph. comfyui only. Tokens %prompt% %negative% %seed% %width% %height% %steps% %cfg% mark where values go; a token the file omits means that option cannot be honoured and is refused rather than dropped. Cannot be combined with `model` or `reference_images` — the workflow names its own checkpoints and inputs."
                },
                "mask": {
                    "type": "string",
                    // Both the provider list and the semantics are generated. The
                    // semantics used to be a hand-written "ADVISORY, not binding"
                    // beside a computed list, which sent agents to re-composite a
                    // render that had already come back pixel-exact.
                    "description": format!(
                        "Path to a PNG concentrating an edit on part of the image: \
                         its TRANSPARENT pixels are what changes. Supported by {}, \
                         and requires reference_images. What it guarantees depends \
                         on the provider. {}",
                        providers_where(|c| c.mask.accepted()),
                        crate::provider::mask_semantics()
                    )
                },
                "reference_images": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": format!(
                        "Paths to existing images to condition on, for editing or \
                         style matching. Supported by {}. On comfyui the result \
                         keeps the first image's aspect ratio unless aspect_ratio \
                         or size is given.",
                        providers_where(|c| c.references)
                    )
                }
            },
            "required": ["prompt", "output_path"]
        }
    })
}

/// The capabilities probe.
///
/// Cheap to call, and it answers the question the generic schema deliberately
/// leaves open: what can *this* provider, on *this* machine, actually be asked
/// for right now. It probes rather than asserts, so an unreachable ComfyUI or a
/// missing API key shows up here instead of halfway through a render.
fn providers_schema() -> Value {
    json!({
        "name": "image_providers",
        "description": concat!(
            "Report which image providers are reachable and what each supports — ",
            "aspect ratios, seed, negative prompt, reference images, and what ",
            "provenance marking its output carries. Spends nothing. Call this ",
            "before generate_image when the choice of provider matters, or after ",
            "a parameter is rejected."
        ),
        "inputSchema": { "type": "object", "properties": {} }
    })
}

/// Video is split into start and check because a Veo render takes minutes —
/// long enough that a single blocking tool call would likely hit the client's
/// timeout and lose a render that was already paid for.
/// Describes every video provider from its own declared capabilities.
///
/// Generated for the same reason the image summary is, and now for the same
/// reason *in fact*: this prose said "Google only" and "output carries a SynthID
/// watermark and a C2PA manifest", both of which stopped being true the moment a
/// second video provider landed. Runway's provenance is unverified and its
/// durations are a range rather than three fixed lengths.
fn video_provider_summary() -> String {
    VideoBackend::ALL
        .iter()
        .map(|backend| {
            let caps = video_capabilities_for(*backend, backend.default_model());
            let mut notes: Vec<String> = vec![caps.duration.describe()];
            if caps.seed {
                notes.push("seed".into());
            }
            if caps.negative_prompt {
                notes.push("negative prompt".into());
            }
            if !caps.text_to_video {
                notes.push("needs a still to animate".into());
            }
            format!(
                "- {}{}: {} [{}] Output carries: {}.",
                backend.name(),
                if *backend == VideoBackend::Google { " (default)" } else { "" },
                caps.tagline,
                notes.join(", "),
                caps.provenance.describe()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn start_video_schema() -> Value {
    json!({
        "name": "start_video",
        "description": format!(
            "Begin rendering a video. Returns immediately with an operation id; \
             the render itself takes 1-3 minutes. Poll it with check_video.\n\n\
             Video is billed per SECOND of output and costs considerably more \
             than an image, so confirm with the user before calling this.\n\n\
             {} providers are available:\n{}\n\n\
             The provider is inferred from the model id; pass `provider` to be \
             explicit. A parameter the chosen provider cannot honour is a hard \
             error naming what it does offer — never a silent drop.",
            VideoBackend::ALL.len(),
            video_provider_summary()
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "What to film, including any camera movement." },
                "image": { "type": "string", "description": "Optional path to a still image to animate instead of generating from text alone. Required by runway's gen4-turbo, which cannot start from text." },
                "provider": {
                    "type": "string",
                    "enum": ["google", "runway", "kling"],
                    "description": "Which backend to use. Inferred from `model` when omitted, defaulting to google."
                },
                "mode": {
                    "type": "string",
                    "description": "Quality tier, where the provider has one. kling takes std, pro or master; the others have none and passing one there is an error."
                },
                "aspect_ratio": {
                    "type": "string",
                    // Deliberately not an enum: google names `16:9`, runway
                    // names the pixel pair `1280:720`, and both are accepted.
                    "description": format!(
                        "W:H. google accepts {}. runway names geometry in pixels — {} \
                         — and a simplified ratio is mapped to the pair that is that \
                         ratio, so 16:9 works on both.",
                        describe_aspect(video_capabilities_for(VideoBackend::Google, "").aspect),
                        describe_aspect(video_capabilities_for(VideoBackend::Runway, crate::runway::DEFAULT_MODEL).aspect)
                    )
                },
                "duration": {
                    "type": "integer",
                    "description": format!(
                        "Seconds of output, and the parameter that decides the bill. \
                         google: {}. runway: {}.",
                        video_capabilities_for(VideoBackend::Google, "").duration.describe(),
                        video_capabilities_for(VideoBackend::Runway, crate::runway::DEFAULT_MODEL).duration.describe()
                    )
                },
                "resolution": { "type": "string", "description": "e.g. 720p or 1080p. google only; on runway the aspect ratio decides the pixel count." },
                "negative_prompt": { "type": "string", "description": "What to keep out of the shot. google only, and not on veo-lite." },
                "seed": { "type": "integer", "description": "Renders the same video again. runway only; google exposes none." },
                "model": {
                    "type": "string",
                    "description": format!(
                        "Model id or alias. Defaults per provider: {}.",
                        VideoBackend::ALL
                            .iter()
                            .map(|b| format!("{} → {}", b.name(), b.default_model()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            },
            "required": ["prompt"]
        }
    })
}

/// One line describing an aspect-ratio capability, shared by both schemas.
fn describe_aspect(support: AspectSupport) -> String {
    match support {
        AspectSupport::Named(ratios) => ratios.join(", "),
        AspectSupport::Free { multiple_of } => format!("any ratio, rounded to {multiple_of} pixels"),
    }
}

fn check_video_schema() -> Value {
    json!({
        "name": "check_video",
        "description": concat!(
            "Check a render started by start_video. If it is still working, says so ",
            "— wait several seconds before checking again rather than polling tightly. ",
            "If it has finished, downloads the video to output_path and returns the ",
            "path written."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "operation": { "type": "string", "description": "The operation id returned by start_video." },
                "provider": { "type": "string", "enum": ["google", "runway", "kling"], "description": "Which provider started it. Inferred from the id's shape when omitted." },
                "output_path": { "type": "string", "description": "Where to write the finished video. An .mp4 extension is applied if missing." }
            },
            "required": ["operation", "output_path"]
        }
    })
}

/// The video twin of `image_providers`.
///
/// Owed from the moment video gained a second provider, and missing until
/// 2026-08-09: the shipped skill tells agents that capability facts live in the
/// probe rather than in prose, and for video there was no probe to consult. The
/// CLI could answer (`lucida models --provider <name>`); the surface the skill is
/// actually written for could not.
///
/// Reports the static capability table always, and the credential check only
/// where a provider has a free endpoint for one — same rule as the image probe,
/// for the same reason: what a provider supports is not a fact about your keys.
fn video_providers_schema() -> Value {
    json!({
        "name": "video_providers",
        "description": concat!(
            "Report which video providers are reachable and what each supports — ",
            "aspect ratios, clip durations, quality tiers, whether it can start ",
            "from text or needs a still, and what provenance marking its output ",
            "carries. Spends nothing. Call this before start_video when the choice ",
            "of provider or model matters, or after a parameter is rejected."
        ),
        "inputSchema": { "type": "object", "properties": {} }
    })
}

fn describe_video_providers() -> String {
    let mut out = String::new();

    for backend in VideoBackend::ALL.iter().copied() {
        out.push_str(&format!("## {}\n", backend.name()));

        let caps = video_capabilities_for(backend, backend.default_model());

        // The reachability check, where the provider offers one for free. Veo
        // has no such endpoint — its models are fixed at release and any probe
        // would be a paid render — so it reports as configured or not, from the
        // credential alone.
        match backend {
            VideoBackend::Google => match crate::genai::Client::from_env() {
                Ok(_) => out.push_str("configured\n"),
                Err(e) => out.push_str(&format!("NOT usable: {e:#}\n")),
            },
            VideoBackend::Runway => match crate::runway::Client::from_env().and_then(|c| c.credits()) {
                Ok(credits) => out.push_str(&format!("reachable — {credits} credit(s) remaining\n")),
                Err(e) => out.push_str(&format!("NOT usable: {e:#}\n")),
            },
            VideoBackend::Kling => match crate::kling::Client::from_env().and_then(|c| c.credits()) {
                Ok(units) => out.push_str(&format!("reachable — {units} unit(s) remaining\n")),
                Err(e) => out.push_str(&format!("NOT usable: {e:#}\n")),
            },
        }

        let models: Vec<&str> = match backend {
            VideoBackend::Google => crate::video::VIDEO_ALIASES.iter().map(|(_, id)| *id).collect(),
            VideoBackend::Runway => crate::runway::MODELS.to_vec(),
            VideoBackend::Kling => crate::kling::MODELS.to_vec(),
        };
        let mut unique: Vec<&str> = Vec::new();
        for model in models {
            if !unique.contains(&model) {
                unique.push(model);
            }
        }
        out.push_str(&format!(
            "models: {} (default {})\n",
            unique.join(", "),
            backend.default_model()
        ));

        out.push_str(&format!(
            "aspect ratio: {}\n\
             duration: {}  |  quality tiers: {}\n\
             from a still: {}  |  from text alone: {}\n\
             negative prompt: {}  |  resolution: {}  |  seed: {}\n\
             output carries: {}\n\n",
            describe_aspect(caps.aspect),
            caps.duration.describe(),
            if caps.modes.is_empty() { "none".to_string() } else { caps.modes.join(", ") },
            caps.image_to_video,
            caps.text_to_video,
            caps.negative_prompt,
            caps.resolution,
            caps.seed,
            caps.provenance.describe()
        ));
    }

    out
}

/// The recovery surface for a render whose session ended.
///
/// New in this version, and a deliberate change to the public MCP surface — this
/// server is registered at user scope, so every agent session on the machine
/// sees it. It earns that: `start_video` hands back an operation id and the
/// render outlives the conversation, so without somewhere to read the id back
/// from, a session that ends mid-render leaves a paid render permanently
/// unreachable. The id was already the only way in; it just had nowhere to live.
fn list_operations_schema() -> Value {
    json!({
        "name": "list_operations",
        "description": concat!(
            "List video renders that were started and never collected, with the ",
            "operation id needed to finish each one. Use this when a render was ",
            "started earlier — by you, by a previous session, or from the shell — ",
            "and its id is no longer to hand. Spends nothing; reads a local file."
        ),
        "inputSchema": { "type": "object", "properties": {} }
    })
}

fn list_operations() -> Result<String> {
    if crate::ledger::disabled() {
        return Ok(
            "The render ledger is switched off (LUCIDA_NO_LEDGER is set), so no \
             operations were recorded. A render's id is reported by start_video \
             at the moment it begins."
                .to_string(),
        );
    }

    let open = crate::ledger::outstanding();
    if open.is_empty() {
        return Ok("No video renders are waiting to be collected.".to_string());
    }

    let mut out = String::from("Video renders started and not yet collected:\n\n");
    for entry in &open {
        out.push_str(&format!(
            "- operation: {}\n  started: {}\n  model: {}\n  prompt: {}\n",
            entry["operation"].as_str().unwrap_or("?"),
            crate::clock::stamp(entry["at"].as_i64().unwrap_or(0)),
            entry["model"].as_str().unwrap_or("?"),
            entry["prompt"].as_str().unwrap_or(""),
        ));
    }
    out.push_str("\nPass an operation id to check_video with an output path to collect it.");
    Ok(out)
}

/// Every tool this server handles.
///
/// Named once so `tools/list` and `tools/call` cannot drift apart — advertising
/// a tool that dispatch does not handle is the kind of bug an agent discovers
/// for you, in production, having already told the user it would work.
const TOOL_NAMES: &[&str] = &[
    "generate_image",
    "image_providers",
    "start_video",
    "check_video",
    "video_providers",
    "list_operations",
];

fn call_tool(params: &Value) -> Result<Value> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = &params["arguments"];

    if !TOOL_NAMES.contains(&name) {
        anyhow::bail!(
            "unknown tool: {name}. This server offers: {}",
            TOOL_NAMES.join(", ")
        );
    }

    match name {
        "generate_image" => wrap(generate_image(args)),
        "image_providers" => wrap(Ok(describe_providers())),
        "start_video" => wrap(start_video(args)),
        "check_video" => wrap(check_video(args)),
        "video_providers" => wrap(Ok(describe_video_providers())),
        "list_operations" => wrap(list_operations()),
        // Unreachable while the guard above and this match agree, which is what
        // the constant is for.
        other => anyhow::bail!("`{other}` is advertised but not implemented"),
    }
}

/// Errors are returned as isError content rather than as JSON-RPC errors, so the
/// model sees the message and can act on it (fix the prompt, pick another
/// provider, wait longer) instead of the call simply failing.
fn wrap(result: Result<String>) -> Result<Value> {
    match result {
        Ok(text) => Ok(json!({ "content": [{ "type": "text", "text": text }] })),
        Err(e) => Ok(json!({
            "content": [{ "type": "text", "text": format!("{e:#}") }],
            "isError": true
        })),
    }
}

fn open(backend: Backend) -> Result<Box<dyn ImageProvider>> {
    Ok(match backend {
        Backend::Google => Box::new(genai::Client::from_env()?),
        Backend::ComfyUi => Box::new(comfy::Client::from_env()?),
        Backend::Bfl => Box::new(bfl::Client::from_env()?),
        Backend::Stability => Box::new(stability::Client::from_env()?),
        Backend::OpenAi => Box::new(openai::Client::from_env()?),
    })
}

/// Reads an optional argument, refusing one of the wrong type.
///
/// `Value::as_str` and its siblings answer `None` for a value of the wrong type
/// exactly as they do for a missing one — and everywhere below, `None` means
/// "not requested". That collapse was this server's one silent drop, and its
/// worst case was not a small one: `"reference_images": "photo.png"`, a string
/// where an array belongs, turned an *edit* into a fresh generation and reported
/// it as a success.
///
/// So absence and wrongness are separated here. Missing or null is `Ok(None)`;
/// anything present but unusable is an error naming the parameter, what arrived,
/// and what belongs there — the same voice as a capability refusal, and for the
/// same reason: the model reads it and fixes the call, rather than believing a
/// lie about what it asked for.
fn optional<'a, T>(
    args: &'a Value,
    key: &str,
    expected: &str,
    extract: impl Fn(&'a Value) -> Option<T>,
) -> Result<Option<T>> {
    match &args[key] {
        Value::Null => Ok(None),
        present => match extract(present) {
            Some(value) => Ok(Some(value)),
            None => anyhow::bail!(
                "`{key}` must be {expected}, but {} was given. Pass it as \
                 {expected}, or leave it out — it was refused rather than dropped, \
                 so nothing has been rendered.",
                describe(present)
            ),
        },
    }
}

/// What arrived, for an error message. The value is quoted rather than merely
/// typed, because "a string was given" is far less useful to whoever has to fix
/// the call than seeing the string itself.
fn describe(value: &Value) -> String {
    match value {
        Value::String(text) => format!("the string {text:?}"),
        Value::Number(number) => format!("the number {number}"),
        Value::Bool(flag) => format!("the boolean {flag}"),
        Value::Array(items) => format!("an array of {} item(s)", items.len()),
        Value::Object(_) => "an object".to_string(),
        Value::Null => "null".to_string(),
    }
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    optional(args, key, "a string", Value::as_str)
}

fn opt_string(args: &Value, key: &str) -> Result<Option<String>> {
    Ok(opt_str(args, key)?.map(str::to_string))
}

/// A seed or a step count. Rejects a negative or fractional number here rather
/// than letting `as_u64` quietly answer `None` for it.
fn opt_u64(args: &Value, key: &str) -> Result<Option<u64>> {
    optional(args, key, "a whole number, zero or above", Value::as_u64)
}

fn opt_f64(args: &Value, key: &str) -> Result<Option<f64>> {
    optional(args, key, "a number", Value::as_f64)
}

/// The elements are checked as well as the container: `["a.png", 3]` names the
/// offending index rather than dropping it, since a dropped reference image is
/// the same silent edit-becomes-generation failure one level down.
fn opt_str_array(args: &Value, key: &str) -> Result<Option<Vec<String>>> {
    let Some(items) = optional(args, key, "an array of strings", Value::as_array)? else {
        return Ok(None);
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                anyhow::anyhow!(
                    "`{key}[{index}]` must be a string, but {} was given. Every \
                     entry is a path to an existing file.",
                    describe(item)
                )
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    opt_str(args, key)?.ok_or_else(|| anyhow::anyhow!("`{key}` is required"))
}

fn generate_image(args: &Value) -> Result<String> {
    let prompt = req_str(args, "prompt")?;
    let output_path = req_str(args, "output_path")?;
    let workflow = opt_str(args, "workflow")?;
    let requested_model = opt_str(args, "model")?;

    // A supplied workflow names its own checkpoints, so an explicit model has
    // nowhere to go. Refused here rather than in the provider because by the
    // time the request reaches comfyui the default model has been filled in
    // and an explicit one is indistinguishable from it.
    if workflow.is_some() && requested_model.is_some() {
        anyhow::bail!(
            "`workflow` and `model` cannot be combined: a supplied workflow \
             names its own checkpoints, so there is nowhere to put a model id. \
             Name the model inside the workflow file, or drop `workflow` to \
             use the built-in graph."
        );
    }

    let backend = match opt_str(args, "provider")? {
        Some(name) => Backend::parse(name)?,
        None => match requested_model {
            Some(model) => infer_backend(model),
            None => Backend::Google,
        },
    };

    let model = requested_model
        .unwrap_or_else(|| backend.default_model())
        .to_string();

    let request = ImageRequest {
        prompt: prompt.to_string(),
        model,
        aspect: opt_str(args, "aspect_ratio")?.map(Aspect::parse).transpose()?,
        size: opt_str(args, "size")?.map(Size::parse).transpose()?,
        references: opt_str_array(args, "reference_images")?.unwrap_or_default(),
        negative_prompt: opt_string(args, "negative_prompt")?,
        mask: opt_string(args, "mask")?,
        workflow: workflow.map(str::to_string),
        seed: opt_u64(args, "seed")?,
        // try_from rather than `as`: a pathological value would wrap silently
        // into a small, plausible step count instead of erroring.
        steps: opt_u64(args, "steps")?
            .map(|n| {
                u32::try_from(n)
                    .map_err(|_| anyhow::anyhow!("`steps` is {n}, which is not a step count"))
            })
            .transpose()?,
        guidance: opt_f64(args, "guidance")?.map(|n| n as f32),
    };

    // The whole point of the abstraction, from an agent's perspective: a
    // parameter this provider cannot honour stops here, with a message naming one
    // that can, rather than being dropped on the way to the API. Checked before a
    // client exists, so a missing credential never masks the real objection.
    let caps = capabilities_for(backend, &request.model);
    caps.check(&request)?;

    let price = crate::spend::price_for(backend, &request.model);
    crate::spend::check(price, "render")?;

    let provider = open(backend)?;
    let image = provider.generate(&request)?;

    // Providers pick the output format themselves, so the requested extension may
    // not match the bytes. Correct it and say so, rather than handing back a file
    // whose name lies about its contents.
    let requested = std::path::Path::new(output_path);
    let destination = crate::correct_extension(requested, &image.mime_type);
    let renamed = destination != requested;
    let written = crate::write_image(&destination, &image.bytes)?;
    crate::ledger::image(
        caps.provider,
        &request.model,
        &request.prompt,
        &written.to_string_lossy(),
        image.seed,
        price.against_budget(),
    );

    // The dimensions are stated because they are not always the ones requested:
    // an edit on comfyui normalizes to roughly a megapixel, so the result can
    // differ from both the request and the source.
    let size = match crate::image_dimensions(&image.bytes, &image.mime_type) {
        Some((w, h)) => format!("{w}x{h}, "),
        None => String::new(),
    };
    let mut text = format!(
        "Wrote {} ({size}{} KB, {}) via {}.",
        written.display(),
        image.bytes.len() / 1024,
        image.mime_type,
        caps.provider
    );
    if renamed {
        text.push_str(&format!(
            "\n\nNote: the provider returned {}, so the extension was corrected \
             (requested {}). Use the path above, not the requested one.",
            image.mime_type,
            requested.display()
        ));
    }
    if let Some(seed) = image.seed {
        text.push_str(&format!(
            "\n\nSeed {seed}. Pass this as `seed` with the same prompt and model to \
             render it again."
        ));
    }
    text.push_str(&format!(
        "\n\nProvenance: {}.",
        caps.provenance.describe()
    ));
    // In the result rather than only on stderr, because the caller that needs
    // to act on it is the model reading this text — and "decide parameters
    // before iterating" is advice it can only follow if it knows the number.
    if price != crate::spend::Price::Free {
        text.push_str(&format!("\n\nCost: {}.", price.describe()));
    }
    if let Some(commentary) = &image.commentary
        && !commentary.is_empty()
    {
        text.push_str(&format!("\n\nModel commentary: {commentary}"));
    }
    Ok(text)
}

/// Probes each provider and reports what it can do, or why it cannot be used.
///
/// A provider that cannot be opened or reached is reported as such rather than
/// omitted — "google is unavailable because no API key is set" is a far more
/// useful answer than a list that quietly has one entry.
fn describe_providers() -> String {
    let mut out = String::new();

    for backend in Backend::ALL.iter().copied() {
        out.push_str(&format!("## {}\n", backend.name()));

        // Read before anything is opened, and printed whatever happens. This
        // used to `continue` on a missing key, so an agent asking what a
        // provider supports got nothing back but "unavailable" — and the fix it
        // needed (pick a provider that has a seed) was in the table it had just
        // been denied. `capabilities_for` is pure, which `provider.rs` says in
        // its own doc comment while this code did the opposite.
        let caps = capabilities_for(backend, backend.default_model());

        match open(backend) {
            Err(e) => out.push_str(&format!(
                "NOT usable: {e:#}\n(what it supports is listed anyway — that does \
                 not depend on a credential)\n"
            )),
            Ok(provider) => match provider.list_models() {
                Ok(models) if models.is_empty() => {
                    out.push_str("reachable, but reports no models\n")
                }
                Ok(models) => {
                    out.push_str(&format!("reachable — {} model(s)\n", models.len()));
                    for model in models.iter().take(12) {
                        out.push_str(&format!("  {model}\n"));
                    }
                    if models.len() > 12 {
                        out.push_str(&format!("  … and {} more\n", models.len() - 12));
                    }
                }
                Err(e) => out.push_str(&format!("NOT reachable: {e:#}\n")),
            },
        }

        let aspect = match caps.aspect {
            AspectSupport::Named(ratios) => ratios.join(", "),
            AspectSupport::Free { multiple_of } => {
                format!("any ratio, rounded to {multiple_of} pixels")
            }
        };
        out.push_str(&format!(
            "aspect ratio: {aspect}\n\
             seed: {}  |  negative prompt: {}  |  reference images: {}\n\
             size: {}  |  mask: {}  |  own workflow: {}\n\
             steps: {}  |  guidance: {}\n\
             output carries: {}\n\n",
            caps.seed,
            caps.negative_prompt,
            caps.references,
            caps.size,
            caps.mask.describe(),
            caps.workflow,
            caps.steps,
            caps.guidance,
            caps.provenance.describe()
        ));
    }

    out
}

fn start_video(args: &Value) -> Result<String> {
    let prompt = req_str(args, "prompt")?;

    let request = VideoRequest {
        prompt: prompt.to_string(),
        model: opt_str(args, "model")?.unwrap_or(DEFAULT_VIDEO_MODEL).to_string(),
        aspect: opt_str(args, "aspect_ratio")?.map(Aspect::parse).transpose()?,
        resolution: opt_string(args, "resolution")?,
        negative_prompt: opt_string(args, "negative_prompt")?,
        image: opt_string(args, "image")?,
        duration: opt_u64(args, "duration")?
            .map(|n| u32::try_from(n).map_err(|_| anyhow::anyhow!("`duration` is {n} seconds, which is not a clip length")))
            .transpose()?,
        seed: opt_u64(args, "seed")?,
        mode: opt_string(args, "mode")?,
    };

    let backend = match opt_str(args, "provider")? {
        Some(name) => crate::provider::VideoBackend::parse(name)?,
        None => crate::provider::infer_video_backend(&request.model),
    };

    // Before a client exists: a parameter this provider cannot honour stops
    // here, naming what it does offer, rather than being dropped on the way.
    let caps = crate::provider::video_capabilities_for(backend, &request.model);
    caps.check(&request)?;

    let resolved = match backend {
        crate::provider::VideoBackend::Google => crate::video::resolve_video_model(&request.model),
        crate::provider::VideoBackend::Runway => crate::runway::resolve_model(&request.model),
        crate::provider::VideoBackend::Kling => crate::kling::resolve_model(&request.model),
    };

    // Video bills per second, so the check happens before the round trip that
    // starts the meter.
    let price = crate::spend::video_price(backend, &resolved, request.duration);
    crate::spend::check(price, "video render")?;

    let client: Box<dyn crate::provider::VideoProvider> = match backend {
        crate::provider::VideoBackend::Google => Box::new(genai::Client::from_env()?),
        crate::provider::VideoBackend::Runway => Box::new(crate::runway::Client::from_env()?),
        crate::provider::VideoBackend::Kling => Box::new(crate::kling::Client::from_env()?),
    };

    let operation = client.start(&request)?;
    // Written down before it is reported, because the reporting is the fragile
    // half: an agent's session can end between this line and the render
    // finishing, and the id would then exist only in a transcript nobody reads
    // again. `lucida ops` reads it back.
    crate::ledger::video_started(
        &resolved,
        &request.prompt,
        &operation,
        price.against_budget(),
    );
    Ok(format!(
        "Render started — {}.\n\noperation: {operation}\n\n\
         It typically takes 1-3 minutes. Wait about 30 seconds, then call \
         check_video with this operation id and an output path.",
        price.describe()
    ))
}

fn check_video(args: &Value) -> Result<String> {
    let operation = req_str(args, "operation")?;
    let output_path = req_str(args, "output_path")?;

    let backend = match opt_str(args, "provider")? {
        Some(name) => crate::provider::VideoBackend::parse(name)?,
        None => crate::provider::infer_video_backend_from_operation(operation),
    };
    let client: Box<dyn crate::provider::VideoProvider> = match backend {
        crate::provider::VideoBackend::Google => Box::new(genai::Client::from_env()?),
        crate::provider::VideoBackend::Runway => Box::new(crate::runway::Client::from_env()?),
        crate::provider::VideoBackend::Kling => Box::new(crate::kling::Client::from_env()?),
    };

    match client.poll(operation)? {
        VideoStatus::Pending => Ok(
            "Still rendering. Wait roughly 30 seconds before checking again — \
             polling faster will not make it finish sooner."
                .to_string(),
        ),
        VideoStatus::Done(bytes) => {
            let requested = std::path::Path::new(output_path);
            let destination = crate::correct_extension(requested, "video/mp4");
            let written = crate::write_image(&destination, &bytes)?;
            crate::ledger::video_done(operation, &written.to_string_lossy());
            Ok(format!(
                "Render complete. Wrote {} ({:.1} MB).",
                written.display(),
                bytes.len() as f64 / 1_048_576.0
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tagline may say why to pick a provider. It may not claim a capability
    /// only one provider has, because that is a fact about the *set*, and the set
    /// is what changes.
    ///
    /// openai's read "the ONLY provider that can mask an edit to part of an
    /// image" — the reason the provider was added at all, and false from the
    /// release where the local lane learned to inpaint. `provider_summary` states
    /// which providers mask and what their masks mean immediately beside the
    /// tagline, computed from `MaskSupport`, so an exclusivity claim here can
    /// only ever contradict the line it sits on.
    #[test]
    fn no_tagline_claims_masking_is_exclusive() {
        for backend in Backend::ALL {
            let tagline = capabilities_for(*backend, backend.default_model())
                .tagline
                .to_lowercase();
            assert!(
                !(tagline.contains("mask") && tagline.contains("only")),
                "{}'s tagline claims exclusive masking — which providers mask is \
                 generated from MaskSupport, and that set has already changed once",
                backend.name()
            );
        }
    }

    /// Masking appears in the generated note list, so the summary answers "can
    /// this provider mask, and does the mask bind" without any tagline having to.
    #[test]
    fn the_summary_states_each_masking_providers_kind() {
        let summary = provider_summary();
        for backend in Backend::ALL {
            let caps = capabilities_for(*backend, backend.default_model());
            let line = summary
                .lines()
                .find(|l| l.starts_with(&format!("- {}", backend.name())))
                .unwrap_or_else(|| panic!("{} is missing from the summary", backend.name()));

            assert_eq!(
                line.contains("masks"),
                caps.mask.accepted(),
                "the summary disagrees with {}'s capabilities: {line}",
                backend.name()
            );
            if caps.mask.accepted() {
                assert!(line.contains(caps.mask.kind()), "{line}");
            }
        }
    }

    /// The schema must not re-acquire a hard enum on a parameter whose legal
    /// values differ per provider — that is exactly the lie this design removes.
    #[test]
    fn provider_specific_parameters_are_not_advertised_as_enums() {
        let schema = image_schema();
        let props = &schema["inputSchema"]["properties"];
        assert!(props["aspect_ratio"]["enum"].is_null());
        assert!(props["size"]["enum"].is_null());
        // `provider` is a genuine closed set, so it keeps its enum.
        assert!(props["provider"]["enum"].is_array());
    }

    /// Every parameter only some providers honour has to say so, since the
    /// schema is the only thing an agent reads before calling.
    #[test]
    fn restricted_parameters_name_their_provider() {
        let schema = image_schema();
        let props = &schema["inputSchema"]["properties"];
        for field in ["negative_prompt", "seed", "steps", "guidance"] {
            let description = props[field]["description"].as_str().unwrap_or_default();
            assert!(
                description.contains("comfyui"),
                "`{field}` must name the provider that honours it"
            );
        }
        // reference_images is checked against the capabilities themselves rather
        // than a fixed phrase. The previous version asserted the words "both
        // providers", which was true of two providers and quietly wrong of three.
        let references = props["reference_images"]["description"]
            .as_str()
            .unwrap_or_default();
        for backend in Backend::ALL {
            if capabilities_for(*backend, backend.default_model()).references {
                assert!(
                    references.contains(backend.name()),
                    "`{}` edits but is not named in the reference_images description",
                    backend.name()
                );
            }
        }
    }

    /// Checked without calling the tools, so the suite needs no network and no
    /// credentials — `image_providers` would otherwise probe both backends.
    #[test]
    fn advertised_tools_match_the_ones_dispatch_handles() {
        let listed = dispatch("tools/list", &Value::Null).unwrap();
        let advertised: Vec<String> = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(advertised, TOOL_NAMES);
    }

    #[test]
    fn an_unknown_tool_is_refused() {
        let called = call_tool(&json!({ "name": "paint_a_fresco", "arguments": {} }));
        assert!(called.unwrap_err().to_string().contains("unknown tool"));
    }

    /// The last silent drop: a workflow names its own checkpoints, so an
    /// explicit model would be discarded without a word. Refused here, where
    /// "explicit" is still visible — and before any client exists, so the test
    /// needs no credentials.
    #[test]
    fn a_workflow_with_an_explicit_model_is_refused() {
        let error = generate_image(&json!({
            "prompt": "x",
            "output_path": "x.png",
            "workflow": "graph.json",
            "model": "klein"
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains("workflow"), "must name the conflict: {error}");
        assert!(error.contains("model"));
    }

    /// Drives the server over a scripted transcript and returns the replies, in
    /// the order they were written.
    fn drive<F>(script: &str, handle: F) -> Vec<Value>
    where
        F: Fn(&Value) -> Result<Value> + Send + Clone + 'static,
    {
        let out = Arc::new(Mutex::new(Vec::new()));
        run(
            std::io::Cursor::new(script.to_string()),
            Arc::clone(&out),
            handle,
        )
        .unwrap();

        let written = out.lock().unwrap().clone();
        String::from_utf8(written)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a reply was not whole JSON"))
            .collect()
    }

    fn request(id: u64, method: &str, params: Value) -> String {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
    }

    /// The headline finding, asserted rather than described.
    ///
    /// `dispatch` used to run to completion before the next line was read, so a
    /// ComfyUI render held the loop for up to its 1800-second deadline. For all
    /// of it the server was deaf: a `ping` went unanswered, which clients using
    /// ping as a liveness probe read as a dead server.
    ///
    /// The tool call here blocks until the test releases it, so the ping can
    /// only be answered by a reader that is not waiting on the render — and it
    /// must be answered *first*, which is what "off the loop" means.
    #[test]
    fn a_ping_is_answered_while_a_render_is_still_running() {
        let gate = Arc::new(Mutex::new(false));
        let held = Arc::clone(&gate);

        let script = format!(
            "{}\n{}\n",
            request(1, "tools/call", json!({ "name": "generate_image", "arguments": {} })),
            request(2, "ping", Value::Null)
        );

        // Released once the ping has had time to come back, so the render is
        // genuinely still in flight while the reader is answering.
        let opener = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            *held.lock().unwrap() = true;
        });

        let replies = drive(&script, move |_| {
            while !*gate.lock().unwrap() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(json!({ "content": [] }))
        });
        opener.join().unwrap();

        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["id"], 2, "the ping waited for the render");
        assert_eq!(replies[1]["id"], 1);
    }

    /// A cancellation is a *notification*: it carries no id of its own, only the
    /// id of the request it cancels. `serve` dropped everything without an id
    /// before looking at the method, so the one message whose whole purpose is
    /// to stop a paid render was the one message guaranteed to be ignored.
    #[test]
    fn a_cancellation_notification_stops_the_work_it_names() {
        let script = format!(
            "{}\n{}\n",
            request(7, "tools/call", json!({ "name": "generate_image", "arguments": {} })),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": { "requestId": 7 }
            })
        );

        // Polls forever unless cancelled — the test hangs rather than fails if
        // the notification is dropped again, so the assertion below only ever
        // runs when cancellation actually arrived.
        let replies = drive(&script, |_| {
            loop {
                cancel::check()?;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["id"], 7);
        let message = replies[0]["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("cancelled"), "{message}");
    }

    /// Four workers, so four renders overlap rather than queueing. The handler
    /// waits for all of them to arrive, which can only happen if they run
    /// concurrently.
    #[test]
    fn tool_calls_run_concurrently_rather_than_queueing() {
        let arrived = Arc::new(Mutex::new(0usize));

        let script: String = (1..=WORKERS)
            .map(|n| {
                request(
                    n as u64,
                    "tools/call",
                    json!({ "name": "generate_image", "arguments": {} }),
                ) + "\n"
            })
            .collect();

        let replies = drive(&script, move |_| {
            *arrived.lock().unwrap() += 1;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while *arrived.lock().unwrap() < WORKERS {
                if std::time::Instant::now() > deadline {
                    anyhow::bail!("only {} of {WORKERS} calls ran at once", arrived.lock().unwrap());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(json!({ "content": [] }))
        });

        assert_eq!(replies.len(), WORKERS);
        for reply in &replies {
            assert!(reply["error"].is_null(), "{reply}");
        }
    }

    /// A pool turns a panicking tool call from "the process dies visibly" into
    /// "one worker vanishes silently". After four, the server would accept calls
    /// and answer none of them while still passing `ping` — a hang with a
    /// healthy liveness probe, which is the worst shape a failure can take.
    #[test]
    fn a_panicking_tool_call_costs_a_reply_and_not_the_server() {
        let script: String = (1..=WORKERS + 1)
            .map(|n| {
                request(
                    n as u64,
                    "tools/call",
                    json!({ "name": "generate_image", "arguments": { "n": n } }),
                ) + "\n"
            })
            .collect();

        // Every call but the last panics, which is one more panic than there are
        // workers: without the guard the pool is empty by the time the last
        // arrives and the test hangs instead of failing.
        let replies = drive(&script, |params| {
            if params["arguments"]["n"].as_u64() == Some(WORKERS as u64 + 1) {
                Ok(json!({ "content": [] }))
            } else {
                panic!("deliberate");
            }
        });

        assert_eq!(replies.len(), WORKERS + 1);
        let last = replies
            .iter()
            .find(|r| r["id"] == json!(WORKERS + 1))
            .expect("the call after the panics never got a reply");
        assert!(last["error"].is_null(), "the pool did not survive: {last}");
    }

    /// Responses may now come back in any order, so every one has to carry the
    /// id it answers — and every one has to be a whole line, since two workers
    /// finishing together would otherwise interleave their JSON and corrupt the
    /// stream permanently. `drive` parses each line, so a torn write fails here.
    #[test]
    fn every_reply_is_one_whole_line_carrying_its_own_id() {
        let script = format!(
            "{}\n{}\n{}\n",
            request(1, "initialize", Value::Null),
            request(2, "tools/list", Value::Null),
            request(3, "ping", Value::Null)
        );
        let replies = drive(&script, |_| Ok(json!({})));

        let ids: Vec<_> = replies.iter().map(|r| r["id"].clone()).collect();
        assert_eq!(ids, vec![json!(1), json!(2), json!(3)]);
        for reply in &replies {
            assert_eq!(reply["jsonrpc"], "2.0");
        }
    }

    /// The worst silent drop this server had, and the reason the typed accessors
    /// exist: `reference_images` given as a bare string rather than an array.
    /// `as_array` answered `None`, `None` meant "not requested", and the *edit*
    /// became a fresh generation — reported as a success, with the user's
    /// reference image nowhere in it.
    ///
    /// Checked before any client is constructed, so it needs no credentials.
    #[test]
    fn a_reference_image_given_as_a_string_is_refused_not_dropped() {
        let error = generate_image(&json!({
            "prompt": "make it blue",
            "output_path": "out.png",
            "reference_images": "photo.png"
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains("reference_images"), "must name it: {error}");
        assert!(error.contains("array"), "must say what belongs there: {error}");
        assert!(
            error.contains("photo.png"),
            "must quote what arrived, so the fix is obvious: {error}"
        );
    }

    /// One bad element is as silent as one bad container, one level down.
    #[test]
    fn a_non_string_reference_image_names_its_index() {
        let error = generate_image(&json!({
            "prompt": "x",
            "output_path": "x.png",
            "reference_images": ["a.png", 3]
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains("reference_images[1]"), "{error}");
    }

    /// A stringified seed made the render unreproducible while reporting
    /// success — the same class of drop, on the one parameter whose entire
    /// purpose is reproducibility.
    #[test]
    fn a_stringified_number_is_refused_rather_than_dropped() {
        for (field, value) in [("seed", json!("42")), ("steps", json!("30"))] {
            let error = generate_image(&json!({
                "prompt": "x",
                "output_path": "x.png",
                field: value
            }))
            .unwrap_err()
            .to_string();
            assert!(error.contains(field), "`{field}` must be named: {error}");
            assert!(
                error.contains("whole number"),
                "`{field}` must say what belongs there: {error}"
            );
        }
    }

    /// Every optional string parameter, so none is left reading `as_str`
    /// directly. `provider` and `model` are excluded deliberately: they are read
    /// through the same accessor but a wrong *value* there has always been a
    /// loud error, and this test is about wrong *types*.
    #[test]
    fn every_optional_string_parameter_refuses_a_non_string() {
        for field in [
            "aspect_ratio",
            "size",
            "negative_prompt",
            "mask",
            "workflow",
            "provider",
            "model",
        ] {
            let error = generate_image(&json!({
                "prompt": "x",
                "output_path": "x.png",
                field: 7
            }))
            .unwrap_err()
            .to_string();
            assert!(
                error.contains(field) && error.contains("must be a string"),
                "`{field}` was not refused as a type mismatch: {error}"
            );
        }
    }

    /// The video surface reads its arguments the same way, so it drops them the
    /// same way. `start_video` refuses before spending the round trip that would
    /// bill; `check_video` before it can write to a nonsense path.
    #[test]
    fn the_video_tools_refuse_mistyped_arguments_too() {
        let error = start_video(&json!({ "prompt": "a fox", "aspect_ratio": 16 }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("aspect_ratio"), "{error}");

        let error = check_video(&json!({ "operation": ["operations/xyz"], "output_path": "v.mp4" }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("operation"), "{error}");
    }

    /// A missing argument and a mistyped one are different failures and must
    /// read differently — the whole point of separating them.
    #[test]
    fn a_missing_argument_still_reads_as_missing() {
        let error = generate_image(&json!({ "output_path": "x.png" }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("`prompt` is required"), "{error}");
    }

    /// An explicit null is absence, not a type mismatch: a client that fills
    /// every field of its schema and leaves the unused ones null is asking for
    /// the default, not making a mistake.
    #[test]
    fn an_explicit_null_means_not_requested() {
        assert_eq!(opt_str(&json!({ "size": null }), "size").unwrap(), None);
        assert_eq!(opt_u64(&json!({ "seed": null }), "seed").unwrap(), None);
        assert_eq!(
            opt_str_array(&json!({ "reference_images": null }), "reference_images").unwrap(),
            None
        );
    }

    /// A step count past u32 used to wrap silently into a small, plausible one.
    #[test]
    fn an_absurd_step_count_errors_rather_than_wrapping() {
        let error = generate_image(&json!({
            "prompt": "x",
            "output_path": "x.png",
            "steps": 4_294_967_297u64
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains("steps"), "must name the parameter: {error}");
    }
}
