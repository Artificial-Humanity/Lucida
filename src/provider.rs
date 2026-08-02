//! What every image provider has in common, and what it does not.
//!
//! Lucida spoke only to Google for its first release, and `ImageRequest` quietly
//! encoded Google's answer to "what is an image request": named aspect ratios,
//! `1K`/`2K`/`4K` sizes, no seed, no negative prompt. A second provider disagrees
//! about all four.
//!
//! The resolution here is deliberately not a union type pretending to be a common
//! interface. It is three separate moves:
//!
//! 1. **Normalize what genuinely maps.** Aspect ratio and size are held as a
//!    ratio and a target long edge, which both providers can express — Google as
//!    its named strings, ComfyUI as pixel dimensions.
//! 2. **Declare what does not, and fail before spending anything.** A provider
//!    publishes [`Capabilities`], and a request carrying a parameter the provider
//!    cannot honour is rejected up front with a message naming one that can.
//!    Silently dropping the parameter is the failure mode worth ruling out: the
//!    user asked for a seed and got an unrepeatable image, with nothing said.
//! 3. **Report what differs downstream.** Provenance is not a request parameter
//!    but it varies per provider, and callers deserve to know which they got.

use anyhow::{Result, bail};
use std::fmt;

/// An image request, in terms every provider can be asked to interpret.
///
/// Fields past `references` are the ones providers disagree about. Each is
/// optional, and each is guarded by [`Capabilities::check`] rather than being
/// quietly ignored by a provider that has no such concept.
#[derive(Debug, Clone, Default)]
pub struct ImageRequest {
    pub prompt: String,
    pub model: String,
    pub aspect: Option<Aspect>,
    pub size: Option<Size>,
    /// Existing images to condition on. Supplying any turns this into an edit.
    pub references: Vec<String>,
    pub negative_prompt: Option<String>,
    /// A mask naming which part of the first reference to change.
    ///
    /// The roadmap predicted from the beginning that `references: Vec<String>`
    /// could not express "this region of this image", and it was right — it
    /// survived ComfyUI editing and BFL editing because both change the whole
    /// picture. OpenAI is where it stopped being deferrable.
    ///
    /// A raster image rather than a rectangle or a polygon, because that is what
    /// every provider that supports masking actually takes.
    pub mask: Option<String>,
    /// A provider-native workflow file to render with, instead of the built-in
    /// graph.
    ///
    /// The typed escape hatch the roadmap held in reserve for "genuinely
    /// provider-specific parameters", and the first thing to need it. A ComfyUI
    /// workflow is not a parameter any other provider could interpret, and
    /// pretending otherwise would mean inventing a graph format nobody speaks.
    pub workflow: Option<String>,
    pub seed: Option<u64>,
    pub steps: Option<u32>,
    pub guidance: Option<f32>,
}

#[derive(Debug)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    /// Models often narrate what they drew; worth surfacing, never required.
    pub commentary: Option<String>,
    /// The seed actually used, when the provider has the concept. Providers that
    /// pick one at random should report it, so a result can be reproduced even
    /// though the request did not pin it.
    pub seed: Option<u64>,
}

/// An aspect ratio, kept as the pair it was written as rather than a float, so
/// `16:9` can be handed back to a provider that wants exactly that string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aspect {
    pub w: u32,
    pub h: u32,
}

impl Aspect {
    pub fn parse(text: &str) -> Result<Self> {
        let (w, h) = text
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("aspect ratio `{text}` is not in W:H form, e.g. 16:9"))?;
        let parse = |part: &str, which| -> Result<u32> {
            part.trim()
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| anyhow::anyhow!("the {which} of aspect ratio `{text}` is not a positive whole number"))
        };
        Ok(Self {
            w: parse(w, "width")?,
            h: parse(h, "height")?,
        })
    }

    fn ratio(self) -> f64 {
        f64::from(self.w) / f64::from(self.h)
    }
}

impl fmt::Display for Aspect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.w, self.h)
    }
}

/// A target size, as the long edge in pixels.
///
/// Google names three tiers; everyone else takes pixel dimensions. Holding the
/// pixel count and naming the tiers on top means both readings are available
/// without either provider having to understand the other's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size(pub u32);

impl Size {
    pub const ONE_K: Size = Size(1024);
    pub const TWO_K: Size = Size(2048);
    pub const FOUR_K: Size = Size(4096);

