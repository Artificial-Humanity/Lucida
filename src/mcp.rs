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
use crate::genai;
use crate::provider::{
    Aspect, AspectSupport, Backend, ImageProvider, ImageRequest, Size, capabilities_for,
    infer_backend,
};
use crate::provider::{VideoBackend, video_capabilities_for};
use crate::video::{VideoRequest, VideoStatus};
use crate::cancel;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// How many tool calls can be in flight at once.
///
/// Four rather than one, which is what this server effectively had, and rather
/// than unbounded, which would let a client with a loop in it have a hundred
/// paid renders running at once. Four is enough that an agent generating a set
/// of assets is not serialised.
///
/// It bounds concurrency, not the bill. The queue in front of the pool is
/// unbounded, so a runaway client's hundred calls all still run, four at a
/// time; this comment used to claim the pool bounded what such a client could
/// spend, and it never did. What limits spend is the budget `spend` enforces
/// on every render — when `LUCIDA_BUDGET` is set; without one nothing here
/// limits it at all — and, once the client is gone, the hang-up handling at
/// the end of [`run`], which drops whatever is still queued.
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
    // Resolved, not assumed: a preference list moves both the provider and
    // therefore the model, and this banner is the operator's first look at
    // which one the session will actually use.
    let (default_provider, default_model) = match crate::provider::resolve_default::<Backend>() {
        Ok((backend, _)) => (backend.name().to_string(), backend.default_model_description()),
        // A preference nothing satisfies is reported by the first render, in
        // full. Saying so here too, briefly, beats naming a default that the
        // very next call is going to refuse.
        Err(_) => ("none".to_string(), "unresolved — see LUCIDA_IMAGE_PROVIDERS".to_string()),
    };
    eprintln!("lucida MCP server ready (default image provider: {default_provider}, model: {default_model})");
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
    // Set once stdin closes. Read and written only under the `in_flight` lock,
    // so a worker deciding whether to start a job and the reader deciding the
    // client has gone cannot interleave: every job is either started before
    // the hang-up, and cancelled with the rest of the running ones, or found
    // queued after it and dropped.
    let hung_up = Arc::new(AtomicBool::new(false));

    let (sender, receiver) = mpsc::channel::<Job>();
    let receiver = Arc::new(Mutex::new(receiver));

    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let receiver = Arc::clone(&receiver);
            let out = Arc::clone(&out);
            let in_flight = Arc::clone(&in_flight);
            let hung_up = Arc::clone(&hung_up);
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

                    // Decided before the handler is entered, because once it is
                    // the token is only advisory: a provider that renders inside
                    // one blocking request never looks at it. The worker used to
                    // go straight to the handler, so a call cancelled while it
                    // sat in the queue still rendered, and billed, the moment a
                    // worker came free.
                    let cancelled_while_queued = {
                        let mut in_flight = in_flight.lock().unwrap();
                        if hung_up.load(Ordering::Relaxed) {
                            // The client has gone, so nobody will read a reply.
                            // None is written: it would go to a closed pipe,
                            // where all a write can do is fail with EPIPE.
                            in_flight.remove(&key);
                            continue;
                        }
                        job.token.is_cancelled()
                    };

                    let result = if cancelled_while_queued {
                        Err(anyhow::anyhow!(
                            "cancelled at the client's request before it started. \
                             Nothing was submitted to a provider, so nothing was billed."
                        ))
                    } else {
                        cancel::with(job.token, || guarded(&handle, &job.params))
                    };
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
                // Dropping the line left the client waiting for an answer that
                // was never coming, indistinguishable from a server that hung.
                // JSON-RPC has a reply for exactly this, and because the
                // request could not be read there is no id to echo: the spec
                // says to send null.
                eprintln!("unparseable line: {e}");
                respond_error(
                    &out,
                    &Value::Null,
                    PARSE_ERROR,
                    &format!("the line is not valid JSON ({e}); nothing was run"),
                );
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
            // A reused id is refused rather than queued. Inserting it replaced
            // the first call's token, so a cancellation naming that id reached
            // only the newer call, and the older one — possibly already
            // rendering — could not be stopped by anything short of a hang-up.
            // JSON-RPC requires ids to be unique among outstanding requests, so
            // this is the client's error, and -32600 (invalid request) says so.
            let admitted = match in_flight.lock().unwrap().entry(id.to_string()) {
                Entry::Occupied(_) => false,
                Entry::Vacant(slot) => {
                    slot.insert(token.clone());
                    true
                }
            };
            if !admitted {
                respond_error(
                    &out,
                    &id,
                    INVALID_REQUEST,
                    &format!(
                        "request id {id} belongs to a tools/call that has not finished yet. \
                         JSON-RPC ids must be unique among outstanding requests. This call \
                         was not started; send it again with an id not already in use."
                    ),
                );
                continue;
            }
            // Send cannot fail while a worker is alive, and if the pool has gone
            // the process is on its way down anyway.
            let _ = sender.send(Job { id, params, token });
            continue;
        }

        respond(&out, &id, dispatch(method, &params));
    }

    // Stdin closed: the client has gone. Calls still queued are dropped unrun —
    // the workers see `hung_up` and discard them unanswered — and the ones
    // already running are asked to stop. Draining the queue used to run every
    // call the departed client had left in it, each a paid render that nobody
    // would collect.
    //
    // Then the workers finish the line they are writing. Cancellation is
    // cooperative, so this is quick for anything in a poll loop — and for a
    // single blocking render it waits, which is right: that call is already paid
    // for and its result may still be worth writing to disk.
    {
        let in_flight = in_flight.lock().unwrap();
        hung_up.store(true, Ordering::Relaxed);
        for token in in_flight.values() {
            token.cancel();
        }
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
    match result {
        Ok(result) => write_reply(out, json!({ "jsonrpc": "2.0", "id": id, "result": result })),
        Err(e) => {
            // The code comes from the error's type, never from its wording: the
            // message used to be matched with `starts_with("unknown method")`,
            // so rewording it would have quietly turned -32601 into -32603.
            // Anything that did not say what it was is an internal error.
            let code = e.downcast_ref::<RpcError>().map_or(INTERNAL_ERROR, |typed| typed.code);
            respond_error(out, id, code, &e.to_string());
        }
    }
}

// The JSON-RPC 2.0 error codes this server uses.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
/// Clients probe for methods this server lacks (resources/list, prompts/list),
/// and this is the code that tells them to stop.
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// An error that knows which JSON-RPC code it is. `respond` downcasts to this;
/// an `anyhow::Error` of any other type is reported as -32603.
#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn failure(code: i64, message: String) -> anyhow::Error {
        anyhow::Error::new(Self { code, message })
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RpcError {}

/// Writes one JSON-RPC error reply with a code chosen by the caller.
fn respond_error<W: Write>(out: &Out<W>, id: &Value, code: i64, message: &str) {
    write_reply(
        out,
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    );
}

fn write_reply<W: Write>(out: &Out<W>, response: Value) {
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
        "tools/list" => Ok(json!({ "tools": tool_schemas() })),
        "tools/call" => call_tool(params),
        other => Err(RpcError::failure(METHOD_NOT_FOUND, format!("unknown method: {other}"))),
    }
}

/// Every tool's schema, in the order `tools/list` advertises them.
///
/// The one place the schemas are gathered, because two things read them: the
/// listing, and the refusal of an argument a tool does not declare.
fn tool_schemas() -> Vec<Value> {
    vec![
        image_schema(),
        providers_schema(),
        start_video_schema(),
        check_video_schema(),
        video_providers_schema(),
        list_operations_schema(),
    ]
}

