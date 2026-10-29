//! Stage regressions ported from the reference suite (`tests/test_semantics_*the reference suite)
//! — provider-receipt safety, paid-task recovery and cached-SERP reuse.
//! Each test cites the original `// from tests/<file>the reference suite::<test>`.

mod oracle;

use serde_json::json;
use spiderhead_semantics::config::Cfg;
use spiderhead_semantics::providers::{
    ExpandResult, FakeArsenkin, FakeDemand, FakeSerp, ProviderError,
};
use spiderhead_semantics::stages;
use spiderhead_semantics::store::Store;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sem-regress-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// the reference suite — preset + cuts + provider caps.
fn sem_cfg(dir: &Path) -> Cfg {
    let mut cfg = Cfg::new();
    for (k, v) in [
        ("_project", json!("fixture-project")),
        ("preset", json!("seo")),
        ("regions", json!(["225"])),
        ("min_base", json!(20)),
        ("exact_cut_base", json!(20)),
        ("exact_cut_exact", json!(10)),
        ("budget_arsenkin_gate", json!(2000)),
        ("reexpand_floor", json!(40)),
        ("max_depth", json!(0)),
        ("num", json!(10)),
        ("rps", json!(100)),
    ] {
        cfg.insert(k.into(), v);
    }
    cfg.insert("_dir".into(), json!(dir.to_string_lossy().to_string()));
    cfg
}

fn store(dir: &Path) -> Store {
    Store::open(&dir.join("sya.db"), false).unwrap()
}

fn kept(store: &Store, norm: &str, base: i64) {
    store.upsert(norm, Some(base), 0, None, "225").unwrap();
    store
        .set_status(norm, "kept", Some("ok"), Some("clean"))
        .unwrap();
    store.commit().unwrap();
}

/// `seed_exact` — kept phrase plus extra fields, committed.
fn seed_exact(store: &Store, phrase: &str, fields: &[(String, serde_json::Value)]) {
    store.upsert(phrase, Some(100), 0, None, "225").unwrap();
    let mut kw: Vec<(String, serde_json::Value)> = vec![("status".into(), json!("kept"))];
    kw.extend(fields.iter().cloned());
    store.set_fields(phrase, &kw).unwrap();
    store.commit().unwrap();
}

fn wordstat_response(rows: serde_json::Value) -> serde_json::Value {
    json!({"code": "TASK_RESULT", "result": {"data": {"result": rows}}})
}

// ---------- cluster / SERP receipts (the reference suite) ----------

#[test]
fn empty_success_is_cached_without_second_submission() {
    let dir = tmp("serp-cached");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    kept(&store, "synthetic phrase", 100);
    let mut serp = FakeSerp::default();
    serp.responses.insert(
        ("synthetic phrase".into(), "225".into()),
        json!({"docs": [], "operation_id": "synthetic-operation", "status": "ok"}),
    );
    stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp).unwrap();
    stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp).unwrap();
    assert_eq!(serp.calls.len(), 1);
    assert_eq!(store.spend().unwrap()["yandex_requests"], json!(1));
    assert_eq!(store.counts().unwrap()["_clusters"], 1);
}

#[test]
fn timed_out_batch_cannot_be_paid_for_twice() {
    let dir = tmp("serp-timeout");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    kept(&store, "synthetic phrase", 100);
    let mut serp = FakeSerp::default();
    // `return {}` — the paid operation never reached the response mapping.
    serp.responses.insert(
        ("synthetic phrase".into(), "225".into()),
        serde_json::Value::Null,
    );
    let e1 = stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp)
        .unwrap_err()
        .to_string();
    assert!(e1.contains("incomplete"), "first run: {e1}");
    let e2 = stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp)
        .unwrap_err()
        .to_string();
    assert!(e2.contains("unresolved"), "second run: {e2}");
    assert_eq!(serp.calls.len(), 1);
    let row = store
        .q1("SELECT state FROM serp_fetches", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row["state"], json!("pending"));
}

