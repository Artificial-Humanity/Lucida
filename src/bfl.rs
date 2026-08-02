//! Black Forest Labs — hosted FLUX.
//!
//! The provider the abstraction was actually built for. ComfyUI proved the trait
//! could hold two dissimilar *shapes*; this proves it can hold a real
//! **substitution** — the same model family, reached a different way, which is
//! the thing anyone switching providers actually wants.
//!
//! The call pattern is the familiar one: submit, poll, download. That is now
//! three providers running the same shape (Veo, ComfyUI, BFL), which is a fair
//! sign it is the right one.
//!
//! # Two things the roadmap got wrong about Flux
//!
//! It predicted Flux would bring "seed, steps, guidance, negative prompt". Read
//! off the live OpenAPI spec:
//!
//! - **There is no negative prompt.** Not on any FLUX.2 endpoint, not on
//!   `flux-dev`, not on `flux-pro-1.1`. The local lane has one because ComfyUI
//!   builds the graph and can wire negative conditioning itself; the hosted API
//!   simply does not expose it. So hosted Flux is *less* capable than local Flux
//!   in the one respect the roadmap was most confident about.
//! - **Capabilities vary per model, not per provider.** `steps` and `guidance`
//!   exist on `flux-2-flex` and `flux-dev` and nowhere else in the family. That
//!   is a new axis: until now a provider had one answer for everyone.

use crate::provider::{
    AspectSupport, Capabilities, GeneratedImage, ImageProvider, ImageRequest, Provenance,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const API_ROOT: &str = "https://api.bfl.ai/v1";

/// The recommended default in BFL's own documentation.
pub const DEFAULT_MODEL: &str = "flux-2-pro";

/// FLUX renders in units of 32 pixels.
const PIXEL_GRID: u32 = 32;
const DEFAULT_DIMENSIONS: (u32, u32) = (1024, 1024);

/// The most reference images any FLUX.2 endpoint takes. The klein variants stop
/// at four, so this is a ceiling rather than a promise.
const MAX_REFERENCES: usize = 8;

/// Friendly names for the endpoints, which are the model ids here.
pub const MODEL_ALIASES: &[(&str, &str)] = &[
    ("bfl", "flux-2-pro"),
    ("flux", "flux-2-pro"),
    ("flux-pro", "flux-2-pro"),
    ("flux-max", "flux-2-max"),
    ("flux-flex", "flux-2-flex"),
    ("flux-klein", "flux-2-klein-9b"),
    ("flux-1.1", "flux-pro-1.1"),
];

pub fn resolve_model(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    MODEL_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, id)| (*id).to_string())
        .unwrap_or_else(|| input.trim().to_string())
}

/// What a given endpoint accepts.
///
/// Keyed on the model because BFL's endpoints genuinely disagree, and the
/// alternative — publishing the union — would advertise `--steps` on models that
/// silently ignore it. That is the exact failure this whole design exists to
/// prevent, so it is worth a table.
pub fn capabilities(model: &str) -> Capabilities {
    let id = resolve_model(model);

    // Only `flex` and `dev` expose the sampler. Everything else in the family
    // decides for itself.
    let tunable = matches!(id.as_str(), "flux-2-flex" | "flux-dev");

    // `flux-pro-1.1` and `flux-dev` take `image_prompt`, which conditions style
    // rather than editing a picture, so they are declared as not supporting
    // reference images at all. Claiming otherwise would mean an edit that
    // quietly became a loosely-inspired generation.
    let edits = id.starts_with("flux-2") || id.starts_with("flux-kontext");

    Capabilities {
        provider: "bfl",
        tagline: "Hosted FLUX. Paid, fast, edits well. The only provider whose capabilities differ per MODEL: steps and guidance exist on flux-2-flex and flux-dev alone.",
        aspect: AspectSupport::Free {
            multiple_of: PIXEL_GRID,
        },
        size: true,
        seed: true,
        // Measured, not assumed: no FLUX endpoint takes one.
        negative_prompt: false,
        references: edits,
        mask: false,
        workflow: false,
        steps: tunable,
        guidance: tunable,
        // Verified in a real render rather than assumed: a signed C2PA manifest
        // in a caBX chunk, and no SynthID at all. That is a third state — Google
        // marks the pixels too, ComfyUI marks nothing — and the difference
        // matters, because a re-encode strips C2PA and cannot strip SynthID.
        provenance: Provenance::C2paOnly,
    }
}

