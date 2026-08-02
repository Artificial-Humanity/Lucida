//! Thin client over the Gemini REST image endpoints.
//!
//! There is no official Google GenAI SDK for Rust, and for image generation none
//! is needed: the whole surface is one POST returning base64 image bytes. Talking
//! to REST directly also sidesteps the churn in the Python SDK, which has a
//! breaking 3.0 on the way.

use crate::provider::{
    AspectSupport, Capabilities, GeneratedImage, ImageProvider, ImageRequest, Provenance,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const API_ROOT: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Nano Banana 2. The Imagen family is scheduled for shutdown on 2026-08-17, so
/// it is deliberately not the default here.
pub const DEFAULT_MODEL: &str = "gemini-3.1-flash-image";

/// The only ratios Google accepts. Published as capabilities so a request for
/// anything else fails before it is sent, naming a provider that takes free
/// dimensions.
pub const ASPECT_RATIOS: &[&str] = &[
    "1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
];

/// Friendly aliases for the Gemini image models.
///
/// "Nano Banana" is Google's codename for this family and the name almost
/// everyone actually uses — but it appears nowhere in the API, where the models
/// are `gemini-*-image`. Accepting both spellings saves a trip to the docs.
pub const MODEL_ALIASES: &[(&str, &str)] = &[
    ("banana", "gemini-3.1-flash-image"),
    ("nano-banana", "gemini-3.1-flash-image"),
    ("flash", "gemini-3.1-flash-image"),
    ("banana-lite", "gemini-3.1-flash-lite-image"),
    ("lite", "gemini-3.1-flash-lite-image"),
    ("banana-pro", "gemini-3-pro-image"),
    ("nano-banana-pro", "gemini-3-pro-image"),
    ("pro", "gemini-3-pro-image"),
    ("banana-1", "gemini-2.5-flash-image"),
];

/// Maps an alias to a real model id, passing anything unrecognised straight
/// through so a brand-new model id works the day it ships.
pub fn resolve_model(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    MODEL_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, id)| (*id).to_string())
        .unwrap_or_else(|| input.trim().to_string())
}

pub struct Client {
    api_key: String,
    http: reqwest::blocking::Client,
}

impl Client {
    /// GOOGLE_API_KEY is checked first, then GEMINI_API_KEY, which is what the
    /// Google SDKs themselves look for. Both fall back to the config file.
    pub fn from_env() -> Result<Self> {
        let api_key = crate::config::var("GOOGLE_API_KEY")
            .or_else(|| crate::config::var("GEMINI_API_KEY"))
            .ok_or_else(no_key)?;

        if api_key.trim().is_empty() {
            bail!("GOOGLE_API_KEY is set but empty");
        }

        let http = reqwest::blocking::Client::builder()
            // 4K renders are genuinely slow; the default 30s times out under load.
            .timeout(Duration::from_secs(300))
            .build()
            .context("building HTTP client")?;

        Ok(Self { api_key, http })
    }

    /// Shared HTTP client, for sibling modules that speak other endpoints.
    pub(crate) fn http(&self) -> &reqwest::blocking::Client {
        &self.http
    }

    pub(crate) fn key(&self) -> &str {
        &self.api_key
    }

    /// Image-capable models this key can actually see. Free, and the quickest way
    /// to confirm a key works without spending anything.
    fn image_models(&self) -> Result<Vec<String>> {
        let response = self
            .http
            .get(format!("{API_ROOT}/models?pageSize=200"))
            .header("x-goog-api-key", &self.api_key)
            .send()
            .context("listing models")?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status, &text));
        }

        let payload: Value = response.json().context("parsing model list")?;
        let mut names: Vec<String> = payload["models"]
            .as_array()
            .map(|models| {
                models
                    .iter()
                    .filter_map(|m| m["name"].as_str())
                    .filter_map(|n| n.strip_prefix("models/"))
                    .filter(|n| n.contains("image") || n.contains("imagen"))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names.dedup();
        Ok(names)
    }
}

