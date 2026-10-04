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
/// the process, Ctrl-D ends the input. Escape sequences (arrow keys and the
/// like) are discarded whole rather than typed into the value.
#[cfg(unix)]
pub fn read_masked() -> Result<String> {
    use std::io::Write;
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
        // Block for exactly one byte per `read`. Turning ICANON off makes the
        // terminal fall back on VMIN/VTIME, and a terminal left with VMIN=0
        // answers the first `read` with 0 bytes — which looks like EOF and
        // returns an empty key without the user having typed anything.
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
            return rpassword::read_password().context("reading the value");
        }
    }

    // Bytes rather than chars, validated once at the end: UTF-8 arrives here
    // one byte at a time, and pushing `byte as char` was a Latin-1
    // reinterpretation that turned a pasted `é` into `Ã©`.
    let mut input = MaskedInput::default();
    let mut byte = [0u8; 1];
    let mut stderr = std::io::stderr();

    loop {
        // A real escape sequence arrives in one burst, so if the next byte of a
        // pending one has not turned up within a moment, the Esc was a key
        // press of its own. Abandon the sequence rather than swallowing
        // whatever the user types next.
        if input.escape_pending() && !byte_ready(fd, ESCAPE_TIMEOUT_MS).context("reading the value")? {
            input.abandon_escape();
            continue;
        }
        if read_byte(fd, &mut byte).context("reading the value")? == 0 {
            break; // EOF
        }
        match input.feed(byte[0]) {
            Key::Done => break,
            // Ctrl-C. Restore before leaving, which the guard does as this scope
            // ends, then exit the way an interrupted program should.
            Key::Interrupt => {
                drop(_restore);
                let _ = writeln!(stderr, "^C");
                std::process::exit(130);
            }
            // Erase one asterisk by moving back, painting a space, and moving
            // back again — the terminal has no undo.
            Key::Erased => {
                let _ = write!(stderr, "\u{8} \u{8}");
                let _ = stderr.flush();
            }
            Key::Added => {
                let _ = write!(stderr, "*");
                let _ = stderr.flush();
            }
            Key::Ignored => {}
        }
    }

    let _ = writeln!(stderr);
    String::from_utf8(input.value).context("the value was not valid UTF-8")
}

/// How long to wait for the rest of an escape sequence before deciding the Esc
/// was typed on its own. Terminal libraries use the same order of magnitude:
/// long enough for a slow link to deliver a sequence's bytes, short enough that
/// nobody notices the wait.
#[cfg(unix)]
const ESCAPE_TIMEOUT_MS: libc::c_int = 50;

