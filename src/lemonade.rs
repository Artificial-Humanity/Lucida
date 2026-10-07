//! Lemonade — the image models a Lemonade server already serves.
//!
//! The second free lane, and the first that renders in one plain HTTP call:
//! `POST /images/generations` (JSON) or `/images/edits` (multipart), answered
//! with the image itself. No graph to build and no queue to poll.
//!
//! Three things set it apart, and each has its home here:
//!
//! - **It is reached only by name.** Its ids collide with other lanes'
//!   inference: `infer_backend` lowercases, so `Flux-2-Klein-4B` would reach
//!   BFL and be billed there.
//! - **Which models exist is the server's answer, not Lucida's.** The fixed
//!   shape in [`CAPABILITIES`] answers `tools/list` and `--dry-run` with no
//!   network. Whether a model is listed, whether it edits and what size it
//!   renders by default are asked of the server inside the render, after one
//!   `GET /models` — as ComfyUI's graph is checked against `/object_info`.
//! - **It never says which seed it used.** So Lucida always chooses one, sends
//!   it and reports it.

use crate::provider::{
    AspectSupport, Capabilities, GeneratedImage, ImageProvider, ImageRequest, MaskSupport,
    Provenance,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::{RequestBuilder, Response};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::Duration;

/// Where Lemonade is reached when `LUCIDA_LEMONADE_URL` is unset: the `/v1`
/// base on this machine, as ComfyUI's default is its local port.
pub const DEFAULT_URL: &str = "http://127.0.0.1:13305/v1";

/// Stands in for "no model named" in capability lookups, and only there.
///
/// `Backend::default_model` must name something distinct for every provider,
/// but this lane has no built-in model: its models are whatever the server
/// holds. So this is never sent and never printed — `provider::model_for`
/// refuses a render that names no model before it could be, and
/// `Backend::default_model_description` says where a model comes from instead.
pub const PLACEHOLDER_MODEL: &str = "lemonade-model";

/// Sizes are rounded to this before they are sent. Lemonade renders on a
/// 16-pixel grid and rounds an off-grid size *up* without saying so — measured:
/// `1000x744` came back 1008x752 — so Lucida rounds first, and the size it
/// sends and reports is the size that comes back.
const PIXEL_GRID: u32 = 16;

/// The longest edge asked of this lane, until a larger one has been measured
/// on it. The GPU behind a Lemonade server is often shared.
const MAX_LONG_EDGE: u32 = 2048;

/// The long edge when a shape is asked for without a size.
pub const ASPECT_LONG_EDGE: u32 = 1024;

/// Used when the server lists no default size for a model. Lemonade's own
/// default for an omitted size is 512x512, so a size is always sent.
const FALLBACK_DIMENSIONS: (u32, u32) = (1024, 1024);

/// References an edit may carry. Measured: a second `image` part was dropped
/// without a word and the edit reported as a success, built from the first.
const MAX_REFERENCES: usize = 1;

/// The formats `/images/edits` decodes, as `sniff_mime` names them. Both were
/// read correctly in a real edit; WebP has not been tried, so it is refused.
const REFERENCE_FORMATS: &[&str] = &["image/png", "image/jpeg"];

/// The multipart field each reference travels in.
const IMAGE_PART: &str = "image";

/// Seeds the server takes are below this. A larger one is wrapped, not refused
/// — measured: 2^32 + 5 rendered as seed 5 — so Lucida never sends one.
const SEED_LIMIT: u64 = 1 << 32;

/// The listing's timeout, and every request's but the render's.
const LISTING_TIMEOUT: Duration = Duration::from_secs(30);

/// The render's own timeout. A cold render, model load included, measured
/// three minutes for a 4B model at 1024x1024; a larger model loads slower.
const RENDER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// What a Lemonade gateway puts in `error.code` when the server behind it is down.
const UPSTREAM_UNREACHABLE: &str = "upstream_unreachable";

/// One Lemonade render at a time in this process.
///
/// The MCP server runs four calls at once, and a server keeps few image models
/// loaded, so concurrent renders on different models would swap them in and out
/// and queue on the server anyway. Waiting here is cancellable; waiting there
/// is not.
static RENDERING: Mutex<()> = Mutex::new(());

pub const CAPABILITIES: Capabilities = Capabilities {
    provider: "lemonade",
    tagline: "A Lemonade server's image models, on this machine or another. Free, one \
              HTTP call per render, and the seed of every render is reported. Reached \
              by name alone — `lemonade` plus a model id from `lucida models --provider \
              lemonade` — and it takes no negative prompt.",
    aspect: AspectSupport::Free {
        multiple_of: PIXEL_GRID,
    },
    size: true,
    seed: true,
    // The API has no field for one.
    negative_prompt: false,
    references: true,
    max_references: Some(MAX_REFERENCES),
    // Lemonade's convention is white = change; Lucida's is transparent = change.
    // Converting needs a PNG decoder, so a mask is refused, pointing at comfyui.
    mask: MaskSupport::No,
    workflow: false,
    steps: true,
    guidance: true,
    // Read chunk by chunk in real renders: no C2PA `caBX`, no `eXIf`. The PNG
    // does carry `tEXt` chunks (`generation_data`, `parameters`) holding the
    // prompt and the seed — text anyone can strip or write, not a mark.
    provenance: Provenance::Unmarked,
    needs_reference: false,
    foreign_model: None,
    reference_formats: Some(REFERENCE_FORMATS),
    max_long_edge: Some(MAX_LONG_EDGE),
    seed_limit: Some(SEED_LIMIT),
};

/// Lemonade could not be reached: a connection that failed, or a 502 from the
/// gateway or proxy in front of it.
///
/// A type rather than a sentence, so the callers that know *why* this lane was
/// chosen can say so, and so `lucida models` can treat it as the ordinary state
/// of a server that is off.
#[derive(Debug)]
pub struct Unreachable(String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

/// Adds which setting chose this lane to a "not reachable" error, when a
/// preference list did.
///
/// The lane needs no credential, so it is always "usable" to
/// `LUCIDA_IMAGE_PROVIDERS` — on a machine with no Lemonade, a list that starts
/// with it fails every render that names no provider. The bare error would send
/// someone to fix a server they never meant to use.
pub fn explain_unreachable(
    error: anyhow::Error,
    source: &Option<crate::provider::DefaultSource>,
) -> anyhow::Error {
    let from_preference = matches!(source, Some(crate::provider::DefaultSource::Preference { .. }));
    if from_preference && error.downcast_ref::<Unreachable>().is_some() {
        let setting = <crate::provider::Backend as crate::provider::Preferred>::SETTING;
        return error.context(format!(
            "lemonade was chosen by {setting}, which counts it as usable because it \
             needs no credential. A preference is not a fallback, so no other provider \
             was tried: on a machine without a Lemonade server, take lemonade out of \
             {setting}, or name a provider"
        ));
    }
    error
}

/// One entry of `GET /models`, as far as this lane reads it.
#[derive(Debug)]
struct Listed {
    id: String,
    labels: Vec<String>,
    /// `recipe_options` width x height, when both are there.
    natural: Option<(u32, u32)>,
    steps: Option<u64>,
    cfg_scale: Option<f64>,
}

impl Listed {
    /// `recipe` is not a discriminator — image and chat models share recipes —
    /// so the label decides.
    fn is_image(&self) -> bool {
        self.labels.iter().any(|label| label == "image")
    }

    fn is_edit(&self) -> bool {
        self.labels.iter().any(|label| label == "edit")
    }

    /// One line for `lucida models` and `image_providers`.
    fn describe(&self, default: Option<&str>) -> String {
        let mut notes = vec![self.labels.join(", ")];
        if let Some((width, height)) = self.natural {
            notes.push(format!("{width}x{height}"));
        }
        if let Some(steps) = self.steps {
            notes.push(format!("{steps} steps"));
        }
        if let Some(cfg) = self.cfg_scale {
            notes.push(format!("cfg {cfg}"));
        }
        if default == Some(self.id.as_str()) {
            notes.push("default, from LUCIDA_LEMONADE_MODEL".to_string());
        }
        format!("{}  ({})", self.id, notes.join("; "))
    }
}

fn parse_models(payload: &Value) -> Result<Vec<Listed>> {
    let data = payload["data"]
        .as_array()
        .ok_or_else(|| anyhow!("Lemonade's model list has no `data` array: {}", preview(&payload.to_string())))?;
    Ok(data
        .iter()
        .filter_map(|entry| {
            let id = entry["id"].as_str()?.to_string();
            let labels: Vec<String> = entry["labels"]
                .as_array()
                .map(|labels| labels.iter().filter_map(|l| l.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let options = &entry["recipe_options"];
            let edge = |key: &str| options[key].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
            let natural = match (edge("width"), edge("height")) {
                (Some(width), Some(height)) => Some((width, height)),
                _ => None,
            };
            Some(Listed {
                id,
                labels,
                natural,
                steps: options["steps"].as_u64(),
                cfg_scale: options["cfg_scale"].as_f64(),
            })
        })
        .collect())
}

/// Lemonade's error document, in either of the shapes it uses:
/// `{"error":{"message","type","code"}}` or `{"error":"<string>"}`.
struct ErrorBody {
    message: Option<String>,
    upstream_unreachable: bool,
}

impl ErrorBody {
    fn parse(body: &str) -> Self {
        let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let error = &parsed["error"];
        if let Some(text) = error.as_str() {
            return Self {
                message: Some(text.to_string()),
                upstream_unreachable: false,
            };
        }
        Self {
            message: error["message"].as_str().map(str::to_string),
            // The identifier, never the sentence. The gateway puts it in `code`
            // and its `type` is the generic `invalid_request_error`; `type` is
            // read too, in case a later gateway moves it.
            upstream_unreachable: error["code"] == UPSTREAM_UNREACHABLE
                || error["type"] == UPSTREAM_UNREACHABLE,
        }
    }
}

pub struct Client {
    /// The `/v1` base, always credential-free: a `user:pass@` in the setting is
    /// moved into `auth` at construction, because this string is printed.
    base: String,
    auth: Option<String>,
    http: reqwest::blocking::Client,
    render_timeout: Duration,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        // Read through `config`, so a GUI-launched MCP server finds it too. Under
        // `cfg(test)` there is no default: on the machine this is built on a real
        // Lemonade answers there, and a unit test falling back to it would render
        // on a GPU other work shares.
        let configured = match crate::config::var("LUCIDA_LEMONADE_URL") {
            Some(url) => url,
            None if cfg!(test) => bail!(
                "no LUCIDA_LEMONADE_URL injected, and a unit test does not fall back to \
                 {DEFAULT_URL} — on a developer's machine a real Lemonade may be there"
            ),
            None => DEFAULT_URL.to_string(),
        };
        let (base, url_credentials) =
            crate::comfy::split_credentials(configured.trim().trim_end_matches('/'));

        // An explicit key beats credentials embedded in the URL, as ComfyUI's does.
        let auth = match crate::config::var("LEMONADE_API_KEY") {
            Some(key) => Some(format!("Bearer {}", key.trim())),
            None => url_credentials,
        };

        // The client-wide timeout is the listing's. The render sets its own, so a
        // hung server cannot hold `lucida models` for fifteen minutes a try.
        let http = reqwest::blocking::Client::builder()
            .timeout(LISTING_TIMEOUT)
            .connect_timeout(crate::retry::CONNECT_TIMEOUT)
            .build()
            .context("building HTTP client")?;

        Ok(Self {
            base,
            auth,
            http,
            render_timeout: RENDER_TIMEOUT,
        })
    }

    fn authed(&self, builder: RequestBuilder) -> RequestBuilder {
        match &self.auth {
            Some(value) => builder.header(reqwest::header::AUTHORIZATION, value),
            None => builder,
        }
    }

    /// `GET /models`: idempotent and free, so retried like every listing.
    fn models(&self) -> Result<Vec<Listed>> {
        let path = "/models";
        let response = crate::retry::send_idempotent("listing Lemonade's models", || {
            self.authed(self.http.get(format!("{}{path}", self.base)))
        })
        .map_err(|e| self.transport(path, &e))?;

        let status = response.status();
        let text = response
            .text()
            .with_context(|| format!("reading Lemonade's answer to {path}"))?;
        if !status.is_success() {
            return Err(self.answer_error(path, status.as_u16(), &text));
        }
        let payload: Value = serde_json::from_str(&text)
            .with_context(|| format!("Lemonade's answer to {path} is not JSON: {}", preview(&text)))?;
        parse_models(&payload)
    }

    // scripts/canary.sh skips Lemonade on this message's opening words; reword both together.
    fn unreachable(&self, why: &str) -> anyhow::Error {
        anyhow::Error::new(Unreachable(format!(
            "Lemonade is not reachable at {} ({why}).\n\n\
             Check that the server is running and that LUCIDA_LEMONADE_URL points at \
             its `/v1` base — it defaults to {DEFAULT_URL}.",
            self.base
        )))
    }

    /// A request that never got an answer.
    fn transport(&self, path: &str, error: &reqwest::Error) -> anyhow::Error {
        if crate::comfy::is_certificate_failure(error) {
            return anyhow!(
                "TLS verification failed for {} (requesting {path}).\n\n\
                 The server answered — this is a certificate problem, not an \
                 unreachable host. Lucida trusts the bundled Mozilla roots only.\n\n\
                 Underlying error: {error}",
                self.base
            );
        }
        if error.is_connect() {
            return self.unreachable(&format!("requesting {path}: {error}"));
        }
        if error.is_timeout() {
            return anyhow!(
                "Lemonade at {} accepted the connection but did not answer {path} in \
                 time. Check the server's own logs.",
                self.base
            );
        }
        anyhow!("could not talk to Lemonade at {} (requesting {path}): {error}", self.base)
    }

    /// The render's own transport failure: a timeout there is not a dead server.
    fn render_transport(&self, path: &str, model: &str, error: &reqwest::Error) -> anyhow::Error {
        if error.is_timeout() {
            return anyhow!(
                "Lemonade did not answer {path} within {}. It may still be rendering \
                 `{model}`: the request was not repeated, because a second one would \
                 render again. Check the server before asking again.",
                describe_wait(self.render_timeout)
            );
        }
        self.transport(path, error)
    }

    /// An answer that was not a success.
    fn answer_error(&self, path: &str, status: u16, body: &str) -> anyhow::Error {
        let error = ErrorBody::parse(body);
        if status == 502 && error.upstream_unreachable {
            return self.unreachable(
                "the gateway in front of it answered HTTP 502: it is up, and Lemonade \
                 behind it is not",
            );
        }
        if status == 502 && error.message.is_none() {
            return self.unreachable("HTTP 502 from a proxy in front of it, with no Lemonade error");
        }
        if status == 404 && path == "/models" {
            return anyhow!(
                "{} answered HTTP 404 for `{path}`, so it is not a Lemonade `/v1` base. \
                 LUCIDA_LEMONADE_URL must be the server's `/v1` base, such as {DEFAULT_URL}.",
                self.base
            );
        }
        // Lemonade's own sentence first; then, for a refused credential, which
        // setting supplies one, since the server's words do not name it.
        let hint = if matches!(status, 401 | 403) {
            "\n\nThe server did not accept this request's credential. Lucida sends \
             LEMONADE_API_KEY as a bearer token: set it, or check the one that is set."
        } else {
            ""
        };
        match error.message {
            Some(message) => anyhow!("Lemonade answered HTTP {status}: {message}{hint}"),
            None => anyhow!("Lemonade answered HTTP {status} for `{path}`: {}{hint}", preview(body)),
        }
    }

    /// The live half of the capability check: the model is listed and labelled
    /// `image`, and an edit's model is labelled `edit`. A refusal: nothing has
    /// been rendered, and asking again cannot change the server's answer.
    fn live_check<'a>(&self, req: &ImageRequest, listed: &'a [Listed]) -> Result<&'a Listed> {
        let images: Vec<&Listed> = listed.iter().filter(|m| m.is_image()).collect();
        if let Some(model) = images.iter().copied().find(|m| m.id == req.model) {
            if !req.references.is_empty() && !model.is_edit() {
                let editors: Vec<&Listed> = images.iter().copied().filter(|m| m.is_edit()).collect();
                return Err(refused(format!(
                    "`{}` on Lemonade cannot edit an image: the server does not label it \
                     `edit`.\n\nIts edit models: {}.",
                    req.model,
                    ids(&editors)
                )));
            }
            return Ok(model);
        }

        let hint = if let Some(near) = images.iter().find(|m| m.id.eq_ignore_ascii_case(&req.model)) {
            format!(" Model ids are case-sensitive here: did you mean `{}`?", near.id)
        } else if listed.iter().any(|m| m.id == req.model) {
            format!(" `{}` is listed, but not as an image model.", req.model)
        } else {
            String::new()
        };
        Err(refused(format!(
            "Lemonade at {} has no image model `{}`.{hint}\n\nIts image models: {}.\n\n\
             `lucida models --provider lemonade` lists them with their default sizes.",
            self.base,
            req.model,
            ids(&images)
        )))
    }

    fn post_generation(&self, req: &ImageRequest, (width, height): (u32, u32), seed: u64) -> Result<Response> {
        let mut body = json!({
            "model": req.model,
            "prompt": req.prompt,
            // Always sent: an omitted size is 512x512, whatever the model's own.
            "size": format!("{width}x{height}"),
            "seed": seed,
            "response_format": "b64_json",
            "n": 1,
        });
        if let Some(steps) = req.steps {
            body["steps"] = json!(steps);
        }
        if let Some(guidance) = req.guidance {
            body["cfg_scale"] = json!(crate::provider::guidance_as_written(guidance));
        }
        // Not retried (see `retry`): free, but a repeat renders again on a GPU
        // other work shares.
        self.authed(self.http.post(format!("{}/images/generations", self.base)))
            .timeout(self.render_timeout)
            .json(&body)
            .send()
            .map_err(|e| self.render_transport("/images/generations", &req.model, &e))
    }

    fn post_edit(&self, req: &ImageRequest, (width, height): (u32, u32), seed: u64) -> Result<Response> {
        let mut form = Form::new()
            .text("model", req.model.clone())
            .text("prompt", req.prompt.clone())
            .text("size", format!("{width}x{height}"))
            .text("seed", seed.to_string())
            .text("response_format", "b64_json")
            .text("n", "1");
        if let Some(steps) = req.steps {
            form = form.text("steps", steps.to_string());
        }
        if let Some(guidance) = req.guidance {
            form = form.text("cfg_scale", crate::provider::guidance_as_written(guidance).to_string());
        }
        // The fixed shape has already capped the count at MAX_REFERENCES.
        for path in &req.references {
            form = form.part(IMAGE_PART, image_part(path)?);
        }
        // Not retried, for the same reason as the generation.
        self.authed(self.http.post(format!("{}/images/edits", self.base)))
            .timeout(self.render_timeout)
            .multipart(form)
            .send()
            .map_err(|e| self.render_transport("/images/edits", &req.model, &e))
    }

    /// The image out of the answer, or why there is none.
    fn decode(&self, path: &str, response: Response) -> Result<(Vec<u8>, &'static str)> {
        let status = response.status();
        let text = response.text().context("reading Lemonade's answer to the render")?;
        if !status.is_success() {
            return Err(self.answer_error(path, status.as_u16(), &text));
        }
        let payload: Value = serde_json::from_str(&text).with_context(|| {
            format!("Lemonade answered {path} with something that is not JSON: {}", preview(&text))
        })?;
        let Some(encoded) = payload["data"][0]["b64_json"].as_str() else {
            bail!(
                "Lemonade answered {path} with HTTP {} and no image in \
                 `data[0].b64_json`. It sent: {}",
                status.as_u16(),
                preview(&text)
            );
        };
        let bytes = STANDARD
            .decode(encoded.trim())
            .context("decoding the image Lemonade returned")?;
        let mime = crate::sniff_mime(&bytes).ok_or_else(|| {
            anyhow!(
                "Lemonade returned {} bytes that are not a PNG, JPEG or WebP image",
                bytes.len()
            )
        })?;
        Ok((bytes, mime))
    }
}

impl ImageProvider for Client {
    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage> {
        // The fixed shape again, though every caller has checked it: it is pure,
        // and checking here means a refused render sends nothing whoever calls.
        CAPABILITIES.check(req)?;

        if crate::cancel::cancelled() {
            bail!(
                "cancelled at the client's request before anything was sent to \
                 Lemonade. Nothing was rendered, and this lane is free, so nothing is \
                 billed."
            );
        }

        let planned = preflight(req)?;

        let listed = self.models()?;
        let model = self.live_check(req, &listed)?;

        let (width, height) = match planned {
            Some(size) => size,
            None => {
                let size = dimensions(req, model.natural.unwrap_or(FALLBACK_DIMENSIONS));
                within_ceiling(size, None)?;
                size
            }
        };
        let seed = req.seed.unwrap_or_else(chosen_seed);

        let _turn = wait_for_turn(&req.model)?;
        // Again, with the turn in hand: a call cancelled while it queued must not
        // go on to render.
        if crate::cancel::cancelled() {
            bail!(
                "cancelled at the client's request while it waited its turn. Nothing \
                 was sent for `{}`, and this lane is free, so nothing is billed.",
                req.model
            );
        }

        let verb = if req.references.is_empty() { "Rendering" } else { "Editing" };
        let assumed = if planned.is_none() && model.natural.is_none() {
            " (the server lists no default size for this model)"
        } else {
            ""
        };
        eprintln!("{verb} {width}x{height}{assumed} with {} on Lemonade (seed {seed})…", req.model);
        eprintln!(
            "  The first render after a quiet spell includes loading the model, \
             which can take minutes."
        );

        let (path, response) = if req.references.is_empty() {
            ("/images/generations", self.post_generation(req, (width, height), seed)?)
        } else {
            ("/images/edits", self.post_edit(req, (width, height), seed)?)
        };
        let (bytes, mime) = self.decode(path, response)?;

        Ok(GeneratedImage {
            bytes,
            mime_type: mime.to_string(),
            commentary: None,
            // Ours, sent with the request: Lemonade reports none.
            seed: Some(seed),
        })
    }

    /// The server's image models, each with its labels and own defaults — the
    /// live per-model detail the fixed shape cannot carry.
    fn list_models(&self) -> Result<Vec<String>> {
        let default = crate::config::var("LUCIDA_LEMONADE_MODEL");
        Ok(self
            .models()?
            .iter()
            .filter(|m| m.is_image())
            .map(|m| m.describe(default.as_deref()))
            .collect())
    }
}

/// What this lane can settle with no network: an edit's size, which is its
/// source's shape on disk, held to the ceiling. `None` for a generation, whose
/// size is the model's own default and needs the server's listing.
///
/// The render calls this before it asks the server anything, and `--dry-run`
/// calls it in place of the render, so a dry run refuses what the render would.
pub fn preflight(req: &ImageRequest) -> Result<Option<(u32, u32)>> {
    let Some(path) = req.references.first() else {
        return Ok(None);
    };
    let natural = source_dimensions(path)?;
    let size = dimensions(req, natural);
    within_ceiling(size, Some(natural))?;
    Ok(Some(size))
}

/// The size sent: the source's or the model's own shape, the requested shape at
/// [`ASPECT_LONG_EDGE`], or the requested size — rounded to the grid.
fn dimensions(req: &ImageRequest, natural: (u32, u32)) -> (u32, u32) {
    let default = if req.aspect.is_some() {
        (ASPECT_LONG_EDGE, ASPECT_LONG_EDGE)
    } else {
        natural
    };
    req.pixels(default, PIXEL_GRID)
}

/// Refuses a size over the ceiling that `--size` did not ask for: an edit at its
/// source's own size, or a model whose own default is larger. `--size` itself is
/// refused by `Capabilities::check`.
fn within_ceiling((width, height): (u32, u32), source: Option<(u32, u32)>) -> Result<()> {
    if width.max(height) <= MAX_LONG_EDGE {
        return Ok(());
    }
    let why = match source {
        Some((w, h)) => format!("the source is {w}x{h}, so an edit at its own size would be {width}x{height}"),
        None => format!("this model's own default size is {width}x{height}"),
    };
    Err(refused(format!(
        "`lemonade` renders a long edge of at most {MAX_LONG_EDGE} pixels here, and {why}.\n\n\
         Pass `--size {MAX_LONG_EDGE}` (or less) to keep the shape at a size this lane renders."
    )))
}

fn source_dimensions(path: &str) -> Result<(u32, u32)> {
    let bytes = std::fs::read(path).with_context(|| format!("reading the image to edit ({path})"))?;
    crate::sniff_mime(&bytes)
        .and_then(|mime| crate::image_dimensions(&bytes, mime))
        .filter(|(width, height)| *width > 0 && *height > 0)
        .ok_or_else(|| {
            anyhow!("could not read the dimensions of {path}, so the edit cannot keep its shape")
        })
}

fn image_part(path: &str) -> Result<Part> {
    let bytes = std::fs::read(path).with_context(|| format!("reading the image to edit ({path})"))?;
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image.png")
        .to_string();
    // The format check has passed, so the bytes are a format this lane reads.
    let mime = crate::sniff_mime(&bytes).unwrap_or("image/png");
    Part::bytes(bytes)
        .file_name(name)
        .mime_str(mime)
        .context("attaching the image to edit")
}

/// A seed inside the range the server takes, from the same source as
/// ComfyUI's, so two renders in one process never share one.
fn chosen_seed() -> u64 {
    crate::comfy::arbitrary_seed() % SEED_LIMIT
}

/// Waits for this process's Lemonade turn, giving up if the call is cancelled.
fn wait_for_turn(model: &str) -> Result<MutexGuard<'static, ()>> {
    let mut announced = false;
    loop {
        match RENDERING.try_lock() {
            Ok(turn) => return Ok(turn),
            // The lock guards no data, so a render that panicked holding it left
            // nothing half-done for the next one.
            Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {}
        }
        if crate::cancel::cancelled() {
            bail!(
                "cancelled at the client's request while it waited its turn behind \
                 another Lemonade render in this process. Nothing was sent for \
                 `{model}`, and this lane is free, so nothing is billed."
            );
        }
        if !announced {
            eprintln!("Waiting for another Lemonade render in this process to finish…");
            announced = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn refused(message: String) -> anyhow::Error {
    anyhow::Error::new(crate::out::Refused(message))
}

fn ids(models: &[&Listed]) -> String {
    if models.is_empty() {
        return "none".to_string();
    }
    models.iter().map(|m| format!("`{}`", m.id)).collect::<Vec<_>>().join(", ")
}

/// The start of a body, for a message: enough to recognise, never a megabyte.
fn preview(text: &str) -> String {
    let cut: String = text.chars().take(300).collect();
    if cut.len() < text.len() { format!("{cut}…") } else { cut }
}

fn describe_wait(wait: Duration) -> String {
    if wait.as_secs() >= 60 {
        format!("{} minutes", wait.as_secs() / 60)
    } else {
        format!("{} ms", wait.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Aspect, Size};
    use crate::testserver::{Reply, Server, serve};

    /// `GET /v1/models`, transcribed from a live recording and scrubbed:
    /// checkpoint paths are dropped, and so is every chat entry but one. The two
    /// `custom`-labelled ids were one host's own models and are replaced with
    /// neutral ids; their labels and `recipe_options` are as the server sent
    /// them. Every other id, label and option is as recorded. One entry per line.
    const MODELS: &str = r#"{"object":"list","data":[
      {"id":"Anima-Turbo","object":"model","owned_by":"lemonade","downloaded":true,"labels":["image"],"recipe":"thenoise","recipe_options":{"cfg_scale":1.0,"height":1024,"steps":8,"width":1024},"image_defaults":{"cfg_scale":1.0,"height":1024,"steps":8,"width":1024}},
      {"id":"Flux-2-Klein-4B-TheNoise","object":"model","owned_by":"lemonade","downloaded":true,"labels":["image","edit"],"recipe":"thenoise","recipe_options":{"cfg_scale":1.0,"height":1024,"steps":4,"width":1024},"image_defaults":{"cfg_scale":1.0,"height":1024,"steps":4,"width":1024}},
      {"id":"Flux-2-Klein-9B-TheNoise","object":"model","owned_by":"lemonade","downloaded":true,"labels":["image","edit"],"recipe":"thenoise","recipe_options":{"cfg_scale":1.0,"height":1024,"steps":4,"width":1024},"image_defaults":{"cfg_scale":1.0,"height":1024,"steps":4,"width":1024}},
      {"id":"Krea-2-Turbo","object":"model","owned_by":"lemonade","downloaded":true,"labels":["image"],"recipe":"thenoise","recipe_options":{"cfg_scale":1.0,"height":1024,"steps":8,"width":1024},"image_defaults":{"cfg_scale":1.0,"height":1024,"steps":8,"width":1024}},
      {"id":"SDXL-Turbo","object":"model","owned_by":"lemonade","downloaded":true,"labels":["image"],"recipe":"sd-cpp","recipe_options":{"cfg_scale":1.0,"height":512,"steps":4,"width":512},"image_defaults":{"cfg_scale":1.0,"height":512,"steps":4,"width":512}},
      {"id":"custom-image-model-a","object":"model","owned_by":"lemonade","downloaded":true,"labels":["custom","image"],"recipe":"sd-cpp","recipe_options":{"cfg_scale":1.0,"height":1024,"steps":4,"width":1024}},
      {"id":"custom-image-model-b","object":"model","owned_by":"lemonade","downloaded":true,"labels":["custom","image"],"recipe":"sd-cpp","recipe_options":{"cfg_scale":6.0,"height":768,"sampling_method":"euler","steps":20,"width":768}},
      {"id":"Gemma-4-12B-it-MTP-GGUF","object":"model","owned_by":"lemonade","downloaded":true,"labels":["chat","tool-calling","llamacpp","vision","mtp"],"recipe":"llamacpp","recipe_options":{}}
    ]}"#;

    const KLEIN: &str = "Flux-2-Klein-4B-TheNoise";
    /// The recorded `custom` model with its own non-square, non-1024 defaults.
    const CUSTOM: &str = "custom-image-model-b";

    /// A gateway's answer when the Lemonade behind it is down. The identifier is
    /// in `code`; `type` is the generic `invalid_request_error`.
    const GATEWAY_DOWN: &str = r#"{"error":{"message":"Lemonade is not reachable.","type":"invalid_request_error","param":null,"code":"upstream_unreachable"}}"#;

    /// Recorded: a render naming a model the server does not hold. HTTP 404,
    /// with the identifier in both `code` and `type`. Transcribed from a live
    /// recording with the host's catalogue names replaced by placeholders and
    /// its count by a small number; the shape, `code`, `type`, `param` and
    /// `requested_model` are as recorded.
    const UNKNOWN_MODEL_BODY: &str = r#"{"error":{"code":"model_not_found","message":"Model 'lucida-recording-no-such-model' was not found. Available models include: 'catalogue-model-a', 'catalogue-model-b', 'catalogue-model-c', and 3 more. Use 'lemonade list' or GET /api/v1/models?show_all=true to see all available models.","param":"model","requested_model":"lucida-recording-no-such-model","type":"model_not_found"}}"#;

    /// Lemonade's other error shape, a bare string. Written, not recorded: the
    /// recording drew no error in it — an edit on a model without the `edit`
    /// label and a malformed size both rendered — so this is the documented
    /// shape, kept so a string `error` still reaches the caller as a sentence.
    const STRING_ERROR_BODY: &str = r#"{"error":"Model SDXL-Turbo does not support image editing"}"#;

    /// A 64x64 black PNG: the recording's image is a megabyte of base64, and
    /// only the answer's shape is under test.
    const TINY_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAIklEQVR4nO3BAQ0AAADCoPdPbQ8HFAAAAAAAAAAAAAAA8G4wQAABiwCo9wAAAABJRU5ErkJggg==";

    /// Both image routes answer alike. The keys are the recording's; only
    /// `b64_json` is replaced.
    fn generated() -> String {
        format!(r#"{{"created":1791327294,"data":[{{"b64_json":"{TINY_PNG_B64}"}}]}}"#)
    }

    fn client_at(base: &str) -> Client {
        Client {
            base: base.to_string(),
            auth: Some("Bearer test-key".into()),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(crate::retry::CONNECT_TIMEOUT)
                .no_proxy()
                .build()
                .unwrap(),
            render_timeout: Duration::from_secs(10),
        }
    }

    /// The `/v1` base on the test server, so the recorded paths are the real ones.
    fn wired(server: &Server) -> Client {
        client_at(&format!("{}/v1", server.url()))
    }

    fn request(model: &str) -> ImageRequest {
        ImageRequest {
            prompt: "a brass astrolabe".into(),
            model: model.into(),
            ..Default::default()
        }
    }

    /// A file holding a PNG signature and an IHDR — all `image_dimensions` reads.
    fn png_file(label: &str, width: u32, height: u32) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("lucida-lemonade-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.png");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        std::fs::write(&path, &png).unwrap();
        (dir, path.to_string_lossy().into_owned())
    }

    fn refused(error: &anyhow::Error) -> bool {
        crate::out::code_for(error) == crate::out::REFUSED
    }

    // --- one render -----------------------------------------------------------

    #[test]
    fn a_generation_always_sends_a_size_and_a_seed_and_reports_the_seed() {
        let server = serve(vec![Reply::json(MODELS), Reply::json(&generated())]);
        let image = wired(&server).generate(&request(KLEIN)).unwrap();

        let requests = server.finish();
        let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["/v1/models", "/v1/images/generations"]);
        for sent in &requests {
            assert_eq!(sent.header("authorization"), Some("Bearer test-key"), "{}", sent.path);
        }

        let body = requests[1].json();
        assert_eq!(body["model"], KLEIN);
        // The model's own default, never omitted: an omitted size is 512x512.
        assert_eq!(body["size"], "1024x1024");
        assert_eq!(body["response_format"], "b64_json");
        assert_eq!(body["n"], 1);
        // Chosen here, since Lemonade reports none — and the one reported is
        // the one that was sent.
        let sent = body["seed"].as_u64().expect("no seed was sent");
        assert!(sent < SEED_LIMIT, "{sent}");
        assert_eq!(image.seed, Some(sent));
        // Nothing asked for, so nothing sent: the model's recipe decides.
        assert!(body.get("steps").is_none() && body.get("cfg_scale").is_none(), "{body}");

        assert_eq!(image.mime_type, "image/png");
        assert_eq!(crate::image_dimensions(&image.bytes, "image/png"), Some((64, 64)));
    }

    #[test]
    fn the_size_comes_from_the_model_unless_a_shape_or_size_is_asked_for() {
        let cases: [(Option<&str>, Option<u32>, &str); 3] = [
            (None, None, "768x768"),          // the model's own recipe default
            (Some("16:9"), None, "1024x576"), // a shape alone: long edge 1024
            (None, Some(1536), "1536x1536"),  // a size alone: the model's shape, scaled
        ];
        for (aspect, size, expected) in cases {
            let server = serve(vec![Reply::json(MODELS), Reply::json(&generated())]);
            let req = ImageRequest {
                aspect: aspect.map(|a| Aspect::parse(a).unwrap()),
                size: size.map(Size),
                steps: Some(8),
                guidance: Some(3.5),
                ..request(CUSTOM)
            };
            wired(&server).generate(&req).unwrap();
            let body = server.finish().remove(1).json();
            assert_eq!(body["size"], expected, "{aspect:?} {size:?}");
            assert_eq!(body["steps"], 8);
            assert_eq!(body["cfg_scale"], 3.5);
        }
    }

    #[test]
    fn an_edit_sends_one_image_part_at_its_sources_own_shape() {
        let (dir, source) = png_file("edit-source", 1600, 900);
        let server = serve(vec![Reply::json(MODELS), Reply::json(&generated())]);
        let req = ImageRequest {
            references: vec![source],
            ..request(KLEIN)
        };
        let image = wired(&server).generate(&req).unwrap();

        let requests = server.finish();
        assert_eq!(requests[1].path, "/v1/images/edits");
        assert!(
            requests[1]
                .header("content-type")
                .is_some_and(|t| t.starts_with("multipart/form-data")),
            "an edit must be multipart"
        );
        let form = requests[1].body_text();
        assert_eq!(form.matches(&format!("name=\"{IMAGE_PART}\"")).count(), 1, "{form}");
        // 1600x900 on the grid: the source's shape, not the model's square.
        assert!(form.contains("name=\"size\"\r\n\r\n1600x896"), "{form}");
        assert!(form.contains(&format!("name=\"seed\"\r\n\r\n{}", image.seed.unwrap())), "{form}");
        assert!(form.contains(&format!("name=\"model\"\r\n\r\n{KLEIN}")), "{form}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A JPEG source travels as `image/jpeg` in the same part, at the size its
    /// frame header (SOF0) gives, rounded to the grid — 1000x744 is the recorded
    /// off-grid size the server itself rounded up to 1008x752.
    #[test]
    fn a_jpeg_edit_sends_its_bytes_as_jpeg_at_the_size_its_frame_header_gives() {
        let dir = std::env::temp_dir().join(format!("lucida-lemonade-jpeg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.jpg");
        // SOI, an APP0 segment to step over, then SOF0: precision, height,
        // width, one component. All `image_dimensions` reads.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpeg.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        jpeg.extend_from_slice(&744u16.to_be_bytes());
        jpeg.extend_from_slice(&1000u16.to_be_bytes());
        jpeg.extend_from_slice(&[0x01, 0x01, 0x11, 0x00, 0xFF, 0xD9]);
        std::fs::write(&path, &jpeg).unwrap();

        let server = serve(vec![Reply::json(MODELS), Reply::json(&generated())]);
        let req = ImageRequest {
            references: vec![path.to_string_lossy().into_owned()],
            ..request(KLEIN)
        };
        wired(&server).generate(&req).unwrap();

        let requests = server.finish();
        assert_eq!(requests[1].path, "/v1/images/edits");
        let form = requests[1].body_text();
        let part = format!("name=\"{IMAGE_PART}\"; filename=\"source.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n");
        assert_eq!(form.matches(&format!("name=\"{IMAGE_PART}\"")).count(), 1, "{form}");
        assert!(form.contains(&part), "{form}");
        assert!(form.contains("name=\"size\"\r\n\r\n1008x752"), "{form}");
        // The file's own bytes, not a re-encoding: the frame header is there.
        let sent = &requests[1].body;
        assert!(sent.windows(jpeg.len()).any(|w| w == jpeg.as_slice()), "the JPEG was not sent as it is");
        let _ = std::fs::remove_dir_all(dir);
    }

    // --- refused before anything is sent ---------------------------------------

    #[test]
    fn an_edit_larger_than_the_ceiling_is_refused_before_any_request() {
        let (dir, source) = png_file("huge-source", 4000, 3000);
        let server = serve(vec![]);
        let req = ImageRequest {
            references: vec![source],
            ..request(KLEIN)
        };
        let error = wired(&server).generate(&req).unwrap_err();
        assert!(server.finish().is_empty(), "a refused edit reached the server");
        assert!(refused(&error), "{error:#}");
        let text = format!("{error:#}");
        assert!(text.contains("4000x3000"), "{text}");
        assert!(text.contains(&format!("--size {MAX_LONG_EDGE}")), "{text}");

        // And the remedy works: at the ceiling the edit keeps the source's shape.
        let fixed = ImageRequest {
            size: Some(Size(2048)),
            ..req
        };
        assert_eq!(dimensions(&fixed, (4000, 3000)), (2048, 1536));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_fixed_shape_refusal_sends_nothing() {
        let (dir, png) = png_file("refusals", 512, 512);
        let webp = dir.join("sticker.webp");
        std::fs::write(&webp, b"RIFF\0\0\0\0WEBPVP8 ").unwrap();
        let webp = webp.to_string_lossy().into_owned();

        let edit = |references: Vec<String>| ImageRequest {
            references,
            ..request(KLEIN)
        };
        let cases: Vec<(&str, ImageRequest, String)> = vec![
            (
                "mask",
                ImageRequest {
                    mask: Some(png.clone()),
                    ..edit(vec![png.clone()])
                },
                "comfyui".to_string(),
            ),
            (
                "negative prompt",
                ImageRequest {
                    negative_prompt: Some("fog".into()),
                    ..request(KLEIN)
                },
                "negative prompt".to_string(),
            ),
            ("WebP reference", edit(vec![webp.clone()]), "reads only".to_string()),
            (
                "too many references",
                edit(vec![png.clone(); MAX_REFERENCES + 1]),
                format!("at most {MAX_REFERENCES} reference"),
            ),
            (
                "long edge",
                ImageRequest {
                    size: Some(Size(MAX_LONG_EDGE * 2)),
                    ..request(KLEIN)
                },
                format!("at most {MAX_LONG_EDGE}"),
            ),
            (
                "seed",
                ImageRequest {
                    seed: Some(SEED_LIMIT),
                    ..request(KLEIN)
                },
                "outside that range".to_string(),
            ),
        ];
        for (what, req, says) in cases {
            let server = serve(vec![]);
            let error = wired(&server).generate(&req).expect_err(what);
            assert!(server.finish().is_empty(), "{what}: a refused render reached the server");
            assert!(refused(&error), "{what}: {error:#}");
            assert!(format!("{error:#}").contains(&says), "{what}: {error:#}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    // --- refused after one listing, never rendered ------------------------------

    fn refused_after_one_listing(req: &ImageRequest) -> String {
        let server = serve(vec![Reply::json(MODELS)]);
        let error = wired(&server).generate(req).unwrap_err();
        let requests = server.finish();
        assert_eq!(requests.len(), 1, "a live refusal must not render");
        assert_eq!(requests[0].path, "/v1/models");
        assert!(refused(&error), "{error:#}");
        format!("{error:#}")
    }

    #[test]
    fn an_unknown_model_is_refused_after_one_listing_naming_the_image_models() {
        let text = refused_after_one_listing(&request("No-Such-Model"));
        for image_model in [KLEIN, "SDXL-Turbo", CUSTOM] {
            assert!(text.contains(image_model), "{text}");
        }
        assert!(!text.contains("Gemma"), "a chat model was offered as an image model: {text}");
    }

    #[test]
    fn a_mis_cased_model_id_is_refused_naming_the_right_one() {
        let text = refused_after_one_listing(&request(&KLEIN.to_lowercase()));
        assert!(text.contains(&format!("did you mean `{KLEIN}`")), "{text}");
    }

    #[test]
    fn a_chat_model_is_refused_as_not_an_image_model() {
        let text = refused_after_one_listing(&request("Gemma-4-12B-it-MTP-GGUF"));
        assert!(text.contains("not as an image model"), "{text}");
    }

    /// The ceiling holds for the size this lane resolves, not only for the one
    /// `--size` names: a model whose own default is over it is refused before
    /// anything renders. No listed model is that large, so the listing is
    /// written for the test from the recorded SDXL entry.
    #[test]
    fn a_model_whose_own_default_is_over_the_ceiling_is_refused_after_one_listing() {
        let big = r#"{"object":"list","data":[{"id":"Huge-Model","object":"model","labels":["image"],"recipe":"sd-cpp","recipe_options":{"cfg_scale":1.0,"height":2304,"steps":4,"width":4096}}]}"#;
        let server = serve(vec![Reply::json(big)]);
        let error = wired(&server).generate(&request("Huge-Model")).unwrap_err();
        assert_eq!(server.finish().len(), 1, "a refusal over the ceiling must not render");
        assert!(refused(&error), "{error:#}");
        let text = format!("{error:#}");
        assert!(text.contains("4096x2304") && text.contains(&format!("--size {MAX_LONG_EDGE}")), "{text}");

        // At the ceiling, the model's own shape survives.
        let fixed = ImageRequest {
            size: Some(Size(MAX_LONG_EDGE)),
            ..request("Huge-Model")
        };
        assert_eq!(dimensions(&fixed, (4096, 2304)), (2048, 1152));
    }

    /// The grid is applied before the ceiling, so a size that only reaches the
    /// ceiling once rounded is measured as it will be sent.
    #[test]
    fn the_ceiling_is_measured_after_rounding_to_the_grid() {
        assert!(within_ceiling(dimensions(&request(KLEIN), (2056, 1000)), None).is_err());
        assert_eq!(dimensions(&request(KLEIN), (2040, 1000)), (2048, 1008));
        assert!(within_ceiling((2048, 1008), None).is_ok());
    }

    #[test]
    fn an_edit_on_a_model_that_cannot_edit_is_refused_after_one_listing() {
        let (dir, source) = png_file("not-an-editor", 512, 512);
        let req = ImageRequest {
            references: vec![source],
            ..request("SDXL-Turbo")
        };
        let text = refused_after_one_listing(&req);
        assert!(text.contains("cannot edit"), "{text}");
        assert!(text.contains(KLEIN), "the refusal must name a model that edits: {text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    // --- a server that is not there, or says no --------------------------------

    #[test]
    fn the_gateways_502_reads_as_unreachable() {
        // Three: the listing is idempotent and retried, and the gateway answers each.
        let server = serve(vec![
            Reply::status(502, GATEWAY_DOWN),
            Reply::status(502, GATEWAY_DOWN),
            Reply::status(502, GATEWAY_DOWN),
        ]);
        let error = wired(&server).generate(&request(KLEIN)).unwrap_err();
        assert_eq!(server.finish().len(), 3);
        assert!(error.downcast_ref::<Unreachable>().is_some(), "{error:#}");
        let text = format!("{error:#}");
        assert!(text.contains("not reachable") && text.contains("LUCIDA_LEMONADE_URL"), "{text}");
    }

    #[test]
    fn a_502_is_unreachable_only_without_a_lemonade_error_or_with_the_gateways_code() {
        // Same sentence, another identifier: matched on the code, never the words.
        let other = r#"{"error":{"message":"Lemonade is not reachable.","type":"invalid_request_error","code":"model_crashed"}}"#;
        for (body, unreachable) in [
            ("", true),
            ("<html>502 Bad Gateway</html>", true),
            (GATEWAY_DOWN, true),
            (other, false),
        ] {
            let server = serve(vec![Reply::json(MODELS), Reply::status(502, body)]);
            let error = wired(&server).generate(&request(KLEIN)).unwrap_err();
            assert_eq!(server.finish().len(), 2, "the render must not be repeated");
            assert_eq!(
                error.downcast_ref::<Unreachable>().is_some(),
                unreachable,
                "{body:?}: {error:#}"
            );
        }
    }

    #[test]
    fn a_refused_connection_reads_as_unreachable() {
        // Port 1: nothing listens, so the refusal is immediate (and retried).
        let error = client_at("http://127.0.0.1:1/v1")
            .generate(&request(KLEIN))
            .unwrap_err();
        assert!(error.downcast_ref::<Unreachable>().is_some(), "{error:#}");
        assert!(format!("{error:#}").contains("http://127.0.0.1:1/v1"), "{error:#}");
    }

    #[test]
    fn a_base_url_without_v1_is_named_as_the_problem() {
        let server = serve(vec![Reply::status(404, r#"{"detail":"Not Found"}"#)]);
        let error = client_at(server.url()).generate(&request(KLEIN)).unwrap_err();
        assert_eq!(server.finish()[0].path, "/models");
        let text = format!("{error:#}");
        assert!(text.contains("LUCIDA_LEMONADE_URL") && text.contains("/v1"), "{text}");
    }

    #[test]
    fn both_error_shapes_reach_the_caller_with_their_status() {
        for (status, body) in [(404, UNKNOWN_MODEL_BODY), (400, STRING_ERROR_BODY)] {
            let server = serve(vec![Reply::json(MODELS), Reply::status(status, body)]);
            let error = wired(&server).generate(&request(KLEIN)).unwrap_err();
            server.finish();
            let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
            let message = parsed["error"]["message"]
                .as_str()
                .or_else(|| parsed["error"].as_str())
                .unwrap();
            assert_eq!(format!("{error:#}"), format!("Lemonade answered HTTP {status}: {message}"));
        }
    }

    /// A server that wants a key says so in its own words; Lucida adds which
    /// setting supplies one, for the listing and the render alike.
    #[test]
    fn a_rejected_credential_names_the_setting_that_sends_one() {
        let denied = r#"{"error":{"message":"Invalid API key","type":"invalid_request_error"}}"#;
        for status in [401, 403] {
            let server = serve(vec![Reply::status(status, denied)]);
            let error = wired(&server).generate(&request(KLEIN)).unwrap_err();
            server.finish();
            let text = format!("{error:#}");
            assert!(text.contains(&format!("HTTP {status}: Invalid API key")), "{text}");
            assert!(text.contains("LEMONADE_API_KEY"), "{text}");

            let server = serve(vec![Reply::json(MODELS), Reply::status(status, denied)]);
            let error = wired(&server).generate(&request(KLEIN)).unwrap_err();
            server.finish();
            assert!(format!("{error:#}").contains("LEMONADE_API_KEY"), "{error:#}");
        }
        // Any other refusal is Lemonade's sentence alone.
        let server = serve(vec![Reply::json(MODELS), Reply::status(400, denied)]);
        let error = wired(&server).generate(&request(KLEIN)).unwrap_err();
        server.finish();
        assert!(!format!("{error:#}").contains("LEMONADE_API_KEY"), "{error:#}");
    }

    #[test]
    fn a_success_without_an_image_says_what_came_back() {
        for (body, says) in [
            (r#"{"created":1,"data":[]}"#, r#""data":[]"#),
            (r#"{"created":1,"data":[{"b64_json":"aGVsbG8="}]}"#, "not a PNG, JPEG or WebP"),
        ] {
            let server = serve(vec![Reply::json(MODELS), Reply::json(body)]);
            let error = wired(&server).generate(&request(KLEIN)).unwrap_err();
            server.finish();
            assert!(format!("{error:#}").contains(says), "{error:#}");
        }
    }

    /// Answers the listing, then holds the render open without a word — the
    /// one conversation `testserver` cannot script, since it sends every reply
    /// at once. Detached: the thread ends when the process does.
    fn silent_after_listing() -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{MODELS}",
                MODELS.len()
            )
            .unwrap();
            drop(stream);
            let (held, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(2));
            drop(held);
        });
        base
    }

    #[test]
    fn a_render_that_outlives_its_timeout_says_it_may_still_be_rendering() {
        let client = Client {
            render_timeout: Duration::from_millis(500),
            ..client_at(&silent_after_listing())
        };
        let error = client.generate(&request(KLEIN)).unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("may still be rendering") && text.contains(KLEIN), "{text}");
        assert!(error.downcast_ref::<Unreachable>().is_none(), "slow is not absent: {text}");
    }

    // --- one render at a time ---------------------------------------------------

    #[test]
    fn a_render_waits_for_the_one_before_it() {
        let held = RENDERING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let server = serve(vec![Reply::json(MODELS), Reply::json(&generated())]);
        let client = wired(&server);
        let waiting = std::thread::spawn(move || client.generate(&request(KLEIN)));

        std::thread::sleep(Duration::from_millis(500));
        assert!(!waiting.is_finished(), "the render went ahead while another held the lane");
        drop(held);

        waiting.join().unwrap().unwrap();
        assert_eq!(server.finish().len(), 2);
    }

    #[test]
    fn a_render_waiting_its_turn_can_still_be_cancelled() {
        let held = RENDERING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let server = serve(vec![Reply::json(MODELS)]);
        let client = wired(&server);
        let token = crate::cancel::Token::new();
        let handle = token.clone();
        let waiting = std::thread::spawn(move || {
            crate::cancel::with(token, || client.generate(&request(KLEIN)))
        });

        // Cancelled only once the listing has arrived: the render is then past
        // its first cancellation check, and the lane is held, so the wait for
        // the turn is the only place left that can see it.
        assert!(server.await_requests(1), "the render never asked for the listing");
        handle.cancel();
        let error = waiting.join().unwrap().unwrap_err();
        drop(held);

        let text = format!("{error:#}");
        assert!(text.contains("cancelled") && text.contains("waited its turn"), "{text}");
        assert!(text.contains("nothing is billed"), "{text}");
        assert_eq!(server.finish().len(), 1, "a cancelled wait must not render");
    }

    // --- the listing, settings, seeds ------------------------------------------

    #[test]
    fn the_model_list_is_the_image_models_with_their_own_defaults() {
        let server = serve(vec![Reply::json(MODELS)]);
        let client = wired(&server);
        let listed = crate::config::with_injected(&[("LUCIDA_LEMONADE_MODEL", "SDXL-Turbo")], || {
            client.list_models()
        })
        .unwrap();
        server.finish();
        assert_eq!(
            listed,
            [
                "Anima-Turbo  (image; 1024x1024; 8 steps; cfg 1)",
                "Flux-2-Klein-4B-TheNoise  (image, edit; 1024x1024; 4 steps; cfg 1)",
                "Flux-2-Klein-9B-TheNoise  (image, edit; 1024x1024; 4 steps; cfg 1)",
                "Krea-2-Turbo  (image; 1024x1024; 8 steps; cfg 1)",
                "SDXL-Turbo  (image; 512x512; 4 steps; cfg 1; default, from LUCIDA_LEMONADE_MODEL)",
                "custom-image-model-a  (custom, image; 1024x1024; 4 steps; cfg 1)",
                "custom-image-model-b  (custom, image; 768x768; 20 steps; cfg 6)",
            ]
        );
    }

    /// The README states the size sent when the server lists none for a model.
    #[test]
    fn the_readme_names_the_fallback_size() {
        let readme = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();
        let (width, height) = FALLBACK_DIMENSIONS;
        assert!(readme.contains(&format!("{width}x{height} when the server lists none")), "README");
    }

    #[test]
    fn a_unit_test_never_falls_back_to_a_real_server() {
        let error = Client::from_env().err().expect("built a client with no URL injected");
        assert!(format!("{error:#}").contains("LUCIDA_LEMONADE_URL"), "{error:#}");
    }

    #[test]
    fn credentials_leave_the_url_and_the_key_wins() {
        let url = ("LUCIDA_LEMONADE_URL", "http://user:pw@lemonade.example:13305/v1/");
        let client = crate::config::with_injected(&[url], Client::from_env).unwrap();
        // Trailing slash gone, credentials gone from the string that gets printed.
        assert_eq!(client.base, "http://lemonade.example:13305/v1");
        assert_eq!(client.auth.as_deref(), Some("Basic dXNlcjpwdw=="));

        let keyed = crate::config::with_injected(&[url, ("LEMONADE_API_KEY", "k-123")], Client::from_env).unwrap();
        assert_eq!(keyed.auth.as_deref(), Some("Bearer k-123"));
    }

    #[test]
    fn chosen_seeds_stay_inside_the_range_the_server_takes() {
        let seeds: std::collections::HashSet<u64> = (0..1000).map(|_| chosen_seed()).collect();
        assert_eq!(seeds.len(), 1000, "two renders were handed the same seed");
        assert!(seeds.iter().all(|seed| *seed < SEED_LIMIT));
    }

    #[test]
    fn the_fixed_shape_is_the_one_the_spec_describes() {
        let caps = CAPABILITIES;
        assert!(matches!(caps.aspect, AspectSupport::Free { multiple_of: PIXEL_GRID }));
        assert!(caps.size && caps.seed && caps.steps && caps.guidance && caps.references);
        assert!(!caps.negative_prompt && !caps.workflow && !caps.mask.accepted());
        assert_eq!(caps.max_references, Some(MAX_REFERENCES));
        assert_eq!(caps.reference_formats, Some(REFERENCE_FORMATS));
        assert_eq!(caps.max_long_edge, Some(MAX_LONG_EDGE));
        assert_eq!(caps.seed_limit, Some(SEED_LIMIT));
        assert_eq!(caps.provenance, Provenance::Unmarked);
    }

    #[test]
    fn an_unreachable_server_names_the_preference_only_when_it_chose_the_lane() {
        let unreachable = || anyhow::Error::new(Unreachable("Lemonade is not reachable at x".into()));
        let preference = Some(crate::provider::DefaultSource::Preference {
            setting: "LUCIDA_IMAGE_PROVIDERS",
            position: 1,
            of: 2,
        });

        let chosen = format!("{:#}", explain_unreachable(unreachable(), &preference));
        assert!(chosen.contains("LUCIDA_IMAGE_PROVIDERS") && chosen.contains("not a fallback"), "{chosen}");
        assert!(chosen.contains("not reachable at x"), "the cause was lost: {chosen}");

        let named = format!("{:#}", explain_unreachable(unreachable(), &None));
        assert!(!named.contains("not a fallback"), "{named}");

        // Only this failure: a refusal from the same lane is not about reachability.
        let refusal = anyhow::Error::new(crate::out::Refused("no such model".into()));
        let refusal = explain_unreachable(refusal, &preference);
        assert!(!format!("{refusal:#}").contains("not a fallback"));
        assert_eq!(crate::out::code_for(&refusal), crate::out::REFUSED);
    }
}
