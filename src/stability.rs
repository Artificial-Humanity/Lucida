//! Stability AI — the developer platform, not Brand Studio.
//!
//! The distinction matters commercially rather than technically: the developer
//! platform bills pay-as-you-go credits, while Brand Studio is a monthly
//! subscription. This targets the former, which is the reason Firefly and
//! Midjourney were ruled out and Stability was not.
//!
//! # A fourth call shape
//!
//! Every provider so far either returns JSON with the image inside it (Google)
//! or submits a job and polls for it (Veo, ComfyUI, BFL). Stability does
//! neither: it takes **`multipart/form-data`** and, with `Accept: image/*`,
//! returns the raw image bytes from that one request. No job id, no polling, no
//! base64. That is a genuinely different shape, and it holding without changing
//! the trait is the strongest evidence yet that the abstraction is real rather
//! than an accommodation of one pattern.
//!
//! # What was measured rather than read
//!
//! Their documentation site is a JavaScript application that cannot be fetched,
//! so everything here came from probing the live API. That turned out to be an
//! advantage: the endpoints validate before they authenticate and report their
//! own enums in the error text.
//!
//! - `aspect_ratio` is a **named enum of nine**, and there is no width or height
//!   anywhere. Stability is the only provider that will not let you choose the
//!   output size at all — which is why `Capabilities` grew a `size` field.
//! - **Unknown fields are silently ignored.** Sending `strength` to an endpoint
//!   without it draws no error at all. So support cannot be inferred from the
//!   absence of a complaint, and every capability below was tested by rendering.
//! - `negative_prompt` **is** honoured — proven by rendering the same seed with
//!   and without one and comparing pixels, not file bytes.
//! - The seed is deterministic in the pixels. Whole files differ every time
//!   because the embedded C2PA manifest carries a fresh instance id, which makes
//!   naive byte comparison say the opposite of the truth.

use crate::provider::{
    AspectSupport, Capabilities, GeneratedImage, ImageProvider, ImageRequest, Provenance,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::time::Duration;

const API_ROOT: &str = "https://api.stability.ai";

/// Cheapest of the three generate endpoints, and a reasonable default.
pub const DEFAULT_MODEL: &str = "core";

/// The nine ratios the API accepts, read out of its own validation error.
///
/// Note this is *not* Google's list: Stability has `9:21` and `21:9` but no
/// `3:4` or `4:3`. Two providers with named ratios, disagreeing about which —
/// which is exactly why the list is published per provider rather than shared.
pub const ASPECT_RATIOS: &[&str] = &[
    "21:9", "16:9", "3:2", "5:4", "1:1", "4:5", "2:3", "9:16", "9:21",
];

/// Endpoint names, which double as model ids here.
pub const MODEL_ALIASES: &[(&str, &str)] = &[
    ("stability", "core"),
    ("sai", "core"),
    ("stable-core", "core"),
    ("stable-ultra", "ultra"),
    ("ultra", "ultra"),
    ("sd3", "sd3"),
    ("sd3.5", "sd3"),
];

pub const KNOWN_MODELS: &[&str] = &["core", "ultra", "sd3"];

/// The `model` values `sd3` accepts, again read from its validation error.
pub const SD3_VARIANTS: &[&str] = &[
    "sd3.5-large",
    "sd3.5-large-turbo",
    "sd3.5-medium",
    "sd3.5-flash",
];

pub fn resolve_model(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    MODEL_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, id)| (*id).to_string())
        .unwrap_or(key)
}

pub fn capabilities(_model: &str) -> Capabilities {
    Capabilities {
        provider: "stability",
        tagline: "Stable Image. Paid, fast, has a negative prompt. Cannot be asked for an output size at all -- only a shape, from its own list of nine ratios.",
        aspect: AspectSupport::Named(ASPECT_RATIOS),
        // The one provider that does not let you choose. `aspect_ratio` is the
        // only geometry control; there is no width, height or resolution field
        // on any of these endpoints.
        size: false,
        seed: true,
        // Verified by rendering, not by the absence of a validation error —
        // unknown fields here are silently dropped, so absence proves nothing.
        negative_prompt: true,
        // Deliberately false for now. The generate endpoints take no reference
        // image; editing lives on separate endpoints (edit/inpaint, edit/erase,
        // search-and-replace) with their own required fields, and inpaint needs
        // a *mask* — which `ImageRequest` cannot express. Claiming support here
        // would silently turn an edit into a fresh generation.
        references: false,
        // Not exposed on core/ultra/sd3.
        steps: false,
        guidance: false,
        // Measured: a caBX chunk naming `Stability_ContentCredentials_Service`
        // and asserting trainedAlgorithmicMedia, with no SynthID. Same category
        // as BFL — marked, but only in metadata a re-encode discards.
        provenance: Provenance::C2paOnly,
    }
}

