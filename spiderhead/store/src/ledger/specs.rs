//! Occurrence identity and evidence slicing for one saved issue.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use indexmap::IndexMap;
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::ingest::{as_int, truthy};
use super::schema::{canonical_url, key};
use crate::error::StoreError;

/// Occurrence representations a spec may carry.
pub const REPRESENTATIONS: &[&str] = &[
    "legacy_fragment",
    "legacy_unknown",
    "rendered",
    "scope",
    "static",
    "unknown",
];

/// Subject value of an audit-wide finding.
pub const SCOPE_SUBJECT: &str = "site";

/// Regex source locating the document id inside an observation id.
pub const DOC_MARKER: &str = r":document:(\d+):representation:";

fn doc_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(DOC_MARKER).expect("static pattern"))
}

/// URL to its recorded representation, the audit document's claim first.
pub fn representation_map(scan_con: &Connection, document: &Value) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let mut collect = || -> Result<(), StoreError> {
        let mut stmt = scan_con
            .prepare("SELECT u.url,p.representation FROM pages p JOIN urls u USING(url_id)")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let url: String = row.get(0)?;
            let representation: Option<String> = row.get(1)?;
            if let Some(rep) = representation.filter(|r| !r.is_empty()) {
                result.insert(canonical_url(&url)?, rep);
            }
        }
        Ok(())
    };
    // A failure stops this pass only; the document's own claims still apply below.
    let _ = collect();
    for page in document
        .get("pages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(page) = page.as_object() else {
            continue;
        };
        let value = page
            .get("metrics")
            .and_then(Value::as_object)
            .and_then(|m| m.get("representation"))
            .and_then(Value::as_str);
        let url = page.get("url").and_then(Value::as_str);
        if let (Some(url), Some(value)) = (url, value) {
            if !value.is_empty() {
                if let Ok(canon) = canonical_url(url) {
                    result.insert(canon, value.to_string());
                }
            }
        }
    }
    result
}

/// Page URL to typed observation ids, and document id to page URL.
pub type ObservationIndex = (BTreeMap<String, BTreeSet<String>>, BTreeMap<i64, String>);

/// Map each saved page URL to the typed observation ids it owns.
///
/// `observation_id` values in the evidence contract are `url_id:N`, `document_id:N`,
/// `response_id:N` or `language_evidence:...:document:N`. Resolving them to URLs lets one
/// occurrence name the retained evidence for its own affected page instead of borrowing another
/// page's.
pub fn observation_index(scan_con: &Connection) -> ObservationIndex {
    let mut by_url: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut doc_url = BTreeMap::new();
    let Ok(mut stmt) = scan_con.prepare(
        "SELECT u.url,p.url_id,p.document_id,d.source_response_id FROM pages p JOIN urls u USING(url_id) LEFT JOIN documents d ON d.document_id=p.document_id",
    ) else {
        return (by_url, doc_url);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return (by_url, doc_url);
    };
    while let Ok(Some(row)) = rows.next() {
        let (Ok(url), Ok(url_id)) = (row.get::<_, String>(0), row.get::<_, i64>(1)) else {
            continue;
        };
        let document_id: Option<i64> = row.get(2).unwrap_or(None);
        let response_id: Option<i64> = row.get(3).unwrap_or(None);
        let Ok(canon) = canonical_url(&url) else {
            continue;
        };
        let owned = by_url.entry(canon.clone()).or_default();
        owned.insert(format!("url_id:{url_id}"));
        if let Some(d) = document_id.filter(|d| *d > 0) {
            owned.insert(format!("document_id:{d}"));
            doc_url.insert(d, canon.clone());
        }
        if let Some(r) = response_id.filter(|r| *r > 0) {
            owned.insert(format!("response_id:{r}"));
        }
    }
    (by_url, doc_url)
}

/// Slice the issue-level contract refs down to this occurrence's own page.
pub fn occurrence_evidence(
    contract: &Value,
    role: &str,
    url: Option<&str>,
    by_url: &BTreeMap<String, BTreeSet<String>>,
    doc_url: &BTreeMap<i64, String>,
) -> Value {
    let observations = contract
        .get("observations")
        .and_then(Value::as_array)
        .filter(|o| !o.is_empty());
    let Some(observations) = observations else {
        let reason = contract
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("no saved evidence contract is present");
        let reason = if reason.is_empty() {
            "no retained observation for this affected URL"
        } else {
            reason
        };
        return json!({"state": "unavailable", "reason": reason, "references": []});
    };
    let mut refs: Vec<&Value> = Vec::new();
    for item in observations {
        let Some(map) = item.as_object() else {
            continue;
        };
        let Some(observation_id) = map.get("observation_id").and_then(Value::as_str) else {
            continue;
        };
        if role == "target" {
            if map.get("role").and_then(Value::as_str) == Some("target") {
                refs.push(item);
                continue;
            }
            if let Some(caps) = doc_marker().captures(observation_id) {
                let doc = caps[1].parse::<i64>().ok();
                if doc.and_then(|d| doc_url.get(&d)).map(String::as_str) == url && url.is_some() {
                    refs.push(item);
                }
            }
        } else if let Some(u) = url {
            if by_url
                .get(u)
                .is_some_and(|set| set.contains(observation_id))
            {
                refs.push(item);
            }
        }
    }
    if refs.is_empty() {
        return json!({
            "state": "unavailable",
            "reason": "no retained observation for this affected URL",
            "references": [],
        });
    }
    let state = refs[0].get("state").and_then(Value::as_str);
    let measured = matches!(state, Some("measured" | "imported_projection"));
    let references: Vec<Value> = refs
        .iter()
        .map(|item| {
            let mut out = Map::new();
            for key in [
                "id",
                "source_table",
                "observation_id",
                "role",
                "representation",
            ] {
                if let Some(v) = item.get(key) {
                    out.insert(key.to_string(), v.clone());
                }
            }
            Value::Object(out)
        })
        .collect();
    json!({
        "state": if measured { state.unwrap_or("unavailable") } else { "unavailable" },
        "reason": if measured { "" } else { "retained observation state is unknown" },
        "references": references,
    })
}

