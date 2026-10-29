use super::*;
use crate::host::FakeHost;

/// A fresh scratch directory and a journal file inside it, unique per test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-core-runlog-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn journal_in(dir: &Path) -> Journal {
    Journal::at(Some(dir.join("runs.jsonl")), "cli")
}

fn args(pairs: Value) -> Args {
    pairs.as_object().cloned().unwrap_or_default()
}

fn entries_text(dir: &Path) -> String {
    fs::read_to_string(dir.join("runs.jsonl")).unwrap_or_default()
}

fn fresh(dir: &Path) -> Journal {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).expect("scratch dir");
    journal_in(dir)
}

#[test]
fn a_successful_call_is_recorded() {
    let dir = scratch("success");
    let journal = journal_in(&dir);
    let out: Result<(), String> = journal.journal(
        "parse",
        &args(json!({"url": "https://example.com/"})),
        |facts| {
            facts.insert("count".into(), json!(3));
            Ok(())
        },
    );
    assert!(out.is_ok());
    let entry = &journal.read_entries(100)[0];
    assert_eq!(entry["tool"], "parse");
    assert_eq!(entry["interface"], "cli");
    assert_eq!(entry["ok"], true);
    assert_eq!(entry["count"], 3);
    assert!(entry["duration_s"].as_f64().unwrap() >= 0.0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_failing_call_is_recorded_and_the_error_still_propagates() {
    let dir = scratch("failure");
    let journal = journal_in(&dir);
    let out: Result<(), String> = journal.journal("parse", &args(json!({"url": "x"})), |_| {
        Err("boom".to_owned())
    });
    assert_eq!(out, Err("boom".to_owned()));
    let entry = &journal.read_entries(100)[0];
    assert_eq!(entry["ok"], false);
    assert!(entry["error"].as_str().unwrap().contains("boom"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn credentials_never_reach_the_journal() {
    for name in [
        "api_key",
        "token",
        "AUTH",
        "password",
        "client_secret",
        "credentials",
    ] {
        let dir = scratch("creds");
        let journal = journal_in(&dir);
        let call = args(json!({name: "super-secret-value", "url": "https://e.com/"}));
        let _: Result<(), String> = journal.journal("serp", &call, |_| Ok(()));
        let text = entries_text(&dir);
        assert!(!text.contains("super-secret-value"), "{name} leaked");
        assert!(text.contains("[redacted]"), "{name} not marked");
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn long_values_are_shortened_not_dropped() {
    let dir = scratch("long");
    let journal = fresh(&dir);
    let call = args(json!({"html": "x".repeat(5000)}));
    let _: Result<(), String> = journal.journal("parse", &call, |_| Ok(()));
    let value = journal.read_entries(100)[0]["arguments"]["html"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(value.ends_with('…'));
    assert!(value.chars().count() < 500);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn long_lists_are_summarised() {
    let urls: Vec<String> = (0..50).map(|n| format!("u{n}")).collect();
    let value = redact(&json!(urls));
    let items = value.as_array().unwrap();
    assert_eq!(items.len(), 11);
    assert!(items[10].as_str().unwrap().contains("40 more"));
}

#[test]
fn the_same_call_gets_the_same_fingerprint() {
    let first = fingerprint("parse", &args(json!({"url": "https://e.com/", "depth": 2})));
    let again = fingerprint("parse", &args(json!({"depth": 2, "url": "https://e.com/"})));
    let other = fingerprint(
        "parse",
        &args(json!({"url": "https://e.com/x", "depth": 2})),
    );
    assert_eq!(first, again, "argument order must not change identity");
    assert_ne!(first, other);
}

// The progress argument is display-only and must not change the identity.
#[test]
fn display_only_arguments_do_not_change_identity() {
    let quiet = fingerprint("crawl_site", &args(json!({"url": "https://e.com/"})));
    let loud = fingerprint(
        "crawl_site",
        &args(json!({"url": "https://e.com/", "progress": true})),
    );
    assert_eq!(quiet, loud);
}

#[test]
fn logging_can_be_switched_off() {
    let host = FakeHost {
        env: BTreeMap::from([("SEOHEAD_RUN_LOG".into(), "off".into())]),
        ..FakeHost::default()
    };
    assert!(log_path(&host).is_none());
    let journal = Journal::from_host(&host, "cli");
    let _: Result<(), String> = journal.journal("parse", &args(json!({"url": "x"})), |_| Ok(()));
    assert!(journal.read_entries(100).is_empty());
}

#[test]
fn an_unwritable_journal_does_not_break_the_run() {
    let dir = scratch("blocked");
    let blocker = dir.join("blocker");
    fs::write(&blocker, "not a directory").unwrap();
    let journal = Journal::at(Some(blocker.join("runs.jsonl")), "cli");
    let out: Result<(), String> = journal.journal("parse", &args(json!({"url": "x"})), |_| Ok(()));
    assert!(out.is_ok());
    assert!(journal.read_entries(100).is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_truncated_final_line_does_not_hide_earlier_entries() {
    let dir = scratch("truncated");
    let journal = fresh(&dir);
    let _: Result<(), String> = journal.journal("first", &Args::new(), |_| Ok(()));
    let mut text = entries_text(&dir);
    text.push_str(r#"{"broken": "#);
    fs::write(dir.join("runs.jsonl"), text).unwrap();
    let tools: Vec<Value> = journal
        .read_entries(100)
        .iter()
        .map(|entry| entry["tool"].clone())
        .collect();
    assert_eq!(tools, vec![json!("first")]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_tools_own_ok_is_kept_apart_from_the_calls_ok() {
    let dir = scratch("own-ok");
    let journal = fresh(&dir);
    let _: Result<(), String> = journal.journal("t", &Args::new(), |facts| {
        facts.insert("result_ok".into(), json!(false));
        Ok(())
    });
    let entry = &journal.read_entries(100)[0];
    assert_eq!(entry["ok"], true);
    assert_eq!(entry["result_ok"], false);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn entries_are_newest_first_and_limited() {
    let dir = scratch("order");
    let journal = fresh(&dir);
    for n in 0..5 {
        let _: Result<(), String> = journal.journal(&format!("t{n}"), &Args::new(), |_| Ok(()));
    }
    let tools: Vec<Value> = journal
        .read_entries(2)
        .iter()
        .map(|entry| entry["tool"].clone())
        .collect();
    assert_eq!(tools, vec![json!("t4"), json!("t3")]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_journal_is_valid_jsonl() {
    let dir = scratch("jsonl");
    let journal = fresh(&dir);
    let _: Result<(), String> =
        journal.journal("parse", &args(json!({"url": "https://e.com/"})), |_| Ok(()));
    for line in entries_text(&dir).lines() {
        serde_json::from_str::<Value>(line).expect("each line is JSON");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn with_no_policy_configured_nothing_is_ever_reused() {
    let dir = scratch("no-policy");
    let journal = fresh(&dir);
    let calls = std::cell::Cell::new(0);
    let run = || {
        journal.journaled::<String>(
            "domain_profile",
            &args(json!({"domain": "example.com"})),
            &[],
            || {
                calls.set(calls.get() + 1);
                Ok(json!({"value": calls.get()}))
            },
        )
    };
    let first = run().unwrap();
    let second = run().unwrap();
    assert_eq!(
        calls.get(),
        2,
        "an unconfigured tool must be called every time"
    );
    assert!(first.get("reused").is_none() && second.get("reused").is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_configured_tool_reuses_a_fresh_answer_without_calling_again() {
    let dir = scratch("reuse");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 3600.0)]));
    let calls = std::cell::Cell::new(0);
    let call = args(json!({"domain": "example.com"}));
    let run = || {
        journal.journaled::<String>("domain_profile", &call, &[], || {
            calls.set(calls.get() + 1);
            Ok(json!({"registrar": "Example Registrar Inc."}))
        })
    };
    let first = run().unwrap();
    let second = run().unwrap();
    assert_eq!(
        calls.get(),
        1,
        "a fresh, matching answer must not be re-fetched"
    );
    assert!(first.get("reused").is_none());
    assert_eq!(second["reused"], true);
    assert_eq!(second["registrar"], "Example Registrar Inc.");
    assert!(second.get("reused_from_ts").is_some());
    let entries = journal.read_entries(100);
    assert_eq!(entries[0]["reused"], true);
    assert_eq!(entries[0]["tool"], "domain_profile");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_reuse_policy_only_applies_to_the_tool_it_names() {
    let dir = scratch("per-tool");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 3600.0)]));
    let mut calls = 0;
    for _ in 0..2 {
        let _ = journal.journaled::<String>(
            "parse",
            &args(json!({"url": "https://example.com/"})),
            &[],
            || {
                calls += 1;
                Ok(json!({"count": calls}))
            },
        );
    }
    assert_eq!(
        calls, 2,
        "a page a client just fixed must not be answered from memory"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn different_arguments_are_not_reused() {
    let dir = scratch("different");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 3600.0)]));
    let mut calls = 0;
    let mut run = |domain: &str| {
        journal.journaled::<String>(
            "domain_profile",
            &args(json!({"domain": domain})),
            &[],
            || {
                calls += 1;
                Ok(json!({"domain": domain}))
            },
        )
    };
    run("example.com").unwrap();
    let result = run("example.org").unwrap();
    assert_eq!(calls, 2);
    assert!(result.get("reused").is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_expired_answer_is_measured_again_not_reused() {
    let dir = scratch("expired");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 60.0)]));
    let old_ts = stamp_epoch(now_epoch() - 3600.0, Format::ZuluSeconds);
    let call = args(json!({"domain": "example.com"}));
    journal.record(&json!({
        "ts": old_ts,
        "interface": "cli",
        "tool": "domain_profile",
        "arguments": safe_arguments(&call),
        "fingerprint": fingerprint("domain_profile", &call),
        "duration_s": 0.1,
        "ok": true,
        "error": null,
        "result": {"registrar": "Stale Registrar"},
    }));
    let mut calls = 0;
    let result = journal
        .journaled::<String>("domain_profile", &call, &[], || {
            calls += 1;
            Ok(json!({"registrar": "Fresh Registrar"}))
        })
        .unwrap();
    assert_eq!(
        calls, 1,
        "an answer older than the configured maximum age must be re-measured"
    );
    assert_eq!(result["registrar"], "Fresh Registrar");
    assert!(result.get("reused").is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_reused_entry_carries_no_result_of_its_own() {
    let dir = scratch("chain");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 3600.0)]));
    let call = args(json!({"domain": "example.com"}));
    let mut calls = 0;
    for _ in 0..3 {
        let _ = journal.journaled::<String>("domain_profile", &call, &[], || {
            calls += 1;
            Ok(json!({"registrar": "Example Registrar Inc."}))
        });
    }
    assert_eq!(calls, 1);
    let entries = journal.read_entries(100);
    assert!(
        entries[0].get("result").is_none(),
        "the newest entry is the reused one"
    );
    assert_eq!(
        entries[0]["reused_from_ts"],
        entries[entries.len() - 1]["ts"]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_malformed_reuse_policy_disables_reuse_rather_than_raising() {
    assert!(parse_reuse_policy("{not json").is_empty());
    assert!(parse_reuse_policy("[1, 2]").is_empty());
    assert_eq!(
        parse_reuse_policy(r#"{"a": 60, "b": "30", "c": -1, "d": null}"#),
        BTreeMap::from([("a".into(), 60.0), ("b".into(), 30.0)])
    );
}

#[test]
fn reuse_never_stores_a_secret_looking_field() {
    let dir = scratch("secret-field");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 3600.0)]));
    let _ = journal.journaled::<String>(
        "domain_profile",
        &args(json!({"domain": "example.com"})),
        &[],
        || Ok(json!({"registrar": "Example Registrar Inc.", "api_key": "super-secret-value"})),
    );
    assert!(!entries_text(&dir).contains("super-secret-value"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_journal_rotates_once_it_crosses_the_ceiling() {
    let dir = scratch("rotate");
    let journal = fresh(&dir).with_max_bytes(600);
    for n in 0..20 {
        let _: Result<(), String> = journal.journal(
            &format!("t{n}"),
            &args(json!({"pad": "x".repeat(100)})),
            |_| Ok(()),
        );
    }
    let live = dir.join("runs.jsonl");
    let backup = backup_path(&live);
    assert!(backup.exists());
    assert!(fs::metadata(&live).unwrap().len() <= 600);
    assert!(fs::metadata(&backup).unwrap().len() <= 600 + 400);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reads_still_see_entries_from_the_rotated_generation() {
    let dir = scratch("rotated-reads");
    let journal = fresh(&dir).with_max_bytes(600);
    for n in 0..20 {
        let _: Result<(), String> = journal.journal(
            &format!("t{n}"),
            &args(json!({"pad": "x".repeat(100)})),
            |_| Ok(()),
        );
    }
    let tools: Vec<String> = journal
        .read_entries(1000)
        .iter()
        .map(|entry| entry["tool"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(tools[0], "t19");
    assert!(tools.contains(&"t18".to_owned()));
    let mut unique = tools.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), tools.len());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_malformed_ceiling_falls_back_to_the_default() {
    for bad in ["abc", "0", "-5"] {
        let host = FakeHost {
            env: BTreeMap::from([(MAX_BYTES_ENV.into(), bad.into())]),
            ..FakeHost::default()
        };
        assert_eq!(max_bytes(&host), DEFAULT_MAX_BYTES, "{bad}");
    }
}

#[test]
fn an_under_ceiling_journal_is_never_rotated() {
    let dir = scratch("no-rotate");
    let journal = fresh(&dir);
    for n in 0..5 {
        let _: Result<(), String> = journal.journal(&format!("t{n}"), &Args::new(), |_| Ok(()));
    }
    assert!(!backup_path(&dir.join("runs.jsonl")).exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_answer_over_the_size_cap_is_measured_again_not_reused() {
    let dir = scratch("cap");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 3600.0)]));
    let overhead = python_json(&json!({"blob": ""})).chars().count();
    let fits = MAX_REUSABLE_RESULT_BYTES - overhead;
    let calls = std::cell::Cell::new(0);
    let run = |size: usize, domain: &str| {
        journal.journaled::<String>(
            "domain_profile",
            &args(json!({"size": size, "domain": domain})),
            &[],
            || {
                calls.set(calls.get() + 1);
                Ok(json!({"blob": "x".repeat(size)}))
            },
        )
    };
    run(fits, "at-cap.example").unwrap();
    assert!(run(fits, "at-cap.example").unwrap().get("reused").is_some());
    assert_eq!(calls.get(), 1);
    run(fits + 1, "over-cap.example").unwrap();
    let again = run(fits + 1, "over-cap.example").unwrap();
    assert!(again.get("reused").is_none());
    assert_eq!(calls.get(), 3);
    let _ = fs::remove_dir_all(&dir);
}

// log_path and reuse_policy are read through the Host seam, so tests need no environment setup.
#[test]
fn default_path_expands_home_and_missing_home_disables_it() {
    let host = FakeHost {
        env: BTreeMap::from([("HOME".into(), "/home/tester".into())]),
        ..FakeHost::default()
    };
    assert_eq!(
        log_path(&host),
        Some(PathBuf::from("/home/tester/.config/seohead/runs.jsonl"))
    );
    assert!(log_path(&FakeHost::default()).is_none());
}

// The calendar stamp round-trips.
#[test]
fn zulu_stamps_round_trip_through_the_parser() {
    let now = 1_782_000_000.0;
    let text = stamp_epoch(now, Format::ZuluSeconds);
    assert_eq!(parse_zulu_seconds(&text), Some(now as i64));
    assert_eq!(parse_zulu_seconds("2026-02-30T00:00:00Z"), None);
    assert_eq!(parse_zulu_seconds("2026-10-27 00:00:00Z"), None);
}

// A credential nested inside a non-secret argument must not reach the journal verbatim.
#[test]
fn nested_credentials_never_reach_the_journal() {
    let dir = scratch("nested");
    let journal = journal_in(&dir);
    let call = args(json!({
        "http.headers": {"Authorization": "Bearer super-secret-value", "X-Keep": "ok"},
        "url": "https://e.com/",
    }));
    let _: Result<(), String> = journal.journal("fetch", &call, |_| Ok(()));
    let text = entries_text(&dir);
    assert!(!text.contains("super-secret-value"), "nested secret leaked");
    let entry = &journal.read_entries(10)[0];
    let headers = &entry["arguments"]["http.headers"];
    assert_eq!(headers["Authorization"], json!("[redacted]"));
    assert_eq!(headers["X-Keep"], json!("ok"));
    let _ = fs::remove_dir_all(&dir);
}

// Hints match whole name tokens: `keyword` and `author` keep their values and still
// distinguish calls for reuse; `credentials` and `Authorization` stay redacted.
#[test]
fn secret_hints_match_tokens_not_substrings() {
    let call = args(json!({
        "keyword": "apple",
        "author": "me",
        "api_key": "s",
        "credentials": "c",
        "Authorization": "b",
        "oauth": "o",
        "xToken2": "t",
    }));
    let shown = safe_arguments(&call);
    assert_eq!(shown["keyword"], json!("apple"));
    assert_eq!(shown["author"], json!("me"));
    assert_eq!(shown["api_key"], json!("[redacted]"));
    assert_eq!(shown["credentials"], json!("[redacted]"));
    assert_eq!(shown["Authorization"], json!("[redacted]"));
    assert_eq!(shown["oauth"], json!("[redacted]"));
    assert_eq!(shown["xToken2"], json!("[redacted]"));
}

// Substring matching would let `keyword=a` and `keyword=b` share a fingerprint; a reused
// answer could then come back for a different query.
#[test]
fn fingerprint_distinguishes_non_secret_values() {
    let first = fingerprint("tool", &args(json!({"keyword": "a"})));
    let second = fingerprint("tool", &args(json!({"keyword": "b"})));
    assert_ne!(
        first, second,
        "non-secret argument values distinguish calls"
    );
}

// A redaction-altered answer is a corrupted copy; replaying it would return `[redacted]`.
#[test]
fn a_result_altered_by_redaction_is_never_reused() {
    let dir = scratch("redacted-result");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 3600.0)]));
    let calls = std::cell::Cell::new(0);
    let call = args(json!({"domain": "example.com"}));
    let run = || {
        journal.journaled::<String>("domain_profile", &call, &[], || {
            calls.set(calls.get() + 1);
            Ok(json!({"registrar": "Example Registrar Inc.", "api_key": "s3cr3t"}))
        })
    };
    let _ = run().unwrap();
    let second = run().unwrap();
    assert_eq!(
        calls.get(),
        2,
        "a redacted stored answer must not be reused"
    );
    assert!(second.get("reused").is_none());
    assert!(!entries_text(&dir).contains("s3cr3t"));
    let _ = fs::remove_dir_all(&dir);
}

// Benign result keys such as `keywords` or `author` are not secret tokens, so a stored answer
// keeps them verbatim.
#[test]
fn benign_names_in_a_reused_result_are_kept_verbatim() {
    let dir = scratch("benign-result");
    let journal = fresh(&dir).with_reuse(BTreeMap::from([("domain_profile".into(), 3600.0)]));
    let calls = std::cell::Cell::new(0);
    let call = args(json!({"domain": "example.com"}));
    let run = || {
        journal.journaled::<String>("domain_profile", &call, &[], || {
            calls.set(calls.get() + 1);
            Ok(json!({"keywords": ["a", "b"], "author": "me"}))
        })
    };
    let _ = run().unwrap();
    let second = run().unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(second["reused"], true);
    assert_eq!(second["keywords"], json!(["a", "b"]));
    assert_eq!(second["author"], json!("me"));
    let _ = fs::remove_dir_all(&dir);
}

// Reads stay bounded: newest entries come from the file tail, not the whole journal.
#[test]
fn read_entries_scans_the_tail_backwards() {
    let dir = scratch("tail");
    let journal = journal_in(&dir);
    let pad = "x".repeat(400);
    let mut text = String::new();
    for n in 0..1500 {
        text.push_str(&format!("{{\"tool\":\"t{n}\",\"pad\":\"{pad}\"}}\n"));
    }
    fs::write(dir.join("runs.jsonl"), &text).unwrap();
    let entries = journal.read_entries(3);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["tool"], "t1499");
    assert_eq!(entries[2]["tool"], "t1497");
    let _ = fs::remove_dir_all(&dir);
}

// Fixed digests. The journal is shared across toolkit versions, so these must never change.
#[test]
fn fingerprint_matches_the_reference_oracle() {
    assert_eq!(
        fingerprint("parse", &args(json!({"url": "https://e.com/", "depth": 2}))),
        "ae585ba29cd147ef"
    );
    assert_eq!(
        fingerprint(
            "crawl_site",
            &args(json!({"url": "https://ex.com/ü", "progress": "x", "max_urls": 5, "token": "s"}))
        ),
        "4845d25190f628b0"
    );
}
