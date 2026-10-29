//! Opening log files as text lines: gzip is recognised by content, newlines are
//! universal, and undecodable bytes become U+FFFD.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use flate2::bufread::MultiGzDecoder;

use crate::error::{LogsError, Result};

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Open `path` for line reading. Compressed logs are detected from the two magic bytes,
/// never from the file name, since rotated archives are often renamed.
pub fn open_log(path: &Path) -> std::io::Result<Box<dyn BufRead + Send>> {
    let mut probe = [0u8; 2];
    let mut head = File::open(path)?;
    let got = head.read(&mut probe)?;
    let file = BufReader::new(File::open(path)?);
    if got == 2 && probe == GZIP_MAGIC {
        Ok(Box::new(BufReader::new(MultiGzDecoder::new(file))))
    } else {
        Ok(Box::new(file))
    }
}

/// Line iterator over any byte source.
///
/// Every item keeps its terminator normalised to `\n` (a final unterminated line has none).
/// With a character cap, a line longer than the cap — terminator included — is an error
/// raised before the rest of the line is buffered.
pub struct Lines {
    inner: Box<dyn BufRead + Send>,
    max_chars: Option<usize>,
    failed: bool,
}

impl Lines {
    /// Lines of `inner` with no length cap.
    pub fn new(inner: Box<dyn BufRead + Send>) -> Self {
        Self {
            inner,
            max_chars: None,
            failed: false,
        }
    }

    /// Lines of `inner`, rejecting any line longer than `max_chars` characters.
    pub fn capped(inner: Box<dyn BufRead + Send>, max_chars: usize) -> Self {
        Self {
            inner,
            max_chars: Some(max_chars),
            failed: false,
        }
    }

    fn read_line(&mut self) -> Result<Option<String>> {
        let mut bytes: Vec<u8> = Vec::new();
        let mut terminated = false;
        loop {
            let buf = self.inner.fill_buf()?;
            if buf.is_empty() {
                break;
            }
            match buf.iter().position(|b| *b == b'\n' || *b == b'\r') {
                Some(i) => {
                    let cr = buf[i] == b'\r';
                    bytes.extend_from_slice(&buf[..i]);
                    self.inner.consume(i + 1);
                    if cr {
                        let next = self.inner.fill_buf()?;
                        if next.first() == Some(&b'\n') {
                            self.inner.consume(1);
                        }
                    }
                    terminated = true;
                    break;
                }
                None => {
                    let n = buf.len();
                    bytes.extend_from_slice(buf);
                    self.inner.consume(n);
                }
            }
            if let Some(cap) = self.max_chars {
                // Four bytes per character is the most any scalar needs.
                if bytes.len() > 4 * (cap + 1) {
                    return Err(LogsError::value("log line exceeds the bounded line length"));
                }
            }
        }
        if bytes.is_empty() && !terminated {
            return Ok(None);
        }
        let mut line = String::from_utf8_lossy(&bytes).into_owned();
        if terminated {
            line.push('\n');
        }
        if let Some(cap) = self.max_chars {
            if line.chars().count() > cap {
                return Err(LogsError::value("log line exceeds the bounded line length"));
            }
        }
        Ok(Some(line))
    }
}

impl Iterator for Lines {
    type Item = Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.read_line() {
            Ok(Some(line)) => Some(Ok(line)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn lines(data: &[u8]) -> Vec<String> {
        Lines::new(Box::new(Cursor::new(data.to_vec())))
            .map(|l| l.unwrap())
            .collect()
    }

    #[test]
    fn newlines_are_universal() {
        assert_eq!(lines(b"a\r\nb\rc\nd"), vec!["a\n", "b\n", "c\n", "d"]);
        assert_eq!(lines(b""), Vec::<String>::new());
        assert_eq!(lines(b"\n\n"), vec!["\n", "\n"]);
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        assert_eq!(lines(b"a\xffb\n"), vec!["a\u{fffd}b\n"]);
    }

    #[test]
    fn cap_counts_the_terminator() {
        let mut ok = Lines::capped(Box::new(Cursor::new(b"abcd\n".to_vec())), 5);
        assert_eq!(ok.next().unwrap().unwrap(), "abcd\n");
        let mut bad = Lines::capped(Box::new(Cursor::new(b"abcde\n".to_vec())), 5);
        assert!(bad.next().unwrap().is_err());
        let mut bare = Lines::capped(Box::new(Cursor::new(b"abcde".to_vec())), 5);
        assert_eq!(bare.next().unwrap().unwrap(), "abcde");
    }
}
