//! Replay the the reference suite fixture: build `AuditContext` over the shared
//! exports dir in mem and disk modes, run the identical scripted operations,
//! and diff every observable against the oracle snapshot.

use serde_json::{json, Map, Value};
use spiderhead_formats::models::{Group, Issue};
use spiderhead_sf_import::context::disk::DiskIssueResults;
use spiderhead_sf_import::context::{AddOpts, AuditContext, PageHandle};
use spiderhead_sf_import::loader;
use spiderhead_sf_import::normalize::Object;
use std::collections::HashSet;
use std::path::PathBuf;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn config() -> Value {
    json!({
        "checks": {
            "TITLE_DUPLICATE": {"enabled": false},
            "META_ROBOTS_NOINDEX": {"severity": "warning"},
            "TITLE_MISSING": {"enabled": "yes"},
        },
        "severity_overrides": {"BROKEN_PAGE_4XX": "critical"},
        "thresholds": {"probe": 7},
    })
}

fn page_json(h: &PageHandle) -> Value {
    h.map(|p| {
        json!({
            "url": p.url,
            "status_code": p.status_code,
            "status": p.status,
            "content_type": p.content_type,
            "indexability": p.indexability,
            "indexability_status": p.indexability_status,
            "metrics": p.metrics,
            "issues": p.issues,
            "issue_ids": p.issue_ids,
            "suppressed_issue_ids": p.suppressed_issue_ids,
        })
    })
}

fn issue_json(i: &Issue) -> Value {
    json!({
        "check": i.check,
        "severity": i.severity,
        "source": i.source,
        "message": i.message,
        "id": i.id,
        "fingerprint": i.fingerprint,
        "target_url": i.target_url,
        "status_code": i.status_code,
        "occurrences_count": i.occurrences_count,
        "locations": i.locations,
        "details": i.details,
        "group_id": i.group_id,
        "fix_hint": i.fix_hint,
        "evidence": i.evidence,
    })
}

fn group_json(g: &Group) -> Value {
    json!({
        "group_id": g.group_id,
        "check": g.check,
        "value": g.value,
        "urls": g.urls,
        "count": g.count,
    })
}

