//! the reference `json.loads` error messages, for flags whose `type=` parses JSON (`_json_list`).
//!
//! Only the failure text is reproduced here; valid documents are parsed by `serde_json`.

struct Scanner<'a> {
    s: &'a [char],
}

type Fail = (String, usize);

impl Scanner<'_> {
    fn ws(&self, mut i: usize) -> usize {
        while i < self.s.len() && matches!(self.s[i], ' ' | '\t' | '\n' | '\r') {
            i += 1;
        }
        i
    }

    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn word(&self, i: usize, w: &str) -> bool {
        w.chars()
            .enumerate()
            .all(|(k, c)| self.at(i + k) == Some(c))
    }

    fn string(&self, quote: usize) -> Result<usize, Fail> {
        let mut i = quote + 1;
        loop {
            match self.at(i) {
                None => return Err(("Unterminated string starting at".into(), quote)),
                Some('"') => return Ok(i + 1),
                Some('\\') => match self.at(i + 1) {
                    None => return Err(("Unterminated string starting at".into(), quote)),
                    Some('u') => {
                        let ok =
                            (2..6).all(|k| self.at(i + k).is_some_and(|c| c.is_ascii_hexdigit()));
                        if !ok {
                            return Err(("Invalid \\uXXXX escape".into(), i + 1));
                        }
                        i += 6;
                    }
                    Some(c) if "\"\\/bfnrt".contains(c) => i += 2,
                    Some(c) => {
                        return Err((
                            format!(
                                "Invalid \\escape: {}",
                                crate::pycompat::repr(&c.to_string())
                            ),
                            i,
                        ))
                    }
                },
                Some(c) if (c as u32) < 0x20 => {
                    return Err(("Invalid control character at".into(), i))
                }
                Some(_) => i += 1,
            }
        }
    }

    fn number(&self, i: usize) -> Option<usize> {
        let mut j = i;
        if self.at(j) == Some('-') {
            j += 1;
        }
        match self.at(j) {
            Some('0') => j += 1,
            Some(c) if c.is_ascii_digit() => {
                while self.at(j).is_some_and(|c| c.is_ascii_digit()) {
                    j += 1;
                }
            }
            _ => return None,
        }
        if self.at(j) == Some('.') && self.at(j + 1).is_some_and(|c| c.is_ascii_digit()) {
            j += 1;
            while self.at(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
        }
        if matches!(self.at(j), Some('e' | 'E')) {
            let mut k = j + 1;
            if matches!(self.at(k), Some('+' | '-')) {
                k += 1;
            }
            if self.at(k).is_some_and(|c| c.is_ascii_digit()) {
                while self.at(k).is_some_and(|c| c.is_ascii_digit()) {
                    k += 1;
                }
                j = k;
            }
        }
        Some(j)
    }

    fn value(&self, i: usize) -> Result<usize, Fail> {
        let stop = || ("Expecting value".to_owned(), i);
        match self.at(i) {
            Some('"') => self.string(i),
            Some('{') => self.object(i),
            Some('[') => self.array(i),
            Some('n') if self.word(i, "null") => Ok(i + 4),
            Some('t') if self.word(i, "true") => Ok(i + 4),
            Some('f') if self.word(i, "false") => Ok(i + 5),
            Some('N') if self.word(i, "NaN") => Ok(i + 3),
            Some('I') if self.word(i, "Infinity") => Ok(i + 8),
            Some('-') if self.word(i, "-Infinity") => Ok(i + 9),
            Some(_) => self.number(i).ok_or_else(stop),
            None => Err(stop()),
        }
    }

    fn object(&self, open: usize) -> Result<usize, Fail> {
        let prop = |i| {
            (
                "Expecting property name enclosed in double quotes".to_owned(),
                i,
            )
        };
        let mut i = self.ws(open + 1);
        if self.at(i) == Some('}') {
            return Ok(i + 1);
        }
        loop {
            if self.at(i) != Some('"') {
                return Err(prop(i));
            }
            i = self.ws(self.string(i)?);
            if self.at(i) != Some(':') {
                return Err(("Expecting ':' delimiter".into(), i));
            }
            i = self.ws(self.value(self.ws(i + 1))?);
            match self.at(i) {
                Some('}') => return Ok(i + 1),
                Some(',') => {
                    let comma = i;
                    i = self.ws(i + 1);
                    if self.at(i) == Some('}') {
                        return Err(("Illegal trailing comma before end of object".into(), comma));
                    }
                }
                _ => return Err(("Expecting ',' delimiter".into(), i)),
            }
        }
    }

    fn array(&self, open: usize) -> Result<usize, Fail> {
        let mut i = self.ws(open + 1);
        if self.at(i) == Some(']') {
            return Ok(i + 1);
        }
        loop {
            i = self.ws(self.value(i)?);
            match self.at(i) {
                Some(']') => return Ok(i + 1),
                Some(',') => {
                    let comma = i;
                    i = self.ws(i + 1);
                    if self.at(i) == Some(']') {
                        return Err(("Illegal trailing comma before end of array".into(), comma));
                    }
                }
                _ => return Err(("Expecting ',' delimiter".into(), i)),
            }
        }
    }
}

/// `None` when the reference `json.loads` accepts `text`, else the `JSONDecodeError` message.
pub fn decode_error(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let sc = Scanner { s: &chars };
    let start = sc.ws(0);
    let (msg, pos) = match sc.value(start) {
        Err(f) => f,
        Ok(end) => {
            let end = sc.ws(end);
            if end == chars.len() {
                return None;
            }
            ("Extra data".to_owned(), end)
        }
    };
    let line = chars[..pos].iter().filter(|&&c| c == '\n').count() + 1;
    let col = match chars[..pos].iter().rposition(|&c| c == '\n') {
        Some(nl) => pos - nl,
        None => pos + 1,
    };
    Some(format!("{msg}: line {line} column {col} (char {pos})"))
}

#[cfg(test)]
mod tests {
    use super::decode_error;

    #[test]
    fn messages_match_python() {
        assert_eq!(
            decode_error("[1,"),
            Some("Expecting value: line 1 column 4 (char 3)".into())
        );
        assert_eq!(
            decode_error("nope"),
            Some("Expecting value: line 1 column 1 (char 0)".into())
        );
        assert_eq!(
            decode_error("[1] x"),
            Some("Extra data: line 1 column 5 (char 4)".into())
        );
        assert_eq!(
            decode_error("[1 2]"),
            Some("Expecting ',' delimiter: line 1 column 4 (char 3)".into())
        );
        assert_eq!(
            decode_error("{a:1}"),
            Some(
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"
                    .into()
            )
        );
        assert_eq!(
            decode_error("[\n1,\n]"),
            Some("Illegal trailing comma before end of array: line 2 column 2 (char 3)".into())
        );
        assert_eq!(decode_error(" [1, {\"a\": \"b\"}] "), None);
    }
}
