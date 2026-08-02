//! Lucida — image and video generation.
//!
//! Named for the camera lucida, the optical device that let artists trace what
//! they saw onto paper.
//!
//! One binary, two front ends: a plain CLI for shell and script use, and an MCP
//! server (`lucida mcp`) so agents can call it as a first-class tool.
//!
//! Images come from one of three providers — Google's Gemini models, a local
//! ComfyUI, or hosted FLUX from Black Forest Labs — chosen from the model id
//! unless `--provider` says otherwise. Video is Google-only for now.

mod bfl;
mod comfy;
mod config;
mod genai;
mod masked;
mod mcp;
mod openai;
mod provider;
mod stability;
#[cfg(test)]
mod testserver;
mod video;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use genai::DEFAULT_MODEL;
use provider::{Aspect, Backend, ImageProvider, ImageRequest, Size, infer_backend};
use std::path::{Path, PathBuf};
use video::{DEFAULT_VIDEO_MODEL, VideoRequest};

#[derive(Parser)]
#[command(
    name = "lucida",
    version,
    about = "Generate and edit images with Google Gemini, a local ComfyUI, or hosted FLUX",
    long_about = "Generate and edit images with Google Gemini, a local ComfyUI, or \
                  hosted FLUX from Black Forest Labs.\n\n\
                  Google reads GOOGLE_API_KEY (or GEMINI_API_KEY) from the \
                  environment, and image generation requires billing to be enabled \
                  on the project behind the key; free-tier keys report a quota of \
                  zero.\n\n\
                  ComfyUI needs no credential. It is found at \
                  http://127.0.0.1:8188 unless LUCIDA_COMFYUI_URL says otherwise.\n\n\
                  Black Forest Labs reads BFL_API_KEY and bills per image. Its \
                  capabilities differ per model — run `lucida models --provider bfl`.\n\n\
                  Any of these can live in a config file; see `lucida config`."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Options shared by `generate` and `edit`.
///
/// Flattened rather than repeated, because the two commands differ only in how
/// they treat the leading image — every knob applies to both.
#[derive(Args, Clone, Default)]
struct ImageOptions {
    /// Aspect ratio, e.g. 16:9, 1:1, 4:5
    #[arg(short, long)]
    aspect: Option<String>,

    /// Long edge: a tier (1K, 2K, 4K) or a pixel count
    #[arg(short, long)]
    size: Option<String>,

    /// Model id or alias. Defaults per provider.
    #[arg(short, long)]
    model: Option<String>,

    /// Which provider to use: google, comfyui or bfl. Inferred from the model when omitted.
    #[arg(short, long)]
    provider: Option<String>,

    /// What to keep out of the picture (comfyui only — no FLUX or Gemini model takes one)
    #[arg(short, long)]
    negative: Option<String>,

    /// Render with a ComfyUI workflow of your own (API format) instead of the
    /// built-in graph. Fill in %prompt% %negative% %seed% %width% %height%
    /// %steps% %cfg% where they belong. comfyui only.
    #[arg(long, value_name = "FILE")]
    workflow: Option<String>,

    /// Concentrate an edit on part of the image: a PNG whose TRANSPARENT pixels
    /// are what changes. openai only, and advisory — the rest may still change.
    #[arg(long)]
    mask: Option<String>,

    /// Seed, for a reproducible render (comfyui and bfl; google has none)
    #[arg(long)]
    seed: Option<u64>,

    /// Sampling steps (comfyui, and bfl on flux-2-flex / flux-dev only)
    #[arg(long)]
    steps: Option<u32>,

    /// Guidance scale (comfyui, and bfl on flux-2-flex / flux-dev only)
    #[arg(short, long)]
    guidance: Option<f32>,
}

#[derive(Subcommand)]
enum Command {
    /// Generate an image from a prompt
    Generate {
        /// What to draw
        prompt: String,

        /// Where to write the image
        #[arg(short, long, default_value = "image.png")]
        out: PathBuf,

        /// Existing image to condition on; repeat for several. Prefer the
        /// `edit` subcommand, which reads better for the common case.
        #[arg(short, long = "ref")]
        reference: Vec<String>,

        #[command(flatten)]
        opts: ImageOptions,
    },

