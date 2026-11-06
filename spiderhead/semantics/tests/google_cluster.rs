//! SERP clustering of Google phrases and task-queue downloads.

use serde_json::{json, Value};
use spiderhead_semantics::google::{
    cluster_google, clusters_google, import_dataforseo, import_topics, url_key,
};
use spiderhead_semantics::store::Store;

fn items(urls: &[&str]) -> Vec<Value> {
    urls.iter()
        .enumerate()
        .map(|(i, u)| {
            let host = u.split('/').nth(2).unwrap_or("");
            json!({"type": "organic", "rank_group": i + 1, "rank_absolute": i + 1,
                   "domain": host, "url": u, "title": "t"})
        })
        .collect()
}

/// One bare task-queue download, the way a `task_get` response is saved.
fn task_file(dir: &std::path::Path, id: &str, kw: &str, lang: &str, urls: &[&str], ai: bool) {
    let mut its = items(urls);
    if ai {
        its.insert(0, json!({"type": "ai_overview", "markdown": "m"}));
    }
    let doc = json!({"id": id, "status_code": 20000, "cost": 0.0006, "path": ["v3"],
        "data": {"api": "serp", "function": "task_get", "se": "google", "keyword": kw,
                 "language_code": lang, "location_code": 2840, "device": "desktop"},
        "result": [{"keyword": kw, "language_code": lang, "location_code": 2840,
                    "datetime": "2026-11-06 07:00:00 +00:00", "items": its}]});
    std::fs::write(dir.join(format!("{id}.json")), doc.to_string()).unwrap();
}

fn setup(tag: &str) -> (Store, std::path::PathBuf) {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("gcluster-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a = [
        "https://www.one.test/a/",
        "http://two.test/b?x=1",
        "https://three.test/c",
        "https://four.test/d#frag",
        "https://five.test/e",
    ];
    // Same four pages as `a` (normalization), plus one new one.
    let b = [
        "https://one.test/a",
        "https://two.test/b",
        "https://three.test/c",
        "https://four.test/d",
        "https://ours.test/x",
    ];
    // Three shared pages with `a`: below the default link threshold.
    let c = [
        "https://one.test/a",
        "https://two.test/b",
        "https://three.test/c",
        "https://nine.test/z",
    ];
    task_file(&dir, "t-a", "token development", "en", &a, true);
    task_file(&dir, "t-b", "token developer", "en", &b, false);
    task_file(&dir, "t-c", "token wallet", "en", &c, false);
    task_file(
        &dir,
        "t-d",
        "lonely phrase",
        "en",
        &["https://solo.test/"],
        false,
    );
    // Same pages in another language never link to the English phrases.
    task_file(&dir, "t-e", "entwicklung", "de", &a, false);
    std::fs::write(dir.join("posted.jsonl"), "{\"id\": \"t-a\"}\n").unwrap();
    std::fs::write(dir.join("index.json"), "{\"note\": \"helper\"}").unwrap();
    let store = Store::open_memory().unwrap();
    let doc = import_dataforseo(&store, &dir).unwrap();
    assert_eq!(doc["tasks_imported"], 5);
    assert_eq!(doc["files_not_responses"], 1);
    // Volumes decide the cluster name.
    for (kw, v) in [
        ("token development", 10),
        ("token developer", 90),
        ("token wallet", 5),
    ] {
        store
            .conn
            .execute(
                "INSERT INTO g_phrases(norm,language,location_code,volume,intent,extra_json) \
                 VALUES(?,'en',2840,?,'commercial','{\"jev_page\":\"service\",\"jev_verdict\":\"keep\"}')",
                rusqlite::params![kw, v],
            )
            .unwrap();
    }
    (store, dir)
}

fn project(dir: &std::path::Path) -> String {
    std::fs::write(
        dir.join("project.json"),
        json!({"site": {"host": "ours.test"}}).to_string(),
    )
    .unwrap();
    dir.to_string_lossy().into_owned()
}

