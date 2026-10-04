//! Lucida — image and video generation.
//!
//! Named for the camera lucida, the optical device that let artists trace what
//! they saw onto paper.
//!
//! One binary, two front ends: a plain CLI for shell and script use, and an MCP
//! server (`lucida mcp`) so agents can call it as a first-class tool.
//!
//! Images come from one of six providers — Google's Gemini models, a local
//! ComfyUI, hosted FLUX from Black Forest Labs, Stability AI, OpenAI, or
//! Runway — chosen from the model id unless `--provider` says otherwise. Video
//! comes from Veo, Runway or Kling.

mod bfl;
mod cancel;
mod clock;
mod comfy;
mod config;
mod genai;
mod kling;
mod ledger;
mod masked;
mod mcp;
mod openai;
mod out;
mod provider;
mod retry;
mod runway;
mod setup;
mod skill;
mod spend;
mod stability;
#[cfg(test)]
mod testserver;
mod update;
mod video;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use provider::{Aspect, Backend, ImageProvider, ImageRequest, Size, infer_backend};
use std::path::{Path, PathBuf};
use video::VideoRequest;

#[derive(Parser)]
#[command(
    name = "lucida",
    version,
    about = "Generate images and video with Google Gemini, Veo, Runway, Kling, a local ComfyUI, FLUX, Stability AI or OpenAI",
    long_about = "Generate and edit images with Google Gemini, a local ComfyUI, \
                  hosted FLUX from Black Forest Labs, Stability AI, OpenAI or \
                  Runway, and video with Veo, Runway or Kling.\n\n\
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
                  Runway reads RUNWAY_API_KEY, one key for both its images and \
                  its video. Kling reads KLINGAI_API_KEY; every Kling render \
                  costs credits.\n\n\
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

    /// Emit one JSON object on stdout instead of prose. Human messages still go
    /// to stderr, so the document stays clean.
    ///
    /// Global rather than per-subcommand: a caller that wants machine output
    /// wants it from whatever it happens to call, and having to remember which
    /// subcommands support it is the kind of detail that turns into a bug.
    #[arg(long, global = true)]
    json: bool,

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

    /// Which provider to use: google, comfyui, bfl, stability, openai or runway. Inferred from the model when omitted.
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

    /// Seed, for a reproducible render (comfyui, bfl, stability and runway; google and openai have none)
    #[arg(long)]
    seed: Option<u64>,

    /// Sampling steps (comfyui, and bfl on flux-2-flex / flux-dev only)
    #[arg(long)]
    steps: Option<u32>,

    /// Guidance scale (comfyui, and bfl on flux-2-flex / flux-dev only)
    #[arg(short, long)]
    guidance: Option<f32>,

    /// Render this many candidates, written as name-1, name-2 and so on.
    ///
    /// Spelled in full because `-n` already means `--negative` here, and
    /// re-using it would break every existing caller to save two keystrokes.
    ///
    /// At least 1: a `--count 0` used to succeed having rendered nothing, which
    /// a script reads as a batch that worked.
    #[arg(long, default_value_t = 1, value_name = "N", value_parser = at_least_one)]
    count: usize,

    /// Print what would be sent — provider, model, every resolved parameter and
    /// the estimated cost — and stop without rendering.
    #[arg(long)]
    dry_run: bool,
}

/// `--count`'s parser. clap's `range` exists for the fixed-width integers and not
/// `usize`, and the count is a `usize` everywhere it is used.
fn at_least_one(text: &str) -> std::result::Result<usize, String> {
    match text.parse::<usize>() {
        Ok(0) => Err("must be at least 1; 0 would render nothing".into()),
        Ok(n) => Ok(n),
        Err(_) => Err(format!("`{text}` is not a whole number")),
    }
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

    /// Generate a video with Veo, Runway or Kling. Renders take minutes and
    /// bill per second.
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

        /// Model id or alias: veo, veo-standard, runway, gen4-turbo, kling…
        ///
        /// Optional, and that matters: a clap `default_value` here would mean
        /// `--provider kling` with no model sends *Veo's* default model id to
        /// Kling, because nothing could tell "unspecified" from "explicitly the
        /// Veo default". `ImageOptions::into_request` solved this for images and
        /// says so; video repeated the mistake until 2026-08-09.
        #[arg(short, long)]
        model: Option<String>,

        /// Which provider to use: google, runway or kling. Inferred from the model
        /// when omitted.
        #[arg(long)]
        provider: Option<String>,

        /// Seconds of output. Every video provider bills per second, so this is
        /// the flag where a wrong value is expensive rather than annoying — what
        /// each accepts is a capability, not a fixed list.
        #[arg(short = 'd', long)]
        duration: Option<u32>,

        /// Seed, for a reproducible render. Runway has one; Veo and Kling do not.
        #[arg(long)]
        seed: Option<u64>,

        /// Quality tier, where the provider has them — kling takes std, pro or
        /// master. `lucida models --provider <name>` says which.
        #[arg(long)]
        mode: Option<String>,

        /// Start the render and print its operation id instead of waiting.
        /// Collect it later with `lucida check`.
        #[arg(long)]
        no_wait: bool,

        /// Print what would be sent — provider, model, every resolved parameter
        /// and the estimated cost — and stop without rendering.
        ///
        /// Video bills per second, and there was previously no way to ask "what
        /// would you send?" without sending it. Confirming that `--provider X`
        /// picks X's own model, or that a duration is in range, cost a render.
        #[arg(long)]
        dry_run: bool,
    },

    /// Resume a video render by operation id, e.g. after a timeout
    Check {
        /// The operation id reported when the render started
        operation: String,

        /// Which provider started it. Inferred from the id's shape when omitted
        /// — Veo's are `operations/...` and Runway's are bare UUIDs.
        #[arg(long)]
        provider: Option<String>,

        /// Where to write the video once it is ready
        #[arg(short, long, default_value = "video.mp4")]
        out: PathBuf,
    },

    /// Video renders that were started and never collected
    Ops,

    /// Recent renders, newest last
    History {
        /// How many to show
        #[arg(short = 'n', long, default_value_t = 20)]
        count: usize,
    },

    /// List the models a provider can reach, and what it can be asked for.
    /// Answers for the video providers too, including remaining credits
    Models {
        /// Which provider to interrogate: google, comfyui, bfl, stability, openai, runway or kling
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
    out::set_json(cli.json);

    // `mcp` is excluded because its client spawns and kills it constantly, so a
    // check there is a network round trip per launch; `update` because it has
    // just done this properly and would otherwise say it twice. `--json` too:
    // a notice on stderr is harmless, but a caller asking for machine output is
    // not asking for news.
    let announce =
        !matches!(cli.command, Command::Mcp | Command::Update { .. }) && !cli.json;

    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            let code = out::code_for(&e);
            eprintln!("error: {e:#}");
            out::emit_error(&e, code);
            // Deliberately no update notice on the way out: the error is what
            // the reader needs, and appending unrelated news to a failure is
            // noise.
            std::process::exit(code);
        }
    };

    // After the work, never before — so a slow or unreachable GitHub costs a
    // few seconds at exit rather than delaying a render. It installs nothing.
    if announce {
        update::notify_if_due(env!("CARGO_PKG_VERSION"));
    }

    if code != out::OK {
        std::process::exit(code);
    }
}

/// Refuses `--json` on a command that has no JSON document to give.
///
/// `out.rs` promises one JSON object on stdout whatever happens, and `--json` is
/// global so that a caller never has to remember which subcommands take it. These
/// five print prose with `println!`, so honouring the flag would mean a stream
/// that is not JSON under a flag that says it is — and ignoring it, which is what
/// they did, made the parser on the other end fail on the first word. The flag is
/// refused instead, by the same rule as any parameter that cannot be honoured.
///
/// Listed by the commands that *lack* a document, not by those that have one, so a
/// new command inherits the promise until someone says it cannot keep it. `mcp` is
/// in neither list: its stdout is JSON-RPC framing, a stream of documents the
/// protocol defines, and the flag changes nothing about it.
fn refuse_json_without_a_document(command: &Command) -> Result<()> {
    let name = match command {
        Command::Models { .. } => "models",
        Command::Config { .. } => "config",
        Command::Skill => "skill",
        Command::Setup { .. } => "setup",
        Command::Update { .. } => "update",
        _ => return Ok(()),
    };
    Err(anyhow::Error::new(out::Refused(format!(
        "`lucida {name}` prints text and has no JSON document, so `--json` cannot \
         be honoured. Run it without `--json`; nothing was done."
    ))))
}

