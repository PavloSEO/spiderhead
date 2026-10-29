//! Explicit, bounded paid backlink-index reads with preflight unit estimates and spend receipts.
//!
//! Ahrefs v3 and Majestic return backlink pairs. Moz's documented legacy v2 URL metrics
//! route returns per-URL authority/spam measurements; explicit `operation="backlinks"`
//! uses the separate JSON-RPC link-list method. A `transport` callable is the request seam;
//! each call issues one paid request only after confirmation and never
//! auto-paginates.

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use spiderhead_core::net::normalize_domain;
use std::path::Path;

use crate::credentials::{self, CredentialError};
use crate::http::{urlencode, Request, Transport};
use crate::metrika::SpendSink;
use crate::spend::{self, Record};
use crate::{pyjson, pystr};

mod errors;
pub use errors::*;
mod collect;
pub use collect::*;

/// Every selected Ahrefs field costs one unit; the 5-unit refdomain/subnet and 10-unit
/// traffic fields are never requested.
pub const AHREFS_FIELDS: [&str; 11] = [
    "url_from",
    "url_to",
    "anchor",
    "is_nofollow",
    "is_sponsored",
    "is_ugc",
    "is_lost",
    "first_seen_link",
    "last_visited",
    "last_seen",
    "domain_rating_source",
];
/// Provider response size limit, applied to both the transport and collect.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

const MOZ_LINKS_ENDPOINT: &str = "https://api.moz.com/jsonrpc";
const MOZ_LINKS_DOCS: &str = "https://moz.com/api/docs/methods/DataSiteLinkListAction";

fn endpoint(provider: &str) -> &'static str {
    match provider {
        "ahrefs" => "https://api.ahrefs.com/v3/site-explorer/all-backlinks",
        "majestic" => "https://api.majestic.com/api/json",
        "moz" => "https://lsapi.seomoz.com/v2/url_metrics",
        _ => MOZ_LINKS_ENDPOINT,
    }
}

/// Injected dependencies that `collect` runs against.
pub struct BlEnv<'a, T: Transport> {
    /// Env-var/filesystem host for `credentials.read`.
    pub host: &'a dyn Host,
    /// Credential config root.
    pub root: &'a Path,
    /// HTTP boundary used by the live transport.
    pub transport: &'a T,
    /// Spend-ledger sink; None writes to the real ledger.
    pub spend: Option<&'a SpendSink<'a>>,
    /// Credential reader substitute, given as (config path, env var).
    pub cred: Option<&'a CredProbe<'a>>,
    /// Request id generator for the Moz JSON-RPC call.
    pub uuid: Option<&'a dyn Fn() -> String>,
}

impl<'a, T: Transport> BlEnv<'a, T> {
    /// The production environment.
    pub fn live(host: &'a dyn Host, root: &'a Path, transport: &'a T) -> Self {
        BlEnv {
            host,
            root,
            transport,
            spend: None,
            cred: None,
            uuid: None,
        }
    }

    /// The real spend ledger or the injected sink; I/O errors propagate.
    fn record(&self, rec: &Record) -> Result<Value, BlError> {
        match self.spend {
            Some(sink) => sink(rec),
            None => spend::record(self.host, rec),
        }
        .map_err(|err| pyerr("OSError", err.to_string()))
    }

    fn cred(&self, path: &str, env_var: &str) -> Result<String, CredentialError> {
        match self.cred {
            Some(probe) => probe(path, env_var),
            None => credentials::read(self.host, self.root, path, env_var, ""),
        }
    }

    /// A random UUID v4 as a string.
    fn uuid4(&self) -> String {
        match self.uuid {
            Some(probe) => probe(),
            None => uuid4(),
        }
    }
}

/// Random UUID v4 string from OS randomness.
fn uuid4() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return String::new();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Use the given credential, or read it from the store when it is empty.
fn or_cred<T: Transport>(
    env: &BlEnv<'_, T>,
    explicit: Option<&Value>,
    path: &str,
    env_var: &str,
) -> Result<Value, CredentialError> {
    match explicit.filter(|v| pystr::truthy(Some(v))) {
        Some(v) => Ok(v.clone()),
        None => env.cred(path, env_var).map(Value::String),
    }
}

/// Estimate units for a call without network or credential access. Estimates show units,
/// not invented dollar prices.
pub fn estimate(
    provider: &Value,
    limit: Option<&Value>,
    operation: Option<&Value>,
) -> Result<Value, BlError> {
    let moz_links = provider == &json!("moz") && operation == Some(&json!("backlinks"));
    if operation.is_some_and(|v| !v.is_null()) && !moz_links {
        return value_err("operation is supported only as moz/backlinks");
    }
    let limit = match limit {
        None | Some(Value::Null) => json!(if moz_links { 25 } else { 100 }),
        Some(v) => v.clone(),
    };
    if !matches!(provider.as_str(), Some("ahrefs" | "majestic" | "moz"))
        || !pystr::is_py_int(&limit)
    {
        return value_err("provider must be ahrefs/majestic/moz and limit must be an integer");
    }
    let limit_i = limit.as_i64().unwrap_or(0);
    if moz_links {
        if !(1..=50).contains(&limit_i) {
            return value_err("Moz backlink limit must be 1..50");
        }
    } else if !(1..=1000).contains(&limit_i) {
        return value_err("limit must be 1..1000");
    }
    let provider_str = provider.as_str().unwrap_or_default();
    let (units, operation_name, reference) = match provider_str {
        "ahrefs" => (
            json!({"api_units": std::cmp::max(50, AHREFS_FIELDS.len() as i64 * limit_i)}),
            "backlinks",
            "https://docs.ahrefs.com/en/api/docs/limits-consumption",
        ),
        "majestic" => (
            json!({"analysis_units": 5000, "retrieval_units": limit_i}),
            "backlinks",
            "https://developer-support.majestic.com/api/commands/get-back-link-data.shtml",
        ),
        _ if moz_links => (Value::Null, "backlinks", MOZ_LINKS_DOCS),
        _ => (
            Value::Null,
            "url_metrics_v2",
            "https://moz.com/help/links-api/making-calls/url-metrics",
        ),
    };
    Ok(json!({
        "ok": true,
        "provider": provider,
        "operation": operation_name,
        "max_rows": if provider_str == "moz" && !moz_links { json!(1) } else { limit },
        "estimated_units": units,
        "currency_cost": null,
        "price_state": "account_plan_dependent",
        "price_reference": reference,
        "requires_confirmation": true,
        "legacy": provider_str == "moz" && !moz_links,
        "field_units": if provider_str == "ahrefs" {
            json!(AHREFS_FIELDS.iter().map(|f| (f.to_string(), json!(1))).collect::<Map<_,_>>())
        } else {
            Value::Null
        },
    }))
}
