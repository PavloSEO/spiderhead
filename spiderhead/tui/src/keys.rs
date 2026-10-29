//! Minimal POSIX raw-mode key reader for the interactive shell.
//!
//! Deliberately dependency-free: a short
//! `poll(2)` window disambiguates a lone Escape from an escape sequence, and
//! `poll` has no `FD_SETSIZE` ceiling.

#[cfg(unix)]
use std::os::unix::io::RawFd;
#[cfg(unix)]
use std::time::Instant;

use crate::error::{TuiError, TuiResult};

/// File descriptor type for the interactive input handle.
#[cfg(not(unix))]
#[allow(dead_code)]
pub type RawFd = i32;

/// Symbolic names returned by [`read_key`]; printable input comes back as
/// `"char:<c>"` so callers never distinguish length-1 strings.
#[cfg(unix)]
fn special(byte: u8) -> Option<&'static str> {
    Some(match byte {
        b'\r' | b'\n' => "enter",
        0x7f | 0x08 => "backspace",
        0x03 => "ctrl_c",
        0x04 => "ctrl_d",
        0x18 => "ctrl_x",
        b'\t' => "tab",
        _ => return None,
    })
}

/// Escape-sequence tails → symbolic keys.
#[cfg(unix)]
fn sequence(tail: &str) -> Option<&'static str> {
    Some(match tail {
        "[A" | "OA" => "up",
        "[B" | "OB" => "down",
        "[C" => "right",
        "[D" => "left",
        "[H" | "OH" => "home",
        "[F" | "OF" => "end",
        "[5~" => "page_up",
        "[6~" => "page_down",
        "[3~" => "delete",
        "[200~" => "paste_start",
        "[201~" => "paste_end",
        _ => return None,
    })
}

/// Whether `tail` could still extend into a known sequence.
#[cfg(unix)]
fn sequence_prefix(tail: &str) -> bool {
    [
        "[A", "[B", "[C", "[D", "[H", "[F", "[5~", "[6~", "[3~", "OA", "OB", "OH", "OF", "[200~",
        "[201~",
    ]
    .iter()
    .any(|seq| seq.starts_with(tail))
}

/// How long to wait for the rest of an escape sequence before deciding the
/// key was a lone Escape.
#[cfg(unix)]
pub const ESCAPE_WINDOW_SECONDS: f64 = 0.05;

/// `_ready` — wait for input on `fd` without `select`'s FD_SETSIZE ceiling.
#[cfg(unix)]
fn ready(fd: RawFd, timeout_seconds: f64) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = (timeout_seconds * 1000.0).ceil() as libc::c_int;
    let result = unsafe { libc::poll(&mut pfd, 1, millis.max(0)) };
    result > 0 && (pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0
}

/// Saved termios state restored by [`RawMode::restore`].
#[cfg(unix)]
pub struct RawMode {
    fd: RawFd,
    previous: libc::termios,
    done: bool,
}

#[cfg(unix)]
impl RawMode {
    /// `keys.raw_mode(fd)` — cbreak-raw with `OPOST|ONLCR` kept on output.
    pub fn enter(fd: RawFd) -> TuiResult<Self> {
        let mut previous = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut previous) } != 0 {
            return Err(TuiError::Io(std::io::Error::last_os_error().to_string()));
        }
        let mut raw = previous;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(TuiError::Io(std::io::Error::last_os_error().to_string()));
        }
        let mut attributes = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut attributes) } != 0 {
            return Err(TuiError::Io(std::io::Error::last_os_error().to_string()));
        }
        attributes.c_oflag |= (libc::OPOST | libc::ONLCR) as libc::tcflag_t;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &attributes) } != 0 {
            return Err(TuiError::Io(std::io::Error::last_os_error().to_string()));
        }
        Ok(RawMode {
            fd,
            previous,
            done: false,
        })
    }

    /// Restore the saved attributes (`TCSADRAIN`).
    pub fn restore(&mut self) {
        if !self.done {
            self.done = true;
            unsafe { libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.previous) };
        }
    }
}

#[cfg(unix)]
impl Drop for RawMode {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Read one byte; `Ok(None)` on EOF.
#[cfg(unix)]
fn read_byte(fd: RawFd) -> Option<u8> {
    let mut buf = [0u8; 1];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 1) };
    if n == 1 {
        Some(buf[0])
    } else {
        None
    }
}

/// `_read_available` — drain bytes already queued within `budget` seconds.
#[cfg(unix)]
fn read_available(fd: RawFd, budget: f64) -> String {
    let mut chunk = String::new();
    let deadline = Instant::now() + std::time::Duration::from_secs_f64(budget);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if !ready(fd, left.as_secs_f64()) {
            return chunk;
        }
        let Some(byte) = read_byte(fd) else {
            return chunk;
        };
        // The escape tail is ASCII; a replacement char keeps parity with
        // Lossy decode: invalid bytes become U+FFFD.
        chunk.push(if byte < 0x80 {
            byte as char
        } else {
            '\u{fffd}'
        });
        if sequence(&chunk).is_some() {
            return chunk;
        }
        if chunk.starts_with('[')
            && chunk.chars().count() > 1
            && matches!(chunk.chars().last(), Some(c) if ('@'..='~').contains(&c))
        {
            return chunk;
        }
        if chunk.chars().count() >= 64 || (!chunk.starts_with('[') && !sequence_prefix(&chunk)) {
            return chunk;
        }
    }
}

/// `keys.read_key` — one keypress, or `"timeout"` for the observer refresh.
#[cfg(unix)]
pub fn read_key(fd: RawFd, timeout: Option<f64>) -> TuiResult<String> {
    if let Some(t) = timeout {
        if !ready(fd, t) {
            return Ok("timeout".into());
        }
    }
    let Some(first_byte) = read_byte(fd) else {
        return Ok("ctrl_d".into());
    };
    if first_byte != 0x1b {
        if let Some(name) = special(first_byte) {
            return Ok(name.into());
        }
        let width = if first_byte < 0x80 {
            1
        } else if first_byte < 0xE0 {
            2
        } else if first_byte < 0xF0 {
            3
        } else {
            4
        };
        let mut data = vec![first_byte];
        while data.len() < width {
            let Some(byte) = read_byte(fd) else {
                return Ok("char:\u{fffd}".into());
            };
            data.push(byte);
        }
        let first = String::from_utf8_lossy(&data).into_owned();
        return Ok(format!("char:{first}"));
    }
    let tail = read_available(fd, ESCAPE_WINDOW_SECONDS);
    if tail.is_empty() {
        return Ok("escape".into());
    }
    Ok(sequence(&tail).unwrap_or("unknown").into())
}

/// Saved terminal state placeholder where POSIX raw mode does not exist.
#[cfg(not(unix))]
pub struct RawMode;

#[cfg(not(unix))]
impl RawMode {
    /// Interactive raw mode requires a POSIX terminal; absent elsewhere.
    pub fn enter(_fd: RawFd) -> TuiResult<Self> {
        Err(TuiError::Io(
            "interactive terminal input is not supported on this platform".into(),
        ))
    }

    /// Nothing was switched, nothing to restore.
    pub fn restore(&mut self) {}
}

/// `keys.read_key` where the interactive key reader is POSIX-only.
#[cfg(not(unix))]
pub fn read_key(_fd: RawFd, _timeout: Option<f64>) -> TuiResult<String> {
    Err(TuiError::Io(
        "interactive terminal input is not supported on this platform".into(),
    ))
}
