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
    AspectSupport, Capabilities, GeneratedImage, ImageProvider, ImageRequest, MaskSupport,
    Provenance, abandoned,
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

/// The most reference images an edit model takes, per model.
///
/// One ceiling of 8 used to cover the family, with a note that klein stopped at
/// four — and the body builder numbered fields up to `input_image_8` for every
/// model. Kontext and klein have no field past `input_image_4`, so the extra
/// images were dropped by BFL without an error and the edit came back as a
/// success built from half of what was asked for.
///
/// Read 2026-10-04 from `https://api.bfl.ai/openapi.json`: `Flux2Inputs`
/// (`/v1/flux-2-pro`, `/v1/flux-2-max`) and `Flux2FlexInputs` (`/v1/flux-2-flex`)
/// define `input_image` through `input_image_8`; `Flux2KleinInputs`
/// (`/v1/flux-2-klein-9b`, `/v1/flux-2-klein-4b`) and `FluxKontextProInputs`
/// (`/v1/flux-kontext-pro`, `/v1/flux-kontext-max`) stop at `input_image_4`.
/// `None` for a model that takes no reference images at all, and for an
/// unknown `flux-2-*` id, which keeps the family's largest figure.
fn reference_ceiling(id: &str) -> Option<usize> {
    if id.starts_with("flux-kontext") || id.starts_with("flux-2-klein") {
        Some(4)
    } else if id.starts_with("flux-2") {
        Some(8)
    } else {
        None
    }
}

/// The ratios offered on the endpoints that take an `aspect_ratio` string and
/// no pixel dimensions.
///
/// BFL documents a *range* for these, 21:9 to 9:21 (the Kontext pages spell the
/// same bounds 7:3 and 3:7), not a list. `AspectSupport::Named` can only say
/// "these exact strings", so this is the conventional set inside that range,
/// written the way the OpenAPI spells its bounds. A ratio inside the range that
/// is not named here is refused rather than passed on: an offer cannot claim
/// more than the table can hold, and the other BFL models still take any ratio.
///
/// Read 2026-10-04 from `https://api.bfl.ai/openapi.json`, schemas
/// `FluxKontextProInputs` (`/v1/flux-kontext-pro` and `/v1/flux-kontext-max`)
/// and `FluxUltraInput` (`/v1/flux-pro-1.1-ultra`), where `aspect_ratio` is
/// "Aspect ratio of the image between 21:9 and 9:21" and `width`/`height` do not
/// exist.
const RATIO_ONLY_ASPECTS: &[&str] = &[
    "21:9", "16:9", "3:2", "4:3", "5:4", "1:1", "4:5", "3:4", "2:3", "9:16", "9:21",
];

/// Whether the endpoint takes an `aspect_ratio` string in place of `width` and
/// `height`. Sending the pixel fields there is not an error from BFL: they are
/// simply not part of the schema, so the render comes back at the model's own
/// shape and the geometry that was asked for is lost without a word.
fn takes_aspect_ratio_only(id: &str) -> bool {
    id.starts_with("flux-kontext") || id == "flux-pro-1.1-ultra"
}

/// The known models that take only a ratio from a short list and no size, read
/// off [`capabilities`] so that every sentence naming them (the MCP schema, the
/// `--size` refusal, `lucida models`) is generated and cannot drift from it.
pub fn ratio_only_models() -> Vec<&'static str> {
    KNOWN_MODELS.iter().copied().filter(|m| !capabilities(m).size).collect()
}