/// Classify how much of a reported total the audit enumerated.
///
/// `enumerated` means the saved document names the whole reported total; `capped` means the
/// audit kept only a bounded location list; `aggregate_only` means repeated locations lack
/// enough stable locator data to distinguish their events. `unknown` means the document does not
/// state a usable total. The enumerated count is stable location events, not distinct affected
/// URLs; those memberships are stored separately.
pub fn coverage_state(issue: &Value, locations: &[Value]) -> (String, i64) {
    let reported = issue.get("occurrences_count").and_then(as_int);
    let entries = location_entries(locations);
    let mut order: Vec<&str> = Vec::new();
    for entry in &entries {
        if !order.contains(&entry.url.as_str()) {
            order.push(&entry.url);
        }
    }
    let groups = locator_groups(issue, &entries);
    let mut enumerated: i64 = order
        .iter()
        .map(|url| groups.get(*url).map_or(1, |g| g.len() as i64))
        .sum();
    if locations.is_empty() && issue.get("target_url").is_some_and(Value::is_string) {
        enumerated = 1;
    }
    let Some(reported) = reported.filter(|r| *r >= 0) else {
        return ("unknown".into(), enumerated);
    };
    if locations.is_empty() {
        return (
            if reported <= enumerated {
                "enumerated"
            } else {
                "aggregate_only"
            }
            .into(),
            enumerated,
        );
    }
    if reported > locations.len() as i64 {
        return ("capped".into(), reported.min(enumerated));
    }
    if reported > enumerated {
        return ("aggregate_only".into(), enumerated);
    }
    ("enumerated".into(), reported.min(enumerated))
}

/// One occurrence spec: where a finding manifests.
#[derive(Debug, Clone, PartialEq)]
pub struct OccurrenceSpec {
    /// `url` or `scope`.
    pub subject_type: String,
    /// Subject value.
    pub subject_value: String,
    /// One of [`REPRESENTATIONS`].
    pub representation: String,
    /// `primary`, `subject` or `locator`.
    pub discriminator_type: String,
    /// Discriminator value.
    pub discriminator_value: String,
    /// `target` or `source`.
    pub role: String,
}

impl OccurrenceSpec {
    /// The spec as the JSON object hashed into an observation payload.
    pub fn to_value(&self) -> Value {
        json!({
            "subject_type": self.subject_type,
            "subject_value": self.subject_value,
            "representation": self.representation,
            "discriminator_type": self.discriminator_type,
            "discriminator_value": self.discriminator_value,
            "role": self.role,
        })
    }
}

/// Ordered `(canonical URL, role)` members; duplicates collapse, roles may both persist.
pub fn affected_urls(issue: &Value) -> Result<Vec<(String, String)>, StoreError> {
    let mut members: Vec<(String, String)> = Vec::new();
    let mut add = |url: Option<&Value>, role: &str| -> Result<(), StoreError> {
        let Some(url) = url.and_then(Value::as_str).filter(|u| !u.trim().is_empty()) else {
            return Ok(());
        };
        let canon = canonical_url(url)?;
        if !members.iter().any(|(u, r)| *u == canon && r == role) {
            members.push((canon, role.to_string()));
        }
        Ok(())
    };
    add(issue.get("target_url"), "subject")?;
    if let Some(locations) = issue.get("locations").and_then(Value::as_array) {
        for location in locations {
            if !location.is_object() {
                continue;
            }
            add(location.get("source_url"), "location")?;
            add(location.get("url"), "location")?;
        }
    }
    Ok(members)
}

/// Stable source/page locator data carried by one audit location.
#[derive(Debug, Clone)]
pub struct LocationEntry {
    /// Canonical page URL.
    pub url: String,
    /// Stripped link path, when usable.
    pub link_path: Option<String>,
    /// Representation named by the location.
    pub representation: Value,
}

