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

/// Whether a provider takes a mask, and whether the mask is a promise.
///
/// This was a `bool` until the review after v0.9.0, and that review is the whole
/// argument for the type. Both masking providers were `mask: true`, so the
/// difference between them — one concentrates a change, the other guarantees the
/// rest of the picture survives — could only live in prose. It lived in prose on
/// seven surfaces: the `--mask` help, `lucida models`, two MCP descriptions, the
/// `image_providers` probe, this module's own refusal text, and the shipped
/// skill. When the local lane learned to bind, exactly one of the seven was
/// updated, and the probe an agent is told to believe was among the six that
/// were not.
///
/// So the distinction is a value now, and every surface reporting it reads
/// [`Self::describe`] or [`mask_semantics`]. The same move [`Provenance`] made,
/// for the same reason: a capability that varies has to be a variant, or the
/// variation ends up in sentences nobody updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskSupport {
    /// No mask at all.
    No,
    /// Accepted, and it concentrates the change without confining it.
    ///
    /// Measured on `gpt-image-1.5`: asking for a change inside a lower-right box
    /// moved 58/255 inside it and 29/255 outside — twice the change where it was
    /// asked for, and the rest of the picture regenerated anyway, losing an
    /// object nowhere near the mask. `gpt-image-2` concentrates 4.5x rather than
    /// 2x, which is better and still not a guarantee.
    Advisory,
    /// Accepted, and pixels outside it come back unchanged.
    ///
    /// Only reachable where Lucida builds the render graph itself, and not for
    /// free even there: conditioning through `InpaintModelConditioning` alone is
    /// merely advisory, measured at 23.8/255 outside the mask. Compositing the
    /// render back through the same mask takes the outside to **0.00/255**, mean
    /// and max, through the release binary.
    Binding,
}

impl MaskSupport {
    /// Whether a mask can be passed at all.
    pub fn accepted(self) -> bool {
        !matches!(self, Self::No)
    }

    /// The adjective, for prose supplying its own sentence.
    pub fn kind(self) -> &'static str {
        match self {
            Self::No => "not accepted",
            Self::Advisory => "advisory",
            Self::Binding => "binding",
        }
    }

    /// What the caller has to do about it, which is the part that differs.
    pub fn guarantee(self) -> &'static str {
        match self {
            Self::No => "no mask can be passed",
            Self::Advisory => {
                "the change is concentrated but not confined, and the rest of the \
                 picture is regenerated too — composite over the original yourself \
                 if untouched pixels matter"
            }
            Self::Binding => {
                "Lucida composites the render back through the mask, so pixels \
                 outside it come back unchanged — measured at 0.00/255, which \
                 leaves nothing for the caller to composite"
            }
        }
    }

    /// One line for a capability listing.
    pub fn describe(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Advisory => "accepted, advisory — the change is concentrated, not confined",
            Self::Binding => "accepted, binding — pixels outside it come back unchanged",
        }
    }
}

/// The providers whose mask means a given thing.
pub fn mask_providers(kind: MaskSupport) -> Vec<&'static str> {
    Backend::ALL
        .iter()
        .filter(|b| capabilities_for(**b, b.default_model()).mask == kind)
        .map(|b| b.name())
        .collect()
}

/// Every provider that takes a mask, the ones that guarantee something first.
///
/// Ordered rather than alphabetical because this list is read as a remedy, and
/// the provider whose mask actually binds is the better answer to "where can I
/// mask instead".
pub fn mask_accepting_providers() -> Vec<&'static str> {
    let mut names = mask_providers(MaskSupport::Binding);
    names.extend(mask_providers(MaskSupport::Advisory));
    names
}

/// Masking across providers, in generated sentences.
///
/// The paragraph every agent-facing surface needs, and the reason it is computed:
/// each of those surfaces already generated its *provider list* while
/// hand-writing the word "advisory" beside it, so the list stayed true through
/// two new providers and the semantics went wrong the first time they forked.
pub fn mask_semantics() -> String {
    let mut parts = Vec::new();
    for kind in [MaskSupport::Binding, MaskSupport::Advisory] {
        let names = mask_providers(kind);
        if !names.is_empty() {
            parts.push(format!(
                "On {} the mask is {}: {}",
                join_and(&names),
                kind.kind(),
                kind.guarantee()
            ));
        }
    }

    if parts.is_empty() {
        return "No provider currently accepts a mask.".to_string();
    }
    format!("{}.", parts.join(". "))
}

