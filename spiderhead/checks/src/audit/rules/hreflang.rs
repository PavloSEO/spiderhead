//! hreflang checks over the saved internationalization derivation
//! (reference the reference suite, native path).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Map, Value};
use spiderhead_parse::hreflang_code::code_error;

use super::details;
use crate::audit::{norm_url, AuditContext};

const LIMIT: usize = 200;
const ANSWERED: [&str; 2] = ["observed", "probed"];
const XDEFAULT: &str = "x-default";

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn active(item: &Value) -> bool {
    s(item, "source_representation_state") == "active" && s(item, "declaration_state") == "declared"
}

/// `ctx.native_hreflang`: the retained internationalization block. `None`
/// means no derivation ran — the export-path fallbacks apply.
fn native_i18n(ctx: &AuditContext) -> Option<Value> {
    ctx.native_hreflang.clone()
}

/// `_hreflang_unavailable`: the skip reason in the words of the run that
/// produced it — an export name on the SF path, a native-crawl statement here.
fn hreflang_unavailable(ctx: &AuditContext, sf_reason: &str) -> String {
    if ctx.scan.is_none() {
        return sf_reason.to_owned();
    }
    let declared = ctx
        .native_hreflang
        .as_ref()
        .and_then(|v| v.get("declarations"))
        .and_then(Value::as_array)
        .is_some_and(|d| !d.is_empty());
    if declared {
        "not measured by this native crawl".to_owned()
    } else {
        "the native crawl retained no hreflang declarations".to_owned()
    }
}

/// `_native_hreflang_groups`: every matching declaration grouped by `key`,
/// groups in first-occurrence order, each sample capped at
/// `output.max_locations_per_issue` beside its full count.
fn native_groups<'a>(
    ctx: &AuditContext,
    declarations: &'a [Value],
    key: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Vec<(String, Vec<&'a Value>, usize)> {
    let limit = ctx
        .config
        .get("output")
        .and_then(|v| v.get("max_locations_per_issue"))
        .and_then(Value::as_i64)
        .map_or(LIMIT, |n| n.max(0) as usize);
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<&'a Value>> = HashMap::new();
    for item in declarations.iter().filter(|i| predicate(i)) {
        let k = s(item, key).to_owned();
        grouped
            .entry(k.clone())
            .or_insert_with(|| {
                order.push(k.clone());
                Vec::new()
            })
            .push(item);
    }
    order
        .into_iter()
        .map(|k| {
            let rows = grouped.remove(&k).unwrap_or_default();
            let count = rows.len();
            (k, rows.into_iter().take(limit).collect(), count)
        })
        .collect()
}

/// `_native_sample_details`: the bounded sample plus truncation counters when
/// the group outgrew it.
fn sample_details(name: &str, entries: Vec<Value>, count: usize) -> Map<String, Value> {
    let shown = entries.len();
    let mut d = details([(name, Value::Array(entries))]);
    if count > shown {
        d.insert("sample_truncated".into(), json!(true));
        d.insert("omitted_relations".into(), json!(count - shown));
        d.insert(
            "full_relations_collection".into(),
            json!("/summary/saved_corpus_derivations/internationalization/declarations"),
        );
    }
    d
}

/// `check_hreflang_targets` (native path): `HREFLANG_BROKEN_TARGET` — active
/// declarations whose observed/probed target answered 3xx+ or carries a
/// redirect. Unanswered targets stay unmeasured and gate a skip, never a
/// clean pass.
pub fn check_hreflang_targets(ctx: &mut AuditContext) {
    let Some(native) = native_i18n(ctx) else {
        ctx.skip(
            "HREFLANG_BROKEN_TARGET",
            &hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export → Links → All Hreflang to enable)",
            ),
        );
        return;
    };
    let declarations = native["declarations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if declarations.is_empty() {
        ctx.skip(
            "HREFLANG_BROKEN_TARGET",
            &hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export → Links → All Hreflang to enable)",
            ),
        );
        return;
    }
    let mut fired = false;
    for (source, rows, count) in native_groups(ctx, &declarations, "source_url", |item| {
        let target = &item["target_observation"];
        let code = target.get("status_code").and_then(Value::as_i64);
        active(item)
            && ANSWERED.contains(&s(target, "state"))
            && code.is_some()
            && (code.is_some_and(|c| c >= 300) || !s(target, "redirect_url").is_empty())
    }) {
        fired = true;
        let targets: Vec<Value> = rows
            .iter()
            .map(|item| {
                json!({
                    "hreflang": item["lang"],
                    "target_url": item["target"],
                    "target_identity": item["target_identity"],
                    "status_code": item["target_observation"]["status_code"],
                    "redirect_url": item["target_observation"]["redirect_url"],
                    "source_declaration_id": item["source_declaration_id"],
                    "label_context": item["label_context"],
                })
            })
            .collect();
        ctx.add_with(
            "HREFLANG_BROKEN_TARGET",
            Some(&source),
            sample_details("broken_targets", targets, count),
            None,
            count as i64,
            None,
        );
    }
    let unmeasured = declarations.iter().any(|item| {
        active(item)
            && (!ANSWERED.contains(&s(&item["target_observation"], "state"))
                || item["target_observation"]
                    .get("status_code")
                    .and_then(Value::as_i64)
                    .is_none())
    });
    if !fired && (unmeasured || s(&native["coverage"], "state") != "complete") {
        ctx.skip(
            "HREFLANG_BROKEN_TARGET",
            "one or more hreflang targets or the retained population were unmeasured",
        );
    }
}

