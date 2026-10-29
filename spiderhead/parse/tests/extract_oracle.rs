//! Differential tests: duplicate, boilerplate_report, markdown_extract, extraction_rules,
//! custom_search, custom_extract and schema_org against the reference outputs recorded in
//! `fixtures/extract_oracle.json` (generator the reference suite). Each module test
//! asserts zero mismatches; `EXTRACT_SHOW` sets how many divergences are printed.

use std::sync::OnceLock;

use serde_json::{json, Value};
use spiderhead_parse::boilerplate_report::{
    boilerplate_consistency_report, boilerplate_hash, BoilerplatePage,
};
use spiderhead_parse::custom_extract::{check_sample, run_extraction, ExtractorSpec};
use spiderhead_parse::custom_search::{run_filter, run_search, FilterSpec};
use spiderhead_parse::duplicate::{
    content_hash, find_duplicates, fnv1a_64, hamming, is_word_char, simhash, similarity, tokenize,
    DuplicateItem, DuplicateOptions,
};
use spiderhead_parse::extraction_rules::{evaluate, validate_result, validate_rules};
use spiderhead_parse::markdown_extract::extract_markdown;
use spiderhead_parse::schema_org::{
    check_schema, json_syntax_hint, normalize_jsonld_ids, type_chain, SchemaSource,
};
use spiderhead_parse::types::{ContentAreaConfig, CorpusDocument};

fn fixture() -> &'static Value {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let text = match std::env::var("EXTRACT_FIXTURE") {
            Ok(path) => std::fs::read_to_string(path).unwrap_or_default(),
            Err(_) => include_str!("fixtures/extract_oracle.json").to_owned(),
        };
        serde_json::from_str(&text).unwrap_or(Value::Null)
    })
}

fn doc(name: &str) -> &'static str {
    fixture()["docs"][name].as_str().unwrap_or_default()
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 300 {
        format!("{}...", text.chars().take(300).collect::<String>())
    } else {
        text
    }
}

fn walk(expected: &Value, actual: &Value, path: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(x), Value::Object(y)) => {
            for (key, ev) in x {
                match y.get(key) {
                    Some(av) => walk(ev, av, &format!("{path}/{key}"), out),
                    None => out.push(format!("{path}/{key}: missing, want {}", short(ev))),
                }
            }
            for (key, av) in y {
                if !x.contains_key(key) {
                    out.push(format!("{path}/{key}: extra {}", short(av)));
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!(
                    "{path}: length want {} got {}\n    want {}\n    got  {}",
                    x.len(),
                    y.len(),
                    short(expected),
                    short(actual)
                ));
                return;
            }
            for (i, (ev, av)) in x.iter().zip(y).enumerate() {
                walk(ev, av, &format!("{path}/{i}"), out);
            }
        }
        (Value::Number(x), Value::Number(y)) if x.as_f64() == y.as_f64() => {}
        _ if expected == actual => {}
        _ => out.push(format!(
            "{path}: want {} got {}",
            short(expected),
            short(actual)
        )),
    }
}

/// Error comparison: the class prefix before `: ` and, for verbatim contracts, the message.
fn error_matches(expected: &str, actual: &str) -> bool {
    let (class, message) = expected.split_once(": ").unwrap_or((expected, ""));
    match class {
        "ValueError"
            if message.starts_with("invalid regex")
                || message.starts_with("invalid CSS selector") =>
        {
            let head = message.split(": ").next().unwrap_or("");
            actual.starts_with(head)
        }
        "ValueError" | "DuplicateBudgetExceeded" => actual == message,
        "XPathEvalError" | "XPathSyntaxError" => actual == expected || actual == message,
        _ => true,
    }
}

struct Check {
    bad: Vec<String>,
    cases: usize,
}

impl Check {
    fn new() -> Self {
        Self {
            bad: Vec::new(),
            cases: 0,
        }
    }