    /// Edit an existing image with a prompt
    Edit {
        /// The image to change
        image: String,

        /// What to change about it
        prompt: String,

        /// Where to write the result. Defaults to overwriting the input.
        #[arg(short, long)]
        out: Option<PathBuf>,

        /// Additional images for style or subject reference
        #[arg(short, long = "ref")]
        reference: Vec<String>,

        #[command(flatten)]
        opts: ImageOptions,
    },

    /// Generate a video with Veo. Renders take minutes and bill per second.
    Video {
        /// What to film
        prompt: String,

        /// Where to write the video
        #[arg(short, long, default_value = "video.mp4")]
        out: PathBuf,

        /// A still image to animate, making this image-to-video
        #[arg(short, long)]
        image: Option<String>,

        /// Aspect ratio: 16:9 or 9:16
        #[arg(short, long)]
        aspect: Option<String>,

        /// Resolution, e.g. 720p or 1080p
        #[arg(short, long)]
        resolution: Option<String>,

        /// What to keep out of the shot
        #[arg(short, long)]
        negative: Option<String>,

        /// Model id or alias: veo, veo-standard, veo-lite
        #[arg(short, long, default_value = DEFAULT_VIDEO_MODEL)]
        model: String,
    },

    /// Resume a video render by operation id, e.g. after a timeout
    Check {
        /// The operation id reported when the render started
        operation: String,

        /// Where to write the video once it is ready
        #[arg(short, long, default_value = "video.mp4")]
        out: PathBuf,
    },

    /// List the image models a provider can reach, and what it can be asked for
    Models {
        /// Which provider to interrogate: google, comfyui or bfl
        #[arg(short, long, default_value = "google")]
        provider: String,
    },

    /// Show what settings this process can see, and where they came from
    Config {
        /// Write a starter config file and print its path
        #[arg(long)]
        init: bool,

        /// Set one setting. Prompts at a terminal, or reads a pipe:
        /// `pbpaste | lucida config --set BFL_API_KEY`
        #[arg(long, value_name = "NAME")]
        set: Option<String>,
    },

    /// Run as an MCP server over stdio
    Mcp,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Mcp => mcp::serve(),

        Command::Models { provider } => list_models(Backend::parse(&provider)?),

        Command::Config { init, set } => match set {
            Some(name) => set_config(&name),
            None if init => init_config(),
            None => {
                show_config();
                Ok(())
            }
        },

        Command::Generate {
            prompt,
            out,
            reference,
            opts,
        } => {
            let (request, backend) = opts.into_request(prompt, reference)?;
            execute(request, backend, out)
        }

        Command::Edit {
            image,
            prompt,
            out,
            reference,
            opts,
        } => {
            // The edited image leads, so it is the primary subject rather than
            // one reference among several.
            let mut references = vec![image.clone()];
            references.extend(reference);

            let destination = out.unwrap_or_else(|| PathBuf::from(&image));
            let (request, backend) = opts.into_request(prompt, references)?;
            execute(request, backend, destination)
        }

        Command::Check { operation, out } => {
            match genai::Client::from_env()?.poll_video(&operation)? {
                video::VideoStatus::Pending => {
                    eprintln!("Still rendering. Try again in half a minute.");
                    Ok(())
                }
                video::VideoStatus::Done(bytes) => {
                    let written = write_image(correct_extension(&out, "video/mp4"), &bytes)?;
                    eprintln!(
                        "Wrote {} ({:.1} MB)",
                        written.display(),
                        bytes.len() as f64 / 1_048_576.0
                    );
                    println!("{}", written.display());
                    Ok(())
                }
            }
        }

        Command::Video {
            prompt,
            out,
            image,
            aspect,
            resolution,
            negative,
            model,
        } => {
            let request = VideoRequest {
                prompt,
                model,
                aspect_ratio: aspect,
                resolution,
                negative_prompt: negative,
                image,
            };
            let resolved = video::resolve_video_model(&request.model);
            eprintln!("Rendering with {resolved}…");

            let bytes = genai::Client::from_env()?.generate_video(&request)?;
            let written = write_image(&out, &bytes)?;
            eprintln!(
                "Wrote {} ({} MB)",
                written.display(),
                bytes.len() / 1_048_576
            );
            println!("{}", written.display());
            Ok(())
        }
    }
}

