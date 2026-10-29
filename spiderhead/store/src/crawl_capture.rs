//! Bounded, redacted transport observations for native body capture.

use crate::error::StoreError;
use flate2::{Decompress, FlushDecompress, Status};
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::OnceLock;
use std::time::Instant;

/// Sorted so the output order is stable.
pub const ALLOW: &[&str] = &[
    "accept",
    "accept-encoding",
    "accept-language",
    "age",
    "cache-control",
    "content-encoding",
    "content-language",
    "content-length",
    "content-range",
    "content-security-policy",
    "content-type",
    "date",
    "etag",
    "expires",
    "host",
    "if-modified-since",
    "if-none-match",
    "last-modified",
    "link",
    "location",
    "permissions-policy",
    "pragma",
    "range",
    "referrer-policy",
    "strict-transport-security",
    "user-agent",
    "vary",
    "x-content-type-options",
    "x-frame-options",
    "x-robots-tag",
];

/// Sorted so the output order is stable.
pub const SENSITIVE: &[&str] = &[
    "authorization",
    "cookie",
    "proxy-authorization",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
];

pub const REDACTED_NAMES_HEADER: &str = "x-seohead-redacted-headers";

pub const TRANSPORT_PHASES: &[&str] = &["dns", "connect", "tls", "ttfb", "download"];

/// Measure only trace events emitted by the request's own transport.
#[derive(Debug, Default)]
pub struct TransportTrace {
    started: BTreeMap<String, f64>,
    seconds: BTreeMap<String, f64>,
    observed: BTreeSet<String>,
    reasons: BTreeMap<String, String>,
    request_started: Option<f64>,
}

/// Seconds on a process-local monotonic clock.
fn monotonic_seconds() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

impl TransportTrace {
    pub fn new() -> Self {
        Self {
            seconds: TRANSPORT_PHASES
                .iter()
                .map(|name| ((*name).to_string(), 0.0))
                .collect(),
            ..Self::default()
        }
    }

    /// Record one trace event at the process monotonic clock.
    pub fn call(&mut self, event_name: &str, _info: &IndexMap<String, Value>) {
        self.observe(event_name, monotonic_seconds());
    }

    /// Record one trace event observed at `now` seconds.
    ///
    /// Event names are `<event>.<state>`; only the known transport events are measured.
    pub fn observe(&mut self, event_name: &str, now: f64) {
        let Some((event, state)) = event_name.rsplit_once('.') else {
            return;
        };
        if !matches!(state, "started" | "complete" | "failed") {
            return;
        }
        if matches!(
            event,
            "http11.send_request_headers" | "http2.send_request_headers"
        ) {
            if state == "started" {
                self.request_started = Some(now);
            }
            return;
        }
        let phase = match event {
            "connection.connect_tcp" => Some("connect"),
            "connection.start_tls" => Some("tls"),
            "http11.receive_response_body" | "http2.receive_response_body" => Some("download"),
            _ => None,
        };
        if let Some(phase) = phase {
            match state {
                "started" => {
                    self.started.insert(phase.to_string(), now);
                }
                "complete" => {
                    if let Some(start) = self.started.remove(phase) {
                        self.add_seconds(phase, now - start);
                    }
                }
                _ => {
                    if let Some(start) = self.started.remove(phase) {
                        self.add_seconds(phase, now - start);
                        self.reasons
                            .insert(phase.to_string(), "transport phase failed".to_string());
                    }
                }
            }
            return;
        }
        if matches!(
            event,
            "http11.receive_response_headers" | "http2.receive_response_headers"
        ) {
            if state == "complete" {
                if let Some(start) = self.request_started.take() {
                    self.add_seconds("ttfb", now - start);
                }
            } else if state == "failed" {
                self.reasons.insert(
                    "ttfb".to_string(),
                    "response headers were not received".to_string(),
                );
            }
        }
    }

    /// Add a non-negative duration and mark the phase observed; negative values clamp to zero.
    fn add_seconds(&mut self, phase: &str, elapsed: f64) {
        *self.seconds.entry(phase.to_string()).or_insert(0.0) += elapsed.max(0.0);
        self.observed.insert(phase.to_string());
    }