/// Waits up to `timeout_ms` for `fd` to have a byte to read.
///
/// Reading goes through `read_byte` on the raw descriptor rather than
/// `std::io::stdin()`: that is a `BufReader`, which swallows a whole burst on
/// the first one-byte read, after which `poll` on the descriptor reports
/// nothing pending while the rest of the sequence sits in the buffer.
#[cfg(unix)]
fn byte_ready(fd: std::os::fd::RawFd, timeout_ms: libc::c_int) -> std::io::Result<bool> {
    let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    loop {
        // SAFETY: `poll` is a valid, initialised `pollfd` and the count is 1.
        let ready = unsafe { libc::poll(&mut poll, 1, timeout_ms) };
        if ready >= 0 {
            return Ok(ready > 0);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Reads one byte straight from `fd`, returning 0 at end of input.
#[cfg(unix)]
fn read_byte(fd: std::os::fd::RawFd, byte: &mut [u8; 1]) -> std::io::Result<usize> {
    loop {
        // SAFETY: `byte` is a valid one-byte buffer and the length passed is 1.
        let n = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// What one input byte did, so the caller can draw it without knowing the rules.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum Key {
    /// Enter or Ctrl-D: the value is complete.
    Done,
    /// Ctrl-C: the caller restores the terminal and exits.
    Interrupt,
    /// A character was removed: paint over its asterisk.
    Erased,
    /// A new character joined the value: print one asterisk.
    Added,
    /// The byte changed nothing visible, or belongs to an escape sequence.
    Ignored,
}

/// Where the parser is inside a terminal escape sequence.
#[cfg(unix)]
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
enum Escape {
    #[default]
    None,
    /// Saw ESC; the next byte says what kind of sequence this is.
    Started,
    /// Inside `ESC [ ...`, waiting for the final byte.
    Csi,
    /// Saw `ESC O`, which carries exactly one more byte.
    Ss3,
}

/// The secret typed so far, plus enough state to tell a keystroke from the
/// bytes a key press sends.
///
/// An arrow key is not one byte: Up is `ESC [ A`. Dropping only the ESC kept
/// the `[A`, and the secret silently gained two characters nobody typed — a
/// key that then fails authentication with no hint why. The whole sequence has
/// to go, and since bytes arrive one at a time that takes state, so this is a
/// pure byte-in, event-out machine that the tests can drive without a terminal.
#[cfg(unix)]
#[derive(Debug, Default)]
struct MaskedInput {
    value: Vec<u8>,
    escape: Escape,
}

#[cfg(unix)]
impl MaskedInput {
    /// Whether an escape sequence is part-way through, so the next byte is
    /// expected to belong to it.
    fn escape_pending(&self) -> bool {
        self.escape != Escape::None
    }

    /// Gives up on a pending escape sequence because nothing followed the Esc
    /// in time. Without this a lone Esc press would swallow the next key, and
    /// a `[` after it would swallow digits until a letter closed the "sequence".
    fn abandon_escape(&mut self) {
        self.escape = Escape::None;
    }

    fn feed(&mut self, byte: u8) -> Key {
        match self.escape {
            Escape::None => {}
            // A control byte inside a sequence is not part of it: a user who
            // pressed Esc and then Enter or Ctrl-C still means Enter or Ctrl-C,
            // so abandon the sequence and handle the byte as usual.
            Escape::Started | Escape::Csi if byte < 0x20 => self.escape = Escape::None,
            Escape::Started => {
                self.escape = match byte {
                    b'[' => Escape::Csi,
                    b'O' => Escape::Ss3,
                    // Any other byte completes a two-byte sequence (Alt+key).
                    _ => Escape::None,
                };
                return Key::Ignored;
            }
            // Parameter bytes 0x30-0x3F and intermediate bytes 0x20-0x2F keep
            // the sequence going; a final byte 0x40-0x7E ends it. Anything
            // above that is not valid in a CSI, so it ends it too and is then
            // treated as ordinary input.
            Escape::Csi if (0x20..=0x3F).contains(&byte) => return Key::Ignored,
            Escape::Csi if (0x40..=0x7E).contains(&byte) => {
                self.escape = Escape::None;
                return Key::Ignored;
            }
            Escape::Csi => self.escape = Escape::None,
            // Application-mode arrows are `ESC O A`: the byte after the O is
            // the key, and would otherwise be typed into the secret.
            Escape::Ss3 => {
                self.escape = Escape::None;
                if byte >= 0x20 {
                    return Key::Ignored;
                }
            }
        }

        match byte {
            b'\n' | b'\r' | 0x04 => Key::Done, // Enter, Ctrl-D
            0x03 => Key::Interrupt,
            0x1b => {
                self.escape = Escape::Started;
                Key::Ignored
            }
            // Backspace and delete.
            0x08 | 0x7f => {
                if pop_last_char(&mut self.value) {
                    Key::Erased
                } else {
                    Key::Ignored
                }
            }
            // Ignore other control characters rather than showing an asterisk for
            // something that contributed nothing to the value.
            c if c < 0x20 => Key::Ignored,
            c => {
                self.value.push(c);
                // One asterisk per character, not per byte: a continuation byte
                // belongs to a character whose asterisk is already printed, and
                // printing another desynchronises the display from backspace.
                if is_continuation(c) {
                    Key::Ignored
                } else {
                    Key::Added
                }
            }
        }
    }
}

/// A UTF-8 continuation byte, `10xxxxxx`.
#[cfg(unix)]
fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

/// Removes the last whole character — its continuation bytes, then their lead —
/// and reports whether there was one to remove, mirroring `Vec::pop`.
#[cfg(unix)]
fn pop_last_char(bytes: &mut Vec<u8>) -> bool {
    let removed = !bytes.is_empty();
    while let Some(byte) = bytes.pop() {
        if !is_continuation(byte) {
            break;
        }
    }
    removed
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A backspace must undo one *character*: popping one byte from a pasted
    /// `é` (two bytes) would leave half a code point in an API-key file.
    #[test]
    fn backspace_removes_a_whole_character() {
        let mut bytes = "aé€".as_bytes().to_vec(); // 1 + 2 + 3 bytes
        assert!(pop_last_char(&mut bytes));
        assert_eq!(bytes, "aé".as_bytes());
        assert!(pop_last_char(&mut bytes));
        assert_eq!(bytes, b"a");
        assert!(pop_last_char(&mut bytes));
        assert!(bytes.is_empty());
        assert!(!pop_last_char(&mut bytes), "an empty value has nothing to erase");
    }

    /// One asterisk per character keeps the display and backspace in step —
    /// two asterisks for `é` with one erased per backspace drifted apart.
    #[test]
    fn asterisks_are_counted_per_character_not_per_byte() {
        let printed = "aé€".bytes().filter(|b| !is_continuation(*b)).count();
        assert_eq!(printed, "aé€".chars().count());
    }

    /// Feeds `bytes` and returns the value typed, as the prompt would see it.
    fn typed(bytes: &[u8]) -> Vec<u8> {
        let mut input = MaskedInput::default();
        for &b in bytes {
            input.feed(b);
        }
        input.value
    }

    /// The bug: Up is `ESC [ A`, and keeping the `[A` put two characters into a
    /// secret the user never typed.
    #[test]
    fn an_arrow_key_adds_nothing_to_the_value() {
        for arrow in [b"\x1b[A", b"\x1b[B", b"\x1b[C", b"\x1b[D"] {
            let mut bytes = b"ab".to_vec();
            bytes.extend_from_slice(arrow);
            bytes.extend_from_slice(b"cd");
            assert_eq!(typed(&bytes), b"abcd", "arrow {arrow:?}");
        }
    }

    /// Parameter and intermediate bytes belong to the sequence: Delete is
    /// `ESC [ 3 ~` and Shift-Up is `ESC [ 1 ; 2 A`, neither of which may leak.
    #[test]
    fn a_csi_sequence_with_parameters_is_discarded_whole() {
        assert_eq!(typed(b"a\x1b[3~b"), b"ab");
        assert_eq!(typed(b"a\x1b[1;2Ab"), b"ab");
        assert_eq!(typed(b"a\x1b[?25 qb"), b"ab", "intermediate byte 0x20");
    }

    /// A two-byte ESC sequence (Alt+x) and the three-byte application-mode
    /// arrow (`ESC O A`) take their trailing bytes with them.
    #[test]
    fn short_escape_sequences_are_discarded_whole() {
        assert_eq!(typed(b"a\x1bxb"), b"ab");
        assert_eq!(typed(b"a\x1bOAb"), b"ab");
    }

    /// An escape sequence must not swallow the keys that end or abort the
    /// prompt, nor desynchronise the byte after it.
    #[test]
    fn control_bytes_still_act_inside_an_escape_sequence() {
        let mut input = MaskedInput::default();
        input.feed(0x1b);
        assert_eq!(input.feed(b'\r'), Key::Done);
        let mut input = MaskedInput::default();
        input.feed(0x1b);
        input.feed(b'[');
        assert_eq!(input.feed(0x03), Key::Interrupt);
        // A malformed CSI ends at the first byte that cannot be part of it, and
        // that byte is then handled as ordinary input. Here it is DEL (0x7f),
        // which is not valid inside a CSI, so it acts as a backspace and erases
        // the `a`; `b` follows. Hence `b`, not `ab`.
        assert_eq!(typed(b"a\x1b[\x7fb"), b"b");
    }

    /// A second Esc restarts the sequence rather than being swallowed by the
    /// first, so `ESC ESC [ A` is still one arrow key.
    #[test]
    fn a_repeated_escape_restarts_the_sequence() {
        assert_eq!(typed(b"a\x1b\x1b[Ab"), b"ab");
    }

    /// A lone Esc (nothing follows within the timeout) must not corrupt what is
    /// typed next: `read_masked` calls `abandon_escape` when the poll expires.
    #[test]
    fn a_lone_escape_is_abandoned_on_timeout() {
        let mut input = MaskedInput::default();
        input.feed(0x1b);
        assert!(input.escape_pending());
        input.abandon_escape();
        assert!(!input.escape_pending());
        for &b in b"abc" {
            input.feed(b);
        }
        assert_eq!(input.value, b"abc");

        // Esc then `[` and then silence: the digits typed afterwards are the
        // user's, not the parameters of a sequence that never finished.
        let mut input = MaskedInput::default();
        input.feed(0x1b);
        input.feed(b'[');
        assert!(input.escape_pending());
        input.abandon_escape();
        for &b in b"12" {
            input.feed(b);
        }
        assert_eq!(input.value, b"12");
    }

    /// The two descriptor helpers `read_masked` uses for the timeout, driven
    /// over a pipe: nothing pending times out, a pending byte is seen and read.
    #[test]
    fn byte_ready_and_read_byte_follow_the_descriptor() {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` has room for the two descriptors `pipe` writes.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (rd, wr) = (fds[0], fds[1]);
        assert!(!byte_ready(rd, 10).unwrap(), "nothing written yet");
        // SAFETY: `wr` is the open write end and the buffer is one byte.
        assert_eq!(unsafe { libc::write(wr, b"x".as_ptr().cast(), 1) }, 1);
        assert!(byte_ready(rd, 10).unwrap());
        let mut byte = [0u8; 1];
        assert_eq!(read_byte(rd, &mut byte).unwrap(), 1);
        assert_eq!(byte, *b"x");
        // SAFETY: closing the write end makes the read end report end of input.
        unsafe { libc::close(wr) };
        assert_eq!(read_byte(rd, &mut byte).unwrap(), 0);
        // SAFETY: `rd` is still open and owned by this test.
        unsafe { libc::close(rd) };
    }

    /// Backspace, Enter, Ctrl-D, Ctrl-C and ordinary typing keep their meaning.
    #[test]
    fn plain_keys_behave_as_before() {
        let mut input = MaskedInput::default();
        assert_eq!(input.feed(b'a'), Key::Added);
        assert_eq!(input.feed(0xC3), Key::Added);
        assert_eq!(input.feed(0xA9), Key::Ignored, "continuation byte");
        assert_eq!(input.feed(0x7f), Key::Erased);
        assert_eq!(input.value, b"a");
        assert_eq!(input.feed(0x08), Key::Erased);
        assert_eq!(input.feed(0x08), Key::Ignored, "nothing left to erase");
        assert_eq!(input.feed(0x01), Key::Ignored, "other control byte");
        assert_eq!(input.feed(b'\n'), Key::Done);
        assert_eq!(input.feed(0x04), Key::Done);
        assert_eq!(input.feed(0x03), Key::Interrupt);
    }
}