impl ImageOptions {
    /// Turns raw CLI strings into a normalized request, and works out which
    /// provider serves it.
    ///
    /// Provider selection is inferred from the model id so `--provider` stays
    /// optional in the common case; naming it explicitly wins, and also supplies
    /// the model default, so `--provider comfyui` alone does the right thing
    /// rather than sending a Gemini model id to a local server.
    fn into_request(
        self,
        prompt: String,
        references: Vec<String>,
    ) -> Result<(ImageRequest, Backend)> {
        let backend = match (&self.provider, &self.model) {
            (Some(name), _) => Backend::parse(name)?,
            (None, Some(model)) => infer_backend(model),
            (None, None) => Backend::Google,
        };

        let model = self.model.unwrap_or_else(|| backend.default_model().to_string());

        let request = ImageRequest {
            prompt,
            model,
            aspect: self.aspect.as_deref().map(Aspect::parse).transpose()?,
            size: self.size.as_deref().map(Size::parse).transpose()?,
            references,
            negative_prompt: self.negative,
            mask: self.mask,
            workflow: self.workflow,
            seed: self.seed,
            steps: self.steps,
            guidance: self.guidance,
        };

        Ok((request, backend))
    }
}

/// Reports what this process can actually see.
///
/// The point is diagnostic rather than informational. When an MCP server cannot
/// find a key that is demonstrably exported in a shell profile, the useful
/// question is "what does *that* process see", and the answer differs from what
/// the same command shows in a terminal. Running it through the same binary is
/// the only way to get a truthful answer.
///
/// Values are never printed — only whether each setting is set and where it came
/// from. That is what diagnoses the problem, and it is safe to paste.
fn show_config() {
    match config::source() {
        Some(path) => println!("Config file: {}", path.display()),
        None => println!("Config file: none found"),
    }

    println!("\nLooked for it at:");
    for path in config::search_paths() {
        let mark = if path.is_file() { "found" } else { "not found" };
        println!("  {}  ({mark})", path.display());
    }

    println!("\nSettings visible to this process:");
    for (key, purpose) in config::KNOWN_KEYS {
        // The environment is reported separately from the file, because "set in
        // both" and "set only in the file" behave differently and the whole
        // class of bug here is about which one a process can reach.
        let in_env = std::env::var(key).is_ok_and(|v| !v.trim().is_empty());
        let source = match (in_env, config::var(key).is_some()) {
            (true, _) => "set (environment)",
            (false, true) => "set (config file)",
            (false, false) => "not set",
        };
        println!("  {key:<22} {source:<20} {purpose}");
    }

    // A name Lucida does not know is the silent failure worth surfacing: the
    // file looks right, the value is there, and nothing ever reads it.
    let unrecognised: Vec<String> = config::keys_in_file()
        .into_iter()
        .filter(|name| !config::KNOWN_KEYS.iter().any(|(known, _)| known == name))
        .collect();
    if !unrecognised.is_empty() {
        println!("\nIn the config file but not recognised by Lucida:");
        for name in &unrecognised {
            println!("  {name}  (ignored — check the spelling)");
        }
    }

    if config::source().is_none() {
        println!(
            "\nNo config file yet. `lucida config --init` writes one — useful when \
             a GUI-launched\napp cannot see your shell's environment."
        );
    }
}