/// `check_hreflang_noindex_targets` (native path): `HREFLANG_NOINDEX_TARGET` —
/// observed alternates whose indexability reason is the noindex directive.
pub fn check_hreflang_noindex(ctx: &mut AuditContext) {
    let Some(native) = native_i18n(ctx) else {
        ctx.skip(
            "HREFLANG_NOINDEX_TARGET",
            &hreflang_unavailable(ctx, "no all_hreflang declaration export"),
        );
        return;
    };
    let declarations = native["declarations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if declarations.is_empty() {
        ctx.skip(
            "HREFLANG_NOINDEX_TARGET",
            &hreflang_unavailable(ctx, "no all_hreflang declaration export"),
        );
        return;
    }
    let mut fired = false;
    for (source, rows, count) in native_groups(ctx, &declarations, "source_url", |item| {
        let target = &item["target_observation"];
        active(item)
            && ["observed", "probed"].contains(&s(target, "state"))
            && s(&target["indexability"], "reason") == "noindex directive"
    }) {
        fired = true;
        let entries: Vec<Value> = rows
            .iter()
            .map(|item| {
                json!({
                    "hreflang": item["lang"],
                    "target": item["target"],
                    "target_identity": item["target_identity"],
                    "source_declaration_id": item["source_declaration_id"],
                    "label_context": item["label_context"],
                })
            })
            .collect();
        ctx.add_with(
            "HREFLANG_NOINDEX_TARGET",
            Some(&source),
            sample_details("noindex_targets", entries, count),
            None,
            count as i64,
            None,
        );
    }
    let unmeasured = declarations.iter().any(|item| {
        active(item)
            && (!["observed", "probed"].contains(&s(&item["target_observation"], "state"))
                || s(&item["target_observation"]["indexability"], "state") == "unmeasured")
    });
    if !fired && (unmeasured || s(&native["coverage"], "state") != "complete") {
        ctx.skip(
            "HREFLANG_NOINDEX_TARGET",
            "one or more hreflang targets or the retained population were unmeasured",
        );
    }
}

