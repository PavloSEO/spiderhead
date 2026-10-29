//! Optional scan.v2 response transport facts; unknown phases stay explicitly unknown.

use crate::crawl_capture::{unavailable_transport_phases, TRANSPORT_PHASES};
use crate::error::StoreError;
use crate::scan_artifact::ScanError;
use indexmap::IndexMap;
use rusqlite::{params_from_iter, types::Value as Sql, Connection};
use serde_json::Value;
use std::collections::BTreeSet;

fn scan_err(message: &str) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// The captured transport facts of one response.
#[derive(Debug, Clone, Default)]
pub struct TransportEvent {
    /// Negotiated HTTP protocol label.
    pub http_version: Option<String>,
    /// `partial` or `unavailable`.
    pub timing_state: String,
    /// Per-phase `{seconds, state, reason}`; empty means the trace was unavailable.
    pub transport_phases: IndexMap<String, IndexMap<String, Value>>,
    /// Total response time in seconds.
    pub response_time: Option<f64>,
}

fn user_version(con: &Connection) -> Result<i64, StoreError> {
    Ok(con.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Create (or widen) `response_transport_meta` on a scan.v2 artifact; a no-op elsewhere.
pub fn ensure_schema(con: &Connection) -> Result<(), StoreError> {
    if user_version(con)? != 2 {
        return Ok(());
    }
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS response_transport_meta(response_id INTEGER PRIMARY KEY,\
         protocol TEXT,protocol_state TEXT NOT NULL,total_seconds REAL,\
         dns_seconds REAL,dns_state TEXT NOT NULL DEFAULT 'unavailable',dns_reason TEXT NOT NULL DEFAULT 'not captured',\
         connect_seconds REAL,connect_state TEXT NOT NULL DEFAULT 'unavailable',connect_reason TEXT NOT NULL DEFAULT 'not captured',\
         tls_seconds REAL,tls_state TEXT NOT NULL DEFAULT 'unavailable',tls_reason TEXT NOT NULL DEFAULT 'not captured',\
         ttfb_seconds REAL,ttfb_state TEXT NOT NULL DEFAULT 'unavailable',ttfb_reason TEXT NOT NULL DEFAULT 'not captured',\
         download_seconds REAL,download_state TEXT NOT NULL DEFAULT 'unavailable',download_reason TEXT NOT NULL DEFAULT 'not captured')",
    )?;
    let mut existing = BTreeSet::new();
    {
        let mut stmt = con.prepare("PRAGMA table_info(response_transport_meta)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in rows {
            existing.insert(name?);
        }
    }
    for phase in TRANSPORT_PHASES {
        for (suffix, definition) in [
            ("seconds", "REAL"),
            ("reason", "TEXT NOT NULL DEFAULT 'not captured'"),
        ] {
            let name = format!("{phase}_{suffix}");
            if !existing.contains(&name) {
                con.execute_batch(&format!(
                    "ALTER TABLE response_transport_meta ADD COLUMN {name} {definition}"
                ))?;
            }
        }
        let state = format!("{phase}_state");
        if !existing.contains(&state) {
            con.execute_batch(&format!(
                "ALTER TABLE response_transport_meta ADD COLUMN {state} TEXT NOT NULL DEFAULT 'unavailable'"
            ))?;
        }
    }
    Ok(())
}

fn json_to_sql(value: &Value) -> Sql {
    match value {
        Value::Null => Sql::Null,
        Value::Number(n) => match n.as_i64() {
            Some(i) => Sql::Integer(i),
            None => Sql::Real(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Sql::Text(s.clone()),
        Value::Bool(b) => Sql::Integer(*b as i64),
        other => Sql::Text(other.to_string()),
    }
}

/// Store the transport facts of one response (scan.v2 only).
pub fn record(
    con: &Connection,
    response_id: i64,
    event: &TransportEvent,
) -> Result<(), StoreError> {
    if user_version(con)? != 2 {
        return Ok(());
    }
    ensure_schema(con)?;
    if let Some(protocol) = &event.http_version {
        if protocol.chars().count() > 32 {
            return Err(scan_err("captured HTTP protocol is invalid"));
        }
    }
    if !matches!(event.timing_state.as_str(), "partial" | "unavailable") {
        return Err(scan_err("captured timing state is invalid"));
    }
    let phases = if event.transport_phases.is_empty() {
        unavailable_transport_phases("transport trace was unavailable")
    } else {
        event.transport_phases.clone()
    };
    let expected: BTreeSet<&str> = TRANSPORT_PHASES.iter().copied().collect();
    let got: BTreeSet<&str> = phases.keys().map(String::as_str).collect();
    if expected != got {
        return Err(scan_err("captured transport phases are invalid"));
    }
    let mut values: Vec<Sql> = vec![
        Sql::Integer(response_id),
        event.http_version.clone().map_or(Sql::Null, Sql::Text),
        Sql::Text(
            if event.http_version.as_deref().is_some_and(|p| !p.is_empty()) {
                "known"
            } else {
                "unavailable"
            }
            .into(),
        ),
        event.response_time.map_or(Sql::Null, Sql::Real),
    ];
    for name in TRANSPORT_PHASES {
        let phase = &phases[*name];
        for key in ["seconds", "state", "reason"] {
            values.push(json_to_sql(phase.get(key).unwrap_or(&Value::Null)));
        }
    }
    con.execute(
        "INSERT INTO response_transport_meta(response_id,protocol,protocol_state,total_seconds,\
         dns_seconds,dns_state,dns_reason,connect_seconds,connect_state,connect_reason,\
         tls_seconds,tls_state,tls_reason,ttfb_seconds,ttfb_state,ttfb_reason,\
         download_seconds,download_state,download_reason) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params_from_iter(values),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch("PRAGMA user_version=2").unwrap();
        con
    }

    #[test]
    fn unavailable_trace_is_stored_as_unknown() {
        let con = v2();
        let event = TransportEvent {
            timing_state: "unavailable".into(),
            ..Default::default()
        };
        record(&con, 7, &event).unwrap();
        let (state, dns): (String, String) = con
            .query_row(
                "SELECT protocol_state,dns_state FROM response_transport_meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (state.as_str(), dns.as_str()),
            ("unavailable", "unavailable")
        );
    }

    #[test]
    fn v1_scan_is_ignored() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch("PRAGMA user_version=1").unwrap();
        record(&con, 1, &TransportEvent::default()).unwrap();
        assert!(con
            .prepare("SELECT 1 FROM response_transport_meta")
            .is_err());
    }

    #[test]
    fn invalid_timing_state_is_refused() {
        let con = v2();
        let event = TransportEvent {
            timing_state: "bogus".into(),
            ..Default::default()
        };
        assert!(record(&con, 1, &event).is_err());
    }
}