// regression (review 2026-10-27): a bad port inside a paid SERP doc aborted
// run() after add_serp but before the receipt UPDATE, stranding the row
// 'pending' and masking the real error as 'unresolved paid SERP operation'
// on every rerun. The receipt must land before the url_key pass can fail.
#[test]
fn bad_port_in_paid_serp_doc_keeps_receipt_resolved() {
    let dir = tmp("serp-badport");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    kept(&store, "synthetic phrase", 100);
    let mut serp = FakeSerp::default();
    serp.responses.insert(
        ("synthetic phrase".into(), "225".into()),
        json!({"docs": [{"url": "https://example.invalid:badport/a", "pos": 1}],
               "operation_id": "synthetic-operation", "status": "ok"}),
    );
    let e1 = stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp)
        .unwrap_err()
        .to_string();
    assert!(e1.contains("Port"), "first run: {e1}");
    let row = store
        .q1("SELECT state FROM serp_fetches", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row["state"], json!("complete"));
    let e2 = stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp)
        .unwrap_err()
        .to_string();
    assert!(e2.contains("Port"), "second run: {e2}");
    assert_eq!(serp.calls.len(), 1); // the paid operation is never re-billed
}

#[test]
fn cached_serp_does_not_overwrite_manual_cluster() {
    let dir = tmp("serp-manual");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    kept(&store, "synthetic phrase", 100);
    let cluster = store
        .upsert_cluster(
            "synthetic phrase",
            Some("manual_sya"),
            None,
            Some("https://example.invalid/landing"),
        )
        .unwrap();
    store.assign_cluster("synthetic phrase", cluster).unwrap();
    let mut doc = spiderhead_semantics::store::Row::new();
    doc.insert("url".into(), json!("https://example.invalid/a"));
    doc.insert("domain".into(), json!("example.invalid"));
    doc.insert("pos".into(), json!(1));
    store.add_serp("synthetic phrase", &[doc], None).unwrap();
    store.commit().unwrap();
    let mut serp = FakeSerp::default(); // any call errors: cached run must not fetch
    stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp).unwrap();
    assert!(serp.calls.is_empty());
    let row = &store.phrases(None, false, None).unwrap()[0];
    assert_eq!(row["cluster_id"], json!(cluster));
    let landing = store
        .q1(
            "SELECT landing_url FROM clusters WHERE id=?",
            &[json!(cluster)],
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        landing["landing_url"],
        json!("https://example.invalid/landing")
    );
}

#[test]
fn region_receipts_are_separate_and_can_be_reused() {
    let dir = tmp("serp-regions");
    let store = store(&dir);
    kept(&store, "synthetic phrase", 100);
    let mut serp = FakeSerp::default();
    for region in ["225", "149"] {
        serp.responses.insert(
            ("synthetic phrase".into(), region.into()),
            json!({"docs": [{"url": format!("https://example.invalid/{region}"), "pos": 1}],
                    "operation_id": format!("synthetic-{region}"), "status": "ok"}),
        );
    }
    for region in ["225", "149", "225"] {
        let mut cfg = sem_cfg(&dir);
        cfg.insert("regions".into(), json!([region]));
        stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp).unwrap();
    }
    let regions: Vec<&str> = serp.calls.iter().map(|(_, r, _)| r.as_str()).collect();
    assert_eq!(regions, ["225", "149"]);
    let n = store
        .q1("SELECT COUNT(*) n FROM serp_fetches", &[])
        .unwrap()
        .unwrap();
    assert_eq!(n["n"], json!(2));
}

