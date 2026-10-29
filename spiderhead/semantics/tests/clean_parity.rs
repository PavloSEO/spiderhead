//! `clean` stage parity check against the reference baseline (inline snapshot).

use serde_json::{json, Value};
use spiderhead_semantics::{config, stages, store::Store};

fn cfg(dir: &str) -> config::Cfg {
    let mut c = config::defaults();
    c.insert("preset".into(), json!("seo"));
    c.insert("min_base".into(), json!(20));
    c.insert("_dir".into(), json!(dir));
    c
}

#[test]
fn clean_matches_reference_oracle() {
    let s = Store::open_memory().unwrap();
    let phrases: &[(&str, Option<i64>)] = &[
        ("seo аудит сайта", Some(500)),
        ("краулер сайта бесплатно", Some(30)),
        ("порно видео", Some(999999)),
        ("котлы навьен", Some(300)),
        ("seo", Some(2)),
        ("что такое seo оптимизация", Some(40)),
        ("seo продвижение сайта", Some(50)),
        ("аудит сайта онлайн", None),
        ("x", Some(100)),
    ];
    for (norm, base) in phrases {
        s.upsert(norm, *base, 0, Some("import"), "225").unwrap();
    }
    let result = stages::clean::run(&s, &cfg(".")).unwrap();
    assert_eq!(result, json!({"changed": 9, "lemma_groups": 9}));

    let rows = s
        .q(
            "SELECT norm,status,route,reason,reason_stage,lemma_group FROM phrases ORDER BY norm",
            &[],
        )
        .unwrap();
    let got: Vec<Value> = rows
        .into_iter()
        .map(|r| Value::Object(r.into_iter().collect()))
        .collect();
    let expected: Vec<Value> = serde_json::from_str(r#"[
      {"norm":"seo","status":"dropped","route":"commercial","reason":"invalid phrase length","reason_stage":"clean","lemma_group":8},
      {"norm":"seo аудит сайта","status":"kept","route":"commercial","reason":"project intent and demand","reason_stage":"clean","lemma_group":2},
      {"norm":"seo продвижение сайта","status":"kept","route":"commercial","reason":"project intent and demand","reason_stage":"clean","lemma_group":5},
      {"norm":"x","status":"dropped","route":"commercial","reason":"invalid phrase length","reason_stage":"clean","lemma_group":4},
      {"norm":"аудит сайта онлайн","status":"kept","route":"commercial","reason":"project intent and demand","reason_stage":"clean","lemma_group":9},
      {"norm":"котлы навьен","status":"review","route":"commercial","reason":"no project intent","reason_stage":"clean","lemma_group":3},
      {"norm":"краулер сайта бесплатно","status":"kept","route":"commercial","reason":"project intent and demand","reason_stage":"clean","lemma_group":7},
      {"norm":"порно видео","status":"dropped","route":"commercial","reason":"stop phrase","reason_stage":"clean","lemma_group":1},
      {"norm":"что такое seo оптимизация","status":"kept","route":"info","reason":"project intent and demand","reason_stage":"clean","lemma_group":6}
    ]"#)
    .unwrap();
    assert_eq!(got, expected);
}
