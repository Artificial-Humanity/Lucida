//! Runway — Gen-4 video, and Gen-4 images.
//!
//! The second video provider, and the one that makes video a *substitution*
//! rather than a single hardcoded lane, the way BFL did for images. The call
//! pattern is the familiar one — submit, poll, download — which is now four
//! providers running the same shape and a fair sign it is the right one.
//!
//! # Runway's own models only, deliberately
//!
//! The endpoint accepts far more than Runway builds. Measured 2026-08-09, the
//! `model` field on `/v1/image_to_video` takes `kling3.0_pro`, `veo3.1`,
//! `seedance2`, `hailuo3`, `grok_imagine_1_5`, `gemini_omni_flash` and more
//! alongside `gen4_turbo`, `gen4` and `gen4.5`. That makes Runway an aggregator,
//! and the 2026-08-09 product review declined aggregators for reasons that all
//! still apply: capabilities become unknowable per model, provenance passthrough
//! is undocumented, and pricing gains a margin on somebody else's model.
//!
//! One case makes it concrete. `veo3.1` here is a *second path* to a lane Lucida
//! already reaches directly on the user's own Google key — with a rate we have
//! verified and provenance we have measured. Routing it through Runway would add
//! a middleman to something we already own.
//!
//! So [`MODELS`] is Runway's own three, owner's call 2026-08-09, and
//! [`is_runway_model`] answers only for those. The rest are reachable by nobody
//! here, which is the intended state rather than an omission.
//!
//! # Three things measured rather than assumed
//!
//! **`X-Runway-Version` is mandatory.** Omitting it is a 400 — "The
//! X-Runway-Version header was not provided in the request" — not a default. It
//! is a date, and requests on a version older than four months may be rejected,
//! so [`API_VERSION`] is a value with its own note rather than a literal buried
//! in a header call.
//!
//! **Ratios are pixel pairs, not simplified ratios.** `1280:720`, not `16:9` —
//! and `gen4.5` accepts only the two landscape/portrait pairs where `gen4_turbo`
//! takes six. That is a seventh geometry model across six providers. Since
//! `Aspect` already holds a width and a height, `1280:720` parses as one
//! natively; what needed writing is [`nearest_ratio`], so someone asking for
//! `16:9` gets the pair that *is* 16:9 rather than a refusal.
//!
//! **Unknown fields are silently ignored.** A body carrying `nonsenseField`
//! validates. This is the Stability trap, not the OpenAI courtesy: absence of an
//! error proves nothing here, so every capability below was established by
//! reading a rejection that named the field, never by the lack of one.

use crate::provider::{
    Aspect, AspectSupport, Capabilities, DurationSupport, GeneratedImage, ImageProvider,
    ImageRequest, MaskSupport, Provenance, VideoCapabilities, VideoProvider,
};
use crate::video::{VideoRequest, VideoStatus, terminal};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const API_ROOT: &str = "https://api.dev.runwayml.com/v1";

/// The dated API version, sent on every request.
///
/// Mandatory: without it the API answers 400 rather than assuming a default.
/// Runway supports a version for four months after its successor ships, so this
/// is a thing to bump deliberately — and the canary is what would notice when it
/// stops being accepted, since nothing else would. It notices only when run, and
/// it is run by hand (see scripts/canary.sh).
const API_VERSION: &str = "2024-11-06";

/// Runway's own video models. Not the catalogue it fronts — see the module note.
///
/// `text_to_video` accepts only `gen4.5` of the three; `gen4_turbo` and `gen4`
/// animate a still and cannot start from a prompt alone. Both measured from the
/// endpoints' own rejections.
pub const MODELS: &[&str] = &["gen4_turbo", "gen4", "gen4.5"];

/// Newest, and the only one of the three that renders from text alone.
pub const DEFAULT_MODEL: &str = "gen4.5";

pub const MODEL_ALIASES: &[(&str, &str)] = &[
    ("runway", "gen4.5"),
    ("gen4", "gen4"),
    ("gen4-turbo", "gen4_turbo"),
    ("gen4.5", "gen4.5"),
];

/// The pixel pairs `gen4_turbo` accepts, read from its own rejection.
const TURBO_RATIOS: &[&str] = &[
    "1280:720", "720:1280", "1104:832", "832:1104", "960:960", "1584:672",
];

/// `gen4.5` takes only landscape and portrait — measured, and a reminder that
/// capabilities vary per model here as they do on BFL.
const GEN45_RATIOS: &[&str] = &["1280:720", "720:1280"];

pub fn resolve_model(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    MODEL_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, id)| (*id).to_string())
        .unwrap_or(key)
}

/// Whether a model id belongs to this provider.
///
/// Answers only for Runway's own three. A `kling3.0_pro` typed by a hopeful user
/// is *not* claimed here, so it falls through to Google and is refused there by
/// name — which is a better outcome than being quietly accepted by a provider
/// this project decided not to expose.
pub fn is_runway_model(model: &str) -> bool {
    let id = resolve_model(model);
    MODELS.contains(&id.as_str())
}

