//! OpenAI — `gpt-image-1`, and the only provider here that can mask.
//!
//! Demoted to last in the roadmap on the grounds that it is a one-off whose
//! parameter model shares little with the others, and that reasoning held: no
//! seed, no negative prompt, no sampler, and a geometry model unlike any of the
//! four before it. It earns its place for one thing the roadmap named correctly
//! from the start — **mask-based editing** — which is the capability that finally
//! forced `ImageRequest` to grow a way of saying "this region of this image".
//!
//! # Two shapes nobody else has
//!
//! **Geometry is three fixed pixel sizes**, not ratios and not free dimensions:
//! `1024x1024`, `1024x1536`, `1536x1024`, plus `auto`. Read out of the API's own
//! validation error. They correspond to 1:1, 2:3 and 3:2, so Lucida presents
//! them as named ratios and translates — but the pixel count is not adjustable,
//! which is the same conclusion Stability forced for a different reason.
//!
//! **Two endpoints with different encodings.** Generation is JSON; editing is
//! `multipart/form-data`, because it carries files. Every other provider uses
//! one encoding for both.
//!
//! # The one genuinely reassuring thing
//!
//! **Unknown parameters are rejected, not ignored.** `seed` and `negative_prompt`
//! both come back as `Unknown parameter`, which is the opposite of Stability
//! silently dropping whatever it does not recognise. Capabilities here could
//! therefore be established by asking rather than by rendering.

use crate::provider::{
    AspectSupport, Capabilities, GeneratedImage, ImageProvider, ImageRequest, Provenance,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const API_ROOT: &str = "https://api.openai.com/v1";

pub const DEFAULT_MODEL: &str = "gpt-image-1";

/// The ratios the three supported pixel sizes correspond to.
pub const ASPECT_RATIOS: &[&str] = &["1:1", "2:3", "3:2"];

/// Quality tier when none is asked for.
///
/// Deliberately not the API's own default of `auto`, which resolves upward and
/// bills accordingly. A stated middling default is friendlier than an unstated
/// expensive one, and this is a provider where the difference is real money.
const DEFAULT_QUALITY: &str = "medium";

pub const MODEL_ALIASES: &[(&str, &str)] = &[
    ("openai", "gpt-image-1"),
    ("gpt-image", "gpt-image-1"),
    ("dalle", "dall-e-3"),
    ("dall-e", "dall-e-3"),
];

pub const KNOWN_MODELS: &[&str] = &["gpt-image-1", "dall-e-3", "dall-e-2"];

pub fn resolve_model(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    MODEL_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, id)| (*id).to_string())
        .unwrap_or(key)
}

pub fn capabilities(model: &str) -> Capabilities {
    let id = resolve_model(model);
    // Only gpt-image-1 takes multiple inputs and a mask; the DALL-E editing
    // surface is narrower and is not implemented here.
    let modern = id == "gpt-image-1";

    Capabilities {
        provider: "openai",
        tagline: "gpt-image-1. Paid, seconds, and the ONLY provider that can mask \
                  an edit to part of an image. No seed and no negative prompt at all.",
        aspect: AspectSupport::Named(ASPECT_RATIOS),
        // Three fixed pixel sizes, chosen by the ratio. The count is not
        // adjustable, so asking for one is an error rather than a rounding.
        size: false,
        // Rejected outright by the API: "Unknown parameter: 'seed'".
        seed: false,
        // Likewise "Unknown parameter: 'negative_prompt'".
        negative_prompt: false,
        references: modern,
        mask: modern,
        steps: false,
        guidance: false,
        // Not yet verified in the bytes for this provider.
        provenance: Provenance::Unverified,
    }
}

pub struct Client {
    key: String,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        let key = crate::config::var("OPENAI_API_KEY").ok_or_else(|| {
            let where_to_put_it = match crate::config::preferred_path() {
                Some(path) => format!(
                    "Set OPENAI_API_KEY, or add it to {} — \
                     `lucida config --set OPENAI_API_KEY` prompts for it and shows \
                     asterisks rather than the value.",
                    path.display()
                ),
                None => "Set OPENAI_API_KEY.".to_string(),
            };
            anyhow!("no OpenAI API key found.\n\n{where_to_put_it}")
        })?;

        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("building HTTP client")?;

