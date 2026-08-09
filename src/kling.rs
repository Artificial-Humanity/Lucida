//! Kling — Kuaishou's video models, reached directly.
//!
//! The third video provider, and deliberately a *direct* relationship rather
//! than the one Runway would have sold us. Runway's endpoint fronts
//! `kling3.0_pro` among a dozen other vendors' models; this talks to Kling's own
//! API on the user's own key, which is the difference between a provider and a
//! middleman — no markup, capabilities knowable per model, and whatever
//! provenance the vendor actually applies rather than whatever survives a
//! passthrough nobody documents.
//!
//! # Two authentication schemes; this is the current one
//!
//! Kling accepts a single API key as a bearer token, and also a legacy
//! AccessKey/SecretKey pair signed into a short-lived HS256 JWT. Only the first
//! is implemented, because it is the one Kling now recommends and the one that
//! needs no crypto in a project whose dependency list is six crates long. A key
//! from the legacy scheme will simply be rejected, and [`explain_error`] says so
//! by name rather than leaving someone to guess.
//!
//! # The thing to know before trusting this API's rejections
//!
//! **Validation is looser than the documentation.** `duration` is documented as
//! `5` or `10`, and the API accepts `3`, `4`, `8` and `15` as well — measured.
//! What it does with them is undefined and would be discovered by paying for it,
//! so [`capabilities`] declares the documented pair and Lucida refuses the rest
//! locally. Refusing something the provider would accept is the safe direction
//! when the alternative is billing for an undefined result.
//!
//! **Unknown fields are silently ignored**, the Stability trap again: `image_url`
//! and `input_image` both validate and both do nothing, while `image` is the
//! field that works. So no capability here rests on the absence of an error.
//!
//! **Errors do not enumerate accepted values.** Runway's rejections list what
//! would have worked; Kling's say only that a value is invalid. Everything below
//! was therefore established by elimination, one candidate at a time, against a
//! request kept deliberately unacceptable by a field validated *later* than the
//! one under test.

use crate::provider::{
    AspectSupport, DurationSupport, Provenance, VideoCapabilities, VideoProvider,
};
use crate::video::{VideoRequest, VideoStatus};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;

const API_ROOT: &str = "https://api-singapore.klingai.com";

/// Every model this key can reach, newest last. Measured by elimination rather
/// than read from a catalogue endpoint, because there is not one.
pub const MODELS: &[&str] = &[
    "kling-v1",
    "kling-v1-5",
    "kling-v1-6",
    "kling-v2-master",
    "kling-v2-1",
    "kling-v2-1-master",
    "kling-v2-5-turbo",
    "kling-v2-6",
];

/// Turbo rather than the newest, on the rule Veo already set here: video is
/// billed per second, so the cheaper tier is the default and the expensive one
/// is asked for by name.
pub const DEFAULT_MODEL: &str = "kling-v2-5-turbo";

pub const MODEL_ALIASES: &[(&str, &str)] = &[
    ("kling", "kling-v2-5-turbo"),
    ("kling-turbo", "kling-v2-5-turbo"),
    ("kling-latest", "kling-v2-6"),
    ("kling-master", "kling-v2-1-master"),
];

/// Quality tiers, cheapest first. The same model rendered at different cost and
/// fidelity — not a model id, not a resolution, and not a concept any other
/// provider here has.
const MODES: &[&str] = &["std", "pro", "master"];

const RATIOS: &[&str] = &["16:9", "9:16", "1:1"];

pub fn resolve_model(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    MODEL_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, id)| (*id).to_string())
        .unwrap_or(key)
}

pub fn is_kling_model(model: &str) -> bool {
    let id = resolve_model(model);
    MODELS.contains(&id.as_str())
}

/// Uniform across all eight models — and that uniformity is a fact about the
/// *validator*, not a promise about the renders.
///
/// Every model accepts the same three ratios and all three tiers, which was
/// measured rather than assumed. It is worth being explicit that this is weaker
/// evidence than it looks: this same validator accepts durations the
/// documentation rules out, so "the API did not object" establishes what can be
/// *sent*, not what will be rendered well. Where the two disagree, the
/// documented answer wins here.
pub fn capabilities(_model: &str) -> VideoCapabilities {
    VideoCapabilities {
        provider: "kling",
        tagline: "Kling, direct rather than through an aggregator. Eight model versions and three quality tiers, at 5 or 10 seconds.",
        aspect: AspectSupport::Named(RATIOS),
        // The documented pair, deliberately narrower than what the API will
        // accept — see the module note.
        duration: DurationSupport::Named(&[5, 10]),
        image_to_video: true,
        text_to_video: true,
        negative_prompt: true,
        // No resolution field; the ratio and the tier decide the pixel count.
        resolution: false,
        // No seed on any endpoint, so a Kling render cannot be repeated exactly.
        seed: false,
        modes: MODES,
        // Nobody has rendered anything here and read the bytes. See
        // `Provenance::Unverified` — BFL is why this is not a guess.
        provenance: Provenance::Unverified,
    }
}

