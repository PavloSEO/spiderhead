//! Google storage: DataForSEO import per endpoint, idempotency, topic map and
//! the cross-engine origin flag.

use serde_json::{json, Value};
use spiderhead_semantics::google::{import_dataforseo, import_topics};
use spiderhead_semantics::store::Store;

fn kd(keyword: &str, volume: i64) -> Value {
    json!({"keyword": keyword, "location_code": 2276, "language_code": "de",
      "keyword_info": {"search_volume": volume, "cpc": 1.5, "competition": 0.2,
        "competition_level": "LOW", "monthly_searches": [{"year": 2026, "month": 9, "search_volume": volume}]},
      "keyword_properties": {"keyword_difficulty": 12},
      "search_intent_info": {"main_intent": "commercial"}})
}

fn task(id: &str, path: &[&str], data: Value, result: Value) -> Value {
    json!({"id": id, "cost": 0.5, "path": path, "data": data, "result": result})
}

fn resp(tasks: Vec<Value>) -> Value {
    json!({"status_code": 20000, "tasks": tasks})
}

fn p(e: &str) -> Vec<&str> {
    vec!["v3", "dataforseo_labs", "google", e, "live"]
}

fn write(dir: &std::path::Path, name: &str, v: &Value) {
    std::fs::write(dir.join(name), v.to_string()).unwrap();
}

fn fixture_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("google-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let labs = json!({"api": "dataforseo_labs", "location_code": 2276, "language_code": "de"});
    write(
        &dir,
        "ranked.json",
        &resp(vec![task(
            "r1",
            &p("ranked_keywords"),
            json!({"target": "example.test", "location_code": 2276, "language_code": "de"}),
            json!([{"target": "example.test", "items": [{"keyword_data": kd("Token Beratung", 90),
          "ranked_serp_element": {"serp_item": {"type": "organic", "rank_group": 3, "rank_absolute": 4,
            "domain": "example.test", "url": "https://example.test/de/a", "etv": 7.5}}}]}]),
        )]),
    );
    // Collector wrapper, path truncated to v3: endpoint comes from data.
    write(
        &dir,
        "ml-sugg.json",
        &json!({"key": "k", "payload": {}, "resp": resp(vec![task("s1", &["v3"],
        json!({"api": "dataforseo_labs", "function": "keyword_suggestions", "keyword": "token", "location_code": 2276, "language_code": "de"}),
        json!([{"seed_keyword_data": kd("token", 10), "items": [kd("token kaufen", 50), kd("token", 10)]}]))])}),
    );
    write(
        &dir,
        "ideas.json",
        &resp(vec![task(
            "i1",
            &p("keyword_ideas"),
            labs.clone(),
            json!([{"items": [kd("idee", 5)]}]),
        )]),
    );
    write(
        &dir,
        "related.json",
        &resp(vec![task(
            "rl1",
            &p("related_keywords"),
            labs.clone(),
            json!([{"items": [{"keyword_data": kd("verwandt", 7), "depth": 1}]}]),
        )]),
    );
    write(
        &dir,
        "comp.json",
        &resp(vec![task(
            "c1",
            &p("competitors_domain"),
            json!({"target": "example.test", "location_code": 2276, "language_code": "de"}),
            json!([{"target": "example.test", "items": [{"domain": "rival.test", "avg_position": 4.0,
          "intersections": 3, "metrics": {"organic": {"etv": 2.0, "count": 3}},
          "full_domain_metrics": {"organic": {"etv": 9.0, "count": 30}}}]}]),
        )]),
    );
    let ads = |k: &str| {
        json!({"keyword": k, "location_code": 2276, "language_code": "de",
        "competition": "LOW", "competition_index": 20, "search_volume": 300, "cpc": 2.0,
        "low_top_of_page_bid": 1.0, "high_top_of_page_bid": 3.0, "monthly_searches": []})
    };
    write(
        &dir,
        "vol.json",
        &resp(vec![
            task(
                "v1",
                &["v3", "keywords_data", "google_ads", "search_volume", "live"],
                json!({"keywords": ["token beratung"], "location_code": 2276, "language_code": "de"}),
                json!([ads("Token Beratung")]),
            ),
            task(
                "v2",
                &[
                    "v3",
                    "keywords_data",
                    "google_ads",
                    "keywords_for_keywords",
                    "live",
                ],
                json!({"keywords": ["x"], "location_code": 2276, "language_code": "de"}),
                json!([ads("ads idee")]),
            ),
        ]),
    );
    write(
        &dir,
        "serp.json",
        &json!([task(
            "p1",
            &["v3", "serp", "google", "organic", "live", "advanced"],
            json!({"keyword": "token beratung", "location_code": 2276, "language_code": "de", "device": "mobile"}),
            json!([{"keyword": "token beratung", "location_code": 2276, "language_code": "de",
          "datetime": "2026-11-06 05:00:00 +00:00", "se_results_count": 10, "item_types": ["organic", "ai_overview"],
          "items": [
            {"type": "ai_overview", "markdown": "text", "references": [{"domain": "cite.test", "url": "https://cite.test/x", "title": "T"}]},
            {"type": "organic", "rank_group": 1, "rank_absolute": 2, "domain": "example.test", "url": "https://example.test/de/a", "title": "A"},
            {"type": "people_also_ask", "items": [{"type": "people_also_ask_element", "title": "Frage?"}]}]}])
        )]),
    );
    write(
        &dir,
        "locations.json",
        &resp(vec![task(
            "l1",
            &["v3", "serp", "google", "locations"],
            json!({}),
            json!([{"location_code": 1}]),
        )]),
    );
    std::fs::write(dir.join("notes.json"), "[\"just\", \"strings\"]").unwrap();
    dir
}

