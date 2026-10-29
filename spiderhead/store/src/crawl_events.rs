//! Bounded, redacted crawl event records with explicit timestamp and coverage state.

use crate::error::StoreError;
use indexmap::IndexMap;
use serde_json::{Map, Value};
use spiderhead_core::artifacts::CRAWL_TIMELINE_V1;
use spiderhead_core::timeutil::{stamp, Format};

pub const FORMAT: &str = "spiderhead.crawl-event.v1";

pub const MAX_EVENTS: i64 = 10000;

pub const MAX_PAYLOAD_BYTES: i64 = 4096;

/// Sorted so the output order is stable.
pub const EVENT_TYPES: &[&str] = &[
    "budget",
    "cache",
    "checkpoint",
    "circuit",
    "provider_enrichment",
    "queue",
    "render",
    "request",
    "retry",
    "scope_narrowed",
    "stop",
    "throttle",
    "urls_added",
];

pub const PAYLOAD_KEYS: &[(&str, &[&str])] = &[
    (
        "queue",
        &["count", "depth", "queue_ordinal", "reason", "url_id"],
    ),
    (
        "request",
        &["attempt", "method", "queue_ordinal", "state", "url_id"],
    ),
    (
        "retry",
        &["attempt", "delay_ms", "queue_ordinal", "reason", "url_id"],
    ),
    ("cache", &["queue_ordinal", "state", "url_id"]),
    (
        "throttle",
        &["concurrency", "delay_ms", "previous_delay_ms", "state"],
    ),
    ("render", &["representation", "state", "url_id"]),
    ("circuit", &["kind", "state", "streak"]),
    ("budget", &["kind", "limit", "used"]),
    ("checkpoint", &["inflight", "queued", "state"]),
    ("stop", &["reason"]),
    (
        "provider_enrichment",
        &["operation", "provider", "rows", "state"],
    ),
    ("scope_narrowed", &["dropped", "kind", "pattern"]),
    ("urls_added", &["count"]),
];

/// Regex source.
pub const SENSITIVE: &str = "(?:token|secret|password|credential|authorization|cookie)";

/// Sorted so the output order is stable.
pub const IDENTIFIER_SENSITIVE: &[&str] =
    &["filter", "host", "query", "site_url", "target_url", "url"];

// Unset timestamps are modelled as `Option::None`:
// `timestamp(None)` and `EventSink::emit(.., None)` mean "not supplied", so the current time
// is stamped with state `known`; an explicit `Some(Value::Null)` means an unknown time.

/// Case-insensitive match of [`SENSITIVE`] anywhere inside the key.
fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "credential",
        "authorization",
        "cookie",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

fn timestamp_error() -> StoreError {
    StoreError::Invalid("event timestamp must be RFC3339 UTC text or null".to_string())
}

/// Resolve an event timestamp into `(occurred_at, timestamp_state)`.
///
/// `None` means the caller supplied nothing (stamped now, state `known`), `null` means the
/// time is unknown, and text must be an RFC 3339 UTC instant with a zero offset.
pub fn timestamp(value: Option<&Value>) -> Result<(Option<String>, String), StoreError> {
    let Some(value) = value else {
        return Ok((Some(stamp(Format::Zulu)), "known".to_string()));
    };
    match value {
        Value::Null => Ok((None, "unknown".to_string())),
        Value::String(text) if is_rfc3339(text, true) => {
            Ok((Some(text.clone()), "known".to_string()))
        }
        _ => Err(timestamp_error()),
    }
}

