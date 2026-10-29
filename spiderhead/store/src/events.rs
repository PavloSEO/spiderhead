//! Optional scan.v2 event-table persistence and read-only bounded timeline views.

use crate::discovery_ledger::{canonical_json, is_py_int};
use crate::error::StoreError;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};

pub const TABLE: &str = "scan_events";
pub const META_TABLE: &str = "scan_event_meta";

fn invalid(msg: &str) -> StoreError {
    StoreError::Invalid(msg.to_string())
}

/// Create the optional v2 extension tables inside the caller's existing transaction lane.
pub fn ensure_schema(con: &Connection) -> Result<(), StoreError> {
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS scan_events(\
         sequence INTEGER PRIMARY KEY CHECK(sequence > 0),\
         event_type TEXT NOT NULL,occurred_at TEXT,timestamp_state TEXT NOT NULL,\
         payload_json TEXT NOT NULL);\
         CREATE TABLE IF NOT EXISTS scan_event_meta(\
         singleton INTEGER PRIMARY KEY CHECK(singleton=1),cap INTEGER NOT NULL,\
         captured INTEGER NOT NULL,dropped INTEGER NOT NULL);",
    )?;
    Ok(())
}

/// Persist one already-bounded event; sequence must remain monotonic in this artifact.
pub fn append(con: &Connection, event: &Map<String, Value>) -> Result<(), StoreError> {
    // The crawl-events validator (crawl_events::validate) takes the raw event object.
    let validated = crate::crawl_events::validate(&Value::Object(event.clone()))?;
    let previous: Option<i64> =
        con.query_row(&format!("SELECT MAX(sequence) FROM {TABLE}"), [], |r| {
            r.get(0)
        })?;
    let sequence = validated
        .get("sequence")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if sequence != previous.unwrap_or(0) + 1 {
        return Err(invalid("scan event sequence is not monotonic"));
    }
    let payload = validated.get("payload").cloned().unwrap_or(Value::Null);
    con.execute(
        &format!(
            "INSERT INTO {TABLE}(sequence,event_type,occurred_at,timestamp_state,payload_json) VALUES(?1,?2,?3,?4,?5)"
        ),
        rusqlite::params![
            sequence,
            validated.get("event_type").and_then(Value::as_str),
            validated.get("occurred_at").and_then(Value::as_str),
            validated.get("timestamp_state").and_then(Value::as_str),
            canonical_json(&payload),
        ],
    )?;
    Ok(())
}

/// Persist the sink's cap outcome without inventing unrecorded event times.
pub fn set_coverage(con: &Connection, coverage: &Map<String, Value>) -> Result<(), StoreError> {
    let max_events = crate::crawl_events::MAX_EVENTS;
    let state = coverage.get("state").and_then(Value::as_str).unwrap_or("");
    let int_field = |name: &str| {
        coverage
            .get(name)
            .filter(|v| is_py_int(v))
            .and_then(Value::as_i64)
    };
    let (captured, dropped, cap) = (
        int_field("captured"),
        int_field("dropped"),
        int_field("cap"),
    );
    let valid = coverage.len() == 4
        && (state == "complete" || state == "partial")
        && matches!((captured, dropped, cap), (Some(c), Some(d), Some(p))
            if c >= 0 && d >= 0 && p >= 0 && (1..=max_events).contains(&p) && c <= p
                && ((state == "complete") == (d == 0)));
    if !valid {
        return Err(invalid("scan event coverage is invalid"));
    }
    let (captured, dropped, cap) = (
        captured.unwrap_or(0),
        dropped.unwrap_or(0),
        cap.unwrap_or(0),
    );
    con.execute(
        &format!(
            "INSERT INTO {META_TABLE}(singleton,cap,captured,dropped) VALUES(1,?1,?2,?3) \
             ON CONFLICT(singleton) DO UPDATE SET cap=excluded.cap,\
             captured=MAX({META_TABLE}.captured,excluded.captured),\
             dropped=MAX({META_TABLE}.dropped,excluded.dropped)"
        ),
        rusqlite::params![cap, captured, dropped],
    )?;
    Ok(())
}

/// Read one bounded ordered timeline; this helper never writes or contacts a provider.
pub fn timeline(con: &Connection, limit: i64) -> Result<Map<String, Value>, StoreError> {
    if !(1..=crate::crawl_events::MAX_EVENTS).contains(&limit) {
        return Err(invalid(&format!(
            "timeline limit must be an integer from 1 to {}",
            crate::crawl_events::MAX_EVENTS
        )));
    }
    let rows: Vec<(i64, String, Option<String>, String, String)> = {
        let fetch = || -> rusqlite::Result<_> {
            let mut st = con.prepare(&format!(
                "SELECT sequence,event_type,occurred_at,timestamp_state,payload_json FROM {TABLE} \
                 ORDER BY sequence DESC LIMIT ?1"
            ))?;
            let rows = st.query_map([limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            rows.collect()
        };
        fetch().map_err(|_| invalid("scan does not contain the optional event timeline"))?
    };
    let meta: Option<(i64, i64, i64)> = con
        .query_row(
            &format!("SELECT cap,captured,dropped FROM {META_TABLE} WHERE singleton=1"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| invalid("scan does not contain the optional event timeline"))?;
    let mut events = Vec::new();
    for (sequence, event_type, occurred_at, timestamp_state, payload_json) in rows.into_iter().rev()
    {
        let payload: Value = serde_json::from_str(&payload_json)
            .map_err(|_| invalid("scan event timeline is malformed"))?;
        let mut raw = Map::new();
        raw.insert("format".into(), Value::from(crate::crawl_events::FORMAT));
        raw.insert("sequence".into(), Value::from(sequence));
        raw.insert("event_type".into(), Value::from(event_type));
        raw.insert(
            "occurred_at".into(),
            occurred_at.map_or(Value::Null, Value::from),
        );
        raw.insert("timestamp_state".into(), Value::from(timestamp_state));
        raw.insert("payload".into(), payload);
        let validated = crate::crawl_events::validate(&Value::Object(raw))
            .map_err(|_| invalid("scan event timeline is malformed"))?;
        events.push(Value::Object(validated.into_iter().collect()));
    }
    let coverage = match meta {
        Some((cap, captured, dropped)) => {
            let mut c = Map::new();
            c.insert(
                "state".into(),
                Value::from(if dropped != 0 { "partial" } else { "complete" }),
            );
            c.insert("captured".into(), Value::from(captured));
            c.insert("dropped".into(), Value::from(dropped));
            c.insert("cap".into(), Value::from(cap));
            c
        }
        None => {
            let mut c = Map::new();
            c.insert("state".into(), Value::from("unknown"));
            c.insert("captured".into(), Value::from(events.len() as i64));
            c.insert("dropped".into(), Value::from("unknown"));
            c.insert("cap".into(), Value::from("unknown"));
            c
        }
    };
    let mut out = Map::new();
    out.insert(
        "format".into(),
        Value::from(spiderhead_core::artifacts::CRAWL_TIMELINE_V1),
    );
    out.insert("events".into(), Value::Array(events));
    out.insert("coverage".into(), Value::Object(coverage));
    Ok(out)
}