pub struct Client {
    key: String,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        let key = crate::config::var("STABILITY_API_KEY").ok_or_else(|| {
            let where_to_put_it = match crate::config::preferred_path() {
                Some(path) => format!(
                    "Set STABILITY_API_KEY, or add it to {} — \
                     `lucida config --set STABILITY_API_KEY` prompts for it without \
                     echoing or storing it in your shell history.",
                    path.display()
                ),
                None => "Set STABILITY_API_KEY.".to_string(),
            };
            anyhow!(
                "no Stability AI API key found.\n\n{where_to_put_it}\n\n\
                 Keys come from https://platform.stability.ai — the developer \
                 platform bills per image from a credit balance."
            )
        })?;

        let http = reqwest::blocking::Client::builder()
            // Synchronous: this one request covers the whole render, so the
            // timeout has to cover it too rather than just a handshake.
            .timeout(Duration::from_secs(300))
            .build()
            .context("building HTTP client")?;

        Ok(Self { key, http })
    }

    /// The account's credit balance. Free, and the only way to check a key
    /// without spending anything.
    pub fn credits(&self) -> Result<f64> {
        let response = self
            .http
            .get(format!("{API_ROOT}/v1/user/balance"))
            .header("Authorization", format!("Bearer {}", self.key))
            .send()
            .context("checking the Stability credit balance")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text, "balance"));
        }

        let payload: Value = response.json().context("parsing the balance response")?;
        payload["credits"]
            .as_f64()
            .ok_or_else(|| anyhow!("no credit balance in the response: {payload}"))
    }
}

impl ImageProvider for Client {
    fn capabilities(&self) -> Capabilities {
        capabilities(DEFAULT_MODEL)
    }

    fn list_models(&self) -> Result<Vec<String>> {
        // No discovery endpoint — the models are paths. Confirm the key works
        // instead, which is what someone running this actually wants to know.
        let credits = self.credits()?;
        eprintln!("Key is valid. Remaining credits: {credits}");
        Ok(KNOWN_MODELS.iter().map(|m| (*m).to_string()).collect())
    }

    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage> {
        let model = resolve_model(&req.model);

        let mut form = reqwest::blocking::multipart::Form::new()
            .text("prompt", req.prompt.clone())
            // PNG rather than the jpeg default, so an image that goes on to be
            // edited is not re-compressed on the way.
            .text("output_format", "png");

        if let Some(aspect) = req.aspect {
            form = form.text("aspect_ratio", aspect.to_string());
        }
        if let Some(seed) = req.seed {
            form = form.text("seed", seed.to_string());
        }
        if let Some(negative) = &req.negative_prompt {
            form = form.text("negative_prompt", negative.clone());
        }
        // `sd3` is an endpoint, not a model: it needs a variant naming which
        // weights actually run, and rejects the request without one.
        if model == "sd3" {
            form = form.text("model", SD3_VARIANTS[0]);
        }

        eprintln!("Rendering with stability {model}…");

        let response = self
            .http
            .post(format!("{API_ROOT}/v2beta/stable-image/generate/{model}"))
            // `image/*` returns the bytes directly. The alternative,
            // `application/json`, wraps them in base64 for no benefit here.
            .header("Accept", "image/*")
            .header("Authorization", format!("Bearer {}", self.key))
            .multipart(form)
            .send()
            .context("calling the Stability AI API")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text, &model));
        }

        let bytes = response.bytes().context("reading image bytes")?.to_vec();

        Ok(GeneratedImage {
            bytes,
            mime_type: "image/png".to_string(),
            commentary: None,
            // Echoed back only when it was asked for: the API does not report the
            // seed it chose, so an unpinned render cannot be reproduced.
            seed: req.seed,
        })
    }
}