pub struct Client {
    key: String,
    http: reqwest::blocking::Client,
    base: String,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        let key = crate::config::var("KLINGAI_API_KEY").ok_or_else(|| {
            let where_to_put_it = match crate::config::preferred_path() {
                Some(path) => format!(
                    "Set KLINGAI_API_KEY, or add it to {} — `lucida config --set \
                     KLINGAI_API_KEY` reads it from stdin so it stays out of your \
                     shell history.",
                    path.display()
                ),
                None => "Set KLINGAI_API_KEY.".to_string(),
            };
            anyhow!(
                "no Kling API key found.\n\n{where_to_put_it}\n\n\
                 Keys come from the Kling developer console — this is a paid API \
                 and every render costs credits. Lucida uses the single-key \
                 scheme; a legacy AccessKey/SecretKey pair will not work here."
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

    fn authed(&self, builder: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        builder.header("Authorization", format!("Bearer {}", self.key))
    }

    /// Remaining units in the subscribed resource pack. Free, and the key check.
    pub fn credits(&self) -> Result<f64> {
        // A day-wide window, because the endpoint wants one and the answer we
        // care about — the pack's remaining quantity — does not depend on it.
        let now = crate::clock::now() * 1000;
        let url = format!(
            "{}/account/costs?start_time={}&end_time={now}",
            self.base,
            now - 86_400_000
        );

        let response = crate::retry::send_idempotent("checking the balance", || {
            self.authed(self.http.get(&url))
        })
        .context("checking the Kling balance")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_error(status.as_u16(), &text));
        }

        let payload: Value = response.json().context("parsing the balance response")?;
        payload["data"]["resource_pack_subscribe_infos"][0]["remaining_quantity"]
            .as_f64()
            .ok_or_else(|| {
                anyhow!("no resource pack on this account — add one in the Kling console")
            })
    }

    fn body(&self, req: &VideoRequest, model: &str) -> Result<(&'static str, Value)> {
        let mut body = serde_json::Map::new();
        body.insert("model_name".into(), json!(model));
        body.insert("prompt".into(), json!(req.prompt));

        let endpoint = match &req.image {
            Some(path) => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("reading source image {path}"))?;
                use base64::{Engine as _, engine::general_purpose::STANDARD};
                // `image`, and only `image`. `image_url` and `input_image` both
                // validate and both do nothing, which is the silent drop this
                // project exists to refuse — so the field name is pinned by a
                // test rather than trusted to memory.
                body.insert("image".into(), json!(STANDARD.encode(&bytes)));
                "image2video"
            }
            None => "text2video",
        };

        if let Some(ratio) = req.aspect {
            body.insert("aspect_ratio".into(), json!(ratio.to_string()));
        }
        // A string, not a number — the API rejects `5` and accepts `"5"`.
        if let Some(seconds) = req.duration {
            body.insert("duration".into(), json!(seconds.to_string()));
        }
        if let Some(mode) = &req.mode {
            body.insert("mode".into(), json!(mode));
        }
        if let Some(negative) = &req.negative_prompt {
            body.insert("negative_prompt".into(), json!(negative));
        }

        Ok((endpoint, Value::Object(body)))
    }
}

