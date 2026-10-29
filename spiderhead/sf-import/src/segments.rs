//! Segment declaration and assignment: named subsets of a crawl reported on
//! separately instead of folded into one averaged number.
//!
//! Membership is per page and non-exclusive. A segment's rules may reference an
//! earlier segment, so the list is a dependency graph that is validated (unknown
//! reference, duplicate name, cycle) and evaluated in a deterministic order that
//! does not depend on how the caller listed the definitions. Each page also gets
//! one `primary` segment so segmented tables sum to the ungrouped totals.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::pyvalue::{py_eq, py_repr_value, str_or_dumps};

/// Schema tag of the segment report.
pub const SCHEMA_VERSION: &str = "segments.v1";
/// Name of the bucket for pages matching no segment.
pub const UNSEGMENTED: &str = "unsegmented";

const VALID_OPS: [&str; 6] = ["prefix", "contains", "eq", "regex", "in", "segment"];

/// A segment set that cannot be evaluated as declared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SegmentError(pub String);

/// One condition; a segment matches a page when any of its rules do.
///
/// `field` is looked up on the page record first and then under `metrics`, so
/// one rule works on flat records and on stored pages alike. `op = "segment"`
/// ignores `field` and tests membership in the earlier segment named by `value`.
#[derive(Debug, Clone)]
pub struct SegmentRule {
    /// One of prefix, contains, eq, regex, in, segment.
    pub op: String,
    /// Record field, dotted paths allowed.
    pub field: String,
    /// Comparison operand.
    pub value: Value,
    regex: Option<Regex>,
}

impl SegmentRule {
    /// Validate the op and compile a regex now, not at the first match.
    pub fn new(op: &str, field: &str, value: Value) -> Result<Self, SegmentError> {
        if !VALID_OPS.contains(&op) {
            return Err(SegmentError(format!(
                "unknown segment rule op {}; expected one of ['contains', 'eq', 'in', 'prefix', 'regex', 'segment']",
                py_repr_value(&json!(op))
            )));
        }
        let regex = if op == "regex" {
            Some(
                Regex::new(&str_or_dumps(&value))
                    .map_err(|e| SegmentError(format!("invalid regex: {e}")))?,
            )
        } else {
            None
        };
        Ok(Self {
            op: op.to_owned(),
            field: field.to_owned(),
            value,
            regex,
        })
    }
}

/// A named set of rules.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Segment name.
    pub name: String,
    /// Rules, OR-ed.
    pub rules: Vec<SegmentRule>,
}

impl Segment {
    /// Names of the earlier segments this one references, first-seen order.
    pub fn depends_on(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for r in self.rules.iter().filter(|r| r.op == "segment") {
            let name = str_or_dumps(&r.value);
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
        seen
    }
}

fn coerce_segment(raw: &Value) -> Result<Segment, SegmentError> {
    let name = raw.get("name").filter(|n| crate::pyvalue::truthy(n));
    let Some(name) = name else {
        return Err(SegmentError(
            "a segment definition needs a non-empty 'name'".into(),
        ));
    };
    let name = str_or_dumps(name);
    let rules_raw = raw
        .get("rules")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| {
            SegmentError(format!(
                "segment {} has no rules; it would never match anything",
                py_repr_value(&json!(name))
            ))
        })?;
    let mut rules = Vec::new();
    for r in rules_raw {
        let op = r
            .get("op")
            .map_or_else(|| "prefix".to_owned(), str_or_dumps);
        let field = r
            .get("field")
            .map_or_else(|| "url".to_owned(), str_or_dumps);
        rules.push(SegmentRule::new(
            &op,
            &field,
            r.get("value").cloned().unwrap_or(Value::Null),
        )?);
    }
    Ok(Segment { name, rules })
}

/// A dotted path walks nested objects; a plain name tries the record, then `metrics`.
fn field_value<'a>(record: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    if path.contains('.') {
        let mut cur: &Value = &Value::Null;
        let mut first = true;
        for part in path.split('.') {
            let obj = if first { Some(record) } else { cur.as_object() };
            first = false;
            cur = obj?.get(part)?;
        }
        return Some(cur);
    }
    if let Some(v) = record.get(path) {
        return Some(v);
    }
    record
        .get("metrics")
        .and_then(Value::as_object)
        .and_then(|m| m.get(path))
}