    /// Accepts either a Google tier name (`2K`) or a plain pixel count (`1536`).
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim().to_ascii_uppercase().as_str() {
            "1K" => Ok(Self::ONE_K),
            "2K" => Ok(Self::TWO_K),
            "4K" => Ok(Self::FOUR_K),
            other => other
                .parse::<u32>()
                .ok()
                .filter(|n| (16..=16384).contains(n))
                .map(Size)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "size `{text}` is neither a tier (1K, 2K, 4K) nor a pixel \
                         count between 16 and 16384"
                    )
                }),
        }
    }

    /// The nearest Google tier name, for the provider that only speaks those.
    pub fn tier_name(self) -> &'static str {
        match self.0 {
            n if n <= 1536 => "1K",
            n if n <= 3072 => "2K",
            _ => "4K",
        }
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}px", self.0)
    }
}

impl ImageRequest {
    /// Resolves aspect and size into concrete pixel dimensions, for the providers
    /// that take them.
    ///
    /// `multiple_of` exists because latent-space models cannot render arbitrary
    /// dimensions — Flux works in units of 16 pixels — so rounding belongs here
    /// rather than in each provider. The long edge is what the size names, and
    /// the short edge follows from the ratio.
    pub fn pixels(&self, default: (u32, u32), multiple_of: u32) -> (u32, u32) {
        let (w, h) = match (self.aspect, self.size) {
            (None, None) => default,
            (None, Some(size)) => {
                // No ratio asked for: keep the default's shape, scale to the size.
                let long = default.0.max(default.1).max(1);
                let scale = f64::from(size.0) / f64::from(long);
                (
                    (f64::from(default.0) * scale) as u32,
                    (f64::from(default.1) * scale) as u32,
                )
            }
            (Some(aspect), size) => {
                let long = size.unwrap_or(Size(default.0.max(default.1))).0;
                if aspect.ratio() >= 1.0 {
                    (long, (f64::from(long) / aspect.ratio()) as u32)
                } else {
                    ((f64::from(long) * aspect.ratio()) as u32, long)
                }
            }
        };
        (round_to(w, multiple_of), round_to(h, multiple_of))
    }
}

fn round_to(value: u32, multiple: u32) -> u32 {
    if multiple <= 1 {
        return value.max(1);
    }
    let rounded = ((value + multiple / 2) / multiple) * multiple;
    rounded.max(multiple)
}

/// How a provider expresses aspect ratio, which decides what it can be asked for.
#[derive(Debug, Clone, Copy)]
pub enum AspectSupport {
    /// Only these exact ratios, and nothing between them.
    Named(&'static [&'static str]),
    /// Any ratio, subject to rounding to `multiple_of` pixels.
    Free { multiple_of: u32 },
}

/// What a provider embeds in its output.
///
/// Not a request parameter, but it varies per provider and it is the kind of
/// difference users find out about by grepping the bytes, which is how we found
/// out. Reporting it is cheaper than that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// An invisible SynthID watermark plus a C2PA manifest asserting
    /// `trainedAlgorithmicMedia`. Verified in Google's raw bytes; there is no
    /// opt-out on any tier. Paid tiers remove only the *visible* glyph.
    SynthIdAndC2pa,
    /// A signed C2PA manifest and **no** pixel watermark.
    ///
    /// The distinction from `SynthIdAndC2pa` is not pedantry, it is the whole
    /// practical difference: C2PA rides in metadata and any re-encode drops it,
    /// while SynthID is in the pixels and is built to survive one. So this
    /// output is marked *removably*, and Google's is not.
    ///
    /// Verified in a real render: a `caBX` chunk naming `Black Forest Labs API`
    /// as claim generator, `FLUX.2` as software agent, and asserting
    /// `digitalSourceType: trainedAlgorithmicMedia` — with no SynthID anywhere.
    C2paOnly,
    /// Nothing embedded — verified, not assumed.
    Unmarked,
    /// Nobody has looked yet.
    ///
    /// Deliberately distinct from `Unmarked`. Every other variant here was
    /// established by grepping real output, and "we checked and found nothing"
    /// is a materially different claim from "we assume nothing is there" — the
    /// second is the kind of thing people repeat until it becomes folklore.
    ///
    /// Currently unused, and kept anyway: it is where a new provider starts
    /// before anyone has rendered anything with it, and BFL is the case in
    /// point. It shipped as `Unverified`, one render proved it was
    /// [`Self::C2paOnly`], and the guess most people would have made — unmarked,
    /// like other non-Google generators — was wrong.
    #[allow(dead_code, reason = "the starting state for the next provider added")]
    Unverified,
}