pub struct Client {
    key: String,
    http: reqwest::blocking::Client,
    /// `API_ROOT` in production; a recorded-response server in tests.
    base: String,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        let key = crate::config::var("BFL_API_KEY").ok_or_else(|| {
            let where_to_put_it = match crate::config::preferred_path() {
                Some(path) => format!(
                    "Set BFL_API_KEY, or add it to {} — `lucida config --set BFL_API_KEY` \
                     reads it from stdin so it stays out of your shell history.",
                    path.display()
                ),
                None => "Set BFL_API_KEY.".to_string(),
            };
            anyhow!(
                "no Black Forest Labs API key found.\n\n{where_to_put_it}\n\n\
                 Keys come from https://dashboard.bfl.ai — this is a paid API and \
                 every render costs credits."
            )
        })?;

        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .context("building HTTP client")?;

        Ok(Self {
            key,
            http,
            base: API_ROOT.to_string(),
        })
    }

    fn body(&self, req: &ImageRequest, model: &str) -> Result<Value> {
        let mut body = serde_json::Map::new();
        body.insert("prompt".into(), json!(req.prompt));

        // Dimensions are sent only when they were actually asked for.
        //
        // An edit with no stated geometry should come back shaped like its
        // source — which is what the local lane does, and what anyone editing a
        // 16:9 still expects. Sending the 1024x1024 default instead silently
        // reframed the picture to square: the edit itself was right and the
        // composition was destroyed. Omitting the fields lets the API derive
        // them from the input image, which is what its `default: 0` means.
        let asked_for_dimensions = req.aspect.is_some() || req.size.is_some();
        if asked_for_dimensions || req.references.is_empty() {
            let (width, height) = req.pixels(DEFAULT_DIMENSIONS, PIXEL_GRID);
            body.insert("width".into(), json!(width));
            body.insert("height".into(), json!(height));
        }
        // PNG rather than the jpeg default: this is the only output format
        // choice on offer, and a lossless one keeps editing chains honest.
        body.insert("output_format".into(), json!("png"));

        if let Some(seed) = req.seed {
            body.insert("seed".into(), json!(seed));
        }
        if let Some(steps) = req.steps {
            body.insert("steps".into(), json!(steps));
        }
        if let Some(guidance) = req.guidance {
            body.insert("guidance".into(), json!(guidance));
        }

        // Checked before the loop, not after: base64-encoding nine images and
        // then rejecting the request would read every file for nothing.
        if req.references.len() > MAX_REFERENCES {
            bail!(
                "`{model}` accepts at most {MAX_REFERENCES} reference images; {} were \
                 given.\n\n\
                 Note the FLUX.2 klein endpoints accept only 4, so a request the \
                 limit here allows may still be rejected by those.",
                req.references.len()
            );
        }

        // Reference images are numbered fields rather than an array:
        // input_image, input_image_2, … input_image_8.
        for (index, reference) in req.references.iter().enumerate() {
            let field = match index {
                0 => "input_image".to_string(),
                n => format!("input_image_{}", n + 1),
            };
            body.insert(field, json!(encode_reference(reference)?));
        }

        Ok(Value::Object(body))
    }

    fn submit(&self, req: &ImageRequest, model: &str) -> Result<(String, Option<f64>)> {
        let response = self
            .http
            .post(format!("{}/{model}", self.base))
            .header("x-key", &self.key)
            .json(&self.body(req, model)?)
            .send()
            .context("calling the Black Forest Labs API")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text, model));
        }

        let payload: Value = response.json().context("parsing the submit response")?;

        // The docs are explicit that the returned polling_url must be used rather
        // than one built by hand, because the global endpoint hands work to a
        // regional one and only it knows where the result will appear.
        let polling_url = payload["polling_url"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                payload["id"]
                    .as_str()
                    .map(|id| format!("{}/get_result?id={id}", self.base))
            })
            .ok_or_else(|| anyhow!("the API accepted the job but returned no id: {payload}"))?;

        Ok((polling_url, payload["cost"].as_f64()))
    }

    /// Polls until the job resolves, then downloads the result.
    fn await_image(&self, polling_url: &str) -> Result<Vec<u8>> {
        let started = Instant::now();
        let deadline = Duration::from_secs(600);
        let mut interval = Duration::from_millis(1000);
        let mut announced = String::new();

        loop {
            if started.elapsed() > deadline {
                bail!(
                    "gave up after {} minutes. The job may still complete; its \
                     polling URL was {polling_url}",
                    deadline.as_secs() / 60
                );
            }

            std::thread::sleep(interval);
            // Backs off to keep a slow job from becoming a tight loop, but stays
            // brisk early because most renders finish in seconds.
            interval = (interval * 2).min(Duration::from_secs(3));

            let response = self
                .http
                .get(polling_url)
                .header("x-key", &self.key)
                .send()
                .context("polling the render")?;

            let status = response.status();
            if !status.is_success() {
                let text = response.text().unwrap_or_default();
                bail!("{}", explain_error(status.as_u16(), &text, "get_result"));
            }

            let payload: Value = response.json().context("parsing the poll response")?;
            let state = payload["status"].as_str().unwrap_or_default();

            match state {
                "Ready" => {
                    eprintln!("Render finished in {}s.", started.elapsed().as_secs());
                    return self.download(&payload);
                }
                // Both moderation outcomes are terminal, and the distinction is
                // worth keeping: one rejected what was asked for, the other
                // rejected what came back.
                "Request Moderated" => bail!(
                    "the prompt was rejected by content moderation before rendering.\n\n\
                     Rephrase it, or raise `safety_tolerance` if the subject is \
                     legitimate. Nothing was charged for a moderated request."
                ),
                "Content Moderated" => bail!(
                    "the image was rendered but rejected by output moderation, so it \
                     cannot be retrieved. Rephrasing usually clears it."
                ),
                "Error" => bail!(
                    "the render failed: {}",
                    payload["details"]
                        .as_str()
                        .or_else(|| payload["details"]["error"].as_str())
                        .unwrap_or(&payload["details"].to_string())
                ),
                "Task not found" => bail!(
                    "the API no longer knows about this job. Results expire, so a \
                     long-delayed poll can see this."
                ),
                // Pending / Reasoning / Generating, plus anything new they add.
                other => {
                    // The state names carry real information — "Reasoning" means
                    // the prompt is being expanded, not that rendering has begun —
                    // so report transitions rather than a uniform tick.
                    if other != announced {
                        let progress = payload["progress"]
                            .as_f64()
                            .map(|p| format!(" ({:.0}%)", p * 100.0))
                            .unwrap_or_default();
                        eprintln!("  {other}{progress}…");
                        announced = other.to_string();
                    }
                }
            }
        }
    }

    fn download(&self, payload: &Value) -> Result<Vec<u8>> {
        let url = payload["result"]["sample"]
            .as_str()
            .ok_or_else(|| anyhow!("the render is ready but carries no image: {payload}"))?;

        // No API key on this request: it is a signed URL pointing at object
        // storage, and sending a credential to a third-party host that does not
        // need it is how credentials end up somewhere unexpected.
        let response = self
            .http
            .get(url)
            .send()
            .context("downloading the finished image")?;

        if !response.status().is_success() {
            bail!(
                "the image URL returned HTTP {}. These URLs are signed and expire \
                 after about 10 minutes.",
                response.status().as_u16()
            );
        }

        Ok(response.bytes().context("reading image bytes")?.to_vec())
    }
}