/// Turns a Stability error body into something worth reading.
///
/// Their shape is `{"errors": [...], "name": "..."}` — a list rather than the
/// single `detail` or `message` every other provider uses, and the list is where
/// the useful text lives.
pub fn explain_error(status: u16, body: &str, model: &str) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let detail = parsed["errors"]
        .as_array()
        .map(|errors| {
            errors
                .iter()
                .filter_map(|e| e.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|joined| !joined.is_empty())
        .unwrap_or_else(|| body.trim().to_string());

    match status {
        401 | 403 => format!(
            "HTTP {status} — the Stability API key was rejected: {detail}\n\n\
             Check STABILITY_API_KEY, or run `lucida config` to see which value \
             this process can actually read. Keys come from \
             https://platform.stability.ai."
        ),
        402 => format!(
            "HTTP 402 — out of credits. Top up at https://platform.stability.ai.\n\n\
             {detail}"
        ),
        404 => format!(
            "HTTP 404 — no such endpoint as `{model}`.\n\n\
             Stability's model ids are URL paths: {}. Run \
             `lucida models --provider stability`.",
            KNOWN_MODELS.join(", ")
        ),
        413 => format!("HTTP 413 — the request was too large. {detail}"),
        429 => format!(
            "HTTP 429 — rate limited. Stability caps concurrent requests; wait for \
             one to finish.\n\n{detail}"
        ),
        // Their validation messages name the accepted values, so they are worth
        // surfacing verbatim rather than paraphrasing.
        400 | 422 => format!("HTTP {status} — the API rejected the request: {detail}"),
        _ => format!("HTTP {status} — {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two providers with named ratios that disagree about which. Asserted
    /// because the tempting simplification — "named ratios are named ratios" —
    /// would silently send `4:3` to a provider that rejects it.
    #[test]
    fn the_ratio_list_is_not_googles() {
        assert!(ASPECT_RATIOS.contains(&"9:21"));
        assert!(!crate::genai::ASPECT_RATIOS.contains(&"9:21"));
        assert!(crate::genai::ASPECT_RATIOS.contains(&"4:3"));
        assert!(!ASPECT_RATIOS.contains(&"4:3"));
    }

    /// The only provider with no size control at all.
    #[test]
    fn size_is_not_offered() {
        assert!(!capabilities("core").size);
        assert!(capabilities("core").seed);
        assert!(capabilities("core").negative_prompt);
    }

    /// Editing lives on separate endpoints and inpaint needs a mask, which
    /// `ImageRequest` cannot express — so this must not claim support.
    #[test]
    fn editing_is_not_claimed_on_the_generate_endpoints() {
        assert!(!capabilities("core").references);
        assert!(!capabilities("ultra").references);
    }

    #[test]
    fn errors_come_out_of_the_errors_array() {
        let body = r#"{"errors":["aspect_ratio: invalid enum value. Expected '21:9' | '16:9'"],"name":"bad_request"}"#;
        let message = explain_error(400, body, "core");
        assert!(message.contains("invalid enum value"));
        assert!(message.contains("21:9"));
    }

    #[test]
    fn a_rejected_key_names_the_config_command() {
        let message = explain_error(401, r#"{"errors":["bad key"],"name":"unauthorized"}"#, "core");
        assert!(message.contains("lucida config"));
    }

    /// The sd3 endpoint refuses a request that does not name a variant, so the
    /// list must be non-empty and its first entry must be a real one.
    #[test]
    fn sd3_has_a_default_variant() {
        assert_eq!(SD3_VARIANTS[0], "sd3.5-large");
        assert!(SD3_VARIANTS.iter().all(|v| v.starts_with("sd3.5-")));
    }

    #[test]
    fn aliases_resolve_to_endpoint_names() {
        assert_eq!(resolve_model("stability"), "core");
        assert_eq!(resolve_model("ultra"), "ultra");
        assert_eq!(resolve_model("SD3.5"), "sd3");
        // Anything unknown passes through, so a new endpoint works immediately.
        assert_eq!(resolve_model("something-new"), "something-new");
    }
}