impl Provenance {
    pub fn describe(self) -> &'static str {
        match self {
            Self::SynthIdAndC2pa => "invisible SynthID watermark + C2PA manifest",
            Self::C2paOnly => "C2PA manifest only — no pixel watermark, so a re-encode removes it",
            Self::Unmarked => "no watermark or provenance manifest",
            Self::Unverified => "unverified — nobody has checked this provider's output",
        }
    }
}

/// What a provider can actually be asked for.
///
/// The point of publishing this is `check`: a parameter the provider cannot
/// honour becomes an error naming a provider that can, before anything is spent.
///
/// Deliberately a plain value each provider declares as a constant, rather than
/// something only a live client can answer. Whether Google has a seed is not a
/// fact about your credentials, and finding out should not require having any —
/// see [`capabilities_for`].
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub provider: &'static str,
    /// One line on why a caller would choose this provider.
    ///
    /// Lives beside the measured capabilities rather than in the MCP schema
    /// because that is where it stays true: the schema's prose was hand-written
    /// while its enum was generated, and by the fourth provider it claimed there
    /// were two, listed three, and omitted the fourth entirely. An agent reads
    /// that and believes it.
    pub tagline: &'static str,
    pub aspect: AspectSupport,
    /// Whether the output size can be chosen at all.
    ///
    /// A late addition, and a reminder that "what varies between providers" is
    /// not knowable up front: Stability exposes `aspect_ratio` and *no*
    /// dimensions whatsoever, so `--size` there is not a value out of range but
    /// a concept the API does not have. Without this it was silently dropped.
    pub size: bool,
    pub seed: bool,
    pub negative_prompt: bool,
    pub references: bool,
    /// Whether the provider *accepts* a mask naming where to concentrate an edit.
    ///
    /// Deliberately not "restricts an edit to part of the image", which is what
    /// this said until it was measured. On `gpt-image-1.5` a mask is **advisory**:
    /// asking for a change in a lower-right box produced a mean difference of
    /// 58/255 inside it and 29/255 outside — twice the change where asked for,
    /// and the rest of the picture regenerated anyway, losing an object that was
    /// nowhere near the mask.
    ///
    /// So this is a real capability and a weaker one than the name suggests.
    /// Anyone needing pixels outside the mask preserved has to composite the
    /// result back over the original themselves.
    pub mask: bool,
    /// Whether the provider can render a caller-supplied workflow.
    pub workflow: bool,
    pub steps: bool,
    pub guidance: bool,
    pub provenance: Provenance,
}

