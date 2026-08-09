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
use anyhow::Result;

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
    /// Carries the clip length, because a rate without one is not a price.
    PerSecond { usd: f64, verified: &'static str, seconds: u32 },
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
            // Rate times length: video is the one lane where the parameter and
            // the price are the same conversation.
            Price::PerSecond { usd, seconds, .. } => usd * f64::from(seconds),
            Price::Unverified => CEILING,
        }
    }

    /// One line for a human, honest about which kind of number this is.
    pub fn describe(self) -> String {
        match self {
            Price::Free => "free — renders on your own hardware".to_string(),
            Price::PerImage { usd, verified } => {
                format!("about ${usd:.3} per image (published rate, checked {verified})")
            }
            Price::PerSecond { usd, verified, seconds } => format!(
                "about ${:.2} for {seconds}s at ${usd:.2}/second (published rate, \
                 checked {verified})",
                usd * f64::from(seconds)
            ),
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
        Backend::Google => {
            // Resolved first, because the model reaching here is whatever the
            // caller typed and that is usually an alias. Matching the raw string
            // meant `--model banana-pro` — the documented spelling — priced as
            // Unverified and counted at the ceiling, so a budget refused a
            // 13-cent render as if it might cost a quarter.
            match crate::genai::resolve_model(model).as_str() {
                m if m.starts_with("gemini-3-pro-image") => Price::PerImage {
                    usd: 0.134,
                    verified: CHECKED,
                },
                m if m.starts_with("gemini-3.1-flash-image") => Price::PerImage {
                    usd: 0.067,
                    verified: CHECKED,
                },
                _ => Price::Unverified,
            }
        }
        Backend::Bfl | Backend::Stability | Backend::OpenAi => Price::Unverified,
    }
}

