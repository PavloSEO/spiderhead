//! Structural JSON diff between reference baseline baselines and Rust output.
//!
//! `diff(expected, actual, opts)` yields one [`Divergence`] per differing leaf,
//! addressed by a JSON pointer. [`KnownDivergences`] (from `oracle/known-divergences.toml`)
//! filters approved differences and reports registry entries that matched nothing.

use serde::Deserialize;
use serde_json::Value;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Present in expected, absent in actual.
    Missing,
    /// Absent in expected, present in actual.
    Extra,
    /// Different JSON types.
    Type,
    /// Same type, different value.
    Value,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Missing => "missing",
            Kind::Extra => "extra",
            Kind::Type => "type",
            Kind::Value => "value",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Divergence {
    /// JSON pointer (`/pages/3/meta/title`); empty string = root.
    pub path: String,
    pub kind: Kind,
    pub expected: Option<Value>,
    pub actual: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Relative float tolerance; 0.0 = exact.
    pub float_tolerance: f64,
    /// Path patterns whose arrays are compared as multisets (order ignored).
    pub set_paths: Vec<String>,
    /// Path patterns whose subtrees are skipped entirely.
    pub ignore_paths: Vec<String>,
}

/// Pattern matching: JSON pointer where a `*` segment matches any single segment.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let mut p = pattern.split('/');
    let mut a = path.split('/');
    loop {
        match (p.next(), a.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x == "*" || x == y => {}
            _ => return false,
        }
    }
}

fn esc(seg: &str) -> String {
    seg.replace('~', "~0").replace('/', "~1")
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn nums_equal(a: &serde_json::Number, b: &serde_json::Number, tol: f64) -> bool {
    if a == b {
        return true;
    }
    if a.is_f64() || b.is_f64() {
        let (x, y) = (
            a.as_f64().unwrap_or(f64::NAN),
            b.as_f64().unwrap_or(f64::NAN),
        );
        if x == y {
            return true;
        }
        if tol > 0.0 {
            return (x - y).abs() <= tol * x.abs().max(y.abs());
        }
    }
    false
}

/// Compare `expected` (reference baseline) against `actual` (Rust output).
pub fn diff(expected: &Value, actual: &Value, opts: &Options) -> Vec<Divergence> {
    let mut out = Vec::new();
    walk(expected, actual, "", opts, &mut out);
    out
}

fn push(out: &mut Vec<Divergence>, path: &str, kind: Kind, e: Option<&Value>, a: Option<&Value>) {
    out.push(Divergence {
        path: path.to_string(),
        kind,
        expected: e.cloned(),
        actual: a.cloned(),
    });
}

fn walk(e: &Value, a: &Value, path: &str, opts: &Options, out: &mut Vec<Divergence>) {
    if opts.ignore_paths.iter().any(|p| path_matches(p, path)) {
        return;
    }
    match (e, a) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, ev) in x {
                let p = format!("{path}/{}", esc(k));
                match y.get(k) {
                    Some(av) => walk(ev, av, &p, opts, out),
                    None if opts.ignore_paths.iter().any(|i| path_matches(i, &p)) => {}
                    None => push(out, &p, Kind::Missing, Some(ev), None),
                }
            }
            for (k, av) in y {
                if !x.contains_key(k) {
                    let p = format!("{path}/{}", esc(k));
                    if !opts.ignore_paths.iter().any(|i| path_matches(i, &p)) {
                        push(out, &p, Kind::Extra, None, Some(av));
                    }
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if opts.set_paths.iter().any(|p| path_matches(p, path)) {
                walk_set(x, y, path, opts, out);
                return;
            }
            for i in 0..x.len().max(y.len()) {
                let p = format!("{path}/{i}");
                match (x.get(i), y.get(i)) {
                    (Some(ev), Some(av)) => walk(ev, av, &p, opts, out),
                    (Some(ev), None) => push(out, &p, Kind::Missing, Some(ev), None),
                    (None, Some(av)) => push(out, &p, Kind::Extra, None, Some(av)),
                    (None, None) => {}
                }
            }
        }
        (Value::Number(x), Value::Number(y)) => {
            if !nums_equal(x, y, opts.float_tolerance) {
                push(out, path, Kind::Value, Some(e), Some(a));
            }
        }
        _ if type_name(e) != type_name(a) => push(out, path, Kind::Type, Some(e), Some(a)),
        _ => {
            if e != a {
                push(out, path, Kind::Value, Some(e), Some(a));
            }
        }
    }
}

