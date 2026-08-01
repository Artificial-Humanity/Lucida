//! ComfyUI provider — local generation, no credential, nothing embedded.
//!
//! ComfyUI is the least tidy provider on the list and that is the point: it takes
//! a **workflow graph**, not a prompt, so implementing it second forces the
//! abstraction to hold two genuinely dissimilar shapes rather than two variations
//! on one. It also costs nothing per render, which is what makes it a reasonable
//! thing to iterate against.
//!
//! The call pattern is closer to Veo than to Gemini: submit, get an id back, poll
//! until it appears in history, then download the result. Unlike Veo the download
//! goes over the same HTTP API rather than the filesystem, which is deliberate —
//! it means a ComfyUI on another machine works exactly as well as a local one.

use crate::provider::{
    AspectSupport, Capabilities, GeneratedImage, ImageProvider, ImageRequest, Provenance,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Where ComfyUI is listening. Overridden with `LUCIDA_COMFYUI_URL`.
///
/// Note this is the port, not a hostname — on the machine this was developed
/// against, `comfyui.ai-lab-0` does not resolve while `localhost:8188` answers,
/// and a hostname baked in here would have failed confusingly.
///
/// The scheme is used verbatim, so `https://` works, as does a path-prefixed
/// reverse proxy such as `https://host/comfyui/`.
pub const DEFAULT_URL: &str = "http://127.0.0.1:8188";

/// Latent-space models render in units of 16 pixels.
const PIXEL_GRID: u32 = 16;

const DEFAULT_STEPS: u32 = 20;
const DEFAULT_GUIDANCE: f32 = 5.0;
const DEFAULT_DIMENSIONS: (u32, u32) = (1024, 1024);

/// Aliases naming a model *family*, not a file.
///
/// The concrete filenames differ per installation, so an alias resolves against
/// whatever the server reports it actually has. Hardcoding the filenames on this
/// machine would produce a provider that only works on this machine.
pub const MODEL_ALIASES: &[(&str, &str)] = &[
    ("local", "flux2"),
    ("klein", "flux2"),
    ("flux2", "flux2"),
    ("flux-2", "flux2"),
    ("flux2-klein", "flux2"),
];

/// The three files a Flux.2 render needs, as this server names them.
#[derive(Debug, Clone)]
struct Checkpoint {
    unet: String,
    clip: String,
    vae: String,
}

pub struct Client {
    /// Always credential-free: any `user:pass@` in the configured URL is moved
    /// into `auth` at construction. This string appears in error messages, and a
    /// password reaching stderr is a password in someone's terminal scrollback.
    base: String,
    auth: Option<String>,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn from_env() -> Result<Self> {
        // Read through `config`, not the environment directly, so a GUI-launched
        // MCP server can be pointed at a remote ComfyUI too — the same problem
        // that made the Google key invisible applies to every setting here.
        let configured =
            crate::config::var("LUCIDA_COMFYUI_URL").unwrap_or_else(|| DEFAULT_URL.to_string());
        let (base, url_credentials) = split_credentials(configured.trim_end_matches('/'));

        // An explicit variable beats credentials embedded in the URL, which are
        // easy to leave behind in a shell profile and hard to notice.
        let auth = match crate::config::var("LUCIDA_COMFYUI_AUTH") {
            Some(raw) if !raw.trim().is_empty() => Some(auth_header(&raw)),
            _ => url_credentials,
        };

        let mut builder = reqwest::blocking::Client::builder()
            // Generous, because the request that matters is the poll loop and a
            // cold model load alone can take minutes on a consumer GPU.
            .timeout(Duration::from_secs(300));

        // Lucida trusts the bundled Mozilla root set and never reads the system
        // CA store — that is what lets the musl binary do TLS on a host with no
        // certificates installed. The cost is that an internal ComfyUI behind a
        // private CA is rejected, so there has to be a way to add one.
        if let Some(path) = crate::config::var("LUCIDA_COMFYUI_CA") {
            let path = path.trim();
            if !path.is_empty() {
                let pem = std::fs::read(path).with_context(|| {
                    format!("reading the certificate named by LUCIDA_COMFYUI_CA ({path})")
                })?;
                let certificates = reqwest::Certificate::from_pem_bundle(&pem).with_context(|| {
                    format!("parsing {path} as PEM certificates for LUCIDA_COMFYUI_CA")
                })?;
                if certificates.is_empty() {
                    bail!(
                        "LUCIDA_COMFYUI_CA points at {path}, which contains no \
                         certificates. It should be a PEM file holding the issuing \
                         CA certificate."
                    );
                }
                // Added to the built-in roots rather than replacing them, so a
                // private CA for one host does not break TLS everywhere else.
                for certificate in certificates {
                    builder = builder.add_root_certificate(certificate);
                }
            }
        }

        let http = builder.build().context("building HTTP client")?;

        Ok(Self { base, auth, http })
    }

    /// Attaches credentials, when there are any, to every request — including the
    /// `/view` download, which is a separate round trip and would otherwise be
    /// the one call that fails against a fenced server.
    fn authed(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        match &self.auth {
            Some(value) => builder.header(reqwest::header::AUTHORIZATION, value),
            None => builder,
        }
    }

    fn get(&self, path: &str) -> Result<reqwest::blocking::Response> {
        self.authed(self.http.get(format!("{}{path}", self.base)))
            .send()
            .map_err(|e| anyhow!("{}", self.explain_transport(path, &e)))
    }

    /// Turns a failed round trip into the right diagnosis.
    ///
    /// Connection refused is the most likely failure by a wide margin, but it is
    /// not the only one, and reporting "check that the server is running" for a
    /// rejected certificate sends people to look at a server that is running
    /// perfectly well.
    fn explain_transport(&self, path: &str, error: &reqwest::Error) -> String {
        if is_certificate_failure(error) {
            return format!(
                "TLS verification failed for {} (requesting {path}).\n\n\
                 The server answered — this is a certificate problem, not an \
                 unreachable host. Lucida trusts the bundled Mozilla root set and \
                 does not read the system CA store, so a private or self-signed \
                 certificate is rejected by default.\n\n\
                 Point LUCIDA_COMFYUI_CA at the issuing certificate (a PEM file) \
                 to trust it.\n\n\
                 Underlying error: {error}",
                self.base
            );
        }

        if error.is_timeout() {
            return format!(
                "timed out talking to ComfyUI at {} (requesting {path}).\n\n\
                 The server accepted the connection but did not answer in time. \
                 If it is mid-render this is expected only for very long \
                 operations; otherwise check the server's own logs.",
                self.base
            );
        }

        format!(
            "could not reach ComfyUI at {} (requesting {path}).\n\n\
             Check that the server is running and that LUCIDA_COMFYUI_URL points \
             at it — it defaults to {DEFAULT_URL}.\n\n\
             Underlying error: {error}",
            self.base
        )
    }

    /// The options a node advertises for one of its inputs.
    ///
    /// `/object_info` is how ComfyUI reports what files it can actually see, which
    /// is the only trustworthy source: the model directory may be a bind mount,
    /// a symlink, or on another host entirely.
    fn options(&self, node: &str, input: &str) -> Result<Vec<String>> {
        let response = self.get(&format!("/object_info/{node}"))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            // Checked before the missing-node message, which would otherwise be
            // reported for a 401 — telling the user their ComfyUI build is too
            // old when in fact it never let them in.
            if let Some(refusal) = explain_refusal(status, &self.auth) {
                bail!("{refusal}");
            }
            bail!(
                "ComfyUI has no `{node}` node (HTTP {status}). This build may be \
                 too old, or missing a custom node pack."
            );
        }

        let payload: Value = response.json().context("parsing /object_info")?;
        let entry = &payload[node]["input"]["required"][input][0];
        Ok(entry
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Resolves a model name into the trio of files a render needs.
    ///
    /// An alias names a family and every file in that family shares its token, so
    /// one substring match finds all three. An explicit `.safetensors` name pins
    /// the diffusion model and lets the family token come from it.
    fn resolve(&self, model: &str) -> Result<Checkpoint> {
        let key = model.trim().to_ascii_lowercase();
        let family = MODEL_ALIASES
            .iter()
            .find(|(alias, _)| *alias == key)
            .map(|(_, family)| (*family).to_string());

        let (family, pinned_unet) = match family {
            Some(family) => (family, None),
            // Not an alias: treat it as a filename and derive the family from it,
            // so `--model flux2-klein-9b-kv_bf16.safetensors` still finds its
            // matching text encoder and VAE.
            None => {
                let family = key
                    .split(['-', '_', '.'])
                    .find(|part| part.starts_with("flux"))
                    .unwrap_or("flux2")
                    .to_string();
                (family, Some(model.trim().to_string()))
            }
        };

        let unet = match pinned_unet {
            Some(name) => {
                let available = self.options("UNETLoader", "unet_name")?;
                if !available.iter().any(|candidate| candidate == &name) {
                    bail!(
                        "ComfyUI has no diffusion model named `{name}`.\n\nIt reports: {}",
                        list_or_none(&available)
                    );
                }
                name
            }
            None => self.pick("UNETLoader", "unet_name", &family, "diffusion model")?,
        };
        let clip = self.pick("CLIPLoader", "clip_name", &family, "text encoder")?;
        let vae = self.pick("VAELoader", "vae_name", &family, "VAE")?;

        Ok(Checkpoint { unet, clip, vae })
    }

    fn pick(&self, node: &str, input: &str, family: &str, what: &str) -> Result<String> {
        let available = self.options(node, input)?;
        available
            .iter()
            .find(|name| normalize(name).contains(&normalize(family)))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "ComfyUI has no {what} for the `{family}` family.\n\n\
                     It reports: {}\n\n\
                     Install the matching files, or name one explicitly with --model.",
                    list_or_none(&available)
                )
            })
    }

    /// Sends an image to the server so a graph can name it.
    ///
    /// ComfyUI's `LoadImage` takes a filename in the server's own input
    /// directory, not a path and not bytes — so editing means uploading first,
    /// even when the server is the same machine. Over HTTP rather than by
    /// writing to the input directory, for the same reason `/view` is used to
    /// fetch results: a remote or containerised server then needs no shared
    /// mount.
    fn upload(&self, path: &str) -> Result<String> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading the image to edit ({path})"))?;
        let filename = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload.png")
            .to_string();

        let form = reqwest::blocking::multipart::Form::new()
            // `overwrite` keeps the server's input directory from filling up with
            // `image (1).png`, `image (2).png` on repeated edits of one file.
            .text("overwrite", "true")
            .part(
                "image",
                reqwest::blocking::multipart::Part::bytes(bytes).file_name(filename),
            );

        let response = self
            .authed(self.http.post(format!("{}/upload/image", self.base)))
            .multipart(form)
            .send()
            .map_err(|e| anyhow!("{}", self.explain_transport("/upload/image", &e)))?;

        let status = response.status();
        if !status.is_success() {
            if let Some(refusal) = explain_refusal(status.as_u16(), &self.auth) {
                bail!("uploading the image to edit was refused.\n\n{refusal}");
            }
            let text = response.text().unwrap_or_default();
            bail!(
                "ComfyUI rejected the upload of {path} (HTTP {}): {text}",
                status.as_u16()
            );
        }

        let payload: Value = response.json().context("parsing the upload response")?;
        let name = payload["name"]
            .as_str()
            .ok_or_else(|| anyhow!("ComfyUI accepted the upload but named no file: {payload}"))?;

        // The server may file it under a subfolder; `LoadImage` wants the
        // combined path, not the bare name.
        Ok(match payload["subfolder"].as_str().unwrap_or("") {
            "" => name.to_string(),
            subfolder => format!("{subfolder}/{name}"),
        })
    }

    /// Builds the API-format graph, for generation or editing.
    ///
    /// Two deliberate choices carry through both shapes. `CFGGuider` rather than
    /// the `BasicGuider` + `FluxGuidance` pair the text-to-image blueprint uses,
    /// because it takes negative conditioning and that is what makes a negative
    /// prompt a real input rather than a parameter quietly dropped. And, when
    /// editing, the encoded source is attached to **both** the positive and the
    /// negative conditioning — which looks redundant and is what the Flux.2 edit
    /// blueprint does, since the negative branch has to be denoising the same
    /// picture for the guidance difference to mean anything.
    fn graph_for(
        req: &ImageRequest,
        ckpt: &Checkpoint,
        seed: u64,
        uploaded: &[String],
    ) -> Value {
        let steps = req.steps.unwrap_or(DEFAULT_STEPS);
        let guidance = req.guidance.unwrap_or(DEFAULT_GUIDANCE);
        let negative = req.negative_prompt.clone().unwrap_or_default();

        let mut nodes = serde_json::Map::new();
        let mut node = |id: &str, class: &str, inputs: Value| {
            nodes.insert(
                id.to_string(),
                json!({ "class_type": class, "inputs": inputs }),
            );
        };

        node("unet", "UNETLoader",
             json!({ "unet_name": ckpt.unet, "weight_dtype": "default" }));
        node("clip", "CLIPLoader",
             json!({ "clip_name": ckpt.clip, "type": "flux2" }));
        node("vae", "VAELoader", json!({ "vae_name": ckpt.vae }));
        node("pos", "CLIPTextEncode",
             json!({ "text": req.prompt, "clip": ["clip", 0] }));
        node("neg", "CLIPTextEncode",
             json!({ "text": negative, "clip": ["clip", 0] }));

        // Each reference is scaled, encoded, and chained onto both conditionings.
        // Chaining is how Flux.2 takes more than one reference: every
        // ReferenceLatent adds its latent to the conditioning it receives.
        let mut positive = json!(["pos", 0]);
        let mut negative_cond = json!(["neg", 0]);

        for (index, name) in uploaded.iter().enumerate() {
            let (load, scale, encode) = (
                format!("load{index}"),
                format!("scale{index}"),
                format!("encode{index}"),
            );
            node(&load, "LoadImage", json!({ "image": name }));
            // `resolution_steps` rounds the scaled dimensions onto the latent
            // grid, so the size derived below is one Flux can actually render.
            node(&scale, "ImageScaleToTotalPixels",
                 json!({ "image": [load, 0], "upscale_method": "nearest-exact",
                         "megapixels": 1.0, "resolution_steps": PIXEL_GRID }));
            node(&encode, "VAEEncode",
                 json!({ "pixels": [scale, 0], "vae": ["vae", 0] }));

            let (pos_ref, neg_ref) = (format!("pos_ref{index}"), format!("neg_ref{index}"));
            node(&pos_ref, "ReferenceLatent",
                 json!({ "conditioning": positive, "latent": [encode, 0] }));
            node(&neg_ref, "ReferenceLatent",
                 json!({ "conditioning": negative_cond, "latent": [encode, 0] }));
            positive = json!([pos_ref, 0]);
            negative_cond = json!([neg_ref, 0]);
        }

        // An edit keeps the source's shape unless the request says otherwise, so
        // the dimensions are read off the scaled image on the server rather than
        // guessed here. Asking for an aspect or size overrides that, which is how
        // an edit reframes.
        let asked_for_dimensions = req.aspect.is_some() || req.size.is_some();
        let (width, height) = if uploaded.is_empty() || asked_for_dimensions {
            let (w, h) = req.pixels(DEFAULT_DIMENSIONS, PIXEL_GRID);
            (json!(w), json!(h))
        } else {
            node("size", "GetImageSize", json!({ "image": ["scale0", 0] }));
            (json!(["size", 0]), json!(["size", 1]))
        };

        node("guider", "CFGGuider",
             json!({ "model": ["unet", 0], "positive": positive,
                     "negative": negative_cond, "cfg": guidance }));
        node("sampler", "KSamplerSelect", json!({ "sampler_name": "euler" }));
        node("sigmas", "Flux2Scheduler",
             json!({ "steps": steps, "width": width, "height": height }));
        node("noise", "RandomNoise", json!({ "noise_seed": seed }));
        node("latent", "EmptyFlux2LatentImage",
             json!({ "width": width, "height": height, "batch_size": 1 }));
        node("out", "SamplerCustomAdvanced",
             json!({ "noise": ["noise", 0], "guider": ["guider", 0],
                     "sampler": ["sampler", 0], "sigmas": ["sigmas", 0],
                     "latent_image": ["latent", 0] }));
        node("decode", "VAEDecode",
             json!({ "samples": ["out", 0], "vae": ["vae", 0] }));
        node("save", "SaveImage",
             json!({ "images": ["decode", 0], "filename_prefix": "lucida" }));

        Value::Object(nodes)
    }

    fn submit(&self, graph: &Value) -> Result<String> {
        let response = self
            .authed(self.http.post(format!("{}/prompt", self.base)))
            .json(&json!({ "prompt": graph, "client_id": "lucida" }))
            .send()
            .map_err(|e| anyhow!("{}", self.explain_transport("/prompt", &e)))?;

        let status = response.status();
        let payload: Value = if status.is_success() {
            response.json().context("parsing the queue response")?
        } else {
            if let Some(refusal) = explain_refusal(status.as_u16(), &self.auth) {
                bail!("{refusal}");
            }
            let text = response.text().unwrap_or_default();
            bail!("{}", explain_rejection(status.as_u16(), &text));
        };

        payload["prompt_id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("ComfyUI queued the job but returned no prompt_id: {payload}"))
    }

    /// Polls until the render appears in history, then pulls the image out.
    ///
    /// A render can outlast any sensible HTTP timeout, so the wait lives here
    /// rather than in a single long request. The deadline is generous because the
    /// first render after a restart pays for loading tens of gigabytes of weights
    /// before it samples anything.
    fn await_image(&self, prompt_id: &str) -> Result<(Vec<u8>, String)> {
        let started = Instant::now();
        let deadline = Duration::from_secs(1800);
        let report_every = Duration::from_secs(30);
        let mut announced = false;
        // Tracked explicitly rather than by testing elapsed seconds against a
        // modulus: one poll running long enough to step over the window skips the
        // report entirely, and a render that stops reporting looks like a render
        // that has hung.
        let mut last_report = Instant::now();

        loop {
            if started.elapsed() > deadline {
                bail!(
                    "gave up waiting after {} minutes. The render may still be \
                     running — check the ComfyUI queue at {}.",
                    deadline.as_secs() / 60,
                    self.base
                );
            }

            let history: Value = self
                .get(&format!("/history/{prompt_id}"))?
                .json()
                .context("parsing render history")?;

            if let Some(entry) = history.get(prompt_id) {
                let status = &entry["status"];
                if status["status_str"].as_str() == Some("error") {
                    bail!("{}", explain_failure(entry));
                }
                if status["completed"].as_bool().unwrap_or(false) {
                    eprintln!("Render finished in {}s.", started.elapsed().as_secs());
                    return self.download(&entry["outputs"]);
                }
            }

            if !announced {
                eprintln!(
                    "Queued. A cold render loads the model first and can take \
                     several minutes."
                );
                announced = true;
            } else if last_report.elapsed() >= report_every {
                eprintln!("  still rendering ({}s elapsed)…", started.elapsed().as_secs());
                last_report = Instant::now();
            }

            std::thread::sleep(Duration::from_secs(3));
        }
    }

    /// Fetches the rendered image over `/view`.
    ///
    /// Over HTTP rather than off the filesystem, so a ComfyUI running in a
    /// container, or on another host, needs no shared mount to work.
    fn download(&self, outputs: &Value) -> Result<(Vec<u8>, String)> {
        let image = find_image(outputs).ok_or_else(|| {
            anyhow!("the render completed but produced no image: {outputs}")
        })?;

        let filename = image["filename"].as_str().unwrap_or_default();
        let subfolder = image["subfolder"].as_str().unwrap_or_default();
        let kind = image["type"].as_str().unwrap_or("output");

        let response = self.get(&format!(
            "/view?filename={}&subfolder={}&type={}",
            urlencode(filename),
            urlencode(subfolder),
            urlencode(kind)
        ))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            if let Some(refusal) = explain_refusal(status, &self.auth) {
                bail!("the render succeeded but the download was refused.\n\n{refusal}");
            }
            bail!("ComfyUI rendered {filename} but refused to serve it (HTTP {status})");
        }

        let mime = match filename.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("webp") => "image/webp",
            _ => "image/png",
        };

        Ok((
            response.bytes().context("reading image bytes")?.to_vec(),
            mime.to_string(),
        ))
    }
}