/// `check_hreflang_confirmation_consistency` (native path):
/// `HREFLANG_INCONSISTENT_CONFIRMATION` — a page's declared code for a
/// counterpart disagrees with the counterpart's own self-reference set.
pub fn check_hreflang_consistency(ctx: &mut AuditContext) {
    let Some(native) = native_i18n(ctx) else {
        ctx.skip(
            "HREFLANG_INCONSISTENT_CONFIRMATION",
            &hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)",
            ),
        );
        return;
    };
    let declarations = native["declarations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if declarations.is_empty() {
        ctx.skip(
            "HREFLANG_INCONSISTENT_CONFIRMATION",
            &hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)",
            ),
        );
        return;
    }
    // Self-reference sets: the corpus owner index when the derivation ran
    // (`ctx.corpus_self_codes`, declared rows only), else the same fold the
    // fallback fold applies over active self-referencing rows.
    let fallback = |declarations: &[Value]| {
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for item in declarations {
            let code = s(item, "lang").trim().to_lowercase();
            if s(item, "source_representation_state") == "active"
                && s(item, "source_identity") == s(item, "target_identity")
                && !code.is_empty()
                && code != XDEFAULT
            {
                out.entry(s(item, "source_identity").to_owned())
                    .or_default()
                    .insert(code);
            }
        }
        out
    };
    let self_codes: BTreeMap<String, BTreeSet<String>> = ctx
        .corpus_self_codes
        .clone()
        .unwrap_or_else(|| fallback(&declarations));
    let rep = super::pagination::norm_index(ctx).1;
    let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
    for item in declarations
        .iter()
        .filter(|i| s(i, "source_representation_state") == "active")
    {
        let src = s(item, "source_url").to_owned();
        match groups.last_mut() {
            Some((last, rows)) if *last == src => rows.push(item),
            _ => groups.push((src, vec![item])),
        }
    }
    for (source, rows) in groups {
        let mut mismatched: Vec<Value> = Vec::new();
        for item in rows {
            let code = s(item, "lang").trim().to_lowercase();
            if code.is_empty()
                || code == XDEFAULT
                || s(item, "source_identity") == s(item, "target_identity")
            {
                continue;
            }
            let confirmed = self_codes.get(s(item, "target_identity"));
            let target = rep.get(s(item, "target_identity")).map(|&i| &ctx.pages[i]);
            if let (Some(confirmed), Some(target)) = (confirmed, target) {
                if !confirmed.contains(&code) {
                    mismatched.push(json!({
                        "counterpart": target.url,
                        "declared_here": code,
                        "confirmed_there": confirmed.iter().collect::<Vec<_>>(),
                    }));
                }
            }
        }
        if !mismatched.is_empty() {
            let n = mismatched.len() as i64;
            let page = rep.get(&norm_url(&source)).map(|&i| &ctx.pages[i]);
            let target_url = page.map(|p| p.url.clone()).unwrap_or(source);
            ctx.add_with(
                "HREFLANG_INCONSISTENT_CONFIRMATION",
                Some(&target_url),
                details([("inconsistent", Value::Array(mismatched))]),
                None,
                n,
                None,
            );
        }
    }
}

/// The shared no-declarations tail of `check_hreflang_quality` and
/// `check_hreflang_reciprocity`: the same `_hreflang_unavailable` reason for
/// every check the two passes measure.
fn quality_unavailable(ctx: &mut AuditContext) {
    let reason = hreflang_unavailable(
        ctx,
        "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)",
    );
    for id in [
        "HREFLANG_INVALID_CODE",
        "HREFLANG_MULTIPLE_ENTRIES",
        "HREFLANG_MISSING_SELF_REFERENCE",
        "HREFLANG_MISSING_XDEFAULT",
        "HREFLANG_NOT_CANONICAL",
        "HREFLANG_MISSING_RETURN_LINK",
    ] {
        ctx.skip(id, &reason);
    }
}