fn walk_set(x: &[Value], y: &[Value], path: &str, opts: &Options, out: &mut Vec<Divergence>) {
    let mut used = vec![false; y.len()];
    for (i, ev) in x.iter().enumerate() {
        let hit = (0..y.len()).find(|&j| !used[j] && diff(ev, &y[j], opts).is_empty());
        match hit {
            Some(j) => used[j] = true,
            None => push(out, &format!("{path}/{i}"), Kind::Missing, Some(ev), None),
        }
    }
    for (j, av) in y.iter().enumerate() {
        if !used[j] {
            push(out, &format!("{path}/{j}"), Kind::Extra, None, Some(av));
        }
    }
}

// ---------------------------------------------------------------- registry

/// One approved Rust-vs-oracle divergence, as written in `oracle/known-divergences.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct KnownEntry {
    /// Unique, never reused (`D-0001`).
    pub id: String,
    /// Golden-set site (or slice) the entry applies to; `*` = any.
    #[serde(default = "any")]
    pub site: String,
    /// Baseline file (or check id) the entry applies to; `*` = any. `check` is an alias.
    #[serde(alias = "check")]
    pub file: String,
    /// JSON-pointer pattern (`*` segment = any index/key).
    pub path: String,
    /// Input or snippet that triggers the difference.
    #[serde(default)]
    pub example: String,
    /// What the oracle produced.
    #[serde(default)]
    pub python_value: String,
    /// What Rust produces.
    #[serde(default)]
    pub rust_value: String,
    /// Why the difference is accepted.
    pub reason: String,
    /// Reviewer who approved it.
    pub approved_by: String,
    /// Approval date, `YYYY-MM-DD`.
    pub approved_on: String,
}

fn any() -> String {
    "*".into()
}

/// The registry of approved divergences. An empty file is a valid, empty registry.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KnownDivergences {
    #[serde(default, rename = "divergence")]
    pub entries: Vec<KnownEntry>,
}

impl KnownDivergences {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let k: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        for (i, e) in k.entries.iter().enumerate() {
            let blank = [&e.id, &e.reason, &e.approved_by, &e.approved_on]
                .iter()
                .any(|v| v.trim().is_empty());
            if blank {
                return Err(format!(
                    "divergence #{i} ({} {}): id, reason, approved_by and approved_on are required",
                    e.file, e.path
                ));
            }
            if k.entries[..i].iter().any(|o| o.id == e.id) {
                return Err(format!("divergence #{i}: duplicate id {}", e.id));
            }
        }
        Ok(k)
    }

    fn find(&self, site: &str, file: &str, path: &str) -> Option<usize> {
        self.entries.iter().position(|e| {
            (e.site == "*" || e.site == site)
                && (e.file == "*" || e.file == file)
                && path_matches(&e.path, path)
        })
    }

    /// Split `found` into unapproved divergences; mark matching entries in `used`.
    pub fn filter(
        &self,
        site: &str,
        file: &str,
        found: Vec<Divergence>,
        used: &mut Vec<bool>,
    ) -> Vec<Divergence> {
        used.resize(self.entries.len(), false);
        found
            .into_iter()
            .filter(|d| match self.find(site, file, &d.path) {
                Some(i) => {
                    used[i] = true;
                    false
                }
                None => true,
            })
            .collect()
    }

    /// Entries never matched in a run (stale registry rows).
    pub fn stale<'a>(&'a self, used: &[bool]) -> Vec<&'a KnownEntry> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(i, _)| !used.get(*i).copied().unwrap_or(false))
            .map(|(_, e)| e)
            .collect()
    }
}

// ------------------------------------------------------------------ report

fn short(v: &Option<Value>) -> String {
    match v {
        None => "<absent>".into(),
        Some(v) => {
            let s = v.to_string();
            if s.chars().count() > 120 {
                format!("{}...", s.chars().take(120).collect::<String>())
            } else {
                s
            }
        }
    }
}