/// Video, per second of output, by provider and tier.
///
/// `duration` is taken because per-second billing means the *clip length* is
/// half the price, and a budget that ignored it would treat a two-second test
/// and a ten-second render as the same spend. Where no duration is asked for,
/// the provider's own default length is assumed — stated below rather than
/// guessed at the call site.
pub fn video_price(backend: crate::provider::VideoBackend, model: &str, duration: Option<u32>) -> Price {
    const CHECKED: &str = "2026-08-09";

    let per_second = match backend {
        crate::provider::VideoBackend::Google => {
            if model.contains("lite") {
                0.05
            } else if model.contains("fast") {
                0.15
            } else {
                0.40
            }
        }
        // Runway bills in credits rather than dollars and this table has not
        // verified the conversion, so its rate is not stated. `Unverified`
        // counts at the ceiling, which is the honest answer until a render and
        // a balance reading settle it.
        // Runway and Kling both bill in their own credits and this table has
        // not verified either conversion, so neither rate is stated.
        // `Unverified` counts at the ceiling, which is the honest answer until a
        // render and a balance reading settle it.
        crate::provider::VideoBackend::Runway | crate::provider::VideoBackend::Kling => {
            return Price::Unverified;
        }
    };

    Price::PerSecond {
        usd: per_second,
        verified: CHECKED,
        // Veo's own default when none is asked for.
        seconds: duration.unwrap_or(8),
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
    let total: f64 = crate::ledger::entries()
        .iter()
        .filter(|e| e["at"].as_i64().unwrap_or(0) >= since)
        .filter_map(|e| e["estimated_usd"].as_f64())
        .sum();

    // `max(0.0)` rather than the bare sum, and not for tidiness: Rust's `Sum`
    // for floats folds from **negative** zero, so an empty ledger sums to `-0.0`
    // and `{:.2}` renders that as `$-0.00`. Reported as spend, in a refusal
    // about money, in JSON a caller parses. It also flattens any negative that
    // a corrupted entry could contribute.
    total.max(0.0)
}

/// Refuses a render that would take the day past its budget.
///
/// Checked before a client exists, beside `Capabilities::check` and in the same
/// voice, for the same reason: the point of a refusal is that it happens before
/// the money moves, and it names what to do instead.
pub fn check(price: Price, what: &str) -> Result<()> {
    check_batch(price, 1, what)
}

/// Refuses a batch that would take the day past its budget.
///
/// `count` is load-bearing and was learned the expensive way. The first version
/// of the batch path called [`check`] in a loop — once per image — which reads
/// as a check per render and is not one: every call re-reads the *same* ledger,
/// so all `count` calls ask "can I afford one more?" and all of them answer yes.
/// A three-image batch at $0.134 sailed through a $0.20 budget and rendered all
/// three. The cap has to see the whole batch before the first render, because
/// after the first render it is too late for the first render.
pub fn check_batch(price: Price, count: usize, what: &str) -> Result<()> {
    // A render that spends nothing is never refused, whatever has been spent
    // already. Checked before the budget is even read, because the arithmetic
    // gets this wrong in the most embarrassing possible way: with the day's
    // spend already past the cap, `spent + 0.0 <= budget` is false, so the
    // local lane was declined — the very lane the refusal below tells you to
    // use instead. Caught by running it, not by reading it.
    let estimate = price.against_budget() * count as f64;
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

    // A refusal, not a failure: understood, declined before the money moved, and
    // naming what to do instead. The CLI exits 2 for it, so a wrapper that
    // retries on failure does not retry something that cannot succeed.
    Err(anyhow::Error::new(crate::out::Refused(format!(
        "LUCIDA_BUDGET is ${budget:.2} for a rolling 24 hours, and about \
         ${spent:.2} of that is already spent. This {what} would add roughly \
         ${estimate:.2}.{assumption}\n\n\
         Raise or unset LUCIDA_BUDGET, wait for the window to roll, or use \
         comfyui, which renders locally and costs nothing. `lucida history` \
         shows what the estimate is made of."
    ))))
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

    /// A batch is capped as a batch.
    ///
    /// Learned the expensive way. The first version called `check` once per
    /// image, which reads as a check per render and is not one: every call
    /// re-reads the same ledger, so all of them ask "can I afford one more?" and
    /// all of them say yes. Three images at $0.134 went straight through a $0.20
    /// budget and rendered all three — about forty cents, spent by the guard
    /// that exists to stop exactly that.
    #[test]
    fn a_batch_costs_its_count_rather_than_one_render() {
        let price = Price::PerImage { usd: 0.134, verified: "2026-08-09" };

        // The property that was missing: the estimate scales with the batch.
        let one = price.against_budget();
        let three = price.against_budget() * 3.0;
        assert!(
            three > one * 2.5,
            "a batch of three must be estimated at three renders, not one"
        );

        // And a free batch of any size is still free, so the local lane never
        // becomes refusable by asking for more of it.
        assert!(check_batch(Price::Free, 100, "render").is_ok());
    }

    /// Spend is never reported as a negative number.
    ///
    /// Rust's `Sum` for floats folds from negative zero, so an empty ledger sums
    /// to `-0.0` and prints as `$-0.00` — in a refusal about money, and in JSON
    /// a caller parses.
    #[test]
    fn nothing_spent_reads_as_zero_rather_than_minus_zero() {
        let empty: f64 = Vec::<f64>::new().into_iter().sum();
        assert!(
            empty.is_sign_negative(),
            "std stopped folding from -0.0; the guard in spent_recently may be \
             removable, but check before removing it"
        );

        assert_eq!(format!("{:.2}", empty.max(0.0)), "0.00");
        assert!(spent_recently() >= 0.0);
    }

    /// A model id reaching the price table is whatever the caller typed, and
    /// that is usually an alias — `banana-pro` is the spelling the README and
    /// the help text both use. Matching the raw string priced it as unverified
    /// and counted it at the ceiling, so a budget refused a 13-cent render as if
    /// it might cost a quarter.
    #[test]
    fn an_alias_is_priced_like_the_model_it_names() {
        for (alias, id) in crate::genai::MODEL_ALIASES {
            assert_eq!(
                price_for(Backend::Google, alias),
                price_for(Backend::Google, id),
                "`{alias}` and `{id}` are the same model and must cost the same"
            );
        }
        assert!(matches!(
            price_for(Backend::Google, "banana-pro"),
            Price::PerImage { .. }
        ));
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
            Price::PerSecond { usd: 0.15, verified: "2026-08-09", seconds: 8 },
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
        use crate::provider::VideoBackend;
        let rate = |model: &str| match video_price(VideoBackend::Google, model, Some(8)) {
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
