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
    /// Nothing embedded — verified, not assumed.
    Unmarked,
}

impl Provenance {
    pub fn describe(self) -> &'static str {
        match self {
            Self::SynthIdAndC2pa => "invisible SynthID watermark + C2PA manifest",
            Self::Unmarked => "no watermark or provenance manifest",
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
    pub aspect: AspectSupport,
    pub seed: bool,
    pub negative_prompt: bool,
    pub references: bool,
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

        if req.seed.is_some() && !self.seed {
            bail!(
                "`{me}` has no concept of a seed, so `--seed` cannot be honoured.\n\n\
                 Google never exposes one, which means results there are not \
                 reproducible by any means. Use the `comfyui` provider (a local \
                 model, e.g. `--model klein`) when you need to render the same \
                 image twice."
            );
        }

        if req.negative_prompt.is_some() && !self.negative_prompt {
            bail!(
                "`{me}` does not accept a negative prompt for images.\n\n\
                 Describe what you do want instead — Gemini responds to positive \
                 description far better than to exclusions — or use the `comfyui` \
                 provider, where a negative prompt is a real conditioning input."
            );
        }

        if req.steps.is_some() && !self.steps {
            bail!(
                "`{me}` does not expose a step count; the model decides how long to \
                 sample. `--steps` applies to the `comfyui` provider."
            );
        }

        if req.guidance.is_some() && !self.guidance {
            bail!(
                "`{me}` does not expose a guidance scale. `--guidance` applies to \
                 the `comfyui` provider."
            );
        }

        if !req.references.is_empty() && !self.references {
            bail!(
                "`{me}` cannot condition on reference images, so there is nothing \
                 for it to edit.\n\n\
                 Both providers currently support editing, so reaching this means \
                 a provider was configured that does not. Generate from a prompt \
                 instead, or use `google` or `comfyui`."
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
}

/// A source of images.
pub trait ImageProvider {
    fn capabilities(&self) -> Capabilities;

    fn generate(&self, req: &ImageRequest) -> Result<GeneratedImage>;

    /// Models this provider can actually reach right now, for `lucida models`.
    fn list_models(&self) -> Result<Vec<String>>;
}

/// What a backend supports, without constructing one.
///
/// This is what lets `--seed` against Google report "google has no concept of a
/// seed" rather than "no API key found". The second message is true and useless:
/// supplying a key would not have helped.
pub fn capabilities_for(backend: Backend) -> Capabilities {
    match backend {
        Backend::Google => crate::genai::CAPABILITIES,
        Backend::ComfyUi => crate::comfy::CAPABILITIES,
    }
}

/// Which backend serves a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Google,
    ComfyUi,
}

impl Backend {
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "google" | "gemini" => Ok(Self::Google),
            "comfyui" | "comfy" | "local" => Ok(Self::ComfyUi),
            other => bail!("unknown provider `{other}`. Known providers: google, comfyui"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::ComfyUi => "comfyui",
        }
    }
}

/// Guesses the backend from a model id, so `--provider` stays optional.
///
/// The alias tables already carry the answer for every name Lucida ships. An
/// unrecognised id falls to Google, which keeps every existing invocation working
/// and means a model id released tomorrow still reaches the API it belongs to.
pub fn infer_backend(model: &str) -> Backend {
    let key = model.trim().to_ascii_lowercase();
    if crate::comfy::MODEL_ALIASES.iter().any(|(a, _)| *a == key)
        || key.ends_with(".safetensors")
        || key.ends_with(".gguf")
        || key.contains("flux")
    {
        Backend::ComfyUi
    } else {
        Backend::Google
    }
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
        let caps = Capabilities {
            provider: "google",
            aspect: AspectSupport::Named(&["1:1"]),
            seed: false,
            negative_prompt: false,
            references: true,
            steps: false,
            guidance: false,
            provenance: Provenance::SynthIdAndC2pa,
        };
        let req = ImageRequest {
            seed: Some(7),
            ..Default::default()
        };
        let error = caps.check(&req).unwrap_err().to_string();
        assert!(error.contains("comfyui"), "the message must name a way forward: {error}");
    }

    #[test]
    fn backends_are_inferred_from_the_model_id() {
        assert_eq!(infer_backend("banana"), Backend::Google);
        assert_eq!(infer_backend("gemini-3.1-flash-image"), Backend::Google);
        assert_eq!(infer_backend("klein"), Backend::ComfyUi);
        assert_eq!(infer_backend("some-model.safetensors"), Backend::ComfyUi);
        // Unknown ids fall to Google so new Gemini models work the day they ship.
        assert_eq!(infer_backend("gemini-9-image"), Backend::Google);
    }
}
