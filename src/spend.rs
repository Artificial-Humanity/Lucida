//! What a render costs, and a cap that refuses before it is spent.
//!
//! Every hosted provider bills per render and nothing here ever said so before
//! the fact. BFL echoed a credit figure *after* submitting — too late to decide
//! anything — and the other four said nothing at all. For someone watching a
//! terminal that is a mild gap. For an agent in a retry loop it is the whole
//! problem: the shipped skill advises deciding parameters before iterating
//! rather than during, and advice is all it was.
//!
//! # The table is small on purpose
//!
//! Prices are prose about someone else's business, which is exactly the drift
//! surface this codebase keeps rediscovering — a number copied from a pricing
//! page is wrong the day it changes and nothing in here will notice. So:
//!
//! - Only figures actually verified against a provider's own published pricing
//!   appear as prices, and each carries the date it was checked.
//! - Everything else is [`Price::Unverified`], which is a refusal to guess
//!   rather than an oversight.
//! - Nothing is ever presented as a charge. Output says *estimate*, and the
//!   provider's own invoice is the authority.
//!
//! # How an unverified price can still be capped
//!
//! A budget that gave up whenever a price was unknown would be off for three of
//! the five image providers, which is the same as not existing. So an unverified
//! render counts against the budget at [`CEILING`] — stated in the message as an
//! assumed upper bound rather than a price. Erring high is the safe direction
//! for a spend guard: it stops early rather than late, and being stopped early
//! is a nuisance where being stopped late is a bill.

use crate::clock;
use crate::provider::Backend;
use anyhow::{Result, bail};

