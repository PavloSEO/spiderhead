//! Bounded standard-library codecs and readers for retained scan bodies.
//!
//!
//! Line counting, the `scan_decoder.v1` entity decoder (charset rules in
//! `bodies/charsets.rs`), the zlib/identity body codec, the bounded body reader and, in
//! `bodies/documents.rs`, the verified document readers.

use crate::error::StoreError;
mod charset_data;
mod charsets;
mod html_charset;
pub use html_charset::{decode_html, DecodedHtml};

pub(crate) use charsets::py_isspace;

use flate2::{Decompress, FlushDecompress, Status};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Regex source with Unicode matching.
pub const SHA256: &str = r"[0-9a-f]{64}\Z";

/// Decoder provenance tag written next to every decoded entity.
pub const DECODER_VERSION: &str = "scan_decoder.v1";

/// Regex source with Unicode matching.
pub const LINE_BOUNDARIES: &str = r"\r\n|[\n\r\v\f\x1c-\x1e\x85  ]";

/// Characters that terminate a `splitlines()` record.
const LINE_END_CHARS: &[char] = &[
    '\n', '\r', '\u{0b}', '\u{0c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}', '\u{2029}',
];

/// Count `splitlines()` records without materializing a list of substrings.
pub fn line_count(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let mut boundaries = 0i64;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
            boundaries += 1;
        } else if LINE_END_CHARS.contains(&c) {
            boundaries += 1;
        }
    }
    let last = text.chars().next_back().unwrap_or('\0');
    let trailing = i64::from(!LINE_END_CHARS.contains(&last));
    boundaries + trailing
}

/// Decode entity bytes under the explicit `scan_decoder.v1` policy.
///
/// The charset is the `charset=` parameter of `content_type`, resolved by MIME and codec
/// lookup rules (see [`charsets`]). A missing, unknown or
/// non-text label falls back to UTF-8. Decoding replaces malformed sequences.
pub fn decode_entity(
    raw: &[u8],
    content_type: &str,
) -> Result<(String, Map<String, Value>), StoreError> {
    let requested = charsets::content_charset(content_type).filter(|label| !label.is_empty());
    if requested
        .as_deref()
        .is_some_and(|label| label.contains('\0'))
    {
        // An embedded NUL in the label is rejected before any codec lookup.
        return Err(StoreError::Invalid(
            "embedded null character in charset".into(),
        ));
    }
    let resolved = requested.as_deref().and_then(charsets::lookup);
    let (charset, source) = match (&requested, resolved) {
        (Some(_), Some(name)) => (name, "content_type_charset"),
        _ => ("utf-8", "utf8_fallback"),
    };
    let text = charsets::decode(charset, raw)?;
    let mut meta = Map::new();
    meta.insert(
        "decoder_version".into(),
        Value::String(DECODER_VERSION.into()),
    );
    meta.insert("decoder_source".into(), Value::String(source.into()));
    meta.insert("decoder_charset".into(), Value::String(charset.into()));
    meta.insert("decoder_errors".into(), Value::String("replace".into()));
    Ok((text, meta))
}

/// Encoded body: content-decoded bytes stored as identity or smaller zlib-6 data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedBody {
    /// Lower-case hex SHA-256 of the exact decoded bytes.
    pub sha256: String,
    /// `"identity"` or `"zlib"`.
    pub codec: &'static str,
    /// Length of the decoded bytes.
    pub decoded_bytes: i64,
    /// Length of `data`.
    pub stored_bytes: i64,
    /// The stored bytes.
    pub data: Vec<u8>,
}

