//! Local-only Looker Studio Linking API URL for the reporting pack.
//!
//! The builder cannot publish or inspect a Looker report. It requires a supplied
//! original report ID and returns that ID as externally unverified: the native
//! five-page template remains a separate, user-authorized Google step.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::{json, Map, Value};
use thiserror::Error;

use spiderhead_core::formats::matches;

use crate::bi::schema::{dataset_specs, BI_SCHEMA_VERSION, MANIFEST_FORMAT};

pub const LINK_FORMAT: &str = "spiderhead.looker-copy-link.v1";
pub const LINK_BASE: &str = "https://datastudio.google.com/reporting/create";
pub const BLUEPRINT_FORMAT: &str = "spiderhead.looker-studio-blueprint.v1";
const PAGE_IDS: [&str; 5] = ["overview", "technical", "links", "content", "performance"];
const MAX_BLUEPRINT_BYTES: u64 = 1_000_000;

/// A local reporting-pack copy link lacks an exact, safe input contract.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct LookerLinkError(pub String);

impl LookerLinkError {
    fn new(message: impl Into<String>) -> Self {
        LookerLinkError(message.into())
    }
}

type Res<T> = Result<T, LookerLinkError>;
type FieldSchema = BTreeMap<String, Vec<Value>>;

/// Full-match check for the small anchored patterns used here.
fn fullmatch(pattern: &str, text: &str) -> bool {
    regex::Regex::new(&format!("^(?:{pattern})$"))
        .map(|re| re.is_match(text))
        .unwrap_or(false)
}

/// Form-encode query parameters: `+` for spaces, reserved characters escaped.
fn urlencode(parameters: &[(String, String)]) -> String {
    fn quote_plus(text: &str) -> String {
        let mut out = String::new();
        for byte in text.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                    out.push(byte as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }
    parameters
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Load one small regular reporting-pack blueprint without a network call.
pub fn load_blueprint(path: &str) -> Res<Value> {
    let source = Path::new(path);
    let meta = fs::symlink_metadata(source)
        .map_err(|_| LookerLinkError::new("blueprint must be an existing regular JSON file"))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(LookerLinkError::new(
            "blueprint must be an existing regular JSON file",
        ));
    }
    let raw = fs::read(source).map_err(|_| LookerLinkError::new("blueprint could not be read"))?;
    if raw.len() as u64 > MAX_BLUEPRINT_BYTES {
        return Err(LookerLinkError::new(
            "blueprint exceeds the 1 MiB local safety bound",
        ));
    }
    let value: Value = serde_json::from_slice(&raw)
        .map_err(|_| LookerLinkError::new("blueprint must be valid UTF-8 JSON"))?;
    if !value.is_object() {
        return Err(LookerLinkError::new("blueprint must be a JSON object"));
    }
    Ok(value)
}

/// `published_fields` — name/type/nullable per dataset (no description).
fn published_fields() -> FieldSchema {
    dataset_specs()
        .iter()
        .map(|(name, fields, _, _)| {
            (
                (*name).to_owned(),
                fields
                    .iter()
                    .map(|f| json!({"name": f.name, "type": f.type_, "nullable": f.nullable}))
                    .collect(),
            )
        })
        .collect()
}

/// `validate_blueprint` -> (used datasets, published field schema).
fn validate_blueprint(blueprint: &Value) -> Res<(BTreeSet<String>, FieldSchema)> {
    if !matches(
        blueprint.get("format").and_then(Value::as_str),
        BLUEPRINT_FORMAT,
    ) {
        return Err(LookerLinkError::new(
            "unsupported Looker reporting-pack blueprint format",
        ));
    }
    if !matches(
        blueprint.get("source_contract").and_then(Value::as_str),
        MANIFEST_FORMAT,
    ) {
        return Err(LookerLinkError::new(
            "blueprint does not consume the supported BI manifest schema",
        ));
    }
    let pages = blueprint.get("pages").and_then(Value::as_array);
    let ids: Vec<Option<&str>> = pages
        .map(|list| {
            list.iter()
                .filter(|p| p.is_object())
                .map(|p| p.get("id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let Some(pages) = pages else {
        return Err(LookerLinkError::new(
            "blueprint must contain the original five reporting-pack pages in order",
        ));
    };
    let matches_ids =
        ids.len() == PAGE_IDS.len() && ids.iter().zip(PAGE_IDS.iter()).all(|(a, b)| a == &Some(*b));
    if !matches_ids {
        return Err(LookerLinkError::new(
            "blueprint must contain the original five reporting-pack pages in order",
        ));
    }
    let published = published_fields();
    let mut used = BTreeSet::new();
    for page in pages {
        let datasets = page
            .as_object()
            .and_then(|p| p.get("datasets"))
            .and_then(Value::as_array);
        let Some(datasets) = datasets else {
            return Err(LookerLinkError::new("blueprint page datasets are invalid"));
        };
        let names: Vec<&str> = datasets.iter().filter_map(Value::as_str).collect();
        if datasets.is_empty()
            || datasets
                .iter()
                .any(|d| !d.as_str().is_some_and(|d| published.contains_key(d)))
        {
            return Err(LookerLinkError::new(
                "blueprint references an unsupported BI dataset",
            ));
        }
        let fields = page.get("fields").and_then(Value::as_array);
        let fields_ok = fields.is_some_and(|list| {
            list.iter().all(|name| {
                name.as_str().is_some_and(|name| {
                    names
                        .iter()
                        .flat_map(|d| published[*d].iter())
                        .any(|field| field["name"].as_str() == Some(name))
                })
            })
        });
        if !fields_ok {
            return Err(LookerLinkError::new(
                "blueprint page fields do not match the published BI schema",
            ));
        }
        used.extend(names.iter().map(|s| (*s).to_owned()));
    }
    if used.len() != published.len() {
        return Err(LookerLinkError::new(
            "blueprint must cover every declared BI dataset",
        ));
    }
    Ok((used, published))
}

fn require_text<'a>(value: Option<&'a Value>, label: &str) -> Res<&'a str> {
    let text = value.and_then(Value::as_str).unwrap_or_default();
    require_str(text, label)
}

fn require_str<'a>(text: &'a str, label: &str) -> Res<&'a str> {
    if text.trim().is_empty() {
        return Err(LookerLinkError::new(format!("{label} is required")));
    }
    Ok(text.trim())
}