fn n(store: &Store, sql: &str) -> i64 {
    store.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn imports_every_endpoint_and_is_idempotent() {
    let dir = fixture_dir("endpoints");
    let store = Store::open_memory().unwrap();
    let doc = import_dataforseo(&store, &dir).unwrap();
    assert_eq!(doc["tasks_imported"], 9);
    assert_eq!(doc["files_not_responses"], 1);
    assert_eq!(doc["cost_usd"], 4.5);
    // token beratung (labs + ads share one row), token kaufen, token, idee, verwandt, ads idee
    assert_eq!(n(&store, "SELECT COUNT(*) FROM g_phrases"), 6);
    let (vol, level): (i64, String) = store
        .conn
        .query_row(
            "SELECT volume, competition_level FROM g_phrases WHERE norm='token beratung'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((vol, level.as_str()), (300, "LOW"));
    assert_eq!(
        n(&store, "SELECT COUNT(*) FROM g_phrases WHERE country='DE'"),
        6
    );
    assert_eq!(
        n(
            &store,
            "SELECT COUNT(*) FROM g_rankings WHERE position=4 AND etv=7.5"
        ),
        1
    );
    assert_eq!(
        n(
            &store,
            "SELECT COUNT(*) FROM g_competitors WHERE domain='rival.test' AND domain_kw_count=30"
        ),
        1
    );
    assert_eq!(
        n(
            &store,
            "SELECT COUNT(*) FROM g_serp WHERE device='mobile' AND type='organic'"
        ),
        1
    );
    assert_eq!(n(&store, "SELECT COUNT(*) FROM g_serp_features WHERE ai_overview=1 AND cited_json LIKE '%cite.test%' AND paa_json LIKE '%Frage%'"), 1);
    assert_eq!(n(&store, "SELECT COUNT(*) FROM provider_runs"), 9);

    let again = import_dataforseo(&store, &dir).unwrap();
    assert_eq!(again["tasks_imported"], 0);
    assert_eq!(again["tasks_already_imported"], 9);
    assert_eq!(n(&store, "SELECT COUNT(*) FROM g_phrases"), 6);
    assert_eq!(n(&store, "SELECT COUNT(*) FROM g_serp"), 1);
}

#[test]
fn topics_link_engines_and_report_gaps() {
    let dir = fixture_dir("topics");
    let store = Store::open_memory().unwrap();
    import_dataforseo(&store, &dir).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO phrases(norm,base) VALUES('token beratung',5)",
            [],
        )
        .unwrap();
    let csv = dir.join("map.csv");
    std::fs::write(&csv, "\u{feff}topic_id,en_topic,page_type,en_landing,de_landing,fr_landing,demand_de,main_kw_de\n\
        T1,token-consulting,service,/services/t,/de/services/t,,90,Token Beratung\n\
        T2,only-de,blog,,/de/blog/x,,0,\n").unwrap();
    let doc = import_topics(&store, &csv).unwrap();
    assert_eq!(doc["topics"], 2);
    assert_eq!(doc["missing_landings"], 1);
    let gap: String = store
        .conn
        .query_row("SELECT locale FROM v_topic_gaps", [], |r| r.get(0))
        .unwrap();
    assert_eq!(gap, "fr");
    let origin: String = store
        .conn
        .query_row(
            "SELECT origin FROM v_topic_phrases WHERE topic_id='T1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(origin, "both");
}

#[test]
fn csv_core_keeps_unicode_and_extra_locations() {
    let dir = fixture_dir("csv");
    let store = Store::open_memory().unwrap();
    let file = dir.join("ml-core-de.csv");
    std::fs::write(&file, "\u{feff}keyword,sv,location,kd,intent,en_topic,page_type,cpc,sv_AT\nÜber Token,100,DE,5,commercial,t,service,1.5,40\nステーブルコイン,10,DE,,,t,service,,\n").unwrap();
    let doc = spiderhead_semantics::google::import_google_csv(&store, &file).unwrap();
    assert_eq!(doc["rows"], 3);
    assert_eq!(n(&store, "SELECT COUNT(*) FROM g_phrases WHERE norm='über token' AND location_code IN (2276,2040)"), 2);
    assert_eq!(
        n(
            &store,
            "SELECT COUNT(*) FROM g_phrases WHERE norm='ステーブルコイン' AND language='de'"
        ),
        1
    );
    assert_eq!(
        spiderhead_semantics::norm::normalize("ステーブルコイン Über-Token"),
        "ステーブルコイン über token"
    );
}