impl VideoProvider for Client {
    fn start(&self, req: &VideoRequest) -> Result<String> {
        let model = resolve_model(&req.model);
        let (endpoint, body) = self.body(req, &model)?;

        // Deliberately not retried (see `retry`): this is the call that spends.
        let response = self
            .authed(self.http.post(format!("{}/v1/videos/{endpoint}", self.base)))
            .json(&body)
            .send()
            .context("starting the Kling render")?;

        let status = response.status();
        let payload: Value = response.json().context("parsing the task response")?;

        // Kling answers 200 with a non-zero `code` for some failures, so the
        // HTTP status alone is not the verdict — a check on `status.is_success()`
        // by itself would read a refusal as a submission.
        if !status.is_success() || payload["code"].as_i64().unwrap_or(0) != 0 {
            bail!("{}", explain_error(status.as_u16(), &payload.to_string()));
        }

        payload["data"]["task_id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Kling accepted the job but returned no task_id: {payload}"))
    }

    fn poll(&self, operation: &str) -> Result<VideoStatus> {
        // Either query path resolves either kind of task — measured — so this
        // does not need to remember which endpoint submitted it. That matters:
        // `lucida check <id>` is handed an id and nothing else.
        let url = format!("{}/v1/videos/text2video/{operation}", self.base);
        let response = crate::retry::send_idempotent("polling the render", || {
            self.authed(self.http.get(&url))
        })
        .context("polling the Kling task")?;

        let status = response.status();
        let payload: Value = response.json().context("parsing the task response")?;
        if !status.is_success() || payload["code"].as_i64().unwrap_or(0) != 0 {
            bail!("{}", explain_error(status.as_u16(), &payload.to_string()));
        }

        let task = &payload["data"];
        match task["task_status"].as_str().unwrap_or_default() {
            "succeed" => {
                let url = task["task_result"]["videos"][0]["url"]
                    .as_str()
                    .ok_or_else(|| anyhow!("the render finished but carries no video: {task}"))?;
                Ok(VideoStatus::Done(self.download(url)?))
            }
            "failed" => {
                let reason = task["task_status_msg"].as_str().unwrap_or("no reason given");
                bail!("the render failed: {reason}");
            }
            // submitted, processing — and anything unrecognised, which must read
            // as in-flight rather than as finished.
            _ => Ok(VideoStatus::Pending),
        }
    }
}

impl Client {
    fn download(&self, url: &str) -> Result<Vec<u8>> {
        // No credential: the output is on a CDN host that does not need one.
        let response = crate::retry::send_idempotent("downloading the video", || self.http.get(url))
            .with_context(|| {
                format!(
                    "downloading the finished video. The render was billed; fetch \
                     it by hand while the URL lasts:\n\n  {url}"
                )
            })?;

        if !response.status().is_success() {
            bail!(
                "the video URL returned HTTP {}.\n\n  {url}",
                response.status().as_u16()
            );
        }

        Ok(response.bytes().context("reading video bytes")?.to_vec())
    }
}