/// The known models that take pixel dimensions: any ratio, and `--size`.
pub fn sized_models() -> Vec<&'static str> {
    KNOWN_MODELS.iter().copied().filter(|m| capabilities(m).size).collect()
}

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
        // Lowercased to match the other hosted providers: `/v1/FLUX-2-PRO`
        // 404s, and no live FLUX endpoint contains uppercase (measured
        // 2026-08-02). See genai::resolve_model for why ComfyUI differs.
        .unwrap_or(key)
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

    // The same per-model disagreement, for geometry: the FLUX.2 and FLUX.1.1
    // endpoints take pixels, Kontext and Ultra take only a ratio.
    let ratio_only = takes_aspect_ratio_only(&id);

    Capabilities {
        provider: "bfl",
        // Names models by hand, because a tagline is a `&'static str`; held
        // against this table by `the_tagline_names_exactly_the_models_the_table_does`.
        tagline: "Hosted FLUX. Paid, fast, edits well. Its capabilities differ per MODEL: steps and guidance exist on flux-2-flex and flux-dev alone, and flux-kontext-* and flux-pro-1.1-ultra take a ratio from a short list instead of a size.",
        aspect: if ratio_only {
            AspectSupport::Named(RATIO_ONLY_ASPECTS)
        } else {
            AspectSupport::Free {
                multiple_of: PIXEL_GRID,
            }
        },
        size: !ratio_only,
        seed: true,
        // Measured, not assumed: no FLUX endpoint takes one.
        negative_prompt: false,
        references: edits,
        max_references: reference_ceiling(&id),
        mask: MaskSupport::No,
        workflow: false,
        steps: tunable,
        guidance: tunable,
        // Verified in a real render rather than assumed: a signed C2PA manifest
        // in a caBX chunk, and no SynthID at all. That is a third state — Google
        // marks the pixels too, ComfyUI marks nothing — and the difference
        // matters, because a re-encode strips C2PA and cannot strip SynthID.
        provenance: Provenance::C2paOnly,
        needs_reference: false,
        foreign_model: None,
        reference_formats: None,
        max_long_edge: None,
        seed_limit: None,
    }
}

