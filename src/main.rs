//! Lucida — image and video generation.
//!
//! Named for the camera lucida, the optical device that let artists trace what
//! they saw onto paper.
//!
//! One binary, two front ends: a plain CLI for shell and script use, and an MCP
//! server (`lucida mcp`) so agents can call it as a first-class tool.
//!
//! Images come from one of five providers — Google's Gemini models, a local
//! ComfyUI, hosted FLUX from Black Forest Labs, Stability AI, or OpenAI —
//! chosen from the model id unless `--provider` says otherwise. Video is
//! Google-only for now.

mod bfl;
mod comfy;
mod config;
mod genai;
mod masked;
mod mcp;
mod openai;
mod provider;
mod retry;
mod setup;
mod skill;
mod stability;
#[cfg(test)]
mod testserver;
mod update;
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
    about = "Generate images and video with Google Gemini, Veo, a local ComfyUI, FLUX, Stability AI or OpenAI",
    long_about = "Generate and edit images with Google Gemini, a local ComfyUI, \
                  hosted FLUX from Black Forest Labs, Stability AI, or OpenAI, \
                  and video with Veo.\n\n\
                  Google reads GEMINI_API_KEY — one key for both images and Veo \
                  video. Image generation requires billing to be enabled on the \
                  project behind the key; free-tier keys report a quota of \
                  zero.\n\n\
                  ComfyUI needs no credential. It is found at \
                  http://127.0.0.1:8188 unless LUCIDA_COMFYUI_URL says otherwise.\n\n\
                  Black Forest Labs reads BFL_API_KEY and bills per image. Its \
                  capabilities differ per model — run `lucida models --provider bfl`.\n\n\
                  Stability reads STABILITY_API_KEY; OpenAI reads OPENAI_API_KEY, \
                  and model access there is granted per project.\n\n\
                  Any of these can live in a config file; see `lucida config`.",
    disable_version_flag = true
)]
struct Cli {
    /// Print version
    ///
    /// clap's own flag is `-V`; this one is `-v`, with the uppercase spelling
    /// kept as an alias. A version flag is exactly what a wrapper script calls,
    /// and breaking one to save a keystroke would be a poor trade.
    ///
    /// This does spend `-v`, which conventionally means `--verbose`. There is no
    /// verbosity flag today, and one would need a different letter.
    #[arg(short = 'v', short_alias = 'V', long, action = clap::ArgAction::Version)]
    version: Option<bool>,

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

    /// Which provider to use: google, comfyui, bfl, stability or openai. Inferred from the model when omitted.
    #[arg(short, long)]
    provider: Option<String>,

    /// What to keep out of the picture (comfyui and stability — no FLUX, Gemini or gpt-image model takes one)
    #[arg(short, long)]
    negative: Option<String>,

    /// Render with a ComfyUI workflow of your own (API format) instead of the
    /// built-in graph. Fill in %prompt% %negative% %seed% %width% %height%
    /// %steps% %cfg% where they belong. comfyui only.
    #[arg(long, value_name = "FILE")]
    workflow: Option<String>,

    /// Concentrate an edit on part of the image: a PNG whose TRANSPARENT pixels
    /// are what changes. Not every provider takes one, and what it guarantees
    /// differs — `lucida models --provider <name>` says which.
    //
    // Deliberately naming no provider and claiming no semantics. A clap help
    // string must be a literal, so this is one of the hand-written surfaces that
    // cannot be generated (2026-08-02 review §5.1) — and it said "openai only,
    // and advisory" for a release after both halves stopped being true. A
    // pointer at the generated answer is the one sentence that stays correct.
    #[arg(long)]
    mask: Option<String>,

    /// Seed, for a reproducible render (comfyui, bfl and stability; google and openai have none)
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

        /// Start the render and print its operation id instead of waiting.
        /// Collect it later with `lucida check`.
        #[arg(long)]
        no_wait: bool,
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
        /// Which provider to interrogate: google, comfyui, bfl, stability or openai
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
        #[arg(long, value_name = "NAME", conflicts_with_all = ["init", "remove"])]
        set: Option<String>,