#[test]
fn manual_landing_without_source_keeps_assignment() {
    let dir = tmp("serp-landing");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    kept(&store, "synthetic phrase", 100);
    let cluster = store
        .upsert_cluster(
            "editorial group",
            None,
            None,
            Some("https://example.invalid/landing"),
        )
        .unwrap();
    store.assign_cluster("synthetic phrase", cluster).unwrap();
    let mut doc = spiderhead_semantics::store::Row::new();
    doc.insert("url".into(), json!("https://example.invalid/a"));
    doc.insert("pos".into(), json!(1));
    store.add_serp("synthetic phrase", &[doc], None).unwrap();
    store.commit().unwrap();
    let mut serp = FakeSerp::default();
    stages::cluster::run(&store, &cfg, 0, 10, "serp", &mut serp).unwrap();
    let row = &store.phrases(None, false, None).unwrap()[0];
    assert_eq!(row["cluster_id"], json!(cluster));
}

// ---------- paid-provider recovery (the reference suite) ----------

#[test]
fn timeout_resumes_paid_task_without_new_submission() {
    let dir = tmp("exact-timeout");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    seed_exact(&store, "seo аудит сайта", &[]);
    let mut arsenkin = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    arsenkin
        .submits
        .push_back(Ok(json!({"task_id": 314, "cost": 3})));
    arsenkin.waits.insert(
        "314".into(),
        VecDeque::from([
            Err(ProviderError::retryable("TIMEOUT already paid")),
            Ok(wordstat_response(
                json!({"seo аудит сайта": {"225": {"quoted": 17, "overal": 0}}}),
            )),
        ]),
    );
    assert_eq!(
        stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap()["pending"],
        json!(1)
    );
    assert_eq!(store.spend().unwrap()["arsenkin"], json!(3));
    assert_eq!(
        stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap()["snapped"],
        json!(1)
    );
    assert_eq!(arsenkin.submitted.len(), 1);
    let wait_ids: Vec<i64> = arsenkin.calls.iter().map(|(_, id)| *id).collect();
    assert_eq!(wait_ids, [314, 314]);
    let task = store
        .q1("SELECT * FROM provider_tasks", &[])
        .unwrap()
        .unwrap();
    assert_eq!(task["state"], json!("complete"));
    assert!(!task["result_json"].is_null());
    assert_eq!(store.spend().unwrap()["arsenkin"], json!(3));
}

#[test]
fn unknown_submission_is_not_automatically_resubmitted() {
    let dir = tmp("exact-unknown");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    seed_exact(&store, "seo аудит сайта", &[]);
    let mut arsenkin = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    arsenkin
        .submits
        .push_back(Err(ProviderError::ambiguous("response lost")));
    for _ in 0..2 {
        assert_eq!(
            stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap()["blocked"],
            json!(1)
        );
    }
    assert_eq!(arsenkin.submitted.len(), 1);
    assert!(arsenkin.calls.is_empty());
}

#[test]
fn interrupted_submission_reservation_prevents_another_charge() {
    let dir = tmp("exact-reserved");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    seed_exact(&store, "seo аудит сайта", &[]);
    // A KeyboardInterrupt mid-set_task leaves the 'submitting' reservation.
    store
        .exec(
            "CREATE TABLE IF NOT EXISTS provider_tasks (\
             id INTEGER PRIMARY KEY, provider TEXT NOT NULL, stage TEXT NOT NULL, \
             region INTEGER NOT NULL, payload_json TEXT NOT NULL, state TEXT NOT NULL, \
             task_id INTEGER, cost REAL, result_json TEXT, error TEXT)",
            &[],
        )
        .unwrap();
    store
        .exec(
            "INSERT INTO provider_tasks(provider,stage,region,payload_json,state) \
             VALUES('arsenkin','exact',225,?,'submitting')",
            &[json!("{\"queries\": [\"seo аудит сайта\"]}")],
        )
        .unwrap();
    store.commit().unwrap();
    let mut arsenkin = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    for _ in 0..2 {
        assert_eq!(
            stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap()["blocked"],
            json!(1)
        );
    }
    assert!(arsenkin.submitted.is_empty());
}

