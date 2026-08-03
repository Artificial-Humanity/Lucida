//! `lucida skill` — the agent skill, printed.
//!
//! # Why the binary carries it
//!
//! The skill lives at `skills/lucida/SKILL.md` and is compiled in with
//! `include_str!`, so the file in the repository *is* the file that ships. Not a
//! copy of it, not a build step that could be forgotten — the same bytes, and a
//! missing file is a compile error rather than a stale artefact.
//!
//! That matters because of who needs it. Someone installing with the one-liner
//! gets a binary and never sees the repository, so a skill that only exists on
//! GitHub is invisible to exactly the people it was written for. Shipping it
//! inside the binary also means it updates with `lucida update`: a user cannot
//! be running one version while holding another version's skill.
//!
//! # Why it prints rather than installs
//!
//! Skill directories differ by client — `~/.claude/skills`, a project's
//! `.agents/skills`, and others — and Lucida has no way to know which one is
//! meant. Guessing would either write somewhere unwanted or invent a convention
//! that is not anyone's. Printing puts that decision where it belongs:
//!
//! ```console
//! $ lucida skill > ~/.claude/skills/lucida/SKILL.md
//! ```
//!
//! The same division as everywhere else here: `generate` prints the path it
//! wrote rather than opening the file, and `config` reports what it can see
//! rather than editing a shell profile.

/// The skill itself. `include_str!` rather than a runtime read, so this cannot
/// disagree with the repository and cannot go missing at runtime.
pub const SKILL: &str = include_str!("../skills/lucida/SKILL.md");

pub fn print() {
    print!("{SKILL}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parsed by lines rather than by byte offsets into `\n`.
    ///
    /// The offset version asserted `starts_with("---\n")` and passed everywhere
    /// it was written, then failed on the Windows CI runner: git checks text
    /// files out as CRLF there, so `include_str!` embedded `---\r\n`. The same
    /// shape of mistake as reading the clock once per call — a platform
    /// assumption with no platform named in it, invisible to the machine that
    /// wrote it.
    ///
    /// `.gitattributes` now pins these files to LF, which is the real fix and
    /// keeps `lucida skill` emitting the bytes the repository holds. This test
    /// is deliberately tolerant anyway: it should be checking that a client can
    /// read the frontmatter, not which line ending arrived.
    #[test]
    fn the_skill_has_the_frontmatter_a_client_needs() {
        let mut lines = SKILL.lines();
        assert_eq!(lines.next().map(str::trim), Some("---"), "no frontmatter block");

        let front: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "---").collect();
        assert!(!front.is_empty(), "unterminated frontmatter");
        let front = front.join("\n");

        assert!(front.contains("name: lucida"), "{front}");

        // The description is what a client matches on to decide the skill is
        // relevant, so an empty one makes the file unreachable rather than
        // merely terse.
        let description = front
            .lines()
            .find_map(|l| l.strip_prefix("description: "))
            .expect("no description");
        assert!(description.len() > 40, "description too thin: {description}");
    }

    /// The property the whole file depends on, enforced rather than asserted in
    /// prose.
    ///
    /// The skill claims, in its own second paragraph, to contain no capability
    /// facts — that is what lets it stay true as providers are added and change.
    /// A provider or model-family name appearing in it is the first symptom of
    /// that claim becoming false, and it is invisible to every other test.
    ///
    /// This caught a real one: the opening line read "across five providers and
    /// video through Veo", which is both a count and a provider name, in the
    /// paragraph immediately above the claim.
    #[test]
    fn the_skill_names_no_provider_or_model_family() {
        let lower = SKILL.to_lowercase();
        for name in [
            "google", "comfyui", "bfl", "stability", "openai", "gemini", "flux", "gpt-image",
            "veo", "banana", "imagen", "black forest",
        ] {
            assert!(
                !lower.contains(name),
                "the skill names `{name}` — capabilities belong in image_providers, \
                 which is current, not here, which is a snapshot"
            );
        }
    }

    /// A count rots the same way a name does, and did: the MCP schema once said
    /// "Four providers are available" and was wrong the day a fifth landed.
    #[test]
    fn the_skill_states_no_provider_count() {
        let lower = SKILL.to_lowercase();
        for count in [
            "one provider",
            "two providers",
            "three providers",
            "four providers",
            "five providers",
            "six providers",
            "seven providers",
        ] {
            assert!(!lower.contains(count), "the skill counts providers: `{count}`");
        }
    }

    /// `lucida skill` promises the bytes the repository holds, and that promise
    /// is only true if the checkout does not rewrite them.
    ///
    /// Guards `.gitattributes`: without `eol=lf`, a Windows checkout embeds
    /// CRLF and the shipped skill differs from the repository's by a byte on
    /// every line. Nothing else would notice — the file still parses, still
    /// renders, still installs.
    #[test]
    fn the_embedded_skill_is_lf_on_every_platform() {
        assert!(
            !SKILL.contains('\r'),
            "the embedded skill contains CR — has .gitattributes lost `*.md text eol=lf`?"
        );
    }

    #[test]
    fn the_skill_points_at_the_live_answer() {
        // Saying what it does not contain is only useful alongside where to get
        // it instead.
        assert!(SKILL.contains("image_providers"));
        assert!(SKILL.contains("lucida models --provider"));
    }
}