/// Writes one setting, taking its value from stdin.
///
/// From stdin rather than an argument, deliberately. A key passed as
/// `--set KEY=value` lands in shell history, in the process table where any
/// other user can read it with `ps`, and in any transcript of the session. A
/// pipe avoids all three:
///
/// ```text
/// pbpaste | lucida config --set BFL_API_KEY
/// ```
///
/// Rewrites the named line in place if present, appends it otherwise, and never
/// disturbs anything else in the file — including comments.
fn set_config(name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!(
            "`{name}` is not a valid setting name — expected something like GOOGLE_API_KEY"
        );
    }

    // Two ways in, and the difference is worth handling rather than making the
    // user absorb it.
    //
    // Piped, the whole of stdin is the value: reading to EOF is the only correct
    // thing, since a key could in principle contain a newline and the writer
    // decides where it ends.
    //
    // At a terminal there is no writer to decide, so reading to EOF means
    // demanding Ctrl-D — which looks like a hang, because nothing has been
    // printed and the cursor just sits there. A single line, ended by Enter, is
    // what anyone typing expects.
    use std::io::{IsTerminal, Read};

    let stdin = std::io::stdin();
    let mut value = String::new();

    if stdin.is_terminal() {
        // To stderr, so stdout stays the machine-readable path as everywhere else.
        eprint!("Value for {name}: ");
        std::io::Write::flush(&mut std::io::stderr()).ok();

        // One asterisk per character: enough to show the paste landed, without
        // showing what landed.
        value = masked::read_masked()?;

        // Still worth stating the count. An asterisk run is hard to eyeball, and
        // a key that arrived truncated or doubled is exactly the failure this
        // catches. Deliberately not the first or last few characters — those are
        // what identifies a key in a screenshot or a pasted transcript.
        eprintln!("({} characters)", value.trim().chars().count());
    } else {
        stdin
            .lock()
            .read_to_string(&mut value)
            .context("reading the value from stdin")?;
    }

    let value = value.trim();

    if value.is_empty() {
        anyhow::bail!(
            "no value was given, so there is nothing to set.\n\n\
             Type it at the prompt, or pipe it in: \
             `pbpaste | lucida config --set {name}`."
        );
    }

    let path = config::preferred_path()
        .context("could not determine a config location: neither HOME nor XDG_CONFIG_HOME is set")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let assignment = format!("{name}={value}");

    let target = lines.iter().position(|line| {
        let bare = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        bare.split_once('=').is_some_and(|(key, _)| key.trim() == name)
    });

    let replaced = target.is_some();
    match target {
        Some(at) => lines[at] = assignment,
        None => lines.push(assignment),
    }

    let mut body = lines.join("\n");
    body.push('\n');
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    restrict_permissions(&path)?;

    // The value is never echoed — the whole point of taking it on stdin.
    eprintln!(
        "{} {name} in {}.",
        if replaced { "Updated" } else { "Added" },
        path.display()
    );
    println!("{}", path.display());
    Ok(())
}

fn init_config() -> Result<()> {
    let path = config::preferred_path()
        .context("could not determine a config location: neither HOME nor XDG_CONFIG_HOME is set")?;

    if path.exists() {
        // Never clobber a file that may hold the only copy of a key.
        eprintln!("{} already exists; leaving it alone.", path.display());
        println!("{}", path.display());
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    std::fs::write(&path, config::template())
        .with_context(|| format!("writing {}", path.display()))?;
    restrict_permissions(&path)?;

    eprintln!(
        "Wrote {}.\n\nEvery line is commented out, so nothing changed yet. \
         Uncomment GOOGLE_API_KEY\nand set it, then check with `lucida config`.",
        path.display()
    );
    println!("{}", path.display());
    Ok(())
}

/// Creates the file readable only by its owner.
///
/// Done at creation rather than left to the umask, because the file is intended
/// to hold an API key and the default umask on most systems leaves it readable
/// by the whole group.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}



fn open(backend: Backend) -> Result<Box<dyn ImageProvider>> {
    Ok(match backend {
        Backend::Google => Box::new(genai::Client::from_env()?),
        Backend::ComfyUi => Box::new(comfy::Client::from_env()?),
        Backend::Bfl => Box::new(bfl::Client::from_env()?),
        Backend::Stability => Box::new(stability::Client::from_env()?),
        Backend::OpenAi => Box::new(openai::Client::from_env()?),
    })
}

fn list_models(backend: Backend) -> Result<()> {
    let provider = open(backend)?;
    let caps = provider.capabilities();
    let models = provider.list_models()?;

    if models.is_empty() {
        println!("No image models visible to the {} provider.", caps.provider);
    } else {
        println!("Image models available to the {} provider:", caps.provider);
        for model in &models {
            let mut notes: Vec<String> = Vec::new();
            if backend == Backend::Google && model == DEFAULT_MODEL {
                notes.push("default".into());
            }
            if model.starts_with("imagen") {
                notes.push("Imagen family — different endpoint, retires 2026-08-17".into());
            }
            // BFL's endpoints disagree with each other, so the differences are
            // listed per model rather than once for the provider. Anything else
            // would send someone to the wrong endpoint for `--steps`.
            if backend == Backend::Bfl {
                if model == bfl::DEFAULT_MODEL {
                    notes.push("default".into());
                }
                let per_model = provider::capabilities_for(backend, model);
                if per_model.steps {
                    notes.push("steps + guidance".into());
                }
                notes.push(if per_model.references {
                    "edits".into()
                } else {
                    "generate only".into()
                });
            }
            let suffix = if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join("; "))
            };
            println!("  {model}{suffix}");
        }
    }

    let aliases: &[(&str, &str)] = match backend {
        Backend::Google => genai::MODEL_ALIASES,
        Backend::ComfyUi => comfy::MODEL_ALIASES,
        Backend::Bfl => bfl::MODEL_ALIASES,
        Backend::Stability => stability::MODEL_ALIASES,
        Backend::OpenAi => openai::MODEL_ALIASES,
    };
    if !aliases.is_empty() {
        println!("\nAliases:");
        for (alias, target) in aliases {
            println!("  {alias:<16} -> {target}");
        }
    }

    // Printed because it is the question users otherwise answer by trial and
    // error, one rejected flag at a time. For bfl this is the floor for the
    // default model; the per-model differences are annotated above.
    println!("\nThis provider supports:");
    println!("  aspect ratio    {}", describe_aspect(caps.aspect));
    println!("  output size     {}", yes_no(caps.size));
    println!("  seed            {}", yes_no(caps.seed));
    println!("  negative prompt {}", yes_no(caps.negative_prompt));
    println!("  reference image {}", yes_no(caps.references));
    println!("  own workflow    {}", yes_no(caps.workflow));
    println!(
        "  mask            {}",
        if caps.mask { "accepted (advisory)" } else { "no" }
    );
    println!("  steps           {}", yes_no(caps.steps));
    println!("  guidance        {}", yes_no(caps.guidance));
    println!("  output carries  {}", caps.provenance.describe());

    Ok(())
}