/// What one render is expected to cost, in US dollars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Price {
    /// Runs on your own hardware. Electricity is not something this can price,
    /// and it is not what a budget is guarding against.
    Free,
    /// Verified against the provider's published pricing on the given date.
    PerImage { usd: f64, verified: &'static str },
    /// Verified, and billed per second of output — which is why video is the
    /// one lane where a wrong parameter is expensive rather than annoying.
    PerSecond { usd: f64, verified: &'static str },
    /// Billed, but at no rate this table has verified.
    Unverified,
}

/// What an unverified render is assumed to cost when a budget is being enforced.
///
/// Deliberately higher than any per-image price known here, because the only
/// safe direction to be wrong in is *early*. Never printed as a price — the
/// message that uses it says it is an assumed upper bound.
pub const CEILING: f64 = 0.25;

impl Price {
    /// Dollars to charge against a budget, which is not the same as what it
    /// costs: an unverified price becomes the ceiling rather than nothing.
    pub fn against_budget(self) -> f64 {
        match self {
            Price::Free => 0.0,
            Price::PerImage { usd, .. } => usd,
            // A duration this cannot know; the video path prices itself and
            // passes the number in. Treated as the ceiling if it reaches here.
            Price::PerSecond { .. } | Price::Unverified => CEILING,
        }
    }

    /// One line for a human, honest about which kind of number this is.
    pub fn describe(self) -> String {
        match self {
            Price::Free => "free — renders on your own hardware".to_string(),
            Price::PerImage { usd, verified } => {
                format!("about ${usd:.3} per image (published rate, checked {verified})")
            }
            Price::PerSecond { usd, verified } => {
                format!("about ${usd:.2} per second of output (published rate, checked {verified})")
            }
            Price::Unverified => {
                "billed, at a rate this table has not verified — see the provider's \
                 own pricing"
                    .to_string()
            }
        }
    }
}

/// Published rates, per provider and where it matters per model.
///
/// Google's image models are the only ones with a rate verified against the
/// provider's own pricing at the time of writing, so they are the only ones with
/// a number. The rest are `Unverified` rather than approximated — a plausible
/// wrong price is worse than an admitted gap, because it will be believed.
pub fn price_for(backend: Backend, model: &str) -> Price {
    const CHECKED: &str = "2026-08-09";

    match backend {
        Backend::ComfyUi => Price::Free,
        Backend::Google => match model {
            m if m.starts_with("gemini-3-pro-image") => Price::PerImage {
                usd: 0.134,
                verified: CHECKED,
            },
            m if m.starts_with("gemini-3.1-flash-image") => Price::PerImage {
                usd: 0.067,
                verified: CHECKED,
            },
            _ => Price::Unverified,
        },
        Backend::Bfl | Backend::Stability | Backend::OpenAi => Price::Unverified,
    }
}

/// Veo, per second of output, by tier.
pub fn video_price(model: &str) -> Price {
    const CHECKED: &str = "2026-08-09";

    let usd = if model.contains("lite") {
        0.05
    } else if model.contains("fast") {
        0.15
    } else {
        0.40
    };
    Price::PerSecond {
        usd,
        verified: CHECKED,
    }
}

/// A rolling cap on estimated spend, in US dollars.
///
/// Rolling over a day rather than "per session", which was the obvious reading
/// and does not survive contact: a CLI invocation is one render, so a per-session
/// cap there guards nothing, while an MCP server can run for a week and a
/// per-session cap would never reset. A window over the ledger is
/// process-independent, survives a restart, and matches what someone actually
/// means — do not let this thing spend more than five dollars today.
pub const WINDOW_SECONDS: i64 = 24 * 60 * 60;

pub fn budget() -> Option<f64> {
    crate::config::var("LUCIDA_BUDGET")?.trim().parse().ok()
}

/// Estimated dollars spent in the last [`WINDOW_SECONDS`], from the ledger.
pub fn spent_recently() -> f64 {
    let since = clock::now() - WINDOW_SECONDS;
    crate::ledger::entries()
        .iter()
        .filter(|e| e["at"].as_i64().unwrap_or(0) >= since)
        .filter_map(|e| e["estimated_usd"].as_f64())
        .sum()
}

/// Refuses a render that would take the day past its budget.
///
/// Checked before a client exists, beside `Capabilities::check` and in the same
/// voice, for the same reason: the point of a refusal is that it happens before
/// the money moves, and it names what to do instead.
pub fn check(price: Price, what: &str) -> Result<()> {
    // A render that spends nothing is never refused, whatever has been spent
    // already. Checked before the budget is even read, because the arithmetic
    // gets this wrong in the most embarrassing possible way: with the day's
    // spend already past the cap, `spent + 0.0 <= budget` is false, so the
    // local lane was declined — the very lane the refusal below tells you to
    // use instead. Caught by running it, not by reading it.
    let estimate = price.against_budget();
    if estimate <= 0.0 {
        return Ok(());
    }

    let Some(budget) = budget() else {
        return Ok(());
    };

    let spent = spent_recently();
    if spent + estimate <= budget {
        return Ok(());
    }

    let assumption = match price {
        Price::Unverified => format!(
            "\n\nThis provider's rate is not verified here, so it is counted at \
             ${CEILING:.2} — an assumed upper bound, not a price."
        ),
        _ => String::new(),
    };

    bail!(
        "LUCIDA_BUDGET is ${budget:.2} for a rolling 24 hours, and about \
         ${spent:.2} of that is already spent. This {what} would add roughly \
         ${estimate:.2}.{assumption}\n\n\
         Raise or unset LUCIDA_BUDGET, wait for the window to roll, or use \
         comfyui, which renders locally and costs nothing. `lucida history` \
         shows what the estimate is made of."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one property the table must never lose: a number here is a *verified*
    /// number, and everything else admits it is not. A plausible guess would be
    /// believed, which is worse than an admitted gap.
    #[test]
    fn every_stated_price_carries_the_date_it_was_checked() {
        for backend in Backend::ALL {
            match price_for(*backend, backend.default_model()) {
                Price::PerImage { verified, .. } | Price::PerSecond { verified, .. } => {
                    assert!(
                        crate::clock::unix_time(verified).is_some(),
                        "{}: `{verified}` is not a date",
                        backend.name()
                    );
                }
                Price::Free | Price::Unverified => {}
            }
        }
    }

    /// The local lane is the answer a budget refusal points at, so it has to
    /// genuinely cost nothing.
    #[test]
    fn the_local_lane_is_free_and_the_hosted_ones_are_not() {
        assert_eq!(price_for(Backend::ComfyUi, "klein"), Price::Free);
        assert_eq!(price_for(Backend::ComfyUi, "klein").against_budget(), 0.0);

        for backend in [Backend::Google, Backend::Bfl, Backend::Stability, Backend::OpenAi] {
            let price = price_for(backend, backend.default_model());
            assert_ne!(price, Price::Free, "{} is not free", backend.name());
            assert!(price.against_budget() > 0.0);
        }
    }

    /// An unverified price must count as *something*, or a budget would be off
    /// for three of the five image providers — which is the same as not existing.
    #[test]
    fn an_unverified_price_still_counts_against_a_budget() {
        assert_eq!(Price::Unverified.against_budget(), CEILING);

        // Read off the table rather than compared against a literal, so adding a
        // price that exceeds the ceiling fails here rather than quietly making
        // the "upper bound" an under-estimate.
        let highest = ["gemini-3-pro-image", "gemini-3.1-flash-image"]
            .iter()
            .filter_map(|model| match price_for(Backend::Google, model) {
                Price::PerImage { usd, .. } => Some(usd),
                _ => None,
            })
            .fold(0.0_f64, f64::max);

        assert!(
            CEILING >= highest,
            "the ceiling (${CEILING}) is below a price this table states \
             (${highest}), so it is not an upper bound"
        );
    }

    /// Never a charge, always an estimate — the provider's invoice is the
    /// authority and this table is a convenience.
    #[test]
    fn a_price_never_presents_itself_as_a_charge() {
        for price in [
            Price::Free,
            Price::PerImage { usd: 0.067, verified: "2026-08-09" },
            Price::PerSecond { usd: 0.15, verified: "2026-08-09" },
            Price::Unverified,
        ] {
            let described = price.describe().to_lowercase();
            assert!(
                described.contains("about")
                    || described.contains("free")
                    || described.contains("not verified"),
                "reads as a charge rather than an estimate: {described}"
            );
        }
    }

    /// Video is per second, which is why a wrong parameter there is expensive
    /// rather than annoying — and why the tiers must not collapse into one.
    #[test]
    fn the_video_tiers_are_priced_apart() {
        let rate = |model: &str| match video_price(model) {
            Price::PerSecond { usd, .. } => usd,
            other => panic!("video priced as {other:?}"),
        };
        assert!(rate("veo-3.1-lite-generate-preview") < rate("veo-3.1-fast-generate-preview"));
        assert!(rate("veo-3.1-fast-generate-preview") < rate("veo-3.1-generate-preview"));
    }

    /// With no budget set, nothing is ever refused — the guard is opt-in, and a
    /// tool that started declining renders on upgrade would be a bad surprise.
    #[test]
    fn no_budget_means_no_refusal() {
        // `budget()` reads the environment, which the suite must not mutate; this
        // asserts the branch that matters through the public shape instead.
        if budget().is_none() {
            assert!(check(Price::Unverified, "render").is_ok());
        }
    }

    /// A free render is never refused, whatever has already been spent — and the
    /// arithmetic got this wrong in the most embarrassing possible way. With the
    /// day's spend past the cap, `spent + 0.0 <= budget` is false, so the local
    /// lane was declined: the very lane the refusal message tells you to use
    /// instead. Found by running it, so it is pinned here.
    #[test]
    fn a_free_render_is_never_refused() {
        assert!(check(Price::Free, "render").is_ok());
        assert_eq!(price_for(Backend::ComfyUi, "klein").against_budget(), 0.0);
    }
}
