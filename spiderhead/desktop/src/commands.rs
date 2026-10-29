//! The command registry: every action the window can take, visible to the UI
//! and to `spiderhead-desktop-ctl` alike. `enabled`/`disabled_reason` reflect the
//! live session (no scan → table commands stay off).

use serde_json::Value;
use spiderhead_control::registry::CommandInfo;

fn cmd(id: &str, label: &str, params: &str) -> CommandInfo {
    CommandInfo {
        id: id.into(),
        label: label.into(),
        params: params.into(),
        enabled: true,
        disabled_reason: None,
    }
}

/// Commands that need an opened scan.
static SCAN_COMMANDS: &[&str] = &[
    "scan.close",
    "scan.info",
    "table.page",
    "table.sort",
    "table.filter",
    "table.select",
    "table.copy",
    "ui.scroll_to",
    "details.load",
    "details.copy",
    "details.toggle",
    "dashboard.inspect",
    "issues.urls",
    "search.run",
    "search.copy",
    "compare.scans",
    "compare.run",
    "compare.url",
];

/// All commands of the running app. `scan_open` gates the scan-bound ones.
pub fn registry(scan_open: bool) -> Vec<CommandInfo> {
    let mut commands = vec![
        cmd("app.quit", "Quit SpiderHead", "{}"),
        cmd(
            "app.install_cli",
            "Link the bundled spiderhead command into the terminal PATH",
            "{}",
        ),
        cmd(
            "app.connect_claude",
            "MCP client configuration for `spiderhead mcp` (copied, no files written)",
            "{}",
        ),
        cmd(
            "commands.list",
            "Every command with its parameters",
            r#"{"search": "text"}"#,
        ),
        cmd(
            "ui.theme",
            "Switch the theme",
            r#"{"mode": "light"|"dark"|"hc"}"#,
        ),
        cmd(
            "ui.mode",
            "Switch the product mode or return to the chooser",
            r#"{"mode": "crawler"|"logs"|"agent"|"start", "nav": "urls"}"#,
        ),
        cmd("ui.inspect", "The state of the window", "{}"),
        cmd(
            "ui.navigate",
            "Show a navigation section",
            r#"{"id": "urls"|"dashboard"|"crawler"|"search"|"compare"|"compare_url"|
                "check"|"logs"|"logs_bots"|"logs_compare"|"work"|"inbox"|"projects"}"#,
        ),
        cmd(
            "ui.find",
            "Open the URL table and focus its filter field",
            "{}",
        ),
        cmd(
            "ui.window",
            "Open or close a dialog window",
            r#"{"id": "settings"|"mcp"|"cli"|"help"|"profile"|"palette"|"about"|
                "usage"|"shortcuts", "open": true}"#,
        ),
        cmd(
            "ui.screenshot",
            "Capture the window to a PNG file",
            r#"{"path": "file.png"}"#,
        ),
        cmd("window.list", "Open windows", "{}"),
        cmd("window.minimize", "Minimize the window to the Dock", "{}"),
        cmd("window.fullscreen", "Toggle fullscreen", "{}"),
        cmd(
            "window.resize",
            "Set the window size in points",
            r#"{"width": 1440, "height": 900}"#,
        ),
        cmd(
            "edit.clipboard",
            "Editing operation on the focused field",
            r#"{"action": "undo"|"cut"|"copy"|"paste"|"select_all"}"#,
        ),
        cmd(
            "scan.open",
            "Open a saved scan (scan.v1); dialog picks a file",
            r#"{"path": "scan.sqlite"} | {"dialog": true}"#,
        ),
        cmd("scan.close", "Close the open scan", "{}"),
        cmd("scan.info", "The open scan's header", "{}"),
        cmd(
            "table.page",
            "Rows of the URL table",
            r#"{"offset": 0, "limit": 50, "columns": ["url","status_code"]}"#,
        ),
        cmd(
            "table.sort",
            "Sort the URL table",
            r#"{"column": "title", "direction": "desc"} | {"column": null}"#,
        ),
        cmd(
            "table.filter",
            "Set table filters, preset or issue selection",
            r#"{"filters": [{"column":"status_class","op":"eq","value":"4xx"}],
                "preset": "title_missing", "issue_check": "BROKEN_INTERNAL_LINK",
                "issue_severity": "critical"}"#,
        ),
        cmd(
            "table.select",
            "Select a row; details panel follows",
            r#"{"url_id": 3} | {"url_id": null}"#,
        ),
        cmd(
            "table.copy",
            "Rows as TSV: the filtered set, or only the selected ids",
            r#"{"selected": [3, 9], "header": false} | {}"#,
        ),
        cmd(
            "table.column.hide",
            "Hide a URL-table column",
            r#"{"column": "h1"}"#,
        ),
        cmd(
            "table.column.show",
            "Bring a hidden column back",
            r#"{"column": "h1"}"#,
        ),
        cmd(
            "table.column.move",
            "Move a column before another one (order persists)",
            r#"{"column": "h1", "before": "title"}"#,
        ),
        cmd(
            "table.column.pin",
            "Freeze the leading columns up to this one (null unfreezes)",
            r#"{"upto": "url"} | {"upto": null}"#,
        ),
        cmd(
            "table.columns.reset",
            "Restore the default column set, order, widths and zoom",
            "{}",
        ),
        cmd(
            "table.columns.configure",
            "Open or close the column-chooser dialog",
            r#"{"open": true}"#,
        ),
        cmd(
            "table.zoom",
            "Zoom the URL table (a ladder percent, or in/out)",
            r#"{"percent": 120} | {"step": "in"|"out"}"#,
        ),
        cmd(
            "ui.scroll_to",
            "Scroll the URL table so this row tops the view",
            r#"{"row": 42}"#,
        ),
        cmd(
            "details.load",
            "A details-tab payload for a URL",
            r#"{"url_id": 3, "tab": "summary"|"headers"|"links_in"|"links_out"|"html"|
                "resources"|"redirects"|"history"|"structured"|"counts"}"#,
        ),
        cmd(
            "details.copy",
            "The retained URL text of a row, for the clipboard",
            r#"{"url_id": 3}"#,
        ),
        cmd(
            "details.toggle",
            "Show or hide the bottom details panel",
            r#"{"open": true}"#,
        ),
        cmd(
            "dashboard.inspect",
            "Saved-audit counters and per-check states",
            "{}",
        ),
        cmd(
            "dashboard.filter",
            "Open the URL table filtered to a check/severity",
            r#"{"issue_check": "BROKEN_INTERNAL_LINK", "issue_severity": "critical"}"#,
        ),
        cmd(
            "ui.sidebar",
            "Fold or unfold the navigation (icon rail)",
            r#"{"collapsed": true}"#,
        ),
        cmd(
            "dashboard.select",
            "Select a check on the dashboard (null clears)",
            r#"{"check": "BROKEN_INTERNAL_LINK"}"#,
        ),
        cmd(
            "issues.severity",
            "Filter the issues list to a severity (empty = all)",
            r#"{"id": "critical"|"warning"|"notice"|"opportunity"|""}"#,
        ),
        cmd(
            "issues.urls",
            "URLs affected by a check, paged like the table",
            r#"{"check": "BROKEN_INTERNAL_LINK", "offset": 0, "limit": 100}"#,
        ),
        cmd(
            "search.options",
            "Set HTML-search options (scope, case, 200-only, selector)",
            r#"{"scope": "text"|"html"|"head"|"css"|"dom", "case_sensitive": false,
                "only_200": false, "selector": ".css"}"#,
        ),
        cmd(
            "search.run",
            "Search the scan's retained HTML for a literal marker",
            r#"{"query": "text to find"}"#,
        ),
        cmd(
            "search.select",
            "Pick a hit and occurrence for the preview pane",
            r#"{"hit": 0, "match": 0}"#,
        ),
        cmd(
            "search.copy",
            "The search hits as TSV, for the clipboard",
            "{}",
        ),
        cmd(
            "compare.scans",
            "Scan files beside the open one (compare pair choices)",
            "{}",
        ),
        cmd(
            "compare.pick",
            "Choose the before/after scans of the comparison",
            r#"{"side": "before"|"after", "path": "scan.sqlite"}"#,
        ),
        cmd("compare.swap", "Swap the before/after scans", "{}"),
        cmd(
            "compare.tab",
            "Comparison tab",
            r#"{"id": "fixed"|"better"|"worse"|"gone"|"new"}"#,
        ),
        cmd(
            "compare.metric",
            "Metric of the better/worse tab",
            r#"{"id": "words"|"inlinks"|"response"}"#,
        ),
        cmd(
            "compare.run",
            "Summary plus a page of the active comparison tab",
            r#"{"offset": 0, "limit": 50}"#,
        ),
        cmd(
            "compare.url",
            "One URL observed in both scans (opens the URL view)",
            r#"{"url": "http://example.com/", "open": true}"#,
        ),
        cmd(
            "simple.check",
            "Look one address up in the open scan",
            r#"{"url": "http://example.com/page"}"#,
        ),
        cmd(
            "settings.list",
            "Every declared setting: value, default, is_set",
            "{}",
        ),
        cmd(
            "settings.get",
            "One setting value, or all values without a key",
            r#"{"key": "view.theme"}"#,
        ),
        cmd(
            "settings.set",
            "Store a setting (validated, saved at once)",
            r#"{"key": "view.theme", "value": "dark"}"#,
        ),
        cmd(
            "settings.reset",
            "Drop stored values under a prefix",
            r#"{"prefix": "view."}"#,
        ),
        cmd(
            "settings.page",
            "Open the settings window on a page",
            r#"{"id": "general"|"view"|"scan"|"core"|"mcp"|"sources"|"agent"|"agent_config"|
                "notify"|"keys"|"data"|"updates"|"about"|"source_detail"|"source_key"|
                "sources_auth"|"spend", "provider": "gsc"}"#,
        ),
        cmd(
            "settings.crawl_defaults",
            "Scan defaults mapped onto core crawl settings",
            "{}",
        ),
        cmd(
            "settings.export",
            "Write the settings JSON (secrets are never part of it)",
            r#"{"path": "settings.json"}"#,
        ),
        cmd(
            "settings.import",
            "Read back an exported settings JSON",
            r#"{"path": "settings.json"}"#,
        ),
        cmd(
            "settings.update_check",
            "Installed versions and the update-check state",
            "{}",
        ),
        cmd(
            "sources.list",
            "Data providers and per-credential set/not-set state",
            "{}",
        ),
        cmd(
            "sources.credential",
            "Store or revoke one provider credential via the credential store",
            r#"{"provider": "arsenkin", "component": "token",
                "action": "set"|"revoke", "value": "…"}"#,
        ),
        cmd(
            "sources.spend",
            "Paid-API spend report",
            r#"{"since": "2026-10-01"}"#,
        ),
    ];
    for c in &mut commands {
        if !scan_open && SCAN_COMMANDS.contains(&c.id.as_str()) {
            c.enabled = false;
            c.disabled_reason = Some("no scan is open".into());
        }
    }
    commands
}

/// The `commands.list` reply payload.
pub fn list_value(commands: &[CommandInfo], search: &str) -> Value {
    let found = spiderhead_control::registry::search_commands(commands, search);
    spiderhead_control::registry::commands_value(&found)
}
