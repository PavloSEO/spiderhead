//! Redirect chain resolution and loop detection over a stored redirect map.
//!
//! Chains report needs the full profile; without it the toolkit still has
//! every redirecting URL's own Location header, already collected in
//! `AuditContext.redirect_map`. Walking that map costs no requests, which is
//! the point: a hop's terminal status is unknown until every hop is fetched,
//! so this can only run as a second pass over the finished crawl, and it must
//! be free to re-run when a threshold changes.

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::normalize::Object;

/// Real chains rarely run more than a few hops; the cap turns a malformed or
/// cyclic map into a bounded walk instead of one proportional to site size.
pub const DEFAULT_HOP_CAP: i64 = 20;

/// `resolve_redirect_chains`: walk every redirecting URL to its terminus.
///
/// Returns one entry per URL that itself redirects (every key of the map):
/// `{"kind": "single" | "chain" | "loop" | "unresolved", "hops": int,
/// "final_url": str | null}`.
///
/// - `single`: an ordinary one-hop redirect — not a chain.
/// - `chain`: two or more hops to a resolved, non-redirecting terminus.
/// - `loop`: the walk revisited a URL already in its own chain — the only
///   way a loop is provable, since a target is only proven cyclic once it
///   reappears.
/// - `unresolved`: the walk hit `hop_cap` without proving either outcome; the
///   caller should not report a finding it cannot support.
pub fn resolve_redirect_chains(
    redirect_map: &IndexMap<String, String>,
    hop_cap: i64,
) -> Map<String, Value> {
    let mut results = Object::new();
    for start in redirect_map.keys() {
        let mut chain = vec![start.as_str()];
        let mut current = start.as_str();
        let mut outcome: Option<(&str, i64, Option<String>)> = None;
        loop {
            let target = redirect_map.get(current);
            match target {
                None => {
                    let hops = chain.len() as i64 - 1;
                    outcome = Some((
                        if hops >= 2 { "chain" } else { "single" },
                        hops,
                        Some(current.to_owned()),
                    ));
                    break;
                }
                Some(target) => {
                    if chain.contains(&target.as_str()) {
                        outcome = Some(("loop", chain.len() as i64, None));
                        break;
                    }
                    if chain.len() as i64 > hop_cap {
                        // hop_cap hops taken and another one is needed
                        break;
                    }
                    chain.push(target.as_str());
                    current = target.as_str();
                }
            }
        }
        let (kind, hops, final_url) =
            outcome.unwrap_or(("unresolved", chain.len() as i64 - 1, None));
        let mut entry = Object::new();
        entry.insert("kind".into(), Value::from(kind));
        entry.insert("hops".into(), Value::from(hops));
        entry.insert(
            "final_url".into(),
            final_url.map_or(Value::Null, Value::String),
        );
        results.insert(start.clone(), Value::Object(entry));
    }
    results
}

/// `redirect_hop_path`: every URL the redirect walk from `start` visits, in
/// order — `[start, hop1, hop2, ...]` for a resolved chain, `[..., X, ..., X]`
/// when the walk closes a loop back on `X`, and a prefix of at most
/// `hop_cap + 1` URLs when the cap is hit. This is the evidence a fixer needs
/// (which hop to replace), and it is the same bounded walk
/// `resolve_redirect_chains` performs, so the two never disagree about where a
/// chain ends.
pub fn redirect_hop_path(
    redirect_map: &IndexMap<String, String>,
    start: &str,
    hop_cap: i64,
) -> Vec<String> {
    let mut path = vec![start.to_owned()];
    let mut seen = std::collections::HashSet::from([start.to_owned()]);
    loop {
        let last = path.last().cloned().unwrap_or_default();
        let Some(target) = redirect_map.get(&last).cloned() else {
            break;
        };
        if seen.contains(&target) {
            path.push(target);
            break;
        }
        if path.len() as i64 > hop_cap {
            break;
        }
        seen.insert(target.clone());
        path.push(target);
    }
    path
}