impl Capabilities {
    /// Rejects a request carrying anything this provider cannot express.
    ///
    /// Every message names an alternative, because "unsupported" without a way
    /// forward just moves the search to the documentation. This is the same shape
    /// as the `veo-lite` negative-prompt guard, which earned its keep.
    pub fn check(&self, req: &ImageRequest) -> Result<()> {
        let me = self.provider;

        if req.size.is_some() && !self.size {
            bail!(
                "`{me}` does not let you choose the output size, so `--size` cannot \
                 be honoured.\n\n\
                 The size is fixed by the provider and follows from the shape you \
                 ask for — `--aspect 16:9` on stability returns 2016x1152, for \
                 instance. Use `--aspect` to control the shape, and `comfyui` or \
                 `bfl` if the pixel count itself matters. Lucida reports the size \
                 it actually wrote."
            );
        }

        if req.seed.is_some() && !self.seed {
            bail!(
                "`{me}` has no concept of a seed, so `--seed` cannot be honoured.\n\n\
                 Google never exposes one, which means results there are not \
                 reproducible by any means. Use `comfyui` (a local model, e.g. \
                 `--model klein`) or `bfl` when you need to render the same image \
                 twice."
            );
        }

        if req.negative_prompt.is_some() && !self.negative_prompt {
            // The remedy differs by provider, so it cannot be one sentence. Telling
            // a BFL user that "Gemini responds better to positive description" is
            // the kind of near-miss advice that wastes more time than silence.
            let remedy = match me {
                "google" => {
                    "Describe what you do want instead — Gemini responds to positive \
                     description far better than to exclusions."
                }
                "bfl" => {
                    "No FLUX endpoint accepts one — not flux-2-*, not flux-dev, not \
                     flux-pro-1.1. That is a limit of the hosted API rather than of \
                     Lucida: the local lane has a negative prompt only because \
                     ComfyUI builds the graph and can wire the conditioning itself."
                }
                _ => "This provider exposes no negative conditioning.",
            };
            bail!(
                "`{me}` does not accept a negative prompt for images.\n\n{remedy}\n\n\
                 Use the `comfyui` provider if you need one — there it is a real \
                 conditioning input."
            );
        }

        if req.steps.is_some() && !self.steps {
            bail!("{}", self.no_sampler("--steps", "a step count"));
        }

        if req.guidance.is_some() && !self.guidance {
            bail!("{}", self.no_sampler("--guidance", "a guidance scale"));
        }

        if req.workflow.is_some() && !self.workflow {
            bail!(
                "`{me}` has no workflow format, so `--workflow` cannot be \
                 honoured.\n\n\
                 A workflow is a provider-native description of how to render — \
                 only `comfyui` has one, because only there does Lucida build a \
                 graph rather than fill in a request."
            );
        }

        if req.mask.is_some() {
            if !self.mask {
                bail!(
                    "`{me}` does not accept a mask, so `--mask` cannot be \
                     honoured.\n\n\
                     Use the `openai` provider, which does — though note that even \
                     there a mask is advisory: it concentrates the change without \
                     guaranteeing the rest of the picture survives. Measured at \
                     roughly twice the change inside the mask as outside."
                );
            }
            if req.references.is_empty() {
                bail!(
                    "`--mask` names which part of an image to change, but no image \
                     was given to change.\n\n\
                     Use `lucida edit <image> <prompt> --mask <mask.png>`."
                );
            }
        }

        if !req.references.is_empty() && !self.references {
            // The editors are computed, not listed: a hand-written list here
            // claimed "both providers" long after there were five.
            let editors: Vec<&str> = Backend::ALL
                .iter()
                .filter(|b| capabilities_for(**b, b.default_model()).references)
                .map(|b| b.name())
                .collect();
            bail!(
                "`{me}` cannot condition on reference images, so there is nothing \
                 for it to edit.\n\n\
                 Generate from a prompt instead, or edit with one of: {}.",
                editors.join(", ")
            );
        }

        if let (Some(aspect), AspectSupport::Named(allowed)) = (req.aspect, self.aspect) {
            let asked = aspect.to_string();
            if !allowed.contains(&asked.as_str()) {
                bail!(
                    "`{me}` supports only these aspect ratios: {}.\n\n\
                     `{asked}` is not among them. Either pick the nearest, or use \
                     the `comfyui` provider, which takes free dimensions.",
                    allowed.join(", ")
                );
            }
        }

        Ok(())
    }

    /// The message for a sampler control this provider will not accept.
    ///
    /// Split out because BFL made it a per-*model* fact rather than a
    /// per-provider one: `flux-2-flex` exposes the sampler and `flux-2-pro` does
    /// not, so saying "bfl does not expose a step count" would be false and would
    /// send someone away from a provider that could have served them.
    fn no_sampler(&self, flag: &str, what: &str) -> String {
        match self.provider {
            "bfl" => format!(
                "this FLUX model does not expose {what}, so `{flag}` cannot be \
                 honoured.\n\n\
                 Within Black Forest Labs only `flux-2-flex` and `flux-dev` do — try \
                 `--model flux-2-flex`. The others decide sampling for themselves. \
                 `lucida models --provider bfl` marks which is which."
            ),
            "google" => format!(
                "`google` does not expose {what}; the model decides how to sample.\n\n\
                 `{flag}` applies to `comfyui`, and to `bfl` on `flux-2-flex` or \
                 `flux-dev`."
            ),
            other => format!("`{other}` does not expose {what}, so `{flag}` cannot be honoured."),
        }
    }
}

/// A source of images.
pub trait ImageProvider {
    fn capabilities(&self) -> Capabilities;

    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage>;

    /// Models this provider can actually reach right now, for `lucida models`.
    fn list_models(&self) -> Result<Vec<String>>;
}

/// What a backend supports for a given model, without constructing one.
///
/// This is what lets `--seed` against Google report "google has no concept of a
/// seed" rather than "no API key found". The second message is true and useless:
/// supplying a key would not have helped.
///
/// Takes the model as well as the backend because BFL forced the issue — within
/// one provider, `steps` exists on `flux-2-flex` and not on `flux-2-pro`. Until
/// then a provider had one answer for everyone, and publishing the union would
/// have advertised parameters that some endpoints silently ignore.
pub fn capabilities_for(backend: Backend, model: &str) -> Capabilities {
    match backend {
        Backend::Google => crate::genai::CAPABILITIES,
        Backend::ComfyUi => crate::comfy::CAPABILITIES,
        Backend::Bfl => crate::bfl::capabilities(model),
        Backend::Stability => crate::stability::capabilities(model),
        Backend::OpenAi => crate::openai::capabilities(model),
    }
}