/// `source_parameters` -> (dataset, [(param, value)]).
fn source_parameters(
    source: &Value,
    datasets: &BTreeSet<String>,
) -> Res<(String, Vec<(String, String)>)> {
    let alias = require_text(source.get("alias"), "data-source alias")?;
    if !fullmatch(r"[A-Za-z][A-Za-z0-9_]{0,62}", alias) {
        return Err(LookerLinkError::new(
            "data-source alias must be a closed identifier, not a wildcard",
        ));
    }
    let dataset = require_text(source.get("dataset"), "BI dataset")?;
    if !datasets.contains(dataset) {
        return Err(LookerLinkError::new(
            "data-source mapping names an unsupported BI dataset",
        ));
    }
    let name = require_text(source.get("data_source_name"), "data-source name")?;
    let kind = source.get("kind");
    let prefix = format!("ds.{alias}.");
    let mut params = vec![
        (format!("{prefix}datasourceName"), name.to_owned()),
        (format!("{prefix}refreshFields"), "false".to_owned()),
    ];
    match kind.and_then(Value::as_str) {
        Some("sheets") => {
            let spreadsheet_id =
                require_text(source.get("spreadsheet_id"), "Sheets spreadsheet ID")?;
            let worksheet_id = source.get("worksheet_id");
            let worksheet_int = match worksheet_id {
                Some(Value::Number(n)) => n.as_i64().filter(|_| {
                    // A JSON float is not an integer, even when it has no fraction.
                    n.is_i64() || n.is_u64()
                }),
                _ => None,
            };
            if !fullmatch(r"[A-Za-z0-9_-]{10,256}", spreadsheet_id)
                || worksheet_int.is_none_or(|w| w < 0)
            {
                return Err(LookerLinkError::new(
                    "Sheets mapping requires a valid spreadsheet ID and non-negative worksheet ID",
                ));
            }
            require_text(source.get("worksheet_title"), "Sheets worksheet title")?;
            params.extend([
                (format!("{prefix}connector"), "googleSheets".to_owned()),
                (format!("{prefix}spreadsheetId"), spreadsheet_id.to_owned()),
                (
                    format!("{prefix}worksheetId"),
                    worksheet_int.unwrap_or_default().to_string(),
                ),
                (format!("{prefix}hasHeader"), "true".to_owned()),
            ]);
        }
        Some("bigquery") => {
            let project_id = require_text(source.get("project_id"), "BigQuery project ID")?;
            let dataset_id = require_text(source.get("dataset_id"), "BigQuery dataset ID")?;
            let table_id = require_text(source.get("table_id"), "BigQuery table ID")?;
            if !fullmatch(r"[a-z][a-z0-9-]{4,61}[a-z0-9]", project_id)
                || !fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,1023}", dataset_id)
                || !fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,1023}", table_id)
            {
                return Err(LookerLinkError::new(
                    "BigQuery mapping contains an invalid project, dataset, or table ID",
                ));
            }
            params.extend([
                (format!("{prefix}connector"), "bigQuery".to_owned()),
                (format!("{prefix}type"), "TABLE".to_owned()),
                (format!("{prefix}projectId"), project_id.to_owned()),
                (format!("{prefix}datasetId"), dataset_id.to_owned()),
                (format!("{prefix}tableId"), table_id.to_owned()),
            ]);
            if source
                .get("billing_project_id")
                .is_some_and(|v| !v.is_null())
            {
                let billing = require_text(
                    source.get("billing_project_id"),
                    "BigQuery billing project ID",
                )?;
                if !fullmatch(r"[a-z][a-z0-9-]{4,61}[a-z0-9]", billing) {
                    return Err(LookerLinkError::new(
                        "BigQuery billing project ID is invalid",
                    ));
                }
                params.push((format!("{prefix}billingProjectId"), billing.to_owned()));
            }
        }
        _ => {
            return Err(LookerLinkError::new(
                "data-source kind must be 'sheets' or 'bigquery'",
            ))
        }
    }
    Ok((dataset.to_owned(), params))
}