        Ok(Self { key, http })
    }

    /// Maps a requested ratio onto one of the three sizes the API accepts.
    ///
    /// `auto` when nothing was asked for, which lets the model choose a shape to
    /// suit the prompt — a genuinely useful default that no other provider here
    /// offers.
    fn size_for(req: &ImageRequest) -> &'static str {
        match req.aspect.map(|a| (a.w, a.h)) {
            None => "auto",
            Some((w, h)) if w == h => "1024x1024",
            Some((w, h)) if w > h => "1536x1024",
            Some(_) => "1024x1536",
        }
    }

    fn generate_fresh(&self, req: &ImageRequest, model: &str) -> Result<Vec<u8>> {
        let body = json!({
            "model": model,
            "prompt": req.prompt,
            "size": Self::size_for(req),
            "quality": DEFAULT_QUALITY,
            "output_format": "png",
            "n": 1,
        });

        let response = self
            .http
            .post(format!("{API_ROOT}/images/generations"))
            .header("Authorization", format!("Bearer {}", self.key))
            .json(&body)
            .send()
            .context("calling the OpenAI image API")?;

        self.decode(response, model)
    }

    /// Editing, with an optional mask.
    ///
    /// Multipart rather than JSON because the images travel as files. The mask
    /// is a PNG whose **transparent** pixels mark what to change — the inverse of
    /// what most people assume, and worth stating in the error rather than
    /// letting someone edit the wrong half of a picture.
    fn edit(&self, req: &ImageRequest, model: &str) -> Result<Vec<u8>> {
        let mut form = reqwest::blocking::multipart::Form::new()
            .text("model", model.to_string())
            .text("prompt", req.prompt.clone())
            .text("size", Self::size_for(req))
            .text("quality", DEFAULT_QUALITY);

        for path in &req.references {
            // `image[]` rather than `image`: gpt-image-1 accepts several, and the
            // singular form silently keeps only the last.
            form = form.part("image[]", file_part(path)?);
        }
        if let Some(mask) = &req.mask {
            form = form.part("mask", file_part(mask)?);
        }

        let response = self
            .http
            .post(format!("{API_ROOT}/images/edits"))
            .header("Authorization", format!("Bearer {}", self.key))
            .multipart(form)
            .send()
            .context("calling the OpenAI image edit API")?;

        self.decode(response, model)
    }

    /// Pulls the image out of a response, or explains why there is not one.
    fn decode(&self, response: reqwest::blocking::Response, model: &str) -> Result<Vec<u8>> {
        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text, model));
        }

        let payload: Value = response.json().context("parsing the API response")?;

        // gpt-image-1 always returns base64; the older models can return a URL
        // instead, so both are handled rather than assuming the modern shape.
        let first = &payload["data"][0];
        if let Some(encoded) = first["b64_json"].as_str() {
            return STANDARD.decode(encoded).context("decoding the image");
        }
        if let Some(url) = first["url"].as_str() {
            let bytes = self
                .http
                .get(url)
                .send()
                .context("downloading the generated image")?
                .bytes()
                .context("reading image bytes")?;
            return Ok(bytes.to_vec());
        }

        bail!("the response contained no image: {payload}")
    }
}

impl ImageProvider for Client {
    fn capabilities(&self) -> Capabilities {
        capabilities(DEFAULT_MODEL)
    }

    fn list_models(&self) -> Result<Vec<String>> {
        // `/v1/models` does not list image models for a project key even when
        // they work — measured, and confusing enough to be worth saying rather
        // than reporting an empty list.
        let response = self
            .http
            .get(format!("{API_ROOT}/models"))
            .header("Authorization", format!("Bearer {}", self.key))
            .send()
            .context("checking the OpenAI key")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text, "models"));
        }

        eprintln!(
            "Key is valid. Note /v1/models does not list image models even when \
             they are usable, so the list below is Lucida's own."
        );
        Ok(KNOWN_MODELS.iter().map(|m| (*m).to_string()).collect())
    }

    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage> {
        let model = resolve_model(&req.model);
        let size = Self::size_for(req);

        let bytes = if req.references.is_empty() {
            eprintln!("Rendering {size} with {model} (quality {DEFAULT_QUALITY})…");
            self.generate_fresh(req, &model)?
        } else {
            let scope = match &req.mask {
                Some(mask) => format!("masked by {mask}"),
                None => "whole image".to_string(),
            };
            eprintln!(
                "Editing {} reference(s), {scope}, {size} with {model}…",
                req.references.len()
            );
            self.edit(req, &model)?
        };

        Ok(GeneratedImage {
            bytes,
            mime_type: "image/png".to_string(),
            commentary: None,
            // No seed exists to report; the API rejects the parameter outright.
            seed: None,
        })
    }
}