    /// Return explicit measurements and reasons; trace internals never enter the scan.
    pub fn snapshot(&self) -> IndexMap<String, IndexMap<String, Value>> {
        let mut result = IndexMap::new();
        for name in TRANSPORT_PHASES {
            let entry = if *name == "dns" {
                phase_entry(
                    Value::Null,
                    "unavailable",
                    "HTTPX trace exposes no separate DNS timing event",
                )
            } else if self.observed.contains(*name) {
                let seconds = self.seconds.get(*name).copied().unwrap_or(0.0);
                let reason = match self.reasons.get(*name) {
                    Some(reason) => reason.clone(),
                    None if *name == "connect" => {
                        "DNS timing is not separate and may be included here".to_string()
                    }
                    None => String::new(),
                };
                let state = if self.reasons.contains_key(*name) {
                    "partial"
                } else {
                    "measured"
                };
                phase_entry(rounded_seconds(seconds), state, &reason)
            } else {
                phase_entry(
                    Value::Null,
                    "unavailable",
                    "transport trace event was not observed",
                )
            };
            result.insert((*name).to_string(), entry);
        }
        result
    }
}

fn phase_entry(seconds: Value, state: &str, reason: &str) -> IndexMap<String, Value> {
    let mut entry = IndexMap::new();
    entry.insert("seconds".to_string(), seconds);
    entry.insert("state".to_string(), Value::String(state.to_string()));
    entry.insert("reason".to_string(), Value::String(reason.to_string()));
    entry
}

/// Round seconds to six decimals, computed through the decimal text of the value.
fn rounded_seconds(seconds: f64) -> Value {
    let rounded: f64 = format!("{seconds:.6}").parse().unwrap_or(seconds);
    serde_json::Number::from_f64(rounded)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// Name unmeasured phases for injected, cached or unsupported transports.
pub fn unavailable_transport_phases(reason: &str) -> IndexMap<String, IndexMap<String, Value>> {
    TRANSPORT_PHASES
        .iter()
        .map(|name| {
            (
                (*name).to_string(),
                phase_entry(Value::Null, "unavailable", reason),
            )
        })
        .collect()
}

/// Text form of a header value.
fn header_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// Headers as `(lowercased name, text value)` pairs, in source order.
///
/// Accepts an array of `[name, value]` pairs (order preserved) or a JSON object.
pub fn header_pairs(headers: &Value) -> Vec<(String, String)> {
    match headers {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Array(pair) if pair.len() == 2 => {
                    Some((header_text(&pair[0]).to_lowercase(), header_text(&pair[1])))
                }
                _ => None,
            })
            .collect(),
        Value::Object(map) => map
            .iter()
            .map(|(name, value)| (name.to_lowercase(), header_text(value)))
            .collect(),
        _ => Vec::new(),
    }
}

/// Keep the allow-listed headers, and name -- never quote -- the sensitive ones.
///
/// Dropping a credential or a cookie outright left two responses indistinguishable
/// in the artifact: the one whose body was omitted as ``credentialed`` and the one
/// that was retained recorded byte-identical header lists, so an operator reading
/// the scan could not tell why a body was missing (#647). The marker carries names
/// only, under a name of our own so it can never be mistaken for a header the site
/// sent or read back as one; no value ever reaches the artifact.
pub fn redact_headers(headers: &Value) -> Vec<(String, String)> {
    let pairs = header_pairs(headers);
    let mut kept: Vec<(String, String)> = pairs
        .iter()
        .filter(|(name, _)| ALLOW.contains(&name.as_str()))
        .cloned()
        .collect();
    let redacted: BTreeSet<&str> = pairs
        .iter()
        .filter(|(name, value)| SENSITIVE.contains(&name.as_str()) && !value.is_empty())
        .map(|(name, _)| name.as_str())
        .collect();
    if !redacted.is_empty() {
        let names: Vec<&str> = redacted.into_iter().collect();
        kept.push((REDACTED_NAMES_HEADER.to_string(), names.join(",")));
    }
    kept
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureEvent {
    pub method: String,
    pub requested_url: String,
    pub effective_url: String,
    pub redirect_history: Vec<IndexMap<String, Value>>,
    pub requested_at: String,
    pub received_at: String,
    pub status_code: Option<i64>,
    pub request_headers: Vec<(String, String)>,
    pub credentials_used: bool,
    pub response_headers: Vec<(String, String)>,
    pub content_type: String,
    pub content_encoding: String,
    pub entity_bytes: Option<Vec<u8>>,
    pub body_fidelity: String,
    pub body_state: String,
    pub body_reason: String,
    pub error: String,
    pub error_kind: String,
    pub effective_status_code: Option<i64>,
    pub effective_headers: Vec<(String, String)>,
    pub response_time: Option<f64>,
    pub session_changed: bool,
    pub http_version: Option<String>,
    pub timing_state: String,
    pub transport_phases: Option<IndexMap<String, IndexMap<String, Value>>>,
}

/// The response entity is truncated or exceeds its declared capture limit.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct EntityLimitError {
    pub message: String,
}