/// Describes every provider from its own declared capabilities.
///
/// Generated rather than written, because the hand-written version drifted: by
/// the fourth provider it opened with "Two providers are available", listed
/// three, and omitted the fourth entirely, which existed only in the enum. An
/// agent reads a schema and believes it, so a claim nobody can forget to update
/// is worth more than a better-phrased one that rots.
fn provider_summary() -> String {
    let default_image = crate::provider::resolve_default::<Backend>()
        .ok()
        .map(|(backend, _)| backend);

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
                if Some(*backend) == default_image { " (default)" } else { "" },
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

/// Each provider's recorded seed range, as sentences — empty when none records one.
fn seed_ranges() -> String {
    Backend::ALL
        .iter()
        .filter_map(|b| {
            capabilities_for(*b, b.default_model())
                .seed_limit
                .map(|limit| format!(" {} takes seeds below {limit}.", b.name()))
        })
        .collect()
}

/// Each provider's recorded reference format and count, as sentences.
fn reference_limits() -> String {
    Backend::ALL
        .iter()
        .filter_map(|b| {
            let caps = capabilities_for(*b, b.default_model());
            caps.reference_formats.map(|formats| {
                format!(
                    " {} reads only {} references{}.",
                    b.name(),
                    crate::provider::format_names(formats),
                    caps.max_references.map(|n| format!(", at most {n}")).unwrap_or_default()
                )
            })
        })
        .collect()
}

/// The providers no model id reaches, as a sentence lists them.
fn named_only() -> String {
    let names: Vec<&str> = Backend::ALL
        .iter()
        .filter(|b| b.reached_only_by_name())
        .map(|b| b.name())
        .collect();
    crate::provider::join_and(&names)
}

/// The sentence that follows a render which used a seed — the CLI's, with the
/// pointer to where an agent reads which lanes are verified.
fn seed_note(seed: u64) -> String {
    format!(
        "{} The `seed` parameter says which are verified.",
        crate::provider::seed_note(seed, "`seed`")
    )
}

/// The `provider` enum an agent selects from.
///
/// Generated, as of 2026-08-09. It was a literal `["google", "comfyui", "bfl",
/// "stability", "openai"]` sitting directly above a `model` description that
/// *was* generated — and whose comment records that the hand-written version of
/// itself had omitted openai. The same list, the same drift, one line apart.
///
/// This one is worse than prose going stale. Every other hand-written list
/// merely describes; a JSON Schema `enum` is what a well-behaved client
/// *validates against*, so a provider missing here is not badly documented, it
/// is unreachable — the tool call never leaves the client, and no message names
/// the provider that could have done the job. Nothing in Lucida would have
/// reported it, because the code behind it works perfectly.
fn provider_enum() -> Value {
    Backend::ALL.iter().map(|b| b.name()).collect()
}

/// The same, for the video lanes.
///
/// Video went from one provider to three in a single day, which is exactly the
/// interval over which a hand-written list is right.
fn video_provider_enum() -> Value {
    crate::provider::VideoBackend::ALL
        .iter()
        .map(|b| b.name())
        .collect()
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
                    "enum": provider_enum(),
                    "description": format!(
                        "Which backend to use. {} Never inferred from a model id, so name it here or list it in {} to use it: {}.",
                        default_provider_note::<Backend>(),
                        <Backend as crate::provider::Preferred>::SETTING,
                        named_only()
                    )
                },
                "model": {
                    "type": "string",
                    "description": format!(
                        "Model id or alias. Defaults per provider: {}.",
                        // Generated: the hand-written list omitted openai.
                        Backend::ALL
                            .iter()
                            .map(|b| format!("{} → {}", b.name(), b.default_model_description()))
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
                         DIFFERENT nine: {}. comfyui accepts any ratio, and so does bfl \
                         except {}, which take only: {}. On \
                         openai, gpt-image-2 takes any ratio and its siblings only \
                         1:1, 2:3 and 3:2. runway names its shapes as pixel pairs, \
                         which are also the output size: {}. lemonade takes {}, with a long \
                         edge of {} unless size says otherwise.",
                        genai::ASPECT_RATIOS.join(", "),
                        crate::stability::ASPECT_RATIOS.join(", "),
                        crate::provider::join_and(&crate::bfl::ratio_only_models()),
                        ratio_only_bfl_ratios(),
                        describe_aspect(
                            capabilities_for(Backend::Runway, crate::runway::DEFAULT_IMAGE_MODEL).aspect
                        ),
                        describe_aspect(crate::lemonade::CAPABILITIES.aspect),
                        crate::lemonade::ASPECT_LONG_EDGE
                    )
                },
                "size": {
                    "type": "string",
                    "description": format!(
                        "Long edge in pixels, or a tier (1K, 2K, 4K). google rounds to a \
                         tier; comfyui and bfl ({}) use the number; openai's gpt-image-2 \
                         scales its pixel budget by it. NOT supported by stability, runway \
                         (whose pixel-pair aspect ratio is the size), bfl's {} (which take \
                         an aspect ratio from a list instead) or the other openai models, \
                         which render fixed sizes — passing it there is an error. lemonade \
                         uses the number up to {}.",
                        crate::provider::join_and(&crate::bfl::sized_models()),
                        crate::provider::join_and(&crate::bfl::ratio_only_models()),
                        crate::lemonade::CAPABILITIES.max_long_edge.unwrap_or_default()
                    )
                },
                "negative_prompt": {
                    "type": "string",
                    "description": format!(
                        "What to keep out of the picture. Supported by {} — the others lack the concept, so passing it there is an error rather than a no-op.",
                        providers_where(|c| c.negative_prompt)
                    )
                },
                "seed": {
                    "type": "integer",
                    "description": format!(
                        "Chooses the seed. Supported by {}; {} expose none, so results there cannot be reproduced. \
                         {} On lemonade the server reports no seed, so Lucida always chooses one, sends it and \
                         reports it — omit this and Lucida chooses one.{}",
                        providers_where(|c| c.seed),
                        providers_where(|c| !c.seed),
                        crate::provider::seed_verified_sentence(),
                        seed_ranges()
                    )
                },
                "steps": {
                    "type": "integer",
                    "description": format!(
                        "Sampling steps. {}, and on bfl only flux-2-flex and flux-dev.",
                        providers_where(|c| c.steps)
                    )
                },
                "guidance": {
                    "type": "number",
                    "description": format!(
                        "How closely to follow the prompt. {}, and on bfl only flux-2-flex and flux-dev.",
                        providers_where(|c| c.guidance)
                    )
                },
                "workflow": {
                    "type": "string",
                    "description": "Path to a ComfyUI workflow in API format, rendered instead of the built-in graph. comfyui only. Tokens %prompt% %negative% %seed% %width% %height% %steps% %cfg% mark where values go; a token the file omits means that option cannot be honoured and is refused rather than dropped. Cannot be combined with `model`, `reference_images` or `mask` — the workflow names its own checkpoints and inputs."
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
                         or size is given.{}",
                        providers_where(|c| c.references),
                        reference_limits()
                    )
                }
            },
            "required": ["prompt", "output_path"],
            "additionalProperties": false
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
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

/// The video providers for which `predicate` holds, as prose.
///
/// The video twin of [`providers_where`], over `VideoBackend::ALL`. It reads each
/// provider's *default* model's capabilities, which is the right question for a
/// per-provider claim; the one model-level exception (`veo-lite` and negative
/// prompts) is said beside the list that cannot carry it.
fn video_providers_where(predicate: fn(&crate::provider::VideoCapabilities) -> bool) -> String {
    let names: Vec<&str> = VideoBackend::ALL
        .iter()
        .filter(|b| predicate(&video_capabilities_for(**b, b.default_model())))
        .map(|b| b.name())
        .collect();
    crate::provider::join_and(&names)
}

