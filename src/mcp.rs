//! MCP server over stdio.
//!
//! The protocol is newline-delimited JSON-RPC 2.0, which is small enough that a
//! dependency would cost more than it saves. It also buys the single most useful
//! property for a stdio server: nothing reaches stdout unless this file puts it
//! there. In Python, one stray `print` in any transitive import corrupts the
//! stream and the failure looks like a mysterious handshake error.
//!
//! Diagnostics therefore go to stderr, which Claude Code captures as server logs.

use crate::genai::{ASPECT_RATIOS, Client, DEFAULT_MODEL, IMAGE_SIZES, ImageRequest};
use crate::video::{DEFAULT_VIDEO_MODEL, VideoRequest, VideoStatus};
use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2024-11-05";

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    eprintln!("mediagen MCP server ready (model default: {DEFAULT_MODEL})");

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
            "serverInfo": { "name": "mediagen", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": [tool_schema(), start_video_schema(), check_video_schema()]
        })),
        "tools/call" => call_tool(params),
        other => anyhow::bail!("unknown method: {other}"),
    }
}

fn tool_schema() -> Value {
    json!({
        "name": "generate_image",
        "description": concat!(
            "Generate an image with Google's Gemini image models and write it to disk. ",
            "Pass reference_images to edit or restyle existing pictures instead of ",
            "creating one from scratch. Returns the path written. ",
            "Note: every generated image carries an invisible SynthID watermark."
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
                "aspect_ratio": {
                    "type": "string",
                    "enum": ASPECT_RATIOS,
                    "description": "Defaults to the model's own choice when omitted."
                },
                "size": {
                    "type": "string",
                    "enum": IMAGE_SIZES,
                    "description": "Render resolution. Larger costs more and takes longer."
                },
                "model": {
                    "type": "string",
                    "description": format!("Model id. Defaults to {DEFAULT_MODEL}.")
                },
                "reference_images": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Paths to existing images to condition on, for editing or style matching."
                }
            },
            "required": ["prompt", "output_path"]
        }
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
            "an image, so confirm with the user before calling this."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "What to film, including any camera movement." },
                "image": { "type": "string", "description": "Optional path to a still image to animate instead of generating from text alone." },
                "aspect_ratio": { "type": "string", "enum": ["16:9", "9:16"] },
                "resolution": { "type": "string", "description": "e.g. 720p or 1080p" },
                "negative_prompt": { "type": "string", "description": "What to keep out of the shot." },
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

fn call_tool(params: &Value) -> Result<Value> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = &params["arguments"];

    match name {
        "generate_image" => {}
        "start_video" => return wrap(start_video(args)),
        "check_video" => return wrap(check_video(args)),
        other => anyhow::bail!("unknown tool: {other}"),
    }

    let prompt = args["prompt"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("`prompt` is required"))?;
    let output_path = args["output_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("`output_path` is required"))?;

    let request = ImageRequest {
        prompt: prompt.to_string(),
        model: args["model"].as_str().unwrap_or(DEFAULT_MODEL).to_string(),
        aspect_ratio: args["aspect_ratio"].as_str().map(str::to_string),
        image_size: args["size"].as_str().map(str::to_string),
        references: args["reference_images"]
            .as_array()
            .map(|refs| {
                refs.iter()
                    .filter_map(|r| r.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    };

    wrap(run(&request, output_path))
}

/// Errors are returned as isError content rather than as JSON-RPC errors, so the
/// model sees the message and can act on it (fix the prompt, pick another model,
/// wait longer) instead of the call simply failing.
fn wrap(result: Result<String>) -> Result<Value> {
    match result {
        Ok(text) => Ok(json!({ "content": [{ "type": "text", "text": text }] })),
        Err(e) => Ok(json!({
            "content": [{ "type": "text", "text": format!("{e:#}") }],
            "isError": true
        })),
    }
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

    let operation = Client::from_env()?.start_video(&request)?;
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

    match Client::from_env()?.poll_video(operation)? {
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

fn run(request: &ImageRequest, output_path: &str) -> Result<String> {
    let client = Client::from_env()?;
    let image = client.generate(request)?;

    // Gemini picks the output format itself, so the requested extension may not
    // match the bytes. Correct it and say so, rather than handing back a file
    // whose name lies about its contents.
    let requested = std::path::Path::new(output_path);
    let destination = crate::correct_extension(requested, &image.mime_type);
    let renamed = destination != requested;
    let written = crate::write_image(&destination, &image.bytes)?;

    let mut text = format!(
        "Wrote {} ({} KB, {})",
        written.display(),
        image.bytes.len() / 1024,
        image.mime_type
    );
    if renamed {
        text.push_str(&format!(
            "\n\nNote: the model returned {}, so the extension was corrected \
             (requested {}). Use the path above, not the requested one.",
            image.mime_type,
            requested.display()
        ));
    }
    if let Some(commentary) = &image.commentary {
        if !commentary.is_empty() {
            text.push_str(&format!("\n\nModel commentary: {commentary}"));
        }
    }
    Ok(text)
}
