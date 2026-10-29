//! Stable remote scan request and evidence contracts, independent of job execution.

use serde_json::{Map, Value};
use spiderhead_core::job_contracts::ScanEvidence;

use crate::error::StoreError;
use crate::status::scan_status;

/// The scan-status keys that make up the evidence block.
const EVIDENCE_KEYS: [&str; 3] = ["source", "frontier", "committed_page_outcomes"];

/// Select the evidence block out of a scan-status answer and validate it.
pub fn evidence_from_status(status: &Map<String, Value>) -> Result<ScanEvidence, StoreError> {
    let mut block = Map::new();
    for key in EVIDENCE_KEYS {
        let value = status
            .get(key)
            .ok_or_else(|| StoreError::Invalid(format!("scan status has no {key}")))?;
        block.insert(key.to_string(), value.clone());
    }
    Ok(serde_json::from_value(Value::Object(block))?)
}

/// Build the same core scan-status evidence used by the CLI/MCP handler.
pub fn evidence_from_scan(input_path: &str) -> Result<ScanEvidence, StoreError> {
    let status = scan_status(input_path, false)?;
    let status: Map<String, Value> = status.into_iter().collect();
    evidence_from_status(&status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status() -> Map<String, Value> {
        json!({
            "source": {
                "scan_uuid": "00000000-0000-4000-8000-000000000000",
                "format_version": "scan.v1",
                "source_kind": "native",
                "parent_scan_uuid": null,
                "writer_version": "1",
                "writer_revision": "abc",
                "evidence_revision": 3,
                "created_at": "2026-01-01T00:00:00Z",
                "finished_at": "2026-01-01T00:01:00Z",
                "lifecycle": "finished",
                "finish_reason": "finished",
                "crawl_partial": false,
                "corpus_partial": false
            },
            "frontier": {"state": "available", "reason": "", "counts": {"queued": 0, "inflight": 0, "done": 10, "excluded": 1}},
            "committed_page_outcomes": {"2xx": 8, "3xx": 1, "4xx": 1, "5xx": 0, "other": 0, "no_response": 0},
            "extra": 1
        })
        .as_object()
        .cloned()
        .unwrap()
    }

    #[test]
    fn evidence_keeps_exactly_the_three_status_keys() {
        let evidence = evidence_from_status(&status()).unwrap();
        assert_eq!(evidence.committed_page_outcomes.status_2xx, 8);
        assert_eq!(evidence.frontier.counts.unwrap().excluded, 1);
        assert_eq!(evidence.source.format_version, "scan.v1");
    }

    #[test]
    fn missing_key_is_an_error() {
        let mut broken = status();
        broken.remove("frontier");
        assert!(evidence_from_status(&broken).is_err());
    }
}
