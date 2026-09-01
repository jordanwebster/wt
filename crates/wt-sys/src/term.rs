//! Raw-mode terminal handling for `setup`'s inline region (A76, §14.7).
//!
//! The region is drawn on the main screen, never the alternate one, so the
//! answers stay in the scrollback as the record of what was done. Everything
//! that decides *what* to draw is in `wt_core::tui`; this module only puts
//! bytes on the terminal and turns bytes back into keys.

use std::io::{Read as _, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use wt_core::tui::{Key, Viewport};
use wt_core::{CoreError, ExitClass};

use crate::Result;

/// How long a lone escape byte waits for the rest of a sequence before it is
/// read as the Escape key.
const ESCAPE_GRACE: Duration = Duration::from_millis(40);

/// A terminal switched into raw mode for the life of the value.
pub struct Terminal {
    saved: libc::termios,
    input: RawFd,
    output: RawFd,
    pending: Vec<u8>,
    /// Lines the live region currently occupies.
    region: usize,
    restored: bool,
}

impl Terminal {
    /// Switches the terminal into raw mode.
    ///
    /// Signal generation is disabled along with echo and line buffering, so an
    /// interrupt arrives as a key this program handles and leaves through its
    /// ordinary exit — which is what restores the terminal.
    pub fn enter() -> Result<Self> {
        let input = std::io::stdin().as_raw_fd();
        let output = std::io::stdout().as_raw_fd();
        let mut saved = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(input, &mut saved) } != 0 {
            return Err(failed("read the terminal mode"));
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::BRKINT | libc::INPCK | libc::ISTRIP);
        // Output post-processing stays on: the region writes `\r\n` explicitly,
        // and anything else the process prints should still behave.
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(input, libc::TCSANOW, &raw) } != 0 {
            return Err(failed("set the terminal mode"));
        }
        Ok(Self {
            saved,
            input,
            output,
            pending: Vec::new(),
            region: 0,
            restored: false,
        })
    }

    /// The terminal's current size, queried per frame so a resize needs no
    /// signal handler.
    pub fn size(&self) -> (u16, u16) {
        let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
        if unsafe { libc::ioctl(self.output, libc::TIOCGWINSZ, &mut size) } != 0
            || size.ws_row == 0
            || size.ws_col == 0
        {
            return (24, 80);
        }
        (size.ws_row, size.ws_col)
    }

    /// A viewport describing this terminal now.
    pub fn viewport(&self, color: bool, unicode: bool) -> Viewport {
        let (rows, cols) = self.size();
        Viewport {
            rows,
            cols,
            color,
            unicode,
        }
    }

    /// Waits up to `timeout` for a key.
    ///
    /// Returning `None` on expiry is what lets the caller redraw an animation
    /// frame without ever delaying a keystroke: input is always the reason the
    /// wait ends first.
    pub fn read_key(&mut self, timeout: Duration) -> Result<Option<Key>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(key) = self.decode()? {
                return Ok(Some(key));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            // Half a key never completes on its own. Waiting the caller's full
            // timeout for the rest would hold the interface for an hour once
            // the scan stops animating, so an unfinished sequence gets the
            // same short grace a lone escape gets and is then discarded.
            let wait = if self.pending.is_empty() {
                remaining
            } else {
                remaining.min(ESCAPE_GRACE)
            };
            if !self.wait(wait)? {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                let stale = std::mem::take(&mut self.pending);
                if stale == [0x1b] {
                    return Ok(Some(Key::Escape));
                }
                continue;
            }
            // A closed input polls ready forever. There is nobody left to
            // answer, so it ends the session rather than spinning on EOF —
            // including when a truncated sequence is still buffered.
            if self.fill()? == Read::Eof {
                self.pending.clear();
                return Ok(Some(Key::Interrupt));
            }
        }
    }

    /// Blocks until the input is readable or the timeout expires.
    fn wait(&self, timeout: Duration) -> Result<bool> {
        let mut fds = libc::pollfd {
            fd: self.input,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let ready = unsafe { libc::poll(&mut fds, 1, millis) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(failed("wait for input"));
        }
        Ok(ready > 0)
    }

    fn fill(&mut self) -> Result<Read> {
        let mut buffer = [0u8; 256];
        match std::io::stdin().read(&mut buffer) {
            Ok(0) => Ok(Read::Eof),
            Ok(read) => {
                self.pending.extend_from_slice(&buffer[..read]);
                Ok(Read::Bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Ok(Read::Bytes),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(Read::Bytes),
            Err(_) => Err(failed("read from the terminal")),
        }
    }

    /// Takes one key off the pending bytes, if a whole one is there.
    fn decode(&mut self) -> Result<Option<Key>> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let (key, consumed) = match decode_key(&self.pending) {
            Decoded::Key(key, consumed) => (key, consumed),
            // Every deadline belongs to `read_key`, which decides how long an
            // unfinished sequence may wait for the rest of itself.
            Decoded::Incomplete => return Ok(None),
            Decoded::Skip(consumed) => {
                self.pending.drain(..consumed);
                return self.decode();
            }
        };
        self.pending.drain(..consumed);
        Ok(Some(key))
    }

    /// Redraws the live region in place, in one write.
    pub fn draw(&mut self, lines: &[String]) -> Result<()> {
        let mut out = String::new();
        if self.region > 0 {
            out.push_str(&format!("\u{1b}[{}A", self.region));
        }
        out.push_str("\u{1b}[?25l");
        for line in lines {
            out.push_str("\r\u{1b}[2K");
            out.push_str(line);
            out.push_str("\r\n");
        }
        let extra = self.region.saturating_sub(lines.len());
        for _ in 0..extra {
            out.push_str("\r\u{1b}[2K\r\n");
        }
        if extra > 0 {
            out.push_str(&format!("\u{1b}[{extra}A"));
        }
        out.push_str("\u{1b}[?25h");
        self.region = lines.len();
        self.write(&out)
    }

    /// Erases the live region, leaving the cursor where it began.
    pub fn clear_region(&mut self) -> Result<()> {
        if self.region == 0 {
            return Ok(());
        }
        let mut out = format!("\u{1b}[{}A", self.region);
        for _ in 0..self.region {
            out.push_str("\r\u{1b}[2K\r\n");
        }
        out.push_str(&format!("\u{1b}[{}A", self.region));
        self.region = 0;
        self.write(&out)
    }

    /// Writes lines that stay in the scrollback, above the live region.
    pub fn commit(&mut self, lines: &[String]) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.clear_region()?;
        let mut out = String::new();
        for line in lines {
            out.push_str("\r\u{1b}[2K");
            out.push_str(line);
            out.push_str("\r\n");
        }
        self.write(&out)
    }

    fn write(&self, text: &str) -> Result<()> {
        let mut stdout = std::io::stdout();
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|_| failed("write to the terminal"))
    }

    /// Restores the terminal mode. Idempotent, and also run from `Drop`.
    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let _ = self.write("\u{1b}[?25h");
        unsafe { libc::tcsetattr(self.input, libc::TCSANOW, &self.saved) };
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Whether a read produced bytes or reached the end of the input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Read {
    Bytes,
    Eof,
}