/// RFC 3339-style timestamp check: `YYYY-MM-DD[T ]HH:MM[:SS[.f{1,6}]]` plus an offset.
///
/// With `utc_only` the offset must be zero (`Z`, `+00:00`, ...); otherwise any valid
/// `+HH:MM`, `-HH:MM` and `+HHMM` offsets are accepted.
pub(crate) fn is_rfc3339(text: &str, utc_only: bool) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = bytes.get(range)?;
        if part.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return false;
    }
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return false,
    };
    if !(1..=days_in_month).contains(&day) || !matches!(bytes.get(10), Some(b'T' | b' ')) {
        return false;
    }
    let (Some(hour), Some(minute)) = (digits(11..13), digits(14..16)) else {
        return false;
    };
    if bytes.get(13) != Some(&b':') || hour > 23 || minute > 59 {
        return false;
    }
    let mut index = 16;
    if bytes.get(index) == Some(&b':') {
        let Some(second) = digits(index + 1..index + 3) else {
            return false;
        };
        if second > 59 {
            return false;
        }
        index += 3;
        if bytes.get(index) == Some(&b'.') {
            let fraction_start = index + 1;
            let mut end = fraction_start;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if !(1..=6).contains(&(end - fraction_start)) {
                return false;
            }
            index = end;
        }
    }
    let rest = &text[index..];
    if matches!(rest, "Z" | "+00:00" | "-00:00" | "+0000" | "-0000") {
        return true;
    }
    if utc_only || rest.len() != 6 && rest.len() != 5 {
        return false;
    }
    // A multi-byte char here can never be a valid offset; string slicing needs ASCII.
    if !rest.is_ascii() {
        return false;
    }
    let sign_ok = matches!(rest.as_bytes()[0], b'+' | b'-');
    let zone = &rest[1..];
    let (zone_hour, zone_minute) = if rest.len() == 6 && rest.as_bytes()[3] == b':' {
        (&zone[..2], &zone[3..])
    } else if rest.len() == 5 {
        (&zone[..2], &zone[2..])
    } else {
        return false;
    };
    sign_ok
        && zone_hour.bytes().all(|b| b.is_ascii_digit())
        && zone_minute.bytes().all(|b| b.is_ascii_digit())
        && zone_hour.parse::<u32>().is_ok_and(|h| h < 24)
        && zone_minute.parse::<u32>().is_ok_and(|m| m < 60)
}

/// Clean one payload object for its event type: redact sensitive keys, keep scalars.
pub fn payload(event_type: &str, value: &Value) -> Result<IndexMap<String, Value>, StoreError> {
    let mismatch =
        || StoreError::Invalid("event payload does not match its event type".to_string());
    let allowed = PAYLOAD_KEYS
        .iter()
        .find(|(name, _)| *name == event_type)
        .map(|(_, keys)| *keys)
        .ok_or_else(mismatch)?;
    let Value::Object(fields) = value else {
        return Err(mismatch());
    };
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(mismatch());
    }
    let mut clean = IndexMap::new();
    for (key, item) in fields {
        if is_sensitive_key(key) || IDENTIFIER_SENSITIVE.contains(&key.as_str()) {
            clean.insert(key.clone(), Value::String("[redacted]".to_string()));
            continue;
        }
        match item {
            Value::Null | Value::Bool(_) | Value::Number(_) => {
                clean.insert(key.clone(), item.clone());
            }
            Value::String(text) => {
                if text.chars().count() > 512 {
                    return Err(StoreError::Invalid(
                        "event payload text exceeds its bound".to_string(),
                    ));
                }
                clean.insert(key.clone(), item.clone());
            }
            Value::Array(_) | Value::Object(_) => {
                return Err(StoreError::Invalid(
                    "event payload values must be bounded scalar facts".to_string(),
                ))
            }
        }
    }
    if python_json_len(&clean) > MAX_PAYLOAD_BYTES as usize {
        return Err(StoreError::Invalid(
            "event payload exceeds its byte bound".to_string(),
        ));
    }
    Ok(clean)
}

/// Byte length of the payload as sorted-key JSON with no NaN or infinity values.
///
/// Separators are `", "` and `": "`, and every non-ASCII
/// character as `\uXXXX` (or a surrogate pair), so each such character counts 6 or 12 bytes.
fn python_json_len(payload: &IndexMap<String, Value>) -> usize {
    let object: Map<String, Value> = payload
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    spiderhead_core::common::python_json(&Value::Object(object))
        .chars()
        .map(|c| match c.len_utf8() {
            1 => 1,
            _ if (c as u32) <= 0xFFFF => 6,
            _ => 12,
        })
        .sum()
}