        /// Remove one setting from the config file, wherever it lives
        #[arg(long, value_name = "NAME", conflicts_with = "init")]
        remove: Option<String>,
    },

    /// Wire Lucida into Claude Code and the Claude app
    Setup {
        /// Set up for one project rather than the whole machine
        #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".")]
        project: Option<PathBuf>,

        /// Show what would be done, and stop
        #[arg(long)]
        dry_run: bool,

        /// Apply without asking. For automation, where there is nobody to prompt
        #[arg(short = 'y', long, conflicts_with = "dry_run")]
        yes: bool,
    },

    /// Print the agent skill, for a client's skills directory
    Skill,

    /// Replace this binary with the latest release
    Update {
        /// Report what is available without installing it
        #[arg(long)]
        check: bool,

        /// Install without asking. For automation, where there is nobody to prompt
        #[arg(short = 'y', long, conflicts_with = "check")]
        yes: bool,
    },

    /// Run as an MCP server over stdio
    Mcp,
}

fn main() {
    let cli = Cli::parse();

    // `mcp` is excluded because its client spawns and kills it constantly, so a
    // check there is a network round trip per launch; `update` because it has
    // just done this properly and would otherwise say it twice.
    let announce = !matches!(cli.command, Command::Mcp | Command::Update { .. });

    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        // Deliberately no update notice on the way out: the error is what the
        // reader needs, and appending unrelated news to a failure is noise.
        std::process::exit(1);
    }

    // After the work, never before — so a slow or unreachable GitHub costs a
    // few seconds at exit rather than delaying a render. It installs nothing.
    if announce {
        update::notify_if_due(env!("CARGO_PKG_VERSION"));
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Mcp => mcp::serve(),

        Command::Models { provider } => list_models(Backend::parse(&provider)?),

        Command::Setup {
            project,
            dry_run,
            yes,
        } => {
            let scope = match project {
                Some(dir) => setup::Scope::Project(
                    std::fs::canonicalize(&dir).unwrap_or(dir),
                ),
                None => setup::Scope::User,
            };
            setup::run(scope, dry_run, yes)
        }

        Command::Skill => {
            skill::print();
            Ok(())
        }

        Command::Update { check, yes } => {
            let mode = match (check, yes) {
                (true, _) => update::Mode::Check,
                (_, true) => update::Mode::Yes,
                _ => update::Mode::Ask,
            };
            update::Updater::new()?.run(mode)
        }

        Command::Config { init, set, remove } => match (set, remove) {
            (Some(name), _) => set_config(&name),
            (_, Some(name)) => remove_config(&name),
            _ if init => init_config(),
            _ => {
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
            no_wait,
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

            let client = genai::Client::from_env()?;

            // The shape the MCP surface has had since it existed — start, hand
            // back the id, let the caller collect it — finally available to the
            // shell too. Unattended callers want it: a render that outlives the
            // process is fine, a process that must survive the render is not.
            if no_wait {
                let operation = client.start_video(&request)?;
                eprintln!("{}", video::resume_notice(&operation));
                // The id on stdout, where the path goes when we do wait: one
                // line, the useful part, capturable by a script.
                println!("{operation}");
                return Ok(());
            }

            let bytes = client.generate_video(&request)?;
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
        // A supplied workflow names its own checkpoints, so an explicit model
        // has nowhere to go — the same reasoning that refuses `--ref` with a
        // workflow. Caught here rather than in the provider because only the
        // entry point still knows the model was typed rather than defaulted.
        if self.workflow.is_some() && self.model.is_some() {
            anyhow::bail!(
                "a workflow and an explicit `--model` cannot be combined.\n\n\
                 A supplied workflow names its own checkpoints, so there is \
                 nowhere to put a model id. Name the model inside the workflow \
                 file, or drop `--workflow` to use the built-in graph."
            );
        }

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
    let mut shadowed: Vec<&str> = Vec::new();
    for (key, purpose) in config::KNOWN_KEYS {
        // The source is reported, not just the presence, because "set in both"
        // and "set in one" resolve to the same value but not to the same
        // situation — and the whole class of bug here is about which source a
        // process actually reaches.
        let source = match config::origin(key) {
            Some(config::Origin::File) => "set (config file)",
            Some(config::Origin::Environment) => "set (environment)",
            Some(config::Origin::FileOverridingEnvironment) => {
                shadowed.push(key);
                "set (config file)"
            }
            None => "not set",
        };
        println!("  {key:<22} {source:<20} {purpose}");
    }

    // Stated rather than left to be inferred from the column above. Someone
    // reading this is usually asking why a key they exported is not being used,
    // and this is the answer.
    if !shadowed.is_empty() {
        println!("\nAlso set in this environment, and not used — the config file wins:");
        for key in shadowed {
            println!("  {key}");
        }
    }

    // A renamed setting is the other way to hold a key that is present, correct
    // and never read. Reported before the unrecognised-name list, because this
    // one has a specific answer rather than "check the spelling".
    let retired = config::retired_in_use();
    if !retired.is_empty() {
        println!("\nSet, but no longer read by Lucida:");
        for (old, new) in retired {
            println!("  {old}  (renamed — use {new})");
        }
    }

    // A name Lucida does not know is the silent failure worth surfacing: the
    // file looks right, the value is there, and nothing ever reads it.
    // A retired name is excluded: it was reported just above with a specific
    // answer, and listing it again under "check the spelling" would contradict
    // that — the spelling is not what is wrong with it.
    let unrecognised: Vec<String> = config::keys_in_file()
        .into_iter()
        .filter(|name| {
            !config::KNOWN_KEYS.iter().any(|(known, _)| known == name)
                && config::replacement_for(name).is_none()
        })
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
/// A setting name is spelled the way an environment variable is; anything else
/// is a typo worth catching before it reaches a file nothing will ever read.
fn validate_setting_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!(
            "`{name}` is not a valid setting name — expected something like GEMINI_API_KEY"
        );
    }
    Ok(())
}

/// Whether `line` assigns `name`, allowing the `export ` prefix that comes along
/// when a fragment of a shell profile is pasted in.
fn assigns(line: &str, name: &str) -> bool {
    let bare = line.trim().strip_prefix("export ").unwrap_or(line.trim());
    bare.split_once('=').is_some_and(|(key, _)| key.trim() == name)
}

/// Removes one setting from the config file.
///
/// The counterpart to `--set`, and the reason it exists is that changing a key
/// otherwise means remembering where the file lives. It edits the file **in
/// use** rather than the preferred location: `--set` writes to the preferred
/// path, but a stale value can be sitting in a file found further down the
/// search order, or in one named by `LUCIDA_CONFIG`. Removing from anywhere else
/// would report success and change nothing.
fn remove_config(name: &str) -> Result<()> {
    let name = name.trim();
    validate_setting_name(name)?;

    let Some(path) = config::source().map(|p| p.to_path_buf()) else {
        anyhow::bail!(
            "no config file was found, so there is nothing to remove from.\n\n\
             `lucida config` lists where one is looked for."
        );
    };

    let existing = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;

    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| !assigns(line, name))
        .collect();

    // Idempotent, but never silent: removing something that was not there is a
    // typo often enough to be worth saying out loud.
    if kept.len() == existing.lines().count() {
        eprintln!(
            "{name} is not in {}, so there is nothing to remove.",
            path.display()
        );
        println!("{}", path.display());
        return Ok(());
    }

    let mut body = kept.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    config::write_replacing(&path, &body, true)?;

    eprintln!("Removed {name} from {}.", path.display());

    // Removing a key is usually a step in changing which one is used, so say
    // what answers now. Under the file-wins rule this is the moment an
    // environment value stops being shadowed and starts being the credential.
    if std::env::var(name).is_ok_and(|v| !v.trim().is_empty()) {
        eprintln!("Note: {name} is set in this environment, so that value now applies.");
    }

    println!("{}", path.display());
    Ok(())
}