pub fn capabilities(model: &str) -> VideoCapabilities {
    let id = resolve_model(model);
    let turbo = id == "gen4_turbo" || id == "gen4";

    VideoCapabilities {
        provider: "runway",
        tagline: "Gen-4. Paid, per second, and a real alternative to Veo — no Google account, and durations from 2 to 10 seconds rather than three fixed lengths.",
        aspect: AspectSupport::Pixels(if turbo { TURBO_RATIOS } else { GEN45_RATIOS }),
        // Measured from both bounds: "expected number to be >=2" and "<=10".
        duration: DurationSupport::Range { min: 2, max: 10 },
        image_to_video: true,
        // `gen4_turbo` and `gen4` are absent from /v1/text_to_video's accepted
        // list, so they genuinely cannot start from a prompt.
        text_to_video: !turbo,
        // No negativePrompt field on either endpoint. Not merely unverified:
        // unknown fields here are silently ignored, so sending one would be the
        // silent drop this project exists to refuse.
        negative_prompt: false,
        // The ratio decides the pixel count; there is no separate resolution.
        resolution: false,
        seed: true,
        // No quality tiers: on Runway the model id *is* the tier.
        modes: &[],
        // `Unverified`, which this enum kept a variant for and whose doc
        // comment predicted this exact moment: it is where a new provider starts
        // before anyone has rendered anything with it. Runway publishes C2PA
        // support for its own output, but the standard here is a manifest read
        // out of bytes we rendered ourselves — BFL shipped as `Unverified`, one
        // render proved it `C2paOnly`, and the guess most people would have made
        // was wrong. Not claiming is the honest state until a render settles it.
        provenance: Provenance::Unverified,
        // Empty is the default, resolved later. Anything else outside the three
        // is a model this endpoint fronts — `veo3.1`, `kling3.0_pro` — which
        // `--provider runway` must not reach. See the module note.
        foreign_model: (!id.is_empty() && !MODELS.contains(&id.as_str())).then_some(MODELS),
    }
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------
//
// Runway's image endpoint, `POST /v1/text_to_image`, is the same submit-poll-
// download shape as its video lane, on the same key, base URL and version
// header — which is why it lives here rather than in a module of its own.
//
// **Its own two models only**, the line the video lane draws and for the same
// reasons. On 2026-09-27 the endpoint's `model` field listed twelve: these two,
// and ten it fronts (`gemini_image3_pro`, `gpt_image_2`, `seedream5_pro`,
// `grok_imagine_image_2` and more). Coverage being per-credential reopens that
// question for someone holding only a Runway key, and it is an open question in
// the roadmap — but it is a separate decision, not a side effect of this lane.
//
// Everything below was read from free validation errors on 2026-09-27, each
// probe carrying a deliberately invalid `ratio` so that nothing could render.

/// Runway's own image models, from the endpoint's own list.
pub const IMAGE_MODELS: &[&str] = &["gen4_image", "gen4_image_turbo"];

/// The one of the two that can start from a prompt alone.
pub const DEFAULT_IMAGE_MODEL: &str = "gen4_image";

pub const IMAGE_ALIASES: &[(&str, &str)] = &[
    ("gen4-image", "gen4_image"),
    ("gen4-image-turbo", "gen4_image_turbo"),
];

/// The pixel pairs both image models accept — identical lists, in the
/// endpoint's own order, which puts the 1080-class pair before the 720-class
/// one of the same shape. [`nearest_ratio`] keeps the first of equally near
/// pairs, so `--aspect 16:9` renders at `1920:1080`.
const IMAGE_RATIOS: &[&str] = &[
    "1024:1024", "1080:1080", "1168:880", "1360:768", "1440:1080", "1080:1440", "1808:768",
    "1920:1080", "1080:1920", "2112:912", "1280:720", "720:1280", "720:720", "960:720", "720:960",
    "1680:720",
];

/// A render that has not resolved by now is abandoned as a wait, not as a
/// charge — the same bound BFL uses.
const IMAGE_DEADLINE: Duration = Duration::from_secs(600);

pub fn resolve_image_model(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    IMAGE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, id)| (*id).to_string())
        .unwrap_or(key)
}

/// Whether a model id is one of Runway's own image models.
pub fn is_runway_image_model(model: &str) -> bool {
    IMAGE_MODELS.contains(&resolve_image_model(model).as_str())
}

pub fn image_capabilities(model: &str) -> Capabilities {
    let id = resolve_image_model(model);
    Capabilities {
        provider: "runway",
        tagline: "Gen-4 images on a Runway key — up to three reference images, a seed, and sixteen fixed pixel ratios.",
        aspect: AspectSupport::Pixels(IMAGE_RATIOS),
        // The ratio is a pixel pair — `1920:1080` — so it *is* the size, and the
        // endpoint has no other dimension field.
        size: false,
        // "expected number to be >=0" and "<=4294967295".
        seed: true,
        // No field for one, and unknown fields are silently ignored here — so
        // sending one would be exactly the silent drop this refuses.
        negative_prompt: false,
        // `referenceImages`: at most three, each `https://`, `runway://` or
        // `data:image/`.
        references: true,
        mask: MaskSupport::No,
        workflow: false,
        steps: false,
        guidance: false,
        // Measured 2026-09-27 on one `gen4_image` render (1024:1024, 8 credits):
        // a `caBX` chunk holding a C2PA manifest signed by a `runwayml.com`
        // certificate, naming "Runway Image Generation" 4.0.0 as software agent
        // and asserting `digitalSourceType: trainedAlgorithmicMedia` — and no
        // pixel watermark found. The same evidence BFL's classification rests
        // on. Runway's *video* lane is a different product and stays
        // `Unverified` until one of its renders is read the same way.
        provenance: Provenance::C2paOnly,
        // Measured: "expected array, received undefined" for `referenceImages`.
        needs_reference: id == "gen4_image_turbo",
        // Empty is the default, resolved later; anything else outside the two
        // is one of the ten models this endpoint fronts.
        foreign_model: (!id.is_empty() && !IMAGE_MODELS.contains(&id.as_str()))
            .then_some(IMAGE_MODELS),
    }
}