/// Kling's errors are `{code, message, request_id}` and never name the values
/// that would have worked, so the most useful thing to add is the request id —
/// it is what their support asks for — and a translation of the auth case.
fn explain_error(status: u16, body: &str) -> String {
    let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = payload["message"].as_str().unwrap_or(body);
    let code = payload["code"].as_i64().unwrap_or(0);

    let mut out = match (status, code) {
        (401 | 403, _) | (_, 1000..=1004) => format!(
            "Kling rejected the key: {message}\n\n\
             Check KLINGAI_API_KEY. Lucida uses the single-key bearer scheme; if \
             this key is half of a legacy AccessKey/SecretKey pair it cannot work \
             here, since that scheme signs a JWT per request."
        ),
        (429, _) | (_, 1302 | 1303) => format!(
            "Kling is rate limiting or the quota is exhausted: {message}\n\n\
             `lucida models --provider kling` reports the remaining balance."
        ),
        _ => format!("Kling refused the request: {message}"),
    };

    if let Some(id) = payload["request_id"].as_str() {
        out.push_str(&format!("\n\nrequest_id: {id}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Aspect;
    use crate::testserver::{Reply, serve};

    fn wired(server: &crate::testserver::Server) -> Client {
        Client::recorded(server.url())
    }

    /// Duration is documented as 5 or 10 and the API accepts 3, 4, 8 and 15 too
    /// — measured. What it renders for those is undefined and would be
    /// discovered by paying for it, so the documented pair is what Lucida
    /// declares and everything else is refused before it can bill.
    #[test]
    fn only_the_documented_durations_are_offered() {
        let caps = capabilities(DEFAULT_MODEL);
        assert!(caps.duration.accepts(5) && caps.duration.accepts(10));
        for loose in [3, 4, 8, 15] {
            assert!(
                !caps.duration.accepts(loose),
                "{loose}s passes Kling's validator but is not documented, and \
                 sending it would bill for an undefined result"
            );
        }
    }

    /// `image_url` and `input_image` both validate and both do nothing. The one
    /// that works is `image`, base64-encoded — pinned here because the failure
    /// mode is silent and expensive.
    #[test]
    fn a_still_travels_in_the_field_that_is_not_ignored() {
        let dir = std::env::temp_dir().join(format!("lucida-kling-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("frame.png");
        std::fs::write(&source, [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();

        let server = serve(vec![Reply::json(
            r#"{"code":0,"message":"SUCCEED","data":{"task_id":"915468728228253726"}}"#,
        )]);

        let request = VideoRequest {
            prompt: "pan right".into(),
            model: "kling".into(),
            image: Some(source.to_string_lossy().into_owned()),
            duration: Some(5),
            ..Default::default()
        };
        wired(&server).start(&request).unwrap();

        let requests = server.finish();
        assert_eq!(requests[0].path, "/v1/videos/image2video");
        let body = requests[0].json();
        assert!(body["image"].is_string(), "the still must go in `image`");
        assert!(
            body["image_url"].is_null() && body["input_image"].is_null(),
            "those fields validate and do nothing, so sending one is a silent drop"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Duration is a *string* on this API: `5` is rejected and `"5"` accepted.
    #[test]
    fn duration_is_sent_as_a_string() {
        let server = serve(vec![Reply::json(
            r#"{"code":0,"message":"SUCCEED","data":{"task_id":"abc"}}"#,
        )]);

        let request = VideoRequest {
            prompt: "a fox".into(),
            model: "kling-v2-6".into(),
            aspect: Aspect::parse("9:16").ok(),
            duration: Some(10),
            mode: Some("pro".into()),
            ..Default::default()
        };
        wired(&server).start(&request).unwrap();

        let body = server.finish()[0].json();
        assert_eq!(body["duration"], "10", "a number here is rejected by the API");
        assert_eq!(body["aspect_ratio"], "9:16");
        assert_eq!(body["mode"], "pro");
        assert_eq!(body["model_name"], "kling-v2-6");
    }

    /// Kling answers **200 with a non-zero `code`** for some refusals, so the
    /// HTTP status alone is not the verdict. Checking only `is_success()` would
    /// read a refusal as a submission and hand back a task id that does not
    /// exist.
    #[test]
    fn a_200_carrying_an_error_code_is_still_a_failure() {
        let server = serve(vec![Reply::json(
            r#"{"code":1201,"message":"duration value '99' is invalid","request_id":"abc-123"}"#,
        )]);

        let error = wired(&server)
            .start(&VideoRequest {
                prompt: "a fox".into(),
                model: "kling".into(),
                ..Default::default()
            })
            .unwrap_err()
            .to_string();

        assert!(error.contains("duration value"), "{error}");
        // Their support asks for this, and it is the only handle on a failed call.
        assert!(error.contains("abc-123"), "the request_id was dropped: {error}");
        server.finish();
    }

    /// Everything short of succeed or failed is still working.
    #[test]
    fn every_in_flight_status_reads_as_pending() {
        for state in ["submitted", "processing", "something_new"] {
            let server = serve(vec![Reply::json(&format!(
                r#"{{"code":0,"data":{{"task_status":"{state}"}}}}"#
            ))]);
            let status = wired(&server).poll("915468728228253726").unwrap();
            assert!(matches!(status, VideoStatus::Pending), "`{state}` was not in flight");
            server.finish();
        }
    }

    #[test]
    fn a_finished_render_is_downloaded_without_the_key() {
        let server = serve(vec![
            Reply::json(
                r#"{"code":0,"data":{"task_status":"succeed","task_result":{"videos":[{"url":"{{server}}/cdn/out.mp4"}]}}}"#,
            ),
            Reply::bytes("video/mp4", b"mp4-bytes"),
        ]);

        match wired(&server).poll("915468728228253726").unwrap() {
            VideoStatus::Done(bytes) => assert_eq!(bytes, b"mp4-bytes"),
            VideoStatus::Pending => panic!("expected the render to be done"),
        }

        let requests = server.finish();
        assert_eq!(requests[1].header("authorization"), None, "key sent to the CDN");
    }

    /// A key from the legacy AccessKey/SecretKey scheme cannot work here, and
    /// the message has to say so — otherwise it reads as "your key is wrong"
    /// when the key is fine and the *scheme* is the problem.
    #[test]
    fn a_rejected_key_distinguishes_the_two_auth_schemes() {
        let explained = explain_error(401, r#"{"code":1001,"message":"invalid token"}"#);
        assert!(explained.contains("KLINGAI_API_KEY"), "{explained}");
        assert!(explained.contains("AccessKey"), "{explained}");
    }

    /// Every advertised model must be claimed by the inference that routes a
    /// bare `--model` to this provider, or it silently goes to Veo.
    #[test]
    fn every_advertised_model_is_claimed() {
        for model in MODELS {
            assert!(is_kling_model(model), "{model} is unclaimed");
        }
        for (alias, _) in MODEL_ALIASES {
            assert!(is_kling_model(alias), "the alias `{alias}` is unclaimed");
        }
        assert!(!is_kling_model("veo-3.1-fast-generate-preview"));
        assert!(!is_kling_model("gen4.5"));
    }
}