pub(crate) fn rule_matches(
    rule: &SegmentRule,
    record: &Map<String, Value>,
    resolved: &HashMap<String, BTreeSet<String>>,
) -> bool {
    if rule.op == "segment" {
        return record
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| {
                resolved
                    .get(&str_or_dumps(&rule.value))
                    .is_some_and(|s| s.contains(url))
            });
    }
    let value = field_value(record, &rule.field);
    if rule.op == "in" {
        let probe = value.unwrap_or(&Value::Null);
        return match &rule.value {
            Value::Array(items) => items.iter().any(|i| py_eq(probe, i)),
            single => py_eq(probe, single),
        };
    }
    let text = match value {
        None | Some(Value::Null) => String::new(),
        Some(v) => str_or_dumps(v),
    };
    match rule.op.as_str() {
        "prefix" => text.starts_with(&str_or_dumps(&rule.value)),
        "contains" => text.contains(&str_or_dumps(&rule.value)),
        "eq" => py_eq(value.unwrap_or(&Value::Null), &rule.value),
        "regex" => rule.regex.as_ref().is_some_and(|re| re.is_match(&text)),
        _ => false,
    }
}

/// The evaluation order: a topological sort by `segment` references, ties
/// broken alphabetically so reordering the input never changes the result.
pub fn resolve_order(segments: &[Value]) -> Result<Vec<Segment>, SegmentError> {
    let list: Vec<Segment> = segments
        .iter()
        .map(coerce_segment)
        .collect::<Result<_, _>>()?;
    let mut by_name: BTreeMap<String, &Segment> = BTreeMap::new();
    for seg in &list {
        if by_name.insert(seg.name.clone(), seg).is_some() {
            return Err(SegmentError(format!(
                "duplicate segment name {}",
                py_repr_value(&json!(seg.name))
            )));
        }
    }
    for seg in &list {
        for dep in seg.depends_on() {
            if !by_name.contains_key(&dep) {
                return Err(SegmentError(format!(
                    "segment {} references unknown segment {}",
                    py_repr_value(&json!(seg.name)),
                    py_repr_value(&json!(dep))
                )));
            }
        }
    }
    let mut in_degree: BTreeMap<&str, usize> = by_name.keys().map(|k| (k.as_str(), 0)).collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> =
        by_name.keys().map(|k| (k.as_str(), Vec::new())).collect();
    for seg in &list {
        for dep in seg.depends_on() {
            *in_degree.get_mut(seg.name.as_str()).expect("registered") += 1;
            let dep_key = by_name
                .get_key_value(dep.as_str())
                .expect("validated")
                .0
                .as_str();
            dependents
                .get_mut(dep_key)
                .expect("registered")
                .push(&seg.name);
        }
    }
    let mut ready: BinaryHeap<Reverse<&str>> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| Reverse(*n))
        .collect();
    let mut order: Vec<&str> = Vec::new();
    while let Some(Reverse(name)) = ready.pop() {
        order.push(name);
        let mut deps = dependents[name].clone();
        deps.sort_unstable();
        for dependent in deps {
            let d = in_degree.get_mut(dependent).expect("registered");
            *d -= 1;
            if *d == 0 {
                ready.push(Reverse(dependent));
            }
        }
    }
    if order.len() != by_name.len() {
        let placed: BTreeSet<&str> = order.iter().copied().collect();
        let remaining: Vec<String> = by_name
            .keys()
            .filter(|n| !placed.contains(n.as_str()))
            .cloned()
            .collect();
        let cycle = find_cycle(&remaining, &by_name);
        return Err(SegmentError(format!(
            "circular segment dependency: {}",
            cycle.join(" -> ")
        )));
    }
    Ok(order.iter().map(|n| (*by_name[*n]).clone()).collect())
}

/// Name one concrete cycle among the nodes the sort could not place.
fn find_cycle(remaining: &[String], by_name: &BTreeMap<String, &Segment>) -> Vec<String> {
    fn dfs(
        name: &str,
        remaining: &BTreeSet<&str>,
        by_name: &BTreeMap<String, &Segment>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        path.push(name.to_owned());
        for dep in by_name[name].depends_on() {
            if !remaining.contains(dep.as_str()) {
                continue;
            }
            if let Some(i) = path.iter().position(|p| *p == dep) {
                let mut cycle = path[i..].to_vec();
                cycle.push(dep);
                return Some(cycle);
            }
            if let Some(found) = dfs(&dep, remaining, by_name, path) {
                return Some(found);
            }
        }
        path.pop();
        None
    }
    let set: BTreeSet<&str> = remaining.iter().map(String::as_str).collect();
    for name in &set {
        if let Some(found) = dfs(name, &set, by_name, &mut Vec::new()) {
            return found;
        }
    }
    remaining.to_vec()
}