/// Whether a polling URL may be sent the API key.
///
/// The docs say to follow the returned `polling_url` rather than build one,
/// because the global endpoint hands work to a regional one — so the host is not
/// fixed, only its family is: the configured base URL's own host, or any
/// `*.bfl.ai`, and the latter only over https. The base's exact origin is also
/// accepted whatever its scheme, which is what keeps a recorded-response server
/// on `http://127.0.0.1` working; production's base is https, so it admits
/// nothing plaintext. Parsed rather than prefix-matched, so
/// `https://api.bfl.ai@evil.example/` and `https://evilbfl.ai/` are refused.
fn trusted_polling_url(base: &str, url: &str) -> bool {
    let (Ok(url), Ok(base)) = (reqwest::Url::parse(url), reqwest::Url::parse(base)) else {
        return false;
    };
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    if url.origin() == base.origin() {
        return true;
    }
    url.scheme() == "https"
        && (base.host_str().is_some_and(|b| b.eq_ignore_ascii_case(&host))
            || host.ends_with(".bfl.ai"))
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
            .connect_timeout(crate::retry::CONNECT_TIMEOUT)
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
        //
        // Kontext and Ultra have no pixel fields at all, only `aspect_ratio`, so
        // for them the same rule is spelled with the ratio: stated, or the
        // square default when there is no source to take a shape from. Kontext
        // is 1:1 by BFL's own default; Ultra's is 16:9, which would break the
        // square every other lane renders, so it is always sent. `check` has
        // already refused a `--size` and any ratio outside the list, so what
        // arrives here is passed on as written.
        let asked_for_dimensions = req.aspect.is_some() || req.size.is_some();
        if takes_aspect_ratio_only(model) {
            let ratio = match req.aspect {
                Some(aspect) => Some(aspect.to_string()),
                None if req.references.is_empty() => Some("1:1".to_string()),
                None => None,
            };
            if let Some(ratio) = ratio {
                body.insert("aspect_ratio".into(), json!(ratio));
            }
        } else if asked_for_dimensions || req.references.is_empty() {
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
            body.insert("guidance".into(), json!(crate::provider::guidance_as_written(guidance)));
        }

        // `Capabilities::check` has already refused a count over this model's
        // ceiling, as exit 2. Held here too because a field past the ceiling is
        // not an error at BFL — it is ignored — so a path that skipped the check
        // must still never number one. Before the loop, so nothing is read.
        if let Some(most) = reference_ceiling(model) {
            if req.references.len() > most {
                bail!(
                    "`{model}` accepts at most {most} reference images; {} were given.",
                    req.references.len()
                );
            }
        }

        // Reference images are numbered fields rather than an array:
        // input_image, input_image_2, … up to the model's ceiling.
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
        // Deliberately not retried (see `retry`): this is the billed call.
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
    ///
    /// The submit has been billed by the time this runs, so every way out of
    /// the *wait* — a cancellation, the deadline, a poll or download that
    /// failed — is marked [`abandoned`], and the caller records the spend. The
    /// provider's own terminal answers below are not: moderation and `Error`
    /// are BFL's verdicts on the render, and whether they bill is not known.
    fn await_image(&self, polling_url: &str) -> Result<Vec<u8>> {
        let started = Instant::now();
        let deadline = Duration::from_secs(600);
        let mut interval = Duration::from_millis(1000);
        let mut announced = String::new();
        let billed = |error: anyhow::Error| abandoned(polling_url, error);

        // The polling URL arrives in the submit response and every poll carries
        // the API key to it, so a response that named some other host would be
        // handed the key. Checked before the first poll, and marked abandoned
        // like every other way out of the wait: the submit was billed, and the
        // URL named here is the one handle left on what was paid for.
        if !trusted_polling_url(&self.base, polling_url) {
            return Err(billed(anyhow!(
                "the API named a polling URL Lucida will not send the key to: \
                 {polling_url}\n\n\
                 Polling is limited to the API's own host (or a `*.bfl.ai` regional \
                 one, over https), because each poll carries `x-key`. The render \
                 was submitted and may be billed; nothing was polled."
            )));
        }

        loop {
            // Checked before the first poll as well as between the rest, so a
            // client that has gone stops costing a poll at once. The render is
            // submitted and billed by this point, so a cancellation stops the
            // *waiting*, not the charge — and the error carries the polling URL,
            // the one handle left on what was paid for. It used to return
            // `cancel::check`'s bare message, which named nothing.
            crate::cancel::check().map_err(|e| {
                billed(anyhow!("{e} Its polling URL was {polling_url}"))
            })?;

            if started.elapsed() > deadline {
                return Err(billed(anyhow!(
                    "gave up after {} minutes. The job may still complete; its \
                     polling URL was {polling_url}",
                    deadline.as_secs() / 60
                )));
            }

            std::thread::sleep(interval);
            // Backs off to keep a slow job from becoming a tight loop, but stays
            // brisk early because most renders finish in seconds.
            interval = (interval * 2).min(Duration::from_secs(3));

            let response = crate::retry::send_idempotent("polling the render", || {
                self.http.get(polling_url).header("x-key", &self.key)
            })
            .context("polling the render")
            .map_err(billed)?;

            let status = response.status();
            if !status.is_success() {
                let text = response.text().unwrap_or_default();
                // Not routed through explain_error's 404 branch: that one is
                // about model endpoints and suggests `lucida models`, which is
                // misleading advice for a job that has simply expired.
                if status.as_u16() == 404 {
                    return Err(billed(anyhow!(
                        "the render is no longer available at its polling URL — \
                         results expire shortly after completion. Submit the \
                         render again.\n\nOriginal message: {}",
                        text.trim()
                    )));
                }
                return Err(billed(anyhow!(
                    "{}",
                    explain_error(status.as_u16(), &text, "get_result")
                )));
            }

            let payload: Value = response
                .json()
                .context("parsing the poll response")
                .map_err(billed)?;
            let state = payload["status"].as_str().unwrap_or_default();

            match state {
                "Ready" => {
                    eprintln!("Render finished in {}s.", started.elapsed().as_secs());
                    return self.download(&payload).map_err(billed);
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

    /// What to say when the finished image cannot be fetched.
    ///
    /// The URL itself is the whole message. The render is paid for and complete
    /// at this point, the URL is signed and expires in about ten minutes, and
    /// without printing it the only record of a bought image is a stack trace
    /// that does not contain it. Ten minutes is not long, but it is long enough
    /// to paste into a browser — which is more recovery than the previous
    /// message offered, which was none.
    fn rescue(url: &str) -> String {
        format!(
            "The render finished and was billed. Its URL is signed and expires \
             about 10 minutes after the render completed — fetch it by hand \
             while it lasts:\n\n  {url}"
        )
    }

    fn download(&self, payload: &Value) -> Result<Vec<u8>> {
        let url = payload["result"]["sample"]
            .as_str()
            .ok_or_else(|| anyhow!("the render is ready but carries no image: {payload}"))?;

        // No API key on this request: it is a signed URL pointing at object
        // storage, and sending a credential to a third-party host that does not
        // need it is how credentials end up somewhere unexpected.
        let response = crate::retry::send_idempotent("downloading the image", || self.http.get(url))
            .with_context(|| format!("downloading the finished image.\n\n{}", Self::rescue(url)))?;

        if !response.status().is_success() {
            bail!(
                "the image URL returned HTTP {}.\n\n{}",
                response.status().as_u16(),
                Self::rescue(url)
            );
        }

        Ok(response.bytes().context("reading image bytes")?.to_vec())
    }
}

impl ImageProvider for Client {
    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage> {
        let model = resolve_model(&req.model);

        let stated = req.aspect.is_some() || req.size.is_some();
        let shape = if takes_aspect_ratio_only(&model)
            && (req.aspect.is_some() || req.references.is_empty())
        {
            // No pixel count to print: the endpoint takes a ratio and chooses
            // the size itself.
            req.aspect.map_or_else(|| "1:1".to_string(), |a| a.to_string())
        } else if stated || req.references.is_empty() {
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
        let response = crate::retry::send_idempotent("checking the key", || {
            self.http
                .get(format!("{}/credits", self.base))
                .header("x-key", &self.key)
        })
        .context("checking the API key against /v1/credits")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text, "credits"));
        }

        if let Ok(payload) = response.json::<Value>() {
            if let Some(credits) = payload["credits"].as_f64() {
                eprintln!("Key is valid. Remaining credits: {credits}");
            }
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
    /// The guidance sent is the one written: 7.1, not the 7.099999904632568
    /// an `f32` widens to through `json!`.
    #[test]
    fn guidance_is_sent_as_written() {
        let client = Client {
            key: "x".into(),
            http: reqwest::blocking::Client::new(),
            base: API_ROOT.into(),
        };
        let req = ImageRequest {
            guidance: Some(7.1),
            ..Default::default()
        };
        let body = client.body(&req, "flux-2-flex").unwrap();
        assert_eq!(body["guidance"].to_string(), "7.1");
    }

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

    /// BFL's OpenAPI gives `flux-kontext-*` and `flux-pro-1.1-ultra` an
    /// `aspect_ratio` string and no `width`/`height`. They used to be declared
    /// free-form and sent pixels the endpoint does not read, so a requested
    /// shape came back as the model's own, with nothing said.
    #[test]
    fn kontext_and_ultra_take_a_ratio_and_no_size() {
        for model in ["flux-kontext-pro", "flux-kontext-max", "flux-pro-1.1-ultra"] {
            let caps = capabilities(model);
            assert!(!caps.size, "{model} has no pixel fields");
            let AspectSupport::Named(ratios) = caps.aspect else {
                panic!("{model} must offer named ratios");
            };
            assert!(ratios.contains(&"16:9") && ratios.contains(&"21:9") && ratios.contains(&"9:21"));
        }
        // Every other endpoint, pixels included, is unchanged.
        for model in ["flux-2-pro", "flux-2-flex", "flux-pro-1.1", "flux-dev"] {
            let caps = capabilities(model);
            assert!(caps.size, "{model}");
            assert!(matches!(caps.aspect, AspectSupport::Free { .. }), "{model}");
        }
    }

    /// The partition the MCP schema, `lucida models` and the `--size` refusal
    /// are generated from.
    #[test]
    fn the_known_models_partition_by_whether_they_take_a_size() {
        let ratio_only = ratio_only_models();
        let sized = sized_models();
        assert_eq!(ratio_only, ["flux-pro-1.1-ultra", "flux-kontext-pro", "flux-kontext-max"]);
        assert_eq!(ratio_only.len() + sized.len(), KNOWN_MODELS.len());
        assert!(sized.contains(&"flux-2-pro") && sized.contains(&"flux-dev"));
    }

    #[test]
    fn kontext_and_ultra_send_aspect_ratio_instead_of_pixels() {
        let client = Client {
            key: "x".into(),
            http: reqwest::blocking::Client::new(),
            base: API_ROOT.into(),
        };
        for model in ["flux-kontext-pro", "flux-kontext-max", "flux-pro-1.1-ultra"] {
            let asked = ImageRequest {
                aspect: Some(Aspect::parse("16:9").unwrap()),
                ..Default::default()
            };
            let body = client.body(&asked, model).unwrap();
            assert_eq!(body["aspect_ratio"], "16:9", "{model}");
            assert!(body.get("width").is_none() && body.get("height").is_none(), "{model}");

            // Nothing stated, nothing to take a shape from: the square default,
            // sent explicitly because Ultra would otherwise choose 16:9.
            let fresh = client.body(&ImageRequest::default(), model).unwrap();
            assert_eq!(fresh["aspect_ratio"], "1:1", "{model}");
            assert!(fresh.get("width").is_none(), "{model}");
        }

        // An edit with no stated shape keeps its source's, as on the pixel models.
        let edit = ImageRequest {
            references: vec!["https://example.com/a.png".into()],
            ..Default::default()
        };
        let body = client.body(&edit, "flux-kontext-pro").unwrap();
        assert!(body.get("aspect_ratio").is_none() && body.get("width").is_none());
        assert_eq!(body["input_image"], "https://example.com/a.png");

        // The pixel models are untouched.
        let body = client.body(&ImageRequest::default(), "flux-2-pro").unwrap();
        assert_eq!(body["width"], 1024);
        assert!(body.get("aspect_ratio").is_none());
    }

    #[test]
    fn what_kontext_and_ultra_cannot_take_is_refused() {
        for model in ["flux-kontext-pro", "flux-pro-1.1-ultra"] {
            let caps = capabilities(model);
            let sized = ImageRequest {
                size: Some(crate::provider::Size(2048)),
                model: model.into(),
                ..Default::default()
            };
            let error = caps.check(&sized).unwrap_err().to_string();
            assert!(error.contains("--size"), "{model}: {error}");
            // It points at the BFL models that do take a size, not at "bfl"
            // wholesale, which is where this refusal came from.
            for sized in sized_models() {
                assert!(error.contains(sized), "{model}: {error}");
            }
            assert!(!error.contains("`bfl` if"), "{model}: {error}");

            // Inside BFL's range, but not a ratio the table names.
            let odd = ImageRequest {
                aspect: Some(Aspect::parse("17:10").unwrap()),
                model: model.into(),
                ..Default::default()
            };
            assert!(caps.check(&odd).is_err(), "{model}");

            let fine = ImageRequest {
                aspect: Some(Aspect::parse("3:2").unwrap()),
                model: model.into(),
                ..Default::default()
            };
            caps.check(&fine).unwrap();
        }
        // The pixel models still take a size and any ratio.
        let caps = capabilities("flux-2-pro");
        caps.check(&ImageRequest {
            size: Some(crate::provider::Size(2048)),
            aspect: Some(Aspect::parse("17:10").unwrap()),
            model: "flux-2-pro".into(),
            ..Default::default()
        })
        .unwrap();
    }

    /// The tagline names models by hand, so it is held against the table.
    ///
    /// It is a `&'static str` beside the measured capabilities, which is where
    /// a tagline stays true — but this one lists which models take steps and
    /// which take only a ratio, and those lists are the capabilities restated.
    /// Each clause is read back, its names (a trailing `*` is a prefix) expanded
    /// against `KNOWN_MODELS`, and compared with what `capabilities` says. A
    /// model added to either group, or a sentence rewritten so its clause can no
    /// longer be found, fails here rather than in front of an agent.
    #[test]
    fn the_tagline_names_exactly_the_models_the_table_does() {
        fn expand(clause: &str) -> Vec<&'static str> {
            let mut named: Vec<&'static str> = clause
                .split(" and ")
                .flat_map(|part| part.split(", "))
                .flat_map(|name| {
                    let name = name.trim();
                    let hits: Vec<&'static str> = match name.strip_suffix('*') {
                        Some(prefix) => {
                            KNOWN_MODELS.iter().copied().filter(|m| m.starts_with(prefix)).collect()
                        }
                        None => KNOWN_MODELS.iter().copied().filter(|m| *m == name).collect(),
                    };
                    assert!(!hits.is_empty(), "the tagline names `{name}`, which is no known model");
                    hits
                })
                .collect();
            named.sort_unstable();
            named
        }
        let sorted = |mut v: Vec<&'static str>| {
            v.sort_unstable();
            v
        };

        let tagline = capabilities(DEFAULT_MODEL).tagline;

        let steps = tagline
            .split_once("steps and guidance exist on ")
            .and_then(|(_, rest)| rest.split_once(" alone"))
            .map(|(clause, _)| clause)
            .unwrap_or_else(|| panic!("no steps clause in the tagline: {tagline}"));
        let tunable = KNOWN_MODELS.iter().copied().filter(|m| capabilities(m).steps).collect();
        assert_eq!(expand(steps), sorted(tunable), "{tagline}");

        let ratio = tagline
            .split_once(" take a ratio")
            .and_then(|(before, _)| before.rsplit_once(", and "))
            .map(|(_, clause)| clause)
            .unwrap_or_else(|| panic!("no ratio clause in the tagline: {tagline}"));
        assert_eq!(expand(ratio), sorted(ratio_only_models()), "{tagline}");
    }

    /// Each edit model's reference ceiling is its own, and a request over it is
    /// refused before anything is read or sent.
    ///
    /// One ceiling of 8 covered the family, and the body builder numbered
    /// fields up to `input_image_8` for every model — but Kontext and klein
    /// have no field past `input_image_4`, so images five to eight were dropped
    /// by BFL without a word and the edit came back as a success built from
    /// half of what was asked for. The figures are BFL's schemas, read from
    /// `https://api.bfl.ai/openapi.json`.
    #[test]
    fn each_edit_model_refuses_references_past_its_own_ceiling() {
        let refs = |n: usize| -> Vec<String> {
            (0..n).map(|i| format!("https://example.com/{i}.png")).collect()
        };
        for (model, most) in [
            ("flux-2-pro", 8),
            ("flux-2-max", 8),
            ("flux-2-flex", 8),
            ("flux-2-klein-9b", 4),
            ("flux-2-klein-4b", 4),
            ("flux-kontext-pro", 4),
            ("flux-kontext-max", 4),
        ] {
            let caps = capabilities(model);
            caps.check(&ImageRequest {
                references: refs(most),
                model: model.into(),
                ..Default::default()
            })
            .unwrap_or_else(|e| panic!("{model} refused {most}: {e:#}"));

            let error = caps
                .check(&ImageRequest {
                    references: refs(most + 1),
                    model: model.into(),
                    ..Default::default()
                })
                .expect_err(&format!("{model} accepted {} references", most + 1));
            assert_eq!(crate::out::code_for(&error), crate::out::REFUSED, "{model}");
            let text = format!("{error:#}");
            assert!(text.contains(&format!("at most {most} reference")), "{model}: {text}");
            assert!(text.contains(model), "{model}: {text}");
        }
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
                .connect_timeout(crate::retry::CONNECT_TIMEOUT)
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
        let error = wired(&server).generate(&request).unwrap_err();
        // BFL's own verdict, not a wait that ended: it says nothing was
        // charged, so it must not be counted as an abandoned render.
        assert!(error.downcast_ref::<crate::provider::Abandoned>().is_none(), "{error:#}");
        let error = error.to_string();
        assert!(error.contains("content moderation"), "{error}");
        assert!(error.contains("Nothing was charged"));
        assert_eq!(server.finish().len(), 2, "no further polling after a terminal state");
    }

    /// A cancellation that lands after the submit has been billed still names
    /// the polling URL. It used to return `cancel::check`'s bare message, which
    /// says a render may complete and be billed but gives no way to find it.
    #[test]
    fn a_cancellation_after_the_submit_names_the_polling_url() {
        let submit = r#"{"id":"abc","polling_url":"{{server}}/v1/get_result?id=abc"}"#;
        let server = serve(vec![Reply::json(submit)]);

        let request = ImageRequest {
            prompt: "a fox".into(),
            model: "flux-2-pro".into(),
            ..Default::default()
        };
        let token = crate::cancel::Token::new();
        token.cancel();
        let error = crate::cancel::with(token, || wired(&server).generate(&request))
            .unwrap_err()
            .to_string();
        assert!(error.contains("cancelled"), "{error}");
        assert!(error.contains("/v1/get_result?id=abc"), "must name the polling URL: {error}");
        assert_eq!(server.finish().len(), 1, "a cancelled wait must not poll");
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
        assert_eq!(server.finish().len(), 1, "a 402 must not be retried or polled");
    }

    /// Renders `request` the way both image call sites do — through
    /// `generate_billed` — and returns the error with the ledger entries that
    /// would follow it. Built in memory rather than written: a test must not
    /// append to the ledger of the machine running the suite.
    fn billed_entries(client: &Client, request: &ImageRequest) -> (anyhow::Error, Vec<Value>) {
        let mut entries = Vec::new();
        let error = crate::generate_billed(client, request, |abandoned| {
            entries.push(crate::ledger::abandoned_entry(
                "bfl",
                &request.model,
                &request.prompt,
                abandoned,
                0.06,
            ));
        })
        .expect_err("the render was meant to fail");
        (error, entries)
    }

    /// A wait cancelled after the submit was billed leaves exactly one
    /// `abandoned` entry carrying the estimate and the polling URL. It left
    /// none, so an MCP client that hung up and retried was billed twice and
    /// counted once.
    #[test]
    fn a_cancelled_wait_is_recorded_as_spend() {
        let submit = r#"{"id":"abc","polling_url":"{{server}}/v1/get_result?id=abc"}"#;
        let server = serve(vec![Reply::json(submit)]);
        let request = ImageRequest {
            prompt: "a fox".into(),
            model: "flux-2-pro".into(),
            ..Default::default()
        };

        let token = crate::cancel::Token::new();
        token.cancel();
        let client = wired(&server);
        let (error, entries) = crate::cancel::with(token, || billed_entries(&client, &request));

        assert!(format!("{error:#}").contains("cancelled"), "the message changed: {error:#}");
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["status"], crate::ledger::ABANDONED);
        assert_eq!(entries[0]["estimated_usd"], 0.06);
        assert_eq!(entries[0]["provider"], "bfl");
        assert!(
            entries[0]["handle"].as_str().unwrap().ends_with("/v1/get_result?id=abc"),
            "{entries:?}"
        );
        assert!(entries[0].get("operation").is_none(), "`ops` would list it: {entries:?}");
        server.finish();
    }

    /// A submit that failed was never billed, so it leaves nothing to count.
    #[test]
    fn a_failed_submit_is_not_recorded_as_spend() {
        let server = serve(vec![Reply::status(402, r#"{"detail":"Not enough credits"}"#)]);
        let request = ImageRequest {
            prompt: "a fox".into(),
            model: "flux-2-pro".into(),
            ..Default::default()
        };
        let (error, entries) = billed_entries(&wired(&server), &request);
        assert!(format!("{error:#}").contains("out of credits"), "{error:#}");
        assert!(entries.is_empty(), "an unbilled submit was counted: {entries:?}");
        server.finish();
    }

    #[test]
    fn only_the_apis_own_hosts_are_trusted_with_the_key() {
        let base = "https://api.bfl.ai/v1";
        for trusted in [
            "https://api.bfl.ai/v1/get_result?id=a",
            "https://api.eu1.bfl.ai/v1/get_result?id=a",
            "https://API.BFL.AI/v1/get_result?id=a",
        ] {
            assert!(trusted_polling_url(base, trusted), "{trusted}");
        }
        for untrusted in [
            "http://api.bfl.ai/v1/get_result?id=a",       // plaintext
            "https://evil.example/v1/get_result?id=a",    // another host
            "https://evilbfl.ai/v1/get_result?id=a",      // suffix without the dot
            "https://api.bfl.ai@evil.example/get_result", // userinfo trick
            "https://bfl.ai.evil.example/get_result",
            "not a url",
        ] {
            assert!(!trusted_polling_url(base, untrusted), "{untrusted}");
        }
        // A recorded-response server is its own base, whatever its scheme.
        assert!(trusted_polling_url("http://127.0.0.1:9/", "http://127.0.0.1:9/get_result"));
        assert!(!trusted_polling_url("http://127.0.0.1:9/", "http://127.0.0.1:10/get_result"));
    }

    /// The key is never sent to a host the submit response invented, and the
    /// refusal is still an abandoned render — it was billed — naming the URL.
    ///
    /// The "foreign" host is a second local server on another port: a different
    /// origin, over plain http, which the check must reject. Local so that a
    /// build with the check missing makes no outbound request — and so that the
    /// request it *would* make is recorded and counted rather than lost to DNS.
    #[test]
    fn a_foreign_polling_url_is_never_sent_the_key() {
        // Answers terminally if it is ever asked, so a missing check ends this
        // test quickly instead of polling on.
        let foreign = serve(vec![Reply::json(r#"{"status":"Request Moderated"}"#)]);
        let polling_url = format!("{}/v1/get_result?id=abc", foreign.url());
        let submit = format!(r#"{{"id":"abc","polling_url":"{polling_url}"}}"#);
        let server = serve(vec![Reply::json(&submit)]);

        let request = ImageRequest {
            prompt: "a fox".into(),
            model: "flux-2-pro".into(),
            ..Default::default()
        };
        let error = wired(&server).generate(&request).unwrap_err();
        let abandoned = error
            .downcast_ref::<crate::provider::Abandoned>()
            .expect("a billed submit's failure is marked abandoned");
        assert_eq!(abandoned.handle, polling_url);
        let message = error.to_string();
        assert!(message.contains("will not send the key to"), "{message}");
        assert!(message.contains(&polling_url), "{message}");

        assert_eq!(server.finish().len(), 1, "nothing but the submit went to the API");

        // The test server only stops listening once its script is used up, and
        // would otherwise hold `finish` for its whole 15 s deadline. So the test
        // makes the one request itself: what the foreign host recorded must be
        // that probe and nothing before it.
        use std::io::{Read, Write};
        let addr = foreign.url().trim_start_matches("http://").to_string();
        if let Ok(mut probe) = std::net::TcpStream::connect(&addr) {
            let _ = probe.write_all(b"GET /probe HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
            let _ = probe.read_to_end(&mut Vec::new());
        }
        let received = foreign.finish();
        let paths: Vec<&str> = received.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["/probe"], "the foreign host was sent a request by the client");
        assert_eq!(received[0].header("x-key"), None);
    }
}