fn file_part(path: &str) -> Result<reqwest::blocking::multipart::Part> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image.png")
        .to_string();
    let mime = if name.to_ascii_lowercase().ends_with(".webp") {
        "image/webp"
    } else if name.to_ascii_lowercase().ends_with(".jpg")
        || name.to_ascii_lowercase().ends_with(".jpeg")
    {
        "image/jpeg"
    } else {
        "image/png"
    };
    reqwest::blocking::multipart::Part::bytes(bytes)
        .file_name(name)
        .mime_str(mime)
        .context("attaching the image")
}

/// Turns an OpenAI error body into something worth reading.
///
/// Their shape is `{"error": {"message", "type", "param", "code"}}` — a fifth
/// distinct shape across five providers, which is itself worth noting: there is
/// no common error format to normalize toward.
pub fn explain_error(status: u16, body: &str, model: &str) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = &parsed["error"];
    let message = error["message"].as_str().unwrap_or(body.trim());
    let param = error["param"].as_str().unwrap_or_default();

    match status {
        401 => format!(
            "HTTP 401 — the OpenAI API key was rejected: {message}\n\n\
             Check OPENAI_API_KEY, or run `lucida config` to see what this process \
             can read."
        ),
        403 => format!(
            "HTTP 403 — this key may not use `{model}`: {message}\n\n\
             Image models often need the organisation to be verified, which is \
             separate from having billing enabled."
        ),
        429 => format!(
            "HTTP 429 — rate limited or out of quota: {message}\n\n\
             Check the billing dashboard; OpenAI reports both conditions here."
        ),
        400 if param == "mask" => format!(
            "HTTP 400 — the mask was rejected: {message}\n\n\
             A mask must be a PNG with an alpha channel, the same dimensions as \
             the image it applies to, and under 4 MB. Note the sense of it: the \
             **transparent** pixels are the part that gets changed."
        ),
        400 => format!("HTTP 400 — {message}"),
        _ => format!("HTTP {status} — {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Aspect;

    fn with_aspect(text: &str) -> ImageRequest {
        ImageRequest {
            aspect: Some(Aspect::parse(text).unwrap()),
            ..Default::default()
        }
    }

    /// Three fixed sizes, picked by shape. Not a rounding of a requested pixel
    /// count — there is no pixel count to request.
    #[test]
    fn ratios_map_onto_the_three_supported_sizes() {
        assert_eq!(Client::size_for(&with_aspect("1:1")), "1024x1024");
        assert_eq!(Client::size_for(&with_aspect("3:2")), "1536x1024");
        assert_eq!(Client::size_for(&with_aspect("16:9")), "1536x1024");
        assert_eq!(Client::size_for(&with_aspect("2:3")), "1024x1536");
        assert_eq!(Client::size_for(&with_aspect("9:16")), "1024x1536");
        // Nothing asked for lets the model choose, which no other provider offers.
        assert_eq!(Client::size_for(&ImageRequest::default()), "auto");
    }

    /// The capability that justifies this provider existing at all.
    #[test]
    fn it_is_the_only_provider_that_masks() {
        assert!(capabilities("gpt-image-1").mask);
        for backend in crate::provider::Backend::ALL {
            if *backend == crate::provider::Backend::OpenAi {
                continue;
            }
            assert!(
                !crate::provider::capabilities_for(*backend, backend.default_model()).mask,
                "{} should not claim masking",
                backend.name()
            );
        }
    }

    /// Both were measured as rejected rather than ignored, so declaring them
    /// false is a fact rather than a caution.
    #[test]
    fn neither_seed_nor_negative_prompt_exists() {
        let caps = capabilities("gpt-image-1");
        assert!(!caps.seed);
        assert!(!caps.negative_prompt);
        assert!(!caps.size);
    }

    #[test]
    fn a_rejected_mask_explains_which_pixels_change() {
        let body = r#"{"error":{"message":"bad mask","param":"mask","type":"x"}}"#;
        let message = explain_error(400, body, "gpt-image-1");
        assert!(message.contains("transparent"));
        assert!(message.contains("alpha"));
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(resolve_model("openai"), "gpt-image-1");
        assert_eq!(resolve_model("dalle"), "dall-e-3");
        assert_eq!(resolve_model("something-new"), "something-new");
    }
}