/// Direct and dependency-required fields per segment, so a consumer holding a
/// page projection can tell an explicit null from a field it never retained.
pub fn required_fields(
    segments: &[Value],
) -> Result<BTreeMap<String, BTreeSet<String>>, SegmentError> {
    let mut required: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for segment in resolve_order(segments)? {
        let mut fields: BTreeSet<String> = segment
            .rules
            .iter()
            .filter(|r| r.op != "segment")
            .map(|r| r.field.clone())
            .collect();
        for dep in segment.depends_on() {
            fields.extend(required[&dep].iter().cloned());
        }
        required.insert(segment.name.clone(), fields);
    }
    Ok(required)
}

/// The resolved order, every membership, and each URL's first match.
pub fn assign_segments(pages: &[Value], segments: &[Value]) -> Result<Value, SegmentError> {
    let order = resolve_order(segments)?;
    let records: Vec<&Map<String, Value>> = pages.iter().filter_map(Value::as_object).collect();
    let url_of = |r: &Map<String, Value>| {
        r.get("url")
            .filter(|u| crate::pyvalue::truthy(u))
            .map(str_or_dumps)
    };
    let mut resolved: HashMap<String, BTreeSet<String>> = HashMap::new();
    for seg in &order {
        let members: BTreeSet<String> = records
            .iter()
            .filter(|r| url_of(r).is_some())
            .filter(|r| {
                seg.rules
                    .iter()
                    .any(|rule| rule_matches(rule, r, &resolved))
            })
            .filter_map(|r| url_of(r))
            .collect();
        resolved.insert(seg.name.clone(), members);
    }
    let mut memberships = Map::new();
    let mut primary = Map::new();
    for r in &records {
        let Some(url) = url_of(r) else { continue };
        let member_of: Vec<&str> = order
            .iter()
            .filter(|s| resolved[&s.name].contains(&url))
            .map(|s| s.name.as_str())
            .collect();
        primary.insert(
            url.clone(),
            member_of.first().map_or(Value::Null, |n| json!(n)),
        );
        memberships.insert(url, json!(member_of));
    }
    Ok(json!({
        "order": order.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        "memberships": memberships,
        "primary": primary,
    }))
}

/// Pages and issues broken down by primary segment; every one lands in exactly
/// one bucket, so the segmented counts sum to the ungrouped totals.
pub fn segment_report(
    pages: &[Value],
    issues: &[Value],
    segments: &[Value],
) -> Result<Value, SegmentError> {
    let assignment = assign_segments(pages, segments)?;
    let primary = assignment["primary"].as_object().expect("primary map");
    let mut names: Vec<String> = assignment["order"]
        .as_array()
        .expect("order")
        .iter()
        .map(|n| n.as_str().unwrap_or_default().to_owned())
        .collect();
    names.push(UNSEGMENTED.to_owned());

    let bucket = |url: Option<&str>| -> String {
        url.and_then(|u| primary.get(u))
            .and_then(Value::as_str)
            .unwrap_or(UNSEGMENTED)
            .to_owned()
    };
    let mut pages_by: BTreeMap<String, i64> = names.iter().map(|n| (n.clone(), 0)).collect();
    for page in pages {
        let url = page.get("url").and_then(Value::as_str);
        *pages_by.entry(bucket(url)).or_insert(0) += 1;
    }
    let mut issues_by: BTreeMap<String, BTreeMap<String, i64>> =
        names.iter().map(|n| (n.clone(), BTreeMap::new())).collect();
    for issue in issues {
        let url = issue.get("target_url").and_then(Value::as_str);
        let check = issue.get("check").map_or_else(String::new, str_or_dumps);
        *issues_by
            .entry(bucket(url))
            .or_default()
            .entry(check)
            .or_insert(0) += 1;
    }
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "order": assignment["order"],
        "memberships": assignment["memberships"],
        "pages_by_segment": pages_by,
        "issues_by_segment": issues_by,
        "totals": {"pages": pages.len(), "issues": issues.len()},
    }))
}