#[test]
fn paid_malformed_payload_is_retained_and_not_resubmitted() {
    let dir = tmp("exact-malformed");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    seed_exact(&store, "seo аудит сайта", &[]);
    let mut arsenkin = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    arsenkin
        .submits
        .push_back(Ok(json!({"task_id": 314, "cost": 3})));
    arsenkin.waits.insert(
        "314".into(),
        VecDeque::from([Ok(json!({"unexpected": "shape"}))]),
    );
    for _ in 0..2 {
        assert_eq!(
            stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap()["snapped"],
            json!(0)
        );
    }
    assert_eq!(arsenkin.submitted.len(), 1);
    assert_eq!(arsenkin.calls.len(), 1);
    let row = store.q1("SELECT exact FROM phrases", &[]).unwrap().unwrap();
    assert!(row["exact"].is_null());
    let task = store
        .q1("SELECT * FROM provider_tasks", &[])
        .unwrap()
        .unwrap();
    assert!(task["result_json"]
        .as_str()
        .unwrap_or_default()
        .contains("unexpected"));
    assert!(!task["error"].is_null());
    assert_eq!(store.spend().unwrap()["arsenkin"], json!(3));
}

// (the Rust port cannot inject a parser fault; the received-state recovery it
// protects is exercised here — raw persisted, no second wait)
#[test]
fn received_task_recovers_without_second_wait() {
    let dir = tmp("exact-received");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    seed_exact(&store, "seo аудит сайта", &[]);
    let raw = wordstat_response(json!({"seo аудит сайта": {"225": {"quoted": 17, "overal": 0}}}));
    store
        .exec(
            "CREATE TABLE IF NOT EXISTS provider_tasks (\
             id INTEGER PRIMARY KEY, provider TEXT NOT NULL, stage TEXT NOT NULL, \
             region INTEGER NOT NULL, payload_json TEXT NOT NULL, state TEXT NOT NULL, \
             task_id INTEGER, cost REAL, result_json TEXT, error TEXT)",
            &[],
        )
        .unwrap();
    store
        .exec(
            "INSERT INTO provider_tasks(provider,stage,region,payload_json,state,\
             task_id,cost,result_json) VALUES('arsenkin','exact',225,?,'received',314,3,?)",
            &[
                json!("{\"queries\": [\"seo аудит сайта\"]}"),
                json!(serde_json::to_string(&raw).unwrap()),
            ],
        )
        .unwrap();
    store.commit().unwrap();
    let mut arsenkin = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    assert_eq!(
        stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap()["snapped"],
        json!(1)
    );
    assert!(arsenkin.submitted.is_empty());
    assert!(arsenkin.calls.is_empty());
}

#[test]
fn zero_null_and_existing_measurements_are_distinct() {
    let dir = tmp("exact-distinct");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    seed_exact(
        &store,
        "seo аудит сайта",
        &[
            ("lemma_group".into(), json!(1)),
            ("exact".into(), json!(40)),
            ("quoted".into(), json!(90)),
        ],
    );
    seed_exact(
        &store,
        "seo аудит сайта новый",
        &[("lemma_group".into(), json!(1))],
    );
    seed_exact(
        &store,
        "seo аудит сайта маленький",
        &[("lemma_group".into(), json!(2))],
    );
    let mut arsenkin = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    arsenkin
        .submits
        .push_back(Ok(json!({"task_id": 314, "cost": 3})));
    arsenkin.waits.insert(
        "314".into(),
        VecDeque::from([Ok(wordstat_response(json!({
            "seo аудит сайта новый": {"225": {"quoted": 17, "overal": 0}},
            "seo аудит сайта маленький": {"225": {"quoted": 5}},
        })))]),
    );
    assert_eq!(
        stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap()["snapped"],
        json!(1)
    );
    let rows: std::collections::HashMap<String, (serde_json::Value, serde_json::Value)> = store
        .phrases(None, false, None)
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["norm"].as_str().unwrap().to_string(),
                (r["exact"].clone(), r["quoted"].clone()),
            )
        })
        .collect();
    assert_eq!(rows["seo аудит сайта"], (json!(40), json!(90)));
    assert_eq!(rows["seo аудит сайта новый"], (json!(0), json!(17)));
    assert_eq!(
        rows["seo аудит сайта маленький"],
        (serde_json::Value::Null, json!(5))
    );
}