pub const CAPABILITIES: Capabilities = Capabilities {
    provider: "comfyui",
    aspect: AspectSupport::Free {
        multiple_of: PIXEL_GRID,
    },
    size: true,
    seed: true,
    negative_prompt: true,
    references: true,
    steps: true,
    guidance: true,
    provenance: Provenance::Unmarked,
};

impl ImageProvider for Client {
    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage> {
        let ckpt = self.resolve(&req.model)?;
        // A render is only reproducible if the seed is known, so an unpinned
        // request still gets a concrete seed — chosen here and reported back —
        // rather than one the server picks and never tells us.
        let seed = req.seed.unwrap_or_else(arbitrary_seed);

        let uploaded = req
            .references
            .iter()
            .map(|path| self.upload(path))
            .collect::<Result<Vec<_>>>()?;

        if uploaded.is_empty() {
            let (width, height) = req.pixels(DEFAULT_DIMENSIONS, PIXEL_GRID);
            eprintln!("Rendering {width}x{height} with {} (seed {seed})…", ckpt.unet);
        } else {
            // The size is decided server-side from the source unless overridden,
            // so there is no honest number to print here.
            let shape = if req.aspect.is_some() || req.size.is_some() {
                let (w, h) = req.pixels(DEFAULT_DIMENSIONS, PIXEL_GRID);
                format!("{w}x{h}")
            } else {
                "at the source's size".to_string()
            };
            eprintln!(
                "Editing with {} reference image(s) {shape}, {} (seed {seed})…",
                uploaded.len(),
                ckpt.unet
            );
        }

        let prompt_id = self.submit(&Self::graph_for(req, &ckpt, seed, &uploaded))?;
        let (bytes, mime_type) = self.await_image(&prompt_id)?;

        Ok(GeneratedImage {
            bytes,
            mime_type,
            commentary: None,
            seed: Some(seed),
        })
    }