fn set_config(name: &str) -> Result<()> {
    let name = name.trim();
    validate_setting_name(name)?;

    // Writing a retired name would file a value nothing reads, then report
    // success — the silent drop this design exists to refuse. Name the
    // replacement rather than accepting the write.
    if let Some(replacement) = config::replacement_for(name) {
        anyhow::bail!(
            "`{name}` is no longer read — it was renamed to `{replacement}`.\n\n\
             Set that instead:\n  lucida config --set {replacement}\n\n\
             And clear the old one if it is still in the file:\n  \
             lucida config --remove {name}"
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
        .context(
            "could not determine a config location: none of XDG_CONFIG_HOME, HOME or \
             USERPROFILE is set",
        )?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let assignment = format!("{name}={value}");

    let target = lines.iter().position(|line| assigns(line, name));

    let replaced = target.is_some();
    match target {
        Some(at) => lines[at] = assignment,
        None => lines.push(assignment),
    }

    let mut body = lines.join("\n");
    body.push('\n');
    config::write_replacing(&path, &body, true)?;

    // The value is never echoed — the whole point of taking it on stdin.
    eprintln!(
        "{} {name} in {}.",
        if replaced { "Updated" } else { "Added" },
        path.display()
    );

    // Setting a name the shell also exports is the reason the precedence rule
    // was reversed, so say plainly which value now applies. Silence here is what
    // made the old behaviour so confusing: the write succeeded, the report said
    // so, and the ambient key kept being used.
    if std::env::var(name).is_ok_and(|v| !v.trim().is_empty()) {
        eprintln!(
            "Note: {name} is also set in this environment. Lucida will use the value \
             you just set — the config file takes precedence."
        );
    }
    println!("{}", path.display());
    Ok(())
}

fn init_config() -> Result<()> {
    let path = config::preferred_path()
        .context(
            "could not determine a config location: none of XDG_CONFIG_HOME, HOME or \
             USERPROFILE is set",
        )?;

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

    config::write_replacing(&path, &config::template(), true)?;

    eprintln!(
        "Wrote {}.\n\nEvery line is commented out, so nothing changed yet. \
         Uncomment the key you need\nand set it, then check with `lucida config`.",
        path.display()
    );
    println!("{}", path.display());
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
                notes.push("Imagen family — a different endpoint, not implemented".into());
            }
            // Generated for every provider, not written for one. The three
            // openai ids that stop working on 2026-12-01 used to be listed here
            // exactly like the ones that will still exist next year.
            if let Some(note) = provider::retirement_note(model) {
                notes.push(note);
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
    println!("  mask            {}", caps.mask.describe());
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

    if let Some(commentary) = &image.commentary
        && !commentary.is_empty()
    {
        eprintln!("{commentary}");
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
/// Identifies an image format from its first bytes.
///
/// Exists because a filename is a claim about the bytes, not a fact: the one
/// place that guessed from the extension treated every non-`.png` reference as
/// JPEG, so a `.webp` source failed dimension-reading and was silently sent
/// with `auto` geometry — the reshaping its sizing exists to prevent.
pub fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        _ if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" => {
            Some("image/webp")
        }
        _ => None,
    }
}

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
        // WebP is one RIFF container holding one of three layouts, told apart
        // by the chunk following "WEBP". Each stores dimensions differently.
        "image/webp" => match bytes.get(12..16)? {
            // Extended: canvas size as 24-bit little-endian minus-one fields,
            // after a flags byte and three reserved bytes.
            b"VP8X" => {
                let le24 =
                    |b: &[u8]| u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16;
                Some((le24(bytes.get(24..27)?) + 1, le24(bytes.get(27..30)?) + 1))
            }
            // Lossy: dimensions follow the 3-byte frame tag and the sync code,
            // 14 bits each in a 16-bit little-endian field.
            b"VP8 " => {
                if bytes.get(23..26)? != [0x9D, 0x01, 0x2A] {
                    return None;
                }
                let w = u16::from_le_bytes([*bytes.get(26)?, *bytes.get(27)?]) & 0x3FFF;
                let h = u16::from_le_bytes([*bytes.get(28)?, *bytes.get(29)?]) & 0x3FFF;
                Some((u32::from(w), u32::from(h)))
            }
            // Lossless: a signature byte, then width-1 and height-1 as
            // consecutive 14-bit fields in a little-endian bit stream.
            b"VP8L" => {
                if *bytes.get(20)? != 0x2F {
                    return None;
                }
                let b = bytes.get(21..25)?;
                let w = 1 + (u32::from(b[1] & 0x3F) << 8 | u32::from(b[0]));
                let h = 1 + (u32::from(b[3] & 0x0F) << 10
                    | u32::from(b[2]) << 2
                    | u32::from(b[1] >> 6));
                Some((w, h))
            }
            _ => None,
        },
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

/// Writes `bytes` to `path` without ever leaving it truncated.
///
/// Stage beside the target, then rename. A rename within one directory either
/// happens or does not, so a crash, a signal or a full disk mid-write leaves
/// whatever was there before exactly as it was — where a truncating `fs::write`
/// leaves a file that is part one image and part another, or no image at all.
///
/// Staged in the target's own directory rather than a temp dir, because a rename
/// across filesystems is a copy-and-delete and hands the guarantee straight back.
///
/// `private` restricts the staged file *before* the rename: a file chmodded
/// after the write is world-readable for the moment it first holds a secret.
pub fn write_atomically(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let staged = staging_path(path);

    let staged_then = |result: Result<()>| -> Result<()> {
        if result.is_err() {
            // A staged file left behind is litter in someone's directory, and
            // one holding a key is worse than litter.
            let _ = std::fs::remove_file(&staged);
        }
        result
    };

    staged_then(
        std::fs::write(&staged, bytes).with_context(|| format!("writing {}", staged.display())),
    )?;

    if private {
        staged_then(config::restrict_to_owner(&staged))?;
    }

    staged_then(
        std::fs::rename(&staged, path)
            .with_context(|| format!("replacing {} with {}", path.display(), staged.display())),
    )
}

/// Where a pending write lives until it takes the target's name.
///
/// Dot-prefixed so it does not appear in a directory listing between the write
/// and the rename. Stamped with the process id so two Lucidas cannot stage over
/// each other, and with a counter because two writes can now be in flight
/// *within* one process — a batch render, or two MCP tool calls — and two
/// writers sharing a staging path would produce exactly the torn file this
/// exists to prevent.
fn staging_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        ".{name}.lucida-{}-{nonce}",
        std::process::id()
    ))
}