#[test]
fn real_demand_prevents_exact_dead_cutoff() {
    let dir = tmp("exact-demand");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    seed_exact(&store, "seo аудит сайта", &[("pos_g".into(), json!(8))]);
    store
        .exec(
            "UPDATE phrases SET base=5 WHERE norm='seo аудит сайта'",
            &[],
        )
        .unwrap();
    store.commit().unwrap();
    let mut arsenkin = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    arsenkin
        .submits
        .push_back(Ok(json!({"task_id": 314, "cost": 3})));
    arsenkin.waits.insert(
        "314".into(),
        VecDeque::from([Ok(wordstat_response(
            json!({"seo аудит сайта": {"225": {"quoted": 17, "overal": 0}}}),
        ))]),
    );
    let result = stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap();
    assert_eq!(result["dead"], json!(0));
    let row = store
        .q1("SELECT status FROM phrases", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row["status"], json!("kept"));
}

#[test]
fn collect_normalizes_queue_and_keeps_stopped_results() {
    let dir = tmp("collect-stop");
    let store = store(&dir);
    let mut cfg = sem_cfg(&dir);
    cfg.insert("preset".into(), json!("generic"));
    cfg.insert("stop".into(), json!("мусор"));
    cfg.insert("max_depth".into(), json!(1));
    let mut demand = FakeDemand::default();
    demand
        .expansions
        .entry(("seo аудит сайта".into(), "225".into()))
        .or_default()
        .push_back(Ok(ExpandResult {
            pool: [
                ("SEO-аудит-сайта".to_string(), 100),
                ("seo аудит сайта".to_string(), 90),
                ("мусор аудит".to_string(), 80),
            ]
            .into_iter()
            .collect(),
            origin: Default::default(),
            results: 3,
            associations: 0,
        }));
    stages::collect::run(
        &store,
        &cfg,
        &["SEO-аудит-сайта".to_string(), "seo аудит сайта".to_string()],
        0,
        100000,
        false,
        &mut demand,
    )
    .unwrap();
    assert_eq!(demand.calls.len(), 1);
    let row = store
        .q1("SELECT * FROM phrases WHERE norm='мусор аудит'", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row["status"], json!("dropped"));
    assert!(!row["reason"].is_null());
    assert_eq!(row["reason_stage"], json!("collect"));
    assert_eq!(store.spend().unwrap()["yandex_requests"], json!(1));
}

#[test]
fn collect_ambiguous_response_blocks_automatic_retry() {
    let dir = tmp("collect-ambiguous");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    let mut demand = FakeDemand::default();
    demand
        .expansions
        .entry(("seo аудит сайта".into(), "225".into()))
        .or_default()
        .push_back(Err(ProviderError::ambiguous("response lost")));
    for resume in [false, true] {
        let result = stages::collect::run(
            &store,
            &cfg,
            &["seo аудит сайта".to_string()],
            0,
            100000,
            resume,
            &mut demand,
        )
        .unwrap();
        assert_eq!(result["blocked"], json!(1), "resume={resume}");
    }
    assert_eq!(demand.calls.len(), 1);
    assert!(!store.is_expanded("seo аудит сайта").unwrap());
}