/// Which backend serves a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Google,
    ComfyUi,
    Bfl,
    Stability,
    OpenAi,
}

impl Backend {
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "google" | "gemini" => Ok(Self::Google),
            "comfyui" | "comfy" | "local" => Ok(Self::ComfyUi),
            "bfl" | "flux" | "blackforestlabs" => Ok(Self::Bfl),
            "stability" | "stabilityai" | "sai" => Ok(Self::Stability),
            "openai" | "oai" | "gpt" => Ok(Self::OpenAi),
            other => bail!(
                "unknown provider `{other}`. Known providers: {}",
                // Generated, so a sixth provider cannot be missing from it —
                // the way openai was missing from the hand-written version.
                Backend::ALL
                    .iter()
                    .map(|b| b.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::ComfyUi => "comfyui",
            Self::Bfl => "bfl",
            Self::Stability => "stability",
            Self::OpenAi => "openai",
        }
    }

    /// The model used when none is named. Lives here rather than in `main` so
    /// anything asking "what can this provider do" gets the same answer the CLI
    /// would give — asking BFL with an empty model reports no editing, because
    /// an empty string is not a FLUX.2 endpoint.
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Google => crate::genai::DEFAULT_MODEL,
            Self::ComfyUi => "klein",
            Self::Bfl => crate::bfl::DEFAULT_MODEL,
            Self::Stability => crate::stability::DEFAULT_MODEL,
            Self::OpenAi => crate::openai::DEFAULT_MODEL,
        }
    }

    pub const ALL: &'static [Backend] = &[
        Backend::Google,
        Backend::ComfyUi,
        Backend::Bfl,
        Backend::Stability,
        Backend::OpenAi,
    ];
}

