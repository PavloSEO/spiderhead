//! Provenance written into the `scan.v1` header: product version, format revision and
//! the runtime versions of the capture. The store checks the exact key set
//! (`store::scan_schema::RUNTIME_KEYS`).

use serde_json::{Map, Value};

/// Product version recorded in the scan header (`writer_version`).
pub const WRITER_VERSION: &str = "3.2.0";

/// Format revision the scan and its regression baselines are pinned to (`writer_revision`).
pub const WRITER_REVISION: &str = "b9847831b9127a88504eabb4cd345f4b8f4bfea9";

/// Runtime provenance of the capture, pinned byte-for-byte.
pub fn runtime_versions() -> Map<String, Value> {
    let mut runtime = Map::new();
    runtime.insert("python".into(), Value::from("3.14.6"));
    runtime.insert("sqlite".into(), Value::from("3.53.3"));
    runtime.insert("httpx".into(), Value::from("0.28.1"));
    runtime.insert("lxml".into(), Value::from("6.1.1"));
    runtime.insert("beautifulsoup4".into(), Value::from("4.15.0"));
    runtime
}

/// Limitation notes recorded on every native crawl, worded exactly as stored.
pub fn limitations() -> Vec<String> {
    vec!["browser-network response capture is unavailable".to_string()]
}