    fn compare(&mut self, label: &str, expected: &Value, actual: Result<Value, String>) {
        self.cases += 1;
        match (expected.get("raised").and_then(Value::as_str), actual) {
            (Some(raised), Err(message)) => {
                if !error_matches(raised, &message) {
                    self.bad.push(format!(
                        "{label}: error text want {raised:?} got {message:?}"
                    ));
                }
            }
            (Some(raised), Ok(value)) => self.bad.push(format!(
                "{label}: reference raised {raised:?}, Rust gave {}",
                short(&value)
            )),
            (None, Err(message)) => self
                .bad
                .push(format!("{label}: Rust failed with {message:?}")),
            (None, Ok(value)) => {
                let want = &expected["out"];
                let before = self.bad.len();
                walk(want, &value, label, &mut self.bad);
                let _ = before;
            }
        }
    }

    fn finish(self, name: &str) {
        let show: usize = std::env::var("EXTRACT_SHOW")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15);
        println!(
            "{name}: {} cases, {} mismatches",
            self.cases,
            self.bad.len()
        );
        assert!(self.cases > 0, "{name}: no cases ran");
        assert!(
            self.bad.is_empty(),
            "{name}: {} mismatches in {} cases\n{}",
            self.bad.len(),
            self.cases,
            self.bad
                .iter()
                .take(show)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// ----------------------------------------------------------------------------- duplicate
#[test]
fn duplicate_matches_reference() {
    let f = &fixture()["dup"];
    let mut c = Check::new();
    for case in f["tokenize"].as_array().into_iter().flatten() {
        let input = case["in"].as_str().unwrap_or_default();
        c.compare(
            &format!("tokenize/{input:?}"),
            case,
            to_value(tokenize(input)),
        );
    }
    for case in f["simhash"].as_array().into_iter().flatten() {
        let input = case["in"][0].as_str().unwrap_or_default();
        let k = case["in"][1].as_u64().unwrap_or(3) as usize;
        c.compare(
            &format!("simhash/{input:?}/{k}"),
            case,
            to_value(simhash(input, k)),
        );
    }
    for case in f["fnv"].as_array().into_iter().flatten() {
        let input = case["in"].as_str().unwrap_or_default();
        c.compare(&format!("fnv/{input:?}"), case, to_value(fnv1a_64(input)));
    }
    for case in f["hash"].as_array().into_iter().flatten() {
        let input = case["in"].as_str().unwrap_or_default();
        c.compare(
            &format!("hash/{input:?}"),
            case,
            to_value(content_hash(input)),
        );
    }
    for case in f["hamming"].as_array().into_iter().flatten() {
        let (a, b) = (
            case["in"][0].as_u64().unwrap_or(0),
            case["in"][1].as_u64().unwrap_or(0),
        );
        c.compare(
            "hamming",
            &json!({"out": {"hamming": case["hamming"], "similarity": case["similarity"]}}),
            Ok(json!({"hamming": hamming(a, b), "similarity": similarity(a, b)})),
        );
    }
    // \w and str.lower() over every code point
    let mut word = vec![false; 0x110000];
    for range in f["word_ranges"].as_array().into_iter().flatten() {
        let (lo, hi) = (
            range[0].as_u64().unwrap_or(0) as usize,
            range[1].as_u64().unwrap_or(0) as usize,
        );
        word[lo..=hi].iter_mut().for_each(|w| *w = true);
    }
    let mut lowered: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    for pair in f["lower_diff"].as_array().into_iter().flatten() {
        lowered.insert(
            pair[0].as_u64().unwrap_or(0) as u32,
            pair[1].as_str().unwrap_or("").to_owned(),
        );
    }
    let (mut word_bad, mut lower_bad) = (Vec::new(), Vec::new());
    for cp in 0..0x110000u32 {
        let Some(ch) = char::from_u32(cp) else {
            continue;
        };
        if is_word_char(ch) != word[cp as usize] {
            word_bad.push(format!("U+{cp:04X} \\w want {}", word[cp as usize]));
        }
        let want = lowered.get(&cp).cloned().unwrap_or_else(|| ch.to_string());
        let got: String = ch.to_lowercase().collect();
        let newer = matches!(cp, 0xA7CE | 0xA7D2 | 0xA7D4 | 0x16EA0..=0x16EB8);
        if got != want && !newer {
            lower_bad.push(format!("U+{cp:04X} lower want {want:?} got {got:?}"));
        }
    }
    c.cases += 2;
    c.bad.extend(word_bad.into_iter().take(200));
    c.bad.extend(lower_bad.into_iter().take(200));
    // find_duplicates
    let scenarios = f["scenarios"].as_array().cloned().unwrap_or_default();
    for (n, case) in f["find"].as_array().into_iter().flatten().enumerate() {
        let items: Vec<DuplicateItem> = scenarios
            [case["in"]["scenario"].as_u64().unwrap_or(0) as usize]
            .as_array()
            .into_iter()
            .flatten()
            .map(|it| DuplicateItem {
                id: match it
                    .get("id")
                    .filter(|v| truthy(v))
                    .or_else(|| it.get("url").filter(|v| truthy(v)))
                {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                },
                text: it
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                indexable: it.get("indexable").is_none_or(truthy),
            })
            .collect();
        let o = &case["in"]["options"];
        let mut options = DuplicateOptions::default();
        if let Some(v) = o.get("threshold").and_then(Value::as_f64) {
            options.threshold = v;
        }
        if let Some(v) = o.get("k").and_then(Value::as_u64) {
            options.k = v as usize;
        }
        options.with_fingerprints = o.get("with_fingerprints").is_some_and(truthy);
        options.only_indexable = o.get("only_indexable").is_none_or(truthy);
        options.max_shingles = o
            .get("max_shingles")
            .and_then(Value::as_u64)
            .map(|v| v as usize);
        options.max_candidate_comparisons = o
            .get("max_candidate_comparisons")
            .and_then(Value::as_u64)
            .map(|v| v as usize);
        let actual = find_duplicates(&items, &options)
            .map_err(|e| e.to_string())
            .and_then(to_value);
        c.compare(&format!("find/{n}"), case, actual);
    }
    c.finish("duplicate");
}

// ----------------------------------------------------------------------------- boilerplate
#[test]
fn boilerplate_report_matches_reference() {
    let f = &fixture()["boiler"];
    let mut c = Check::new();
    for case in f["hash"].as_array().into_iter().flatten() {
        let name = case["in"].as_str().unwrap_or_default();
        c.compare(
            &format!("hash/{name}"),
            case,
            to_value(boilerplate_hash(doc(name))),
        );
    }
    for (n, case) in f["report"].as_array().into_iter().flatten().enumerate() {
        let pages: Vec<BoilerplatePage> = case["in"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| BoilerplatePage {
                url: p
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                html: p.get("html").and_then(Value::as_str).map(str::to_owned),
                hash: p.get("hash").and_then(Value::as_str).map(str::to_owned),
            })
            .collect();
        let actual = boilerplate_consistency_report(&pages)
            .map_err(|e| e.to_string())
            .and_then(to_value);
        c.compare(&format!("report/{n}"), case, actual);
    }
    c.finish("boilerplate_report");
}

// ----------------------------------------------------------------------------- markdown
#[test]
fn markdown_extract_matches_reference() {
    let f = &fixture()["markdown"];
    let configs: Vec<Option<ContentAreaConfig>> = f["configs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    let mut c = Check::new();
    for case in f["cases"].as_array().into_iter().flatten() {
        let name = case["in"][0].as_str().unwrap_or_default();
        let ci = case["in"][1].as_u64().unwrap_or(0) as usize;
        let actual = extract_markdown(doc(name), configs[ci].as_ref())
            .map_err(|e| e.to_string())
            .and_then(to_value);
        c.compare(&format!("markdown/{name}/{ci}"), case, actual);
    }
    c.finish("markdown_extract");
}

// ----------------------------------------------------------------------------- rules
#[test]
fn extraction_rules_match_python() {
    let f = &fixture()["rules"];
    let sets = f["rule_sets"].as_array().cloned().unwrap_or_default();
    let mut c = Check::new();
    for (n, case) in f["cases"].as_array().into_iter().flatten().enumerate() {
        let i = &case["in"];
        let name = i["doc"].as_str();
        let html: Option<String> = match (name, i.get("html").and_then(Value::as_str)) {
            (_, Some(h)) => Some(h.to_owned()),
            (Some("long"), None) => f["long"].as_str().map(str::to_owned),
            (Some(n), None) => Some(doc(n).to_owned()),
            (None, None) => None,
        };
        let rules = i
            .get("rules")
            .cloned()
            .unwrap_or_else(|| sets[i["set"].as_u64().unwrap_or(0) as usize].clone());
        let parsed = (!i["jsonld"].is_null()).then(|| json!({"jsonld": i["jsonld"]}));
        let representation = if name.is_none() && html.is_none() && parsed.is_none() {
            "rendered"
        } else {
            "static"
        };
        let actual = evaluate(html.as_deref(), parsed.as_ref(), &rules, representation)
            .map_err(|e| e.to_string())
            .and_then(to_value);
        c.compare(
            &format!("evaluate/{n}/{name:?}/set{}", i["set"]),
            case,
            actual,
        );
    }
    for (n, case) in f["validate"].as_array().into_iter().flatten().enumerate() {
        let actual = validate_rules(&case["in"])
            .map_err(|e| e.to_string())
            .and_then(to_value);
        c.compare(&format!("validate_rules/{n}"), case, actual);
    }
    for (n, case) in f["result"].as_array().into_iter().flatten().enumerate() {
        let actual = validate_result(&case["in"])
            .map(|()| Value::Null)
            .map_err(|e| e.to_string());
        c.compare(&format!("validate_result/{n}"), case, actual);
    }
    c.finish("extraction_rules");
}

// ----------------------------------------------------------------------------- search
fn corpus_of(value: &Value) -> Vec<CorpusDocument> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            serde_json::from_value(d.clone()).unwrap_or_else(|_| CorpusDocument {
                url: String::new(),
                ok: true,
                html: None,
                text: None,
                rendered: false,
            })
        })
        .collect()
}