/// The declared HTTP content coding cannot be decoded as an entity.
/// Belongs to the entity limit error family.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct EntityDecodeError {
    pub message: String,
}

fn limit_error(message: &str) -> StoreError {
    StoreError::EntityLimitError(EntityLimitError {
        message: message.to_string(),
    })
}

fn decode_error(message: &str) -> StoreError {
    StoreError::EntityDecodeError(EntityDecodeError {
        message: message.to_string(),
    })
}

/// Return a capped content-decoded entity without allocating a compression bomb.
pub fn bounded_entity(
    raw: &[u8],
    content_encoding: &str,
    limit: i64,
) -> Result<Vec<u8>, StoreError> {
    if limit < 0 {
        return Err(StoreError::Invalid(
            "entity capture requires bytes and a nonnegative limit".to_string(),
        ));
    }
    decode_chunks(std::iter::once(raw), content_encoding, limit)
}

/// Decode raw HTTP chunks incrementally under a finite decoded-byte cap.
pub fn bounded_entity_chunks(
    chunks: &[Vec<u8>],
    content_encoding: &str,
    limit: i64,
) -> Result<Vec<u8>, StoreError> {
    if limit < 0 {
        return Err(StoreError::Invalid(
            "entity capture limit must be nonnegative".to_string(),
        ));
    }
    decode_chunks(chunks.iter().map(Vec::as_slice), content_encoding, limit)
}

/// Shared decoder behind both public entry points; `limit` is already known to be >= 0.
fn decode_chunks<'a, I>(
    chunks: I,
    content_encoding: &str,
    limit: i64,
) -> Result<Vec<u8>, StoreError>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let limit = usize::try_from(limit)
        .map_err(|_| StoreError::Invalid("entity capture limit must be nonnegative".to_string()))?;
    let encoding = content_encoding.trim().to_ascii_lowercase();
    match encoding.as_str() {
        "" | "identity" => {
            let mut out = Vec::new();
            for chunk in chunks {
                if chunk.len() > limit - out.len().min(limit) {
                    return Err(limit_error("entity exceeds capture limit"));
                }
                out.extend_from_slice(chunk);
            }
            Ok(out)
        }
        "zstd" => decode_zstd(chunks, limit),
        "gzip" | "deflate" => decode_zlib_family(chunks, &encoding, limit),
        _ => Err(decode_error("unsupported content encoding")),
    }
}

/// Decode gzip (multi-member) or deflate (zlib-wrapped or raw) under the decoded cap.
///
/// Streams input in chunks: the encoded total is capped per chunk, output
/// is capped as it is produced, and trailing data after a deflate stream is an error.
fn decode_zlib_family<'a, I>(chunks: I, encoding: &str, limit: usize) -> Result<Vec<u8>, StoreError>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let encoded_cap = limit.saturating_add(64 * 1024);
    let mut out = Vec::new();
    let mut prefix: Vec<u8> = Vec::new();
    let mut decoder: Option<Decompress> = None;
    let mut ended = false;
    let mut encoded_bytes = 0usize;
    let mut scratch = vec![0u8; 64 * 1024];
    for chunk in chunks {
        encoded_bytes += chunk.len();
        if encoded_bytes > encoded_cap {
            return Err(limit_error("encoded response exceeds capture limit"));
        }
        let mut pending: Vec<u8> = if decoder.is_none() {
            prefix.extend_from_slice(chunk);
            if prefix.len() < 2 {
                continue;
            }
            let zlib_header_valid = prefix[0] & 15 == 8
                && (u32::from(prefix[0]) * 256 + u32::from(prefix[1])) % 31 == 0;
            decoder = Some(match encoding {
                "gzip" => Decompress::new_gzip(15),
                _ => Decompress::new(zlib_header_valid),
            });
            std::mem::take(&mut prefix)
        } else {
            chunk.to_vec()
        };
        while !pending.is_empty() {
            if ended {
                if encoding != "gzip" {
                    return Err(decode_error("compressed entity contains trailing data"));
                }
                // gzip permits concatenated members; they share the aggregate caps.
                decoder = Some(Decompress::new_gzip(15));
                ended = false;
            }
            let dec = decoder.as_mut().ok_or_else(|| {
                StoreError::Invalid("decoder must exist once input is pending".to_string())
            })?;
            let (stream_ended, consumed) =
                inflate_some(dec, &pending, &mut out, &mut scratch, limit)?;
            if stream_ended {
                ended = true;
                pending = pending.split_off(consumed);
            } else {
                pending.clear();
            }
        }
    }
    if decoder.is_none() || !ended {
        return Err(limit_error("compressed entity is truncated"));
    }
    Ok(out)
}

