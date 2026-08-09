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

#[derive(Debug, Clone, Default)]
pub struct VideoRequest {
    pub prompt: String,
    pub model: String,
    /// Normalized, so a provider taking pixel pairs and one taking `16:9` can
    /// both be handed the same request. Runway forced this: its ratios are
    /// `1280:720`-style pixel pairs, which is a seventh geometry model across
    /// six providers, and `Aspect` already holds a width and a height.
    pub aspect: Option<crate::provider::Aspect>,
    pub resolution: Option<String>,
    pub negative_prompt: Option<String>,
    /// A still to animate. Supplying one makes this image-to-video.
    pub image: Option<String>,
    /// Seconds of output. The one parameter where a wrong value is expensive
    /// rather than annoying, since every video provider bills per second.
    pub duration: Option<u32>,
    pub seed: Option<u64>,
}

/// Veo, as of the 3.1 family.
///
/// Durations are 4, 6 and 8 — which answers a question the roadmap left open
/// ("is 8 s a hard limit?") with a no, and is exactly the sort of fact that
/// belongs in a value rather than in prose nobody re-reads.
pub const CAPABILITIES: crate::provider::VideoCapabilities = crate::provider::VideoCapabilities {
    provider: "google",
    tagline: "Veo. Native audio, and the only video lane whose output carries a pixel watermark that survives re-encoding.",
    aspect: crate::provider::AspectSupport::Named(&["16:9", "9:16"]),
    duration: crate::provider::DurationSupport::Named(&[4, 6, 8]),
    image_to_video: true,
    text_to_video: true,
    // True of the family; `veo-lite` rejects one outright and says so before
    // spending a round trip — see `start_video`.
    negative_prompt: true,
    resolution: true,
    // Google exposes no seed on video any more than on images.
    seed: false,
    provenance: crate::provider::Provenance::SynthIdAndC2pa,
};

/// One poll of a render in flight.
pub enum VideoStatus {
    /// Still working. Carries whatever progress text the API offered.
    Pending,
    Done(Vec<u8>),
}

impl Client {
    /// Kicks off a render and returns the operation name to poll.
    ///
    /// Kept as an inherent method as well as a trait one: `mcp.rs` and the tests
    /// hold a concrete `genai::Client`, and making them go through the trait
    /// would be ceremony rather than clarity.
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
            // Sniffed, not guessed from the name — a still being animated is
            // exactly the sort of file that arrives from a screenshot tool with
            // an extension that does not match its bytes, and the declared type
            // is what Veo validates against.
            let mime = crate::sniff_mime(&bytes).unwrap_or("image/png");
            instance.insert(
                "image".into(),
                json!({ "bytesBase64Encoded": STANDARD.encode(&bytes), "mimeType": mime }),
            );
        }

        let mut parameters = serde_json::Map::new();
        if let Some(v) = &req.aspect {
            parameters.insert("aspectRatio".into(), json!(v.to_string()));
        }
        if let Some(v) = &req.resolution {
            parameters.insert("resolution".into(), json!(v));
        }
        if let Some(v) = &req.negative_prompt {
            parameters.insert("negativePrompt".into(), json!(v));
        }
        if let Some(seconds) = req.duration {
            parameters.insert("durationSeconds".into(), json!(seconds));
        }

        Ok(json!({
            "instances": [Value::Object(instance)],
            "parameters": Value::Object(parameters),
        }))
    }

    fn start_operation(&self, model: &str, body: &Value) -> Result<String> {
        let url = format!("{}/models/{model}:predictLongRunning", self.base());
        // Deliberately not retried (see `retry`): this is the call that starts
        // billing, and a retry of a request that in fact succeeded buys a second
        // render nobody asked for.
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
        let url = format!("{}/{operation}", self.base());
        // Retried: a poll spends nothing, and this is the request a render can
        // be lost on — minutes into a wait, one 502 used to abandon it.
        let response = crate::retry::send_idempotent("polling the render", || {
            self.http().get(&url).header("x-goog-api-key", self.key())
        })
        .context("polling the video operation")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        response.json().context("parsing poll response")
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
        let response = crate::retry::send_idempotent("downloading the video", || {
            self.http().get(uri).header("x-goog-api-key", self.key())
        })
        .context("downloading the rendered video")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        Ok(response.bytes().context("reading video bytes")?.to_vec())
    }
}

