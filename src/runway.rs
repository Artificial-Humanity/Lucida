//! Runway — Gen-4 video.
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
    Aspect, AspectSupport, DurationSupport, Provenance, VideoCapabilities, VideoProvider,
};
use crate::video::{VideoRequest, VideoStatus};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;

const API_ROOT: &str = "https://api.dev.runwayml.com/v1";

/// The dated API version, sent on every request.
///
/// Mandatory: without it the API answers 400 rather than assuming a default.
/// Runway supports a version for four months after its successor ships, so this
/// is a thing to bump deliberately — and the canary is what will notice when it
/// stops being accepted, since nothing else would.
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
        aspect: AspectSupport::Named(if turbo { TURBO_RATIOS } else { GEN45_RATIOS }),
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
            AspectSupport::Named(ratios) => ratios,
            // Unreachable: Runway's ratios are always a named set.
            AspectSupport::Free { .. } => GEN45_RATIOS,
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
        let response = crate::retry::send_idempotent("polling the render", || {
            self.authed(self.http.get(format!("{}/tasks/{operation}", self.base)))
        })
        .context("polling the Runway task")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        let payload: Value = response.json().context("parsing the task response")?;
        match payload["status"].as_str().unwrap_or_default() {
            "SUCCEEDED" => {
                let url = payload["output"][0]
                    .as_str()
                    .ok_or_else(|| anyhow!("the render finished but carries no output: {payload}"))?;
                Ok(VideoStatus::Done(self.download(url)?))
            }
            "FAILED" | "CANCELLED" => {
                let reason = payload["failure"]
                    .as_str()
                    .or_else(|| payload["failureCode"].as_str())
                    .unwrap_or("no reason given");
                bail!("the render failed: {reason}");
            }
            // PENDING, RUNNING, THROTTLED — all still in flight.
            _ => Ok(VideoStatus::Pending),
        }
    }
}

impl Client {
    fn download(&self, url: &str) -> Result<Vec<u8>> {
        // No credential on this request: the output URL is pre-signed object
        // storage, and sending a key to a host that does not need it is how
        // credentials end up somewhere unexpected. Same reasoning as BFL, and
        // the opposite of Veo, whose download URL does require one — which is
        // exactly why both are pinned by tests.
        let response = crate::retry::send_idempotent("downloading the video", || {
            self.http.get(url)
        })
        .with_context(|| {
            format!(
                "downloading the finished video. The render was billed; its URL \
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

        Ok(response.bytes().context("reading video bytes")?.to_vec())
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
}
