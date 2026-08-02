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

/// The newest of the family, verified working, and the best at masking.
///
/// Measured against `gpt-image-1.5` on the same source and mask:
/// `gpt-image-2` concentrated 4.5x more change inside the mask than outside,
/// against 2.0x — and changed only 10.9/255 outside it, against 29.1. Neither
/// binds the mask, but the difference is large enough to decide the default.
///
/// **A 400 does not prove access.** Validation runs before the entitlement
/// check, so invalid-parameter errors come back for models a project cannot use.
/// Only a successful call, or a 403, settles it — worth remembering, because a
/// validation error reads exactly like proof that the model is reachable.
pub const DEFAULT_MODEL: &str = "gpt-image-2";

/// Latent grid for `gpt-image-2`, from its own validation error: "width and
/// height must both be divisible by 16".
const PIXEL_GRID: u32 = 16;

/// Target area for `gpt-image-2`, in pixels.
///
/// Sized by AREA rather than by long edge, which is a sixth geometry model
/// across five providers. The reason is a constraint that appears in no
/// documentation: a request below "the current minimum pixel budget" is
/// rejected, and long-edge sizing quietly falls under it as the ratio widens —
/// 16:9 at a 1024 long edge is 0.59 MP and refused, while the same long edge at
/// 1:1 is 1.05 MP and fine. Holding the area constant makes every ratio behave
/// the same way, which is what someone changing `--aspect` expects anyway.
const TARGET_AREA: u32 = 1024 * 1024;

/// The ratios the three supported pixel sizes correspond to.
pub const ASPECT_RATIOS: &[&str] = &["1:1", "2:3", "3:2"];

/// Quality tier when none is asked for.
///
/// Deliberately not the API's own default of `auto`, which resolves upward and
/// bills accordingly. A stated middling default is friendlier than an unstated
/// expensive one, and this is a provider where the difference is real money.
const DEFAULT_QUALITY: &str = "medium";

pub const MODEL_ALIASES: &[(&str, &str)] = &[
    ("openai", "gpt-image-2"),
    ("gpt-image", "gpt-image-2"),
    ("oai", "gpt-image-2"),
];

/// Measured against the live API, which distinguishes "no access" (403) from
/// "does not exist" (400) — a distinction worth exploiting, because it maps the
/// catalogue without an entitlement.
///
/// **DALL·E is gone.** `dall-e-3` and `dall-e-2` both report *does not exist*,
/// not merely no access, so the aliases that pointed at them were shipping a
/// dead end. Removed rather than kept as a courtesy.
pub const KNOWN_MODELS: &[&str] = &[
    "gpt-image-2",
    "chatgpt-image-latest",
    "gpt-image-1.5",
    "gpt-image-1-mini",
    "gpt-image-1",
];