impl ImageProvider for Client {
    fn capabilities(&self) -> Capabilities {
        // The model-specific answer is resolved by `capabilities_for`; this is
        // the conservative floor for callers holding only a provider.
        capabilities(DEFAULT_MODEL)
    }

    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage> {
        let model = resolve_model(&req.model);

        let stated = req.aspect.is_some() || req.size.is_some();
        let shape = if stated || req.references.is_empty() {
            let (width, height) = req.pixels(DEFAULT_DIMENSIONS, PIXEL_GRID);
            format!("{width}x{height}")
        } else {
            // Nothing honest to print: the API picks it from the source.
            "at the source's shape".to_string()
        };
        let verb = if req.references.is_empty() {
            "Rendering"
        } else {
            "Editing"
        };
        eprintln!("{verb} {shape} with {model}…");

        let (polling_url, cost) = self.submit(req, &model)?;
        if let Some(cost) = cost {
            // Stated up front, because this is the one provider where a typo in
            // a loop costs real money.
            eprintln!("  cost: {cost} credits");
        }

        let bytes = self.await_image(&polling_url)?;

        Ok(GeneratedImage {
            bytes,
            mime_type: "image/png".to_string(),
            commentary: None,
            // BFL echoes no seed, so an unpinned render cannot be reproduced —
            // report only what was actually asked for rather than inventing one.
            seed: req.seed,
        })
    }