/// A model whose provider has announced the date it stops working.
pub struct Retirement {
    /// Matched against the start of a model id, so one entry covers a family.
    pub prefix: &'static str,
    /// The date the provider published, as the provider wrote it.
    pub date: &'static str,
}

/// Every announced shutdown, in one place.
///
/// Declared rather than described, for the reason this codebase keeps
/// rediscovering: a date written into prose is a claim that goes false on a
/// schedule, silently, and in the six places nobody re-reads. Imagen's shutdown
/// was future-tense in five files at once; three OpenAI ids sat in `KNOWN_MODELS`
/// with their December date recorded nowhere at all, so `lucida models` listed
/// them exactly like the ones that will still exist next year.
///
/// Adding a row here annotates every surface that prints a model id, and
/// [`retirement_note`] gets the tense right on both sides of the date.
pub const RETIREMENTS: &[Retirement] = &[
    // Replaced by gemini-3.1-flash-image, which is already the default.
    Retirement { prefix: "imagen", date: "2026-08-17" },
    // Announced alongside gpt-image-2, which is already the default.
    Retirement { prefix: "gpt-image-1.5", date: "2026-12-01" },
    Retirement { prefix: "gpt-image-1-mini", date: "2026-12-01" },
    Retirement { prefix: "chatgpt-image-latest", date: "2026-12-01" },
];

/// "retires 2026-08-17" while it still works; "retired 2026-08-17" afterwards.
///
/// The tense is computed, not written, which is the whole point: the same string
/// stays true the day after the date it names.
pub fn retirement_note(model: &str) -> Option<String> {
    let retirement = RETIREMENTS.iter().find(|r| model.starts_with(r.prefix))?;
    let verb = if past(retirement.date) { "retired" } else { "retires" };
    Some(format!("{verb} {}", retirement.date))
}

/// Whether a `YYYY-MM-DD` date is behind us.
///
/// UTC, and by whole days: a model does not stop working at a moment this
/// process could know precisely, and being a few hours early or late with the
/// word "retired" costs nothing. An unparseable date reads as future, so a typo
/// here can only ever understate.
fn past(date: &str) -> bool {
    let Some(midnight) = unix_time(date) else {
        return false;
    };
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64 >= midnight)
        .unwrap_or(false)
}

/// `YYYY-MM-DD` to seconds since the epoch, at midnight UTC.
///
/// Hand-rolled rather than reached for: this is the only date arithmetic in the
/// program, and a date crate would be a runtime dependency bought for eleven
/// lines. The conversion is Howard Hinnant's `days_from_civil`, which is exact
/// for every proleptic Gregorian date and is the algorithm every date library
/// uses underneath.
fn unix_time(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Shift the year so it starts in March, which puts the leap day last and
    // makes the month-length pattern regular.
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    Some(days * 86_400)
}