/// Feed `input` to `dec`, appending output to `out`.
///
/// Returns whether the stream ended and how many input bytes were consumed up to its
/// end. Output beyond `limit` is a limit error, checked as each block is produced.
fn inflate_some(
    dec: &mut Decompress,
    input: &[u8],
    out: &mut Vec<u8>,
    scratch: &mut [u8],
    limit: usize,
) -> Result<(bool, usize), StoreError> {
    let mut consumed = 0usize;
    loop {
        let before_in = dec.total_in();
        let before_out = dec.total_out();
        let status = dec
            .decompress(&input[consumed..], &mut *scratch, FlushDecompress::None)
            .map_err(|_| decode_error("compressed entity is invalid"))?;
        let used = usize::try_from(dec.total_in() - before_in).unwrap_or(0);
        let produced = usize::try_from(dec.total_out() - before_out).unwrap_or(0);
        consumed += used;
        out.extend_from_slice(&scratch[..produced]);
        if out.len() > limit {
            return Err(limit_error("compressed entity exceeds capture limit"));
        }
        if status == Status::StreamEnd {
            return Ok((true, consumed));
        }
        let input_done = consumed >= input.len();
        if input_done && produced < scratch.len() {
            return Ok((false, consumed));
        }
        if used == 0 && produced == 0 {
            return Ok((false, consumed));
        }
    }
}

/// Decode zstd under the decoded cap, refusing a first frame whose window exceeds the
/// bound `max(21, (limit - 1).bit_length())`.
fn decode_zstd<'a, I>(chunks: I, limit: usize) -> Result<Vec<u8>, StoreError>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let encoded_cap = limit.saturating_add(64 * 1024);
    let mut encoded = Vec::new();
    for chunk in chunks {
        if chunk.len() > encoded_cap - encoded.len().min(encoded_cap) {
            return Err(limit_error("encoded response exceeds capture limit"));
        }
        encoded.extend_from_slice(chunk);
    }
    let window_log_max = std::cmp::max(21, usize::BITS - limit.saturating_sub(1).leading_zeros());
    if let Some(window_log) = first_frame_window_log(&encoded) {
        if window_log > window_log_max {
            return Err(decode_error("compressed entity is invalid"));
        }
    }
    let mut reader = ruzstd::decoding::StreamingDecoder::new(encoded.as_slice())
        .map_err(|_| decode_error("compressed entity is invalid"))?;
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| decode_error("compressed entity is invalid"))?;
        if read == 0 {
            break;
        }
        if read > limit - out.len().min(limit) {
            return Err(limit_error("compressed entity exceeds capture limit"));
        }
        out.extend_from_slice(&buffer[..read]);
    }
    Ok(out)
}