/// Validate one serializable event without fabricating timestamps or source detail.
pub fn validate(event: &Value) -> Result<IndexMap<String, Value>, StoreError> {
    const KEYS: [&str; 6] = [
        "format",
        "sequence",
        "event_type",
        "occurred_at",
        "timestamp_state",
        "payload",
    ];
    let unsupported = || StoreError::Invalid("event has unsupported fields".to_string());
    let Value::Object(fields) = event else {
        return Err(unsupported());
    };
    if fields.len() != KEYS.len() || KEYS.iter().any(|key| !fields.contains_key(*key)) {
        return Err(unsupported());
    }
    let sequence_ok = fields["sequence"].as_i64().is_some_and(|value| value >= 1);
    if fields["format"] != FORMAT || !sequence_ok {
        return Err(StoreError::Invalid(
            "event format or sequence is invalid".to_string(),
        ));
    }
    let (occurred_at, timestamp_state) = timestamp(Some(&fields["occurred_at"]))?;
    if fields["timestamp_state"] != timestamp_state {
        return Err(StoreError::Invalid(
            "event timestamp state disagrees with its timestamp".to_string(),
        ));
    }
    let event_type = fields["event_type"].as_str().unwrap_or_default();
    let mut out = IndexMap::new();
    out.insert("format".to_string(), Value::String(FORMAT.to_string()));
    out.insert("sequence".to_string(), fields["sequence"].clone());
    out.insert("event_type".to_string(), fields["event_type"].clone());
    out.insert(
        "occurred_at".to_string(),
        occurred_at.map_or(Value::Null, Value::String),
    );
    out.insert(
        "timestamp_state".to_string(),
        Value::String(timestamp_state),
    );
    out.insert(
        "payload".to_string(),
        Value::Object(
            payload(event_type, &fields["payload"])?
                .into_iter()
                .collect(),
        ),
    );
    Ok(out)
}

/// Writer callback that persists one accepted event.
pub type EventWriter = Box<dyn FnMut(&IndexMap<String, Value>) -> Result<(), StoreError>>;

/// In-memory bounded event sink; a writer may persist each accepted event transactionally.
pub struct EventSink {
    cap: i64,
    writer: Option<EventWriter>,
    events: Vec<IndexMap<String, Value>>,
    dropped: i64,
}

impl EventSink {
    pub fn new(cap: i64, writer: Option<EventWriter>) -> Result<Self, StoreError> {
        if !(1..=MAX_EVENTS).contains(&cap) {
            return Err(StoreError::Invalid(format!(
                "event cap must be an integer from 1 to {MAX_EVENTS}"
            )));
        }
        Ok(Self {
            cap,
            writer,
            events: Vec::new(),
            dropped: 0,
        })
    }

    /// Append a typed record, or record omitted coverage when the bounded cap is reached.
    /// `occurred_at = None` stamps the current time.
    pub fn emit(
        &mut self,
        event_type: &str,
        payload: &IndexMap<String, Value>,
        occurred_at: Option<&Value>,
    ) -> Result<Option<IndexMap<String, Value>>, StoreError> {
        if self.events.len() as i64 >= self.cap {
            self.dropped += 1;
            return Ok(None);
        }
        let (timestamp, timestamp_state) = timestamp(occurred_at)?;
        let record = serde_json::json!({
            "format": FORMAT,
            "sequence": self.events.len() as i64 + 1,
            "event_type": event_type,
            "occurred_at": timestamp,
            "timestamp_state": timestamp_state,
            "payload": payload.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Map<String, Value>>(),
        });
        let event = validate(&record)?;
        if let Some(writer) = self.writer.as_mut() {
            writer(&event)?;
        }
        self.events.push(event.clone());
        Ok(Some(event))
    }

    pub fn coverage(&self) -> IndexMap<String, Value> {
        let state = if self.dropped > 0 {
            "partial"
        } else {
            "complete"
        };
        let mut coverage = IndexMap::new();
        coverage.insert("state".to_string(), Value::String(state.to_string()));
        coverage.insert(
            "captured".to_string(),
            Value::from(self.events.len() as i64),
        );
        coverage.insert("dropped".to_string(), Value::from(self.dropped));
        coverage.insert("cap".to_string(), Value::from(self.cap));
        coverage
    }

