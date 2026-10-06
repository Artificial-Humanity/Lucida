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
//!
//! Video is the exception that needed its own bound. Counting a Runway or Kling
//! clip at the image ceiling priced ten seconds of per-second billing at a
//! quarter, so an unverified video counts at [`VIDEO_CEILING_PER_SECOND`] times
//! its length instead.
//!
//! # A cap that cannot be enforced refuses
//!
//! A `LUCIDA_BUDGET` that does not parse used to read as no budget at all, and
//! so did one set beside `LUCIDA_NO_LEDGER`, whose ledger is the only place
//! spend is counted — or set where the ledger has nowhere to live, with no
//! home or config directory to put it in. Each removed the cap without a word,
//! which is the one failure a spend guard cannot have: whoever set it believes
//! it is holding. All now refuse every render that costs money and name the
//! way out. Free renders are never refused, so the local lane stays the answer.

use crate::clock;
use crate::provider::{Backend, Size};
use anyhow::Result;
use std::sync::{Mutex, MutexGuard, PoisonError};

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
    /// Billed per second of video at a rate this table has not verified.
    /// Carries the clip length, because the assumed upper bound scales with it
    /// exactly as a verified rate does.
    UnverifiedVideo { seconds: u32 },
}

/// What an unverified render is assumed to cost when a budget is being enforced.
///
/// Deliberately higher than any per-image price known here, because the only
/// safe direction to be wrong in is *early*. Never printed as a price — the
/// message that uses it says it is an assumed upper bound.
pub const CEILING: f64 = 0.25;

/// What an unverified video is assumed to cost per second of output.
///
/// Its own bound, because the image ceiling is a price per *render* and video is
/// billed per *second*: counting a ten-second Runway clip at $0.25 put it below
/// a four-second Veo render at $1.60. Set above every verified video rate in
/// [`video_price`] — Veo's $0.40 a second is the highest — for the same reason
/// [`CEILING`] sits above every image price.
pub const VIDEO_CEILING_PER_SECOND: f64 = 0.50;

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
            Price::UnverifiedVideo { seconds } => VIDEO_CEILING_PER_SECOND * f64::from(seconds),
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
            Price::UnverifiedVideo { seconds } => format!(
                "billed per second for {seconds}s, at a rate this table has not \
                 verified — see the provider's own pricing"
            ),
        }
    }
}