/// Guesses the backend from a model id, so `--provider` stays optional.
///
/// Order matters here, and it is the fiddly part. A local checkpoint file and a
/// hosted endpoint can both be called something-flux, so exact aliases are
/// checked before any pattern, and a filename extension decides before a name
/// does. An unrecognised id falls to Google, which keeps every existing
/// invocation working and means a Gemini model released tomorrow works today.
pub fn infer_backend(model: &str) -> Backend {
    let key = model.trim().to_ascii_lowercase();

    if crate::comfy::MODEL_ALIASES.iter().any(|(a, _)| *a == key) {
        return Backend::ComfyUi;
    }
    if crate::stability::MODEL_ALIASES.iter().any(|(a, _)| *a == key)
        || crate::stability::KNOWN_MODELS.contains(&key.as_str())
        || crate::stability::SD3_VARIANTS.contains(&key.as_str())
    {
        return Backend::Stability;
    }
    if crate::openai::MODEL_ALIASES.iter().any(|(a, _)| *a == key)
        || crate::openai::KNOWN_MODELS.contains(&key.as_str())
    {
        return Backend::OpenAi;
    }
    if crate::bfl::MODEL_ALIASES.iter().any(|(a, _)| *a == key)
        || crate::bfl::KNOWN_MODELS.contains(&key.as_str())
    {
        return Backend::Bfl;
    }
    // A checkpoint file is unambiguously something ComfyUI loads.
    if key.ends_with(".safetensors") || key.ends_with(".gguf") {
        return Backend::ComfyUi;
    }
    // Anything else shaped like a BFL endpoint path — this is what lets a model
    // released tomorrow reach the right provider without a code change.
    if key.starts_with("flux-") {
        return Backend::Bfl;
    }

    Backend::Google
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_round_trips_through_its_written_form() {
        assert_eq!(Aspect::parse("16:9").unwrap().to_string(), "16:9");
        assert!(Aspect::parse("16x9").is_err());
        assert!(Aspect::parse("16:0").is_err());
    }

    #[test]
    fn size_accepts_tiers_and_raw_pixels() {
        assert_eq!(Size::parse("2k").unwrap(), Size::TWO_K);
        assert_eq!(Size::parse("1536").unwrap(), Size(1536));
        assert!(Size::parse("8").is_err());
    }

    #[test]
    fn pixels_put_the_named_size_on_the_long_edge() {
        let req = ImageRequest {
            aspect: Some(Aspect::parse("16:9").unwrap()),
            size: Some(Size::ONE_K),
            ..Default::default()
        };
        assert_eq!(req.pixels((1024, 1024), 16), (1024, 576));

        let portrait = ImageRequest {
            aspect: Some(Aspect::parse("9:16").unwrap()),
            size: Some(Size::ONE_K),
            ..Default::default()
        };
        assert_eq!(portrait.pixels((1024, 1024), 16), (576, 1024));
    }

    #[test]
    fn pixels_round_to_the_latent_grid() {
        // 3:2 at 1K is 682.66 on the short edge, which Flux cannot render.
        let req = ImageRequest {
            aspect: Some(Aspect::parse("3:2").unwrap()),
            size: Some(Size::ONE_K),
            ..Default::default()
        };
        let (w, h) = req.pixels((1024, 1024), 16);
        assert_eq!((w, h), (1024, 688));
        assert_eq!(h % 16, 0);
    }

    #[test]
    fn an_empty_request_gets_the_providers_default() {
        let req = ImageRequest::default();
        assert_eq!(req.pixels((1024, 1024), 16), (1024, 1024));
    }

    #[test]
    fn check_rejects_a_seed_the_provider_cannot_honour() {
        // Derived from a real provider rather than spelled out: a literal here
        // has broken three times now, once per capability added, which is noise
        // that says nothing about the behaviour under test.
        let caps = Capabilities {
            seed: false,
            ..crate::comfy::CAPABILITIES
        };
        let req = ImageRequest {
            seed: Some(7),
            ..Default::default()
        };
        let error = caps.check(&req).unwrap_err().to_string();
        assert!(error.contains("comfyui"), "the message must name a way forward: {error}");
    }

    /// Stability is the only provider with no size control, and `--size` there
    /// would otherwise be silently dropped — the failure this design exists to
    /// prevent, found only because the API turned out to have no width or height
    /// field at all.
    #[test]
    fn a_provider_without_size_control_rejects_size() {
        let caps = capabilities_for(Backend::Stability, "core");
        assert!(!caps.size);
        let req = ImageRequest {
            size: Some(Size::TWO_K),
            ..Default::default()
        };
        let error = caps.check(&req).unwrap_err().to_string();
        assert!(error.contains("--size"));
        assert!(error.contains("--aspect"), "must name what it does support");

        // The others all take one.
        for backend in [Backend::Google, Backend::ComfyUi, Backend::Bfl] {
            assert!(capabilities_for(backend, "").size, "{backend:?} should take a size");
        }
    }

    #[test]
    fn backends_are_inferred_from_the_model_id() {
        assert_eq!(infer_backend("banana"), Backend::Google);
        // All three Stability endpoints, not just the two that happen to be
        // aliases — `core` used to fall through to Google and 404 there.
        assert_eq!(infer_backend("core"), Backend::Stability);
        assert_eq!(infer_backend("ultra"), Backend::Stability);
        // The sd3 variant spellings too, now that they are reachable model ids.
        assert_eq!(infer_backend("sd3.5-flash"), Backend::Stability);
        assert_eq!(infer_backend("gpt-image-2"), Backend::OpenAi);
        assert_eq!(infer_backend("gemini-3.1-flash-image"), Backend::Google);
        assert_eq!(infer_backend("klein"), Backend::ComfyUi);
        assert_eq!(infer_backend("some-model.safetensors"), Backend::ComfyUi);
        assert_eq!(infer_backend("flux-2-pro"), Backend::Bfl);
        assert_eq!(infer_backend("flux-max"), Backend::Bfl);
        // Unknown ids fall to Google so new Gemini models work the day they ship.
        assert_eq!(infer_backend("gemini-9-image"), Backend::Google);
        // …and an unknown flux-shaped id reaches BFL for the same reason.
        assert_eq!(infer_backend("flux-3-pro"), Backend::Bfl);
    }

    /// The ambiguity worth pinning down: local checkpoints and hosted endpoints
    /// both get called something-flux, and picking wrong sends a paid request to
    /// the wrong place — or a local filename to a billing API.
    #[test]
    fn local_and_hosted_flux_names_do_not_collide() {
        // Bare family aliases are the local lane.
        assert_eq!(infer_backend("flux2"), Backend::ComfyUi);
        assert_eq!(infer_backend("flux-2"), Backend::ComfyUi);
        assert_eq!(infer_backend("flux2-klein"), Backend::ComfyUi);
        // Endpoint-shaped names are hosted.
        assert_eq!(infer_backend("flux-2-klein-9b"), Backend::Bfl);
        // A file is always local, whatever it is called.
        assert_eq!(infer_backend("flux-2-pro.safetensors"), Backend::ComfyUi);
    }
}