/// Whether a model takes free dimensions rather than the three fixed sizes.
///
/// `gpt-image-2` alone: "width and height must both be divisible by 16", where
/// its siblings accept only 1024x1024, 1024x1536, 1536x1024 and auto. The same
/// per-model divergence BFL forced, arriving again in a different provider —
/// which is now less a surprise than a pattern.
fn free_dimensions(model: &str) -> bool {
    model == "gpt-image-2"
}

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
    let free = free_dimensions(&id);

    Capabilities {
        provider: "openai",
        tagline: "gpt-image. Paid, seconds, and the ONLY provider that can mask an \
                  edit to part of an image. No seed and no negative prompt at all; \
                  gpt-image-2 takes free dimensions while the rest take three \
                  fixed sizes.",
        aspect: if free {
            AspectSupport::Free {
                multiple_of: PIXEL_GRID,
            }
        } else {
            AspectSupport::Named(ASPECT_RATIOS)
        },
        // Only gpt-image-2 lets the pixel count be chosen; its siblings offer
        // three fixed sizes, so asking for one there is an error not a rounding.
        size: free,
        // Rejected outright by the API: "Unknown parameter: 'seed'".
        seed: false,
        // Likewise "Unknown parameter: 'negative_prompt'".
        negative_prompt: false,
        references: true,
        mask: true,
        steps: false,
        guidance: false,
        // Measured on a real gpt-image-1.5 render: a caBX chunk carrying a C2PA
        // manifest asserting trainedAlgorithmicMedia, and no SynthID. Third
        // provider in this category — marked, but only in metadata.
        provenance: Provenance::C2paOnly,
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
    /// The aspect an edit should keep when none was asked for.
    ///
    /// `auto` lets the model choose a shape from the prompt, which is a good
    /// default for a fresh image and a bad one for an edit: gpt-image-1-mini
    /// turned a 1024x1024 source into 1024x1536, reshaping a picture nobody
    /// asked to reshape. So an edit with no stated geometry follows its source,
    /// which is what the local lane and BFL already do.
    fn implied_aspect(req: &ImageRequest) -> Option<crate::provider::Aspect> {
        if req.aspect.is_some() || req.references.is_empty() {
            return req.aspect;
        }
        let first = req.references.first()?;
        let bytes = std::fs::read(first).ok()?;
        let mime = if first.to_ascii_lowercase().ends_with(".png") {
            "image/png"
        } else {
            "image/jpeg"
        };
        let (w, h) = crate::image_dimensions(&bytes, mime)?;
        Some(crate::provider::Aspect { w, h })
    }

    fn size_for(req: &ImageRequest, model: &str) -> String {
        let asked = Self::implied_aspect(req);

        if free_dimensions(model) {
            // Nothing asked for still means `auto`, which lets the model choose a
            // shape to suit the prompt — worth keeping rather than imposing a
            // square by default.
            if asked.is_none() && req.size.is_none() {
                return "auto".to_string();
            }
            let scoped = ImageRequest {
                aspect: asked,
                ..req.clone()
            };
            let (w, h) = area_dimensions(&scoped, TARGET_AREA);
            return format!("{w}x{h}");
        }

        match asked.map(|a| (a.w, a.h)) {
            None => "auto".to_string(),
            Some((w, h)) if w == h => "1024x1024".to_string(),
            Some((w, h)) if w > h => "1536x1024".to_string(),
            Some(_) => "1024x1536".to_string(),
        }
    }

    fn generate_fresh(&self, req: &ImageRequest, model: &str) -> Result<Vec<u8>> {
        let body = json!({
            "model": model,
            "prompt": req.prompt,
            "size": Self::size_for(req, model),
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
    ///
    /// **The mask is advisory, and how advisory depends on the model.** Measured
    /// on one source with one mask:
    ///
    /// | model | inside | outside | concentration |
    /// |---|---|---|---|
    /// | `gpt-image-1.5` | 58.0/255 | 29.1/255 | 2.0x |
    /// | `gpt-image-2` | 49.2/255 | 10.9/255 | 4.5x |
    ///
    /// Neither confines the edit — `gpt-image-1.5` lost an object nowhere near
    /// the mask — but `gpt-image-2` leaves the rest of the picture far closer to
    /// untouched, which is why it is the default.
    fn edit(&self, req: &ImageRequest, model: &str) -> Result<Vec<u8>> {
        let mut form = reqwest::blocking::multipart::Form::new()
            .text("model", model.to_string())
            .text("prompt", req.prompt.clone())
            .text("size", Self::size_for(req, model))
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
        let size = Self::size_for(req, &model);

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

/// Dimensions holding `target` pixels at the requested ratio, on the 16-grid.
///
/// `--size` scales the budget rather than setting an edge: asking for 2K on a
/// provider measured in area means "about four times the pixels", which is the
/// only reading that keeps a ratio change from also changing the resolution.
fn area_dimensions(req: &ImageRequest, target: u32) -> (u32, u32) {
    let ratio = req.aspect.map_or(1.0, |a| f64::from(a.w) / f64::from(a.h));

    // A requested long edge is honoured as a scale on the budget, since there is
    // no edge to set directly.
    let target = match req.size {
        Some(size) => {
            let scale = f64::from(size.0) / 1024.0;
            (f64::from(target) * scale * scale) as u32
        }
        None => target,
    };

    let height = (f64::from(target) / ratio).sqrt();
    let width = height * ratio;

    let round = |v: f64| {
        let n = ((v / f64::from(PIXEL_GRID)).round() as u32).max(1);
        n * PIXEL_GRID
    };
    (round(width), round(height))
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
        // Measured: this arrives naming the *project*, not the organisation.
        // OpenAI gates models per project, so enabling one org-wide does not
        // reach a project created before or outside that change — and the first
        // message here blamed organisation verification, which sent the reader
        // to the wrong settings page entirely.
        403 => format!(
            "HTTP 403 — this project may not use `{model}`.\n\n{message}\n\n\
             Model access on OpenAI is granted PER PROJECT, not per organisation \
             or per key. In the console open the project named above, then \
             Limits (or Model permissions) and enable `{model}` for that project \
             specifically. Enabling it org-wide, or having billing active, does \
             not do this on its own.\n\n\
             If the project name is not one you recognise, the key belongs to a \
             different project than you edited — check which key `lucida config` \
             is reading."
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
        let m = "gpt-image-1.5";
        assert_eq!(Client::size_for(&with_aspect("1:1"), m), "1024x1024");
        assert_eq!(Client::size_for(&with_aspect("3:2"), m), "1536x1024");
        assert_eq!(Client::size_for(&with_aspect("16:9"), m), "1536x1024");
        assert_eq!(Client::size_for(&with_aspect("2:3"), m), "1024x1536");
        assert_eq!(Client::size_for(&with_aspect("9:16"), m), "1024x1536");
        // Nothing asked for lets the model choose, which no other provider offers.
        assert_eq!(Client::size_for(&ImageRequest::default(), m), "auto");
    }

    /// gpt-image-2 diverges from its own siblings, which is the BFL lesson
    /// arriving in a second provider: capabilities are a property of the model.
    #[test]
    fn gpt_image_2_takes_free_dimensions() {
        assert_eq!(Client::size_for(&ImageRequest::default(), "gpt-image-2"), "auto");
        assert!(capabilities("gpt-image-2").size);
        assert!(!capabilities("gpt-image-1.5").size);
        assert!(!capabilities("gpt-image-1").size);
    }

    /// Every ratio must clear the undocumented pixel-budget floor. Long-edge
    /// sizing did not: 16:9 came out 1024x576, which the API refuses while
    /// accepting 1024x1024 at the same long edge.
    #[test]
    fn every_ratio_holds_roughly_the_same_area() {
        for ratio in ["1:1", "16:9", "9:16", "21:9", "3:2", "2:3"] {
            let (w, h) = area_dimensions(&with_aspect(ratio), TARGET_AREA);
            let area = w * h;
            assert_eq!(w % 16, 0, "{ratio} width off-grid");
            assert_eq!(h % 16, 0, "{ratio} height off-grid");
            assert!(
                area > 900_000,
                "{ratio} came out {w}x{h} = {area}px, under the budget that 16:9 \
                 originally tripped"
            );
            // …and not wildly over, or a wide ratio silently costs more.
            assert!(area < 1_250_000, "{ratio} came out {w}x{h} = {area}px");
        }
    }

    /// An edit with no stated geometry follows its source rather than letting
    /// the model choose: gpt-image-1-mini turned a square source into a portrait,
    /// reshaping a picture nobody asked to reshape.
    #[test]
    fn an_edit_keeps_the_sources_shape() {
        let dir = std::env::temp_dir().join("lucida-openai-shape-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wide.png");
        // A minimal PNG header is enough: only IHDR is read.
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1536u32.to_be_bytes());
        png.extend_from_slice(&1024u32.to_be_bytes());
        std::fs::write(&path, &png).unwrap();

        let edit = ImageRequest {
            references: vec![path.to_string_lossy().into_owned()],
            ..Default::default()
        };
        // 3:2 source, so the landscape size rather than `auto`.
        assert_eq!(Client::size_for(&edit, "gpt-image-1.5"), "1536x1024");

        // A fresh image still lets the model choose.
        assert_eq!(
            Client::size_for(&ImageRequest::default(), "gpt-image-1.5"),
            "auto"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--size` scales the budget, since there is no edge to set.
    #[test]
    fn size_scales_the_area_budget() {
        let big = ImageRequest {
            size: Some(crate::provider::Size::TWO_K),
            ..with_aspect("1:1")
        };
        let (w, h) = area_dimensions(&big, TARGET_AREA);
        // 2K means twice the edge, so about four times the pixels.
        assert!((w as f64 - 2048.0).abs() < 32.0, "got {w}x{h}");
    }

    /// DALL·E reports "does not exist", not "no access" — it is retired, and
    /// pointing an alias at it would ship a dead end.
    #[test]
    fn dall_e_is_not_offered() {
        assert!(!KNOWN_MODELS.iter().any(|m| m.contains("dall")));
        assert!(!MODEL_ALIASES.iter().any(|(_, t)| t.contains("dall")));
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

    /// Measured against the live API: the 403 names the project, and the fix is
    /// a per-project setting. The first version of this message blamed
    /// organisation verification and would have sent the reader to the wrong
    /// page — plausible, and wrong.
    #[test]
    fn a_403_points_at_per_project_model_access() {
        let body = r#"{"error":{"message":"Project `proj_x` does not have access to model `gpt-image-1`","type":"x"}}"#;
        let message = explain_error(403, body, "gpt-image-1");
        assert!(message.contains("PER PROJECT"));
        assert!(message.contains("proj_x"), "must echo the project it named");
        assert!(!message.contains("organisation to be verified"));
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
        assert_eq!(resolve_model("openai"), "gpt-image-2");
        assert_eq!(resolve_model("something-new"), "something-new");
    }
}