// (a dropped `edges` table plays the role of the monkeypatched `add_edge` failure)
#[test]
fn successful_collect_cost_survives_later_processing_error() {
    let dir = tmp("collect-cost");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    let mut demand = FakeDemand::default();
    demand
        .expansions
        .entry(("seo аудит сайта".into(), "225".into()))
        .or_default()
        .push_back(Ok(ExpandResult {
            pool: [("аудит сайта другой".to_string(), 80)]
                .into_iter()
                .collect(),
            origin: Default::default(),
            results: 1,
            associations: 0,
        }));
    store.exec("DROP TABLE edges", &[]).unwrap();
    store.commit().unwrap();
    assert!(stages::collect::run(
        &store,
        &cfg,
        &["seo аудит сайта".to_string()],
        0,
        100000,
        false,
        &mut demand,
    )
    .is_err());
    store.rollback().unwrap();
    assert_eq!(store.spend().unwrap()["yandex_requests"], json!(1));
    let row = store
        .q1("SELECT state FROM collect_attempts", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row["state"], json!("submitting"));
}

// (an ambiguous provider error stands in for KeyboardInterrupt: the attempt
// stays blocked, progress is not rechecked and missing is not zero)
#[test]
fn synonym_progress_survives_interrupt_and_missing_is_not_zero() {
    let dir = tmp("syn-missing");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    std::fs::write(
        dir.join("synonyms.txt"),
        "аудит сайта большой\nаудит сайта маленький\n",
    )
    .unwrap();
    let mut demand = FakeDemand::default();
    demand.tops.insert(
        ("аудит сайта большой".into(), "225".into()),
        Ok(json!({"totalCount": serde_json::Value::Null})),
    );
    demand.tops.insert(
        ("аудит сайта маленький".into(), "225".into()),
        Err(ProviderError::ambiguous("response lost")),
    );
    let r1 = stages::synonyms::run(&store, &cfg, &mut demand).unwrap();
    assert_eq!(r1["checked"], json!(1));
    let row = store
        .q1(
            "SELECT * FROM phrases WHERE norm='аудит сайта большой'",
            &[],
        )
        .unwrap()
        .unwrap();
    assert!(row["base"].is_null());
    assert_eq!(row["status"], json!("review"));
    assert_eq!(store.spend().unwrap()["yandex_requests"], json!(1));
    let r2 = stages::synonyms::run(&store, &cfg, &mut demand).unwrap();
    assert_eq!(r2["checked"], json!(0));
    assert_eq!(demand.calls.len(), 2);
}

// ---------- cluster method / url keys / pipeline (tests/test_semantics_*the reference suite) ----------

#[test]
fn louvain_groups_cached_serp_without_a_provider() {
    let dir = tmp("louvain-cached");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    for (topic, phrases) in [
        (
            "audit",
            vec![
                "seo аудит сайта",
                "аудит сайта онлайн",
                "технический аудит сайта",
            ],
        ),
        (
            "crawl",
            vec!["краулер сайта", "краулер для seo", "парсер сайта для seo"],
        ),
    ] {
        for phrase in phrases {
            kept(&store, phrase, 100);
            let docs: Vec<spiderhead_semantics::store::Row> = (0..4)
                .map(|n| {
                    let mut d = spiderhead_semantics::store::Row::new();
                    d.insert(
                        "url".into(),
                        json!(format!("https://example.invalid/{topic}/{n}")),
                    );
                    d.insert("pos".into(), json!(n + 1));
                    d
                })
                .collect();
            store.add_serp(phrase, &docs, None).unwrap();
        }
    }
    store.commit().unwrap();
    let mut serp = FakeSerp::default(); // any fetch errors: cached run must not call
    let result = stages::cluster::run(&store, &cfg, 0, 10, "louvain", &mut serp).unwrap();
    assert_eq!(result["method"], json!("louvain"));
    assert_eq!(result["requests"], json!(0));
    assert!(serp.calls.is_empty());
    let ids: std::collections::HashMap<String, serde_json::Value> = store
        .phrases(None, false, None)
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["norm"].as_str().unwrap().to_string(),
                r["cluster_id"].clone(),
            )
        })
        .collect();
    let audit: std::collections::HashSet<_> = [
        "seo аудит сайта",
        "аудит сайта онлайн",
        "технический аудит сайта",
    ]
    .iter()
    .map(|p| ids[*p].clone())
    .collect();
    let crawl: std::collections::HashSet<_> =
        ["краулер сайта", "краулер для seo", "парсер сайта для seo"]
            .iter()
            .map(|p| ids[*p].clone())
            .collect();
    assert_eq!(audit.len(), 1);
    assert!(audit.is_disjoint(&crawl));
}