fn yes_no(supported: bool) -> &'static str {
    if supported { "yes" } else { "no" }
}

fn describe_aspect(support: provider::AspectSupport) -> String {
    match support {
        provider::AspectSupport::Named(ratios) => ratios.join(", "),
        provider::AspectSupport::Free { multiple_of } => {
            format!("any, rounded to {multiple_of} pixels")
        }
    }
}

fn execute(request: ImageRequest, backend: Backend, out: PathBuf) -> Result<()> {
    // Before a client is even constructed: reject what this provider cannot
    // express, rather than dropping it and returning an image that quietly
    // ignored half the request. Checked first so that asking Google for a seed
    // says so even with no API key set — the key was never the problem.
    let caps = provider::capabilities_for(backend, &request.model);
    caps.check(&request)?;

    let provider = open(backend)?;

    let verb = if request.references.is_empty() {
        "Generating"
    } else {
        "Editing"
    };
    eprintln!("{verb} via {}…", caps.provider);

    let image = provider.generate(&request)?;

    let destination = correct_extension(&out, &image.mime_type);
    if destination != out {
        eprintln!(
            "note: the model returned {}, so writing {} rather than {}",
            image.mime_type,
            destination.display(),
            out.display()
        );
    }
    let written = write_image(&destination, &image.bytes)?;

    if let Some(commentary) = &image.commentary {
        if !commentary.is_empty() {
            eprintln!("{commentary}");
        }
    }
    if let Some(seed) = image.seed {
        eprintln!("Seed {seed} — pass `--seed {seed}` to render this again.");
    }

    // The size is reported rather than assumed, because an edit on the local lane
    // normalizes to roughly a megapixel and may not match the source.
    let size = match image_dimensions(&image.bytes, &image.mime_type) {
        Some((w, h)) => format!("{w}x{h}, "),
        None => String::new(),
    };
    eprintln!(
        "Wrote {} ({size}{} KB)",
        written.display(),
        image.bytes.len() / 1024
    );

    // The path alone on stdout, so this composes in a pipeline.
    println!("{}", written.display());
    Ok(())
}