/// Return the stable source/page locator data carried by audit locations.
pub fn location_entries(locations: &[Value]) -> Vec<LocationEntry> {
    let mut entries = Vec::new();
    for location in locations {
        let Some(map) = location.as_object() else {
            continue;
        };
        let raw = [map.get("source_url"), map.get("url")]
            .into_iter()
            .flatten()
            .find(|v| truthy(v));
        let Some(raw_url) = raw.and_then(Value::as_str).filter(|u| !u.trim().is_empty()) else {
            continue;
        };
        let Ok(url) = canonical_url(raw_url) else {
            continue;
        };
        let link_path = map
            .get("link_path")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty() && p.chars().count() <= 2048)
            .map(|p| p.trim().to_string());
        entries.push(LocationEntry {
            url,
            link_path,
            representation: map.get("representation").cloned().unwrap_or(Value::Null),
        });
    }
    entries
}

/// Repeated URL locations whose link paths make each occurrence stable.
pub fn locator_groups(
    issue: &Value,
    entries: &[LocationEntry],
) -> IndexMap<String, Vec<LocationEntry>> {
    let mut by_url: IndexMap<String, Vec<LocationEntry>> = IndexMap::new();
    for entry in entries {
        by_url
            .entry(entry.url.clone())
            .or_default()
            .push(entry.clone());
    }
    let reported = issue.get("occurrences_count").and_then(as_int);
    match reported {
        Some(r) if r > by_url.len() as i64 => {}
        _ => return IndexMap::new(),
    }
    by_url
        .into_iter()
        .filter(|(_, rows)| {
            let paths: BTreeSet<&Option<String>> = rows.iter().map(|r| &r.link_path).collect();
            rows.len() > 1
                && rows.iter().all(|r| r.link_path.is_some())
                && paths.len() == rows.len()
        })
        .collect()
}

/// Discriminator of an occurrence told apart by its link path.
pub fn locator_discriminator(subject_value: &str, url: &str, link_path: &str) -> String {
    format!(
        "link_path:{}",
        key(&[
            "spiderhead.ledger-link-path.v1",
            subject_value,
            url,
            link_path
        ])
    )
}

fn spec(
    subject_type: &str,
    subject_value: &str,
    representation: &str,
    discriminator_type: &str,
    discriminator_value: &str,
    role: &str,
) -> OccurrenceSpec {
    OccurrenceSpec {
        subject_type: subject_type.into(),
        subject_value: subject_value.into(),
        representation: representation.into(),
        discriminator_type: discriminator_type.into(),
        discriminator_value: discriminator_value.into(),
        role: role.into(),
    }
}

/// Derive the occurrence set for one saved issue.
///
/// The primary occurrence sits at the finding's own subject; every other distinct affected URL
/// gets an occurrence discriminated by that subject, so two findings of one check landing on the
/// same page stay two cases. A location naming the subject itself merges into the primary
/// occurrence.
pub fn occurrence_specs(
    issue: &Value,
    subject_type: &str,
    subject_value: &str,
    members: &[(String, String)],
    representations: &BTreeMap<String, String>,
) -> Vec<OccurrenceSpec> {
    let repr_for = |url: &str| -> String {
        let claimed = issue.get("representation").filter(|v| truthy(v));
        match claimed {
            Some(Value::String(s)) => s.clone(),
            Some(_) => "unknown".to_string(),
            None => representations
                .get(url)
                .cloned()
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "unknown".into()),
        }
    };
    let pick = |entry: &LocationEntry, url: &str| -> String {
        match entry.representation.as_str() {
            Some(r) if REPRESENTATIONS.contains(&r) => r.to_string(),
            _ => repr_for(url),
        }
    };
    let locations: Vec<Value> = issue
        .get("locations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let entries = location_entries(&locations);
    let groups = locator_groups(issue, &entries);
    let mut specs = Vec::new();
    if subject_type == "scope" {
        specs.push(spec(
            "scope",
            SCOPE_SUBJECT,
            "scope",
            "primary",
            "",
            "target",
        ));
    } else if let Some(rows) = groups.get(subject_value) {
        for entry in rows {
            let path = entry.link_path.as_deref().unwrap_or("");
            specs.push(spec(
                "url",
                subject_value,
                &pick(entry, subject_value),
                "locator",
                &locator_discriminator(subject_value, subject_value, path),
                "target",
            ));
        }
    } else {
        specs.push(spec(
            "url",
            subject_value,
            &repr_for(subject_value),
            "primary",
            "",
            "target",
        ));
    }
    for (url, _role) in members {
        if subject_type == "url" && url == subject_value {
            continue;
        }
        if let Some(rows) = groups.get(url) {
            for entry in rows {
                let path = entry.link_path.as_deref().unwrap_or("");
                specs.push(spec(
                    "url",
                    url,
                    &pick(entry, url),
                    "locator",
                    &locator_discriminator(subject_value, url, path),
                    "source",
                ));
            }
        } else {
            specs.push(spec(
                "url",
                url,
                &repr_for(url),
                "subject",
                subject_value,
                "source",
            ));
        }
    }
    specs
}
