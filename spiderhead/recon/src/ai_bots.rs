//! Audit site access for AI crawlers from a GEO/AEO perspective.
//!
//! A network-free read of `robots.txt` content over [`crate::robots_txt::parse_robots`]
//! and [`spiderhead_checks::audit::robots::is_allowed`]. Bot roles: `training` collects for
//! model training, `retrieval` finds sources for real-time answers, `user` fetches on behalf of a user.

use std::sync::LazyLock;

use serde_json::{json, Value as Json};
use spiderhead_checks::audit::robots::is_allowed;

use crate::robots_txt::parse_robots;

#[derive(serde::Deserialize, serde::Serialize, Clone)]
struct BotEntry {
    token: String,
    vendor: String,
    #[serde(rename = "type")]
    kind: String,
    purpose: String,
}

/// Known AI crawlers.
pub static AI_BOTS: LazyLock<Vec<Json>> = LazyLock::new(|| {
    #[derive(serde::Deserialize)]
    struct Table {
        ai_bots: Vec<BotEntry>,
    }
    serde_json::from_str::<Table>(include_str!("data/ai_bots.json"))
        .unwrap_or_else(|e| panic!("embedded ai_bots table is invalid: {e}"))
        .ai_bots
        .iter()
        .map(
            |b| json!({"token": b.token, "vendor": b.vendor, "type": b.kind, "purpose": b.purpose}),
        )
        .collect()
});

/// Determine each AI crawler's status from `robots.txt` content, with a fixed set of result keys.
pub fn check_ai_access(robots_text: &str) -> Json {
    let parsed = parse_robots(robots_text);
    let declared_uas: std::collections::BTreeSet<String> = parsed["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| {
            g["user_agents"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|u| u.as_str().map(str::to_lowercase))
                .collect::<Vec<_>>()
        })
        .collect();

    let mut bots: Vec<Json> = Vec::new();
    let mut blocked = 0usize;
    let mut allowed_explicit = 0usize;
    let mut allowed_default = 0usize;
    let mut by_type = serde_json::Map::new();
    for kind in ["training", "retrieval", "user"] {
        by_type.insert(kind.to_string(), json!({"blocked": 0, "allowed": 0}));
    }

    for bot in AI_BOTS.iter() {
        let token = bot["token"].as_str().unwrap_or("");
        let kind = bot["type"].as_str().unwrap_or("");
        let declared = declared_uas.contains(&token.to_lowercase());
        let allowed_root = is_allowed(&parsed, "/", token);
        let blocked_root = !allowed_root;

        let status = if blocked_root {
            blocked += 1;
            by_type[kind]["blocked"] = json!(by_type[kind]["blocked"].as_u64().unwrap_or(0) + 1);
            "blocked"
        } else if declared {
            allowed_explicit += 1;
            by_type[kind]["allowed"] = json!(by_type[kind]["allowed"].as_u64().unwrap_or(0) + 1);
            "allowed_explicit"
        } else {
            allowed_default += 1;
            by_type[kind]["allowed"] = json!(by_type[kind]["allowed"].as_u64().unwrap_or(0) + 1);
            "allowed_default"
        };

        bots.push(json!({
            "token": token,
            "vendor": bot["vendor"],
            "type": kind,
            "purpose": bot["purpose"],
            "declared_in_robots": declared,
            "blocked_root": blocked_root,
            "status": status,
        }));
    }

    json!({
        "ok": true,
        "declared_groups": parsed["groups"].as_array().map(|g| g.len()).unwrap_or(0),
        "summary": {
            "total": AI_BOTS.len(),
            "blocked": blocked,
            "allowed_explicit": allowed_explicit,
            "allowed_default": allowed_default,
            "by_type": Json::Object(by_type),
        },
        "bots": bots,
    })
}