/// How a Runway task stands, shared by the image and video lanes.
enum Task {
    Pending,
    /// The first output URL.
    Done(String),
}

pub struct Client {
    key: String,
    http: reqwest::blocking::Client,
    /// `API_ROOT` in production; a recorded-response server in tests.
    base: String,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        let key = crate::config::var("RUNWAY_API_KEY").ok_or_else(|| {
            let where_to_put_it = match crate::config::preferred_path() {
                Some(path) => format!(
                    "Set RUNWAY_API_KEY, or add it to {} — `lucida config \
                     --set RUNWAY_API_KEY` reads it from stdin so it stays \
                     out of your shell history.",
                    path.display()
                ),
                None => "Set RUNWAY_API_KEY.".to_string(),
            };
            anyhow!(
                "no Runway API key found.\n\n{where_to_put_it}\n\n\
                 Keys come from https://dev.runwayml.com — this is a paid API and \
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

    #[cfg(test)]
    pub(crate) fn recorded(base: &str) -> Self {
        Self {
            key: "test-key".into(),
            base: base.to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .connect_timeout(crate::retry::CONNECT_TIMEOUT)
                .no_proxy()
                .build()
                .unwrap(),
        }
    }

    /// Both headers, on every request. The version one is not optional.
    fn authed(&self, builder: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        builder
            .header("Authorization", format!("Bearer {}", self.key))
            .header("X-Runway-Version", API_VERSION)
    }

    /// Remaining credits. Free, and the only way to check a key without
    /// spending — the same role BFL's `/credits` and Stability's balance play.
    pub fn credits(&self) -> Result<f64> {
        let response = crate::retry::send_idempotent("checking the balance", || {
            self.authed(self.http.get(format!("{}/organization", self.base)))
        })
        .context("checking the Runway credit balance")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        let payload: Value = response.json().context("parsing the balance response")?;
        payload["creditBalance"]
            .as_f64()
            .ok_or_else(|| anyhow!("no creditBalance in the response: {payload}"))
    }

    fn body(&self, req: &VideoRequest, model: &str) -> Result<(&'static str, Value)> {
        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(model));

        let endpoint = match &req.image {
            Some(path) => {
                // Runway takes the image as a data URI rather than raw bytes, so
                // a local file needs encoding rather than uploading — no second
                // round trip, unlike ComfyUI.
                let bytes = std::fs::read(path)
                    .with_context(|| format!("reading source image {path}"))?;
                let mime = crate::sniff_mime(&bytes).unwrap_or("image/png");
                use base64::{Engine as _, engine::general_purpose::STANDARD};
                body.insert(
                    "promptImage".into(),
                    json!(format!("data:{mime};base64,{}", STANDARD.encode(&bytes))),
                );
                if !req.prompt.trim().is_empty() {
                    body.insert("promptText".into(), json!(req.prompt));
                }
                "image_to_video"
            }
            None => {
                body.insert("promptText".into(), json!(req.prompt));
                "text_to_video"
            }
        };

        let accepted = match capabilities(model).aspect {
            AspectSupport::Pixels(ratios) => ratios,
            // Unreachable: Runway's ratios are always pixel pairs.
            AspectSupport::Named(_) | AspectSupport::Free { .. } => GEN45_RATIOS,
        };
        body.insert("ratio".into(), json!(nearest_ratio(req.aspect, accepted)));

        if let Some(seconds) = req.duration {
            body.insert("duration".into(), json!(seconds));
        }
        if let Some(seed) = req.seed {
            body.insert("seed".into(), json!(seed));
        }

        Ok((endpoint, Value::Object(body)))
    }
}

/// Turns a requested ratio into one of the pixel pairs Runway accepts.
///
/// Necessary because Runway names geometry in pixels — `1280:720` — while every
/// other provider here, and everyone typing a command, says `16:9`. Refusing
/// `--aspect 16:9` on the grounds that the accepted value is spelled `1280:720`
/// would be technically true and useless, since they are the same shape.
///
/// Exact spellings win, so `--aspect 1280:720` passes through untouched. Anything
/// else picks the accepted pair whose proportions are closest, which for `16:9`
/// is exactly `1280:720`. With nothing asked for, the first accepted pair is the
/// provider's own default order.
fn nearest_ratio(requested: Option<Aspect>, accepted: &[&str]) -> String {
    let fallback = accepted.first().copied().unwrap_or("1280:720").to_string();
    let Some(aspect) = requested else {
        return fallback;
    };

    // Exact first: several offered pairs can share a shape (`1920:1080` and
    // `1280:720`), and choosing by shape alone would send the first of them
    // for either.
    if let Some(pair) = accepted.iter().find(|pair| Aspect::parse(pair).ok() == Some(aspect)) {
        return (*pair).to_string();
    }

    let wanted = f64::from(aspect.w) / f64::from(aspect.h);
    accepted
        .iter()
        .filter_map(|pair| {
            let parsed = Aspect::parse(pair).ok()?;
            let ratio = f64::from(parsed.w) / f64::from(parsed.h);
            Some((pair, (ratio - wanted).abs()))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(pair, _)| (*pair).to_string())
        .unwrap_or(fallback)
}

impl VideoProvider for Client {
    fn start(&self, req: &VideoRequest) -> Result<String> {
        let model = resolve_model(&req.model);
        let (endpoint, body) = self.body(req, &model)?;

        // Deliberately not retried (see `retry`): this is the call that starts
        // billing, and a retry of a request that in fact succeeded buys a second
        // render nobody asked for.
        let response = self
            .authed(self.http.post(format!("{}/{endpoint}", self.base)))
            .json(&body)
            .send()
            .context("starting the Runway render")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        let payload: Value = response.json().context("parsing the task response")?;
        payload["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Runway accepted the job but returned no task id: {payload}"))
    }

    fn poll(&self, operation: &str) -> Result<VideoStatus> {
        match self.task(operation)? {
            Task::Done(url) => Ok(VideoStatus::Done(self.download(&url, "video")?)),
            Task::Pending => Ok(VideoStatus::Pending),
        }
    }
}

impl ImageProvider for Client {
    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage> {
        let model = resolve_image_model(&req.model);

        // The front ends check this before a client exists; checked again here
        // so a direct caller cannot send what the check refuses.
        image_capabilities(&model).check(req)?;

        let body = self.image_body(req, &model)?;
        let verb = if req.references.is_empty() { "Rendering" } else { "Editing" };
        eprintln!("{verb} {} with {model}…", body["ratio"].as_str().unwrap_or("?"));

        // Deliberately not retried, as with video: this call starts billing.
        let response = self
            .authed(self.http.post(format!("{}/text_to_image", self.base)))
            .json(&body)
            .send()
            .context("starting the Runway render")?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }
        let payload: Value = response.json().context("parsing the task response")?;
        let id = payload["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Runway accepted the job but returned no task id: {payload}"))?
            .to_string();

        let url = self.await_task(&id)?;
        let bytes = self.download(&url, "image")?;
        let mime_type = crate::sniff_mime(&bytes).unwrap_or("image/png").to_string();

        Ok(GeneratedImage {
            bytes,
            mime_type,
            commentary: None,
            // Runway echoes no seed, so only a pinned one can be reported.
            seed: req.seed,
        })
    }

