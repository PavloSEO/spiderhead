//! Human-readable caveats for a Cloudflare traffic summary.
use super::*;

/// Readable caveats appended to a traffic summary.
pub(super) fn findings(r: &Value) -> Value {
    let mut out = vec![r.get("note").cloned().unwrap_or(Value::Null)];
    let truncated: Vec<String> = r
        .get("truncated_days")
        .and_then(Value::as_array)
        .map(|d| {
            d.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if !truncated.is_empty() {
        out.push(json!(format!(
            "Row cap reached on {}: low-volume rows were cut off",
            truncated.join(", ")
        )));
    }
    let fams = r.get("by_family").cloned().unwrap_or_default();
    if r.get("requests").and_then(Value::as_i64).unwrap_or(0) == 0 {
        out.push(json!(
            "No requests in the period: the zone may not be proxied or has no traffic"
        ));
        return Value::Array(out);
    }
    for wanted in ["googlebot", "yandexbot", "bingbot"] {
        if fams.get(wanted).is_none() {
            out.push(json!(format!("{wanted} does not appear in the period")));
        }
    }
    if let Some(ai) = fams.get("ai") {
        let total: i64 = ai
            .as_object()
            .map(|m| m.values().filter_map(Value::as_i64).sum())
            .unwrap_or(0);
        out.push(json!(format!("AI crawlers made {total} requests")));
    }
    if let Some(status) = r.get("status_by_family").and_then(Value::as_object) {
        for (family, codes) in status {
            if family == "human" {
                continue;
            }
            let mut errors_n = 0i64;
            let mut total = 0i64;
            if let Some(map) = codes.as_object() {
                for (s, c) in map {
                    let v = c.as_i64().unwrap_or(0);
                    total += v;
                    if s.parse::<i64>().unwrap_or(0) >= 400 {
                        errors_n += v;
                    }
                }
            }
            let total = total.max(1);
            if errors_n as f64 / total as f64 > 0.1 {
                // Percentages round half to even.
                let pct = (errors_n as f64 / total as f64 * 100.0).round_ties_even();
                out.push(json!(format!(
                    "{family}: {errors_n} of {total} requests returned errors ({pct:.0}%)"
                )));
            }
        }
    }
    Value::Array(out)
}