    fn list_models(&self) -> Result<Vec<String>> {
        self.options("UNETLoader", "unet_name")
    }
}

/// A seed with no `rand` dependency.
///
/// The clock is a poor random source in general and a perfectly good one here:
/// nothing depends on the value being unpredictable, only on successive renders
/// differing and the value being reported so it can be pinned again.
fn arbitrary_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        // Flux treats the low bits as significant; the nanosecond field alone is
        // too coarse on some platforms to differ between quick successive calls.
        .wrapping_mul(6_364_136_223_846_793_005)
        >> 1
}

/// Separates `scheme://user:pass@host/path` into a credential-free base URL and
/// an `Authorization` header value.
///
/// Done here rather than left to the HTTP client for two reasons: the base URL
/// is concatenated with paths by hand, and — more importantly — it is printed in
/// error messages. A password in an error message ends up in terminal
/// scrollback, CI logs and pasted bug reports.
fn split_credentials(url: &str) -> (String, Option<String>) {
    let Some((scheme, rest)) = url.split_once("://") else {
        return (url.to_string(), None);
    };

    // Only the authority can carry credentials; an `@` later in the path is
    // ordinary data and must be left alone.
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };

    match authority.rsplit_once('@') {
        Some((userinfo, host)) if !userinfo.is_empty() => (
            format!("{scheme}://{host}{path}"),
            Some(basic_header(userinfo)),
        ),
        _ => (url.to_string(), None),
    }
}