pub const CAPABILITIES: Capabilities = Capabilities {
    provider: "google",
    tagline: "Highest quality, costs money per image, seconds to render. The only provider whose output carries a pixel watermark that survives re-encoding.",
    // Ten named ratios and nothing between them.
    aspect: AspectSupport::Named(ASPECT_RATIOS),
    // 1K / 2K / 4K tiers.
    size: true,
    // Google exposes no seed on any image model, so nothing here can express
    // "give me that result again".
    seed: false,
    // Veo takes one; the image models do not.
    negative_prompt: false,
    references: true,
    mask: false,
    workflow: false,
    steps: false,
    guidance: false,
    provenance: Provenance::SynthIdAndC2pa,
};

impl ImageProvider for Client {
    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn list_models(&self) -> Result<Vec<String>> {
        self.image_models()
    }

    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage> {
        let model = resolve_model(&req.model);

        // Imagen speaks a different endpoint entirely (`:predict`, with an
        // instances/parameters body). Rather than build a second call shape for a
        // family Google shuts down on 2026-08-17, say so plainly.
        if model.starts_with("imagen") {
            bail!(
                "`{model}` belongs to the Imagen family, which uses a different API \
                 endpoint that lucida does not implement.\n\n\
                 Imagen is scheduled for shutdown on 2026-08-17. Use a Gemini image \
                 model instead — `banana` (fast), `banana-pro` (highest quality), or \
                 `banana-lite` (cheapest)."
            );
        }

        let mut parts: Vec<Value> = vec![json!({ "text": req.prompt })];

        for path in &req.references {
            let (mime, data) = read_image_as_inline(path)?;
            parts.push(json!({ "inlineData": { "mimeType": mime, "data": data } }));
        }

        // The normalized request carries a ratio and a pixel count; Google speaks
        // named ratios and three tier names, so translate rather than pass through.
        let mut image_config = serde_json::Map::new();
        if let Some(aspect) = req.aspect {
            image_config.insert("aspectRatio".into(), json!(aspect.to_string()));
        }
        if let Some(size) = req.size {
            image_config.insert("imageSize".into(), json!(size.tier_name()));
        }

        let mut generation_config = serde_json::Map::new();
        generation_config.insert("responseModalities".into(), json!(["TEXT", "IMAGE"]));
        if !image_config.is_empty() {
            generation_config.insert("imageConfig".into(), Value::Object(image_config));
        }

        let body = json!({
            "contents": [{ "parts": parts }],
            "generationConfig": generation_config,
        });

        // The key travels as a header, never as a `?key=` query parameter, which
        // would leak it into shell history, proxy logs and crash reports.
        let url = format!("{API_ROOT}/models/{model}:generateContent");
        let response = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .context("calling the Gemini API")?;

        let status = response.status();
        let payload: Value = if status.is_success() {
            response.json().context("parsing API response")?
        } else {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        };

        extract_image(&payload)
    }
}

/// The missing-key message.
///
/// Worth writing carefully, because the most common way to hit it is also the
/// most confusing: an MCP server launched by a GUI application inherits no login
/// shell, so a key exported in a shell profile is genuinely absent even though
/// the same binary works from a terminal. Telling that user to open a fresh
/// shell — which the previous version did — sends them somewhere with no answer.
fn no_key() -> anyhow::Error {
    let config_hint = match crate::config::source() {
        Some(path) => format!(
            "A config file was read from {}, but it sets neither key.",
            path.display()
        ),
        None => match crate::config::preferred_path() {
            Some(path) => format!(
                "No config file was found. Create one with `lucida config --init`, \
                 which writes {}.",
                path.display()
            ),
            None => "No config file was found.".to_string(),
        },
    };

    anyhow!(
        "no API key found: set GOOGLE_API_KEY (or GEMINI_API_KEY).\n\n\
         {config_hint}\n\n\
         If the key IS exported in your shell profile and this still fails, the \
         process was almost certainly not started from a shell — a GUI-launched \
         app, and any MCP server it spawns, inherits no login environment. The \
         config file exists for exactly that case. Run `lucida config` to see \
         what this process can actually see."
    )
}