/// `check_hreflang_quality` + `check_hreflang_reciprocity` (native path):
/// quality rows per declaring page and the reciprocity row per missing
/// return target.
pub fn check_hreflang(ctx: &mut AuditContext) {
    let Some(native) = native_i18n(ctx) else {
        quality_unavailable(ctx);
        return;
    };
    let declarations: Vec<Value> = native["declarations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if declarations.is_empty() {
        quality_unavailable(ctx);
        return;
    }
    // quality: consecutive groups of active declarations by source
    let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
    for item in declarations
        .iter()
        .filter(|i| s(i, "source_representation_state") == "active")
    {
        let src = s(item, "source_url").to_owned();
        match groups.last_mut() {
            Some((last, rows)) if *last == src => rows.push(item),
            _ => groups.push((src, vec![item])),
        }
    }
    for (source, rows) in &groups {
        let entries: Vec<(String, String)> = rows
            .iter()
            .map(|r| (s(r, "lang").to_owned(), s(r, "target").to_owned()))
            .collect();
        let invalid: Vec<Value> = entries
            .iter()
            .filter(|(l, _)| !l.is_empty())
            .filter_map(|(l, d)| {
                code_error(l)
                    .map(|reason| json!({"hreflang": l, "destination": d, "reason": reason}))
            })
            .collect();
        if !invalid.is_empty() {
            let n = invalid.len() as i64;
            ctx.add_with(
                "HREFLANG_INVALID_CODE",
                Some(source),
                details([("invalid", Value::Array(invalid))]),
                None,
                n,
                None,
            );
        }
        let folded: Vec<String> = entries
            .iter()
            .filter(|(l, _)| !l.is_empty())
            .map(|(l, _)| l.trim().to_lowercase())
            .collect();
        let dups: BTreeSet<&String> = folded
            .iter()
            .filter(|l| folded.iter().filter(|x| x == l).count() > 1)
            .collect();
        if !dups.is_empty() {
            let n = dups.len() as i64;
            ctx.add_with(
                "HREFLANG_MULTIPLE_ENTRIES",
                Some(source),
                details([("duplicate_values", json!(dups))]),
                None,
                n,
                None,
            );
        }
        let targets: Vec<&(String, String)> =
            entries.iter().filter(|(_, d)| !d.is_empty()).collect();
        let dest_norms: BTreeSet<String> = targets.iter().map(|(_, d)| norm_url(d)).collect();
        if !dest_norms.contains(&norm_url(source)) {
            let declared: BTreeSet<&String> = targets.iter().map(|(_, d)| d).collect();
            ctx.add(
                "HREFLANG_MISSING_SELF_REFERENCE",
                Some(source),
                details([("declared_targets", json!(declared))]),
            );
        }
        let tf: BTreeSet<String> = targets
            .iter()
            .filter(|(l, _)| !l.is_empty())
            .map(|(l, _)| l.trim().to_lowercase())
            .collect();
        if !tf.contains("x-default") {
            ctx.add(
                "HREFLANG_MISSING_XDEFAULT",
                Some(source),
                Default::default(),
            );
        }
        let (mut offenders, mut unmeasured) = (Vec::new(), 0);
        for (lang, dest) in &targets {
            let page = ctx.pages.iter().find(|p| p.url == *dest).or_else(|| {
                let key = norm_url(dest);
                let list: Vec<&crate::audit::Page> = ctx
                    .pages
                    .iter()
                    .filter(|p| norm_url(&p.url) == key)
                    .collect();
                list.iter()
                    .find(|p| p.is_2xx())
                    .copied()
                    .or_else(|| list.first().copied())
            });
            // A captured page's canonical, else the post-crawl probe's — the
            // probe observation is the only canonical read available for a
            // target the crawl never reached.
            let canonical = match page {
                Some(p) => json!(p.text("canonical")),
                None => ctx
                    .target_probes
                    .get(&norm_url(dest))
                    .and_then(|pr| pr.get("canonical").cloned())
                    .unwrap_or(Value::Null),
            };
            if (page.is_none() && canonical.is_null()) || page.is_some_and(|p| p.body_unavailable())
            {
                unmeasured += 1;
                continue;
            }
            if let Some(c) = canonical.as_str() {
                if !c.is_empty() && norm_url(c) != norm_url(dest) {
                    offenders.push(json!({"hreflang": lang, "destination": dest, "canonical": c}));
                }
            }
        }
        if !offenders.is_empty() {
            let n = offenders.len() as i64;
            ctx.add_with(
                "HREFLANG_NOT_CANONICAL",
                Some(source),
                details([("non_canonical_targets", Value::Array(offenders))]),
                None,
                n,
                None,
            );
        }
        if unmeasured > 0 {
            ctx.skip("HREFLANG_NOT_CANONICAL", &format!("{unmeasured} hreflang target(s) lack measured canonical annotations; not measured by this native crawl or export"));
        }
    }
    // reciprocity: missing-return groups keyed by target identity, in
    // first-occurrence order (`_native_hreflang_groups` ORDER BY MIN(seq)).
    let rep = super::pagination::norm_index(ctx).1;
    let mut fired = false;
    for (identity, rows, count) in native_groups(ctx, &declarations, "target_identity", |item| {
        active(item)
            && s(item, "source_identity") != s(item, "target_identity")
            && s(&item["reciprocity"], "state") == "missing"
    }) {
        fired = true;
        let relations: Vec<Value> = rows
            .iter()
            .map(|i| {
                json!({"source_url": i["source_url"], "source_identity": i["source_identity"], "target": i["target"], "target_identity": i["target_identity"], "hreflang": i["lang"], "source_declaration_id": i["source_declaration_id"], "label_context": i["label_context"]})
            })
            .collect();
        let mut d = sample_details("relations", relations, count);
        let back: BTreeSet<&str> = rows.iter().map(|i| s(i, "source_url")).collect();
        d.insert("expected_return_to".into(), json!(back));
        let target = rep
            .get(&identity)
            .map(|&i| ctx.pages[i].url.clone())
            .unwrap_or_else(|| s(rows[0], "target").to_owned());
        ctx.add_with(
            "HREFLANG_MISSING_RETURN_LINK",
            Some(&target),
            d,
            None,
            count as i64,
            None,
        );
    }
    if !fired && native["coverage"]["state"] != "complete" {
        ctx.skip(
            "HREFLANG_MISSING_RETURN_LINK",
            &format!(
                "retained hreflang relationship coverage is incomplete: {}",
                s(&native["coverage"], "reason")
            ),
        );
    }
}