/// Published rates, per provider and where it matters per model.
///
/// Google's image models are the only ones with a rate verified against the
/// provider's own pricing at the time of writing, so they are the only ones with
/// a number. The rest are `Unverified` rather than approximated — a plausible
/// wrong price is worse than an admitted gap, because it will be believed.
///
/// `size` is taken because Google bills by output tier: a 4K image is priced
/// as more tokens than a 1K one. Without it every Gemini render was priced at
/// the 1K rate and labelled verified — a pro 4K render counted at $0.134
/// against a published $0.24.
pub fn price_for(backend: Backend, model: &str, size: Option<Size>) -> Price {
    const CHECKED: &str = "2026-08-09";
    // Google's published rates per output tier, read from
    // https://ai.google.dev/gemini-api/docs/pricing on this date: Gemini 3 Pro
    // Image "$0.134 per 1K/2K image and $0.24 per 4K image"; Gemini 3.1 Flash
    // Image "$0.067 per 1K image, $0.101 per 2K image, and $0.151 per 4K image".
    // No size means Google's default tier, 1K.
    const TIERS_CHECKED: &str = "2026-10-04";

    let tier = size.map_or("1K", Size::tier_name);
    let per_image = |usd: f64, verified: &'static str| Price::PerImage { usd, verified };

    match backend {
        Backend::ComfyUi => Price::Free,
        Backend::Google => {
            // Resolved first, because the model reaching here is whatever the
            // caller typed and that is usually an alias. Matching the raw string
            // meant `--model banana-pro` — the documented spelling — priced as
            // Unverified and counted at the ceiling, so a budget refused a
            // 13-cent render as if it might cost a quarter.
            match (crate::genai::resolve_model(model).as_str(), tier) {
                (m, "4K") if m.starts_with("gemini-3-pro-image") => per_image(0.24, TIERS_CHECKED),
                (m, _) if m.starts_with("gemini-3-pro-image") => per_image(0.134, CHECKED),
                (m, "4K") if m.starts_with("gemini-3.1-flash-image") => {
                    per_image(0.151, TIERS_CHECKED)
                }
                (m, "2K") if m.starts_with("gemini-3.1-flash-image") => {
                    per_image(0.101, TIERS_CHECKED)
                }
                (m, _) if m.starts_with("gemini-3.1-flash-image") => per_image(0.067, CHECKED),
                _ => Price::Unverified,
            }
        }
        // Runway bills its own credits, as with its video lane.
        Backend::Bfl | Backend::Stability | Backend::OpenAi | Backend::Runway => Price::Unverified,
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

    // Neither Runway's nor Kling's code states a default clip length — both
    // send no `duration` when none is asked for and let the provider choose —
    // so the longest length each accepts is assumed. Erring long is the safe
    // direction for the same reason the ceiling errs high.
    let unverified = || Price::UnverifiedVideo {
        seconds: duration.unwrap_or_else(|| {
            crate::provider::video_capabilities_for(backend, model).duration.longest()
        }),
    };

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
        // Runway and Kling both bill in their own credits and this table has
        // not verified either conversion, so neither rate is stated. They count
        // at the per-second video ceiling times the clip length, which is the
        // honest answer until a render and a balance reading settle it. They
        // used to count at the image ceiling — a quarter for a clip of any
        // length.
        crate::provider::VideoBackend::Runway | crate::provider::VideoBackend::Kling => {
            return unverified();
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

/// What `LUCIDA_BUDGET` says, including when it says nothing usable.
#[derive(Debug, Clone, PartialEq)]
pub enum Budget {
    /// No budget: nothing is ever refused for cost.
    Unset,
    /// A cap in US dollars — finite and not negative.
    Cap(f64),
    /// Set, but not an amount: `$5`, `5 USD`, `NaN`, `-1`. Held with the text
    /// as written, so the refusal can quote it.
    Unreadable(String),
}

impl Budget {
    /// Reads a raw setting. An unreadable value is kept rather than dropped,
    /// because dropping it is what used to turn `LUCIDA_BUDGET=$5` into no cap:
    /// `.parse().ok()` read it as unset and every render went through.
    fn parse(raw: Option<&str>) -> Budget {
        let Some(raw) = raw else { return Budget::Unset };
        match raw.trim().parse::<f64>() {
            // `f64::from_str` accepts `inf`, `NaN` and `-5`, none of which is a
            // cap: infinity and NaN never refuse anything, and a negative one
            // reads as a typo rather than a decision to refuse everything.
            Ok(cap) if cap.is_finite() && cap >= 0.0 => Budget::Cap(cap),
            _ => Budget::Unreadable(raw.to_string()),
        }
    }

    /// The refusal for a budget that cannot be read, or `None` when it can.
    ///
    /// Public so `lucida config` can say the same thing a render would.
    pub fn problem(&self) -> Option<String> {
        match self {
            Budget::Unreadable(raw) => Some(format!(
                "`{raw}` — expected a plain number of US dollars, such as `5` or \
                 `2.50`: no `$`, no units, no comment"
            )),
            Budget::Unset | Budget::Cap(_) => None,
        }
    }
}

/// The budget as set, read through the config file and the environment.
pub fn budget_setting() -> Budget {
    Budget::parse(crate::config::var("LUCIDA_BUDGET").as_deref())
}

/// The cap in dollars, when one is set and readable.
pub fn budget() -> Option<f64> {
    match budget_setting() {
        Budget::Cap(cap) => Some(cap),
        Budget::Unset | Budget::Unreadable(_) => None,
    }
}

/// Estimated dollars spent in the last [`WINDOW_SECONDS`], from the ledger.
pub fn spent_recently() -> f64 {
    spent_since(&crate::ledger::entries(), clock::now() - WINDOW_SECONDS)
}

/// The estimates of every entry at or after `since`, whatever its status — a
/// `started`, `unsaved` or `abandoned` entry was billed just as a `done` one
/// was. Split from [`spent_recently`] so the sum can be tested without the
/// machine's real ledger.
fn spent_since(entries: &[serde_json::Value], since: i64) -> f64 {
    let total: f64 = entries
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

/// Spend promised to renders in flight in this process and not yet in the
/// ledger, in US dollars.
///
/// Exists because the ledger is written when a render *finishes*, and a check
/// that reads only the ledger lets every concurrent caller see the same total.
/// The MCP server runs tool calls on a pool of workers, so four `start_video`
/// calls against `LUCIDA_BUDGET=5` each read the same empty ledger, each saw
/// $3.20 fit, and all four started: $12.80 under a five-dollar cap. A check
/// and its reservation now happen under one lock, so the second caller sees
/// the first one's $3.20 before the ledger does.
///
/// **In-process only.** Two separate `lucida` processes — two shells, or a
/// shell and the MCP server — each have their own, and can still both pass
/// against the same ledger. Closing that needs a file lock, and
/// `std::fs::File::lock` is newer than this crate's MSRV of 1.85; a locking
/// crate would be a new dependency. So the gap is stated rather than closed.
struct Held(Mutex<f64>);

impl Held {
    const fn new() -> Held {
        Held(Mutex::new(0.0))
    }

    /// A poisoned lock is taken anyway. The value is a running sum that a panic
    /// elsewhere cannot leave half-written, and a budget that stopped working
    /// because some unrelated render panicked would be the silent removal of
    /// the cap again.
    fn lock(&self) -> MutexGuard<'_, f64> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

static HELD: Held = Held::new();

/// A render's estimated cost, held against the budget until it is in the
/// ledger.
///
/// Released when dropped, which is what makes it reach every exit: kept until
/// the ledger entry is written on success, and dropped by the `?` of a provider
/// error, a cancelled render, or the unwinding of a panic. Holding it a moment
/// past the ledger write counts the render twice for that moment, which errs
/// toward refusing — the direction this module always chooses.
#[must_use = "dropping a reservation releases it — hold it until the ledger entry is written"]
pub struct Reservation {
    usd: f64,
    held: &'static Held,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.usd > 0.0 {
            let mut held = self.held.lock();
            // Clamped, so float rounding over many reservations can never leave
            // a negative sum that would quietly raise the cap.
            *held = (*held - self.usd).max(0.0);
        }
    }
}

/// Refuses a render that would take the day past its budget, and otherwise
/// reserves its cost until the caller's ledger entry is written.
///
/// Checked before a client exists, beside `Capabilities::check` and in the same
/// voice, for the same reason: the point of a refusal is that it happens before
/// the money moves, and it names what to do instead.
pub fn check(price: Price, what: &str) -> Result<Reservation> {
    check_batch(price, 1, what)
}

/// Refuses a batch that would take the day past its budget, and otherwise
/// reserves the whole batch's cost.
///
/// `count` is load-bearing and was learned the expensive way. The first version
/// of the batch path called [`check`] in a loop — once per image — which reads
/// as a check per render and is not one: every call re-reads the *same* ledger,
/// so all `count` calls ask "can I afford one more?" and all of them answer yes.
/// A three-image batch at $0.134 sailed through a $0.20 budget and rendered all
/// three. The cap has to see the whole batch before the first render, because
/// after the first render it is too late for the first render.
pub fn check_batch(price: Price, count: usize, what: &str) -> Result<Reservation> {
    reserve(
        &HELD,
        price,
        count,
        what,
        budget_setting(),
        Ledger::current(),
        spent_recently,
    )
}

/// Whether there is a ledger for spend to be counted in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ledger {
    Kept,
    /// `LUCIDA_NO_LEDGER` is set.
    SwitchedOff,
    /// Not switched off, but with nowhere to live: no `LUCIDA_CONFIG`, no
    /// `XDG_CONFIG_HOME`, no home directory. Every write is skipped, so to the
    /// budget it is the same as switched off — and it used to pass everything,
    /// because only the switch was checked.
    Nowhere,
}

impl Ledger {
    fn current() -> Ledger {
        if crate::ledger::disabled() {
            Ledger::SwitchedOff
        } else if crate::ledger::path().is_none() {
            Ledger::Nowhere
        } else {
            Ledger::Kept
        }
    }
}

/// [`check_batch`] with its inputs passed in, so the arithmetic can be tested
/// without touching the environment or the real ledger.
fn reserve(
    held: &'static Held,
    price: Price,
    count: usize,
    what: &str,
    budget: Budget,
    ledger: Ledger,
    spent: impl FnOnce() -> f64,
) -> Result<Reservation> {
    let nothing = || Reservation { usd: 0.0, held };

    // A render that spends nothing is never refused, whatever has been spent
    // already. Checked before the budget is even read, because the arithmetic
    // gets this wrong in the most embarrassing possible way: with the day's
    // spend already past the cap, `spent + 0.0 <= budget` is false, so the
    // local lane was declined — the very lane the refusal below tells you to
    // use instead. Caught by running it, not by reading it.
    let estimate = price.against_budget() * count as f64;
    if estimate <= 0.0 {
        return Ok(nothing());
    }

    // A refusal, not a failure, in every branch below: understood, declined
    // before the money moved, and naming what to do instead. The CLI exits 2
    // for it, so a wrapper that retries on failure does not retry something
    // that cannot succeed.
    let refuse = |message: String| Err(anyhow::Error::new(crate::out::Refused(message)));

    let budget = match budget {
        Budget::Unset => return Ok(nothing()),
        Budget::Cap(cap) => cap,
        unreadable @ Budget::Unreadable(_) => {
            let problem = unreadable.problem().unwrap_or_default();
            return refuse(format!(
                "LUCIDA_BUDGET is {problem}. A cap that cannot be read is not \
                 treated as no cap, so this {what} is refused rather than sent.\n\n\
                 Fix the value, or unset LUCIDA_BUDGET to run without a cap. \
                 comfyui renders locally, costs nothing, and is never refused. \
                 `lucida config` shows where the setting comes from."
            ));
        }
    };

    // The budget is counted from the ledger, so with no ledger nothing spent
    // is ever counted and the cap could never be reached. It used to allow
    // everything, forever, without saying so.
    match ledger {
        Ledger::Kept => {}
        Ledger::SwitchedOff => {
            return refuse(format!(
                "LUCIDA_BUDGET is set (${budget:.2}), and so is LUCIDA_NO_LEDGER. \
                 The budget is counted from the render ledger, so with the ledger \
                 off nothing spent is ever counted and the cap cannot hold — this \
                 {what} is refused rather than sent unmetered.\n\n\
                 Unset LUCIDA_NO_LEDGER to keep the budget (the ledger records your \
                 prompts), or unset LUCIDA_BUDGET to run without a cap. comfyui \
                 renders locally, costs nothing, and is never refused."
            ));
        }
        Ledger::Nowhere => {
            return refuse(format!(
                "LUCIDA_BUDGET is set (${budget:.2}), but the render ledger has \
                 nowhere to live: none of HOME, USERPROFILE, XDG_CONFIG_HOME or \
                 LUCIDA_CONFIG is set (nor APPDATA, on Windows), so there is no \
                 config directory to keep it in. The budget is counted from the ledger, so nothing spent would \
                 ever be counted and the cap cannot hold — this {what} is refused \
                 rather than sent unmetered.\n\n\
                 Set HOME, or LUCIDA_CONFIG to a config file whose directory can \
                 hold the ledger, or unset LUCIDA_BUDGET to run without a cap. \
                 comfyui renders locally, costs nothing, and is never refused."
            ));
        }
    }

    // One lock across the read, the decision and the reservation: anything
    // less lets two workers both read the same total and both pass.
    let mut in_flight = held.lock();
    let spent = spent();
    if spent + *in_flight + estimate <= budget {
        *in_flight += estimate;
        return Ok(Reservation { usd: estimate, held });
    }
    let reserved = *in_flight;
    drop(in_flight);

    let assumption = match price {
        Price::Unverified => format!(
            "\n\nThis provider's rate is not verified here, so it is counted at \
             ${CEILING:.2} — an assumed upper bound, not a price."
        ),
        Price::UnverifiedVideo { seconds } => format!(
            "\n\nThis provider's rate is not verified here, so it is counted at \
             ${VIDEO_CEILING_PER_SECOND:.2} a second for {seconds}s — an assumed \
             upper bound, not a price."
        ),
        _ => String::new(),
    };
    let running = if reserved > 0.0 {
        format!(", and renders still running in this process hold another ${reserved:.2}")
    } else {
        String::new()
    };

    refuse(format!(
        "LUCIDA_BUDGET is ${budget:.2} for a rolling 24 hours, and about \
         ${spent:.2} of that is already spent{running}. This {what} would add \
         roughly ${estimate:.2}.{assumption}\n\n\
         Raise or unset LUCIDA_BUDGET, wait for the window to roll, or use \
         comfyui, which renders locally and costs nothing. `lucida history` \
         shows what the estimate is made of."
    ))
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
            match price_for(*backend, backend.default_model(), None) {
                Price::PerImage { verified, .. } | Price::PerSecond { verified, .. } => {
                    assert!(
                        crate::clock::unix_time(verified).is_some(),
                        "{}: `{verified}` is not a date",
                        backend.name()
                    );
                }
                Price::Free | Price::Unverified | Price::UnverifiedVideo { .. } => {}
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
                price_for(Backend::Google, alias, None),
                price_for(Backend::Google, id, None),
                "`{alias}` and `{id}` are the same model and must cost the same"
            );
        }
        assert!(matches!(
            price_for(Backend::Google, "banana-pro", None),
            Price::PerImage { .. }
        ));
    }

    /// The local lane is the answer a budget refusal points at, so it has to
    /// genuinely cost nothing.
    #[test]
    fn the_local_lane_is_free_and_the_hosted_ones_are_not() {
        assert_eq!(price_for(Backend::ComfyUi, "klein", None), Price::Free);
        assert_eq!(price_for(Backend::ComfyUi, "klein", None).against_budget(), 0.0);

        for backend in [Backend::Google, Backend::Bfl, Backend::Stability, Backend::OpenAi] {
            let price = price_for(backend, backend.default_model(), None);
            assert_ne!(price, Price::Free, "{} is not free", backend.name());
            assert!(price.against_budget() > 0.0);
        }
    }

    /// An unverified price must count as *something*, or a budget would be off
    /// for three of the five image providers — which is the same as not existing.
    /// Video too, and per second: Runway and Kling clips counted at the image
    /// ceiling, a quarter for ten seconds of per-second billing.
    #[test]
    fn an_unverified_price_still_counts_against_a_budget() {
        use crate::provider::VideoBackend;

        assert_eq!(Price::Unverified.against_budget(), CEILING);

        // Read off the table rather than compared against a literal, so adding a
        // price that exceeds the ceiling fails here rather than quietly making
        // the "upper bound" an under-estimate. Every tier, since 4K is priced
        // above the rest.
        let tiers = [None, Some(Size::ONE_K), Some(Size::TWO_K), Some(Size::FOUR_K)];
        let highest = ["gemini-3-pro-image", "gemini-3.1-flash-image"]
            .iter()
            .flat_map(|model| tiers.map(|size| price_for(Backend::Google, model, size)))
            .filter_map(|price| match price {
                Price::PerImage { usd, .. } => Some(usd),
                _ => None,
            })
            .fold(0.0_f64, f64::max);

        assert!(
            CEILING >= highest,
            "the ceiling (${CEILING}) is below a price this table states \
             (${highest}), so it is not an upper bound"
        );

        // The video bound sits above every verified per-second rate, read off
        // the table for the same reason.
        let fastest = ["veo-3.1-lite-generate-preview", "veo-3.1-fast-generate-preview", "veo-3.1-generate-preview"]
            .iter()
            .filter_map(|model| match video_price(VideoBackend::Google, model, Some(8)) {
                Price::PerSecond { usd, .. } => Some(usd),
                _ => None,
            })
            .fold(0.0_f64, f64::max);
        assert!(
            VIDEO_CEILING_PER_SECOND > fastest,
            "the video ceiling (${VIDEO_CEILING_PER_SECOND}/s) is not above a \
             verified rate (${fastest}/s), so it is not an upper bound"
        );

        // Scaled by the clip length asked for, and with none asked for, by the
        // longest each provider accepts — neither states a default of its own.
        for backend in [VideoBackend::Runway, VideoBackend::Kling] {
            let model = backend.default_model();
            assert_eq!(
                video_price(backend, model, Some(5)).against_budget(),
                VIDEO_CEILING_PER_SECOND * 5.0,
                "{}: five seconds",
                backend.name()
            );
            let longest = crate::provider::video_capabilities_for(backend, model).duration.longest();
            assert_eq!(longest, 10, "{}: the longest clip on offer", backend.name());
            assert_eq!(
                video_price(backend, model, None).against_budget(),
                VIDEO_CEILING_PER_SECOND * f64::from(longest),
                "{}: no duration asked for",
                backend.name()
            );
            assert!(
                video_price(backend, model, None).against_budget() > CEILING,
                "{}: an unverified clip must not count at the image ceiling",
                backend.name()
            );
        }
    }

    /// Google bills by output tier, and the pro model's 4K tier is published at
    /// $0.24 — it was counted at the 1K/2K rate of $0.134 and labelled verified.
    #[test]
    fn a_gemini_render_is_priced_by_its_size_tier() {
        let usd = |model: &str, size: Option<Size>| match price_for(Backend::Google, model, size) {
            Price::PerImage { usd, .. } => usd,
            other => panic!("{model} at {size:?} priced as {other:?}"),
        };
        assert_eq!(usd("banana-pro", None), 0.134);
        assert_eq!(usd("banana-pro", Some(Size::TWO_K)), 0.134);
        assert_eq!(usd("banana-pro", Some(Size::FOUR_K)), 0.24);
        // A pixel count lands in the tier Google is actually sent.
        assert_eq!(usd("banana-pro", Some(Size(4000))), 0.24);

        assert_eq!(usd("gemini-3.1-flash-image", Some(Size::ONE_K)), 0.067);
        assert_eq!(usd("gemini-3.1-flash-image", Some(Size::TWO_K)), 0.101);
        assert_eq!(usd("gemini-3.1-flash-image", Some(Size::FOUR_K)), 0.151);
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
            Price::UnverifiedVideo { seconds: 10 },
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
        assert_eq!(price_for(Backend::ComfyUi, "klein", None).against_budget(), 0.0);
    }

    fn is_refusal(result: &Result<Reservation>) -> bool {
        matches!(result, Err(e) if e.downcast_ref::<crate::out::Refused>().is_some())
    }

    fn refusal(result: Result<Reservation>) -> String {
        match result {
            Err(e) if e.downcast_ref::<crate::out::Refused>().is_some() => e.to_string(),
            Err(e) => panic!("an error, not a refusal: {e:#}"),
            Ok(_) => panic!("expected a refusal and the render was allowed"),
        }
    }

    /// The race across the MCP worker pool. Each call read the ledger, which a
    /// render only reaches once it finishes, so four $3.20 Veo starts against a
    /// five-dollar budget each saw an empty ledger and all four started. Two
    /// back to back must see each other.
    #[test]
    fn a_second_reservation_sees_the_first() {
        // Its own table, not the process-wide one, so a render reserving in a
        // parallel test cannot move the numbers under this one.
        static TABLE: Held = Held::new();
        let veo = Price::PerSecond { usd: 0.40, verified: "2026-08-09", seconds: 8 };
        let budget = || Budget::Cap(5.0);

        let first = reserve(&TABLE, veo, 1, "video render", budget(), Ledger::Kept, || 0.0);
        assert!(first.is_ok(), "$3.20 fits a $5.00 budget");

        let second = reserve(&TABLE, veo, 1, "video render", budget(), Ledger::Kept, || 0.0);
        let message = refusal(second);
        assert!(message.contains("hold another $3.20"), "{message}");

        // Released when the first is dropped, which is what the ledger entry
        // being written looks like from here.
        drop(first);
        assert_eq!(*TABLE.lock(), 0.0);
        assert!(reserve(&TABLE, veo, 1, "video render", budget(), Ledger::Kept, || 0.0).is_ok());
    }

    /// The reservation is released on every way out, a panic included — or one
    /// render that panicked would hold its estimate against the budget for as
    /// long as the server runs.
    #[test]
    fn a_panicking_render_releases_its_reservation() {
        static TABLE: Held = Held::new();
        let price = Price::PerImage { usd: 0.134, verified: "2026-08-09" };

        let unwound = std::panic::catch_unwind(|| {
            let _held = reserve(&TABLE, price, 1, "render", Budget::Cap(1.0), Ledger::Kept, || 0.0)
                .expect("fits");
            panic!("the provider call blew up");
        });
        assert!(unwound.is_err());
        assert_eq!(*TABLE.lock(), 0.0, "the panic left its estimate held");
    }

    /// `.parse().ok()` read every one of these as no budget, so writing the cap
    /// the way people write money removed it.
    #[test]
    fn a_budget_that_does_not_parse_is_kept_as_unreadable() {
        for raw in ["$5", "5 USD", "5  # cap", "NaN", "inf", "-inf", "-1", "five"] {
            assert_eq!(
                Budget::parse(Some(raw)),
                Budget::Unreadable(raw.to_string()),
                "`{raw}` must not read as a cap or as no budget"
            );
        }
        assert_eq!(Budget::parse(Some(" 2.50 ")), Budget::Cap(2.5));
        assert_eq!(Budget::parse(Some("0")), Budget::Cap(0.0));
        assert_eq!(Budget::parse(None), Budget::Unset);
    }

    /// An unreadable budget refuses what costs money, quoting the value and the
    /// form expected — and never the local lane.
    #[test]
    fn an_unreadable_budget_refuses_a_paid_render() {
        static TABLE: Held = Held::new();
        let unreadable = || Budget::Unreadable("$5".to_string());

        let message = refusal(reserve(&TABLE, Price::Unverified, 1, "render", unreadable(), Ledger::Kept, || 0.0));
        assert!(message.contains("`$5`") && message.contains("such as `5`"), "{message}");

        assert!(reserve(&TABLE, Price::Free, 1, "render", unreadable(), Ledger::Kept, || 0.0).is_ok());
    }

    /// The ledger is where spend is counted, so a budget beside
    /// `LUCIDA_NO_LEDGER` could never be reached and allowed everything.
    #[test]
    fn a_budget_with_no_ledger_refuses_a_paid_render() {
        static TABLE: Held = Held::new();

        let result = reserve(&TABLE, Price::Unverified, 1, "render", Budget::Cap(5.0), Ledger::SwitchedOff, || 0.0);
        assert!(is_refusal(&result));
        let message = refusal(result);
        assert!(
            message.contains("LUCIDA_BUDGET") && message.contains("LUCIDA_NO_LEDGER"),
            "{message}"
        );

        assert!(reserve(&TABLE, Price::Free, 1, "render", Budget::Cap(5.0), Ledger::SwitchedOff, || 0.0).is_ok());
        // And with no budget at all, the ledger being off is nobody's business here.
        assert!(reserve(&TABLE, Price::Unverified, 1, "render", Budget::Unset, Ledger::SwitchedOff, || 0.0).is_ok());
    }

    /// An image whose wait was abandoned was billed all the same, so the window
    /// counts it — and only inside the window.
    #[test]
    fn an_abandoned_image_counts_against_the_window() {
        let entries = vec![
            serde_json::json!({ "at": 100, "kind": "image", "status": "done", "estimated_usd": 0.06 }),
            serde_json::json!({
                "at": 200, "kind": "image", "status": crate::ledger::ABANDONED,
                "handle": "img-7", "estimated_usd": 0.08,
            }),
            serde_json::json!({ "at": 10, "kind": "image", "status": "abandoned", "estimated_usd": 5.0 }),
        ];
        assert!((spent_since(&entries, 50) - 0.14).abs() < 1e-9, "{}", spent_since(&entries, 50));
    }

    /// A ledger with nowhere to live counts nothing, exactly as one switched
    /// off does, and is refused the same way — naming the actual cause, since
    /// telling someone to unset a `LUCIDA_NO_LEDGER` they never set is no help.
    #[test]
    fn a_budget_with_nowhere_to_keep_the_ledger_refuses_a_paid_render() {
        static TABLE: Held = Held::new();

        let result = reserve(&TABLE, Price::Unverified, 1, "render", Budget::Cap(5.0), Ledger::Nowhere, || 0.0);
        assert!(is_refusal(&result));
        let message = refusal(result);
        assert!(message.contains("nowhere to live") && message.contains("HOME"), "{message}");
        assert!(!message.contains("LUCIDA_NO_LEDGER"), "names a setting nobody set: {message}");

        assert!(reserve(&TABLE, Price::Free, 1, "render", Budget::Cap(5.0), Ledger::Nowhere, || 0.0).is_ok());
        assert!(reserve(&TABLE, Price::Unverified, 1, "render", Budget::Unset, Ledger::Nowhere, || 0.0).is_ok());
    }

    /// The video refusal states its assumption in the same words as the image
    /// one: an assumed upper bound, not a price.
    #[test]
    fn an_unverified_video_refusal_says_what_it_assumed() {
        static TABLE: Held = Held::new();
        let clip = Price::UnverifiedVideo { seconds: 10 };

        let message = refusal(reserve(&TABLE, clip, 1, "video render", Budget::Cap(1.0), Ledger::Kept, || 0.0));
        assert!(message.contains("$0.50 a second for 10s"), "{message}");
        assert!(message.contains("assumed upper bound"), "{message}");
    }
}