/// One clause per video provider, `name: what it offers`, from its own table.
///
/// For the parameters every provider takes but in a different shape — aspect
/// ratio and duration — where a list of who supports it would say nothing. This
/// is the shape the hand-written text had, covering two of three providers: it
/// was true of google and runway and silent about kling.
fn video_per_provider(
    describe: fn(&crate::provider::VideoCapabilities) -> String,
) -> String {
    VideoBackend::ALL
        .iter()
        .map(|b| format!("{}: {}", b.name(), describe(&video_capabilities_for(*b, b.default_model()))))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The quality tiers each provider that has any offers, as `kling takes std, pro
/// or master`. Empty tables are skipped, which is what "where the provider has
/// one" means.
fn video_modes() -> String {
    let clauses: Vec<String> = VideoBackend::ALL
        .iter()
        .filter_map(|b| {
            let modes = video_capabilities_for(*b, b.default_model()).modes;
            // Empty means the provider has no tiers, and is skipped.
            let (last, rest) = modes.split_last()?;
            Some(if rest.is_empty() {
                format!("{} takes {last}", b.name())
            } else {
                format!("{} takes {} or {last}", b.name(), rest.join(", "))
            })
        })
        .collect();
    if clauses.is_empty() { "none offers any".to_string() } else { clauses.join("; ") }
}

/// What `provider` does when it is omitted, as a sentence both schemas share.
///
/// This read "defaulting to google", which is the built-in fallback and not the
/// behaviour: with neither `provider` nor `model` given, `resolve_default` walks
/// the user's preference list first, so on a machine configured for another
/// provider the schema said the wrong thing about the machine it was read on.
/// The setting and the fallback both come from the `Preferred` impl that
/// `resolve_default` itself reads. A `model` that is given decides on its own,
/// before any of that, and is said first because it is the case an agent meets.
///
/// All three ways a set preference can fail are `out::Refused` in
/// `provider::preference_list` and `resolve_default`. Two of them were plain
/// errors (exit 1) while this sentence called them refused, so the word was
/// true of one case in three.
fn default_provider_note<T: crate::provider::Preferred>() -> String {
    format!(
        "Inferred from `model` when one is given. With neither, the first provider \
         in {setting} that is usable (its credential is configured, or it needs \
         none), or {built_in} when that setting is unset or blank. If the setting \
         is set but names no provider at all (only commas), names one that does \
         not exist, or none of its providers is usable, the call is refused \
         (exit 2, nothing spent) rather than falling back to {built_in}.",
        setting = T::SETTING,
        built_in = T::BUILT_IN.provider_name()
    )
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
    let default_video = crate::provider::resolve_default::<VideoBackend>()
        .ok()
        .map(|(backend, _)| backend);

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
                if Some(*backend) == default_video { " (default)" } else { "" },
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
                    "enum": video_provider_enum(),
                    "description": format!("Which backend to use. {}", default_provider_note::<VideoBackend>())
                },
                "mode": {
                    "type": "string",
                    "description": format!(
                        "Quality tier, where the provider has one: {}. The others have \
                         none, and passing one there is an error.",
                        video_modes()
                    )
                },
                "aspect_ratio": {
                    "type": "string",
                    // Deliberately not an enum: google names `16:9`, runway
                    // names the pixel pair `1280:720`, and each is accepted by
                    // the provider that names it. Per provider, from the table.
                    "description": format!("W:H. {}.", video_per_provider(|c| describe_aspect(c.aspect)))
                },
                "duration": {
                    "type": "integer",
                    "description": format!(
                        "Seconds of output, and the parameter that decides the bill. {}.",
                        video_per_provider(|c| c.duration.describe())
                    )
                },
                "resolution": {
                    "type": "string",
                    "description": format!(
                        "e.g. 720p or 1080p. Supported by {}. Elsewhere the shape you ask \
                         for decides the pixel count, and passing one is an error.",
                        video_providers_where(|c| c.resolution)
                    )
                },
                "negative_prompt": {
                    "type": "string",
                    // `veo-lite` is the one exception that is a model, not a
                    // provider, so it cannot come from the table and a test
                    // holds it against the guard in `video.rs` instead.
                    "description": format!(
                        "What to keep out of the shot. Supported by {}, except that \
                         google's veo-lite refuses one. Anywhere else passing it is an \
                         error rather than a no-op.",
                        video_providers_where(|c| c.negative_prompt)
                    )
                },
                "seed": {
                    "type": "integer",
                    "description": format!(
                        "Renders the same video again. Supported by {}; every other \
                         provider exposes none, so results there cannot be reproduced \
                         and passing one is an error.",
                        video_providers_where(|c| c.seed)
                    )
                },
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
            "required": ["prompt"],
            "additionalProperties": false
        }
    })
}

/// The ratios BFL's ratio-only models offer, from their capabilities.
fn ratio_only_bfl_ratios() -> String {
    crate::bfl::ratio_only_models()
        .first()
        .map(|m| describe_aspect(capabilities_for(Backend::Bfl, m).aspect))
        .unwrap_or_default()
}

/// One line describing an aspect-ratio capability, shared by both schemas.
fn describe_aspect(support: AspectSupport) -> String {
    match support {
        AspectSupport::Named(ratios) => ratios.join(", "),
        AspectSupport::Pixels(pairs) => pixel_pairs(pairs),
        AspectSupport::Free { multiple_of } => format!("any ratio, rounded to {multiple_of} pixels"),
    }
}

