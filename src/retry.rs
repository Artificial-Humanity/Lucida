//! Retrying the requests where retrying is safe.
//!
//! Until this file existed there were no retries anywhere: every `.send()` in
//! the codebase was a single attempt. That is defensible for a request that
//! spends money — retrying a submit is how you get billed twice — but it is not
//! defensible for the *polls*, which is where the time is actually spent. A Veo
//! render is polled for minutes; one transient 502, one dropped connection, one
//! laptop sleep, and a paid render was abandoned mid-flight.
//!
//! So the rule this file encodes is not "retry harder", it is **retry exactly
//! the idempotent calls**:
//!
//! - polls, downloads, and the free key-check endpoints behind `lucida models`
//!   go through [`send_idempotent`];
//! - every endpoint that starts a render — genai's `generateContent`, BFL's and
//!   Stability's and OpenAI's generate/edit posts, ComfyUI's `/prompt`, Veo's
//!   `predictLongRunning` — deliberately does not, and calls `.send()` directly.
//!
//! The second half of that rule is the load-bearing one, and it looks exactly
//! like an oversight from the outside, which is why it is written down here as
//! well as at each site.
//!
//! What counts as transient is deliberately narrow: a connection that never
//! opened, a request that timed out, an explicit 429, or a 5xx. A 4xx is the
//! provider saying no, and asking again more slowly does not change its mind.

use reqwest::blocking::{RequestBuilder, Response};
use std::time::Duration;

/// Attempts in total, not retries after the first.
///
/// Three, because the failures this exists for are momentary — a load-balancer
/// blip, a re-established connection — and a call that has failed three times
/// over several seconds is failing for a reason that a fourth will not fix. The
/// poll loops above this each have their own deadline, and those are what bound
/// a genuinely stuck render.
const ATTEMPTS: u32 = 3;

const FIRST_BACKOFF: Duration = Duration::from_secs(1);

/// A cap on what a provider's own `Retry-After` can ask for. Honouring the
/// header is right; letting a header hold a render hostage for an hour is not.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Sends a request that is safe to send twice, retrying transient failures.
///
/// Takes a closure rather than a `RequestBuilder` because a builder is consumed
/// by `send`, and `try_clone` gives up on exactly the bodies worth retrying.
/// Building afresh per attempt also means the credential header is applied each
/// time by the same code that applies it once.
///
/// Returns `reqwest`'s own result so every caller keeps the error mapping it
/// already had — `.context(…)` in most providers, ComfyUI's `explain_transport`
/// in the one that diagnoses a missing local server.
pub fn send_idempotent(
    what: &str,
    build: impl Fn() -> RequestBuilder,
) -> reqwest::Result<Response> {
    send_with_backoff(what, FIRST_BACKOFF, build)
}

/// The mechanism, with the wait made a parameter so tests can exercise the
/// retry path without spending its real backoff in the suite.
fn send_with_backoff(
    what: &str,
    first_backoff: Duration,
    build: impl Fn() -> RequestBuilder,
) -> reqwest::Result<Response> {
    let mut backoff = first_backoff;

    for attempt in 1..=ATTEMPTS {
        let last = attempt == ATTEMPTS;

        match build().send() {
            Ok(response) => {
                if last || !worth_retrying(response.status()) {
                    return Ok(response);
                }
                let wait = retry_after(&response).unwrap_or(backoff);
                announce(what, &format!("{}", response.status()), attempt, wait);
                std::thread::sleep(wait);
            }
            Err(error) => {
                if last || !transient(&error) {
                    return Err(error);
                }
                announce(what, &describe(&error), attempt, backoff);
                std::thread::sleep(backoff);
            }
        }

        backoff = (backoff * 2).min(MAX_BACKOFF);
    }

    // Unreachable: the final attempt returns from inside the loop either way.
    // Written as a last attempt rather than an `unreachable!` so a future edit
    // to the loop bounds cannot turn a wrong count into a panic.
    build().send()
}

/// Said out loud, because a silent retry makes a slow call look like a hang —
/// and on the CLI this is the only sign that anything is happening at all.
fn announce(what: &str, reason: &str, attempt: u32, wait: Duration) {
    eprintln!(
        "  {what}: {reason} — retrying in {:.0}s (attempt {attempt} of {ATTEMPTS})",
        wait.as_secs_f64()
    );
}