enum Decoded {
    Key(Key, usize),
    /// More bytes are needed before this is a key.
    Incomplete,
    /// These bytes are not a key wt acts on.
    Skip(usize),
}

/// Turns a byte prefix into one key.
fn decode_key(bytes: &[u8]) -> Decoded {
    let Some(&first) = bytes.first() else {
        return Decoded::Incomplete;
    };
    match first {
        0x03 => Decoded::Key(Key::Interrupt, 1),
        0x04 => Decoded::Key(Key::Escape, 1),
        b'\r' | b'\n' => Decoded::Key(Key::Enter, 1),
        b' ' => Decoded::Key(Key::Space, 1),
        0x7f | 0x08 => Decoded::Key(Key::Backspace, 1),
        0x1b => decode_escape(bytes),
        // Other control bytes are not keys this interface uses.
        byte if byte < 0x20 => Decoded::Skip(1),
        _ => decode_char(bytes),
    }
}

fn decode_escape(bytes: &[u8]) -> Decoded {
    if bytes.len() < 2 {
        return Decoded::Incomplete;
    }
    match bytes[1] {
        b'[' | b'O' => {
            if bytes.len() < 3 {
                return Decoded::Incomplete;
            }
            // CSI parameters run until a byte in the final range; wt only acts
            // on the arrows, so anything else is consumed and ignored.
            let mut end = 2;
            while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                end += 1;
            }
            if end >= bytes.len() {
                return Decoded::Incomplete;
            }
            let consumed = end + 1;
            match bytes[end] {
                b'A' => Decoded::Key(Key::Up, consumed),
                b'B' => Decoded::Key(Key::Down, consumed),
                _ => Decoded::Skip(consumed),
            }
        }
        // Alt-modified keys are not bound; drop the pair.
        _ => Decoded::Skip(2),
    }
}

