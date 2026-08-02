//! Reading a secret from a terminal while showing asterisks.
//!
//! `rpassword` shows nothing at all, which is the safest thing and also
//! unsettling: pasting a key produces no response whatsoever, so there is no
//! signal that the paste registered, that the terminal has focus, or that the
//! program is even waiting. An asterisk per character answers all three without
//! revealing the value.
//!
//! Doing that means reading byte by byte, which means turning off canonical
//! mode, which means owning the terminal's state — and the hazard there is
//! specific and nasty: if the process leaves without restoring, the user's shell
//! is left with no echo and no line editing. They will not know what happened
//! and `reset` is the fix nobody remembers.
//!
//! Two things keep that from happening:
//!
//! 1. A [`Restore`] guard puts the original settings back on the way out of the
//!    function, including when unwinding from a panic.
//! 2. `ISIG` is turned off, so Ctrl-C does **not** raise a signal that would kill
//!    the process past the guard. It arrives as byte `0x03` and is handled here,
//!    restoring first and then exiting. That removes the need for a signal
//!    handler, which is the part that usually goes wrong.
//!
//! Non-Unix falls back to `rpassword`, which shows nothing but is correct.

use anyhow::{Context, Result};

/// Reads a line without echoing it, printing `*` per character.
///
/// Returns the value with no trailing newline. Backspace deletes, Ctrl-C aborts
/// the process, Ctrl-D ends the input.
#[cfg(unix)]
pub fn read_masked() -> Result<String> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();

    // SAFETY: `fd` is a valid descriptor for the process's own standard input,
    // and `termios` is fully initialised by `tcgetattr` before it is read.
    let original = unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut termios) != 0 {
            // Not a terminal we can configure — the caller should not have got
            // here, but failing soft beats failing obscurely.
            return rpassword::read_password().context("reading the value");
        }
        termios
    };

    let _restore = Restore { fd, original };

    // SAFETY: same descriptor, and `raw` is a copy of a struct we just filled.
    unsafe {
        let mut raw = original;
        // ECHO off so the value never appears; ICANON off so bytes arrive as
        // typed rather than a line at a time; ISIG off so Ctrl-C reaches us as
        // data instead of killing the process before the guard can run.
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
        if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
            return rpassword::read_password().context("reading the value");
        }
    }

    let mut value = String::new();
    let mut byte = [0u8; 1];
    let mut stderr = std::io::stderr();

    loop {
        if stdin.lock().read(&mut byte).context("reading the value")? == 0 {
            break; // EOF
        }
        match byte[0] {
            b'\n' | b'\r' => break,
            // Ctrl-C. Restore before leaving, which the guard does as this scope
            // ends, then exit the way an interrupted program should.
            0x03 => {
                drop(_restore);
                let _ = writeln!(stderr, "^C");
                std::process::exit(130);
            }
            0x04 => break, // Ctrl-D
            // Backspace and delete. Erase one asterisk by moving back, painting a
            // space, and moving back again — the terminal has no undo.
            0x08 | 0x7f => {
                if value.pop().is_some() {
                    let _ = write!(stderr, "\u{8} \u{8}");
                    let _ = stderr.flush();
                }
            }
            // Ignore other control characters rather than showing an asterisk for
            // something that contributed nothing to the value.
            c if c < 0x20 => {}
            c => {
                value.push(c as char);
                let _ = write!(stderr, "*");
                let _ = stderr.flush();
            }
        }
    }

    let _ = writeln!(stderr);
    Ok(value)
}

#[cfg(not(unix))]
pub fn read_masked() -> Result<String> {
    // No asterisks here: getting this right on Windows needs a different API
    // entirely, and showing nothing is the safe failure rather than the pretty
    // one.
    rpassword::read_password().context("reading the value")
}

/// Puts the terminal back exactly as it was found.
///
/// A guard rather than a call at the end of the function, so it also runs when
/// unwinding from a panic. Without it a crash mid-read leaves the user with a
/// shell that neither echoes nor line-edits.
#[cfg(unix)]
struct Restore {
    fd: std::os::fd::RawFd,
    original: libc::termios,
}

#[cfg(unix)]
impl Drop for Restore {
    fn drop(&mut self) {
        // SAFETY: the descriptor and the saved settings both came from
        // `tcgetattr` on this same terminal moments earlier.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}