/// A failure that never reached the provider's opinion.
///
/// `is_connect` covers a refused or unreachable host, `is_timeout` a request
/// that outlived its deadline. Notably absent: body and decode errors. A
/// connection dropped *mid-download* surfaces on `.bytes()`, not on `.send()`,
/// so it is out of this helper's reach — the caller would have to re-issue the
/// whole request, which is a larger change than this one.
fn transient(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout()
}

/// 429 and 5xx only. A 4xx is the provider saying no, and asking again more
/// politely does not change the answer.
fn worth_retrying(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// The delta-seconds form of `Retry-After`, which is what providers actually
/// send. The HTTP-date form is legal and would need a date parser; a provider
/// that used it simply falls back to the backoff rather than earning a
/// dependency.
fn retry_after(response: &Response) -> Option<Duration> {
    let seconds: u64 = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds).min(MAX_BACKOFF))
}

fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timed out".to_string()
    } else {
        "could not connect".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, serve};

    fn client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    /// The failure the whole file exists for: one transient 502 in the middle of
    /// a poll used to abandon a render that was already paid for.
    #[test]
    fn a_transient_server_error_is_retried_and_then_succeeds() {
        let server = serve(vec![
            Reply::status(502, r#"{"error":"bad gateway"}"#),
            Reply::json(r#"{"done":true}"#),
        ]);
        let url = server.url().to_string();
        let http = client();

        let response =
            send_with_backoff("polling", Duration::from_millis(1), || http.get(&url)).unwrap();

        assert!(response.status().is_success());
        assert_eq!(server.finish().len(), 2, "the first attempt must have happened");
    }

    /// A 4xx is the provider's considered answer. Retrying it wastes the user's
    /// time to arrive at the same refusal, and hides the real message behind a
    /// delay.
    #[test]
    fn a_client_error_is_returned_at_once() {
        let server = serve(vec![Reply::status(401, r#"{"error":"bad key"}"#)]);
        let url = server.url().to_string();
        let http = client();

        let response =
            send_with_backoff("checking", Duration::from_millis(1), || http.get(&url)).unwrap();

        assert_eq!(response.status(), 401);
        assert_eq!(server.finish().len(), 1, "a 401 must not be asked twice");
    }

    /// Exhausting the attempts hands back the last response rather than an error
    /// of its own, so the caller's existing status handling still produces the
    /// provider's message.
    #[test]
    fn a_persistent_failure_gives_the_caller_the_last_response() {
        let server = serve(vec![
            Reply::status(503, "{}"),
            Reply::status(503, "{}"),
            Reply::status(503, "{}"),
        ]);
        let url = server.url().to_string();
        let http = client();

        let response =
            send_with_backoff("polling", Duration::from_millis(1), || http.get(&url)).unwrap();

        assert_eq!(response.status(), 503);
        assert_eq!(server.finish().len(), ATTEMPTS as usize);
    }

    /// A host that is not listening is a connection error rather than a status,
    /// and it must still be retried — that is the laptop-sleep case.
    #[test]
    fn a_refused_connection_is_retried_before_giving_up() {
        let http = client();
        let error = send_with_backoff("polling", Duration::from_millis(1), || {
            http.get("http://127.0.0.1:1/nothing")
        })
        .unwrap_err();
        assert!(error.is_connect(), "{error}");
    }

    #[test]
    fn a_retry_after_header_overrides_the_backoff() {
        let server = serve(vec![
            Reply::status(429, "{}").with_header("Retry-After", "0"),
            Reply::json("{}"),
        ]);
        let url = server.url().to_string();
        let http = client();

        // A ten-second backoff that the header must override, or this test takes
        // ten seconds to pass.
        let response =
            send_with_backoff("polling", Duration::from_secs(10), || http.get(&url)).unwrap();

        assert!(response.status().is_success());
        assert_eq!(server.finish().len(), 2);
    }

    #[test]
    fn an_absurd_retry_after_is_capped() {
        let server = serve(vec![Reply::status(429, "{}").with_header("Retry-After", "86400")]);
        let http = client();
        let response = http.get(server.url()).send().unwrap();
        assert_eq!(retry_after(&response), Some(MAX_BACKOFF));
        server.finish();
    }
}
