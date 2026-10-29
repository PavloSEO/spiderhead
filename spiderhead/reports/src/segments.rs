//! Segment declaration and assignment: named subsets of a crawl, reported on
//! separately rather than folded into one averaged number.
//!
//! The rule engine is kept local: the analysis crate is not a dependency of this crate.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{json, Map, Value};

/// Schema marker for the segment report result.
pub const SCHEMA_VERSION: &str = "segments.v1";
/// Explicit bucket for pages matching no segment.
pub const UNSEGMENTED: &str = "unsegmented";

const VALID_OPS: [&str; 6] = ["prefix", "contains", "eq", "regex", "in", "segment"];

/// A segment set cannot be evaluated as declared.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SegmentError(pub String);

/// One condition. A segment matches a page if any of its rules do (OR).
#[derive(Debug, Clone)]
pub struct SegmentRule {
    /// `prefix`/`contains`/`eq`/`regex`/`in`/`segment`.
    pub op: String,
    /// Page-record field path (falls back to `metrics.<field>`).
    pub field: String,
    /// Comparison value (segment name for `segment` rules).
    pub value: Value,
}

impl SegmentRule {
    /// `SegmentRule.__post_init__` validation.
    pub fn new(op: &str, field: &str, value: Value) -> Result<Self, SegmentError> {
        if !VALID_OPS.contains(&op) {
            return Err(SegmentError(format!(
                "unknown segment rule op {:?}; expected one of {:?}",
                op, VALID_OPS
            )));
        }
        if op == "regex" {
            let pattern = crate::py::str_value(&value);
            if regex::Regex::new(&pattern).is_err() {
                // Only compilability is checked; a bad pattern never reaches matching.
            }
            regex::Regex::new(&pattern).map_err(|e| SegmentError(e.to_string()))?;
        }
        Ok(Self {
            op: op.to_owned(),
            field: field.to_owned(),
            value,
        })
    }
}

/// A named rule set.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Segment name.
    pub name: String,
    /// OR-ed rules.
    pub rules: Vec<SegmentRule>,
}

impl Segment {
    /// Names of earlier segments this segment references.
    pub fn depends_on(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for rule in &self.rules {
            if rule.op == "segment" {
                let name = crate::py::str_value(&rule.value);
                if !seen.contains(&name) {
                    seen.push(name);
                }
            }
        }
        seen
    }
}

fn coerce_rule(raw: &Value) -> Result<SegmentRule, SegmentError> {
    SegmentRule::new(
        raw.get("op").and_then(Value::as_str).unwrap_or("prefix"),
        raw.get("field").and_then(Value::as_str).unwrap_or("url"),
        raw.get("value").cloned().unwrap_or(Value::Null),
    )
}

fn coerce_segment(raw: &Value) -> Result<Segment, SegmentError> {
    let name = raw.get("name").and_then(Value::as_str).unwrap_or_default();
    if name.is_empty() {
        return Err(SegmentError(
            "a segment definition needs a non-empty 'name'".into(),
        ));
    }
    let rules_raw = raw.get("rules").and_then(Value::as_array);
    let rules_raw = match rules_raw {
        Some(r) if !r.is_empty() => r,
        _ => {
            return Err(SegmentError(format!(
                "segment {name:?} has no rules; it would never match anything"
            )))
        }
    };
    let mut rules = Vec::with_capacity(rules_raw.len());
    for item in rules_raw {
        rules.push(coerce_rule(item)?);
    }
    Ok(Segment {
        name: name.to_owned(),
        rules,
    })
}

/// Field value: dotted paths first, then the `metrics.<field>` fallback.
pub fn field_value(record: &Value, field_path: &str) -> Option<Value> {
    if field_path.contains('.') {
        let mut value = record;
        for part in field_path.split('.') {
            value = value.get(part)?;
        }
        return Some(value.clone());
    }
    if let Some(value) = record.get(field_path) {
        return Some(value.clone());
    }
    record
        .get("metrics")
        .and_then(|m| m.get(field_path))
        .cloned()
}

/// Membership lookup shared between the in-memory and SQLite evaluators.
pub trait Memberships {
    /// Whether `url` is a member of segment `name`.
    fn contains(&self, name: &str, url: &str) -> bool;
    /// Fields the segment could not evaluate for `url` (unknown state).
    fn unknown_fields(&self, name: &str, url: &str) -> BTreeSet<String> {
        let _ = (name, url);
        BTreeSet::new()
    }
}

/// In-memory memberships (`assign_segments` shape).
pub struct MapMemberships<'a>(pub &'a HashMap<String, HashSet<String>>);

impl Memberships for MapMemberships<'_> {
    fn contains(&self, name: &str, url: &str) -> bool {
        self.0.get(name).is_some_and(|set| set.contains(url))
    }
}