    fn list_models(&self) -> Result<Vec<String>> {
        // No endpoint lists them for free; the catalogue is fixed at release.
        // What `lucida models` is really asking is whether the key works, and
        // the balance answers that without spending.
        self.credits()?;
        Ok(IMAGE_MODELS.iter().map(|m| m.to_string()).collect())
    }
}

impl Client {
    fn image_body(&self, req: &ImageRequest, model: &str) -> Result<Value> {
        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(model));
        body.insert("promptText".into(), json!(req.prompt));
        body.insert("ratio".into(), json!(nearest_ratio(req.aspect, IMAGE_RATIOS)));

        if !req.references.is_empty() {
            let mut images = Vec::new();
            for reference in &req.references {
                // What the endpoint takes as a URI passes through: it fetches
                // `https://`, resolves `runway://` uploads and decodes data URIs.
                let uri = if ["https://", "runway://", "data:image/"]
                    .iter()
                    .any(|scheme| reference.starts_with(scheme))
                {
                    reference.clone()
                } else {
                    let bytes = std::fs::read(reference)
                        .with_context(|| format!("reading reference image {reference}"))?;
                    let mime = crate::sniff_mime(&bytes).unwrap_or("image/png");
                    use base64::{Engine as _, engine::general_purpose::STANDARD};
                    format!("data:{mime};base64,{}", STANDARD.encode(&bytes))
                };
                images.push(json!({ "uri": uri }));
            }
            body.insert("referenceImages".into(), Value::Array(images));
        }
        if let Some(seed) = req.seed {
            body.insert("seed".into(), json!(seed));
        }
        Ok(Value::Object(body))
    }

    /// Polls an image task until it resolves. The video lane polls from the
    /// caller instead, because a video outlives a session and an image does not.
    fn await_task(&self, id: &str) -> Result<String> {
        let started = Instant::now();
        let mut interval = Duration::from_millis(500);
        loop {
            // Between polls only: the render is billed by now, so a cancellation
            // stops the waiting, not the charge.
            crate::cancel::check()?;
            if started.elapsed() > IMAGE_DEADLINE {
                bail!(
                    "gave up after {} minutes. The render may still complete; its \
                     Runway task id is {id}.",
                    IMAGE_DEADLINE.as_secs() / 60
                );
            }
            if let Task::Done(url) = self.task(id)? {
                return Ok(url);
            }
            std::thread::sleep(interval);
            interval = (interval * 2).min(Duration::from_secs(3));
        }
    }

    /// One read of a task, for either lane.
    fn task(&self, id: &str) -> Result<Task> {
        let response = crate::retry::send_idempotent("polling the render", || {
            self.authed(self.http.get(format!("{}/tasks/{id}", self.base)))
        })
        .context("polling the Runway task")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        let payload: Value = response.json().context("parsing the task response")?;
        match payload["status"].as_str().unwrap_or_default() {
            "SUCCEEDED" => payload["output"][0]
                .as_str()
                .map(|url| Task::Done(url.to_string()))
                .ok_or_else(|| anyhow!("the render finished but carries no output: {payload}")),
            "FAILED" | "CANCELLED" => {
                let reason = payload["failure"]
                    .as_str()
                    .or_else(|| payload["failureCode"].as_str())
                    .unwrap_or("no reason given");
                // Runway's own terminal states, not an inference from silence.
                Err(terminal(format!("the render failed: {reason}")))
            }
            // PENDING, RUNNING, THROTTLED — all still in flight.
            _ => Ok(Task::Pending),
        }
    }
}