/// Writes an image to `path`, creating parent directories, and returns the
/// absolute path actually written.
///
/// Atomic, and not incidentally: `lucida edit` defaults its output to its own
/// *input*, so the file being overwritten here is routinely the user's original
/// and the only copy of it. A truncating write that failed halfway — a full
/// disk, a signal — destroyed the source and the edit together.
pub fn write_image(path: impl AsRef<Path>, bytes: &[u8]) -> Result<PathBuf> {
    let path = path.as_ref();

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    write_atomically(path, bytes, false)?;

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

    /// The shopfront surfaces — the package description and the `--help` banner
    /// — are the first and often only thing anyone reads, and they are pure
    /// prose, so nothing generates them and nothing caught them rotting. The
    /// repository description said "Generate and edit images with Google's
    /// Gemini models" through four providers and all of video.
    ///
    /// Checked against `Backend::ALL`, so provider six fails here rather than
    /// going unmentioned for a release. Video is checked by name for the same
    /// reason: it was the whole capability the description omitted.
    #[test]
    fn the_shopfront_names_every_provider_and_video() {
        use clap::CommandFactory;

        let banner = Cli::command().get_about().map(|a| a.to_string()).unwrap();

        for surface in [env!("CARGO_PKG_DESCRIPTION"), banner.as_str()] {
            for backend in Backend::ALL {
                assert!(
                    surface.contains(backend.product_name()),
                    "`{}` is missing from a surface someone reads before installing: {surface}",
                    backend.product_name()
                );
            }
            assert!(
                surface.contains("Veo") || surface.contains("video"),
                "video is not mentioned at all: {surface}"
            );
        }
    }

    /// The property that matters — an interrupted write leaving the previous
    /// file intact — is the one a test cannot easily provoke, so what is checked
    /// is the mechanism that provides it: the bytes are never written to the
    /// target's own name, and the staging file lands in the target's own
    /// directory. A rename across filesystems is a copy-and-delete, which would
    /// hand the guarantee straight back.
    #[test]
    fn a_staged_write_never_touches_the_target_until_it_is_whole() {
        let path = std::path::Path::new("/tmp/gallery/cat.png");
        let staged = staging_path(path);

        assert_ne!(staged, path);
        assert_eq!(staged.parent(), path.parent());
        assert!(
            staged.file_name().unwrap().to_string_lossy().starts_with('.'),
            "the staging file shows up in a listing mid-write: {}",
            staged.display()
        );
    }

    /// Two writes can be in flight at once — a batch, or two MCP tool calls —
    /// and two writers sharing a staging path would produce exactly the torn
    /// file staging exists to prevent.
    #[test]
    fn concurrent_writes_do_not_share_a_staging_path() {
        let path = std::path::Path::new("image.png");
        assert_ne!(staging_path(path), staging_path(path));
    }

    /// `lucida edit` defaults its output to its own input, so the file being
    /// overwritten is routinely the user's original and the only copy of it.
    #[test]
    fn writing_an_image_over_itself_leaves_a_whole_file() {
        let dir = std::env::temp_dir().join(format!("lucida-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cat.png");

        std::fs::write(&path, b"original").unwrap();
        write_image(&path, b"edited").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"edited");

        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(left, vec!["cat.png"], "a staging file survived: {left:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A started render must always be collectable from the terminal, and the
    /// only thing that makes it so is the operation id being on screen. It was
    /// printed in exactly one branch — the 15-minute deadline — so every other
    /// way of leaving the wait lost a paid render.
    #[test]
    fn the_resume_notice_carries_the_id_and_the_command_that_uses_it() {
        let notice = video::resume_notice("operations/abc123");
        assert!(notice.contains("operations/abc123"), "{notice}");
        assert!(
            notice.contains("lucida check operations/abc123"),
            "the id alone is not a way forward; the command has to be there: {notice}"
        );
    }

    /// `--no-wait` is the CLI catching up with the MCP surface, which has
    /// returned an operation id rather than blocking since it existed.
    #[test]
    fn video_can_start_a_render_without_waiting_for_it() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["lucida", "video", "a fox running", "--no-wait"])
            .expect("--no-wait must parse");
        match cli.command {
            Command::Video { no_wait, .. } => assert!(no_wait),
            _ => panic!("`video --no-wait` parsed as the wrong subcommand"),
        }
    }

    /// The `--mask` help is the only mask surface that cannot be generated, so
    /// it is the one that has to be guarded.
    ///
    /// A clap attribute takes a literal, which is why this string is
    /// hand-maintained — and it is where "openai only, and advisory" survived a
    /// release after both halves had stopped being true. Any provider name or
    /// either semantics word here means a fact was copied out of `MaskSupport`
    /// into a place nothing updates; the help may only point at the answer that
    /// is generated.
    ///
    /// The banned names come from `Backend::ALL`, so a sixth provider is covered
    /// the day it lands rather than the day someone remembers this test.
    #[test]
    fn the_mask_help_states_no_capability_fact() {
        use clap::CommandFactory;

        let command = Cli::command();
        let generate = command
            .get_subcommands()
            .find(|c| c.get_name() == "generate")
            .expect("no `generate` subcommand");
        let mask = generate
            .get_arguments()
            .find(|a| a.get_id() == "mask")
            .expect("no `--mask` argument");
        let help = mask
            .get_help()
            .expect("`--mask` has no help")
            .to_string()
            .to_lowercase();

        for backend in Backend::ALL {
            assert!(
                !help.contains(backend.name()),
                "the --mask help names `{}` — which providers mask is generated, \
                 and a literal here cannot follow it",
                backend.name()
            );
        }
        for claim in ["advisory", "binding"] {
            assert!(
                !help.contains(claim),
                "the --mask help says `{claim}` — the kind of mask a provider has \
                 lives in MaskSupport, and every generated surface reads it"
            );
        }

        // Saying what it does not contain is only useful alongside where the
        // answer is — the same bargain the skill makes.
        assert!(help.contains("lucida models"), "{help}");
    }

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
    fn mime_is_sniffed_from_magic_bytes_not_names() {
        assert_eq!(sniff_mime(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A]), Some("image/png"));
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0; 4]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_mime(&webp), Some("image/webp"));
        assert_eq!(sniff_mime(b"GIF89a"), None);
        assert_eq!(sniff_mime(&[]), None);
    }

    #[test]
    fn webp_dimensions_come_out_of_all_three_container_layouts() {
        // VP8X: canvas size as 24-bit minus-one fields after flags + reserved.
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        vp8x.extend_from_slice(&[10, 0, 0, 0]); // chunk size
        vp8x.extend_from_slice(&[0; 4]); // flags + reserved
        vp8x.extend_from_slice(&(1360u32 - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(768u32 - 1).to_le_bytes()[..3]);
        assert_eq!(image_dimensions(&vp8x, "image/webp"), Some((1360, 768)));

        // VP8 (lossy): frame tag, sync code, then 14-bit LE dimensions.
        let mut vp8 = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        vp8.extend_from_slice(&[0; 4]); // chunk size
        vp8.extend_from_slice(&[0; 3]); // frame tag
        vp8.extend_from_slice(&[0x9D, 0x01, 0x2A]);
        vp8.extend_from_slice(&1024u16.to_le_bytes());
        vp8.extend_from_slice(&576u16.to_le_bytes());
        assert_eq!(image_dimensions(&vp8, "image/webp"), Some((1024, 576)));

        // VP8L (lossless): 1024x576 packed as consecutive 14-bit fields.
        let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L".to_vec();
        vp8l.extend_from_slice(&[0; 4]); // chunk size
        vp8l.push(0x2F); // signature
        vp8l.extend_from_slice(&[0xFF, 0xC3, 0x8F, 0x00]);
        assert_eq!(image_dimensions(&vp8l, "image/webp"), Some((1024, 576)));
    }

    /// The last silent drop from the review: `--workflow` ignored an explicit
    /// `--model` without a word, because the provider cannot tell "typed" from
    /// "defaulted" once into_request fills the default in. So it is refused
    /// here, where explicitness is still visible — same precedent as the
    /// `--ref` + `--workflow` refusal in comfy.
    #[test]
    fn a_workflow_refuses_an_explicit_model() {
        let opts = ImageOptions {
            workflow: Some("graph.json".into()),
            model: Some("klein".into()),
            ..Default::default()
        };
        let error = opts
            .into_request("x".into(), Vec::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains("--workflow"), "must name the conflict: {error}");
        assert!(error.contains("--model"));

        // A workflow alone still passes — the refusal is the combination.
        let alone = ImageOptions {
            workflow: Some("graph.json".into()),
            provider: Some("comfyui".into()),
            ..Default::default()
        };
        assert!(alone.into_request("x".into(), Vec::new()).is_ok());
    }

    #[test]
    fn truncated_or_unknown_data_reports_nothing_rather_than_guessing() {
        assert_eq!(image_dimensions(&[0x89, b'P', b'N', b'G'], "image/png"), None);
        assert_eq!(image_dimensions(&[0xFF, 0xD8], "image/jpeg"), None);
        assert_eq!(image_dimensions(&[0; 64], "image/webp"), None);
    }
}