/// Decodes one UTF-8 scalar, waiting for a partial sequence to arrive.
fn decode_char(bytes: &[u8]) -> Decoded {
    let width = match bytes[0] {
        byte if byte < 0x80 => 1,
        byte if byte & 0xe0 == 0xc0 => 2,
        byte if byte & 0xf0 == 0xe0 => 3,
        byte if byte & 0xf8 == 0xf0 => 4,
        // A stray continuation byte cannot start a character.
        _ => return Decoded::Skip(1),
    };
    if bytes.len() < width {
        return Decoded::Incomplete;
    }
    match std::str::from_utf8(&bytes[..width]) {
        Ok(text) => match text.chars().next() {
            Some(character) => Decoded::Key(Key::Char(character), width),
            None => Decoded::Skip(width),
        },
        Err(_) => Decoded::Skip(width),
    }
}

fn failed(what: &str) -> CoreError {
    CoreError::new(
        ExitClass::Internal,
        "INPUT_FAILED",
        format!("could not {what}"),
        "run `wt setup` in an ordinary terminal, or use `wt register`",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(bytes: &[u8]) -> Option<(Key, usize)> {
        match decode_key(bytes) {
            Decoded::Key(key, consumed) => Some((key, consumed)),
            _ => None,
        }
    }

    #[test]
    fn ordinary_keys_decode_one_byte_at_a_time() {
        assert_eq!(key(b"a"), Some((Key::Char('a'), 1)));
        assert_eq!(key(b" "), Some((Key::Space, 1)));
        assert_eq!(key(b"\r"), Some((Key::Enter, 1)));
        assert_eq!(key(b"\n"), Some((Key::Enter, 1)));
        assert_eq!(key(&[0x7f]), Some((Key::Backspace, 1)));
        assert_eq!(key(&[0x03]), Some((Key::Interrupt, 1)));
    }

    #[test]
    fn arrows_decode_from_their_escape_sequences() {
        assert_eq!(key(b"\x1b[A"), Some((Key::Up, 3)));
        assert_eq!(key(b"\x1b[B"), Some((Key::Down, 3)));
        assert_eq!(
            key(b"\x1bOA"),
            Some((Key::Up, 3)),
            "application cursor mode"
        );
    }

    #[test]
    fn a_parameterised_sequence_is_consumed_whole() {
        // An arrow with modifiers, as an extended-keys terminal sends it.
        assert_eq!(key(b"\x1b[1;5A"), Some((Key::Up, 6)));
        match decode_key(b"\x1b[200~") {
            Decoded::Skip(consumed) => assert_eq!(consumed, 6, "bracketed paste is dropped whole"),
            _ => panic!("a sequence wt does not bind must be consumed, not reinterpreted"),
        }
    }

    #[test]
    fn a_partial_sequence_waits_for_the_rest() {
        assert!(matches!(decode_key(b"\x1b["), Decoded::Incomplete));
        assert!(matches!(decode_key(b"\x1b[1;"), Decoded::Incomplete));
        assert!(matches!(decode_key(b"\x1b"), Decoded::Incomplete));
    }

    #[test]
    fn a_multibyte_character_waits_for_all_its_bytes() {
        let bytes = "é".as_bytes();
        assert!(matches!(decode_key(&bytes[..1]), Decoded::Incomplete));
        assert_eq!(key(bytes), Some((Key::Char('é'), 2)));
        let wide = "日".as_bytes();
        assert_eq!(key(wide), Some((Key::Char('日'), 3)));
    }

    #[test]
    fn a_stray_continuation_byte_is_dropped_not_misread() {
        assert!(matches!(decode_key(&[0x80]), Decoded::Skip(1)));
    }

    #[test]
    fn an_unfinished_sequence_is_never_reinterpreted_as_its_first_byte() {
        // The grace period discards it whole; reading `\x1b[` as Escape then
        // `[` would toggle a row the reader never asked for.
        assert!(matches!(decode_key(b"\x1b["), Decoded::Incomplete));
        assert!(matches!(decode_key("é".as_bytes()), Decoded::Key(..)));
        assert!(matches!(
            decode_key(&"é".as_bytes()[..1]),
            Decoded::Incomplete
        ));
    }

    #[test]
    fn unbound_control_bytes_are_skipped() {
        assert!(matches!(decode_key(&[0x01]), Decoded::Skip(1)));
    }
}