impl Client {
    fn download(&self, url: &str, what: &str) -> Result<Vec<u8>> {
        // No credential on this request: the output URL is pre-signed object
        // storage, and sending a key to a host that does not need it is how
        // credentials end up somewhere unexpected. Same reasoning as BFL, and
        // the opposite of Veo, whose download URL does require one — which is
        // exactly why both are pinned by tests.
        let response = crate::retry::send_idempotent(&format!("downloading the {what}"), || {
            self.http.get(url)
        })
        .with_context(|| {
            format!(
                "downloading the finished {what}. The render was billed; its URL \
                 expires, so fetch it by hand while it lasts:\n\n  {url}"
            )
        })?;

        if !response.status().is_success() {
            bail!(
                "the output URL returned HTTP {}. These URLs are signed and \
                 expire.\n\n  {url}",
                response.status().as_u16()
            );
        }

        Ok(response.bytes().with_context(|| format!("reading {what} bytes"))?.to_vec())
    }
}

/// Turns Runway's own error shape into something that names the fix.
///
/// Its validation errors are structured — an `issues` array where each entry
/// carries the offending `path` and, for a bad enum, the `values` that would
/// have worked. That is far more useful than the message alone, and it is the
/// shape the free probes read, so it is worth unpacking rather than printing raw.
fn explain_error(status: u16, body: &str) -> String {
    let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = payload["error"].as_str().unwrap_or(body);

    let mut out = match status {
        401 | 403 => format!(
            "HTTP {status} — Runway rejected the key. {message}\n\n\
             Check RUNWAY_API_KEY, and that the key has not been rotated in \
             the Developer Portal."
        ),
        429 => format!(
            "HTTP {status} — {message}\n\nRunway caps concurrent and daily \
             generations per tier; this is a rate limit rather than a bad request."
        ),
        _ => format!("HTTP {status} — {message}"),
    };

    if let Some(issues) = payload["issues"].as_array() {
        for issue in issues {
            let path = issue["path"]
                .as_array()
                .map(|p| {
                    p.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .unwrap_or_default();
            if path.is_empty() {
                continue;
            }
            match issue["values"].as_array() {
                Some(values) => {
                    let accepted: Vec<&str> = values.iter().filter_map(|v| v.as_str()).collect();
                    out.push_str(&format!("\n  `{path}` accepts: {}", accepted.join(", ")));
                }
                None => {
                    if let Some(detail) = issue["message"].as_str() {
                        out.push_str(&format!("\n  `{path}`: {detail}"));
                    }
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, serve};

    fn wired(server: &crate::testserver::Server) -> Client {
        Client::recorded(server.url())
    }

    // ---- images -------------------------------------------------------------

    /// Runway spells geometry in pixels, so `--aspect 16:9` has to be accepted
    /// wherever an offered pair *is* 16:9 — the module note promised it, while
    /// the capability check compared strings and refused it (found 2026-09-27,
    /// on a release). A shape nothing offers is still refused, not approximated.
    #[test]
    fn a_ratio_is_accepted_when_an_offered_pixel_pair_has_its_shape() {
        let aspect = |text| ImageRequest { aspect: Aspect::parse(text).ok(), ..Default::default() };
        let caps = image_capabilities(DEFAULT_IMAGE_MODEL);
        for ok in ["1:1", "16:9", "9:16", "4:3", "1024:1024", "1920:1080"] {
            assert!(caps.check(&aspect(ok)).is_ok(), "{ok}");
        }
        assert!(caps.check(&aspect("3:2")).is_err(), "no pair is 3:2");

        let video = |text| VideoRequest { aspect: Aspect::parse(text).ok(), ..Default::default() };
        let gen45 = capabilities("gen4.5");
        assert!(gen45.check(&video("16:9")).is_ok());
        assert!(gen45.check(&video("9:16")).is_ok());
        assert!(gen45.check(&video("1:1")).is_err(), "gen4.5 offers no square");
    }

    /// Every advertised pair is sent as itself. Runway lists several pairs per
    /// shape — `1920:1080` and `1280:720` — and picking by shape alone sent the
    /// first, so six of the sixteen could never be asked for and a 720-class
    /// request rendered, and billed, at 1080 (found in review, 2026-09-27).
    #[test]
    fn every_offered_pair_is_sent_as_itself() {
        for list in [IMAGE_RATIOS, TURBO_RATIOS, GEN45_RATIOS] {
            for pair in list {
                assert_eq!(nearest_ratio(Aspect::parse(pair).ok(), list), *pair);
            }
        }
    }

    /// Refused before a client exists, so with no key the answer is the real
    /// objection (exit 2), and `--dry-run` reports what a real run would.
    #[test]
    fn turbo_without_a_reference_is_refused_by_the_capability_check() {
        let caps = image_capabilities("gen4_image_turbo");
        let bare = ImageRequest { model: "gen4_image_turbo".into(), ..Default::default() };
        let error = caps.check(&bare).unwrap_err();
        assert!(error.downcast_ref::<crate::out::Refused>().is_some());
        assert!(format!("{error:#}").contains("gen4_image"), "{error:#}");

        let edit = ImageRequest { references: vec!["a.png".into()], ..bare };
        assert!(caps.check(&edit).is_ok());
    }

    /// Only Runway's own models, even with `--provider runway` explicit: the
    /// endpoints list what Runway fronts for other companies, and sending one
    /// would bill it here and record it as Runway's own output.
    #[test]
    fn a_fronted_model_is_refused_even_when_runway_is_named() {
        let image = ImageRequest { model: "gpt_image_2".into(), ..Default::default() };
        let error = image_capabilities("gpt_image_2").check(&image).unwrap_err();
        assert!(error.downcast_ref::<crate::out::Refused>().is_some());
        assert!(format!("{error:#}").contains("gen4_image"), "{error:#}");
        for own in ["gen4_image", "gen4-image-turbo", ""] {
            let req = ImageRequest { model: own.into(), references: vec!["a.png".into()], ..Default::default() };
            assert!(image_capabilities(own).check(&req).is_ok(), "{own}");
        }

        let video = VideoRequest { model: "veo3.1".into(), ..Default::default() };
        let error = capabilities("veo3.1").check(&video).unwrap_err();
        assert!(format!("{error:#}").contains("gen4.5"), "{error:#}");
        for own in ["gen4.5", "gen4-turbo", ""] {
            let req = VideoRequest { model: own.into(), image: Some("a.png".into()), ..Default::default() };
            assert!(capabilities(own).check(&req).is_ok(), "{own}");
        }
    }

    /// A reference the endpoint can fetch or decode itself is passed through
    /// rather than read as a local path.
    #[test]
    fn a_reference_uri_the_endpoint_accepts_is_passed_through() {
        let server = serve(vec![
            Reply::json(r#"{"id":"img-3"}"#),
            Reply::json(r#"{"status":"SUCCEEDED","output":["{{server}}/signed/out.png"]}"#),
            Reply::bytes("image/png", &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        ]);
        let refs = [
            "https://example.com/ref.png",
            "runway://upload/0000-1111",
            "data:image/png;base64,iVBORw0KGgo=",
        ];
        let request = ImageRequest {
            prompt: "x".into(),
            model: "gen4_image".into(),
            references: refs.iter().map(|r| r.to_string()).collect(),
            ..Default::default()
        };
        wired(&server).generate(&request).unwrap();
        let body = server.finish()[0].json();
        for (i, r) in refs.iter().enumerate() {
            assert_eq!(body["referenceImages"][i]["uri"], *r);
        }
    }

    /// `--size` has no field here, but the pixel count *can* be chosen — by
    /// spelling the pair. The generic remedy sent people to other providers.
    #[test]
    fn a_size_request_is_told_to_spell_the_pixel_pair() {
        let req = ImageRequest { size: Some(crate::provider::Size(1920)), ..Default::default() };
        let error = format!("{:#}", image_capabilities(DEFAULT_IMAGE_MODEL).check(&req).unwrap_err());
        assert!(error.contains("--aspect 1024:1024"), "{error}");
        assert!(error.contains("1920:1080"), "{error}");
    }

    /// Runway's own two image models route here, by id or alias. Its video ids
    /// do not, and neither does the catalogue it fronts — the same line the video
    /// lane draws, owner's call 2026-08-09.
    #[test]
    fn runways_own_image_models_route_to_runway() {
        use crate::provider::{Backend, infer_backend};
        for model in IMAGE_MODELS {
            assert_eq!(infer_backend(model), Backend::Runway, "{model}");
        }
        assert_eq!(infer_backend("gen4-image-turbo"), Backend::Runway);
        assert_ne!(infer_backend("gen4.5"), Backend::Runway, "a video model");
        assert_ne!(infer_backend("seedream5_pro"), Backend::Runway, "a fronted model");
    }

    /// Each value here was read from a rejection naming the field, 2026-09-27 —
    /// never from the absence of one, since this API ignores unknown fields.
    #[test]
    fn image_capabilities_are_what_the_endpoint_validates() {
        use crate::provider::MaskSupport;
        for model in IMAGE_MODELS {
            let caps = image_capabilities(model);
            assert!(caps.references, "{model}: up to three reference images");
            assert!(caps.seed, "{model}: 0..=4294967295");
            // The ratio is a pixel pair and decides the size; there is no other.
            assert!(!caps.size, "{model}");
            assert!(!caps.negative_prompt, "{model}");
            assert!(!caps.steps && !caps.guidance && !caps.workflow, "{model}");
            assert_eq!(caps.mask, MaskSupport::No, "{model}");
            // Measured 2026-09-27 on a real `gen4_image` render: a signed C2PA
            // manifest and no pixel watermark found — the evidence BFL's was read on.
            assert_eq!(caps.provenance, Provenance::C2paOnly, "{model}");
            match caps.aspect {
                AspectSupport::Pixels(ratios) => assert_eq!(ratios.len(), 16, "{model}"),
                _ => panic!("{model}: Runway names its ratios as pixel pairs"),
            }
        }
    }

    /// Submit, poll, download — and the download carries no key, as with video.
    #[test]
    fn an_image_is_submitted_polled_and_downloaded() {
        let server = serve(vec![
            Reply::json(r#"{"id":"img-1"}"#),
            Reply::json(r#"{"status":"RUNNING"}"#),
            Reply::json(r#"{"status":"SUCCEEDED","output":["{{server}}/signed/out.png"]}"#),
            Reply::bytes("image/png", &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        ]);

        let request = ImageRequest {
            prompt: "a lighthouse at dusk".into(),
            model: "gen4_image".into(),
            aspect: Aspect::parse("16:9").ok(),
            seed: Some(7),
            ..Default::default()
        };
        let image = wired(&server).generate(&request).unwrap();
        assert_eq!(image.bytes[..4], [0x89, b'P', b'N', b'G']);
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.seed, Some(7));

        let requests = server.finish();
        assert_eq!(requests[0].path, "/text_to_image");
        assert_eq!(requests[0].header("x-runway-version"), Some(API_VERSION));
        let body = requests[0].json();
        assert_eq!(body["model"], "gen4_image");
        assert_eq!(body["promptText"], "a lighthouse at dusk");
        // 16:9 is two of the accepted pairs; the larger comes first in Runway's list.
        assert_eq!(body["ratio"], "1920:1080");
        assert_eq!(body["seed"], 7);
        assert!(body.get("referenceImages").is_none());

        assert_eq!(requests[1].path, "/tasks/img-1");
        assert_eq!(requests[2].path, "/tasks/img-1");
        assert_eq!(requests[3].path, "/signed/out.png");
        assert_eq!(requests[3].header("authorization"), None, "the key went to object storage");
    }

    /// A local reference file travels as a data URI — the endpoint takes
    /// `https://`, `runway://` uploads or `data:image/`, and the last needs no
    /// second round trip.
    #[test]
    fn a_reference_image_travels_as_a_data_uri() {
        let dir = std::env::temp_dir().join(format!("lucida-runway-ref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("ref.png");
        std::fs::write(&source, [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();

        let server = serve(vec![
            Reply::json(r#"{"id":"img-2"}"#),
            Reply::json(r#"{"status":"SUCCEEDED","output":["{{server}}/signed/out.png"]}"#),
            Reply::bytes("image/png", &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        ]);
        let request = ImageRequest {
            prompt: "the same lighthouse, in snow".into(),
            model: "gen4_image_turbo".into(),
            references: vec![source.to_string_lossy().into_owned()],
            ..Default::default()
        };
        wired(&server).generate(&request).unwrap();

        let body = server.finish()[0].json();
        let uri = body["referenceImages"][0]["uri"].as_str().unwrap();
        assert!(uri.starts_with("data:image/png;base64,"), "{uri}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `gen4_image_turbo` cannot start from text — the endpoint requires
    /// `referenceImages` for it. Said before anything is sent, as a refusal.
    #[test]
    fn turbo_without_a_reference_is_refused_before_anything_is_sent() {
        let server = serve(vec![]);
        let request = ImageRequest {
            prompt: "a lighthouse".into(),
            model: "gen4_image_turbo".into(),
            ..Default::default()
        };
        let error = wired(&server).generate(&request).unwrap_err();
        assert!(error.downcast_ref::<crate::out::Refused>().is_some(), "{error:#}");
        assert!(format!("{error:#}").contains("gen4_image"), "{error:#}");
        assert!(server.finish().is_empty(), "a refused request reached the API");
    }

    /// The catalogue Runway fronts is deliberately unreachable. Owner's call,
    /// 2026-08-09: its own models are a substitution, the rest would make Lucida
    /// an aggregator front-end — and `veo3.1` here would be a second, worse path
    /// to a lane already reached directly on the user's own Google key.
    #[test]
    fn only_runways_own_models_are_claimed() {
        for model in MODELS {
            assert!(is_runway_model(model), "{model} is ours and must be claimed");
        }
        for aggregated in [
            "kling3.0_pro",
            "veo3.1",
            "veo3.1_fast",
            "seedance2",
            "hailuo3",
            "grok_imagine_1_5",
            "gemini_omni_flash",
        ] {
            assert!(
                !is_runway_model(aggregated),
                "`{aggregated}` is another vendor's model behind Runway's endpoint \
                 and must not be claimed here"
            );
        }
    }

    /// Runway names geometry in pixels and everyone else says `16:9`. Refusing
    /// the latter because the accepted spelling is `1280:720` would be
    /// technically true and useless — they are the same shape.
    #[test]
    fn a_simplified_ratio_becomes_the_pixel_pair_that_is_that_ratio() {
        assert_eq!(nearest_ratio(Aspect::parse("16:9").ok(), TURBO_RATIOS), "1280:720");
        assert_eq!(nearest_ratio(Aspect::parse("9:16").ok(), TURBO_RATIOS), "720:1280");
        assert_eq!(nearest_ratio(Aspect::parse("1:1").ok(), TURBO_RATIOS), "960:960");

        // An exact spelling passes through untouched.
        assert_eq!(nearest_ratio(Aspect::parse("1584:672").ok(), TURBO_RATIOS), "1584:672");

        // Nothing asked for takes the provider's own first option.
        assert_eq!(nearest_ratio(None, TURBO_RATIOS), "1280:720");

        // gen4.5 offers only two, so a square request lands on the nearer of
        // them rather than on a pair it does not accept.
        let square = nearest_ratio(Aspect::parse("1:1").ok(), GEN45_RATIOS);
        assert!(GEN45_RATIOS.contains(&square.as_str()), "{square}");
    }

    /// Capabilities vary per model here as they do on BFL: `gen4_turbo` animates
    /// a still and cannot start from a prompt, which is measured from
    /// /v1/text_to_video's own accepted list.
    #[test]
    fn only_gen45_renders_from_text_alone() {
        assert!(!capabilities("gen4_turbo").text_to_video);
        assert!(!capabilities("gen4").text_to_video);
        assert!(capabilities("gen4.5").text_to_video);

        for model in MODELS {
            assert!(capabilities(model).image_to_video, "{model} must animate a still");
        }
    }

    /// Both headers on every request, and the version one is not optional:
    /// without it the API answers 400 rather than assuming a default. Only the
    /// wire can prove it was actually sent.
    #[test]
    fn every_request_carries_the_mandatory_version_header() {
        let server = serve(vec![Reply::json(
            r#"{"id":"4f1a2b3c-0000-4000-8000-000000000000"}"#,
        )]);

        let request = VideoRequest {
            prompt: "a fox running".into(),
            model: "gen4.5".into(),
            aspect: Aspect::parse("16:9").ok(),
            duration: Some(5),
            ..Default::default()
        };
        let id = wired(&server).start(&request).unwrap();
        assert_eq!(id, "4f1a2b3c-0000-4000-8000-000000000000");

        let requests = server.finish();
        assert_eq!(requests[0].path, "/text_to_video");
        assert_eq!(requests[0].header("x-runway-version"), Some(API_VERSION));
        assert_eq!(requests[0].header("authorization"), Some("Bearer test-key"));

        let body = requests[0].json();
        assert_eq!(body["model"], "gen4.5");
        assert_eq!(body["promptText"], "a fox running");
        // Sent as the pixel pair, not as what the caller typed.
        assert_eq!(body["ratio"], "1280:720");
        assert_eq!(body["duration"], 5);
    }

    /// A still goes to a different endpoint entirely, as a data URI rather than
    /// a separate upload.
    #[test]
    fn animating_a_still_posts_to_image_to_video() {
        let dir = std::env::temp_dir().join(format!("lucida-runway-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("frame.png");
        std::fs::write(&source, [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();

        let server = serve(vec![Reply::json(
            r#"{"id":"4f1a2b3c-0000-4000-8000-000000000001"}"#,
        )]);

        let request = VideoRequest {
            prompt: "pan slowly right".into(),
            model: "gen4_turbo".into(),
            image: Some(source.to_string_lossy().into_owned()),
            ..Default::default()
        };
        wired(&server).start(&request).unwrap();

        let requests = server.finish();
        assert_eq!(requests[0].path, "/image_to_video");
        let body = requests[0].json();
        assert!(
            body["promptImage"].as_str().unwrap().starts_with("data:image/png;base64,"),
            "the still must travel as a data URI"
        );
        assert_eq!(body["promptText"], "pan slowly right");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The output URL must NOT carry the credential — it is pre-signed object
    /// storage. The opposite of Veo, whose download does require one, which is
    /// why both are pinned rather than assumed.
    #[test]
    fn the_download_does_not_leak_the_key_to_object_storage() {
        let server = serve(vec![
            Reply::json(
                r#"{"status":"SUCCEEDED","output":["{{server}}/signed/out.mp4"]}"#,
            ),
            Reply::bytes("video/mp4", b"mp4-bytes"),
        ]);

        let status = wired(&server).poll("4f1a2b3c-0000-4000-8000-000000000000").unwrap();
        match status {
            VideoStatus::Done(bytes) => assert_eq!(bytes, b"mp4-bytes"),
            VideoStatus::Pending => panic!("expected the render to be done"),
        }

        let requests = server.finish();
        assert_eq!(requests[0].path, "/tasks/4f1a2b3c-0000-4000-8000-000000000000");
        assert_eq!(requests[1].path, "/signed/out.mp4");
        assert_eq!(
            requests[1].header("authorization"),
            None,
            "the key was sent to object storage"
        );
    }

    /// Everything short of SUCCEEDED or FAILED is still working. Treating an
    /// unrecognised status as finished would abandon a render mid-flight.
    #[test]
    fn every_in_flight_status_reads_as_pending() {
        for state in ["PENDING", "RUNNING", "THROTTLED", "SOMETHING_NEW"] {
            let server = serve(vec![Reply::json(&format!(r#"{{"status":"{state}"}}"#))]);
            let status = wired(&server).poll("4f1a2b3c-0000-4000-8000-000000000000").unwrap();
            assert!(
                matches!(status, VideoStatus::Pending),
                "`{state}` was not treated as in flight"
            );
            server.finish();
        }
    }

    /// Runway's rejections carry the accepted values, which is what makes the
    /// free probes worth running — and what an agent needs in order to retry
    /// correctly rather than guess.
    #[test]
    fn a_rejection_reports_the_values_that_would_have_worked() {
        let body = r#"{"error":"Validation of body failed","issues":[
            {"code":"invalid_value","values":["gen4_turbo","gen4","gen4.5"],"path":["model"]},
            {"code":"too_big","message":"Too big: expected number to be <=10","path":["duration"]}
        ]}"#;

        let explained = explain_error(400, body);
        assert!(explained.contains("`model` accepts: gen4_turbo, gen4, gen4.5"), "{explained}");
        assert!(explained.contains("`duration`"), "{explained}");
        assert!(explained.contains("<=10"), "{explained}");
    }

    /// A rejected key must not read as a bad request, since the fix is entirely
    /// different and the message is the only thing pointing at it.
    #[test]
    fn a_rejected_key_says_so() {
        let explained = explain_error(401, r#"{"error":"Unauthorized"}"#);
        assert!(explained.contains("RUNWAY_API_KEY"), "{explained}");
    }

    /// The ledger retires an operation only on a failure the provider reported
    /// as final, and recognises it by this type. FAILED and CANCELLED are the
    /// provider saying so; a rejected poll says nothing about the render.
    #[test]
    fn a_failed_task_is_a_terminal_failure_and_a_rejected_poll_is_not() {
        let id = "4f1a2b3c-0000-4000-8000-000000000000";
        for state in ["FAILED", "CANCELLED"] {
            let server = serve(vec![Reply::json(&format!(
                r#"{{"status":"{state}","failure":"content moderation"}}"#
            ))]);
            let error = wired(&server).poll(id).err().expect("a failed task must be an error");
            let failure = error
                .downcast_ref::<crate::video::TerminalFailure>()
                .unwrap_or_else(|| panic!("`{state}` was not marked terminal: {error:#}"));
            assert!(failure.0.contains("content moderation"), "{}", failure.0);
            server.finish();
        }

        let server = serve(vec![Reply::status(401, r#"{"error":"Unauthorized"}"#)]);
        let error = wired(&server).poll(id).err().expect("a rejected poll must be an error");
        assert!(
            error.downcast_ref::<crate::video::TerminalFailure>().is_none(),
            "a rejected poll retired the operation: {error:#}"
        );
        server.finish();
    }
}