fn read_image_as_inline(path: &str) -> Result<(String, String)> {
    let bytes = std::fs::read(path).with_context(|| format!("reading reference image {path}"))?;
    let mime = match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "image/png",
    };
    Ok((mime.to_string(), STANDARD.encode(&bytes)))
}

fn extract_image(payload: &Value) -> Result<GeneratedImage> {
    let parts = payload["candidates"][0]["content"]["parts"]
        .as_array()
        .ok_or_else(|| {
            // A blocked prompt returns a well-formed response with no parts, so
            // check for that before complaining about the shape.
            let reason = payload["candidates"][0]["finishReason"]
                .as_str()
                .or_else(|| payload["promptFeedback"]["blockReason"].as_str());
            match reason {
                // Counterintuitive enough to be worth spelling out: recitation
                // fires when the output would too closely reproduce training
                // data, which *generic* prompts trigger more readily than
                // elaborate ones. "a red circle on white" has one obvious
                // rendering; a described scene has many.
                Some("IMAGE_RECITATION") => anyhow!(
                    "the model declined to return an image (IMAGE_RECITATION).\n\n\
                     This filter fires when the result would too closely reproduce \
                     training data, and simple, iconic prompts trip it most often — \
                     adding detail usually clears it. Describe materials, lighting, \
                     composition or style rather than naming the object alone."
                ),
                Some(r) => anyhow!("the model returned no image (finish reason: {r})"),
                None => anyhow!("unexpected API response shape: {payload}"),
            }
        })?;

    let mut commentary = None;
    for part in parts {
        if let Some(text) = part["text"].as_str() {
            commentary = Some(text.trim().to_string());
        }
        if let Some(data) = part["inlineData"]["data"].as_str() {
            let bytes = STANDARD.decode(data).context("decoding image payload")?;
            let mime_type = part["inlineData"]["mimeType"]
                .as_str()
                .unwrap_or("image/png")
                .to_string();
            return Ok(GeneratedImage {
                bytes,
                mime_type,
                commentary,
                // Google never surfaces one, so there is nothing to report and no
                // way to reproduce this result.
                seed: None,
            });
        }
    }

    match commentary {
        Some(text) => bail!("the model replied with text instead of an image: {text}"),
        None => bail!("no image data in the API response"),
    }
}

/// Turns the API's raw error bodies into something worth reading. The 429 case is
/// the one that matters: on a free-tier project image generation is not rate
/// limited, it is entirely unavailable, and the stock message does not say so.
pub(crate) fn explain_error(status: u16, body: &str) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = parsed["error"]["message"].as_str().unwrap_or(body).trim();

    match status {
        429 if message.contains("limit: 0") => format!(
            "HTTP 429 — image generation is not available on a free-tier project.\n\n\
             The API reports `limit: 0`, which means no quota exists at all rather \
             than a quota that was used up. Waiting will not help.\n\n\
             Enable billing on the Google Cloud project behind this API key:\n  \
             https://aistudio.google.com/billing\n\n\
             Original message: {message}"
        ),
        429 => format!("HTTP 429 — rate limited. {message}"),
        400 if message.contains("API key not valid") => {
            format!("HTTP 400 — the API key was rejected. {message}")
        }
        403 => format!(
            "HTTP 403 — the key is valid but lacks permission for this model. {message}"
        ),
        404 => format!(
            "HTTP 404 — no such model. Run `mediagen models` to list what this key can see. \
             Note that the Imagen 3 IDs are retired. {message}"
        ),
        _ => format!("HTTP {status} — {message}"),
    }
}