/// Returns the exit code rather than `()`, because "still working" is an
/// outcome and not an error — `lucida check` has to be able to say so without
/// pretending something went wrong.
fn run(cli: Cli) -> Result<i32> {
    // Before any command runs: `config --init` writes a file and `update` goes to
    // the network, and a refusal reported after either would be a lie about
    // "nothing was done".
    if cli.json {
        refuse_json_without_a_document(&cli.command)?;
    }

    match cli.command {
        Command::Mcp => mcp::serve().map(|()| out::OK),

        // Image backends first, then video. A provider of both media — `google`
        // and, since Runway's image lane, `runway` — lists both: before that lane
        // `--provider runway` showed its video models, and gaining images must
        // not hide them. Both halves run even when the first fails, since a
        // missing key for one lane says nothing about the other's table.
        Command::Models { provider } => match Backend::parse(&provider) {
            Ok(backend) => {
                let video = provider::VideoBackend::parse(&provider).ok();
                // Two capability tables read alike, so each half says which it is.
                if video.is_some() {
                    println!("== Images ==\n");
                }
                let images = list_models(backend);
                match video {
                    Some(video) => {
                        println!("\n== Video ==\n");
                        let videos = list_video_models(video);
                        images.and(videos).map(|()| out::OK)
                    }
                    None => images.map(|()| out::OK),
                }
            }
            Err(image_error) => match provider::VideoBackend::parse(&provider) {
                Ok(backend) => list_video_models(backend).map(|()| out::OK),
                // The image error, not the video one: six of the seven providers
                // are image providers, so that is the more likely mistake and
                // the more useful list to be shown.
                Err(_) => Err(image_error),
            },
        },

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
            setup::run(scope, dry_run, yes).map(|()| out::OK)
        }

        Command::Skill => skill::print().map(|()| out::OK),

        Command::Update { check, yes } => {
            let mode = match (check, yes) {
                (true, _) => update::Mode::Check,
                (_, true) => update::Mode::Yes,
                _ => update::Mode::Ask,
            };
            update::Updater::new()?.run(mode).map(|()| out::OK)
        }

        Command::Config { init, set, remove } => match (set, remove) {
            (Some(name), _) => set_config(&name).map(|()| out::OK),
            (_, Some(name)) => remove_config(&name).map(|()| out::OK),
            _ if init => init_config().map(|()| out::OK),
            _ => {
                show_config();
                Ok(out::OK)
            }
        },

        Command::Generate {
            prompt,
            out,
            reference,
            opts,
        } => {
            let (count, dry_run) = (opts.count, opts.dry_run);
            let (request, backend, source) = opts.into_request(prompt, reference)?;
            execute(request, backend, out, count, dry_run, source).map(|()| out::OK)
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
            let (count, dry_run) = (opts.count, opts.dry_run);
            let (request, backend, source) = opts.into_request(prompt, references)?;
            execute(request, backend, destination, count, dry_run, source).map(|()| out::OK)
        }

        Command::Check {
            operation,
            provider,
            out,
        } => {
            let backend = match &provider {
                Some(name) => provider::VideoBackend::parse(name)?,
                None => provider::infer_video_backend_from_operation(&operation),
            };
            let polled = open_video(backend)?.poll(&operation);
            if let Err(error) = &polled {
                // A render the provider says is over leaves `lucida ops`; any
                // other failure leaves it there to be asked about again.
                ledger::note_failure(backend.name(), &operation, error);
            }
            match polled? {
                video::VideoStatus::Pending => {
                    // Its own exit code. This used to be 0 with nothing on
                    // stdout, which a polling script cannot tell apart from a
                    // render that finished and was written — so a loop built on
                    // it either spins forever or abandons something already paid
                    // for.
                    eprintln!("Still rendering. Try again in half a minute.");
                    out::emit(pending_document(backend.name(), &operation));
                    Ok(out::PENDING)
                }
                video::VideoStatus::Done(bytes) => {
                    let written = write_image(correct_extension(&out, "video/mp4"), &bytes)?;
                    eprintln!(
                        "Wrote {} ({:.1} MB)",
                        written.display(),
                        bytes.len() as f64 / 1_048_576.0
                    );
                    // Retires the operation from `lucida ops`, wherever it was
                    // started from — the outstanding list is derived from the
                    // log rather than stored, so nothing has to be told.
                    ledger::video_done(backend.name(), &operation, &written.to_string_lossy());
                    if out::json() {
                        out::emit(serde_json::json!({
                            "ok": true,
                            "status": "done",
                            "operation": operation,
                            "path": written.to_string_lossy(),
                            "bytes": bytes.len(),
                            "exit_code": out::OK,
                        }));
                    } else {
                        println!("{}", written.display());
                    }
                    Ok(out::OK)
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
            provider,
            duration,
            seed,
            mode,
            no_wait,
            dry_run,
        } => {
            // Named explicitly, or inferred from the model id — the same rule
            // images have used since `--provider` became optional there.
            // Explicit provider wins and supplies the model default; otherwise
            // the model names the provider. Either way the pair is consistent,
            // which is the whole point.
            let (backend, default_source) = match &provider {
                Some(name) => (provider::VideoBackend::parse(name)?, None),
                None => match &model {
                    Some(model) => (provider::infer_video_backend(model), None),
                    None => {
                        let (backend, source) =
                            provider::resolve_default::<provider::VideoBackend>()?;
                        (backend, Some(source))
                    }
                },
            };
            announce_default(&default_source, backend.name());
            let model = model.unwrap_or_else(|| backend.default_model().to_string());

            // The image list annotates a retired id where it is *displayed*;
            // video has no such list — every alias points at a current model, so
            // a retired id can only arrive by being typed. Warn rather than
            // refuse: `gemini-*-image-preview` is the standing proof that an
            // announced shutdown and a provider's actual behaviour can disagree
            // for months, and refusing on the announcement would make Lucida
            // wrong in the direction that costs the user a render they could
            // have had.
            if let Some(note) = provider::retirement_note(&model) {
                eprintln!(
                    "⚠ {model} {note} — expect this to fail. Current ids: {}.",
                    video::VIDEO_ALIASES
                        .iter()
                        .map(|(alias, _)| *alias)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            let request = VideoRequest {
                prompt,
                model,
                aspect: aspect.map(|a| Aspect::parse(&a)).transpose()?,
                resolution,
                negative_prompt: negative,
                image,
                duration,
                seed,
                mode,
            };

            // Before a client exists, so asking Veo for a seed says so with no
            // key set — the key was never the problem.
            let caps = provider::video_capabilities_for(backend, &request.model);
            caps.check(&request)?;

            let resolved = resolve_video_model(backend, &request.model);

            // Video is the one lane billed per second, so the rate is stated
            // before the render rather than after it — this is where a wrong
            // parameter is expensive rather than merely annoying.
            let price = spend::video_price(backend, &resolved, request.duration);
            let reservation = spend::check(price, "video render")?;

            // After the capability and budget checks, so a dry run reports the
            // same refusals a real one would, and before any client exists, so
            // it needs no credential and sends nothing.
            if dry_run {
                report_plan(serde_json::json!({
                    "ok": true,
                    "status": "dry-run",
                    "provider": backend.name(),
                    "provider_source": default_source.as_ref().map(|s| s.tag()),
                    "model": resolved,
                    "prompt": request.prompt,
                    "aspect": request.aspect.map(|a| a.to_string()),
                    // Resolution and the negative prompt are sent too; they
                    // were the two fields of the request this left out.
                    "resolution": request.resolution,
                    "negative_prompt": request.negative_prompt,
                    "duration": request.duration,
                    "mode": request.mode,
                    "seed": request.seed,
                    "image": request.image,
                    "estimated_usd": price.against_budget(),
                    "exit_code": out::OK,
                }))?;
                return Ok(out::OK);
            }

            eprintln!("Rendering with {resolved} — {}.", price.describe());

            let client = open_video(backend)?;

            // Started and waited on in two visible steps, so the operation id
            // exists out here where it can be printed and written down. It used
            // to live inside one blocking call, which is why nothing but the
            // deadline message ever mentioned it.
            let operation = client.start(&request)?;
            ledger::video_started(
                backend.name(),
                &resolved,
                &request.prompt,
                &operation,
                price.against_budget(),
            );
            // The started entry carries the spend, so the hold can go — before
            // the wait, which can run for minutes.
            drop(reservation);
            eprintln!("{}", video::resume_notice(&operation));

            // The shape the MCP surface has had since it existed — start, hand
            // back the id, let the caller collect it — finally available to the
            // shell too. Unattended callers want it: a render that outlives the
            // process is fine, a process that must survive the render is not.
            if no_wait {
                if out::json() {
                    out::emit(serde_json::json!({
                        "ok": true,
                        "status": "started",
                        "operation": operation,
                        "model": resolved,
                        "estimated_usd": price.against_budget(),
                        "exit_code": out::OK,
                    }));
                } else {
                    // The id on stdout, where the path goes when we do wait: one
                    // line, the useful part, capturable by a script.
                    println!("{operation}");
                }
                return Ok(out::OK);
            }

            // A wait that runs out is not a failure: the render is still going
            // and already billed, so it leaves the ledger as it is and exits 3.
            let waited = await_video(client.as_ref(), backend.name(), &operation, Pacing::DEFAULT)
                .inspect_err(|error| {
                    ledger::note_failure(backend.name(), &operation, error);
                })?;
            let bytes = match waited {
                Waited::Done(bytes) => bytes,
                Waited::StillRunning => {
                    eprintln!("{}", still_running_notice(backend.name(), &operation));
                    out::emit(pending_document(backend.name(), &operation));
                    return Ok(out::PENDING);
                }
            };
            let written = write_image(correct_extension(&out, "video/mp4"), &bytes)?;
            eprintln!(
                "Wrote {} ({:.1} MB)",
                written.display(),
                bytes.len() as f64 / 1_048_576.0
            );
            ledger::video_done(backend.name(), &operation, &written.to_string_lossy());
            if out::json() {
                out::emit(serde_json::json!({
                    "ok": true,
                    "status": "done",
                    "operation": operation,
                    "path": written.to_string_lossy(),
                    "model": resolved,
                    "bytes": bytes.len(),
                    "estimated_usd": price.against_budget(),
                    "exit_code": out::OK,
                }));
            } else {
                println!("{}", written.display());
            }
            Ok(out::OK)
        }

        Command::Ops => show_operations().map(|()| out::OK),

        Command::History { count } => show_history(count).map(|()| out::OK),
    }
}

/// Video renders that were started and never collected.
///
/// The command the ledger exists for. An agent starts a render, hands back an
/// operation id, and its session ends; the id then lives only in a transcript
/// nobody will read again, and a render that is already being billed is
/// unreachable. This is where it is now written down.
fn show_operations() -> Result<()> {
    if ledger::disabled() {
        eprintln!(
            "The render ledger is off (LUCIDA_NO_LEDGER is set), so nothing was \
             recorded to list."
        );
        return Ok(());
    }

    let open = ledger::outstanding();

    if out::json() {
        out::emit(serde_json::json!({
            "ok": true,
            "operations": open,
            "exit_code": out::OK,
        }));
        return Ok(());
    }

    if open.is_empty() {
        println!("No video renders are waiting to be collected.");
        return Ok(());
    }

    println!("Video renders started and not yet collected:\n");
    for entry in &open {
        let operation = entry["operation"].as_str().unwrap_or("?");
        println!(
            "  {}  {}\n    {}\n    {}\n",
            clock::stamp(entry["at"].as_i64().unwrap_or(0)),
            entry["model"].as_str().unwrap_or("?"),
            truncate(entry["prompt"].as_str().unwrap_or(""), 68),
            check_command(entry, operation),
        );
    }
    Ok(())
}

/// The command that collects a render, naming the provider that started it when
/// the ledger knows. An entry from before the ledger recorded one gets the bare
/// form, which infers the provider from the id.
fn check_command(entry: &serde_json::Value, operation: &str) -> String {
    match ledger::recorded_provider(entry) {
        Some(provider) => check_command_for(provider, operation),
        None => format!("lucida check {operation}"),
    }
}

/// [`check_command`] when the provider is known for certain, as it is for a
/// render this process started.
fn check_command_for(provider: &str, operation: &str) -> String {
    format!("lucida check --provider {provider} {operation}")
}

/// The `--json` document for a render that is still running. One shape for
/// `lucida check` and for a wait that ran out, so a wrapper polling either
/// parses the same thing — and carries the provider, because the id alone does
/// not always say which one to ask.
fn pending_document(provider: &str, operation: &str) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "status": "pending",
        "provider": provider,
        "operation": operation,
        "exit_code": out::PENDING,
    })
}