/// The operation id, and the one command that turns it back into a file.
///
/// Printed the moment the render starts, before any waiting — which is the
/// whole point. Previously the id appeared only in the message announcing the
/// 15-minute deadline, so every other way of leaving the wait lost it: a single
/// 502, a closed laptop, a Ctrl-C, a dropped connection. Minutes of billed Veo
/// output, unreachable, because the one string needed to fetch it was never
/// shown. Nothing about this is expensive; it was simply never printed.
pub fn resume_notice(operation: &str) -> String {
    format!(
        "Render started; this usually takes 1-3 minutes.\n\
         If this command is interrupted, the render continues and can be \
         collected with:\n\n  lucida check {operation}\n"
    )
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

impl crate::provider::VideoProvider for Client {
    fn start(&self, req: &VideoRequest) -> Result<String> {
        self.start_video(req)
    }

    fn poll(&self, operation: &str) -> Result<VideoStatus> {
        self.poll_video(operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, serve};

    fn request(model: &str) -> VideoRequest {
        VideoRequest {
            prompt: "a fox running".into(),
            model: model.into(),
            aspect: crate::provider::Aspect::parse("16:9").ok(),
            ..Default::default()
        }
    }

    #[test]
    fn starting_a_render_resolves_the_alias_and_returns_the_operation() {
        let server = serve(vec![Reply::json(r#"{"name":"operations/xyz"}"#)]);
        let operation = Client::recorded(server.url())
            .start_video(&request("veo"))
            .unwrap();
        assert_eq!(operation, "operations/xyz");

        let requests = server.finish();
        assert_eq!(
            requests[0].path,
            "/models/veo-3.1-fast-generate-preview:predictLongRunning"
        );
        assert_eq!(requests[0].header("x-goog-api-key"), Some("test-key"));
        let body = requests[0].json();
        assert_eq!(body["instances"][0]["prompt"], "a fox running");
        assert_eq!(body["parameters"]["aspectRatio"], "16:9");
    }

    /// The lite guard runs before any HTTP: aimed at a port nothing listens on,
    /// it must still answer with the models that would work.
    #[test]
    fn lite_rejects_a_negative_prompt_before_spending_a_round_trip() {
        let mut req = request("veo-lite");
        req.negative_prompt = Some("rain".into());
        let error = Client::recorded("http://127.0.0.1:1")
            .start_video(&req)
            .unwrap_err()
            .to_string();
        assert!(error.contains("veo-standard"), "must name a way forward: {error}");
    }

    #[test]
    fn a_pending_poll_downloads_nothing() {
        let server = serve(vec![Reply::json(r#"{"done":false}"#)]);
        let status = Client::recorded(server.url()).poll_video("operations/xyz").unwrap();
        assert!(matches!(status, VideoStatus::Pending));
        assert_eq!(server.finish().len(), 1);
    }

    /// The finished video lives at a URI that itself requires the API key —
    /// measured, and the opposite of BFL's signed URLs, which must NOT get one.
    /// Both are deliberate, which is exactly why both are pinned.
    #[test]
    fn the_video_download_carries_the_key_because_veo_urls_require_it() {
        let done = r#"{"done":true,"response":{"generateVideoResponse":{
            "generatedSamples":[{"video":{"uri":"{{server}}/files/v1:download?alt=media"}}]}}}"#;
        let server = serve(vec![
            Reply::json(done),
            Reply::bytes("video/mp4", b"mp4-bytes"),
        ]);

        let status = Client::recorded(server.url()).poll_video("operations/xyz").unwrap();
        match status {
            VideoStatus::Done(bytes) => assert_eq!(bytes, b"mp4-bytes"),
            VideoStatus::Pending => panic!("expected the render to be done"),
        }

        let requests = server.finish();
        assert_eq!(requests[0].path, "/operations/xyz");
        assert_eq!(requests[1].path, "/files/v1:download?alt=media");
        assert_eq!(requests[1].header("x-goog-api-key"), Some("test-key"));
    }
}