/// `rule_matches`.
pub fn rule_matches(
    rule: &SegmentRule,
    record: &Value,
    resolved: &dyn Memberships,
) -> Result<bool, SegmentError> {
    if rule.op == "segment" {
        let url = record
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Ok(resolved.contains(&crate::py::str_value(&rule.value), url));
    }
    let value = field_value(record, &rule.field);
    if rule.op == "in" {
        let container: Vec<Value> = match &rule.value {
            Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        };
        return Ok(container.iter().any(|item| Some(item.clone()) == value));
    }
    let text = value.as_ref().map(crate::py::str_value).unwrap_or_default();
    match rule.op.as_str() {
        "prefix" => Ok(text.starts_with(&crate::py::str_value(&rule.value))),
        "contains" => Ok(text.contains(&crate::py::str_value(&rule.value))),
        "eq" => Ok(value == Some(rule.value.clone())),
        "regex" => {
            let pattern = crate::py::str_value(&rule.value);
            match regex::Regex::new(&pattern) {
                Ok(re) => Ok(re.is_match(&text)),
                Err(e) => Err(SegmentError(e.to_string())),
            }
        }
        _ => Err(SegmentError(format!(
            "unknown segment rule op {:?}",
            rule.op
        ))),
    }
}

fn segment_matches(
    segment: &Segment,
    record: &Value,
    resolved: &dyn Memberships,
) -> Result<bool, SegmentError> {
    for rule in &segment.rules {
        if rule_matches(rule, record, resolved)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `resolve_order` — topological sort, alphabetical tie-break.
pub fn resolve_order(segments: &[Value]) -> Result<Vec<Segment>, SegmentError> {
    let mut resolved = Vec::with_capacity(segments.len());
    for raw in segments {
        resolved.push(coerce_segment(raw)?);
    }
    let mut by_name: BTreeMap<String, Segment> = BTreeMap::new();
    for seg in &resolved {
        if by_name.contains_key(&seg.name) {
            return Err(SegmentError(format!(
                "duplicate segment name {:?}",
                seg.name
            )));
        }
        by_name.insert(seg.name.clone(), seg.clone());
    }
    for seg in &resolved {
        for dep in seg.depends_on() {
            if !by_name.contains_key(&dep) {
                return Err(SegmentError(format!(
                    "segment {:?} references unknown segment {:?}",
                    seg.name, dep
                )));
            }
        }
    }
    let mut in_degree: HashMap<String, usize> = by_name.keys().map(|n| (n.clone(), 0)).collect();
    let mut dependents: HashMap<String, Vec<String>> =
        by_name.keys().map(|n| (n.clone(), Vec::new())).collect();
    for seg in &resolved {
        for dep in seg.depends_on() {
            *in_degree.get_mut(&seg.name).unwrap_or(&mut 0) += 1;
            dependents.entry(dep).or_default().push(seg.name.clone());
        }
    }
    let mut ready: Vec<String> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| n.clone())
        .collect();
    ready.sort();
    let mut order = Vec::with_capacity(ready.len());
    while let Some(name) = ready.first().cloned() {
        ready.remove(0);
        order.push(name.clone());
        let mut deps = dependents.get(&name).cloned().unwrap_or_default();
        deps.sort();
        for dependent in deps {
            let degree = in_degree.get_mut(&dependent).unwrap();
            *degree -= 1;
            if *degree == 0 {
                ready.push(dependent);
                ready.sort();
            }
        }
    }
    if order.len() != by_name.len() {
        let remaining: Vec<String> = by_name
            .keys()
            .filter(|n| !order.contains(*n))
            .cloned()
            .collect();
        let cycle = find_cycle(&remaining, &by_name);
        return Err(SegmentError(format!(
            "circular segment dependency: {}",
            cycle.join(" -> ")
        )));
    }
    Ok(order
        .iter()
        .map(|name| by_name.get(name).unwrap().clone())
        .collect())
}

/// `required_fields` — direct + dependency fields per segment, keyed by name.
pub fn required_fields(
    segments: &[Value],
) -> Result<HashMap<String, HashSet<String>>, SegmentError> {
    let mut required: HashMap<String, HashSet<String>> = HashMap::new();
    for segment in resolve_order(segments)? {
        let mut fields: HashSet<String> = segment
            .rules
            .iter()
            .filter(|r| r.op != "segment")
            .map(|r| r.field.clone())
            .collect();
        for dep in segment.depends_on() {
            if let Some(dep_fields) = required.get(&dep) {
                fields.extend(dep_fields.iter().cloned());
            }
        }
        required.insert(segment.name.clone(), fields);
    }
    Ok(required)
}

/// `required_fields` over an already-resolved order (`PrimarySegmentIndex`).
pub fn required_fields_ordered(order: &[Segment]) -> HashMap<String, HashSet<String>> {
    let mut required: HashMap<String, HashSet<String>> = HashMap::new();
    for segment in order {
        let mut fields: HashSet<String> = segment
            .rules
            .iter()
            .filter(|r| r.op != "segment")
            .map(|r| r.field.clone())
            .collect();
        for dep in segment.depends_on() {
            if let Some(dep_fields) = required.get(&dep) {
                fields.extend(dep_fields.iter().cloned());
            }
        }
        required.insert(segment.name.clone(), fields);
    }
    required
}

fn find_cycle(remaining: &[String], by_name: &BTreeMap<String, Segment>) -> Vec<String> {
    let remaining_set: HashSet<&String> = remaining.iter().collect();
    let mut path: Vec<String> = Vec::new();
    let mut on_path: HashSet<String> = HashSet::new();

    fn dfs(
        name: &str,
        by_name: &BTreeMap<String, Segment>,
        remaining_set: &HashSet<&String>,
        path: &mut Vec<String>,
        on_path: &mut HashSet<String>,
    ) -> Option<Vec<String>> {
        path.push(name.to_owned());
        on_path.insert(name.to_owned());
        if let Some(seg) = by_name.get(name) {
            for dep in seg.depends_on() {
                if !remaining_set.contains(&dep) {
                    continue;
                }
                if on_path.contains(&dep) {
                    let start = path.iter().position(|p| *p == dep).unwrap_or(0);
                    let mut cycle: Vec<String> = path[start..].to_vec();
                    cycle.push(dep);
                    return Some(cycle);
                }
                if let Some(found) = dfs(&dep, by_name, remaining_set, path, on_path) {
                    return Some(found);
                }
            }
        }
        path.pop();
        on_path.remove(name);
        None
    }

    let mut sorted: Vec<&String> = remaining.iter().collect();
    sorted.sort();
    for name in sorted {
        if let Some(found) = dfs(name, by_name, &remaining_set, &mut path, &mut on_path) {
            return found;
        }
    }
    remaining.to_vec()
}

/// `assign_segments` — order + memberships + primary per URL.
pub fn assign_segments(pages: &[Value], segments: &[Value]) -> Result<Value, SegmentError> {
    assign_segments_ordered(pages, &resolve_order(segments)?)
}

/// `assign_segments` over an already-resolved order.
pub fn assign_segments_ordered(pages: &[Value], order: &[Segment]) -> Result<Value, SegmentError> {
    let mut resolved: HashMap<String, HashSet<String>> = HashMap::new();
    for seg in order.iter() {
        let mut members = HashSet::new();
        for page in pages {
            if page.get("url").and_then(Value::as_str).is_some()
                && segment_matches(seg, page, &MapMemberships(&resolved))?
            {
                members.insert(page.get("url").and_then(Value::as_str).unwrap().to_owned());
            }
        }
        resolved.insert(seg.name.clone(), members);
    }
    let mut memberships = Map::new();
    let mut primary = Map::new();
    for page in pages {
        let Some(url) = page.get("url").and_then(Value::as_str) else {
            continue;
        };
        let member_of: Vec<String> = order
            .iter()
            .filter(|seg| resolved.get(&seg.name).is_some_and(|s| s.contains(url)))
            .map(|seg| seg.name.clone())
            .collect();
        primary.insert(
            url.to_owned(),
            member_of.first().map(|s| json!(s)).unwrap_or(Value::Null),
        );
        memberships.insert(url.to_owned(), json!(member_of));
    }
    Ok(json!({
        "order": order.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        "memberships": memberships,
        "primary": primary,
    }))
}

/// `segment_report` — pages and issues attributed to exactly one bucket.
pub fn segment_report(
    pages: &[Value],
    issues: &[Value],
    segments: &[Value],
) -> Result<Value, SegmentError> {
    let assignment = assign_segments(pages, segments)?;
    let primary = assignment
        .get("primary")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut names: Vec<String> = assignment
        .get("order")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    names.push(UNSEGMENTED.to_owned());
    let mut pages_by_segment: Map<String, Value> =
        names.iter().map(|n| (n.clone(), json!(0))).collect();
    for page in pages {
        let url = page.get("url").and_then(Value::as_str).unwrap_or_default();
        let bucket = primary
            .get(url)
            .and_then(Value::as_str)
            .unwrap_or(UNSEGMENTED);
        let entry = pages_by_segment
            .entry(bucket.to_owned())
            .or_insert(json!(0));
        *entry = json!(entry.as_i64().unwrap_or_default() + 1);
    }
    let mut issues_by_segment: Map<String, Value> =
        names.iter().map(|n| (n.clone(), json!({}))).collect();
    for issue in issues {
        let url = issue
            .get("target_url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let bucket = primary
            .get(url)
            .and_then(Value::as_str)
            .unwrap_or(UNSEGMENTED);
        let row = issues_by_segment
            .entry(bucket.to_owned())
            .or_insert(json!({}));
        let check = issue
            .get("check")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(obj) = row.as_object_mut() {
            let count = obj.get(&check).and_then(Value::as_i64).unwrap_or_default();
            obj.insert(check, json!(count + 1));
        }
    }
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "order": assignment.get("order").cloned().unwrap_or(json!([])),
        "memberships": assignment.get("memberships").cloned().unwrap_or(json!({})),
        "pages_by_segment": pages_by_segment,
        "issues_by_segment": issues_by_segment,
        "totals": {"pages": pages.len(), "issues": issues.len()},
    }))
}