/// Encode exact content-decoded bytes using identity or smaller zlib-6 data.
pub fn encode_body(raw: &[u8]) -> Result<EncodedBody, StoreError> {
    let compressed = {
        let mut out = Vec::new();
        let mut enc = flate2::write::ZlibEncoder::new(&mut out, flate2::Compression::new(6));
        std::io::Write::write_all(&mut enc, raw)?;
        enc.finish()?;
        out
    };
    let (codec, data) = if compressed.len() < raw.len() {
        ("zlib", compressed)
    } else {
        ("identity", raw.to_vec())
    };
    Ok(EncodedBody {
        sha256: hex_sha256(raw),
        codec,
        decoded_bytes: raw.len() as i64,
        stored_bytes: data.len() as i64,
        data,
    })
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_lower_hex_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Read and verify one body without allowing a compressed expansion bomb.
pub fn read_body(
    con: &Connection,
    sha256: &str,
    max_decoded_bytes: i64,
) -> Result<Vec<u8>, StoreError> {
    let invalid = |msg: &str| StoreError::Invalid(msg.to_owned());
    if !is_lower_hex_sha256(sha256) {
        return Err(invalid("body SHA-256 must be lowercase hex"));
    }
    if max_decoded_bytes < 0 {
        return Err(invalid(
            "body decoded-byte limit must be a nonnegative integer",
        ));
    }
    let row: Option<(String, String, i64, i64, i64)> = con
        .query_row(
            "SELECT sha256, codec, decoded_bytes, stored_bytes, length(data) FROM bodies WHERE sha256=?1",
            params![sha256],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((stored_sha, codec, decoded_bytes, stored_bytes, actual_stored_bytes)) = row else {
        return Err(invalid("body unavailable: not_in_corpus"));
    };
    if stored_sha != sha256
        || decoded_bytes < 0
        || stored_bytes < 0
        || stored_bytes != actual_stored_bytes
    {
        return Err(invalid("body metadata or stored length is invalid"));
    }
    if decoded_bytes > max_decoded_bytes {
        return Err(invalid("body unavailable: decoded byte limit exceeded"));
    }
    if stored_bytes > decoded_bytes {
        return Err(invalid(
            "body stored size exceeds its decoded-byte declaration",
        ));
    }
    let data: Vec<u8> = con.query_row(
        "SELECT data FROM bodies WHERE sha256=?1",
        params![sha256],
        |r| r.get(0),
    )?;
    let raw = match codec.as_str() {
        "identity" => {
            if data.len() as i64 != decoded_bytes {
                return Err(invalid("identity body decoded length disagrees"));
            }
            data
        }
        "zlib" => inflate_bounded(&data, max_decoded_bytes, decoded_bytes)?,
        _ => return Err(invalid("body codec is unsupported")),
    };
    if hex_sha256(&raw) != sha256 {
        return Err(invalid("body SHA-256 disagrees with decoded bytes"));
    }
    Ok(raw)
}

/// Inflate a zlib stream while refusing to produce more than `limit` bytes.
fn inflate_bounded(data: &[u8], limit: i64, expected: i64) -> Result<Vec<u8>, StoreError> {
    let limit = limit as usize;
    let mut decoder = Decompress::new(true);
    let mut out = Vec::with_capacity(expected.max(0) as usize);
    let status = decoder
        .decompress_vec(data, &mut out, FlushDecompress::Finish)
        .map_err(|_| StoreError::Invalid("compressed body has an invalid zlib stream".into()))?;
    if out.len() > limit {
        return Err(StoreError::Invalid(
            "compressed body exceeds decoded byte limit".into(),
        ));
    }
    if status != Status::StreamEnd
        || decoder.total_in() as usize != data.len()
        || out.len() as i64 != expected
    {
        return Err(StoreError::Invalid(
            "compressed body is truncated, trailing, or length-mismatched".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod doc_tests;
mod documents;

pub use documents::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn body_db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE bodies(sha256 TEXT PRIMARY KEY, codec TEXT NOT NULL, \
             decoded_bytes INTEGER NOT NULL, stored_bytes INTEGER NOT NULL, data BLOB NOT NULL)",
        )
        .unwrap();
        con
    }

    fn store(con: &Connection, raw: &[u8]) -> EncodedBody {
        let e = encode_body(raw).unwrap();
        con.execute(
            "INSERT INTO bodies VALUES(?1,?2,?3,?4,?5)",
            params![e.sha256, e.codec, e.decoded_bytes, e.stored_bytes, e.data],
        )
        .unwrap();
        e
    }

    #[test]
    fn encode_identity_zlib_and_zero_bytes() {
        assert_eq!(encode_body(b"").unwrap().codec, "identity");
        assert_eq!(encode_body(b"").unwrap().sha256, hex_sha256(b""));
        let raw = b"same bytes ".repeat(200);
        assert_eq!(encode_body(&raw).unwrap().codec, "zlib");
        assert_eq!(encode_body(&raw).unwrap(), encode_body(&raw).unwrap());
    }

    #[test]
    fn read_body_round_trips_and_detects_corruption() {
        let con = body_db();
        let raw = b"abc ".repeat(500);
        let e = store(&con, &raw);
        assert_eq!(read_body(&con, &e.sha256, 1 << 20).unwrap(), raw);
        // Decoded-byte cap is enforced before inflating.
        assert!(read_body(&con, &e.sha256, 10).is_err());
        // Malformed sha is rejected without touching the table.
        assert!(read_body(&con, "ABC", 10).is_err());
        assert!(matches!(
            read_body(&con, &"0".repeat(64), 10),
            Err(StoreError::Invalid(m)) if m.contains("not_in_corpus")
        ));
    }

    #[test]
    fn read_body_rejects_truncated_zlib() {
        let con = body_db();
        let raw = b"truncate me ".repeat(300);
        let e = encode_body(&raw).unwrap();
        assert_eq!(e.codec, "zlib");
        let cut = &e.data[..e.data.len() - 4];
        con.execute(
            "INSERT INTO bodies VALUES(?1,'zlib',?2,?3,?4)",
            params![e.sha256, e.decoded_bytes, cut.len() as i64, cut],
        )
        .unwrap();
        assert!(read_body(&con, &e.sha256, 1 << 20).is_err());
    }

    #[test]
    fn decode_entity_charset_and_fallback() {
        let (text, _) = decode_entity(b"caf\xe9", "text/html; charset=ISO-8859-1").unwrap();
        assert_eq!(text, "café");
        let (text, meta) = decode_entity(b"\xff", "text/html; charset=not-a-codec").unwrap();
        assert_eq!(text, "\u{fffd}");
        assert_eq!(meta["decoder_source"], "utf8_fallback");
        assert_eq!(meta["decoder_charset"], "utf-8");
        assert_eq!(meta["decoder_errors"], "replace");
        assert_eq!(meta["decoder_version"], DECODER_VERSION);
        assert_eq!(
            decode_entity(b"\xff", "application/octet-stream")
                .unwrap()
                .0,
            "\u{fffd}"
        );
    }

    #[test]
    fn line_count_matches_splitlines() {
        for text in [
            "",
            "a",
            "a\n",
            "a\r\nb",
            "a\rb\nc\u{2028}",
            "x\u{85}",
            "\n\n",
            "one\u{0c}two",
        ] {
            assert_eq!(
                line_count(text),
                text.lines_split_like_python().len() as i64,
                "{text:?}"
            );
        }
    }

    /// Reference line split: split after every boundary, drop the trailing empty record.
    trait SplitLikePython {
        fn lines_split_like_python(&self) -> Vec<&str>;
    }
    impl SplitLikePython for str {
        fn lines_split_like_python(&self) -> Vec<&str> {
            let mut out = Vec::new();
            let mut start = 0;
            let mut it = self.char_indices().peekable();
            while let Some((i, c)) = it.next() {
                if c == '\r' && matches!(it.peek(), Some((_, '\n'))) {
                    out.push(&self[start..i]);
                    it.next();
                    start = i + 2;
                } else if LINE_END_CHARS.contains(&c) {
                    out.push(&self[start..i]);
                    start = i + c.len_utf8();
                }
            }
            if start < self.len() {
                out.push(&self[start..]);
            }
            out
        }
    }
}

#[cfg(test)]
mod differential {
    use super::*;

    fn fixture(name: &str) -> Vec<Value> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    // Table of decoder cases. Rows tagged "exact" must match byte for byte;
    // rows tagged "gap" (CJK multibyte, stateful, escape codecs) are decoded best-effort
    // through encoding_rs and only counted.
    #[test]
    fn decode_entity_matches_reference_table() {
        let rows = fixture("decode_entity.json");
        let mut bad = std::collections::BTreeMap::<String, usize>::new();
        let mut gap = (0, 0);
        let mut first = Vec::new();
        for row in &rows {
            let ct = row["content_type"].as_str().unwrap();
            let raw = unhex(row["raw_hex"].as_str().unwrap());
            let got = decode_entity(&raw, ct);
            let ok = match (row.get("error"), &got) {
                (Some(_), Err(_)) => true,
                (None, Ok((text, meta))) => {
                    row["text"].as_str() == Some(text.as_str())
                        && row["meta"].as_object() == Some(meta)
                }
                _ => false,
            };
            if row["tier"] == "gap" {
                gap = (gap.0 + 1, gap.1 + usize::from(ok));
            } else if !ok {
                *bad.entry(ct.to_owned()).or_default() += 1;
                if first.len() < 5 {
                    first.push(format!(
                        "{ct:?} {} -> {got:?}\n   want {row}",
                        row["raw_hex"].as_str().unwrap().get(..24).unwrap_or("")
                    ));
                }
            }
        }
        eprintln!(
            "decode_entity: {} rows, gap tier {}/{} match",
            rows.len(),
            gap.1,
            gap.0
        );
        assert!(
            bad.is_empty(),
            "{} rows, mismatching content types: {bad:?}\n{}",
            rows.len(),
            first.join("\n")
        );
    }

    /// Rebuild a fixture body from its recipe.
    fn build(recipe: &Value) -> Vec<u8> {
        match recipe["kind"].as_str().unwrap() {
            "hex" => unhex(recipe["hex"].as_str().unwrap()),
            "repeat" => unhex(recipe["unit_hex"].as_str().unwrap())
                .repeat(recipe["count"].as_u64().unwrap() as usize),
            kind => {
                let alphabet = recipe
                    .get("alphabet_hex")
                    .map(|h| unhex(h.as_str().unwrap()));
                let mut state = recipe["seed"].as_u64().unwrap();
                (0..recipe["len"].as_u64().unwrap())
                    .map(|_| {
                        state = (state * 1_103_515_245 + 12_345) & 0x7FFF_FFFF;
                        match (&alphabet, kind) {
                            (Some(a), "lcg_alpha") => a[((state >> 16) as usize) % a.len()],
                            _ => ((state >> 16) & 0xFF) as u8,
                        }
                    })
                    .collect()
            }
        }
    }

    // Checks the stored bytes against zlib level 6 output: same codec choice, identical bytes.
    #[test]
    fn encode_body_matches_reference_zlib_bytes() {
        let rows = fixture("encode_body.json");
        assert!(rows.len() >= 200);
        let mut bad = Vec::new();
        for row in &rows {
            let raw = build(&row["recipe"]);
            let got = encode_body(&raw).unwrap();
            let ok = got.sha256 == row["sha256"].as_str().unwrap()
                && got.codec == row["codec"].as_str().unwrap()
                && got.decoded_bytes == row["decoded_bytes"].as_i64().unwrap()
                && got.stored_bytes == row["stored_bytes"].as_i64().unwrap()
                && hex_sha256(&got.data) == row["data_sha256"].as_str().unwrap();
            if !ok {
                bad.push(format!("len {} {}", raw.len(), row["recipe"]["kind"]));
            }
        }
        assert!(
            bad.is_empty(),
            "{} of {} rows differ: {:?}",
            bad.len(),
            rows.len(),
            &bad[..bad.len().min(8)]
        );
    }
}