#[test]
fn task_downloads_import_idempotently_and_skip_pending() {
    let (store, dir) = setup("import");
    let again = import_dataforseo(&store, &dir).unwrap();
    assert_eq!(again["tasks_imported"], 0);
    assert_eq!(again["tasks_already_imported"], 5);
    std::fs::write(
        dir.join("queued.json"),
        json!({"id": "q1", "status_code": 40602, "data": {"api": "serp"}, "result": null})
            .to_string(),
    )
    .unwrap();
    // A queued task has no result array: not a response at all.
    let doc = import_dataforseo(&store, &dir).unwrap();
    assert_eq!(doc["tasks_imported"], 0);
    let n: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM g_serp", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 5 + 5 + 5 + 4 + 1);
}

#[test]
fn clusters_by_shared_urls_per_language_and_replaces_on_rerun() {
    let (store, dir) = setup("cluster");
    let project = project(&dir);
    let doc = cluster_google(&store, &project, None, None, 4, 10).unwrap();
    assert_eq!(doc["our_domain"], "ours.test");
    let scopes = doc["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 2);
    let en = &scopes[1];
    assert_eq!(
        (en["language"].as_str(), en["phrases"].as_i64()),
        (Some("en"), Some(4))
    );
    // {development, developer}, {wallet}, {lonely}
    assert_eq!(
        (en["clusters"].as_i64(), en["singletons"].as_i64()),
        (Some(3), Some(2))
    );

    let page = clusters_google(&store, Some("en"), Some(2840), 0, 2).unwrap();
    assert_eq!(page["total"], 3);
    assert_eq!(page["next_offset"], 2);
    let top = &page["clusters"][0];
    assert_eq!(top["name"], "token developer");
    assert_eq!(top["size"], 2);
    assert_eq!(top["volume_sum"], 100);
    assert_eq!(top["ai_overview_share"], 0.5);
    assert_eq!(top["page_type"], "service");
    assert_eq!(top["our_position"], 5);
    assert_eq!(top["top_domains"][0]["n"], 2);

    // Lower threshold links the wallet phrase; re-run replaces, never duplicates.
    let doc = cluster_google(&store, &project, Some("en"), None, 3, 10).unwrap();
    assert_eq!(doc["scopes"][0]["clusters"], 2);
    let n: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM g_clusters", [], |r| r.get(0))
        .unwrap();
    let m: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM g_cluster_members", [], |r| r.get(0))
        .unwrap();
    // 2 English clusters plus the untouched German one.
    assert_eq!((n, m), (3, 5));

    // Limiting to the top 2 results leaves too little overlap to link anything.
    let doc = cluster_google(&store, &project, Some("en"), None, 3, 2).unwrap();
    assert_eq!(doc["scopes"][0]["clusters"], 4);
}

#[test]
fn clusters_expose_topics_and_status_counts() {
    let (store, dir) = setup("topics");
    let csv = dir.join("map.csv");
    std::fs::write(
        &csv,
        "topic_id,en_topic,page_type,en_landing,main_kw_en\nT1,tokens,service,/t,token developer\n",
    )
    .unwrap();
    import_topics(&store, &csv).unwrap();
    cluster_google(&store, &project(&dir), Some("en"), None, 4, 10).unwrap();
    let page = clusters_google(&store, Some("en"), None, 0, 10).unwrap();
    assert_eq!(page["clusters"][0]["topics"][0]["name"], "tokens");
    let status = spiderhead_semantics::google::status(&store);
    assert_eq!(status["clusters"][0]["clusters"], 3);
}

#[test]
fn url_key_drops_scheme_www_query_and_slash() {
    assert_eq!(
        url_key("HTTPS://www.Ex.test/A/?q=1#f").as_deref(),
        Some("ex.test/A")
    );
    assert_eq!(url_key("http://ex.test/").as_deref(), Some("ex.test"));
    assert_eq!(url_key(""), None);
}