/// `a`, `a and b`, `a, b and c` — the form a sentence needs rather than a table.
pub fn join_and(names: &[&str]) -> String {
    match names.split_last() {
        None => "no providers".to_string(),
        Some((last, [])) => (*last).to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
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
    /// Whether the provider accepts a mask naming where to concentrate an edit,
    /// and what that mask guarantees.
    ///
    /// Deliberately not a `bool`, which is what it was while the answer was
    /// "openai, and it is advisory". Two providers mask now and they mean
    /// different things by it — see [`MaskSupport`], which is also where the
    /// measurements live.
    pub mask: MaskSupport,
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
            if !self.mask.accepted() {
                // Both halves generated. The hand-written version of this named
                // only `openai` and called every mask advisory, which by v0.9.0
                // sent people away from the one provider whose mask binds.
                bail!(
                    "`{me}` does not accept a mask, so `--mask` cannot be \
                     honoured.\n\n\
                     Use one of: {}. What a mask guarantees differs between them, \
                     and that difference is usually the reason to prefer one:\n\n{}",
                    join_and(&mask_accepting_providers()),
                    mask_semantics()
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
    // No `capabilities()` here, deliberately, and it was removed rather than
    // never written: asking a *client* what its provider supports requires
    // having built one, which requires a credential, which is precisely the
    // coupling `capabilities_for` exists to break. The method survived because
    // its two callers already held a client — and both of them therefore gave
    // up before printing the table when no key was set. Use
    // `capabilities_for(backend, model)`; it needs neither.
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

    /// How this provider is named to someone who has not used Lucida yet.
    ///
    /// [`name`](Self::name) is the token `--provider` takes, and it is not the
    /// same thing: `bfl` is a flag value, while the product is called FLUX and
    /// nobody searching for it types the company's initials. This is the
    /// spelling the shopfront surfaces use — the package description, the
    /// `--help` banner — and it exists so those can be *checked* against
    /// `Backend::ALL` rather than hand-maintained.
    ///
    /// That check is owed: the repository description read "Generate and edit
    /// images with Google's Gemini models" for four providers and all of video,
    /// while being the only pitch a visitor ever saw.
    ///
    /// Test-only on purpose. Nothing Lucida *prints* should use these — output
    /// names the token you can type — so a production caller would be a sign the
    /// two vocabularies had started to blur. Its job is to be the list a prose
    /// surface can be measured against.
    #[cfg(test)]
    pub fn product_name(self) -> &'static str {
        match self {
            Self::Google => "Gemini",
            Self::ComfyUi => "ComfyUI",
            Self::Bfl => "FLUX",
            Self::Stability => "Stability",
            Self::OpenAi => "OpenAI",
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

    /// The claim `capabilities_for`'s own doc comment makes — that finding out
    /// whether Google has a seed should not require having a key — held for the
    /// function and not for the two commands that printed it. Both held a client
    /// first and gave up before the table when one could not be built.
    ///
    /// The trait no longer offers a `capabilities()` at all, so the coupling
    /// cannot come back by the same route. This checks what is left: every
    /// backend answers, for every model, with nothing constructed.
    #[test]
    fn every_capability_is_answerable_without_a_client() {
        for backend in Backend::ALL {
            for model in ["", "nonsense", backend.default_model()] {
                let caps = capabilities_for(*backend, model);
                assert!(
                    !caps.provider.is_empty() && !caps.tagline.is_empty(),
                    "{} answered emptily for `{model}`",
                    backend.name()
                );
            }
        }
    }

    /// Pinned against dates computed independently, because everything about
    /// retirement notes rests on this one function and it is the only date
    /// arithmetic in the program. The epoch itself, a leap day, a century that is
    /// not a leap year, and the two dates actually in the table.
    #[test]
    fn a_civil_date_converts_to_the_right_instant() {
        assert_eq!(unix_time("1970-01-01"), Some(0));
        assert_eq!(unix_time("2000-02-29"), Some(951_782_400));
        assert_eq!(unix_time("1900-03-01"), Some(-2_203_891_200));
        assert_eq!(unix_time("2026-08-17"), Some(1_786_924_800));
        assert_eq!(unix_time("2026-12-01"), Some(1_796_083_200));
    }

    /// A malformed date must read as "not yet", so a typo in the table can only
    /// ever understate — announcing a retirement that has not happened is a
    /// worse failure than being late to announce one that has.
    #[test]
    fn an_unparseable_date_is_never_in_the_past() {
        for bad in ["", "soon", "2026-13-01", "2026-08-32", "2026-08", "2026-08-17-1"] {
            assert_eq!(unix_time(bad), None, "{bad} parsed");
            assert!(!past(bad), "{bad} read as past");
        }
    }

    /// The tense is the whole feature: the same string has to stay true the day
    /// after the date it names. Imagen's date is within days of this being
    /// written, so both branches are about to be exercised for real.
    #[test]
    fn a_retirement_note_reads_correctly_on_both_sides_of_its_date() {
        let note = retirement_note("imagen-4.0-generate-001").expect("imagen retires");
        assert!(note.contains("2026-08-17"), "{note}");
        assert_eq!(note.starts_with("retired"), past("2026-08-17"), "{note}");

        assert!(retirement_note("gpt-image-1.5").is_some());
        assert!(retirement_note("chatgpt-image-latest").is_some());
    }

    /// The models that are still current must not be annotated — a false
    /// retirement notice sends someone to migrate off the thing they should be
    /// using. `gpt-image-1` is the sharp case: it is not retiring, and it is a
    /// prefix of two ids that are.
    #[test]
    fn the_current_defaults_carry_no_retirement_note() {
        for backend in Backend::ALL {
            let model = backend.default_model();
            assert_eq!(
                retirement_note(model),
                None,
                "`{model}` is a default and is marked as retiring"
            );
        }
        assert_eq!(retirement_note("gpt-image-1"), None);
    }

    /// Every announced date has to be reachable from a model id someone can
    /// actually name, or the row is decoration.
    #[test]
    fn every_retirement_matches_a_model_the_provider_lists() {
        for retirement in RETIREMENTS {
            assert!(
                unix_time(retirement.date).is_some(),
                "`{}` has an unparseable date: {}",
                retirement.prefix,
                retirement.date
            );
            assert!(
                retirement_note(retirement.prefix).is_some(),
                "`{}` matches no model id, not even its own prefix",
                retirement.prefix
            );
        }
    }

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

    /// Every hosted provider rejects an uppercase model id — measured against
    /// all four live APIs, which 404 the path or report the model does not
    /// exist. So a typed `--model CORE` must reach the wire lowercased rather
    /// than as a rejection nobody can read. The four used to disagree about
    /// this, purely by accident.
    #[test]
    fn hosted_model_ids_reach_the_wire_lowercased() {
        assert_eq!(crate::stability::resolve_model("CORE"), "core");
        assert_eq!(crate::openai::resolve_model("GPT-IMAGE-2"), "gpt-image-2");
        assert_eq!(crate::bfl::resolve_model("FLUX-2-PRO"), "flux-2-pro");
        assert_eq!(
            crate::genai::resolve_model("GEMINI-3.1-FLASH-IMAGE"),
            "gemini-3.1-flash-image"
        );
        // An alias still resolves to its target, whatever the caller typed.
        assert_eq!(crate::bfl::resolve_model("FLUX"), crate::bfl::resolve_model("flux"));
    }

    /// The generated mask paragraph has to cover every provider that masks, and
    /// name the kind for each — that is the whole reason it is generated.
    ///
    /// What this would have caught: after v0.9.0 the local lane bound its mask
    /// and six of the seven surfaces that describe masking still said
    /// "advisory", because each of them generated the provider *list* beside a
    /// hand-written semantics claim. A provider added with a mask now appears
    /// here automatically, and one added with the wrong kind fails the openai
    /// pair test.
    #[test]
    fn the_mask_paragraph_covers_every_masking_provider_and_names_its_kind() {
        let text = mask_semantics();

        for backend in Backend::ALL {
            let caps = capabilities_for(*backend, backend.default_model());
            if caps.mask.accepted() {
                assert!(
                    text.contains(backend.name()),
                    "{} masks but is missing from the generated paragraph: {text}",
                    backend.name()
                );
                assert!(text.contains(caps.mask.kind()), "{text}");
            }
        }

        // The remedy order is load-bearing: a caller sent away from a provider
        // that cannot mask should be pointed first at the one whose mask is a
        // guarantee, which is the reason to prefer it.
        let accepting = mask_accepting_providers();
        assert_eq!(accepting.first(), Some(&"comfyui"), "{accepting:?}");
    }

    /// The refusal has to name a way forward, and — since v0.9.0 — the right one.
    ///
    /// The hand-written version named `openai` and called every mask advisory,
    /// so it sent people away from the only provider whose mask guarantees
    /// anything. Both halves are computed now.
    #[test]
    fn refusing_a_mask_names_the_provider_whose_mask_binds() {
        let caps = capabilities_for(Backend::Google, "");
        let req = ImageRequest {
            mask: Some("mask.png".into()),
            ..Default::default()
        };

        let error = caps.check(&req).unwrap_err().to_string();
        assert!(error.contains("comfyui"), "{error}");
        assert!(error.contains("binding"), "{error}");
    }

    /// A mask with nothing to apply it to is refused before the capability
    /// question, since "which provider" is not the problem there.
    #[test]
    fn a_mask_without_a_reference_image_says_so() {
        let caps = capabilities_for(Backend::OpenAi, "gpt-image-2");
        let req = ImageRequest {
            mask: Some("mask.png".into()),
            ..Default::default()
        };
        let error = caps.check(&req).unwrap_err().to_string();
        assert!(error.contains("no image"), "{error}");
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