fn basic_header(userinfo: &str) -> String {
    format!("Basic {}", STANDARD.encode(userinfo))
}

/// Interprets `LUCIDA_COMFYUI_AUTH`.
///
/// Three forms are accepted because all three are what people actually have to
/// hand, and guessing wrong is a confusing 401 rather than a clear error:
///
/// - `Basic dXNlcjpwdw==` or `Bearer eyJ…` — already a header value, used as is.
/// - `user:password` — encoded as Basic. This is the Caddy `basic_auth` case.
/// - anything else — treated as a bare token and sent as `Bearer`.
fn auth_header(raw: &str) -> String {
    let value = raw.trim();

    // A leading alphabetic word followed by a value is an auth scheme. Checked
    // before the colon rule, since `Basic dXNlcjpwdw==` contains a colon inside
    // its base64 payload often enough to matter.
    let looks_like_a_header = value.split_once(' ').is_some_and(|(scheme, credentials)| {
        !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphabetic())
            && !credentials.trim().is_empty()
    });
    if looks_like_a_header {
        return value.to_string();
    }

    if value.contains(':') {
        return basic_header(value);
    }

    format!("Bearer {value}")
}

/// Explains a server that is there and refusing us, which is the opposite of the
/// "missing node" that a bare non-200 would otherwise be read as.
///
/// Takes whether credentials were sent, because "you need to authenticate" and
/// "the credentials you sent were rejected" send the reader to different places.
fn explain_refusal(status: u16, auth: &Option<String>) -> Option<String> {
    let sent = auth.is_some();
    match status {
        401 if sent => Some(
            "HTTP 401 — ComfyUI (or a proxy in front of it) rejected the \
             credentials.\n\n\
             They were sent, so this is a wrong username, password or token \
             rather than a missing one. Check LUCIDA_COMFYUI_AUTH, or the \
             credentials embedded in LUCIDA_COMFYUI_URL."
                .to_string(),
        ),
        401 => Some(
            "HTTP 401 — ComfyUI (or a proxy in front of it) requires \
             authentication, and none was sent.\n\n\
             Set LUCIDA_COMFYUI_AUTH to `user:password`, to a bare token, or to a \
             complete header value such as `Bearer eyJ…`. Credentials embedded in \
             LUCIDA_COMFYUI_URL (`https://user:pass@host`) work too."
                .to_string(),
        ),
        403 if sent => Some(
            "HTTP 403 — the credentials were accepted but this request is not \
             permitted.\n\n\
             The account may lack access rather than the password being wrong."
                .to_string(),
        ),
        403 => Some(
            "HTTP 403 — the server refused the request.\n\n\
             No credentials were sent; if this ComfyUI is behind a proxy that \
             requires them, set LUCIDA_COMFYUI_AUTH."
                .to_string(),
        ),
        407 => Some(
            "HTTP 407 — an HTTP proxy requires authentication. Check the \
             HTTPS_PROXY environment variable."
                .to_string(),
        ),
        _ => None,
    }
}