#[test]
fn unknown_cluster_method_is_rejected() {
    let dir = tmp("cluster-method");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    let mut serp = FakeSerp::default();
    let e = stages::cluster::run(&store, &cfg, 0, 10, "random", &mut serp)
        .unwrap_err()
        .to_string();
    assert!(e.contains("serp or louvain"), "{e}");
}

#[test]
fn host_normalization_preserves_case_sensitive_pages() {
    assert_eq!(
        stages::cluster::url_key("https://WWW.EXAMPLE.INVALID/Products/?x=1#part").unwrap(),
        "example.invalid/Products"
    );
    assert_ne!(
        stages::cluster::url_key("https://example.invalid/Products").unwrap(),
        stages::cluster::url_key("https://example.invalid/products").unwrap()
    );
}

#[test]
fn hub_names_in_paths_or_unrelated_hosts_are_not_hubs() {
    assert!(!stages::cluster::is_hub_url("https://example.invalid/google.com").unwrap());
    assert!(!stages::cluster::is_hub("wikipedia.org.example.invalid"));
    assert!(stages::cluster::is_hub_url("https://en.wikipedia.org/page").unwrap());
    assert!(stages::cluster::is_hub("portal.gov.ru"));
}

#[test]
fn collect_retries_a_transient_non_quota_failure_instead_of_poisoning_the_seed() {
    let dir = tmp("collect-transient");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    let mut demand = FakeDemand::default();
    let q = demand
        .expansions
        .entry(("краулер сайта".into(), "225".into()))
        .or_default();
    q.push_back(Err(ProviderError::retryable(
        "topRequests 502: temporary upstream error",
    )));
    q.push_back(Ok(ExpandResult {
        pool: [("краулер сайта онлайн".to_string(), 80)]
            .into_iter()
            .collect(),
        origin: [("краулер сайта онлайн".to_string(), "results".to_string())]
            .into_iter()
            .collect(),
        results: 1,
        associations: 0,
    }));
    let first = stages::collect::run(
        &store,
        &cfg,
        &["краулер сайта".to_string()],
        1,
        100000,
        false,
        &mut demand,
    )
    .unwrap();
    assert_eq!(first["quota_hit"], json!(false));
    assert_eq!(first["expanded"], json!(0));
    assert!(!store.is_expanded("краулер сайта").unwrap());
    let second = stages::collect::run(
        &store,
        &cfg,
        &["краулер сайта".to_string()],
        1,
        100000,
        false,
        &mut demand,
    )
    .unwrap();
    assert_eq!(second["quota_hit"], json!(false));
    assert_eq!(second["expanded"], json!(1));
    assert_eq!(demand.calls.len(), 2);
    let row = store
        .q1(
            "SELECT base FROM phrases WHERE norm='краулер сайта онлайн'",
            &[],
        )
        .unwrap()
        .unwrap();
    assert_eq!(row["base"], json!(80));
}