fn probe(ctx: &mut AuditContext, disk: bool) -> Value {
    let mut out = Map::new();
    out.insert(
        "flags".into(),
        json!({
            "enabled_dup": ctx.enabled("TITLE_DUPLICATE"),
            "enabled_missing": ctx.enabled("TITLE_MISSING"),
            "enabled_unknown": ctx.enabled("NOPE_UNKNOWN_XYZ"),
            "sev_4xx": ctx.severity("BROKEN_PAGE_4XX"),
            "sev_robots": ctx.severity("META_ROBOTS_NOINDEX"),
            "sev_dup": ctx.severity("TITLE_DUPLICATE"),
            "sev_unknown": ctx.severity("NOPE_UNKNOWN_XYZ"),
            "settled_early": ctx.settled("TITLE_MISSING"),
        }),
    );
    out.insert(
        "pages".into(),
        Value::Array(ctx.pages_iter().iter().map(page_json).collect()),
    );
    out.insert(
        "page_by_url_keys".into(),
        Value::Array(ctx.iter_urls().into_iter().map(Value::String).collect()),
    );
    out.insert("len_page_by_url".into(), Value::from(ctx.url_count()));
    let norms = ctx.iter_norms();
    out.insert(
        "norms_iter".into(),
        Value::Array(norms.iter().cloned().map(Value::String).collect()),
    );
    out.insert("len_pages_by_norm".into(), Value::from(ctx.norm_count()));
    out.insert(
        "len_page_by_norm".into(),
        Value::from(ctx.page_by_norm_len()),
    );
    let mut buckets = Map::new();
    let mut reps = Map::new();
    for norm in &norms {
        buckets.insert(
            norm.clone(),
            Value::Array(
                ctx.pages_by_norm(norm)
                    .iter()
                    .map(|h| Value::String(h.url()))
                    .collect(),
            ),
        );
        if let Some(rep) = ctx.page_by_norm(norm) {
            reps.insert(norm.clone(), Value::String(rep.url()));
        }
    }
    out.insert("pages_by_norm".into(), Value::Object(buckets));
    out.insert("page_by_norm".into(), Value::Object(reps));
    out.insert(
        "redirect_map".into(),
        serde_json::to_value(&ctx.redirect_map).unwrap(),
    );
    out.insert(
        "html_page_keys".into(),
        Value::Array(
            ctx.html_page_keys()
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );
    out.insert(
        "html_pages".into(),
        Value::Array(
            ctx.html_pages()
                .iter()
                .map(|h| Value::String(h.url()))
                .collect(),
        ),
    );
    out.insert(
        "indexable_html_pages".into(),
        Value::Array(
            ctx.indexable_html_pages()
                .iter()
                .map(|h| Value::String(h.url()))
                .collect(),
        ),
    );
    out.insert(
        "page_neg1".into(),
        ctx.pages_index(-1)
            .map(|h| Value::String(h.url()))
            .unwrap_or_else(|| Value::String("IndexError".into())),
    );
    out.insert(
        "page_oob".into(),
        if ctx.pages_index(999).is_none() {
            Value::String("IndexError".into())
        } else {
            Value::String("ok".into())
        },
    );
    out.insert(
        "page_get_missing".into(),
        Value::Bool(ctx.page_by_url("https://a.test/nope").is_none()),
    );
    out.insert(
        "bucket_missing".into(),
        Value::Bool(ctx.pages_by_norm("https://nope.test/").is_empty()),
    );

    // Page mutations (dict/list writes / _StoredDict+_StoredList write-through).
    ctx.mutate_page("https://a.test/miss", |p| {
        p.metrics.insert("probe_metric".into(), Value::from(7));
        p.metrics.insert("probe_drop".into(), Value::from(1));
        p.metrics.shift_remove("probe_drop");
        p.issues.push("TITLE_MISSING".into());
        p.issue_ids.push("ISS-1".into());
        p.suppressed_issue_ids.push("SUP-1".into());
    });
    out.insert(
        "after_mutate".into(),
        page_json(&ctx.page_by_url("https://a.test/miss").unwrap()),
    );
    if disk {
        ctx.attach_issue("https://a.test/x/", "X_CHECK", "ISS-9", true);
        out.insert(
            "after_attach".into(),
            page_json(&ctx.page_by_url("https://a.test/x/").unwrap()),
        );
    }

    ctx.skip("BROKEN_PAGE_4XX", "tentative");
    let fired1 = ctx
        .add(
            "BROKEN_PAGE_4XX",
            AddOpts {
                target_url: Some("https://a.test/five".into()),
                status_code: Some(500),
                locations: vec![{
                    let mut l = Object::new();
                    l.insert("url".into(), Value::from("https://a.test/"));
                    l.insert("position".into(), Value::from("body"));
                    l
                }],
                details: {
                    let mut d = Object::new();
                    d.insert("n".into(), Value::from(1));
                    d
                },
                evidence: {
                    let mut e = Object::new();
                    e.insert("seen".into(), Value::Bool(true));
                    e
                },
                ..AddOpts::default()
            },
        )
        .unwrap();
    let fired_disabled = ctx
        .add(
            "TITLE_DUPLICATE",
            AddOpts {
                target_url: Some("https://a.test/dup".into()),
                ..AddOpts::default()
            },
        )
        .unwrap();
    let fired_unknown = ctx
        .add(
            "NOPE_UNKNOWN_XYZ",
            AddOpts {
                target_url: Some("https://a.test/miss".into()),
                ..AddOpts::default()
            },
        )
        .unwrap();
    let fired_hint = ctx
        .add(
            "TITLE_MISSING",
            AddOpts {
                target_url: Some("https://a.test/miss".into()),
                fix_hint: Some(None),
                source: Some("custom".into()),
                message: Some("msg".into()),
                occurrences_count: Some(3),
                ..AddOpts::default()
            },
        )
        .unwrap();
    ctx.skip("NOPE_UNKNOWN_XYZ", "late");
    ctx.skip("CANONICAL_OUTSIDE_SITEMAP", "plain skip");
    ctx.skip("CANONICAL_OUTSIDE_SITEMAP", "twice");
    ctx.retract("BROKEN_PAGE_4XX", "withheld").unwrap();
    out.insert(
        "adds".into(),
        json!({
            "fired1": fired1.as_ref().map(issue_json),
            "fired_disabled": fired_disabled.as_ref().map(issue_json),
            "fired_unknown": fired_unknown.as_ref().map(issue_json),
            "fired_hint": fired_hint.as_ref().map(issue_json),
        }),
    );
    out.insert(
        "settled_after".into(),
        json!({
            "TITLE_MISSING": ctx.settled("TITLE_MISSING"),
            "CANONICAL_OUTSIDE_SITEMAP": ctx.settled("CANONICAL_OUTSIDE_SITEMAP"),
            "BROKEN_PAGE_4XX": ctx.settled("BROKEN_PAGE_4XX"),
        }),
    );

    let g1 = ctx
        .add_group(
            "TITLE_MISSING",
            Some("Same title".into()),
            vec!["https://a.test/a".into(), "https://a.test/b".into()],
        )
        .unwrap();
    let g2 = ctx
        .add_group("TITLE_DUPLICATE", None, vec!["https://a.test/c".into()])
        .unwrap();
    ctx.add_site_wide(
        "META_ROBOTS_NOINDEX",
        &[
            "https://a.test/z2".into(),
            "https://a.test/z1".into(),
            "https://a.test/z3".into(),
        ],
        {
            let mut d = Object::new();
            d.insert("shared".into(), Value::from("tag"));
            d
        },
        Some(2),
    )
    .unwrap();
    ctx.add_site_wide(
        "H1_MISSING",
        &["https://a.test/lone".into()],
        {
            let mut d = Object::new();
            d.insert("shared".into(), Value::from("x"));
            d
        },
        Some(5),
    )
    .unwrap();
    out.insert(
        "groups_added".into(),
        json!([g1.as_ref().map(group_json), g2.as_ref().map(group_json)]),
    );

    ctx.skip_unsupported(
        &HashSet::from(["internal_all", "images_missing_alt"]),
        &HashSet::new(),
    );

    out.insert(
        "issues".into(),
        Value::Array(ctx.issues_iter().iter().map(issue_json).collect()),
    );
    if disk {
        out.insert(
            "issues_deduped_sorted".into(),
            Value::Array(ctx.issues_deduped_sorted().iter().map(issue_json).collect()),
        );
        out.insert(
            "has_check".into(),
            json!({
                "TITLE_MISSING": ctx.issues_has_check("TITLE_MISSING"),
                "BROKEN_PAGE_4XX": ctx.issues_has_check("BROKEN_PAGE_4XX"),
            }),
        );
    }
    out.insert("issues_len".into(), Value::from(ctx.issues_len()));
    out.insert(
        "groups".into(),
        Value::Array(ctx.groups_iter().iter().map(group_json).collect()),
    );
    out.insert(
        "skipped".into(),
        Value::Array(
            ctx.skipped
                .iter()
                .map(|s| json!({"id": s.id, "reason": s.reason}))
                .collect(),
        ),
    );

    // Final-findings store + implausible-target ledger (both modes).
    let mut res = DiskIssueResults::new().unwrap();
    for i in ctx.issues_iter() {
        res.append_active(&i).unwrap();
    }
    let img = {
        let mut i = Issue::new("IMG_ALT_MISSING", "notice", "SF-derived", "img");
        i.target_url = Some("https://a.test/i.png".into());
        i.details
            .insert("source_page".into(), Value::from("https://a.test/"));
        i.details
            .insert("target_scope".into(), Value::from("content"));
        i
    };
    res.append_active(&img).unwrap();
    res.append_suppressed(&{
        let mut o = Object::new();
        o.insert("check".into(), Value::from("X"));
        o.insert("severity".into(), Value::from("notice"));
        o.insert("target_url".into(), Value::Null);
        o.insert("k".into(), Value::from(1));
        o
    })
    .unwrap();
    res.append_suppressed(&{
        let mut o = Object::new();
        o.insert("check".into(), Value::from("Y"));
        o.insert("severity".into(), Value::from("warning"));
        o
    })
    .unwrap();
    let orphan = {
        let mut i = Issue::new("ORPHAN", "notice", "SF-derived", "m");
        i.target_url = Some("https://a.test/orphan".into());
        i.locations = vec![
            {
                let mut l = Object::new();
                l.insert("url".into(), Value::from("https://a.test/loc"));
                l
            },
            {
                let mut l = Object::new();
                l.insert("url".into(), Value::from(""));
                l
            },
            {
                let mut l = Object::new();
                l.insert("n".into(), Value::from(1));
                l
            },
        ];
        i
    };
    res.remember_implausible_targets(&orphan, false);
    let img_only = {
        let mut i = Issue::new("IMG_ALT_MISSING", "notice", "SF-derived", "m");
        i.target_url = Some("https://a.test/i.png".into());
        i
    };
    res.remember_implausible_targets(&img_only, true);
    out.insert(
        "final_active".into(),
        Value::Array(res.iter().iter().map(issue_json).collect()),
    );
    out.insert("final_index0".into(), issue_json(&res.index(0).unwrap()));
    out.insert(
        "final_suppressed".into(),
        Value::Array(
            res.iter_suppressed()
                .into_iter()
                .map(Value::Object)
                .collect(),
        ),
    );
    out.insert(
        "final_suppressed_len".into(),
        Value::from(res.suppressed_len()),
    );
    out.insert(
        "final_suppressed_idx".into(),
        Value::Object(res.suppressed_index(0).unwrap()),
    );
    out.insert(
        "final_suppressed_neg".into(),
        if res.suppressed_index(-1).is_none() {
            Value::String("IndexError".into())
        } else {
            Value::String("ok".into())
        },
    );
    let mut impl_counts: Vec<(String, i64)> = res.implausible_counts();
    impl_counts.sort();
    out.insert(
        "final_implausible".into(),
        serde_json::to_value(impl_counts).unwrap(),
    );
    Value::Object(out)
}

#[test]
fn oracle_cases() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context");
    let exports = loader::load_exports(&dir, &["internal_all"], false).unwrap();
    let want = fixture("context.json");
    for (mode, disk) in [("mem", false), ("disk", true)] {
        let mut ctx = AuditContext::new(exports.clone(), config(), None, disk).unwrap();
        let got = probe(&mut ctx, disk);
        let want_mode = &want[mode];
        if &got != want_mode {
            let ga = got.as_object().unwrap();
            let wa = want_mode.as_object().unwrap();
            for key in wa.keys().chain(ga.keys()) {
                if ga.get(key) != wa.get(key) {
                    panic!(
                        "context[{mode}].{key} mismatch\n want: {}\n  got: {}",
                        serde_json::to_string_pretty(&wa.get(key)).unwrap(),
                        serde_json::to_string_pretty(&ga.get(key)).unwrap(),
                    );
                }
            }
        }
        ctx.close();
    }
}