/// Whether a failed request died verifying TLS rather than connecting.
///
/// `reqwest` does not expose this as a predicate, so the source chain is
/// inspected. Matched broadly and case-insensitively because the wording comes
/// from rustls and differs per failure — `UnknownIssuer`, `NotValidForName`,
/// `CaUsedAsEndEntity` and `BadSignature` are all the same story as far as the
/// person reading the message is concerned.
fn is_certificate_failure(error: &reqwest::Error) -> bool {
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(current) = source {
        let text = current.to_string().to_ascii_lowercase();
        if text.contains("certificate")
            || text.contains("unknownissuer")
            || text.contains("notvalidforname")
            || text.contains("badsignature")
            || text.contains("invalidcertificate")
        {
            return true;
        }
        source = current.source();
    }
    false
}

/// Compares model filenames ignoring the punctuation that varies between
/// releases — `flux2`, `flux-2` and `flux_2` all name the same family.
fn normalize(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "nothing".to_string()
    } else {
        items.join(", ")
    }
}

/// Depth-first search for the first image record in a node's outputs.
///
/// Which node key holds it depends on the graph, and a `--workflow` override
/// could put it anywhere, so this looks for the shape rather than a fixed path.
fn find_image(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(map) => {
            if let Some(first) = map
                .get("images")
                .and_then(Value::as_array)
                .and_then(|images| images.iter().find(|i| i.get("filename").is_some()))
            {
                return Some(first);
            }
            map.values().find_map(find_image)
        }
        Value::Array(items) => items.iter().find_map(find_image),
        _ => None,
    }
}