/// Build a copy URL from a verified local blueprint and explicit source aliases.
///
/// `original_report_id` is syntactically validated only. This local code cannot
/// prove it refers to a published, viewable native report, so its state stays
/// explicit in the result rather than claiming template availability.
pub fn build_looker_copy_link(
    blueprint: &Value,
    original_report_id: &str,
    original_report_confirmed: bool,
    report_name: &str,
    data_sources: &Value,
) -> Res<Value> {
    let (datasets, fields) = validate_blueprint(blueprint)?;
    let report_id = require_str(original_report_id, "original report ID")?;
    if !fullmatch(r"[A-Za-z0-9_-]{8,128}", report_id) {
        return Err(LookerLinkError::new(
            "original report ID must be a real report identifier; blank/default templates are refused",
        ));
    }
    if !original_report_confirmed {
        return Err(LookerLinkError::new(
            "original report template is missing or unconfirmed; confirm the supplied real report ID",
        ));
    }
    let name = require_str(report_name, "report name")?;
    let Some(sources) = data_sources.as_array().filter(|s| !s.is_empty()) else {
        return Err(LookerLinkError::new(
            "one explicit data-source mapping is required for every BI dataset",
        ));
    };
    let mut parameters: Vec<(String, String)> = vec![
        ("c.reportId".to_owned(), report_id.to_owned()),
        ("c.mode".to_owned(), "edit".to_owned()),
        ("c.explain".to_owned(), "true".to_owned()),
        ("r.reportName".to_owned(), name.to_owned()),
    ];
    let mut mapped = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    for source in sources {
        if !source.is_object() {
            return Err(LookerLinkError::new("data-source mappings must be objects"));
        }
        let (dataset, source_params) = source_parameters(source, &datasets)?;
        let alias = source["alias"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if mapped.contains(&dataset) || aliases.contains(&alias) {
            return Err(LookerLinkError::new(
                "each BI dataset and data-source alias must be mapped exactly once",
            ));
        }
        mapped.insert(dataset);
        aliases.insert(alias);
        parameters.extend(source_params);
    }
    if mapped != datasets {
        return Err(LookerLinkError::new(
            "data-source mappings must cover every reporting-pack BI dataset",
        ));
    }
    Ok(json!({
        "format": LINK_FORMAT,
        "network": false,
        "writes": false,
        "source_contract": MANIFEST_FORMAT,
        "source_schema_version": BI_SCHEMA_VERSION,
        "template_state": "operator_confirmed_original_report_id_unverified_locally",
        "template_verification_required": [
            "Confirm the supplied original report ID is a real five-page reporting-pack template.",
            "Confirm the user has view access to that report and its configured data sources.",
            "Do not use a blank or Google default report as a substitute for this pack.",
        ],
        "original_report_id": report_id,
        "copy_link": format!("{LINK_BASE}?{}", urlencode(&parameters)),
        "data_source_aliases": aliases.iter().cloned().collect::<Vec<_>>(),
        "dataset_mappings": mapped.iter().cloned().collect::<Vec<_>>(),
        "field_schema": Value::Object(
            fields
                .into_iter()
                .map(|(k, v)| (k, Value::Array(v)))
                .collect::<Map<String, Value>>(),
        ),
        "five_page_import_checklist": blueprint["pages"].clone(),
        "global_controls": blueprint.get("global_controls").cloned().unwrap_or_else(|| json!([])),
        "date_controls": blueprint.get("date_controls").cloned().unwrap_or_else(|| json!([])),
        "calculated_fields": blueprint.get("calculated_fields").cloned().unwrap_or_else(|| json!([])),
        "filter_rules": blueprint.get("filter_rules").cloned().unwrap_or_else(|| json!([])),
    }))
}