/// Filter cases split across shards so nextest runs them in parallel.
const SEARCH_SHARDS: usize = 8;

fn custom_search_shard(shard: usize) {
    let f = &fixture()["search"];
    let corpus = corpus_of(&f["corpus"]);
    let mut c = Check::new();
    for (n, case) in f["cases"].as_array().into_iter().flatten().enumerate() {
        if n % SEARCH_SHARDS != shard {
            continue;
        }
        let spec: FilterSpec =
            serde_json::from_value(case["in"].clone()).unwrap_or_else(|_| panic!("spec {n}"));
        let actual = run_filter(&corpus, &spec)
            .map_err(|e| e.to_string())
            .and_then(to_value);
        c.compare(&format!("filter/{n}/{}", case["in"]), case, actual);
    }
    c.finish(&format!("custom_search/{shard}"));
}

macro_rules! search_shards {
    ($($name:ident = $shard:expr),*) => {$(
        #[test]
        fn $name() {
            custom_search_shard($shard);
        }
    )*};
}

search_shards!(
    custom_search_matches_reference_0 = 0,
    custom_search_matches_reference_1 = 1,
    custom_search_matches_reference_2 = 2,
    custom_search_matches_reference_3 = 3,
    custom_search_matches_reference_4 = 4,
    custom_search_matches_reference_5 = 5,
    custom_search_matches_reference_6 = 6,
    custom_search_matches_reference_7 = 7
);