/// Turns a rejected submission into something worth reading.
///
/// ComfyUI validates the whole graph before queueing and reports per-node
/// errors, which are precise but deeply nested — the useful sentence is usually
/// four levels down and easy to miss in the raw body.
fn explain_rejection(status: u16, body: &str) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);

    let mut lines = vec![format!(
        "ComfyUI rejected the workflow (HTTP {status}): {}",
        parsed["error"]["message"]
            .as_str()
            .unwrap_or("no message given")
    )];

    if let Some(nodes) = parsed["node_errors"].as_object() {
        for (node, detail) in nodes {
            for error in detail["errors"].as_array().unwrap_or(&Vec::new()) {
                lines.push(format!(
                    "  {node}: {} ({})",
                    error["message"].as_str().unwrap_or("unknown"),
                    error["details"].as_str().unwrap_or("")
                ));
            }
        }
    }

    if body.contains("not in") || body.contains("value_not_in_list") {
        lines.push(
            "\nThis usually means a model file named in the workflow is not \
             installed. Run `lucida models --provider comfyui` to see what the \
             server actually has."
                .to_string(),
        );
    }

    lines.join("\n")
}

/// Digs the cause out of a failed render.
fn explain_failure(entry: &Value) -> String {
    let messages = entry["status"]["messages"].as_array().cloned().unwrap_or_default();

    for message in &messages {
        // Each message is [name, payload]; the interesting one is execution_error.
        if message[0].as_str() == Some("execution_error") {
            let payload = &message[1];
            return format!(
                "the render failed in node `{}` ({}): {}",
                payload["node_id"].as_str().unwrap_or("?"),
                payload["node_type"].as_str().unwrap_or("?"),
                payload["exception_message"].as_str().unwrap_or("no detail")
            );
        }
    }

    format!("the render failed, and ComfyUI gave no detail: {entry}")
}