/// What a wait that ran out says on stderr. The second sentence is the one
/// that matters: the exit used to be 1, which a wrapper retries, and a retry
/// starts a second render and pays for it while the first is still running.
fn still_running_notice(provider: &str, operation: &str) -> String {
    format!(
        "The render is still running; the wait ran out, not the render. It is \
         already billed, so do not start another.\n\
         Collect it later with: {}",
        check_command_for(provider, operation)
    )
}

fn show_history(count: usize) -> Result<()> {
    if ledger::disabled() {
        eprintln!("The render ledger is off (LUCIDA_NO_LEDGER is set).");
        return Ok(());
    }

    let all = ledger::entries();

    if out::json() {
        let recent: Vec<_> = all.iter().rev().take(count).rev().cloned().collect();
        out::emit(serde_json::json!({
            "ok": true,
            "entries": recent,
            "estimated_usd_24h": spend::spent_recently(),
            "budget_usd": spend::budget(),
            // Beside `budget_usd` rather than folded into it, because that
            // field is `null` both for no budget and for one that cannot be
            // read, and the two mean opposite things: the first refuses
            // nothing, the second refuses every paid render. This is non-null
            // exactly when the setting is there and unreadable, worded as
            // `lucida config` words it.
            "budget_problem": spend::budget_setting().problem(),
            "exit_code": out::OK,
        }));
        return Ok(());
    }

    if all.is_empty() {
        println!("Nothing recorded yet.");
        return Ok(());
    }

    for entry in all.iter().rev().take(count).rev() {
        let seed = match entry["seed"].as_u64() {
            Some(seed) => format!("  seed {seed}"),
            None => String::new(),
        };
        println!(
            "{}  {:9} {:10} {}{seed}",
            clock::stamp(entry["at"].as_i64().unwrap_or(0)),
            entry["provider"].as_str().unwrap_or("?"),
            entry["status"].as_str().unwrap_or("?"),
            entry["path"]
                .as_str()
                .or_else(|| entry["operation"].as_str())
                .or_else(|| entry["handle"].as_str())
                .unwrap_or("?"),
        );
        let prompt = entry["prompt"].as_str().unwrap_or("");
        if !prompt.is_empty() {
            println!("    {}", truncate(prompt, 72));
        }
    }

    // The number a budget is actually enforced against, shown wherever someone
    // is already looking at what they generated. Called an estimate every time
    // it appears: the provider's invoice is the authority and this is a sum of
    // published rates, some of which are assumed upper bounds.
    let spent = spend::spent_recently();
    if spent > 0.0 {
        print!("\nEstimated spend in the last 24 hours: ${spent:.2}");
        match spend::budget_setting() {
            spend::Budget::Cap(budget) => println!(" of a ${budget:.2} LUCIDA_BUDGET"),
            spend::Budget::Unset => println!(" (no LUCIDA_BUDGET set)"),
            // Not "no budget": an unreadable one refuses every paid render.
            spend::Budget::Unreadable(raw) => println!(
                " (LUCIDA_BUDGET is `{raw}`, which is not a number — paid renders \
                 are refused until it is fixed)"
            ),
        }
    }
    Ok(())
}