/// Reads the pixel dimensions out of an encoded image.
///
/// Worth the forty lines because the output size is not always the size that was
/// asked for, and on the local lane it is not always the size of the input
/// either: an edit normalizes to roughly a megapixel, so a 1024x576 source comes
/// back 1360x768. That was a surprise when measured, and a surprise is only
/// acceptable once it is stated — so the size actually written gets reported.
///
/// Hand-rolled rather than pulling in an image crate, since this needs the first
/// few bytes of a header and nothing else.
pub fn image_dimensions(bytes: &[u8], mime: &str) -> Option<(u32, u32)> {
    match mime {
        // IHDR is always the first chunk, at a fixed offset.
        "image/png" => {
            let (w, h) = (bytes.get(16..20)?, bytes.get(20..24)?);
            Some((
                u32::from_be_bytes(w.try_into().ok()?),
                u32::from_be_bytes(h.try_into().ok()?),
            ))
        }
        // JPEG has no fixed offset: walk the marker segments to the frame header.
        "image/jpeg" => {
            let mut at = 2;
            while at + 9 < bytes.len() {
                if bytes[at] != 0xFF {
                    at += 1;
                    continue;
                }
                let marker = bytes[at + 1];
                // Every SOFn carries the dimensions except the four that are not
                // frame headers at all (DHT, JPG, DAC, and the RSTn range).
                let is_frame = matches!(marker, 0xC0..=0xCF)
                    && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
                if is_frame {
                    let h = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]);
                    let w = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]);
                    return Some((u32::from(w), u32::from(h)));
                }
                let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
                at += 2 + length.max(2);
            }
            None
        }
        _ => None,
    }
}

/// Corrects a file extension that disagrees with what the provider actually
/// returned.
///
/// Gemini decides the output format itself — usually JPEG, whatever the request
/// asked for — so `-o icon.png` would otherwise leave a file named `.png` holding
/// JPEG bytes. That passes unnoticed until some downstream tool rejects it. The
/// real path is what goes to stdout, so scripts capturing it stay correct.
pub fn correct_extension(path: &Path, mime: &str) -> PathBuf {
    let expected = match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        _ => return path.to_path_buf(),
    };

    let actual = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);

    let matches = match actual.as_deref() {
        Some("jpg" | "jpeg") => expected == "jpg",
        Some(other) => other == expected,
        None => false,
    };

    if matches {
        path.to_path_buf()
    } else {
        path.with_extension(expected)
    }
}

/// Writes `bytes` to `path`, creating parent directories, and returns the
/// absolute path actually written.
pub fn write_image(path: impl AsRef<Path>, bytes: &[u8]) -> Result<PathBuf> {
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
    }

    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;

    Ok(std::fs::canonicalize(path)
        .map(strip_unc_prefix)
        .unwrap_or_else(|_| path.to_path_buf()))
}

/// Removes the `\\?\` verbatim prefix that Windows `canonicalize` returns.
///
/// The prefix is legal and the path works, but it leaks into printed output and
/// some tools reject it. Written without `cfg(windows)` because the prefix
/// cannot occur on other platforms, so the check is simply inert there.
fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(stripped) => PathBuf::from(stripped),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_dimensions_come_from_the_ihdr_chunk() {
        // A minimal PNG header: signature, chunk length, "IHDR", then w/h.
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1360u32.to_be_bytes());
        png.extend_from_slice(&768u32.to_be_bytes());
        assert_eq!(image_dimensions(&png, "image/png"), Some((1360, 768)));
    }

    #[test]
    fn jpeg_dimensions_are_found_by_walking_to_the_frame_header() {
        // SOI, then a JFIF APP0 to be skipped, then SOF0 carrying the size.
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        jpeg.extend_from_slice(&[0u8; 14]);
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        jpeg.extend_from_slice(&576u16.to_be_bytes()); // height precedes width
        jpeg.extend_from_slice(&1024u16.to_be_bytes());
        jpeg.extend_from_slice(&[0u8; 8]);
        assert_eq!(image_dimensions(&jpeg, "image/jpeg"), Some((1024, 576)));
    }

    #[test]
    fn truncated_or_unknown_data_reports_nothing_rather_than_guessing() {
        assert_eq!(image_dimensions(&[0x89, b'P', b'N', b'G'], "image/png"), None);
        assert_eq!(image_dimensions(&[0xFF, 0xD8], "image/jpeg"), None);
        assert_eq!(image_dimensions(&[0; 64], "image/webp"), None);
    }
}