/// Percent-encodes a query parameter. Filenames can contain spaces and `&`, and
/// pulling in a URL crate for three characters is not worth it.
fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_tokens_survive_punctuation_differences() {
        assert!(normalize("flux2-klein_vae.safetensors").contains(&normalize("flux-2")));
        assert!(normalize("FLUX_2_klein.safetensors").contains(&normalize("flux2")));
    }

    #[test]
    fn images_are_found_wherever_the_graph_puts_them() {
        let outputs = json!({
            "some_node": { "text": ["ignored"] },
            "save": { "images": [{ "filename": "lucida_00001_.png",
                                   "subfolder": "", "type": "output" }] }
        });
        let found = find_image(&outputs).expect("image record");
        assert_eq!(found["filename"], "lucida_00001_.png");
    }

    #[test]
    fn missing_files_are_explained_rather_than_dumped() {
        let body = r#"{"error":{"message":"Prompt outputs failed validation"},
                       "node_errors":{"unet":{"errors":[
                         {"message":"Value not in list","details":"unet_name: 'nope'",
                          "type":"value_not_in_list"}]}}}"#;
        let explained = explain_rejection(400, body);
        assert!(explained.contains("unet: Value not in list"));
        assert!(explained.contains("lucida models --provider comfyui"));
    }

    #[test]
    fn execution_failures_name_the_node() {
        let entry = json!({ "status": { "status_str": "error", "messages": [
            ["execution_start", {}],
            ["execution_error", { "node_id": "out", "node_type": "SamplerCustomAdvanced",
                                  "exception_message": "HIP out of memory" }]
        ]}});
        assert!(explain_failure(&entry).contains("HIP out of memory"));
        assert!(explain_failure(&entry).contains("SamplerCustomAdvanced"));
    }

    #[test]
    fn successive_seeds_differ() {
        assert_ne!(arbitrary_seed(), arbitrary_seed());
    }

    #[test]
    fn filenames_with_spaces_survive_the_view_url() {
        assert_eq!(urlencode("a file&b.png"), "a%20file%26b.png");
    }

    fn checkpoint() -> Checkpoint {
        Checkpoint {
            unet: "flux2.safetensors".into(),
            clip: "flux2_te.safetensors".into(),
            vae: "flux2_vae.safetensors".into(),
        }
    }

    fn request(prompt: &str) -> ImageRequest {
        ImageRequest {
            prompt: prompt.into(),
            model: "klein".into(),
            ..Default::default()
        }
    }

    #[test]
    fn generating_loads_no_image_and_sizes_itself() {
        let graph = Client::graph_for(&request("a fox"), &checkpoint(), 1, &[]);
        assert!(graph.get("load0").is_none(), "nothing to load when generating");
        assert!(graph.get("size").is_none());
        // Dimensions are literals, not wired from an image.
        assert_eq!(graph["latent"]["inputs"]["width"], 1024);
    }

    /// The wiring most likely to be got wrong, and the reason it is tested:
    /// the encoded source attaches to the negative branch as well as the
    /// positive. It looks redundant; the Flux.2 edit blueprint does it because
    /// the negative branch must denoise the same picture for the guidance
    /// difference to mean anything.
    #[test]
    fn editing_conditions_both_branches_on_the_source() {
        let graph = Client::graph_for(&request("make it blue"), &checkpoint(), 1,
                                      &["cat.png".to_string()]);

        assert_eq!(graph["load0"]["inputs"]["image"], "cat.png");
        assert_eq!(graph["encode0"]["inputs"]["pixels"], json!(["scale0", 0]));

        assert_eq!(graph["pos_ref0"]["inputs"]["conditioning"], json!(["pos", 0]));
        assert_eq!(graph["neg_ref0"]["inputs"]["conditioning"], json!(["neg", 0]));
        // Both reference the same encoded latent.
        assert_eq!(graph["pos_ref0"]["inputs"]["latent"], json!(["encode0", 0]));
        assert_eq!(graph["neg_ref0"]["inputs"]["latent"], json!(["encode0", 0]));

        // …and the guider takes the referenced conditioning, not the raw text.
        assert_eq!(graph["guider"]["inputs"]["positive"], json!(["pos_ref0", 0]));
        assert_eq!(graph["guider"]["inputs"]["negative"], json!(["neg_ref0", 0]));
    }

    #[test]
    fn an_edit_keeps_the_sources_shape_by_default() {
        let graph = Client::graph_for(&request("brighter"), &checkpoint(), 1,
                                      &["cat.png".to_string()]);
        // Read off the server rather than guessed here.
        assert_eq!(graph["size"]["inputs"]["image"], json!(["scale0", 0]));
        assert_eq!(graph["latent"]["inputs"]["width"], json!(["size", 0]));
        assert_eq!(graph["sigmas"]["inputs"]["height"], json!(["size", 1]));
    }

    #[test]
    fn asking_for_an_aspect_reframes_the_edit() {
        let req = ImageRequest {
            aspect: Some(crate::provider::Aspect::parse("16:9").unwrap()),
            ..request("brighter")
        };
        let graph = Client::graph_for(&req, &checkpoint(), 1, &["cat.png".to_string()]);
        // An explicit request wins over the source's shape, so no GetImageSize.
        assert!(graph.get("size").is_none());
        assert_eq!(graph["latent"]["inputs"]["width"], 1024);
        assert_eq!(graph["latent"]["inputs"]["height"], 576);
    }

    /// More than one reference chains, each adding its latent to the
    /// conditioning the previous one produced.
    #[test]
    fn references_chain_in_order() {
        let graph = Client::graph_for(
            &request("combine these"),
            &checkpoint(),
            1,
            &["a.png".to_string(), "b.png".to_string()],
        );
        assert_eq!(graph["pos_ref0"]["inputs"]["conditioning"], json!(["pos", 0]));
        assert_eq!(
            graph["pos_ref1"]["inputs"]["conditioning"],
            json!(["pos_ref0", 0])
        );
        assert_eq!(graph["pos_ref1"]["inputs"]["latent"], json!(["encode1", 0]));
        assert_eq!(graph["guider"]["inputs"]["positive"], json!(["pos_ref1", 0]));
        // Dimensions still come from the first image, not the last.
        assert_eq!(graph["size"]["inputs"]["image"], json!(["scale0", 0]));
    }

    #[test]
    fn credentials_are_lifted_out_of_the_url() {
        let (base, auth) = split_credentials("https://bob:hunter2@comfy.example/comfyui");
        // The password must not survive into the string that error messages print.
        assert_eq!(base, "https://comfy.example/comfyui");
        assert!(!base.contains("hunter2"));
        assert_eq!(auth.unwrap(), format!("Basic {}", STANDARD.encode("bob:hunter2")));
    }

    #[test]
    fn urls_without_credentials_are_left_alone() {
        let (base, auth) = split_credentials("http://127.0.0.1:8188");
        assert_eq!(base, "http://127.0.0.1:8188");
        assert!(auth.is_none());
    }

    /// An `@` in the path is ordinary data, not a credential separator.
    #[test]
    fn an_at_sign_in_the_path_is_not_a_credential() {
        let (base, auth) = split_credentials("https://comfy.example/users/@bob");
        assert_eq!(base, "https://comfy.example/users/@bob");
        assert!(auth.is_none());
    }

    #[test]
    fn auth_accepts_the_three_forms_people_actually_have() {
        // A complete header value passes through untouched…
        assert_eq!(auth_header("Bearer eyJhbGci"), "Bearer eyJhbGci");
        // …including Basic, whose base64 payload may itself contain a colon.
        assert_eq!(auth_header("Basic dXNlcjpwdw=="), "Basic dXNlcjpwdw==");
        // user:password is encoded — the Caddy basic_auth case.
        assert_eq!(
            auth_header("bob:hunter2"),
            format!("Basic {}", STANDARD.encode("bob:hunter2"))
        );
        // A bare token becomes a Bearer.
        assert_eq!(auth_header("  sk-abc123  "), "Bearer sk-abc123");
    }

    /// The bug this replaced: a 401 was reported as "ComfyUI has no `UNETLoader`
    /// node … this build may be too old", which is the opposite of the truth.
    #[test]
    fn refusals_are_told_apart_from_a_missing_node() {
        let anonymous = explain_refusal(401, &None).expect("401 must be explained");
        assert!(anonymous.contains("requires authentication"));
        assert!(anonymous.contains("LUCIDA_COMFYUI_AUTH"));

        let credentialed =
            explain_refusal(401, &Some("Basic x".into())).expect("401 must be explained");
        assert!(credentialed.contains("rejected the credentials"));

        // Anything else is left to the caller's own diagnosis.
        assert!(explain_refusal(404, &None).is_none());
        assert!(explain_refusal(400, &None).is_none());
    }
}