// (reference monkeypatches detect_homonyms; a real two-component homonym plays
// the same role here)
#[test]
fn graph_does_not_demote_observed_search_demand() {
    let dir = tmp("graph-demand");
    let store = store(&dir);
    let mut cfg = sem_cfg(&dir);
    cfg.insert("preset".into(), json!("seo"));
    // `ключ` splits into two unrelated intentless components (замок vs токен).
    for phrase in [
        "ключ замка входной",
        "ключ замка гараж",
        "ключ api токен",
        "ключ токен доступ",
    ] {
        store.upsert(phrase, Some(0), 0, None, "225").unwrap();
        store
            .set_status(phrase, "kept", Some("ok"), Some("clean"))
            .unwrap();
    }
    store
        .set_fields("ключ замка входной", &[("pos_g".into(), json!(3))])
        .unwrap();
    store.commit().unwrap();
    stages::graph::run(&store, &cfg, &dir).unwrap();
    let row = store
        .q1(
            "SELECT status FROM phrases WHERE norm='ключ замка входной'",
            &[],
        )
        .unwrap()
        .unwrap();
    assert_eq!(row["status"], json!("kept"));
    let flagged = store
        .q1("SELECT COUNT(*) n FROM phrases WHERE status='review'", &[])
        .unwrap()
        .unwrap();
    assert_eq!(flagged["n"], json!(3));
}

#[test]
fn normalize_produces_a_stable_deduplication_key() {
    for (raw, expected) in [
        ("  Ёлка---аудит!!! ", "елка аудит"),
        ("SEO-аудит/2026", "seo аудит 2026"),
        ("краулер\n\tдля   сайта", "краулер для сайта"),
    ] {
        assert_eq!(
            spiderhead_semantics::norm::normalize(raw),
            expected,
            "{raw:?}"
        );
    }
}

#[test]
fn presets_and_explicit_patterns() {
    let seo = spiderhead_semantics::norm::Filters::new(None, None, None, false, "seo").unwrap();
    assert_eq!(
        seo.classify("краулер сайта", Some(100), 20).unwrap(),
        ("kept", "commercial")
    );
    assert_eq!(
        seo.classify("как провести seo аудит", Some(40), 20)
            .unwrap(),
        ("kept", "info")
    );
    assert_eq!(
        seo.classify("seo курсы обучение", Some(100), 20).unwrap().0,
        "dropped"
    );
    assert_eq!(
        seo.classify("ремонт велосипеда", Some(100), 20).unwrap().0,
        "review"
    );
    let generic = spiderhead_semantics::norm::Filters::new(
        None,
        Some("forbidden"),
        Some("guide"),
        false,
        "generic",
    )
    .unwrap();
    assert_eq!(
        generic
            .classify("arbitrary commercial phrase", Some(100), 20)
            .unwrap(),
        ("kept", "commercial")
    );
    assert_eq!(
        generic
            .classify("guide to arbitrary phrase", Some(100), 20)
            .unwrap(),
        ("kept", "info")
    );
    assert_eq!(
        generic
            .classify("forbidden arbitrary phrase", Some(100), 20)
            .unwrap()
            .0,
        "dropped"
    );
    let e = match spiderhead_semantics::norm::Filters::new(None, None, None, false, "missing") {
        Err(e) => e.to_string(),
        Ok(_) => panic!("unknown preset accepted"),
    };
    assert!(e.contains("unknown preset"), "{e}");
}

#[test]
fn billed_invalid_task_id_retains_charge_without_resubmission() {
    let dir = tmp("exact-billed");
    let store = store(&dir);
    let cfg = sem_cfg(&dir);
    seed_exact(&store, "seo аудит сайта", &[]);
    let mut arsenkin = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    arsenkin.submits.push_back(Err(ProviderError::with_payload(
        "billed without an ID",
        json!({"task_id": serde_json::Value::Null, "cost": 7}),
    )));
    for _ in 0..2 {
        assert_eq!(
            stages::exact::run(&store, &cfg, false, 0, &mut arsenkin).unwrap()["blocked"],
            json!(1)
        );
    }
    assert_eq!(arsenkin.submitted.len(), 1);
    assert_eq!(store.spend().unwrap()["arsenkin"], json!(7));
    let task = store
        .q1("SELECT cost FROM provider_tasks", &[])
        .unwrap()
        .unwrap();
    assert_eq!(task["cost"].as_f64(), Some(7.0));
}
