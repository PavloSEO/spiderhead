//! Deprecated saved-scan flag spellings, rewritten before parsing (`_rewrite_deprecated_scan_flags`).

use serde_json::Value;

const SCAN_PATH_COMMANDS: [&str; 13] = [
    "scan-inspect",
    "scan-url-detail",
    "scan-body-read",
    "scan-url-query",
    "scan-url-counts",
    "scan-directory-tree",
    "scan-link-inspect",
    "headers-analyze",
    "scan-status",
    "scan-rendered-routes",
    "scan-snapshot",
    "scan-pin",
    "scan-reanalyze",
];

fn is_json_object(value: &str) -> bool {
    serde_json::from_str::<Value>(value).is_ok_and(|v| v.is_object())
}

/// Rewrite `--json-input` to `--input` and path-shaped `--input` to `--scan` for the saved-scan
/// readers; returns the new argv and the deprecation warnings (deduplicated, in order).
pub fn rewrite_deprecated_scan_flags(argv: &[String]) -> (Vec<String>, Vec<String>) {
    const JSON_INPUT: &str = "--json-input is deprecated; use --input for inline JSON";
    const INPUT_FILE: &str = "--input FILE is deprecated for scan artifacts; use --scan FILE";
    let mut out = argv.to_vec();
    let mut warnings: Vec<&str> = Vec::new();
    let mut command: Option<String> = None;
    for index in 0..out.len() {
        let token = out[index].clone();
        if token == "scan" && index + 1 < out.len() {
            command = Some(format!("scan-{}", out[index + 1]));
            continue;
        }
        if SCAN_PATH_COMMANDS.contains(&token.as_str()) {
            command = Some(token);
            continue;
        }
        let Some(cmd) = command.as_deref().filter(|c| c.starts_with("scan-")) else {
            continue;
        };
        let path_command = SCAN_PATH_COMMANDS.contains(&cmd);
        if token == "--json-input" {
            out[index] = "--input".into();
            warnings.push(JSON_INPUT);
        } else if let Some(v) = token.strip_prefix("--json-input=") {
            out[index] = format!("--input={v}");
            warnings.push(JSON_INPUT);
        } else if path_command && token == "--input" && index + 1 < out.len() {
            if !is_json_object(&out[index + 1]) {
                out[index] = "--scan".into();
                warnings.push(INPUT_FILE);
            }
        } else if let Some(v) = token.strip_prefix("--input=").filter(|_| path_command) {
            if !is_json_object(v) {
                out[index] = format!("--scan={v}");
                warnings.push(INPUT_FILE);
            }
        }
    }
    let mut seen = Vec::new();
    for w in warnings {
        if !seen.contains(&w) {
            seen.push(w);
        }
    }
    (out, seen.into_iter().map(str::to_owned).collect())
}