/// Human-readable report: first `limit` divergences with path and both values, then totals.
pub fn report(divs: &[Divergence], limit: usize) -> String {
    let mut s = String::new();
    for d in divs.iter().take(limit) {
        let _ = writeln!(
            s,
            "  [{}] {}\n      expected: {}\n      actual:   {}",
            d.kind.as_str(),
            if d.path.is_empty() { "/" } else { &d.path },
            short(&d.expected),
            short(&d.actual)
        );
    }
    if divs.len() > limit {
        let _ = writeln!(s, "  ... {} more not shown", divs.len() - limit);
    }
    let count = |k| divs.iter().filter(|d| d.kind == k).count();
    let _ = writeln!(
        s,
        "total: {} (missing {}, extra {}, type {}, value {})",
        divs.len(),
        count(Kind::Missing),
        count(Kind::Extra),
        count(Kind::Type),
        count(Kind::Value)
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(e: Value, a: Value) -> Vec<Divergence> {
        diff(&e, &a, &Options::default())
    }

    #[test]
    fn equal_is_empty() {
        let v = json!({"a":[1,{"b":null}],"c":"x"});
        assert!(d(v.clone(), v).is_empty());
    }

    #[test]
    fn nested_value_path() {
        let r = d(
            json!({"pages":[{"meta":{"title":"A"}},{"meta":{"title":"B"}}]}),
            json!({"pages":[{"meta":{"title":"A"}},{"meta":{"title":"C"}}]}),
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].path, "/pages/1/meta/title");
        assert_eq!(r[0].kind, Kind::Value);
        assert_eq!(r[0].expected, Some(json!("B")));
        assert_eq!(r[0].actual, Some(json!("C")));
    }

    #[test]
    fn missing_and_extra_keys() {
        let r = d(json!({"a":1,"b":2}), json!({"a":1,"c":3}));
        assert_eq!(r.len(), 2);
        assert!(r.iter().any(|x| x.path == "/b" && x.kind == Kind::Missing));
        assert!(r.iter().any(|x| x.path == "/c" && x.kind == Kind::Extra));
    }

    #[test]
    fn type_mismatch() {
        let r = d(json!({"a":"1"}), json!({"a":1}));
        assert_eq!(r[0].kind, Kind::Type);
        let r = d(json!({"a":null}), json!({"a":{}}));
        assert_eq!(r[0].kind, Kind::Type);
    }

    #[test]
    fn array_length_and_order() {
        let r = d(json!([1, 2, 3]), json!([1, 2]));
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].path.as_str(), r[0].kind), ("/2", Kind::Missing));
        let r = d(json!([1, 2]), json!([1, 2, 9]));
        assert_eq!((r[0].path.as_str(), r[0].kind), ("/2", Kind::Extra));
        assert_eq!(d(json!([1, 2]), json!([2, 1])).len(), 2);
    }

    #[test]
    fn pointer_escaping() {
        let r = d(json!({"a/b":1,"c~d":1}), json!({"a/b":2,"c~d":2}));
        let paths: Vec<_> = r.iter().map(|x| x.path.as_str()).collect();
        assert!(paths.contains(&"/a~1b") && paths.contains(&"/c~0d"));
    }

    #[test]
    fn floats_exact_and_tolerant() {
        assert_eq!(d(json!({"x":1.0}), json!({"x":1.0000001})).len(), 1);
        let o = Options {
            float_tolerance: 1e-6,
            ..Default::default()
        };
        assert!(diff(&json!({"x":1.0}), &json!({"x":1.0000001}), &o).is_empty());
        assert_eq!(diff(&json!({"x":1.0}), &json!({"x":1.001}), &o).len(), 1);
        // int vs float with same value
        assert!(d(json!(1), json!(1.0)).is_empty());
        // tolerance never applies to two integers
        assert_eq!(diff(&json!(1000000), &json!(1000001), &o).len(), 1);
    }

    #[test]
    fn set_paths_ignore_order() {
        let o = Options {
            set_paths: vec!["/tags".into()],
            ..Default::default()
        };
        let e = json!({"tags":["a","b","c"],"o":[1,2]});
        let a = json!({"tags":["c","a","b"],"o":[1,2]});
        assert!(diff(&e, &a, &o).is_empty());
        let a2 = json!({"tags":["c","a","z"],"o":[1,2]});
        let r = diff(&e, &a2, &o);
        assert_eq!(r.len(), 2);
        assert!(r
            .iter()
            .any(|x| x.kind == Kind::Missing && x.expected == Some(json!("b"))));
        assert!(r
            .iter()
            .any(|x| x.kind == Kind::Extra && x.actual == Some(json!("z"))));
    }

    #[test]
    fn set_paths_wildcard_and_duplicates() {
        let o = Options {
            set_paths: vec!["/p/*/t".into()],
            ..Default::default()
        };
        let e = json!({"p":[{"t":[1,1,2]},{"t":[3]}]});
        assert!(diff(&e, &json!({"p":[{"t":[2,1,1]},{"t":[3]}]}), &o).is_empty());
        // multiset: duplicate count matters
        assert_eq!(
            diff(&e, &json!({"p":[{"t":[2,1,2]},{"t":[3]}]}), &o).len(),
            2
        );
    }

    #[test]
    fn ignore_paths() {
        let o = Options {
            ignore_paths: vec!["/ts".into(), "/pages/*/id".into()],
            ..Default::default()
        };
        let e = json!({"ts":1,"pages":[{"id":1,"u":"a"}]});
        let a = json!({"ts":2,"pages":[{"id":9,"u":"a"}],});
        assert!(diff(&e, &a, &o).is_empty());
        // ignored key absent on one side
        assert!(diff(&e, &json!({"pages":[{"u":"a"}]}), &o).is_empty());
        assert!(diff(
            &json!({"pages":[{"u":"a"}]}),
            &json!({"ts":3,"pages":[{"id":5,"u":"a"}]}),
            &o
        )
        .is_empty());
        // non-ignored still reported
        assert_eq!(
            diff(&e, &json!({"ts":2,"pages":[{"id":9,"u":"b"}]}), &o).len(),
            1
        );
    }

    #[test]
    fn path_pattern_matching() {
        assert!(path_matches("/a/*/c", "/a/3/c"));
        assert!(!path_matches("/a/*/c", "/a/3/c/d"));
        assert!(!path_matches("/a/*", "/a"));
        assert!(path_matches("", ""));
    }

    const REG: &str = r#"
