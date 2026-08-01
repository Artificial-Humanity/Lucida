//! Veo video generation.
//!
//! Video does not work like images. Instead of returning bytes, Veo starts a
//! long-running operation and hands back an operation name to poll until it
//! reports done, after which the result is fetched from a URL that itself needs
//! the API key. Three round trips minimum, and a render can take minutes.

use crate::genai::{Client, explain_error};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const DEFAULT_VIDEO_MODEL: &str = "veo-3.1-fast-generate-preview";

/// Video is billed per second of output, so `fast` is the default rather than
/// the standard model.
pub const VIDEO_ALIASES: &[(&str, &str)] = &[
    ("veo", "veo-3.1-fast-generate-preview"),
    ("veo-fast", "veo-3.1-fast-generate-preview"),
    ("veo-standard", "veo-3.1-generate-preview"),
    ("veo-quality", "veo-3.1-generate-preview"),
    ("veo-lite", "veo-3.1-lite-generate-preview"),
];

pub fn resolve_video_model(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    VIDEO_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, id)| (*id).to_string())
        .unwrap_or_else(|| input.trim().to_string())
}

#[derive(Debug, Clone)]
pub struct VideoRequest {
    pub prompt: String,
    pub model: String,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    pub negative_prompt: Option<String>,
    /// A still to animate. Supplying one makes this image-to-video.
    pub image: Option<String>,
}

/// One poll of a render in flight.
pub enum VideoStatus {
    /// Still working. Carries whatever progress text the API offered.
    Pending,
    Done(Vec<u8>),
}

impl Client {
    /// Blocking generate: start, poll to completion, download. Used by the CLI,
    /// where waiting is fine. The MCP server drives `start_video`/`poll_video`
    /// separately, because a render outlasts a tool call.
    pub fn generate_video(&self, req: &VideoRequest) -> Result<Vec<u8>> {
        let operation = self.start_video(req)?;
        eprintln!("Render started; this usually takes 1-3 minutes.");
        let done = self.await_operation(&operation)?;
        self.fetch_video(&done)
    }

    /// Kicks off a render and returns the operation name to poll.
    pub fn start_video(&self, req: &VideoRequest) -> Result<String> {
        let model = resolve_video_model(&req.model);

        // The lite model rejects negativePrompt outright. Catching it here saves
        // a round trip and names the fix, which the API's own message does not.
        if req.negative_prompt.is_some() && model.contains("lite") {
            bail!(
                "`{model}` does not support a negative prompt.\n\n\
                 Use `veo` (fast) or `veo-standard` instead, or drop the negative \
                 prompt to stay on lite."
            );
        }

        let body = self.build_video_body(req)?;
        self.start_operation(&model, &body)
    }

    /// Checks a render once, without blocking.
    pub fn poll_video(&self, operation: &str) -> Result<VideoStatus> {
        let payload = self.poll_once(operation)?;

        if let Some(error) = payload.get("error") {
            let message = error["message"].as_str().unwrap_or("unknown error");
            bail!("the render failed: {message}");
        }

        if payload["done"].as_bool().unwrap_or(false) {
            Ok(VideoStatus::Done(self.fetch_video(&payload)?))
        } else {
            Ok(VideoStatus::Pending)
        }
    }

    fn build_video_body(&self, req: &VideoRequest) -> Result<Value> {
        let mut instance = serde_json::Map::new();
        instance.insert("prompt".into(), json!(req.prompt));
        if let Some(path) = &req.image {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading source image {path}"))?;
            let mime = if path.to_ascii_lowercase().ends_with(".jpg")
                || path.to_ascii_lowercase().ends_with(".jpeg")
            {
                "image/jpeg"
            } else {
                "image/png"
            };
            instance.insert(
                "image".into(),
                json!({ "bytesBase64Encoded": STANDARD.encode(&bytes), "mimeType": mime }),
            );
        }

        let mut parameters = serde_json::Map::new();
        if let Some(v) = &req.aspect_ratio {
            parameters.insert("aspectRatio".into(), json!(v));
        }
        if let Some(v) = &req.resolution {
            parameters.insert("resolution".into(), json!(v));
        }
        if let Some(v) = &req.negative_prompt {
            parameters.insert("negativePrompt".into(), json!(v));
        }

        Ok(json!({
            "instances": [Value::Object(instance)],
            "parameters": Value::Object(parameters),
        }))
    }

    fn start_operation(&self, model: &str, body: &Value) -> Result<String> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:predictLongRunning"
        );
        let response = self
            .http()
            .post(&url)
            .header("x-goog-api-key", self.key())
            .json(body)
            .send()
            .context("starting the video operation")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        let payload: Value = response.json().context("parsing operation response")?;
        payload["name"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("no operation name in response: {payload}"))
    }

    /// A single non-blocking check of an operation.
    fn poll_once(&self, operation: &str) -> Result<Value> {
        let url = format!("https://generativelanguage.googleapis.com/v1beta/{operation}");
        let response = self
            .http()
            .get(&url)
            .header("x-goog-api-key", self.key())
            .send()
            .context("polling the video operation")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        response.json().context("parsing poll response")
    }

    /// Polls until the operation reports done. Veo renders are slow and the wait
    /// is unpredictable, so this backs off rather than hammering.
    fn await_operation(&self, operation: &str) -> Result<Value> {
        let started = Instant::now();
        let deadline = Duration::from_secs(900);
        let mut interval = Duration::from_secs(5);

        loop {
            if started.elapsed() > deadline {
                bail!(
                    "gave up after {} minutes; the render may still finish. \
                     Poll it with: lucida check {operation}",
                    deadline.as_secs() / 60
                );
            }

            std::thread::sleep(interval);
            interval = (interval * 2).min(Duration::from_secs(30));

            let payload = self.poll_once(operation)?;

            if let Some(error) = payload.get("error") {
                let message = error["message"].as_str().unwrap_or("unknown error");
                bail!("the render failed: {message}");
            }

            if payload["done"].as_bool().unwrap_or(false) {
                eprintln!("Render finished in {}s.", started.elapsed().as_secs());
                return Ok(payload);
            }

            eprintln!("  still rendering ({}s elapsed)…", started.elapsed().as_secs());
        }
    }

    /// Digs the video out of a completed operation.
    ///
    /// The payload nests differently depending on model and API version, and it
    /// may carry either a URL or inline bytes, so this searches the tree for
    /// whichever turns up rather than hardcoding one path.
    pub(crate) fn fetch_video(&self, done: &Value) -> Result<Vec<u8>> {
        if let Some(encoded) = find_key(done, "bytesBase64Encoded").and_then(|v| v.as_str()) {
            return STANDARD
                .decode(encoded)
                .context("decoding inline video bytes");
        }

        let uri = find_key(done, "uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow!("completed operation contained neither a video URI nor inline bytes: {done}")
            })?;

        // The download URL is itself authenticated.
        let response = self
            .http()
            .get(uri)
            .header("x-goog-api-key", self.key())
            .send()
            .context("downloading the rendered video")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        Ok(response.bytes().context("reading video bytes")?.to_vec())
    }
}

/// Depth-first search for the first value under `target`, at any depth.
fn find_key<'a>(value: &'a Value, target: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(target) {
                return Some(found);
            }
            map.values().find_map(|v| find_key(v, target))
        }
        Value::Array(items) => items.iter().find_map(|v| find_key(v, target)),
        _ => None,
    }
}