/// Pixel pairs, with the rule that makes `16:9` reach them — otherwise a reader
/// of the list would reasonably conclude only the literal pairs are accepted.
fn pixel_pairs(pairs: &[&str]) -> String {
    format!("{} (or any W:H with the same shape as one of them)", pairs.join(", "))
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
                "provider": { "type": "string", "enum": video_provider_enum(), "description": "Which provider started it. Inferred from the id's shape when omitted." },
                "output_path": { "type": "string", "description": "Where to write the finished video. An .mp4 extension is applied if missing." }
            },
            "required": ["operation", "output_path"],
            "additionalProperties": false
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
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
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
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
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
            "- operation: {}\n  started: {}\n",
            entry["operation"].as_str().unwrap_or("?"),
            crate::clock::stamp(entry["at"].as_i64().unwrap_or(0)),
        ));
        // Absent for an entry from before the ledger recorded one, rather than
        // a guess the caller would pass straight back to check_video.
        if let Some(provider) = crate::ledger::recorded_provider(entry) {
            out.push_str(&format!("  provider: {provider}\n"));
        }
        out.push_str(&format!(
            "  model: {}\n  prompt: {}\n",
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
        // A call naming a tool that does not exist is invalid params for
        // `tools/call` (the MCP spec's own example), not an internal fault.
        return Err(RpcError::failure(
            INVALID_PARAMS,
            format!(
                "unknown tool: {name}. This server offers: {}",
                TOOL_NAMES.join(", ")
            ),
        ));
    }

    if let Err(refusal) = refuse_unknown_arguments(name, args) {
        return wrap(Err(refusal));
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

/// Refuses an argument the tool's own schema does not declare.
///
/// Reading arguments by name means a key nobody asked for is never read, so a
/// misspelling is not an error anywhere downstream — it is just absent.
/// `"reference_image": "photo.png"`, singular, rendered a fresh generation and
/// reported success: the edit-that-becomes-a-generation failure `optional`
/// exists to prevent, arriving by the other door. The accepted names are read
/// from the schema `tools/list` serves rather than listed again here, so what a
/// client was told it may send and what the server accepts cannot differ.
fn refuse_unknown_arguments(tool: &str, args: &Value) -> Result<()> {
    let Some(given) = args.as_object() else {
        return Ok(());
    };
    let schema = tool_schemas()
        .into_iter()
        .find(|schema| schema["name"] == tool)
        .ok_or_else(|| anyhow::anyhow!("`{tool}` is advertised without a schema"))?;
    let accepted: Vec<&str> = schema["inputSchema"]["properties"]
        .as_object()
        .map(|properties| properties.keys().map(String::as_str).collect())
        .unwrap_or_default();

    let unknown: Vec<&str> = given
        .keys()
        .map(String::as_str)
        .filter(|key| !accepted.contains(key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }

    let names = unknown.iter().map(|key| format!("`{key}`")).collect::<Vec<_>>().join(", ");
    let offered = if accepted.is_empty() {
        format!("`{tool}` takes no arguments")
    } else {
        format!(
            "`{tool}` accepts: {}",
            accepted.iter().map(|key| format!("`{key}`")).collect::<Vec<_>>().join(", ")
        )
    };
    anyhow::bail!(
        "{} not an argument of `{tool}`. {offered}. It was refused rather than \
         ignored, so nothing has been run or billed — fix the name, or leave it out.",
        if unknown.len() == 1 { format!("{names} is") } else { format!("{names} are") }
    )
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
        Backend::Runway => Box::new(crate::runway::Client::from_env()?),
        Backend::Lemonade => Box::new(crate::lemonade::Client::from_env()?),
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
    let requested_model = requested_model(args)?;

    // A supplied workflow names its own checkpoints, so an explicit model has
    // nowhere to go. Refused here rather than in the provider because by the
    // time the request reaches comfyui the default model has been filled in
    // and an explicit one is indistinguishable from it.
    // A refusal, since nothing has been sent.
    if workflow.is_some() && requested_model.is_some() {
        return Err(anyhow::Error::new(crate::out::Refused(
            "`workflow` and `model` cannot be combined: a supplied workflow \
             names its own checkpoints, so there is nowhere to put a model id. \
             Name the model inside the workflow file, or drop `workflow` to \
             use the built-in graph."
                .to_string(),
        )));
    }

    let (backend, default_source) = match opt_str(args, "provider")? {
        Some(name) => (Backend::parse(name)?, None),
        None => match requested_model {
            Some(model) => {
                crate::provider::refuse_misrouted_model(model)?;
                (infer_backend(model), None)
            }
            None => {
                let (backend, source) = crate::provider::resolve_default::<Backend>()?;
                (backend, Some(source))
            }
        },
    };

    let model = crate::provider::model_for(backend, requested_model)?;

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

    // Held, not just checked: workers run calls concurrently, and the cost
    // stays reserved until the ledger entry below exists. Every early return —
    // a provider error, a cancellation — drops it, and one that comes after a
    // billed submit is recorded as `abandoned` before it does, because a client
    // that hangs up cancels this call and may well ask again.
    let price = crate::spend::price_for(backend, &request.model, request.size);
    let reservation = crate::spend::check(price, "render")?;

    let provider = open(backend)?;
    let image = crate::generate_billed(provider.as_ref(), &request, |abandoned| {
        crate::ledger::abandoned_image(
            caps.provider,
            &request.model,
            &request.prompt,
            abandoned,
            price.against_budget(),
        );
    })
    .map_err(|error| crate::lemonade::explain_unreachable(error, &default_source))?;

    // Providers pick the output format themselves, so the requested extension may
    // not match the bytes. Correct it and say so, rather than handing back a file
    // whose name lies about its contents.
    let requested = std::path::Path::new(output_path);
    let destination = crate::correct_extension(requested, &image.mime_type);
    let renamed = destination != requested;
    let written = crate::write_billed(&destination, &image.bytes, |path, unsaved| {
        crate::ledger::image(
            caps.provider,
            &request.model,
            &request.prompt,
            path,
            image.seed,
            price.against_budget(),
            unsaved,
        );
    })?;
    drop(reservation);

    // The dimensions are stated because they are not always the ones requested:
    // an edit on comfyui normalizes to roughly a megapixel, so the result can
    // differ from both the request and the source.
    let size = match crate::image_dimensions(&image.bytes, &image.mime_type) {
        Some((w, h)) => format!("{w}x{h}, "),
        None => String::new(),
    };
    let mut text = format!(
        "Wrote {} ({size}{} KB, {}) via {}.{}",
        written.display(),
        image.bytes.len() / 1024,
        image.mime_type,
        caps.provider,
        default_note(&default_source, backend.name()),
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
        text.push_str("\n\n");
        text.push_str(&seed_note(seed));
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
    if let Some(commentary) = &image.commentary {
        if !commentary.is_empty() {
            text.push_str(&format!("\n\nModel commentary: {commentary}"));
        }
    }
    if let Some(note) = late_cancellation_note(price) {
        text.push_str(&format!("\n\n{note}"));
    }
    Ok(text)
}

/// The `model` argument, trimmed the way `LUCIDA_LEMONADE_MODEL` is.
///
/// A padded id would otherwise reach a lane that matches ids exactly (Lemonade's)
/// with the padding still on it, while the setting form of the same id worked.
fn requested_model(args: &Value) -> Result<Option<&str>> {
    Ok(opt_str(args, "model")?.map(str::trim))
}

/// What a caller that cancelled needs to hear when the render finished anyway.
///
/// A provider that renders inside one blocking request cannot be stopped once
/// it has started: the image comes back and is written. Said, rather than left
/// for a client to find a file it believes it cancelled.
fn late_cancellation_note(price: crate::spend::Price) -> Option<String> {
    if !cancel::cancelled() {
        return None;
    }
    let cost = if price == crate::spend::Price::Free {
        "this lane is free, so nothing was billed"
    } else {
        "it was billed"
    };
    Some(format!(
        "Note: this call was cancelled while the render was already running, too late \
         to stop it, so the image above was written — {cost}."
    ))
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

        out.push_str(&capabilities_text(&caps));
    }

    out
}

/// What one provider supports, as the lines `image_providers` prints for it. A
/// limit with nothing recorded gets no phrase, rather than a phrase saying so.
fn capabilities_text(caps: &crate::provider::Capabilities) -> String {
    // ` (below N)`, ` (PNG only)`, ...: present only when something is recorded.
    let note = |phrase: Option<String>| phrase.map(|p| format!(" ({p})")).unwrap_or_default();
    format!(
        "aspect ratio: {}\n\
         seed: {}{}  |  negative prompt: {}  |  reference images: {}{}\n\
         size: {}{}  |  mask: {}  |  own workflow: {}\n\
         steps: {}  |  guidance: {}\n\
         output carries: {}\n\n",
        describe_aspect(caps.aspect),
        caps.seed,
        note(caps.describe_seed_limit()),
        caps.negative_prompt,
        caps.references,
        note(caps.describe_reference_formats()),
        caps.size,
        note(caps.describe_long_edge().map(|edge| format!("long edge: {edge}"))),
        caps.mask.describe(),
        caps.workflow,
        caps.steps,
        caps.guidance,
        caps.provenance.describe()
    )
}

/// A line naming the resolved provider, for a call that named none.
///
/// The MCP surface needs this at least as much as the CLI does: the caller is
/// an agent that will report back to a person, and "it used bfl because that is
/// first in your list" is the difference between a default and a substitution.
/// Empty when the caller named a provider or a model — they already know.
fn default_note(source: &Option<crate::provider::DefaultSource>, chosen: &str) -> String {
    match source {
        Some(source) => format!("\n\nProvider: {}", source.describe(chosen)),
        None => String::new(),
    }
}

fn start_video(args: &Value) -> Result<String> {
    let prompt = req_str(args, "prompt")?;

    let requested_model = opt_str(args, "model")?;

    let (backend, default_source) = match opt_str(args, "provider")? {
        Some(name) => (crate::provider::VideoBackend::parse(name)?, None),
        None => match requested_model {
            Some(model) => (crate::provider::infer_video_backend(model), None),
            None => {
                let (backend, source) =
                    crate::provider::resolve_default::<crate::provider::VideoBackend>()?;
                (backend, Some(source))
            }
        },
    };

    let request = VideoRequest {
        prompt: prompt.to_string(),
        model: requested_model
            .unwrap_or_else(|| backend.default_model())
            .to_string(),
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
    let reservation = crate::spend::check(price, "video render")?;

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
        backend.name(),
        &resolved,
        &request.prompt,
        &operation,
        price.against_budget(),
    );
    // The started entry carries the spend, so the hold can go.
    drop(reservation);
    Ok(format!(
        "Render started — {}.\n\noperation: {operation}\n\n\
         It typically takes 1-3 minutes. Wait about 30 seconds, then call \
         check_video with this operation id and an output path.{}",
        price.describe(),
        default_note(&default_source, backend.name()),
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

    let polled = client.poll(operation);
    if let Err(error) = &polled {
        // Retired from list_operations only when the provider says it is over.
        crate::ledger::note_failure(backend.name(), operation, error);
    }
    match polled? {
        VideoStatus::Pending => Ok(
            "Still rendering. Wait roughly 30 seconds before checking again — \
             polling faster will not make it finish sooner."
                .to_string(),
        ),
        VideoStatus::Done(bytes) => {
            let requested = std::path::Path::new(output_path);
            let destination = crate::correct_extension(requested, "video/mp4");
            let written = crate::write_image(&destination, &bytes)?;
            crate::ledger::video_done(backend.name(), operation, &written.to_string_lossy());
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

    /// Generated where it was hand-written: every provider that honours a
    /// restricted parameter is named in its description.
    #[test]
    fn restricted_parameters_name_every_provider_that_honours_them() {
        let schema = image_schema();
        let props = &schema["inputSchema"]["properties"];
        type Honours = fn(&crate::provider::Capabilities) -> bool;
        let fields: [(&str, Honours); 4] = [
            ("negative_prompt", |c| c.negative_prompt),
            ("seed", |c| c.seed),
            ("steps", |c| c.steps),
            ("guidance", |c| c.guidance),
        ];
        for (field, honours) in fields {
            let description = props[field]["description"].as_str().unwrap();
            for backend in Backend::ALL {
                if honours(&capabilities_for(*backend, backend.default_model())) {
                    assert!(description.contains(backend.name()), "`{field}` omits {}: {description}", backend.name());
                }
            }
        }
    }

    /// The per-provider geometry prose names every provider, and states each
    /// recorded ceiling, seed range and reference format with its number.
    #[test]
    fn the_geometry_and_limits_prose_covers_every_provider() {
        let schema = image_schema();
        let props = &schema["inputSchema"]["properties"];
        let aspect = props["aspect_ratio"]["description"].as_str().unwrap();
        let size = props["size"]["description"].as_str().unwrap();
        let seed = props["seed"]["description"].as_str().unwrap();
        let references = props["reference_images"]["description"].as_str().unwrap();
        let provider = props["provider"]["description"].as_str().unwrap();
        for backend in Backend::ALL {
            let caps = capabilities_for(*backend, backend.default_model());
            assert!(aspect.contains(backend.name()), "aspect_ratio omits {}: {aspect}", backend.name());
            assert!(size.contains(backend.name()), "size omits {}: {size}", backend.name());
            if let Some(most) = caps.max_long_edge {
                assert!(size.contains(&most.to_string()), "{size}");
            }
            if let Some(limit) = caps.seed_limit {
                assert!(seed.contains(&limit.to_string()), "{seed}");
            }
            if let Some(formats) = caps.reference_formats {
                assert!(references.contains(&crate::provider::format_names(formats)), "{references}");
            }
            if backend.reached_only_by_name() {
                assert!(provider.contains(backend.name()), "{provider}");
            }
        }
    }

    /// A seed is reported for every lane that has one, but only some have been
    /// shown to give the same image from it again (`Backend::seed_verified`). The text that follows a render
    /// may say the seed can be passed back; it may not promise the picture.
    #[test]
    fn the_seed_note_promises_no_repeat_of_the_picture() {
        let note = seed_note(42);
        assert!(note.contains("42") && note.contains("`seed`"), "{note}");
        for promise in ["same image", "render it again", "reproduc", "identical"] {
            assert!(!note.to_lowercase().contains(promise), "the seed note promises `{promise}`: {note}");
        }
    }

    /// The `seed` description names the lanes a seed is verified to repeat on
    /// in the shared sentence, and claims no other.
    #[test]
    fn the_seed_description_names_the_verified_lanes_from_one_place() {
        let schema = image_schema();
        let seed = schema["inputSchema"]["properties"]["seed"]["description"].as_str().unwrap();
        assert!(seed.contains(&crate::provider::seed_verified_sentence()), "{seed}");
        assert_eq!(seed.matches("verified").count(), 1, "a second claim about verification: {seed}");
    }

    /// The placeholder is for capability lookups. An agent reading the schema
    /// must never be offered it as a model id.
    #[test]
    fn the_schema_never_offers_the_placeholder_model() {
        let schemas = serde_json::to_string(&tool_schemas()).unwrap();
        assert!(!schemas.contains(crate::lemonade::PLACEHOLDER_MODEL), "{schemas}");
        let model = image_schema()["inputSchema"]["properties"]["model"]["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(model.contains("lemonade → none built in"), "{model}");
    }

    /// A provider that records no reference format, long edge or seed range says
    /// nothing about them: no filler phrase, and no parenthetical contradicting
    /// "reference images: false". Only a recorded limit is printed.
    #[test]
    fn image_providers_prints_a_limit_only_when_one_is_recorded() {
        let phrases = [
            "any format the provider accepts",
            "no ceiling recorded",
            "long edge",
            "below",
            "only)",
        ];
        // Lemonade is the provider that records all three, so it is the case
        // below rather than one of the providers recording none.
        for backend in Backend::ALL.iter().copied().filter(|b| *b != Backend::Lemonade) {
            let text = capabilities_text(&capabilities_for(backend, backend.default_model()));
            for phrase in phrases {
                assert!(!text.contains(phrase), "{}: `{phrase}` in:\n{text}", backend.name());
            }
        }
        let lemonade = capabilities_text(&crate::lemonade::CAPABILITIES);
        assert!(lemonade.contains("(PNG and JPEG only)"), "{lemonade}");
        assert!(lemonade.contains("(long edge: at most 2048 pixels)"), "{lemonade}");
        assert!(lemonade.contains("(below 4294967296)"), "{lemonade}");

        let limited = crate::provider::Capabilities {
            reference_formats: Some(&["image/png"]),
            max_long_edge: Some(2048),
            seed_limit: Some(1 << 31),
            ..crate::comfy::CAPABILITIES
        };
        let text = capabilities_text(&limited);
        assert!(text.contains("(PNG only)"), "{text}");
        assert!(text.contains("(long edge: at most 2048 pixels)"), "{text}");
        assert!(text.contains("(below 2147483648)"), "{text}");
    }

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

    /// BFL's endpoints disagree about geometry: FLUX.2, FLUX.1.1 and dev take
    /// pixels, Kontext and Ultra take an `aspect_ratio` from a list and no size.
    /// The schema said "comfyui and bfl accept any ratio" and "use the number"
    /// of the provider as a whole, which sent an agent to `--size` on a model
    /// that refuses it. Both clauses are generated from the per-model
    /// capabilities; this holds each model named on the right side of each.
    #[test]
    fn the_schema_says_which_bfl_models_take_a_size() {
        let schema = image_schema();
        let props = &schema["inputSchema"]["properties"];
        let aspect = props["aspect_ratio"]["description"].as_str().unwrap();
        let size = props["size"]["description"].as_str().unwrap();

        let ratio_only = crate::bfl::ratio_only_models();
        let sized = crate::bfl::sized_models();
        assert!(!ratio_only.is_empty() && !sized.is_empty());
        for model in &ratio_only {
            assert!(aspect.contains(model), "`{model}` is missing from aspect_ratio: {aspect}");
            assert!(size.contains(model), "`{model}` is missing from size: {size}");
        }
        for model in &sized {
            assert!(size.contains(model), "`{model}` is missing from size: {size}");
            assert!(
                !aspect.contains(&format!("{model},")) && !aspect.contains(&format!("{model} and")),
                "`{model}` takes any ratio and must not be listed as ratio-only: {aspect}"
            );
        }
        // The ratio list is the one the ratio-only models themselves offer.
        let AspectSupport::Named(ratios) =
            capabilities_for(Backend::Bfl, ratio_only[0]).aspect
        else {
            panic!("ratio-only models name their ratios");
        };
        assert!(aspect.contains(&ratios.join(", ")), "{aspect}");
        // And the old blanket claim about the provider is gone.
        assert!(!aspect.contains("comfyui and bfl accept any ratio"), "{aspect}");
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

    /// Every provider Lucida has must appear in the enum an agent selects from.
    ///
    /// This is the one hand-written list whose failure is silent in both
    /// directions. A JSON Schema `enum` is what a well-behaved client validates
    /// against before sending, so a provider missing here cannot be chosen at
    /// all — and the request never reaches Lucida, so none of the careful
    /// refusal messages ever get the chance to name it. The code behind the
    /// missing provider works perfectly, which is why nothing would report it.
    ///
    /// It was a literal until 2026-08-09, one line above a `model` description
    /// generated precisely because the hand-written version had omitted openai.
    #[test]
    fn every_provider_is_selectable_through_the_schema() {
        let listed = |schema: &Value, path: &str| -> Vec<String> {
            schema[path]["properties"]["provider"]["enum"]
                .as_array()
                .expect("provider must offer a closed set")
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };

        let images = listed(&image_schema(), "inputSchema");
        for backend in Backend::ALL {
            assert!(
                images.contains(&backend.name().to_string()),
                "{} is implemented but cannot be selected: {images:?}",
                backend.name()
            );
        }
        assert_eq!(images.len(), Backend::ALL.len(), "stale name in the enum");

        // Both video surfaces: starting a render and asking after one. The
        // second is easy to forget, and forgetting it strands an operation.
        for schema in [start_video_schema(), check_video_schema()] {
            let video = listed(&schema, "inputSchema");
            for backend in crate::provider::VideoBackend::ALL {
                assert!(
                    video.contains(&backend.name().to_string()),
                    "video provider {} cannot be selected in {}: {video:?}",
                    backend.name(),
                    schema["name"]
                );
            }
        }
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

    /// The video counterpart: every parameter only some video providers honour
    /// says which, and the answer is read off `VideoCapabilities` rather than
    /// off a phrase.
    ///
    /// The image test above pins `comfyui`, which is one provider's name in one
    /// table. This is checked in both directions against the whole of
    /// `VideoBackend::ALL`, because the failure it holds was a hand-written
    /// "google only" that went on being believed after kling declared
    /// `negative_prompt: true` — an agent reading it would never have asked
    /// kling for one, and nothing would have said the answer was yes.
    #[test]
    fn restricted_video_parameters_name_their_provider() {
        let schema = start_video_schema();
        let props = &schema["inputSchema"]["properties"];
        let text = |field: &str| -> String {
            props[field]["description"]
                .as_str()
                .unwrap_or_else(|| panic!("`{field}` has no description"))
                .to_string()
        };

        type Honoured = fn(&crate::provider::VideoCapabilities) -> bool;
        let flags: [(&str, Honoured); 3] = [
            ("negative_prompt", |c| c.negative_prompt),
            ("seed", |c| c.seed),
            ("resolution", |c| c.resolution),
        ];
        for (field, honoured) in flags {
            // The generated clause itself, not the description around it: the
            // negative_prompt text names google again in its veo-lite sentence,
            // so `contains` on the whole string could never fail for google.
            // The clause must be exactly the providers that honour the
            // parameter, and end where the list ends.
            let description = text(field);
            let honouring: Vec<&str> = VideoBackend::ALL
                .iter()
                .filter(|b| honoured(&video_capabilities_for(**b, b.default_model())))
                .map(|b| b.name())
                .collect();
            let clause = format!("Supported by {}", crate::provider::join_and(&honouring));
            let at = description
                .find(&clause)
                .unwrap_or_else(|| panic!("`{field}` must open its list with `{clause}`: {description}"));
            let after = description[at + clause.len()..].chars().next();
            assert!(
                matches!(after, Some(',' | ';' | '.')),
                "`{field}` names more providers than honour it (`{clause}` runs on): {description}"
            );
        }

        // Quality tiers carry their own names, since the tier list is the thing
        // an agent has to pass.
        let mode = text("mode");
        for backend in VideoBackend::ALL {
            let caps = video_capabilities_for(*backend, backend.default_model());
            assert_eq!(mode.contains(backend.name()), !caps.modes.is_empty(), "{mode}");
            for tier in caps.modes {
                assert!(mode.contains(tier), "`{tier}` is missing from `mode`: {mode}");
            }
        }

        // Aspect and duration are offered by everyone, in different shapes, so
        // each provider is named beside its own description of the shape.
        let aspect = text("aspect_ratio");
        let duration = text("duration");
        for backend in VideoBackend::ALL {
            let caps = video_capabilities_for(*backend, backend.default_model());
            assert!(
                aspect.contains(&format!("{}: {}", backend.name(), describe_aspect(caps.aspect))),
                "`aspect_ratio` does not give {}'s shapes: {aspect}",
                backend.name()
            );
            assert!(
                duration.contains(&format!("{}: {}", backend.name(), caps.duration.describe())),
                "`duration` does not give {}'s lengths: {duration}",
                backend.name()
            );
        }

        // The one model-level exception cannot come from a per-provider table,
        // so it stays hand-written and is held here against the guard it
        // describes: `veo-lite` is a real alias, and it is the `lite` the guard
        // in `video.rs` refuses a negative prompt for.
        assert!(text("negative_prompt").contains("veo-lite"));
        assert!(crate::video::resolve_video_model("veo-lite").contains("lite"));
    }

    /// A `provider` description may not name a default the code does not use.
    ///
    /// It read "defaulting to google" while `resolve_default` walked
    /// `LUCIDA_IMAGE_PROVIDERS` first — so an agent on a machine configured for
    /// another provider was told the wrong thing about its own machine. The
    /// setting and the built-in fallback are both read from the `Preferred`
    /// impl that `resolve_default` uses, so the sentence moves when they do.
    #[test]
    fn the_provider_description_names_the_setting_that_picks_the_default() {
        use crate::provider::Preferred;

        let described = |schema: Value| -> String {
            schema["inputSchema"]["properties"]["provider"]["description"]
                .as_str()
                .expect("provider has a description")
                .to_string()
        };

        let image = described(image_schema());
        assert!(image.contains(Backend::SETTING), "{image}");
        assert!(image.contains(Backend::BUILT_IN.name()), "{image}");

        let video = described(start_video_schema());
        assert!(video.contains(VideoBackend::SETTING), "{video}");
        assert!(video.contains(VideoBackend::BUILT_IN.name()), "{video}");

        // The old claim was unconditional, and it is the unconditional form
        // that is false.
        for text in [&image, &video] {
            assert!(!text.contains("defaulting to"), "{text}");
            // The rule `resolve_default` is built around: a preference is an
            // order, never a fallback chain, so a setting nothing satisfies
            // refuses instead of quietly reaching the built-in provider. Without
            // this sentence a reader would take the built-in for a safety net.
            assert!(text.contains("the call is refused"), "{text}");
            assert!(text.contains("rather than falling back to"), "{text}");
            // All three refusals are named, the empty list among them.
            assert!(text.contains("names no provider at all"), "{text}");
            assert!(text.contains("does not exist"), "{text}");
            assert!(text.contains("none of its providers is usable"), "{text}");
            // ComfyUI needs no credential and is usable without one.
            assert!(text.contains("or it needs none"), "{text}");
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

    /// What `respond` writes for `result`, as the one reply it is.
    fn replied(result: Result<Value>) -> Value {
        let out = Arc::new(Mutex::new(Vec::new()));
        respond(&out, &json!(1), result);
        let written = out.lock().unwrap().clone();
        serde_json::from_slice(&written).unwrap()
    }

    /// The code is carried by the error's type. These messages are the exact
    /// wording the old `starts_with` check keyed on, so a pass here is the
    /// proof that wording no longer decides anything.
    #[test]
    fn the_code_comes_from_the_type_of_the_error_not_its_wording() {
        let untyped = replied(Err(anyhow::anyhow!("unknown method: resources/list")));
        assert_eq!(untyped["error"]["code"], INTERNAL_ERROR, "{untyped}");

        let typed = replied(dispatch("resources/list", &Value::Null));
        assert_eq!(typed["error"]["code"], METHOD_NOT_FOUND, "{typed}");

        let reworded = replied(Err(RpcError::failure(INVALID_PARAMS, "something else".into())));
        assert_eq!(reworded["error"]["code"], INVALID_PARAMS, "{reworded}");
    }

    #[test]
    fn an_unknown_tool_is_invalid_params() {
        let called = replied(call_tool(&json!({ "name": "paint_a_fresco", "arguments": {} })));
        assert_eq!(called["error"]["code"], INVALID_PARAMS, "{called}");
    }

    /// A line that is not JSON is answered, with the id JSON-RPC prescribes when
    /// the request could not be read.
    #[test]
    fn an_unparseable_line_is_answered_with_a_parse_error() {
        let replies = drive("not json at all\n", |_| Ok(json!({})));
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(replies[0]["error"]["code"], PARSE_ERROR, "{}", replies[0]);
        assert_eq!(replies[0]["id"], Value::Null);
    }

    /// The misspelling that turned an edit into a fresh generation.
    ///
    /// `workflow` with `model` is a refusal `generate_image` raises before it
    /// touches a provider, so if the argument check ever stopped running this
    /// test would fail on the wrong message — not render. A test of this guard
    /// that passed a valid call would, on a machine holding credentials, spend
    /// money the moment the guard broke; the first version of it did.
    #[test]
    fn a_misspelt_argument_is_refused_naming_it_and_what_is_accepted() {
        let called = call_tool(&json!({
            "name": "generate_image",
            "arguments": {
                "prompt": "a fox",
                "output_path": "fox.png",
                "workflow": "graph.json",
                "model": "klein",
                "reference_image": "photo.png"
            }
        }))
        .unwrap();
        assert_eq!(called["isError"], true, "{called}");
        let text = called["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("`reference_image`"), "{text}");
        assert!(text.contains("`reference_images`"), "{text}");
        assert!(text.contains("refused rather than ignored"), "{text}");
    }

    /// A tool that takes nothing says so, rather than listing nothing.
    #[test]
    fn an_argument_to_a_tool_that_takes_none_is_refused() {
        let called = call_tool(&json!({ "name": "list_operations", "arguments": { "all": true } })).unwrap();
        assert_eq!(called["isError"], true, "{called}");
        let text = called["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("takes no arguments"), "{text}");
    }

    /// The accepted set is read from the schema, so every property a schema
    /// declares is accepted by construction — and each schema says it is closed,
    /// which is what a validating client enforces before the call is sent.
    #[test]
    fn every_declared_property_is_accepted_and_every_schema_is_closed() {
        for schema in tool_schemas() {
            let name = schema["name"].as_str().unwrap();
            assert_eq!(schema["inputSchema"]["additionalProperties"], false, "{name}");

            let declared = schema["inputSchema"]["properties"].as_object().unwrap();
            let args: serde_json::Map<String, Value> =
                declared.keys().map(|key| (key.clone(), Value::Null)).collect();
            assert!(
                refuse_unknown_arguments(name, &Value::Object(args)).is_ok(),
                "{name} refused an argument its own schema declares"
            );
            assert!(
                refuse_unknown_arguments(name, &json!({ "no_such_argument": 1 })).is_err(),
                "{name} accepted an argument its schema does not declare"
            );
        }
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
        .unwrap_err();
        // A refusal, as the README says: nothing was sent, and retrying the
        // same call cannot succeed.
        assert_eq!(crate::out::code_for(&error), crate::out::REFUSED, "{error:#}");
        let error = error.to_string();
        assert!(error.contains("workflow"), "must name the conflict: {error}");
        assert!(error.contains("model"));
    }

    /// A scripted client's side of stdin. `at_end` runs once, when the server
    /// asks for more input after the last line, and end of input is reported
    /// only after it returns.
    ///
    /// A plain `Cursor` reported end of input the instant the script was read,
    /// which is a client that sends its requests and hangs up at once. That was
    /// harmless while the server ran every queued call regardless; now that a
    /// hang-up discards calls not yet started, each test has to say when its
    /// client leaves. By the time `at_end` runs, every line has been acted on:
    /// `BufReader` asks its inner reader for more only once the lines already
    /// buffered have been handed out, and the loop handles each line before it
    /// asks for the next.
    struct Transcript<F: FnOnce()> {
        lines: std::io::Cursor<Vec<u8>>,
        at_end: Option<F>,
    }

    impl<F: FnOnce()> std::io::Read for Transcript<F> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let read = self.lines.read(buf)?;
            if read == 0 {
                if let Some(at_end) = self.at_end.take() {
                    at_end();
                }
            }
            Ok(read)
        }
    }

    /// Runs the server over `script`, writing into `out`, and calls `at_end`
    /// once every line has been acted on, hanging up when it returns. Returns
    /// the replies in the order they were written.
    fn converse<F, E>(script: &str, handle: F, out: Out<Vec<u8>>, at_end: E) -> Vec<Value>
    where
        F: Fn(&Value) -> Result<Value> + Send + Clone + 'static,
        E: FnOnce(),
    {
        let transcript = Transcript {
            lines: std::io::Cursor::new(script.as_bytes().to_vec()),
            at_end: Some(at_end),
        };
        run(std::io::BufReader::new(transcript), Arc::clone(&out), handle).unwrap();

        let written = out.lock().unwrap().clone();
        String::from_utf8(written)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a reply was not whole JSON"))
            .collect()
    }

    /// Drives the server as a well-behaved client: it sends the script, runs
    /// `at_end`, then waits for an answer to every request before hanging up.
    fn drive_until<F, E>(script: &str, handle: F, at_end: E) -> Vec<Value>
    where
        F: Fn(&Value) -> Result<Value> + Send + Clone + 'static,
        E: FnOnce(),
    {
        let expected = script
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|message| message.get("id").is_some())
            .count();
        let out = Arc::new(Mutex::new(Vec::new()));
        let written = Arc::clone(&out);

        converse(script, handle, out, move || {
            at_end();
            // Bounded, so a missing reply fails the test's assertions rather
            // than hanging it. Ten seconds is far beyond anything these
            // in-memory handlers take.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                let replies = written.lock().unwrap().iter().filter(|b| **b == b'\n').count();
                if replies >= expected {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        })
    }

    fn drive<F>(script: &str, handle: F) -> Vec<Value>
    where
        F: Fn(&Value) -> Result<Value> + Send + Clone + 'static,
    {
        drive_until(script, handle, || {})
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

    /// One `tools/call` line per `(id, n)`, with `n` in the arguments so a
    /// handler can tell which call it was entered for.
    fn calls_of(script: impl Iterator<Item = (u64, u64)>) -> String {
        script
            .map(|(id, n)| {
                request(id, "tools/call", json!({ "name": "generate_image", "arguments": { "n": n } }))
                    + "\n"
            })
            .collect()
    }

    fn cancellation(id: u64) -> String {
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": id }
        })
        .to_string()
            + "\n"
    }

    /// A call cancelled while it waited for a worker is answered without ever
    /// reaching the handler. The worker used to install the cancelled token and
    /// call the handler anyway, and a provider that renders inside one blocking
    /// request never looks at the token — so the render the user had cancelled
    /// ran, and billed, once a worker came free.
    ///
    /// Every worker is held on a gate that opens only after the whole script —
    /// the cancellation included — has been read, so the fifth call is still in
    /// the queue when its cancellation arrives.
    #[test]
    fn a_call_cancelled_while_queued_never_reaches_the_handler() {
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let opener = Arc::clone(&gate);
        let entered = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&entered);

        let queued = WORKERS as u64 + 1;
        let script = calls_of((1..=queued).map(|n| (n, n))) + &cancellation(queued);

        let replies = drive_until(
            &script,
            move |params| {
                let n = params["arguments"]["n"].as_u64().unwrap();
                entered.lock().unwrap().push(n);
                while !gate.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(json!({ "content": [] }))
            },
            move || opener.store(true, std::sync::atomic::Ordering::SeqCst),
        );

        assert!(
            !seen.lock().unwrap().contains(&queued),
            "the cancelled call reached the handler: {:?}",
            seen.lock().unwrap()
        );
        assert_eq!(replies.len(), WORKERS + 1);
        let reply = replies
            .iter()
            .find(|r| r["id"] == json!(queued))
            .expect("the cancelled call got no reply");
        let message = reply["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("cancelled"), "{reply}");
        assert!(message.contains("Nothing was submitted"), "must say nothing was spent: {reply}");
    }

    /// When the client hangs up, calls still waiting for a worker are dropped
    /// rather than run. Stdin closing used to cancel the running calls and then
    /// let the workers drain the queue — running every call the departed client
    /// had left in it, each one a paid render nobody would ever collect.
    ///
    /// Calls already running behave as they did: their tokens are cancelled and
    /// they answer. Queued ones get no answer at all, because nobody is left to
    /// read one.
    #[test]
    fn calls_still_queued_when_the_client_hangs_up_are_never_run() {
        let started = Arc::new(Mutex::new(0usize));
        let running = Arc::clone(&started);
        let entered = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&entered);

        let total = WORKERS as u64 + 3;
        let script = calls_of((1..=total).map(|n| (n, n)));

        let replies = converse(
            &script,
            move |params| {
                entered.lock().unwrap().push(params["arguments"]["n"].as_u64().unwrap());
                *started.lock().unwrap() += 1;
                loop {
                    cancel::check()?;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            },
            Arc::new(Mutex::new(Vec::new())),
            // Hangs up only once every worker is inside a call, so exactly the
            // last three are still queued — and bounded, so a pool that never
            // fills fails the assertions below instead of hanging the test.
            move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while *running.lock().unwrap() < WORKERS && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            },
        );

        let mut seen = seen.lock().unwrap().clone();
        seen.sort_unstable();
        assert_eq!(seen, (1..=WORKERS as u64).collect::<Vec<_>>(), "queued calls ran after the hang-up");
        assert_eq!(replies.len(), WORKERS, "{replies:?}");
        for reply in &replies {
            assert!(reply["id"].as_u64().unwrap() <= WORKERS as u64, "{reply}");
            let message = reply["error"]["message"].as_str().unwrap_or_default();
            assert!(message.contains("cancelled"), "{reply}");
        }
    }

    /// A `tools/call` reusing the id of one still in flight is refused, and the
    /// first keeps its token. The second used to overwrite the first's entry, so
    /// a cancellation naming that id reached only the newer call, and the older
    /// one — already rendering — could no longer be stopped at all.
    #[test]
    fn a_call_reusing_an_id_still_in_flight_is_refused() {
        let entered = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&entered);

        let script = calls_of([(5, 1), (5, 2)].into_iter()) + &cancellation(5);
        let replies = drive(&script, move |params| {
            entered.lock().unwrap().push(params["arguments"]["n"].as_u64().unwrap());
            loop {
                cancel::check()?;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });

        // The first call may or may not have reached a worker before its
        // cancellation did — either way the duplicate must never run.
        assert!(!seen.lock().unwrap().contains(&2), "the duplicate was run: {:?}", seen.lock().unwrap());
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert_eq!(replies[0]["id"], 5);
        assert_eq!(replies[0]["error"]["code"], -32600, "{}", replies[0]);
        assert_eq!(replies[1]["id"], 5);
        let message = replies[1]["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("cancelled"), "the first call lost its token: {}", replies[1]);
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

    #[test]
    fn a_render_that_finished_after_its_cancellation_says_so() {
        assert_eq!(late_cancellation_note(crate::spend::Price::Free), None);

        let token = cancel::Token::new();
        token.cancel();
        cancel::with(token, || {
            let free = late_cancellation_note(crate::spend::Price::Free).unwrap();
            assert!(free.contains("too late") && free.contains("nothing was billed"), "{free}");
            let paid = late_cancellation_note(crate::spend::Price::Unverified).unwrap();
            assert!(paid.contains("billed") && !paid.contains("nothing was billed"), "{paid}");
        });
    }

    #[test]
    fn a_padded_model_argument_is_trimmed_like_the_setting_is() {
        assert_eq!(requested_model(&json!({"model": "  SDXL-Turbo \n"})).unwrap(), Some("SDXL-Turbo"));
        assert_eq!(requested_model(&json!({"model": "flux-2-pro"})).unwrap(), Some("flux-2-pro"));
        assert_eq!(requested_model(&json!({})).unwrap(), None);
    }
}