[[divergence]]
id = "D-0001"
file = "pages.json"
path = "/pages/*/ts"
example = "/pages/0/ts"
reason = "timestamp format"
approved_by = "pavel"
approved_on = "2026-10-27"

[[divergence]]
id = "D-0002"
site = "*"
check = "*"
path = "/never"
reason = "gone"
approved_by = "pavel"
approved_on = "2026-10-27"
"#;

    #[test]
    fn known_divergences_filter_and_stale() {
        let k = KnownDivergences::from_toml(REG).unwrap();
        let found = d(
            json!({"pages":[{"ts":1,"u":"a"}]}),
            json!({"pages":[{"ts":2,"u":"b"}]}),
        );
        assert_eq!(found.len(), 2);
        let mut used = vec![];
        let left = k.filter("s", "pages.json", found.clone(), &mut used);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].path, "/pages/0/u");
        let stale = k.stale(&used);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].path, "/never");
        // different check id: nothing filtered
        let mut used2 = vec![];
        assert_eq!(k.filter("s", "other.json", found, &mut used2).len(), 2);
        assert_eq!(k.stale(&used2).len(), 2);
    }

    #[test]
    fn registry_requires_reason_and_approver() {
        let entry = |id: &str, reason: &str, on: &str| {
            format!(
                "[[divergence]]\nid=\"{id}\"\nfile=\"a\"\npath=\"/x\"\nreason=\"{reason}\"\napproved_by=\"p\"\napproved_on=\"{on}\"\n"
            )
        };
        assert!(KnownDivergences::from_toml(&entry("D-1", "r", "2026-10-27")).is_ok());
        assert!(KnownDivergences::from_toml(&entry("D-1", "", "2026-10-27")).is_err());
        assert!(KnownDivergences::from_toml(&entry("D-1", "r", "")).is_err());
        let dup = format!("{}{}", entry("D-1", "r", "d"), entry("D-1", "r", "d"));
        assert!(KnownDivergences::from_toml(&dup).is_err());
        assert!(KnownDivergences::from_toml("").unwrap().entries.is_empty());
        assert!(KnownDivergences::from_toml("[[divergence]]\nfile=\"a\"").is_err());
    }

    #[test]
    fn site_scoped_entry_only_matches_its_site() {
        let toml = "[[divergence]]\nid=\"D-1\"\nsite=\"basic\"\nfile=\"*\"\npath=\"/a\"\nreason=\"r\"\napproved_by=\"p\"\napproved_on=\"2026-10-27\"\n";
        let k = KnownDivergences::from_toml(toml).unwrap();
        let found = d(json!({"a":1}), json!({"a":2}));
        assert!(k
            .filter("basic", "x.json", found.clone(), &mut vec![])
            .is_empty());
        assert_eq!(k.filter("other", "x.json", found, &mut vec![]).len(), 1);
    }

    /// The real registry parses (it is empty until a reviewed commit adds entries) and every
    /// entry it holds satisfies the schema.
    #[test]
    fn real_registry_file_parses() {
        let text = include_str!("../../known-divergences.toml");
        let k = KnownDivergences::from_toml(text).expect("oracle/known-divergences.toml is valid");
        assert!(k.entries.iter().all(|e| e.approved_on.len() == 10));
    }

    #[test]
    fn report_limits_and_totals() {
        let r = d(json!([1, 2, 3, 4]), json!([9, 9, 9, 9]));
        let s = report(&r, 2);
        assert!(s.contains("[value] /0") && s.contains("expected: 1") && s.contains("actual:   9"));
        assert!(s.contains("... 2 more not shown"));
        assert!(s.contains("total: 4 (missing 0, extra 0, type 0, value 4)"));
        assert!(report(&[], 5).contains("total: 0"));
    }

    #[test]
    fn root_level_divergence() {
        let r = d(json!(1), json!("x"));
        assert_eq!(r[0].path, "");
        assert!(report(&r, 5).contains("] /"));
    }
}
