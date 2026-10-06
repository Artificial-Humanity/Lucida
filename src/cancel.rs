//! Cooperative cancellation for work that outlives the request that asked for it.
//!
//! MCP clients send `notifications/cancelled` when a user hits escape, or when a
//! request has already timed out on their side. Until this module existed that
//! notification was a no-op — worse, it was *unreachable*, because `serve` drops
//! every message without an `id` and a notification has none by definition. A
//! cancelled render kept running and kept billing, and the client had no way to
//! stop it short of killing the server.
//!
//! # Why a thread-local rather than a parameter
//!
//! The obvious design threads a token through `ImageProvider::generate` and down
//! into each provider's poll loop. That means changing the trait every provider
//! implements, and the trait is the abstraction the whole product rests on — a
//! signature change there is paid for by five implementors, the CLI, and every
//! future provider, to serve one caller. The CLI never cancels; it has a user
//! with a Ctrl-C.
//!
//! So the token is ambient to the thread doing the work. The MCP worker installs
//! one for the duration of a tool call, and the poll loops ask [`check`] between
//! sleeps. Nothing else in the program has to know cancellation exists.
//!
//! # What it cannot do
//!
//! Cancellation is cooperative and lands only where there is a loop to check it
//! in: ComfyUI's queue poll, BFL's render poll, Veo's operation poll. A single
//! blocking POST — Gemini, Stability, OpenAI, all of which render inside one
//! request — runs to completion, and pretending otherwise would mean abandoning a
//! response for an image that has already been billed. Cancelling those is
//! honestly out of reach without dropping the connection mid-render, which buys
//! nothing: the charge is incurred when the provider starts work, not when we
//! read the reply.

use anyhow::{Result, bail};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A one-way flag: once set, it stays set.
#[derive(Clone, Default)]
pub struct Token(Arc<AtomicBool>);

impl Token {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the work to stop. Safe to call more than once, and from any thread —
    /// which is the point, since the caller is the stdin reader and the work is
    /// on a worker.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Token>> = const { RefCell::new(None) };
}

/// Runs `work` with `token` as this thread's cancellation token.
///
/// Restores the previous token on the way out rather than clearing it, so this
/// nests. Nothing nests today; a version that clobbered would be a trap for the
/// first thing that did.
///
/// The restore happens in a drop guard, so it runs when `work` unwinds as well
/// as when it returns. It used to be a plain statement after `work()`, which
/// was safe only because the MCP worker catches panics *inside* this call: had
/// the catch ever moved outside it, a panicking render would have left its
/// token installed on a pooled thread, and a cancelled one would have cancelled
/// the next request that thread picked up before it had done anything.
pub fn with<T>(token: Token, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<Token>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CURRENT.with(|current| *current.borrow_mut() = previous);
        }
    }

    let _restore = Restore(CURRENT.with(|current| current.replace(Some(token))));
    work()
}

/// Whether the work on this thread has been cancelled. False on any thread that
/// never installed a token, which is every CLI thread.
pub fn cancelled() -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(Token::is_cancelled)
    })
}

/// Stops the work if it has been cancelled.
///
/// Its message cannot name a render, because it does not know one. So once a
/// submit has succeeded, a poll loop must not return this error bare: the render
/// exists and is paid for, and abandoning it without reporting its id or
/// polling URL loses something already billed. Such a loop adds the id to the
/// error on the way out, the way its deadline error already does.
pub fn check() -> Result<()> {
    if cancelled() {
        bail!(
            "cancelled at the client's request. If a render had already been \
             submitted it may still complete at the provider, and will still be \
             billed."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_with_no_token_is_never_cancelled() {
        assert!(!cancelled());
        assert!(check().is_ok());
    }

    #[test]
    fn a_token_cancels_the_work_holding_it() {
        let token = Token::new();
        let handle = token.clone();

        with(token, || {
            assert!(check().is_ok());
            handle.cancel();
            assert!(cancelled());
            let error = check().unwrap_err().to_string();
            assert!(error.contains("billed"), "must warn about the charge: {error}");
        });
    }

    /// The flag has to be settable from a thread that is not the one doing the
    /// work — the reader sets it, a worker reads it.
    #[test]
    fn cancellation_crosses_threads() {
        let token = Token::new();
        let handle = token.clone();

        let worker = std::thread::spawn(move || {
            with(token, || {
                for _ in 0..1000 {
                    if check().is_err() {
                        return true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                false
            })
        });

        std::thread::sleep(std::time::Duration::from_millis(20));
        handle.cancel();
        assert!(worker.join().unwrap(), "the worker never saw the cancellation");
    }

    /// The same, when the work unwinds rather than returns. Caught outside
    /// `with` deliberately: that is the arrangement the drop guard exists for,
    /// and the one where a plain restore after `work()` never ran.
    #[test]
    fn the_token_does_not_outlive_work_that_panicked() {
        let token = Token::new();
        token.cancel();
        let unwound = std::panic::catch_unwind(|| {
            with(token, || {
                assert!(cancelled());
                panic!("deliberate");
            })
        });
        assert!(unwound.is_err());
        assert!(!cancelled(), "a panicking request left its token on the thread");
    }

    /// Leaving `with` must not leave the token behind for whatever the thread
    /// does next — on a pooled worker, that is the following request.
    #[test]
    fn the_token_does_not_outlive_the_work_it_was_installed_for() {
        let token = Token::new();
        token.cancel();
        with(token, || assert!(cancelled()));
        assert!(!cancelled(), "a cancelled token leaked past its request");
    }
}
