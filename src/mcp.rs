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
use crate::video::{DEFAULT_VIDEO_MODEL, VideoRequest, VideoStatus};
use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2024-11-05";

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    eprintln!("lucida MCP server ready (default image model: {DEFAULT_MODEL})");

    for line in stdin.lock().lines() {
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

        // No id means a notification: act on it, but never reply. Replying to a
        // notification is a protocol violation some clients treat as fatal.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };

        let method = request["method"].as_str().unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = match dispatch(method, &params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(e) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": e.to_string() }
            }),
        };

        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }

    Ok(())
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
            let mut notes: Vec<&str> = Vec::new();
            if caps.seed {
                notes.push("seed");
            }
            if caps.negative_prompt {
                notes.push("negative prompt");
            }
            if caps.references {
                notes.push("editing");
            }
            if caps.steps {
                notes.push("steps/guidance");
            }
            if !caps.size {
                notes.push("NO size control");
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
    match names.split_last() {
        None => "no providers".to_string(),
        Some((last, [])) => (*last).to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

fn image_schema() -> Value {
    json!({
        "name": "generate_image",
        "description": format!(
            "Generate an image and write it to disk. Returns the path written.\n\n\
             Four providers are available and the choice matters — cost, speed, \
             what you can ask for, and what ends up embedded in the file all \
             differ:\n{}\n\n\
             The provider is inferred from the model id; pass `provider` to be \
             explicit. Not every parameter works on every provider, and the ones \
             that do not are a hard error naming one that does — never a silent \
             drop. Call image_providers for live capabilities and which are \
             actually reachable.\n\n\
             Pass reference_images to edit an existing picture ({}); pass mask as \
             well to concentrate the change on part of one ({}, advisory).",
            provider_summary(),
            providers_where(|c| c.references),
            providers_where(|c| c.mask)
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
                        "Model id or alias. Defaults to {DEFAULT_MODEL} on google, \
                         `klein` on comfyui, flux-2-pro on bfl, core on stability."
                    )
                },
                "aspect_ratio": {
                    "type": "string",
                    // Deliberately not an enum: the providers with named ratios
                    // disagree about which, and the others take any ratio at all.
                    "description": format!(
                        "W:H, e.g. 16:9. google accepts only: {}. stability accepts a \
                         DIFFERENT nine: {}. comfyui and bfl accept any ratio.",
                        genai::ASPECT_RATIOS.join(", "),
                        crate::stability::ASPECT_RATIOS.join(", ")
                    )
                },
                "size": {
                    "type": "string",
                    "description": "Long edge in pixels, or a tier (1K, 2K, 4K). google rounds to a tier; comfyui and bfl use the number. NOT supported by stability at all, which renders a fixed size — passing it there is an error."
                },
                "negative_prompt": {
                    "type": "string",
                    "description": "What to keep out of the picture. comfyui and stability only — google's image models and every FLUX endpoint lack the concept, so passing it there is an error rather than a no-op."
                },
                "seed": {
                    "type": "integer",
                    "description": "Renders the same image again. comfyui, bfl and stability; google exposes none, so results there cannot be reproduced. comfyui is verified pixel-identical across runs."
                },
                "steps": {
                    "type": "integer",
                    "description": "Sampling steps. comfyui, and on bfl only flux-2-flex and flux-dev."
                },
                "guidance": {
                    "type": "number",
                    "description": "How closely to follow the prompt. comfyui, and on bfl only flux-2-flex and flux-dev."
                },
                "mask": {
                    "type": "string",
                    "description": format!(
                        "Path to a PNG concentrating an edit on part of the image: \
                         its TRANSPARENT pixels are what changes. Supported by {}, \
                         and requires reference_images. ADVISORY, not binding — \
                         measured at about twice the change inside the mask as \
                         outside, with the rest of the picture regenerated too. \
                         Composite over the original if untouched pixels matter.",
                        providers_where(|c| c.mask)
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
fn start_video_schema() -> Value {
    json!({
        "name": "start_video",
        "description": concat!(
            "Begin rendering a video with Veo. Returns immediately with an operation ",
            "id; the render itself takes 1-3 minutes. Poll it with check_video. ",
            "Video is billed per second of output and costs considerably more than ",
            "an image, so confirm with the user before calling this. Google only; ",
            "output carries a SynthID watermark and a C2PA manifest."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "What to film, including any camera movement." },
                "image": { "type": "string", "description": "Optional path to a still image to animate instead of generating from text alone." },
                "aspect_ratio": { "type": "string", "enum": ["16:9", "9:16"] },
                "resolution": { "type": "string", "description": "e.g. 720p or 1080p" },
                "negative_prompt": { "type": "string", "description": "What to keep out of the shot. Not supported on veo-lite." },
                "model": {
                    "type": "string",
                    "description": format!("Model id or alias: veo, veo-standard, veo-lite. Defaults to {DEFAULT_VIDEO_MODEL}.")
                }
            },
            "required": ["prompt"]
        }
    })
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
                "output_path": { "type": "string", "description": "Where to write the finished video. An .mp4 extension is applied if missing." }
            },
            "required": ["operation", "output_path"]
        }
    })
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

fn generate_image(args: &Value) -> Result<String> {
    let prompt = args["prompt"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("`prompt` is required"))?;
    let output_path = args["output_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("`output_path` is required"))?;

    let backend = match args["provider"].as_str() {
        Some(name) => Backend::parse(name)?,
        None => match args["model"].as_str() {
            Some(model) => infer_backend(model),
            None => Backend::Google,
        },
    };

    let model = args["model"]
        .as_str()
        .unwrap_or(match backend {
            Backend::Google => DEFAULT_MODEL,
            Backend::ComfyUi => "klein",
            Backend::Bfl => bfl::DEFAULT_MODEL,
            Backend::Stability => stability::DEFAULT_MODEL,
            Backend::OpenAi => openai::DEFAULT_MODEL,
        })
        .to_string();

    let request = ImageRequest {
        prompt: prompt.to_string(),
        model,
        aspect: args["aspect_ratio"].as_str().map(Aspect::parse).transpose()?,
        size: args["size"].as_str().map(Size::parse).transpose()?,
        references: args["reference_images"]
            .as_array()
            .map(|refs| {
                refs.iter()
                    .filter_map(|r| r.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        negative_prompt: args["negative_prompt"].as_str().map(str::to_string),
        mask: args["mask"].as_str().map(str::to_string),
        seed: args["seed"].as_u64(),
        steps: args["steps"].as_u64().map(|n| n as u32),
        guidance: args["guidance"].as_f64().map(|n| n as f32),
    };

    // The whole point of the abstraction, from an agent's perspective: a
    // parameter this provider cannot honour stops here, with a message naming one
    // that can, rather than being dropped on the way to the API. Checked before a
    // client exists, so a missing credential never masks the real objection.
    let caps = capabilities_for(backend, &request.model);
    caps.check(&request)?;

    let provider = open(backend)?;
    let image = provider.generate(&request)?;

    // Providers pick the output format themselves, so the requested extension may
    // not match the bytes. Correct it and say so, rather than handing back a file
    // whose name lies about its contents.
    let requested = std::path::Path::new(output_path);
    let destination = crate::correct_extension(requested, &image.mime_type);
    let renamed = destination != requested;
    let written = crate::write_image(&destination, &image.bytes)?;

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
    if let Some(commentary) = &image.commentary {
        if !commentary.is_empty() {
            text.push_str(&format!("\n\nModel commentary: {commentary}"));
        }
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

        let provider = match open(backend) {
            Ok(provider) => provider,
            Err(e) => {
                out.push_str(&format!("unavailable: {e:#}\n\n"));
                continue;
            }
        };

        let caps = provider.capabilities();
        match provider.list_models() {
            Ok(models) if models.is_empty() => out.push_str("reachable, but reports no models\n"),
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
             steps: {}  |  guidance: {}\n\
             output carries: {}\n\n",
            caps.seed,
            caps.negative_prompt,
            caps.references,
            caps.steps,
            caps.guidance,
            caps.provenance.describe()
        ));
    }

    out
}

fn start_video(args: &Value) -> Result<String> {
    let prompt = args["prompt"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("`prompt` is required"))?;

    let request = VideoRequest {
        prompt: prompt.to_string(),
        model: args["model"]
            .as_str()
            .unwrap_or(DEFAULT_VIDEO_MODEL)
            .to_string(),
        aspect_ratio: args["aspect_ratio"].as_str().map(str::to_string),
        resolution: args["resolution"].as_str().map(str::to_string),
        negative_prompt: args["negative_prompt"].as_str().map(str::to_string),
        image: args["image"].as_str().map(str::to_string),
    };

    let operation = genai::Client::from_env()?.start_video(&request)?;
    Ok(format!(
        "Render started.\n\noperation: {operation}\n\n\
         It typically takes 1-3 minutes. Wait about 30 seconds, then call \
         check_video with this operation id and an output path."
    ))
}

fn check_video(args: &Value) -> Result<String> {
    let operation = args["operation"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("`operation` is required"))?;
    let output_path = args["output_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("`output_path` is required"))?;

    match genai::Client::from_env()?.poll_video(operation)? {
        VideoStatus::Pending => Ok(
            "Still rendering. Wait roughly 30 seconds before checking again — \
             polling faster will not make it finish sooner."
                .to_string(),
        ),
        VideoStatus::Done(bytes) => {
            let requested = std::path::Path::new(output_path);
            let destination = crate::correct_extension(requested, "video/mp4");
            let written = crate::write_image(&destination, &bytes)?;
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
}