    pub fn snapshot(&self) -> IndexMap<String, Value> {
        let mut snapshot = IndexMap::new();
        snapshot.insert(
            "format".to_string(),
            Value::String(CRAWL_TIMELINE_V1.to_string()),
        );
        snapshot.insert(
            "events".to_string(),
            Value::Array(
                self.events
                    .iter()
                    .map(|event| Value::Object(event.clone().into_iter().collect()))
                    .collect(),
            ),
        );
        snapshot.insert(
            "coverage".to_string(),
            Value::Object(self.coverage().into_iter().collect()),
        );
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn payload_map(value: Value) -> IndexMap<String, Value> {
        value
            .as_object()
            .expect("object")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    #[test]
    fn sink_drops_past_cap_and_reports_partial_coverage() {
        let mut sink = EventSink::new(2, None).expect("sink");
        let payload = payload_map(json!({"count": 1}));
        for _ in 0..2 {
            assert!(sink
                .emit("urls_added", &payload, Some(&Value::Null))
                .expect("emit")
                .is_some());
        }
        assert!(sink
            .emit("urls_added", &payload, Some(&Value::Null))
            .expect("emit")
            .is_none());
        let coverage = sink.coverage();
        assert_eq!(coverage["state"], "partial");
        assert_eq!(coverage["captured"], 2);
        assert_eq!(coverage["dropped"], 1);
        assert_eq!(coverage["cap"], 2);
    }

    #[test]
    fn writer_receives_each_accepted_event_in_order() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink_seen = Rc::clone(&seen);
        let writer: EventWriter = Box::new(move |event| {
            sink_seen.borrow_mut().push(event["sequence"].clone());
            Ok(())
        });
        let mut sink = EventSink::new(10, Some(writer)).expect("sink");
        let payload = payload_map(json!({"count": 3}));
        sink.emit("urls_added", &payload, Some(&Value::Null))
            .expect("first");
        sink.emit("urls_added", &payload, Some(&Value::Null))
            .expect("second");
        assert_eq!(*seen.borrow(), vec![json!(1), json!(2)]);
    }

    // A non-ASCII byte at the zone offset must return false, not panic on a slice.
    #[test]
    fn rfc3339_rejects_non_ascii_zone_without_panicking() {
        assert!(!is_rfc3339("2026-10-25T12:34:56€+0", false));
        assert!(!is_rfc3339("2026-10-25T12:34:56€+00", false));
        assert!(!is_rfc3339("2026-10-25T12:34:56+€0:0", false));
        assert!(!is_rfc3339("2026-10-25T12:34:56+€000", false));
        assert!(is_rfc3339("2026-10-25T12:34:56+05:30", false));
        assert!(is_rfc3339("2026-10-25T12:34:56+0530", false));
    }

    #[test]
    fn sink_rejects_cap_outside_bounds() {
        assert!(EventSink::new(0, None).is_err());
        assert!(EventSink::new(MAX_EVENTS + 1, None).is_err());
    }

    #[test]
    fn payload_keeps_scalar_facts_and_redacts_identifier_keys() {
        let clean = payload(
            "request",
            &json!({"url_id": 7, "method": "GET", "state": "ok", "queue_ordinal": 1, "attempt": 0}),
        )
        .expect("payload");
        // `url_id` is not in IDENTIFIER_SENSITIVE (exact match), so the scalar survives.
        assert_eq!(clean["url_id"], 7);
        assert_eq!(clean["method"], "GET");
        assert_eq!(clean["attempt"], 0);
    }

    #[test]
    fn payload_rejects_unknown_keys_nested_values_and_unknown_types() {
        assert!(payload("stop", &json!({"reason": "x", "extra": 1})).is_err());
        assert!(payload("stop", &json!({"reason": ["nested"]})).is_err());
        assert!(payload("nonexistent", &json!({})).is_err());
    }

    #[test]
    fn payload_text_bound_is_512_characters() {
        assert!(payload("stop", &json!({"reason": "a".repeat(513)})).is_err());
        assert!(payload("stop", &json!({"reason": "a".repeat(512)})).is_ok());
    }

    #[test]
    fn timestamp_states_follow_the_value() {
        assert_eq!(timestamp(Some(&Value::Null)).expect("null").1, "unknown");
        let (value, state) = timestamp(Some(&json!("2026-10-25T12:34:56.123456Z"))).expect("utc");
        assert_eq!(state, "known");
        assert_eq!(value.as_deref(), Some("2026-10-25T12:34:56.123456Z"));
        assert!(timestamp(Some(&json!("2026-10-25T12:34:56"))).is_err());
        assert!(timestamp(Some(&json!("2026-10-25T12:34:56+03:00"))).is_err());
        assert!(timestamp(Some(&json!(5))).is_err());
        assert_eq!(timestamp(None).expect("unset").1, "known");
    }

    #[test]
    fn validate_accepts_a_well_formed_event_and_rejects_bad_state() {
        let good = json!({
            "format": FORMAT, "sequence": 1, "event_type": "stop",
            "occurred_at": null, "timestamp_state": "unknown",
            "payload": {"reason": "done"}
        });
        assert_eq!(validate(&good).expect("valid")["payload"]["reason"], "done");
        let mut bad = good.clone();
        bad["timestamp_state"] = json!("known");
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn python_json_length_escapes_non_ascii() {
        let mut clean = IndexMap::new();
        clean.insert("reason".to_string(), Value::String("\u{e9}".to_string()));
        // Sorted-key JSON escapes `é` as `\u00e9`: 20 bytes in total.
        assert_eq!(python_json_len(&clean), 20);
    }
}