#[test]
fn custom_search_bad_inputs_match_python() {
    let f = &fixture()["search"];
    let corpus = corpus_of(&f["corpus"]);
    let mut c = Check::new();
    for (n, case) in f["bad"].as_array().into_iter().flatten().enumerate() {
        let small = &corpus[..20];
        let actual = if let Some(multi) = case["in"].get("multi") {
            let specs: Vec<FilterSpec> = serde_json::from_value(multi.clone()).unwrap_or_default();
            run_search(&corpus[..12], &specs)
                .map_err(|e| e.to_string())
                .and_then(to_value)
        } else {
            let spec: FilterSpec =
                serde_json::from_value(case["in"].clone()).unwrap_or_else(|_| panic!("spec {n}"));
            run_filter(small, &spec)
                .map_err(|e| e.to_string())
                .and_then(to_value)
        };
        c.compare(&format!("bad/{n}/{}", case["in"]), case, actual);
    }
    c.finish("custom_search/bad");
}

// ----------------------------------------------------------------------------- extract
#[test]
fn custom_extract_matches_reference() {
    let f = &fixture()["extract"];
    let mut c = Check::new();
    let sample = |html: &str, spec: &Value| -> Result<Value, String> {
        let spec: ExtractorSpec =
            serde_json::from_value(spec.clone()).map_err(|e| e.to_string())?;
        check_sample(html, &[spec], "", 2.0)
            .map_err(|e| e.to_string())
            .and_then(to_value)
    };
    // XPath syntax validation through the `xpath` scope of custom_search.
    let tiny = vec![CorpusDocument {
        url: "u".into(),
        ok: true,
        html: Some("<p>x</p>".into()),
        text: None,
        rendered: false,
    }];
    for case in f["xpath_compile"].as_array().into_iter().flatten() {
        let q = case["in"].as_str().unwrap_or_default();
        let spec = FilterSpec {
            name: None,
            mode: None,
            kind: None,
            scope: Some("xpath".into()),
            query: "x".into(),
            selector: q.to_owned(),
            case_sensitive: false,
        };
        let actual = match run_filter(&tiny, &spec) {
            Ok(_) => Ok(Value::Null),
            Err(e) => Err(e.to_string().rsplit(": ").next().unwrap_or("").to_owned()),
        };
        let expected = case.clone();
        c.compare(&format!("xpath_compile/{q:?}"), &expected, actual);
    }
    for (n, case) in f["xpath"].as_array().into_iter().flatten().enumerate() {
        let name = case["in"]["doc"].as_str().unwrap_or_default();
        let q = case["in"]["spec"]["query"].as_str().unwrap_or_default();
        let output = case["in"]["spec"]["output"].as_str().unwrap_or_default();
        let actual = sample(doc(name), &case["in"]["spec"]);
        // The extractor entry is compared whole except the sample-level wrapper.
        let actual = actual.map(|mut v| v["extractors"][0].take());
        let mut expected = case.clone();
        if let Some(out) = expected.get_mut("out") {
            let first = out["extractors"][0].take();
            *out = first;
        }
        c.compare(
            &format!("xpath/{n}/{name}/{q:?}/{output}"),
            &expected,
            actual,
        );
    }
    for case in f["numbers"].as_array().into_iter().flatten() {
        let q = case["in"].as_str().unwrap_or_default();
        let spec = json!({"mode": "xpath", "query": format!("string({q})"), "output": "text"});
        let actual = sample("<p>x</p>", &spec).map(|v| v["extractors"][0]["values"][0].clone());
        c.compare(&format!("number/{q}"), case, actual);
    }
    for (n, case) in f["css"].as_array().into_iter().flatten().enumerate() {
        let name = case["in"]["doc"].as_str().unwrap_or_default();
        let actual = sample(doc(name), &case["in"]["spec"]);
        let mut expected = case.clone();
        if expected.get("raised").is_some() {
            expected["raised"] = json!("Error: css");
        }
        c.compare(
            &format!("css/{n}/{name}/{}", case["in"]["spec"]["query"]),
            &expected,
            actual,
        );
    }
    for (n, case) in f["regex"].as_array().into_iter().flatten().enumerate() {
        let name = case["in"]["doc"].as_str().unwrap_or_default();
        let html = format!("{} UA-123-4 ua-55 ", doc(name));
        let actual = sample(&html, &case["in"]["spec"]);
        let mut expected = case.clone();
        if expected.get("raised").is_some() {
            expected["raised"] = json!("Error: regex");
        }
        c.compare(
            &format!("regex/{n}/{name}/{}", case["in"]["spec"]["query"]),
            &expected,
            actual,
        );
    }
    for (n, case) in f["bad"].as_array().into_iter().flatten().enumerate() {
        let actual = sample(doc("x-xp-1"), &case["in"]);
        c.compare(&format!("bad/{n}"), case, actual);
    }
    let corpus = &f["corpus"];
    let documents = corpus_of(&corpus["documents"]);
    let specs: Vec<ExtractorSpec> =
        serde_json::from_value(corpus["specs"].clone()).unwrap_or_default();
    let actual = run_extraction(&documents, &specs, 2.0)
        .map_err(|e| e.to_string())
        .and_then(to_value);
    c.compare("corpus", corpus, actual);
    for (n, case) in f["samples"].as_array().into_iter().flatten().enumerate() {
        let html = case["in"]["html"].as_str().unwrap_or_default();
        let spec: ExtractorSpec =
            serde_json::from_value(case["in"]["spec"].clone()).unwrap_or_else(|_| panic!("spec"));
        let actual = check_sample(html, &[spec], "https://example.com/s", 2.0)
            .map_err(|e| e.to_string())
            .and_then(to_value);
        c.compare(&format!("sample/{n}"), case, actual);
    }
    c.finish("custom_extract");
}

