//! The shape of the internal link graph as numbers: click-depth histogram and
//! edge counts by position. Pure maps in, JSON objects out.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

/// Key under which edges with no recorded position are counted.
pub const UNCLASSIFIED: &str = "unclassified";

/// Depth thresholds reported in the `within` block.
pub const DEPTH_MARKS: [i64; 3] = [3, 5, 10];

/// Edge counts by recorded position; the empty-string key means "never
/// classified". Positions with a zero count are absent rather than reported as
/// zero, since a zero would claim the site has no such region when an earlier
/// rule usually matched first.
pub fn summarize_positions(totals: &BTreeMap<String, i64>) -> Value {
    let by_position: BTreeMap<&str, i64> = totals
        .iter()
        .filter(|(position, count)| !position.is_empty() && **count != 0)
        .map(|(position, count)| (position.as_str(), *count))
        .collect();
    let unclassified = totals.get("").copied().unwrap_or(0);
    let classified: i64 = by_position.values().sum();
    let edges_total = classified + unclassified;
    let fraction = if edges_total != 0 {
        round4(classified as f64 / edges_total as f64)
    } else {
        0.0
    };
    json!({
        "edges_total": edges_total,
        "by_position": by_position,
        UNCLASSIFIED: unclassified,
        "classified_fraction": fraction,
        "positions_note": "edges with no recorded position are counted under 'unclassified', never as content; re-crawl with link_position.classify to classify them",
    })
}

/// Click-depth histogram, maximum and reach over one page population.
///
/// `depth_for` gives the hops from the seed for a normalized URL, `None` when
/// the walk never reached it. Linked-but-unfetched destinations stay out of
/// `page_keys`, and a page nothing links to counts as unreachable. `floor` is
/// recorded because every verdict drawn from the numbers depends on it.
pub fn summarize_depth<'a>(
    depth_for: &dyn Fn(&str) -> Option<i64>,
    page_keys: impl IntoIterator<Item = &'a str>,
    seed: &str,
    floor: i64,
) -> Value {
    let mut histogram: BTreeMap<i64, i64> = BTreeMap::new();
    let (mut reached, mut deepest, mut pages) = (0i64, 0i64, 0i64);
    for key in page_keys {
        pages += 1;
        let Some(depth) = depth_for(key) else {
            continue;
        };
        reached += 1;
        deepest = deepest.max(depth);
        *histogram.entry(depth).or_insert(0) += 1;
    }
    let histogram_json: Map<String, Value> = histogram
        .iter()
        .map(|(d, c)| (d.to_string(), json!(c)))
        .collect();
    let within: Map<String, Value> = DEPTH_MARKS
        .iter()
        .map(|mark| {
            let count: i64 = histogram
                .iter()
                .filter(|(d, _)| **d <= *mark)
                .map(|(_, c)| c)
                .sum();
            (mark.to_string(), json!(count))
        })
        .collect();
    json!({
        "measured": true,
        "seed": seed,
        "pages": pages,
        "reachable": reached,
        "unreachable": pages - reached,
        "max": if reached != 0 { json!(deepest) } else { Value::Null },
        "histogram": histogram_json,
        "within": within,
        "floor_used": floor,
        "edges": "internal followed hyperlinks",
    })
}

/// A measurement that could not be taken, saying why; an absent block would
/// read as "this site has no depth".
pub fn unmeasured(reason: &str) -> Value {
    json!({"measured": false, "reason": reason})
}

/// Round half to even at four decimals.
fn round4(x: f64) -> f64 {
    format!("{x:.4}").parse().unwrap_or_default()
}