/// Shortens on a character boundary. A prompt is arbitrary user text, so slicing
/// it by byte index is a panic waiting for the first accented character.
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit.saturating_sub(1)).collect::<String>() + "…"
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
    ) -> Result<(ImageRequest, Backend, Option<provider::DefaultSource>)> {
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

        // Nothing named: this is the only branch a preference may answer, and
        // it resolves once, here, before any client exists. See
        // `provider::resolve_default` for why it can never become a fallback.
        let (backend, default_source) = match (&self.provider, &self.model) {
            (Some(name), _) => (Backend::parse(name)?, None),
            (None, Some(model)) => (infer_backend(model), None),
            (None, None) => {
                let (backend, source) = provider::resolve_default::<Backend>()?;
                (backend, Some(source))
            }
        };
        announce_default(&default_source, backend.name());

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

        Ok((request, backend, default_source))
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

    // Said out loud rather than left to be discovered, because this file records
    // **prompts** — the most personal thing Lucida handles — and someone who does
    // not want them on disk should not have to find the file first to learn it
    // exists.
    match ledger::path() {
        Some(path) => println!("Render ledger: {}", path.display()),
        None if ledger::disabled() => {
            println!("Render ledger: off (LUCIDA_NO_LEDGER is set)")
        }
        None => println!("Render ledger: nowhere to write one"),
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

    // The other way a setting can be present and not do what it says. Flagged
    // here because a budget that cannot be read refuses every paid render, and
    // this is where someone looks to find out why.
    if let Some(problem) = spend::budget_setting().problem() {
        println!("\nSet, but not a value Lucida can use:");
        println!("  LUCIDA_BUDGET  ({problem} — paid renders are refused until it is fixed)");
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



/// What a video provider can be asked for, and whether it can be reached.
///
/// The video twin of [`list_models`], and it earns its place for the same reason
/// that one does: the capability table is a fact about the provider rather than
/// about your credentials, so it prints whether or not a key is present. There
/// is no model *list* to fetch — both providers' catalogues are fixed at release
/// — so what the probe buys here is the credential check and, on Runway, the
/// balance.
fn list_video_models(backend: provider::VideoBackend) -> Result<()> {
    let caps = provider::video_capabilities_for(backend, backend.default_model());

    match backend {
        provider::VideoBackend::Google => {
            println!("Video models available to the google provider:");
            for (alias, id) in video::VIDEO_ALIASES {
                let default = if *id == video::DEFAULT_VIDEO_MODEL { "  (default)" } else { "" };
                println!("  {alias:<14} -> {id}{default}");
            }
        }
        provider::VideoBackend::Runway => {
            match runway::Client::from_env().and_then(|c| c.credits()) {
                Ok(credits) => println!("Key is valid. Remaining credits: {credits}"),
                Err(e) => println!("The runway provider cannot be used right now:\n\n  {e:#}\n"),
            }
            println!("Video models available to the runway provider:");
            for model in runway::MODELS {
                let default = if *model == runway::DEFAULT_MODEL { "  (default)" } else { "" };
                let per_model = provider::video_capabilities_for(backend, model);
                let text = if per_model.text_to_video { "" } else { ";  needs a still" };
                println!("  {model}{default}{text}");
            }
        }
        provider::VideoBackend::Kling => {
            match kling::Client::from_env().and_then(|c| c.credits()) {
                Ok(units) => println!("Key is valid. Remaining units: {units}"),
                Err(e) => println!("The kling provider cannot be used right now:\n\n  {e:#}\n"),
            }
            println!("Video models available to the kling provider:");
            for model in kling::MODELS {
                let default = if *model == kling::DEFAULT_MODEL { "  (default)" } else { "" };
                println!("  {model}{default}");
            }
            println!("\nAliases:");
            for (alias, target) in kling::MODEL_ALIASES {
                println!("  {alias:<16} -> {target}");
            }
        }
    }

    println!("\nThis provider supports:");
    println!("  aspect ratio    {}", describe_aspect(caps.aspect));
    println!("  duration        {}", caps.duration.describe());
    println!("  from a still    {}", yes_no(caps.image_to_video));
    println!("  from text alone {}", yes_no(caps.text_to_video));
    println!("  negative prompt {}", yes_no(caps.negative_prompt));
    println!("  resolution      {}", yes_no(caps.resolution));
    println!("  seed            {}", yes_no(caps.seed));
    if !caps.modes.is_empty() {
        println!("  quality tiers   {}", caps.modes.join(", "));
    }
    println!("  output carries  {}", caps.provenance.describe());

    Ok(())
}

fn open_video(backend: provider::VideoBackend) -> Result<Box<dyn provider::VideoProvider>> {
    Ok(match backend {
        provider::VideoBackend::Google => Box::new(genai::Client::from_env()?),
        provider::VideoBackend::Runway => Box::new(runway::Client::from_env()?),
        provider::VideoBackend::Kling => Box::new(kling::Client::from_env()?),
    })
}

/// The model actually sent, once each provider's aliases are applied.
fn resolve_video_model(backend: provider::VideoBackend, model: &str) -> String {
    match backend {
        provider::VideoBackend::Google => video::resolve_video_model(model),
        provider::VideoBackend::Runway => runway::resolve_model(model),
        provider::VideoBackend::Kling => kling::resolve_model(model),
    }
}

/// How [`await_video`] paces itself. A value rather than constants so a test
/// can wait for milliseconds instead of a quarter of an hour.
#[derive(Clone, Copy)]
struct Pacing {
    deadline: std::time::Duration,
    first_interval: std::time::Duration,
    max_interval: std::time::Duration,
}

impl Pacing {
    const DEFAULT: Pacing = Pacing {
        deadline: std::time::Duration::from_secs(900),
        first_interval: std::time::Duration::from_secs(5),
        max_interval: std::time::Duration::from_secs(30),
    };
}

/// How a blocking wait ended, when it did not fail.
#[derive(Debug)]
enum Waited {
    Done(Vec<u8>),
    /// The deadline passed with the render still running. Not an error: the
    /// provider has not said it failed, and it is billed either way.
    StillRunning,
}

/// Polls a render to completion, for the CLI's blocking path.
///
/// Lives here rather than on the trait because the waiting is a *front-end*
/// decision, not a provider one: the MCP surface deliberately never blocks, and
/// a provider that had to implement both would be implementing a policy it does
/// not own. Both providers get the same backoff, the same deadline and the same
/// cancellation check this way, rather than each reinventing them.
///
/// Running out of time is [`Waited::StillRunning`] and not an `Err`. It was an
/// error, exit 1, while the render was still going and billed — and a wrapper
/// that retries on 1 then paid for the same video twice.
fn await_video(
    client: &dyn provider::VideoProvider,
    provider: &str,
    operation: &str,
    pacing: Pacing,
) -> Result<Waited> {
    let started = std::time::Instant::now();
    let mut interval = pacing.first_interval;

    loop {
        cancel::check().map_err(|e| {
            anyhow::anyhow!(
                "{e}\n\nCollect it later with: {}",
                check_command_for(provider, operation)
            )
        })?;

        if started.elapsed() > pacing.deadline {
            return Ok(Waited::StillRunning);
        }

        std::thread::sleep(interval);
        interval = (interval * 2).min(pacing.max_interval);

        let polled = client.poll(operation).map_err(|error| {
            // A failure the provider reported as final has nothing left to
            // collect, and it must keep its type for `note_failure`. Anything
            // else — a dropped connection, a 5xx — says nothing about the
            // render, which is still billed and may still finish.
            if error.downcast_ref::<video::TerminalFailure>().is_some() {
                error
            } else {
                anyhow::anyhow!(
                    "{error:#}\n\nThe render may still be running. Ask again with: {}",
                    check_command_for(provider, operation)
                )
            }
        })?;
        if let video::VideoStatus::Done(bytes) = polled {
            eprintln!("Render finished in {}s.", started.elapsed().as_secs());
            return Ok(Waited::Done(bytes));
        }

        eprintln!("  still rendering ({}s elapsed)…", started.elapsed().as_secs());
    }
}

fn open(backend: Backend) -> Result<Box<dyn ImageProvider>> {
    Ok(match backend {
        Backend::Google => Box::new(genai::Client::from_env()?),
        Backend::ComfyUi => Box::new(comfy::Client::from_env()?),
        Backend::Bfl => Box::new(bfl::Client::from_env()?),
        Backend::Stability => Box::new(stability::Client::from_env()?),
        Backend::OpenAi => Box::new(openai::Client::from_env()?),
        Backend::Runway => Box::new(runway::Client::from_env()?),
    })
}

/// What `lucida models` could learn by asking the provider.
///
/// Separated from the capability table below it because the two answer different
/// questions and only one of them needs a credential.
enum Reachability {
    /// The provider answered. Carries what it listed, which may be nothing.
    Listed(Vec<String>),
    /// No client could be built — almost always a missing key.
    Unavailable(String),
    /// A client exists but the provider did not answer.
    Unreachable(String),
}

fn list_models(backend: Backend) -> Result<()> {
    // Asked for first, and printed no matter what happens below. Whether Google
    // has a seed is not a fact about your credentials, and `capabilities_for` is
    // a pure function saying so — but both this command and the MCP probe used
    // to return early the moment a client could not be built, so the one
    // question that needed no key was the one you could not get an answer to
    // without one. `provider.rs` says as much in its own doc comment; the code
    // disagreed with it.
    let caps = provider::capabilities_for(backend, backend.default_model());

    let reachability = match open(backend) {
        Err(e) => Reachability::Unavailable(format!("{e:#}")),
        Ok(provider) => match provider.list_models() {
            Ok(models) => Reachability::Listed(models),
            Err(e) => Reachability::Unreachable(format!("{e:#}")),
        },
    };

    let models = match &reachability {
        Reachability::Listed(models) => models.clone(),
        Reachability::Unavailable(why) => {
            println!("The {} provider cannot be used right now:\n\n  {why}\n", caps.provider);
            println!("What it supports is a fact about the provider, not about your \
                      credentials, so it is printed anyway:\n");
            Vec::new()
        }
        Reachability::Unreachable(why) => {
            println!("The {} provider did not answer:\n\n  {why}\n", caps.provider);
            Vec::new()
        }
    };

    if models.is_empty() && matches!(reachability, Reachability::Listed(_)) {
        println!("No image models visible to the {} provider.", caps.provider);
    } else if !models.is_empty() {
        println!("Image models available to the {} provider:", caps.provider);
        for model in &models {
            let mut notes: Vec<String> = Vec::new();
            // Generated from `Backend::default_model()`, so every provider gets
            // the annotation. It was written per-provider and only two of the
            // five ever got it: google here and bfl in its own block below,
            // while openai and stability listed their default indistinguishably
            // from everything else. Found by the canary, which was checking
            // something else entirely and could not find the marker it expected.
            if model == backend.default_model() {
                notes.push("default".into());
            }
            if model.starts_with("imagen") {
                notes.push("Imagen family — a different endpoint, not implemented".into());
            }
            // Runway's turbo cannot start from text; the video half marks its
            // equivalent ("needs a still"), so the image half does too.
            if provider::capabilities_for(backend, model).needs_reference {
                notes.push("needs a reference image".into());
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
                notes.extend(bfl_model_notes(model));
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
        Backend::Runway => runway::IMAGE_ALIASES,
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

/// What differs per BFL model, as the notes beside it in `lucida models`.
/// Generated from the capabilities, so a note cannot outlive the table.
fn bfl_model_notes(model: &str) -> Vec<String> {
    let per_model = provider::capabilities_for(Backend::Bfl, model);
    let mut notes = Vec::new();
    if per_model.steps {
        notes.push("steps + guidance".to_string());
    }
    // The ceiling is said here because the over-ceiling refusal points here
    // for a model that takes more.
    notes.push(match (per_model.references, per_model.max_references) {
        (true, Some(most)) => format!("edits, up to {most} references"),
        (true, None) => "edits".to_string(),
        (false, _) => "generate only".to_string(),
    });
    // Geometry differs per model too: Kontext and Ultra take a ratio from a
    // list and no `--size`.
    if !per_model.size {
        notes.push("aspect ratio from a list, no --size".to_string());
    }
    notes
}

fn yes_no(supported: bool) -> &'static str {
    if supported { "yes" } else { "no" }
}

fn describe_aspect(support: provider::AspectSupport) -> String {
    match support {
        provider::AspectSupport::Named(ratios) => ratios.join(", "),
        provider::AspectSupport::Pixels(pairs) => {
            format!("{} (or any W:H with the same shape)", pairs.join(", "))
        }
        provider::AspectSupport::Free { multiple_of } => {
            format!("any, rounded to {multiple_of} pixels")
        }
    }
}

/// Says which provider a default landed on, and where the default came from.
///
/// ⚠ **This is the half of the feature that makes it acceptable at all.** A
/// default that routes a render somewhere the user did not name is the same
/// class of event as a silently dropped parameter — unless it announces itself.
/// So this is not optional polish; it is the condition the preference order was
/// allowed to exist under (ROADMAP § 6, constraint 1).
///
/// stderr rather than stdout, because `--json` must stay a single document on
/// stdout and a test holds that. Nothing is printed when the user named the
/// provider or the model: they already know, and narrating a choice back to the
/// person who just made it is noise.
fn announce_default(source: &Option<provider::DefaultSource>, chosen: &str) {
    if let Some(source) = source {
        eprintln!("Provider: {}", source.describe(chosen));
    }
}

/// Renders `count` images, and prints whatever the caller asked to see.
///
/// The batch is checked and budgeted as a *whole* before the first render, so
/// asking for ten of something you cannot afford is refused once rather than
/// nine times after the first one succeeded.
/// Prints a plan without sending it — pretty for a human, one object for `--json`.
fn report_plan(plan: serde_json::Value) -> Result<()> {
    if out::json() {
        out::emit(plan);
    } else {
        eprintln!("Dry run — nothing was sent.");
        println!("{}", serde_json::to_string_pretty(&plan)?);
    }
    Ok(())
}

fn execute(
    request: ImageRequest,
    backend: Backend,
    out: PathBuf,
    count: usize,
    dry_run: bool,
    default_source: Option<provider::DefaultSource>,
) -> Result<()> {
    let caps = provider::capabilities_for(backend, &request.model);
    caps.check(&request)?;

    // The whole batch, in one check. Calling `check` per image reads as a check
    // per render and is not one — every call re-reads the same ledger, so all of
    // them ask "can I afford one more?" and all of them say yes. Measured: three
    // images at $0.134 went through a $0.20 budget and rendered all three.
    //
    // The reservation is held for the whole batch, released when this function
    // returns — after the last image's ledger entry, or on the first failure.
    // Each image's entry lands while the batch's hold is still up, so for that
    // moment it counts twice; nothing in a CLI process checks again meanwhile.
    let price = spend::price_for(backend, &request.model, request.size);
    let _reservation = spend::check_batch(price, count, "render")?;

    // A pinned seed asks for one specific image; a batch asks for several
    // different ones. Together they are a contradiction that renders the same
    // picture `count` times and bills for each — silently, since every render
    // would look like a success. Refused with the two ways out rather than
    // guessed at, because incrementing someone's seed for them is its own
    // silent substitution.
    if count > 1 && request.seed.is_some() {
        return Err(anyhow::Error::new(out::Refused(format!(
            "`--seed` pins one image and `--count {count}` asks for several, so \
             together they would render the same picture {count} times and bill \
             for each.\n\n\
             Drop `--seed` to get {count} different images, or drop `--count` to \
             reproduce the one the seed names."
        ))));
    }

    if dry_run {
        report_plan(serde_json::json!({
            "ok": true,
            "status": "dry-run",
            "provider": caps.provider,
            "provider_source": default_source.as_ref().map(|s| s.tag()),
            "model": request.model,
            "prompt": request.prompt,
            "count": count,
            "aspect": request.aspect.map(|a| a.to_string()),
            "size": request.size.map(|s| s.0),
            "seed": request.seed,
            "references": request.references,
            // Everything else the render would send, so the plan answers
            // "what would you send?" in full. These five used to be resolved,
            // validated and then left out of the document that claims to list
            // every resolved parameter.
            "negative_prompt": request.negative_prompt,
            "mask": request.mask,
            "workflow": request.workflow,
            "steps": request.steps,
            "guidance": request.guidance.map(provider::guidance_as_written),
            "estimated_usd": price.against_budget() * count as f64,
            "exit_code": out::OK,
        }))?;
        return Ok(());
    }

    let written = render_batch(&out, count, |destination| {
        render_one(&request, backend, caps, price, destination)
    })?;

    if out::json() {
        out::emit(serde_json::json!({
            "ok": true,
            "status": "done",
            "images": written,
            "exit_code": out::OK,
        }));
    } else {
        for image in &written {
            // One path per line, so a batch pipes as readily as a single render.
            println!("{}", image["path"].as_str().unwrap_or_default());
        }
    }
    Ok(())
}

/// Runs `render` once per image and collects what it returns.
///
/// A failure at image k is an error that names images 1..k-1, because those were
/// written — and, on a paid provider, billed — and nothing else would ever print
/// their paths: stdout is only reached on success.
fn render_batch(
    out: &Path,
    count: usize,
    mut render: impl FnMut(PathBuf) -> Result<serde_json::Value>,
) -> Result<Vec<serde_json::Value>> {
    let mut written = Vec::new();
    for n in 1..=count {
        match render(numbered(out, n, count)) {
            Ok(image) => written.push(image),
            Err(error) if written.is_empty() => return Err(error),
            Err(error) => {
                let paths = written
                    .iter()
                    .map(|image| image["path"].as_str().unwrap_or_default().to_string())
                    .collect();
                return Err(error.context(out::Written(paths)));
            }
        }
    }
    Ok(written)
}

/// `image.png` → `image-2.png`, but only when there is more than one.
///
/// A single render keeps the name it was given, because that is what `--out`
/// means and suffixing it would break every existing caller.
fn numbered(out: &Path, n: usize, count: usize) -> PathBuf {
    if count <= 1 {
        return out.to_path_buf();
    }
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let numbered = match out.extension().and_then(|e| e.to_str()) {
        Some(extension) => format!("{stem}-{n}.{extension}"),
        None => format!("{stem}-{n}"),
    };
    out.with_file_name(numbered)
}

fn render_one(
    request: &ImageRequest,
    backend: Backend,
    caps: provider::Capabilities,
    price: spend::Price,
    out: PathBuf,
) -> Result<serde_json::Value> {
    let request = request.clone();

    let provider = open(backend)?;

    let verb = if request.references.is_empty() {
        "Generating"
    } else {
        "Editing"
    };
    eprintln!("{verb} via {}…", caps.provider);

    let image = generate_billed(provider.as_ref(), &request, |abandoned| {
        ledger::abandoned_image(
            caps.provider,
            &request.model,
            &request.prompt,
            abandoned,
            price.against_budget(),
        );
    })?;

    let destination = correct_extension(&out, &image.mime_type);
    if destination != out {
        eprintln!(
            "note: the model returned {}, so writing {} rather than {}",
            image.mime_type,
            destination.display(),
            out.display()
        );
    }
    let written = write_billed(&destination, &image.bytes, |path, unsaved| {
        ledger::image(
            caps.provider,
            &request.model,
            &request.prompt,
            path,
            image.seed,
            price.against_budget(),
            unsaved,
        );
    })?;

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

    // What is embedded in the file that was just written. The MCP surface has
    // reported this on every render since provenance became a value; the CLI
    // reported it only in `lucida models`, where you have to go and ask. That is
    // backwards: the moment it matters is when a file exists and you are about to
    // publish it, not when you are choosing a provider. It is also the one
    // difference between the local lane and every hosted one that survives being
    // copied out of this tool.
    eprintln!("Provenance: {}.", caps.provenance.describe());

    // Said after the render as well as counted, because "what did that cost" is
    // a question someone asks holding the file, not before asking for it.
    if price != spend::Price::Free {
        eprintln!("Cost: {}.", price.describe());
    }

    // Returned rather than printed, so a batch can be reported as one document
    // and a single render still gets its path on stdout alone — which is what
    // makes `$(lucida generate …)` compose.
    let (width, height) = match image_dimensions(&image.bytes, &image.mime_type) {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };
    Ok(serde_json::json!({
        "path": written.to_string_lossy(),
        "provider": caps.provider,
        "model": request.model,
        "mime": image.mime_type,
        "bytes": image.bytes.len(),
        "width": width,
        "height": height,
        "seed": image.seed,
        "provenance": caps.provenance.describe(),
        "estimated_usd": price.against_budget(),
    }))
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
///
/// Only an extension that is *a known image or video extension* is replaced. Any
/// other suffix is part of the name, not a format: `-o hero.v1` and `-o hero.v2`
/// both went through `with_extension` and became `hero.png`, so the second render
/// silently overwrote the first. Those get the extension appended instead
/// (`hero.v1.png`). The list is the formats Lucida itself writes, which is also the
/// set a person might plausibly have meant as a mismatched format.
pub fn correct_extension(path: &Path, mime: &str) -> PathBuf {
    const FORMATS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "mp4"];

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
        return path.to_path_buf();
    }
    if actual.as_deref().is_some_and(|e| FORMATS.contains(&e)) {
        return path.with_extension(expected);
    }

    // Appended, to the file name alone so the directory is untouched. A path with
    // no file name at all (`..`, or empty) has nothing to append to; the old
    // behaviour is the best of the bad options there.
    let Some(name) = path.file_name() else {
        return path.with_extension(expected);
    };
    let mut name = name.to_os_string();
    // `hero.` already has its dot.
    if !name.to_string_lossy().ends_with('.') {
        name.push(".");
    }
    name.push(expected);
    path.with_file_name(name)
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

/// Asks `provider` for an image, and hands `record` the marker when the
/// provider billed for one it never returned.
///
/// The twin of [`write_billed`], one step earlier. A BFL or Runway wait that is
/// cancelled or runs out comes back as an error after the submit was billed,
/// and both image call sites used to let it go by `?` with no ledger entry — so
/// the budget never counted it. Routing both through here means the marker
/// cannot be missed at one of them. Only a [`provider::Abandoned`] error is
/// passed on; every other error, a failed submit among them, records nothing.
pub fn generate_billed(
    provider: &dyn ImageProvider,
    request: &ImageRequest,
    record: impl FnOnce(&provider::Abandoned),
) -> Result<provider::GeneratedImage> {
    provider.generate(request).inspect_err(|error| {
        if let Some(abandoned) = error.downcast_ref::<provider::Abandoned>() {
            record(abandoned);
        }
    })
}

/// Writes a render the provider has already billed, recording it whether or not
/// the write succeeds.
///
/// `record` receives the path written, or the path intended and the write's
/// error. The ledger call used to come after a successful write, so a full disk
/// or an unwritable path after a paid render left no entry, and the budget —
/// summed from the ledger — never counted money that was spent. Routing both
/// image call sites through here makes the record unskippable: there is no way
/// to get the written path without passing it.
pub fn write_billed(
    destination: &Path,
    bytes: &[u8],
    record: impl FnOnce(&str, Option<&anyhow::Error>),
) -> Result<PathBuf> {
    match write_image(destination, bytes) {
        Ok(written) => {
            record(&written.to_string_lossy(), None);
            Ok(written)
        }
        Err(error) => {
            record(&destination.to_string_lossy(), Some(&error));
            Err(error)
        }
    }
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

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
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

    /// `lucida models --provider bfl` annotated Kontext and Ultra exactly like
    /// the pixel models, though they take a ratio from a list and no `--size`.
    #[test]
    fn bfl_models_are_annotated_with_their_geometry() {
        for model in bfl::ratio_only_models() {
            let notes = bfl_model_notes(model).join("; ");
            assert!(notes.contains("no --size"), "{model}: {notes}");
        }
        for model in bfl::sized_models() {
            let notes = bfl_model_notes(model).join("; ");
            assert!(!notes.contains("--size"), "{model}: {notes}");
        }
        // The notes that were already there survive.
        assert!(bfl_model_notes("flux-2-flex").join("; ").contains("steps + guidance"));
        assert!(bfl_model_notes("flux-dev").contains(&"generate only".to_string()));
        // And each edit model's reference ceiling, which the over-ceiling
        // refusal sends a caller here to compare.
        assert!(bfl_model_notes("flux-kontext-pro").join("; ").contains("up to 4 references"));
        assert!(bfl_model_notes("flux-2-pro").join("; ").contains("up to 8 references"));
    }

    /// A suffix that is not a format is part of the name, so two renders that
    /// differ only in it must not land on one file.
    ///
    /// This was `with_extension`, which replaces whatever follows the last dot:
    /// `hero.v1` and `hero.v2` both became `hero.png`, and the second render
    /// overwrote the first without a word.
    #[test]
    fn an_unknown_suffix_is_kept_and_the_extension_appended() {
        let png = |p: &str| correct_extension(Path::new(p), "image/png");

        assert_eq!(png("hero.v1"), PathBuf::from("hero.v1.png"));
        assert_eq!(png("hero.v2"), PathBuf::from("hero.v2.png"));
        assert_ne!(png("hero.v1"), png("hero.v2"), "two names collapsed into one file");

        // No extension at all, and the directory untouched.
        assert_eq!(png("out/hero"), PathBuf::from("out/hero.png"));
        assert_eq!(png("out.v1/hero.v1"), PathBuf::from("out.v1/hero.v1.png"));
        assert_eq!(png("hero."), PathBuf::from("hero.png"));
    }

    /// A known format that disagrees with the bytes is replaced, which is the
    /// whole reason the function exists — in either case, and for video.
    #[test]
    fn a_known_extension_that_disagrees_with_the_bytes_is_replaced() {
        let fix = |p: &str, mime: &str| correct_extension(Path::new(p), mime);

        assert_eq!(fix("icon.png", "image/jpeg"), PathBuf::from("icon.jpg"));
        assert_eq!(fix("icon.PNG", "image/webp"), PathBuf::from("icon.webp"));
        assert_eq!(fix("icon.webp", "image/png"), PathBuf::from("icon.png"));
        assert_eq!(fix("clip.jpg", "video/mp4"), PathBuf::from("clip.mp4"));
        assert_eq!(fix("clip.mp4", "image/png"), PathBuf::from("clip.png"));
        assert_eq!(fix("a.v1/icon.jpeg", "image/png"), PathBuf::from("a.v1/icon.png"));
    }

    /// An extension that already agrees is left exactly as written, including its
    /// case and `jpeg` for JPEG, and an unknown mime type changes nothing.
    #[test]
    fn an_agreeing_extension_is_left_alone() {
        let fix = |p: &str, mime: &str| correct_extension(Path::new(p), mime);

        assert_eq!(fix("icon.PNG", "image/png"), PathBuf::from("icon.PNG"));
        assert_eq!(fix("icon.jpeg", "image/jpeg"), PathBuf::from("icon.jpeg"));
        assert_eq!(fix("icon.v1", "application/octet-stream"), PathBuf::from("icon.v1"));
    }

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
            // Video providers too, now that there is more than one of them —
            // "video comes from Veo" was the whole capability the description
            // omitted last time, and a second lane is exactly as easy to forget.
            for backend in provider::VideoBackend::ALL {
                let name = Backend::video_product_name(*backend);
                assert!(
                    surface.contains(name),
                    "`{name}` is missing from a surface someone reads before installing: {surface}"
                );
            }
        }
    }

    /// `--help` is the long form of the shopfront, and the one a person reads
    /// when they are deciding which key to export — so it has to name every
    /// provider and every credential, not only the ones that existed when it was
    /// written. It named five of six image providers and no Runway or Kling key.
    ///
    /// Both lists come from the same tables the code routes by. A provider whose
    /// credential is `None` (the local one) has nothing to name.
    #[test]
    fn the_long_help_names_every_provider_and_every_key() {
        use clap::CommandFactory;

        let long = Cli::command()
            .get_long_about()
            .map(|a| a.to_string())
            .expect("the CLI has no long description");

        for backend in Backend::ALL {
            assert!(
                long.contains(backend.product_name()),
                "`{}` is missing from the long help: {long}",
                backend.product_name()
            );
            if let Some(key) = backend.credential() {
                assert!(long.contains(key), "{key} is missing from the long help: {long}");
            }
        }
        for backend in provider::VideoBackend::ALL {
            let name = Backend::video_product_name(*backend);
            assert!(long.contains(name), "`{name}` is missing from the long help: {long}");
            if let Some(key) = backend.credential() {
                assert!(long.contains(key), "{key} is missing from the long help: {long}");
            }
        }
    }

    /// `video --provider` is a hand-written clap string, and it listed two of
    /// three providers. The image flag's list is held against the MCP enum in
    /// `tests/cli.rs`; this holds the video one against `VideoBackend::ALL`
    /// directly, which is visible from here.
    #[test]
    fn the_video_provider_help_names_every_video_provider() {
        use clap::CommandFactory;

        let command = Cli::command();
        let video = command
            .get_subcommands()
            .find(|c| c.get_name() == "video")
            .expect("no `video` subcommand");
        let help = video
            .get_arguments()
            .find(|a| a.get_id() == "provider")
            .expect("no `--provider` argument")
            .get_help()
            .expect("`video --provider` has no help")
            .to_string();

        for backend in provider::VideoBackend::ALL {
            assert!(
                help.contains(backend.name()),
                "`{}` is a video provider but the `video --provider` help omits it: {help}",
                backend.name()
            );
        }
    }

    /// Every `](#anchor)` in the README points at a heading that exists.
    ///
    /// The table of contents is a hand-maintained index of a document that gets
    /// edited, which is the shape every stale thing in this repository has had.
    /// A broken anchor is silent on GitHub — the link simply does nothing — so
    /// nothing but a check would report it.
    ///
    /// Implements GitHub's slug rule: lowercase, drop everything that is not
    /// alphanumeric, space or hyphen, then spaces to hyphens. Headings written
    /// as raw `<h3 id="…">` supply their own, which is why the two ambiguous
    /// ones ("Configuration" appears twice) are spelled that way.
    #[test]
    fn every_readme_link_points_at_a_heading_that_exists() {
        let readme =
            std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
                .expect("README.md must exist");

        let slug = |heading: &str| -> String {
            let mut text = heading.to_string();
            // Inline code and links contribute their text, not their markup.
            text = text.replace('`', "");
            while let (Some(open), Some(close)) = (text.find("]("), text.find(')')) {
                if open < close {
                    text.replace_range(open..=close, "");
                } else {
                    break;
                }
            }
            text.replace('[', "")
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
                .collect::<String>()
                .trim()
                .replace(' ', "-")
        };

        let mut anchors: Vec<String> = Vec::new();
        for line in readme.lines() {
            if let Some(rest) = line.trim_start().strip_prefix('#') {
                let heading = rest.trim_start_matches('#').trim();
                if !heading.is_empty() {
                    anchors.push(slug(heading));
                }
            }
            // An explicit id wins, and is how a duplicate heading name is made
            // linkable at all.
            if let Some(at) = line.find("<h") {
                if let Some(start) = line[at..].find("id=\"") {
                    let rest = &line[at + start + 4..];
                    anchors.push(rest[..rest.find('"').unwrap()].to_string());
                }
            }
        }

        let mut links = 0;
        for (offset, _) in readme.match_indices("](#") {
            let rest = &readme[offset + 3..];
            let target = &rest[..rest.find(')').expect("an unterminated link")];
            links += 1;

            assert!(
                anchors.contains(&target.to_string()),
                "README links to #{target}, which is not a heading in it.\n\
                 headings are: {anchors:?}"
            );
        }

        // The scan has to have found the links, or an empty README would pass.
        assert!(links > 10, "only {links} internal links found — the scan broke");
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

    /// A render the provider has returned has been billed, so it is recorded
    /// whether or not the file lands. The ledger call used to follow a
    /// successful write, and a failed one left a paid render uncounted.
    #[test]
    fn a_billed_image_is_recorded_even_when_the_write_fails() {
        let dir = std::env::temp_dir().join(format!("lucida-billed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A regular file where a directory has to be: the write cannot succeed.
        let blocker = dir.join("not-a-directory");
        std::fs::write(&blocker, b"").unwrap();
        let unwritable = blocker.join("cat.png");

        let mut recorded = None;
        let result = write_billed(&unwritable, b"paid for", |path, unsaved| {
            recorded = Some((path.to_string(), unsaved.map(|e| format!("{e:#}"))));
        });
        assert!(result.is_err(), "the write was expected to fail");
        let (path, unsaved) = recorded.expect("a failed write recorded nothing");
        assert_eq!(path, unwritable.to_string_lossy());
        assert!(unsaved.is_some(), "recorded as saved when it was not");

        let mut recorded = None;
        let written = write_billed(&dir.join("cat.png"), b"paid for", |path, unsaved| {
            recorded = Some((path.to_string(), unsaved.is_some()));
        })
        .unwrap();
        assert_eq!(recorded, Some((written.to_string_lossy().into_owned(), false)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A single render keeps the name it was given — that is what `--out` means,
    /// and suffixing it would break every existing caller. Only a batch numbers.
    #[test]
    fn only_a_batch_numbers_its_output() {
        let out = Path::new("public/icon.png");

        assert_eq!(numbered(out, 1, 1), PathBuf::from("public/icon.png"));
        assert_eq!(numbered(out, 1, 3), PathBuf::from("public/icon-1.png"));
        assert_eq!(numbered(out, 3, 3), PathBuf::from("public/icon-3.png"));

        // The directory has to survive, or a batch scatters into the working
        // directory instead of where it was asked to go.
        assert_eq!(numbered(out, 2, 3).parent(), out.parent());

        // No extension is a legitimate output path; the number still goes on.
        assert_eq!(numbered(Path::new("out/frame"), 2, 2), PathBuf::from("out/frame-2"));
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

    /// A video provider that answers every poll from a script, then keeps
    /// saying "pending" — a render that never finishes.
    struct Scripted(std::sync::Mutex<Vec<Result<video::VideoStatus>>>);

    impl provider::VideoProvider for Scripted {
        fn start(&self, _: &video::VideoRequest) -> Result<String> {
            unreachable!("a wait never starts a render")
        }
        fn poll(&self, _: &str) -> Result<video::VideoStatus> {
            let mut replies = self.0.lock().unwrap();
            if replies.is_empty() {
                Ok(video::VideoStatus::Pending)
            } else {
                replies.remove(0)
            }
        }
    }

    fn quick(deadline_ms: u64) -> Pacing {
        let ms = std::time::Duration::from_millis;
        Pacing { deadline: ms(deadline_ms), first_interval: ms(5), max_interval: ms(10) }
    }

    /// The wait running out is an outcome, not an error: it exited 1 while the
    /// render was still going and billed, and a wrapper that retries on 1 paid
    /// twice. Returning `Ok` is also what keeps the ledger entry, because the
    /// caller only retires an operation from an `Err`.
    #[test]
    fn a_wait_that_runs_out_is_pending_and_not_an_error() {
        let client = Scripted(Default::default());
        let waited = await_video(&client, "kling", "op-1", quick(40)).expect("not an error");
        assert!(matches!(waited, Waited::StillRunning));
    }

    #[test]
    fn a_render_that_finishes_in_time_is_returned() {
        let client = Scripted(std::sync::Mutex::new(vec![
            Ok(video::VideoStatus::Pending),
            Ok(video::VideoStatus::Done(vec![1, 2, 3])),
        ]));
        match await_video(&client, "kling", "op-1", quick(5_000)).unwrap() {
            Waited::Done(bytes) => assert_eq!(bytes, vec![1, 2, 3]),
            Waited::StillRunning => panic!("it finished, the wait said it had not"),
        }
    }

    /// Both the document and the prose carry the id and the provider, and the
    /// document is the one `lucida check` emits.
    #[test]
    fn a_wait_that_ran_out_names_the_render_and_its_provider() {
        let document = pending_document("kling", "op-1");
        assert_eq!(document["status"], "pending");
        assert_eq!(document["operation"], "op-1");
        assert_eq!(document["provider"], "kling");
        assert_eq!(document["exit_code"], out::PENDING);
        assert_eq!(document["ok"], true);

        let notice = still_running_notice("kling", "op-1");
        assert!(notice.contains("lucida check --provider kling op-1"), "{notice}");
        assert!(notice.contains("do not start another"), "{notice}");
    }

    /// A poll that errors keeps exit 1, and its message says how to ask again —
    /// except a failure the provider called final, which has nothing to collect
    /// and must stay the type the ledger looks for.
    #[test]
    fn a_poll_error_says_how_to_ask_again() {
        let client = Scripted(std::sync::Mutex::new(vec![Err(anyhow::anyhow!("502 from the gateway"))]));
        let error = await_video(&client, "runway", "op-2", quick(5_000)).expect_err("an error");
        let text = format!("{error:#}");
        assert!(text.contains("502 from the gateway"), "{text}");
        assert!(text.contains("lucida check --provider runway op-2"), "{text}");
        assert_eq!(out::code_for(&error), out::ERROR);

        let client = Scripted(std::sync::Mutex::new(vec![Err(video::terminal("moderation"))]));
        let error = await_video(&client, "runway", "op-2", quick(5_000)).expect_err("an error");
        assert!(error.downcast_ref::<video::TerminalFailure>().is_some(), "retired renders must stay retirable");
    }

    /// Images 1..k-1 of a batch that failed at k were written (and, on a paid
    /// provider, billed), and the error is the only thing that reaches the
    /// caller.
    #[test]
    fn a_batch_that_fails_partway_reports_what_it_wrote() {
        let mut calls = 0;
        let error = render_batch(Path::new("out.png"), 4, |destination| {
            calls += 1;
            if calls == 3 {
                anyhow::bail!("the provider said no");
            }
            Ok(serde_json::json!({ "path": destination.to_string_lossy() }))
        })
        .expect_err("the third image fails");

        assert_eq!(out::written_before(&error).unwrap(), ["out-1.png", "out-2.png"]);
        let text = format!("{error:#}");
        assert!(text.contains("out-1.png") && text.contains("out-2.png"), "{text}");
        assert!(text.contains("the provider said no"), "{text}");
        assert_eq!(out::code_for(&error), out::ERROR, "the exit code must not move");
    }

    /// A first-image failure wrote nothing, so there is nothing to report and
    /// the error stays exactly as it was.
    #[test]
    fn a_batch_that_fails_at_once_adds_nothing() {
        let error = render_batch(Path::new("out.png"), 2, |_| anyhow::bail!("no key")).unwrap_err();
        assert!(out::written_before(&error).is_none());
        assert_eq!(format!("{error:#}"), "no key");
    }
}
