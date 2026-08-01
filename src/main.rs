//! lucida — image generation via the Google Gemini API.
//!
//! Named for the camera lucida, the optical device that let artists trace what
//! they saw onto paper.
//!
//! One binary, two front ends: a plain CLI for shell and script use, and an MCP
//! server (`lucida mcp`) so agents can call it as a first-class tool.

mod genai;
mod mcp;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use genai::{Client, DEFAULT_MODEL, ImageRequest};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "lucida",
    version,
    about = "Generate and edit images with Google's Gemini models",
    long_about = "Generate and edit images with Google's Gemini models.\n\n\
                  Reads GOOGLE_API_KEY (or GEMINI_API_KEY) from the environment.\n\
                  Image generation requires billing to be enabled on the project \
                  behind the key; free-tier keys report a quota of zero."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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

        /// Aspect ratio, e.g. 16:9, 1:1, 4:5
        #[arg(short, long)]
        aspect: Option<String>,

        /// Render resolution: 1K, 2K or 4K
        #[arg(short, long)]
        size: Option<String>,

        /// Model id
        #[arg(short, long, default_value = DEFAULT_MODEL)]
        model: String,

        /// Existing image to condition on; repeat for several. Prefer the
        /// `edit` subcommand, which reads better for the common case.
        #[arg(short, long = "ref")]
        reference: Vec<String>,
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

        /// Aspect ratio, e.g. 16:9, 1:1, 4:5
        #[arg(short, long)]
        aspect: Option<String>,

        /// Render resolution: 1K, 2K or 4K
        #[arg(short, long)]
        size: Option<String>,

        /// Model id
        #[arg(short, long, default_value = DEFAULT_MODEL)]
        model: String,

        /// Additional images for style or subject reference
        #[arg(short, long = "ref")]
        reference: Vec<String>,
    },

    /// List image-capable models this API key can see (free, spends nothing)
    Models,

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

        Command::Models => {
            let models = Client::from_env()?.list_image_models()?;
            if models.is_empty() {
                println!("No image-capable models visible to this key.");
                return Ok(());
            }
            println!("Image models available to this key:");
            for model in &models {
                let mut notes: Vec<&str> = Vec::new();
                if model == DEFAULT_MODEL {
                    notes.push("default");
                }
                if model.starts_with("imagen") {
                    notes.push("Imagen family — different endpoint, retires 2026-08-17");
                }
                let suffix = if notes.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", notes.join("; "))
                };
                println!("  {model}{suffix}");
            }

            println!("\nAliases (\"Nano Banana\" is Google's codename for these):");
            for (alias, target) in genai::MODEL_ALIASES {
                println!("  {alias:<16} -> {target}");
            }
            Ok(())
        }

        Command::Generate {
            prompt,
            out,
            aspect,
            size,
            model,
            reference,
        } => execute(
            ImageRequest {
                prompt,
                model,
                aspect_ratio: aspect,
                image_size: size,
                references: reference,
            },
            out,
        ),

        Command::Edit {
            image,
            prompt,
            out,
            aspect,
            size,
            model,
            reference,
        } => {
            // The edited image leads, so it is the primary subject rather than
            // one reference among several.
            let mut references = vec![image.clone()];
            references.extend(reference);

            let destination = out.unwrap_or_else(|| PathBuf::from(&image));
            execute(
                ImageRequest {
                    prompt,
                    model,
                    aspect_ratio: aspect,
                    image_size: size,
                    references,
                },
                destination,
            )
        }
    }
}

fn execute(request: ImageRequest, out: PathBuf) -> Result<()> {
    let verb = if request.references.is_empty() {
        "Generating"
    } else {
        "Editing"
    };
    // Report the resolved id, not the alias, so it is obvious what actually ran.
    let resolved = genai::resolve_model(&request.model);
    if resolved == request.model {
        eprintln!("{verb} with {resolved}…");
    } else {
        eprintln!("{verb} with {resolved} (via \"{}\")…", request.model);
    }

    let image = Client::from_env()?.generate(&request)?;
    let written = write_image(&out, &image.bytes)?;

    if let Some(commentary) = &image.commentary {
        if !commentary.is_empty() {
            eprintln!("{commentary}");
        }
    }
    eprintln!("Wrote {} ({} KB)", written.display(), image.bytes.len() / 1024);

    // The path alone on stdout, so this composes in a pipeline.
    println!("{}", written.display());
    Ok(())
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

    Ok(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}