// ----------------------------------------------------------------------------- schema
#[test]
fn schema_org_matches_reference() {
    let f = &fixture()["schema"];
    let mut c = Check::new();
    for (n, case) in f["cases"].as_array().into_iter().flatten().enumerate() {
        let name = case["in"]["doc"].as_str().unwrap_or_default();
        let base = case["in"]["base"].as_str();
        let actual = check_schema(SchemaSource::Html(doc(name)), None, 0.0, base)
            .map_err(|e| e.to_string())
            .and_then(to_value);
        c.compare(&format!("check/{n}/{name}/{base:?}"), case, actual);
    }
    for case in f["hint"].as_array().into_iter().flatten() {
        let t = case["in"].as_str().unwrap_or_default();
        c.compare(&format!("hint/{t:?}"), case, to_value(json_syntax_hint(t)));
    }
    for (n, case) in f["ids"].as_array().into_iter().flatten().enumerate() {
        let actual = normalize_jsonld_ids(
            &case["in"]["payload"],
            case["in"]["base"].as_str().unwrap_or_default(),
        )
        .map_err(|e| e.to_string());
        c.compare(&format!("ids/{n}"), case, actual);
    }
    let vocab = spiderhead_parse::schema_org::load_vocab();
    for case in f["chain"].as_array().into_iter().flatten() {
        let name = case["in"].as_str().unwrap_or_default();
        c.compare(
            &format!("chain/{name}"),
            &json!({"out": case["out"]}),
            to_value(type_chain(name, vocab)),
        );
    }
    let summary = json!({"types": vocab["types"].as_object().map_or(0, |t| t.len()), "properties": vocab["properties"].as_object().map_or(0, |t| t.len()), "fetched": vocab["fetched"]});
    c.compare("vocab", &json!({"out": f["vocab"]}), Ok(summary));
    c.compare(
        "rules_version",
        &json!({"out": f["rules_version"]}),
        Ok(spiderhead_parse::schema_org::rich_results_rules()["version"].clone()),
    );
    c.finish("schema_org");
}