/// `windowLog` of the first zstd frame, read from its header; `None` for other input.
fn first_frame_window_log(data: &[u8]) -> Option<u32> {
    if data.get(..4)? != [0x28, 0xb5, 0x2f, 0xfd] {
        return None;
    }
    let descriptor = *data.get(4)?;
    let single_segment = descriptor & 0x20 != 0;
    if single_segment {
        return None;
    }
    let window_byte = *data.get(5)?;
    Some(10 + u32::from(window_byte >> 3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).expect("write");
        encoder.finish().expect("finish")
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).expect("write");
        encoder.finish().expect("finish")
    }

    fn raw_deflate(data: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).expect("write");
        encoder.finish().expect("finish")
    }

    fn limit_kind(result: Result<Vec<u8>, StoreError>) -> &'static str {
        match result {
            Ok(_) => "ok",
            Err(StoreError::EntityLimitError(_)) => "limit",
            Err(StoreError::EntityDecodeError(_)) => "decode",
            Err(_) => "other",
        }
    }

    #[test]
    fn bounded_entity_decodes_compressed_input() {
        let cases = [("gzip", gzip(b"hello")), ("deflate", zlib(b"hello"))];
        for (encoding, encoded) in cases {
            assert_eq!(
                bounded_entity(&encoded, encoding, 5).expect("decode"),
                b"hello"
            );
            assert_eq!(limit_kind(bounded_entity(&encoded, encoding, 4)), "limit");
        }
    }

    #[test]
    fn bounded_entity_supports_raw_deflate_and_rejects_unknown_coding() {
        let raw = raw_deflate(b"raw deflate");
        assert_eq!(
            bounded_entity(&raw, "deflate", 100).expect("raw"),
            b"raw deflate"
        );
        let err = bounded_entity(b"encoded elsewhere", "br", 100).expect_err("unknown");
        assert!(err.to_string().contains("unsupported content encoding"));
    }

    #[test]
    fn zstd_rejects_frame_window_above_bounded_limit() {
        let frame: &[u8] = b"\x28\xb5\x2f\xfd\x00\x60\x01\x00\x00";
        let err = bounded_entity(frame, "zstd", 1 << 16).expect_err("window");
        assert!(err.to_string().contains("compressed entity is invalid"));
    }

    // Single-segment frame with one raw block; the fixture is hand-built because the
    // workspace carries no zstd encoder.
    #[test]
    fn zstd_decodes_raw_block_under_limit() {
        let payload = b"bounded zstd";
        let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x20, payload.len() as u8];
        let block_header = ((payload.len() as u32) << 3) | 1;
        frame.extend_from_slice(&block_header.to_le_bytes()[..3]);
        frame.extend_from_slice(payload);
        assert_eq!(
            bounded_entity(&frame, "zstd", payload.len() as i64).expect("zstd"),
            payload
        );
        assert_eq!(
            limit_kind(bounded_entity(&frame, "zstd", payload.len() as i64 - 1)),
            "limit"
        );
    }

    #[test]
    fn concatenated_gzip_members_fit_exact_limit() {
        let mut payload = gzip(b"first ");
        payload.extend(gzip(b"second"));
        assert_eq!(
            bounded_entity(&payload, "gzip", 12).expect("members"),
            b"first second"
        );
    }

    #[test]
    fn chunked_gzip_matches_single_buffer() {
        let encoded = gzip(b"chunked entity bytes");
        let chunks: Vec<Vec<u8>> = encoded.chunks(3).map(<[u8]>::to_vec).collect();
        assert_eq!(
            bounded_entity_chunks(&chunks, "gzip", 20).expect("chunks"),
            b"chunked entity bytes"
        );
    }

    #[test]
    fn transport_trace_measures_only_observed_phases() {
        let mut trace = TransportTrace::new();
        trace.observe("connection.connect_tcp.started", 1.0);
        trace.observe("connection.connect_tcp.complete", 1.01);
        trace.observe("connection.start_tls.started", 1.02);
        trace.observe("connection.start_tls.complete", 1.05);
        trace.observe("http11.send_request_headers.started", 1.06);
        trace.observe("http11.send_request_headers.complete", 1.08);
        trace.observe("http11.receive_response_headers.complete", 1.10);
        trace.observe("http11.receive_response_body.started", 1.15);
        trace.observe("http11.receive_response_body.complete", 1.20);
        let phases = trace.snapshot();
        assert_eq!(phases["dns"]["state"], "unavailable");
        assert!(phases["dns"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("separate DNS"));
        assert_eq!(phases["connect"]["seconds"], 0.01);
        assert_eq!(phases["tls"]["seconds"], 0.03);
        assert_eq!(phases["ttfb"]["seconds"], 0.04);
        assert_eq!(phases["download"]["seconds"], 0.05);
    }

    #[test]
    fn redact_headers_names_but_never_quotes_sensitive_values() {
        let headers = serde_json::json!([
            ["Content-Type", "text/html"],
            ["Set-Cookie", "secret"],
            ["Cookie", "sid=secret"],
            ["X-Unlisted", "value"]
        ]);
        let kept = redact_headers(&headers);
        assert_eq!(
            kept,
            vec![
                ("content-type".to_string(), "text/html".to_string()),
                (
                    REDACTED_NAMES_HEADER.to_string(),
                    "cookie,set-cookie".to_string()
                ),
            ]
        );
        assert!(!kept.iter().any(|(_, value)| value.contains("secret")));
    }

    #[test]
    fn unavailable_phases_name_every_phase() {
        let phases = unavailable_transport_phases("injected transport");
        assert_eq!(phases.len(), TRANSPORT_PHASES.len());
        assert_eq!(phases["tls"]["reason"], "injected transport");
        assert_eq!(phases["tls"]["seconds"], Value::Null);
    }
}