    fn list_models(&self) -> Result<Vec<String>> {
        // There is no endpoint that lists models — they are paths, fixed at
        // release. Confirm the key works instead, since that is what a user
        // running `lucida models` is really asking.
        let response = self
            .http
            .get(format!("{}/credits", self.base))
            .header("x-key", &self.key)
            .send()
            .context("checking the API key against /v1/credits")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text, "credits"));
        }

        if let Ok(payload) = response.json::<Value>()
            && let Some(credits) = payload["credits"].as_f64()
        {
            eprintln!("Key is valid. Remaining credits: {credits}");
        }

        Ok(KNOWN_MODELS.iter().map(|m| (*m).to_string()).collect())
    }
}

/// The endpoints Lucida knows about, read off the live OpenAPI spec.
///
/// A fixed list because BFL publishes no discovery endpoint. An unrecognised id
/// is still passed through as a path, so a model released tomorrow works today.
pub const KNOWN_MODELS: &[&str] = &[
    "flux-2-pro",
    "flux-2-max",
    "flux-2-flex",
    "flux-2-klein-9b",
    "flux-2-klein-4b",
    "flux-pro-1.1",
    "flux-pro-1.1-ultra",
    "flux-dev",
    "flux-kontext-pro",
    "flux-kontext-max",
];

/// Reference images: a URL passes straight through, a local file is base64'd.
///
/// BFL's documented examples use URLs and its schema says only "Path to the
/// input image", so both are accepted — a local path is far more useful, and a
/// URL costs nothing to support since it needs no transformation.
fn encode_reference(reference: &str) -> Result<String> {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return Ok(reference.to_string());
    }

    let bytes = std::fs::read(reference)
        .with_context(|| format!("reading the reference image {reference}"))?;
    Ok(STANDARD.encode(&bytes))
}

/// Turns an HTTP failure into something worth reading.
pub fn explain_error(status: u16, body: &str, model: &str) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let detail = parsed["detail"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            if parsed["detail"].is_null() {
                body.trim().to_string()
            } else {
                parsed["detail"].to_string()
            }
        });

    // Checked before the status code, because BFL reports a malformed key as a
    // 422 validation failure rather than a 401 — measured, not assumed. Reading
    // that as "a parameter was wrong" sends people to inspect their prompt.
    if detail.to_ascii_lowercase().contains("api key") {
        return format!(
            "HTTP {status} — the Black Forest Labs API key was not accepted: {detail}\n\n\
             Note this arrives as a {status} rather than a 401. Check BFL_API_KEY, or \
             `lucida config` to see which one this process can actually read. Keys \
             come from https://dashboard.bfl.ai."
        );
    }

    match status {
        401 | 403 => format!(
            "HTTP {status} — the Black Forest Labs API key was rejected.\n\n\
             Check BFL_API_KEY (or `lucida config`). Keys come from \
             https://dashboard.bfl.ai.\n\nOriginal message: {detail}"
        ),
        402 => format!(
            "HTTP 402 — out of credits. Top up at https://dashboard.bfl.ai.\n\n\
             Original message: {detail}"
        ),
        404 => format!(
            "HTTP 404 — no such endpoint as `{model}`.\n\n\
             Run `lucida models --provider bfl` for the ones Lucida knows about. \
             Model ids here are URL paths, so a typo looks exactly like this."
        ),
        422 => format!(
            "HTTP 422 — the API rejected a parameter for `{model}`.\n\n\
             Endpoints in this family differ: `steps` and `guidance` exist only on \
             flux-2-flex and flux-dev, and no FLUX model takes a negative prompt.\n\n\
             Original message: {detail}"
        ),
        429 => format!(
            "HTTP 429 — too many active requests. BFL limits how many renders can \
             be in flight at once; wait for one to finish.\n\nOriginal message: {detail}"
        ),
        _ => format!("HTTP {status} — {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Aspect;

    #[test]
    fn only_flex_and_dev_expose_the_sampler() {
        assert!(!capabilities("flux-2-pro").steps);
        assert!(!capabilities("flux-2-max").steps);
        assert!(capabilities("flux-2-flex").steps);
        assert!(capabilities("flux-2-flex").guidance);
        assert!(capabilities("flux-dev").steps);
        // Aliases resolve before the lookup.
        assert!(!capabilities("flux").steps);
        assert!(capabilities("flux-flex").steps);
    }

    /// The roadmap predicted Flux would bring a negative prompt. It does not —
    /// on any endpoint. Asserted so the claim cannot quietly creep back.
    #[test]
    fn no_flux_endpoint_takes_a_negative_prompt() {
        for model in KNOWN_MODELS {
            assert!(
                !capabilities(model).negative_prompt,
                "{model} must not claim a negative prompt"
            );
        }
    }

    #[test]
    fn style_conditioning_models_do_not_claim_to_edit() {
        // flux-2 and kontext take input_image and genuinely edit…
        assert!(capabilities("flux-2-pro").references);
        assert!(capabilities("flux-kontext-pro").references);
        // …while these take image_prompt, which conditions style instead.
        assert!(!capabilities("flux-pro-1.1").references);
        assert!(!capabilities("flux-dev").references);
    }

    /// Measured on a real render: a signed C2PA manifest and no pixel
    /// watermark. Asserted because the tempting shorthand — "hosted Flux is
    /// unmarked" — is false, and the opposite shorthand ("marked like Google")
    /// is also false in the way that matters.
    #[test]
    fn provenance_is_c2pa_without_a_pixel_watermark() {
        assert_eq!(capabilities("flux-2-pro").provenance, Provenance::C2paOnly);
        assert_ne!(capabilities("flux-2-pro").provenance, Provenance::Unmarked);
    }

    #[test]
    fn references_become_numbered_fields() {
        assert_eq!(encode_reference("https://example.com/a.png").unwrap(),
                   "https://example.com/a.png");
    }

    /// An edit with no stated geometry must not carry dimensions, or the API
    /// reframes the picture to a square default. Measured the hard way: a 16:9
    /// source came back 1024x1024 with the composition destroyed, while the edit
    /// itself was perfectly good.
    #[test]
    fn an_edit_sends_no_dimensions_unless_asked() {
        let client = Client {
            key: "x".into(),
            http: reqwest::blocking::Client::new(),
            base: API_ROOT.into(),
        };

        let edit = ImageRequest {
            references: vec!["https://example.com/a.png".into()],
            ..Default::default()
        };
        let body = client.body(&edit, "flux-2-pro").unwrap();
        assert!(body.get("width").is_none(), "an edit must not force a size");
        assert_eq!(body["input_image"], "https://example.com/a.png");

        // Stating one overrides that — this is how an edit reframes.
        let reframed = ImageRequest {
            aspect: Some(Aspect::parse("1:1").unwrap()),
            ..edit
        };
        assert_eq!(client.body(&reframed, "flux-2-pro").unwrap()["width"], 1024);

        // Generation always carries dimensions; there is no source to infer from.
        let fresh = ImageRequest::default();
        assert_eq!(client.body(&fresh, "flux-2-pro").unwrap()["width"], 1024);
    }

    #[test]
    fn dimensions_land_on_the_32_pixel_grid() {
        let req = ImageRequest {
            aspect: Some(Aspect::parse("3:2").unwrap()),
            ..Default::default()
        };
        let (w, h) = req.pixels(DEFAULT_DIMENSIONS, PIXEL_GRID);
        assert_eq!(w % 32, 0);
        assert_eq!(h % 32, 0);
    }

    #[test]
    fn a_mistyped_model_is_explained_as_a_path() {
        let message = explain_error(404, "{}", "flux-2-prooo");
        assert!(message.contains("no such endpoint"));
        assert!(message.contains("lucida models --provider bfl"));
    }

    /// Measured against the live API: BFL reports a malformed key as a 422
    /// validation failure, not a 401. Read by status alone that becomes "a
    /// parameter was wrong", which sends people to inspect their prompt.
    #[test]
    fn a_bad_key_is_recognised_even_though_it_arrives_as_a_422() {
        let message = explain_error(422, r#"{"detail":"Invalid API key format"}"#, "credits");
        assert!(message.contains("API key was not accepted"));
        assert!(message.contains("lucida config"));
        assert!(!message.contains("flux-2-flex"), "must not be read as a parameter problem");
    }

    #[test]
    fn parameter_rejection_names_the_models_that_would_accept_it() {
        let message = explain_error(422, r#"{"detail":"steps not permitted"}"#, "flux-2-pro");
        assert!(message.contains("flux-2-flex"));
    }

    // --- recorded responses -------------------------------------------------

    use crate::provider::ImageProvider;
    use crate::testserver::{Reply, serve};

    fn wired(server: &crate::testserver::Server) -> Client {
        Client {
            key: "test-key".into(),
            base: server.url().to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .no_proxy()
                .build()
                .unwrap(),
        }
    }

    /// The whole call shape, replayed: submit, poll at the URL the API handed
    /// back, download the result. Three claims that only the wire can prove:
    ///
    /// 1. The polling URL is used **verbatim** — the docs insist on this because
    ///    the global endpoint delegates to a regional one.
    /// 2. The key travels on submit and poll as `x-key`.
    /// 3. The download — a signed URL into third-party object storage — carries
    ///    **no credential at all**. Sending one there is how keys leak.
    #[test]
    fn the_signed_download_url_never_receives_the_api_key() {
        let submit = r#"{"id":"abc","polling_url":"{{server}}/v1/get_result?id=abc","cost":0.06}"#;
        let ready = r#"{"status":"Ready","result":{"sample":"{{server}}/delivery/img.png"}}"#;
        let server = serve(vec![
            Reply::json(submit),
            Reply::json(ready),
            Reply::bytes("image/png", b"png-bytes"),
        ]);

        let request = ImageRequest {
            prompt: "a fox".into(),
            model: "flux-2-pro".into(),
            seed: Some(7),
            ..Default::default()
        };
        let image = wired(&server).generate(&request).unwrap();
        assert_eq!(image.bytes, b"png-bytes");
        assert_eq!(image.seed, Some(7), "the pinned seed is reported back");

        let requests = server.finish();
        assert_eq!(requests.len(), 3);

        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/flux-2-pro", "the model id is the path under /v1");
        assert_eq!(requests[0].header("x-key"), Some("test-key"));
        let body = requests[0].json();
        assert_eq!(body["prompt"], "a fox");
        assert_eq!(body["seed"], 7);
        assert_eq!(body["width"], 1024, "generation always states its dimensions");
        assert_eq!(body["output_format"], "png");

        // Poll where told, not where a hand-built URL would go.
        assert_eq!(requests[1].path, "/v1/get_result?id=abc");
        assert_eq!(requests[1].header("x-key"), Some("test-key"));

        // The signed URL gets no key.
        assert_eq!(requests[2].path, "/delivery/img.png");
        assert_eq!(requests[2].header("x-key"), None);
    }

    /// Both moderation states are terminal and the message distinguishes them —
    /// a recorded poll proves the mapping from the API's own status strings.
    #[test]
    fn a_moderated_prompt_stops_the_poll_with_a_clear_verdict() {
        let submit = r#"{"id":"abc","polling_url":"{{server}}/v1/get_result?id=abc"}"#;
        let moderated = r#"{"status":"Request Moderated"}"#;
        let server = serve(vec![Reply::json(submit), Reply::json(moderated)]);

        let request = ImageRequest {
            prompt: "a fox".into(),
            model: "flux-2-pro".into(),
            ..Default::default()
        };
        let error = wired(&server).generate(&request).unwrap_err().to_string();
        assert!(error.contains("content moderation"), "{error}");
        assert!(error.contains("Nothing was charged"));
        assert_eq!(server.finish().len(), 2, "no further polling after a terminal state");
    }

    /// An HTTP failure on submit flows through `explain_error` with the real
    /// body, so the whole path from status code to advice is exercised.
    #[test]
    fn running_out_of_credits_names_the_dashboard() {
        let server = serve(vec![Reply::status(402, r#"{"detail":"Not enough credits"}"#)]);
        let request = ImageRequest {
            prompt: "a fox".into(),
            model: "flux-2-pro".into(),
            ..Default::default()
        };
        let error = wired(&server).generate(&request).unwrap_err().to_string();
        assert!(error.contains("out of credits"), "{error}");
        assert!(error.contains("dashboard.bfl.ai"));
    }
}
