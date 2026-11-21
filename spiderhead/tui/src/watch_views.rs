//! TUI watch views: section browsing over the observer snapshot.
//!
//! view keys, pagination/selection
//! helpers, rate and run summaries, the section line renderer and the detail
//! view. `snapshot`/`data` overrides keep the same seam tests monkeypatched.

use serde_json::{json, Value};

use crate::labels::{label as human_label, title as human_title, value_lines};
use crate::localization::ui;
use crate::panels::select;
use crate::state::{PageIdentity, ShellState};
use crate::text::Text;
use crate::theme::Palette;
use crate::values::{self, OrTruthy};

/// `_view_key` — only data selection belongs in the read key.
pub fn view_key(state: &ShellState) -> Value {
    let detail = state.view == "watch_detail";
    let opt = |v: &Option<String>| v.clone().map(Value::String).unwrap_or(Value::Null);
    json!([
        state.watch_section,
        detail,
        state.watch_offset,
        state.watch_query,
        opt(&state.watch_status),
        state.watch_sort,
        state.watch_descending,
        if ["findings", "views"].contains(&state.watch_section.as_str()) {
            opt(&state.watch_selected_scan_uuid)
        } else {
            Value::Null
        },
        opt(&state.watch_site_uuid),
        if detail {
            Value::String(state.watch_detail_kind.clone())
        } else {
            Value::Null
        },
        if detail {
            state
                .watch_detail_ordinal
                .map(Value::from)
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        },
        if detail {
            opt(&state.watch_item_id)
        } else {
            Value::Null
        },
        if detail {
            opt(&state.watch_inbox_entry_id)
        } else {
            Value::Null
        },
        if detail {
            opt(&state.watch_view_name)
        } else {
            Value::Null
        },
        if detail {
            Value::from(state.watch_view_offset)
        } else {
            Value::Null
        },
        if detail {
            opt(&state.watch_site_detail_uuid)
        } else {
            Value::Null
        },
    ])
}

/// `_page_selection` — keep the selected row stable across refreshes.
pub fn page_selection(state: &mut ShellState, items: &[Value], pagination: &Value) -> String {
    let identities: Vec<String> = items
        .iter()
        .map(|item| {
            if item.is_object() {
                ["fingerprint", "uuid", "project_uuid", "id", "name"]
                    .iter()
                    .filter_map(|k| values::get(item, k))
                    .find(|v| values::truthy(v))
                    .map(values::scalar_text)
                    .unwrap_or_else(|| "None".into())
            } else {
                values::scalar_text(item)
            }
        })
        .collect();
    let page_identity = PageIdentity {
        section: state.watch_section.clone(),
        offset: state.watch_offset,
        query: state.watch_query.clone(),
        identities: identities.clone(),
    };
    if let Some(prev) = &state.watch_page_identity {
        let same_prefix = prev.section == page_identity.section
            && prev.offset == page_identity.offset
            && prev.query == page_identity.query;
        if same_prefix
            && prev != &page_identity
            && state
                .watch_selected_row_id
                .as_ref()
                .is_some_and(|id| identities.contains(id))
        {
            let id = state.watch_selected_row_id.clone().unwrap_or_default();
            state.watch_index = identities.iter().position(|i| *i == id).unwrap_or(0) as i64;
        }
    }
    state.watch_count = Some(items.len() as i64);
    state.watch_index = state.watch_index.min((items.len() as i64 - 1).max(0));
    state.watch_selected_row_id = if items.is_empty() {
        None
    } else {
        identities.get(state.watch_index as usize).cloned()
    };
    state.watch_page_identity = Some(page_identity);
    state.watch_next_offset = values::int_of(pagination, "next_offset");
    state.watch_previous_offset = if values::get(pagination, "previous_offset").is_some() {
        values::int_of(pagination, "previous_offset")
    } else {
        Some((state.watch_offset - 50).max(0))
    };
    let total = values::get(pagination, "total")
        .or_else(|| values::get(pagination, "matched"))
        .map(values::scalar_text)
        .unwrap_or_else(|| items.len().to_string());
    let start = if items.is_empty() {
        0
    } else {
        state.watch_offset + 1
    };
    format!(
        "{}-{} of {total} · PgUp/PgDn pages",
        start,
        state.watch_offset + items.len() as i64
    )
}

/// `_local_entries` — sections served from the snapshot itself.
pub fn local_entries(snapshot: &Value, section: &str) -> Vec<Value> {
    if section == "sites" {
        let mut sites = values::path(snapshot, &["sites", "items"])
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let inbox_key = if values::get(snapshot, "latest_inbox").is_some() {
            "latest_inbox"
        } else {
            "inbox"
        };
        for entry in values::list_of(values::obj(snapshot, inbox_key), "entries") {
            let entry_id = values::str_of(entry, "id").to_string();
            for (outcome_index, outcome) in values::list_of(entry, "triage").iter().enumerate() {
                let reason = values::get(outcome, "reason")
                    .cloned()
                    .unwrap_or(Value::Null);
                let recorded = values::get(outcome, "recorded_at")
                    .cloned()
                    .unwrap_or(Value::Null);
                for (ordinal, target) in values::list_of(outcome, "competitors").iter().enumerate()
                {
                    let host = target
                        .as_str()
                        .and_then(|t| url::Url::parse(t).ok())
                        .and_then(|u| u.host_str().map(str::to_string))
                        .map(Value::String)
                        .unwrap_or(Value::Null);
                    sites.push(json!({
                        "project_uuid": format!("proposal:{}/{}/{}", entry_id, outcome_index, ordinal),
                        "site": {"target": target, "host": host, "label": Value::Null},
                        "role": "proposed_competitor",
                        "source_note_id": entry_id,
                        "reason": reason,
                        "recorded_at": recorded,
                        "directory": Value::Null,
                        "candidate": {"state": "Suggested; not prepared or analyzed"},
                        "scans": {"total": Value::Null, "items": []},
                        "methods": {"state": "unavailable"},
                    }));
                }
            }
        }
        return sites;
    }
    if section == "activity" {
        let mut runs: Vec<Value> = values::path(snapshot, &["sites", "items"])
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .flat_map(|site| {
                        let label = values::path(site, &["site", "label"])
                            .or_truthy(|| values::path(site, &["site", "host"]))
                            .filter(|v| values::truthy(v))
                            .cloned()
                            .unwrap_or(Value::Null);
                        values::path(site, &["runs", "items"])
                            .and_then(Value::as_array)
                            .map(|arr| arr.as_slice())
                            .unwrap_or(&[])
                            .iter()
                            .map(|run| {
                                let mut run = run.clone();
                                run["site_label"] = label.clone();
                                run
                            })
                            .collect::<Vec<Value>>()
                    })
                    .collect::<Vec<Value>>()
            })
            .unwrap_or_default();
        runs.extend(
            values::path(snapshot, &["execution", "runs"])
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .rev(),
        );
        return runs;
    }
    if section == "sf" {
        return values::path(snapshot, &["runs", "items"])
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|item| values::str_of(item, "kind") == "screaming_frog")
            .collect();
    }
    if section == "views" {
        return values::path(snapshot, &["saved_views", "views"])
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
    }
    values::path(snapshot, &["log", "text"])
        .and_then(Value::as_str)
        .map(|text| text.lines().map(|l| json!(l)).collect())
        .unwrap_or_default()
}

/// `_local_page` — paginate the snapshot-side sections.
pub fn local_page(snapshot: &Value, state: &mut ShellState) -> (Vec<Value>, String) {
    let entries = local_entries(snapshot, &state.watch_section);
    let offset = state.watch_offset.max(0) as usize;
    let items: Vec<Value> = entries.iter().skip(offset).take(50).cloned().collect();
    let hint = page_selection(
        state,
        &items,
        &json!({
            "total": entries.len(),
            "next_offset": if offset + 50 < entries.len() { Value::from(offset + 50) } else { Value::Null },
        }),
    );
    (items, hint)
}

/// `_rate_text` — a measured resource rate, never a machine code.
pub fn rate_text(telemetry: &Value) -> String {
    let unit = if values::str_of(telemetry, "unit") == "urls_including_resources" {
        "URLs incl. resources"
    } else {
        "pages"
    };
    match values::float_of(telemetry, "current_rate_per_second") {
        Some(rate) => format!("{rate:.2} {unit}/s"),
        None => ui("rate unavailable"),
    }
}

/// `_run_summary` — one line per run in activity views.
pub fn run_summary(item: &Value) -> String {
    let telemetry = values::obj(item, "telemetry");
    let title = values::path(item, &["scenario", "id"])
        .filter(|v| values::truthy(v))
        .map(values::scalar_text)
        .unwrap_or_else(|| human_label(values::get(item, "kind").unwrap_or(&Value::Null)));
    let mode = values::path(item, &["collector", "mode"]).filter(|v| values::truthy(v));
    let state_label = values::get(telemetry, "state")
        .filter(|v| values::truthy(v))
        .map(|s| {
            if s.as_str() == Some("stale") {
                ui("Stale sample")
            } else {
                human_label(s)
            }
        });
    [
        values::get(item, "site_label")
            .filter(|v| values::truthy(v))
            .map(values::scalar_text),
        Some(title),
        Some(human_label(
            values::get(item, "state").unwrap_or(&Value::Null),
        )),
        mode.map(human_label),
        state_label,
        Some(rate_text(telemetry)),
        values::get(item, "next_action")
            .filter(|v| values::truthy(v))
            .map(values::scalar_text),
    ]
    .into_iter()
    .flatten()
    .filter(|v| !v.is_empty())
    .collect::<Vec<String>>()
    .join(" · ")
}

/// `_watch_lines` — browsable retained project evidence; it never starts,
/// cancels or resumes work.
pub fn watch_lines(
    project: &str,
    state: &mut ShellState,
    palette: &Palette,
    _message: Option<&str>,
    snapshot: Option<&Value>,
    data: Option<&Value>,
) -> Vec<Text> {
    let owned;
    let snapshot = match snapshot {
        Some(s) => s,
        None => {
            let (s, failure) = crate::app::watch_snapshot(project);
            match s {
                Some(snap) => {
                    owned = snap;
                    &owned
                }
                None => return failure,
            }
        }
    };
    let owned_data;
    let data = match data {
        Some(d) => d.clone(),
        None => {
            crate::app::read_view(project, state, snapshot).unwrap_or_else(|e| json!({"error": e}))
        }
    };
    owned_data = data;
    let data = &owned_data;
    if values::truthy(values::get(data, "loading").unwrap_or(&Value::Null))
        || values::get(data, "error").is_some()
    {
        return vec![Text::new(
            &values::get(data, "error")
                .map(values::scalar_text)
                .unwrap_or_else(|| ui("Loading retained entries…")),
            "",
        )];
    }
    let mut site = values::path(snapshot, &["project", "site"])
        .cloned()
        .unwrap_or(json!({}));
    if state.watch_site_uuid.is_some()
        && ["scans", "findings", "views"].contains(&state.watch_section.as_str())
    {
        if let Some(found) = values::path(snapshot, &["sites", "items"])
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    values::str_of(item, "project_uuid")
                        == state.watch_site_uuid.as_deref().unwrap_or("")
                })
            })
        {
            site = values::get(found, "site").cloned().unwrap_or(json!({}));
        }
    }
    let progress = values::obj(snapshot, "progress");
    let preparation = values::obj(snapshot, "preparation");
    let section = state.watch_section.clone();
    let site_name = values::get(&site, "label")
        .or_truthy(|| values::get(&site, "host"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let site_target = values::str_of(&site, "target").to_string();
    let mut lines = vec![
        Text::new(
            &format!("{section} · {site_name}"),
            if palette.color { &palette.title } else { "" },
        ),
        Text::new(&format!("site     {site_target}"), ""),
        Text::blank(),
    ];
    match section.as_str() {
        "overview" => overview_lines(snapshot, progress, preparation, palette, &mut lines),
        "sites" => {
            let (sites, hint) = local_page(snapshot, state);
            let selected = sites
                .get((state.watch_index.max(0)) as usize)
                .cloned()
                .unwrap_or(json!({}));
            state.watch_site_detail_uuid =
                values::get(&selected, "project_uuid").map(values::scalar_text);
            state.watch_site_openable = !values::get(&selected, "directory")
                .unwrap_or(&Value::Null)
                .is_null();
            lines.push(Text::new(
                &format!("{hint} · configured sites + recent inbox proposals"),
                "",
            ));
            let entries: Vec<String> = sites
                .iter()
                .map(|item| {
                    let total = values::path(item, &["scans", "total"]);
                    let scans = match total.and_then(Value::as_i64) {
                        Some(t) => format!("{t} saved scans"),
                        None => "No scan measurement".into(),
                    };
                    let site_label = values::path(item, &["site", "label"])
                        .or_truthy(|| values::path(item, &["site", "target"]))
                        .map(values::scalar_text)
                        .unwrap_or_default();
                    format!(
                        "{} · {} · {}",
                        human_label(values::get(item, "role").unwrap_or(&Value::Null)),
                        site_label,
                        scans
                    )
                })
                .collect();
            lines.extend(select(&entries, state.watch_index, palette));
        }
        "tasks" | "methods" | "schema" => {
            let items = values::list_of(data, "items");
            let page_hint = page_selection(state, items, values::obj(data, "pagination"));
            state.watch_item_id = items
                .get(state.watch_index.max(0) as usize)
                .and_then(|i| values::get(i, "id"))
                .map(values::scalar_text);
            let entries: Vec<String> = items
                .iter()
                .map(|item| {
                    format!(
                        "{} · {}",
                        human_title(values::str_of(item, "title")),
                        human_label(
                            values::get(item, "display_state")
                                .or_else(|| values::get(item, "state"))
                                .unwrap_or(&Value::Null)
                        )
                    )
                })
                .collect();
            lines.push(Text::new(
                &format!(
                    "{} · {} · filter {}",
                    page_hint,
                    state
                        .watch_status
                        .as_ref()
                        .map(|s| human_label(&Value::String(s.clone())))
                        .unwrap_or_else(|| "All states".into()),
                    if state.watch_query.is_empty() {
                        "all".into()
                    } else {
                        state.watch_query.clone()
                    }
                ),
                "",
            ));
            lines.extend(select(&entries, state.watch_index, palette));
            if state.watch_index >= entries.len() as i64 {
                state.watch_index = (entries.len() as i64 - 1).max(0);
            }
        }
        "scans" => {
            let scans = values::list_of(data, "items");
            let page_hint = page_selection(state, scans, values::obj(data, "pagination"));
            let mut entries = Vec::new();
            for scan in scans {
                let partial =
                    values::truthy(values::get(scan, "crawl_partial").unwrap_or(&Value::Null))
                        || values::truthy(
                            values::get(scan, "corpus_partial").unwrap_or(&Value::Null),
                        );
                let status = if partial {
                    Value::String("partial".into())
                } else {
                    values::get(scan, "lifecycle")
                        .cloned()
                        .unwrap_or(Value::Null)
                };
                let finding_count = values::path(scan, &["evidence", "findings", "total"])
                    .map(values::scalar_text)
                    .unwrap_or_else(|| "unavailable".into());
                let uuid = values::str_of(scan, "uuid");
                entries.push(format!(
                    "{} · {} · {} findings · {} · {}",
                    human_label(values::get(scan, "source_kind").unwrap_or(&Value::Null)),
                    human_label(&status),
                    finding_count,
                    human_label(values::get(scan, "finish_reason").unwrap_or(&Value::Null)),
                    &uuid[..uuid.len().min(8)]
                ));
            }
            if let Some(scan) = scans.get(state.watch_index.max(0) as usize) {
                state.watch_selected_scan_uuid = Some(values::str_of(scan, "uuid").to_string());
            }
            lines.push(Text::new(&page_hint, ""));
            lines.extend(select(&entries, state.watch_index, palette));
            if let Some(steps) = values::get(preparation, "steps").and_then(Value::as_object) {
                for (name, step) in steps {
                    if ["crawl", "sitemap"].contains(&name.as_str()) {
                        lines.push(Text::new(
                            &format!(
                                "  {}: {} · {}",
                                name,
                                values::get(step, "state")
                                    .map(values::scalar_text)
                                    .unwrap_or_else(|| ui("unknown")),
                                values::str_of(step, "reason")
                            ),
                            "",
                        ));
                    }
                }
            }
        }
        "findings" => {
            let page = data;
            if values::truthy(&Value::Bool(!page.is_null())) {
                let items = values::list_of(page, "items");
                let pagination = values::obj(page, "pagination").clone();
                let mut merged = pagination.clone();
                merged["total"] = values::path(page, &["counts", "matched"])
                    .cloned()
                    .unwrap_or(Value::Null);
                page_selection(state, items, &merged);
                state.watch_selected_scan_uuid =
                    values::path(page, &["scan", "uuid"]).map(values::scalar_text);
                state.watch_detail_ordinal = None;
                if let Some(item) = items.get(state.watch_index.max(0) as usize) {
                    state.watch_detail_ordinal = values::int_of(item, "ordinal");
                }
                lines.push(Text::new(
                    &format!(
                        "findings {}/{} · page {} · filter={} · sort={} {}",
                        values::path(page, &["counts", "matched"])
                            .map(values::scalar_text)
                            .unwrap_or_default(),
                        values::path(page, &["counts", "source"])
                            .map(values::scalar_text)
                            .unwrap_or_default(),
                        values::int_of(&pagination, "offset").unwrap_or(0),
                        values::repr(&state.watch_query),
                        state.watch_sort,
                        if state.watch_descending {
                            "desc"
                        } else {
                            "asc"
                        }
                    ),
                    "",
                ));
                let entries: Vec<String> = items
                    .iter()
                    .map(|item| {
                        let message = values::get(item, "message")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                human_label(
                                    values::get(item, "check")
                                        .or_truthy(|| values::get(item, "id"))
                                        .unwrap_or(&Value::Null),
                                )
                            });
                        format!(
                            "{} · {} · {}",
                            human_label(values::get(item, "severity").unwrap_or(&Value::Null)),
                            message,
                            values::get(item, "target_url")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                        )
                    })
                    .collect();
                lines.extend(select(&entries, state.watch_index, palette));
                lines.push(Text::new(
                    "f filter · c clear · s sort field · r reverse · enter evidence · page keys next/previous",
                    "",
                ));
            }
        }
        "views" => {
            let mut views_snapshot = snapshot.clone();
            views_snapshot["saved_views"] = data.clone();
            let (views, page_hint) = local_page(&views_snapshot, state);
            let view_lines: Vec<String> = views
                .iter()
                .map(|item| {
                    let columns = values::path(item, &["definition", "columns"])
                        .and_then(Value::as_array)
                        .map(|cols| {
                            cols.iter()
                                .map(values::scalar_text)
                                .collect::<Vec<String>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    format!(
                        "{} · revision {} · {}",
                        values::str_of(item, "name"),
                        values::get(item, "revision")
                            .map(values::scalar_text)
                            .unwrap_or_default(),
                        columns
                    )
                })
                .collect();
            state.watch_view_name = views
                .get(state.watch_index.max(0) as usize)
                .and_then(|v| values::get(v, "name"))
                .map(values::scalar_text);
            let review = values::obj(snapshot, "review");
            lines.push(Text::new(&page_hint, ""));
            lines.extend(select(&view_lines, state.watch_index, palette));
            lines.extend([
                Text::new(
                    &format!(
                        "manual review waiting: {}",
                        values::list_of(review, "manual_waiting").len()
                    ),
                    "",
                ),
                Text::new(
                    &format!(
                        "approved deliverables ready: {}",
                        values::list_of(review, "deliverable_ready").len()
                    ),
                    "",
                ),
                Text::new(
                    "Export is an explicit CLI/MCP action; this observer does not publish anything.",
                    "",
                ),
            ]);
        }
        "activity" | "sf" => {
            let (entries, hint) = local_page(snapshot, state);
            lines.push(Text::new(&hint, ""));
            let summaries: Vec<String> = entries.iter().map(run_summary).collect();
            lines.extend(select(&summaries, state.watch_index, palette));
        }
        "inbox" => {
            let entries = values::list_of(data, "entries");
            let page_hint = page_selection(state, entries, values::obj(data, "pagination"));
            state.watch_inbox_entry_id = entries
                .get(state.watch_index.max(0) as usize)
                .and_then(|e| values::get(e, "id"))
                .map(values::scalar_text);
            lines.push(Text::new(
                &format!("Inbox · {page_hint} · Enter opens outcome"),
                "",
            ));
            let summaries: Vec<String> = entries
                .iter()
                .map(|item| {
                    let triage = values::list_of(item, "triage");
                    let last = triage
                        .last()
                        .and_then(|t| values::get(t, "kind"))
                        .map(human_label)
                        .unwrap_or_else(|| "Waiting for agent".into());
                    format!(
                        "{} · {} · {}",
                        values::str_of(item, "text").replace('\n', " "),
                        human_label(values::get(item, "kind").unwrap_or(&Value::Null)),
                        last
                    )
                })
                .collect();
            lines.extend(select(&summaries, state.watch_index, palette));
        }
        _ => {
            let (entries, hint) = local_page(snapshot, state);
            lines.push(Text::new(
                &format!("{hint} · retained log; Enter expands a line"),
                "",
            ));
            let strings: Vec<String> = entries.iter().map(values::scalar_text).collect();
            lines.extend(select(&strings, state.watch_index, palette));
            if values::truthy(values::path(snapshot, &["log", "truncated"]).unwrap_or(&Value::Null))
            {
                lines.push(Text::new(
                    "Older log bytes are outside this retained tail.",
                    "",
                ));
            }
        }
    }
    lines
}

/// The overview section body (split out for the 600-line file budget).
fn overview_lines(
    snapshot: &Value,
    progress: &Value,
    preparation: &Value,
    _palette: &Palette,
    lines: &mut Vec<Text>,
) {
    let runs = values::path(snapshot, &["runs", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let empty = json!({});
    let run = runs
        .iter()
        .find(|r| values::str_of(r, "state") == "running")
        .unwrap_or(runs.first().unwrap_or(&empty));
    let scans = values::path(snapshot, &["scans", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let latest = scans.first().unwrap_or(&empty);
    let evidence = values::obj(latest, "evidence");
    let frontier = values::path(evidence, &["frontier", "counts"])
        .cloned()
        .filter(|v| !v.is_null())
        .unwrap_or(json!({}));
    let counts = if values::truthy(values::get(run, "counters").unwrap_or(&Value::Null)) {
        values::get(run, "counters").cloned().unwrap_or(json!({}))
    } else {
        let mut c =
            json!({"fetched": values::get(&frontier, "done").cloned().unwrap_or(Value::Null)});
        if let Value::Object(map) = &frontier {
            for (k, v) in map {
                c[k] = v.clone();
            }
        }
        c
    };
    let fetched = values::get(&counts, "fetched")
        .cloned()
        .unwrap_or(Value::Null);
    let queued = values::get(&counts, "queued")
        .cloned()
        .unwrap_or(Value::Null);
    let inflight = values::get(&counts, "inflight")
        .cloned()
        .unwrap_or(Value::Null);
    let telemetry = values::obj(run, "telemetry");
    let semantics = values::str_of(telemetry, "queue_semantics");
    let semantics = if semantics.is_empty() {
        if values::truthy(&frontier) {
            "separate"
        } else {
            "unknown"
        }
    } else {
        semantics
    };
    let mut population = vec![fetched.clone(), queued.clone()];
    if semantics == "separate" {
        population.push(inflight.clone());
    }
    let total: Option<i64> = if semantics != "unknown" && population.iter().all(values::is_int) {
        Some(population.iter().filter_map(Value::as_i64).sum())
    } else {
        None
    };
    let amount = if fetched.is_null() {
        ui("unknown")
    } else {
        values::scalar_text(&fetched)
    };
    let speed_text = rate_text(telemetry);
    let collected = match total {
        Some(total) => format!(
            "Collected {amount} / {total} discovered{}",
            if total != 0 {
                format!(
                    " ({}%)",
                    crate::rlayout::py_round(
                        fetched.as_f64().unwrap_or(0.0) / total as f64 * 100.0
                    )
                )
            } else {
                String::new()
            }
        ),
        None => format!("Collected {amount}; discovered total unknown"),
    };
    let source = human_label(
        values::get(run, "kind")
            .or_truthy(|| values::get(latest, "source_kind"))
            .unwrap_or(&Value::Null),
    );
    let state_label = human_label(
        values::get(run, "state")
            .or_truthy(|| values::get(latest, "lifecycle"))
            .unwrap_or(&Value::Null),
    );
    let freshness = if values::truthy(telemetry) {
        match values::get(telemetry, "state").and_then(Value::as_str) {
            Some("stale") => ui("Stale sample"),
            other => human_label(
                &other
                    .map(|s| Value::String(s.into()))
                    .unwrap_or(Value::Null),
            ),
        }
    } else {
        ui("Retained evidence")
    };
    lines.push(Text::new(
        &format!("{source} · {state_label} · {freshness}"),
        "",
    ));
    lines.push(Text::new(&format!("{collected} · {speed_text}"), ""));
    let unknown = ui("unknown");
    lines.push(Text::new(
        &format!(
            "{} {} · {} {} · {} {}",
            ui("Queue"),
            if queued.is_null() {
                unknown.clone()
            } else {
                values::scalar_text(&queued)
            },
            ui("In flight"),
            if inflight.is_null() {
                unknown.clone()
            } else {
                values::scalar_text(&inflight)
            },
            ui("Excluded"),
            values::get(&counts, "excluded")
                .filter(|v| !v.is_null())
                .map(values::scalar_text)
                .unwrap_or(unknown),
        ),
        "",
    ));
    let sitemap = values::path(evidence, &["sitemaps", "fetch_summaries"]).cloned();
    let sitemap_step = values::path(preparation, &["steps", "sitemap"]).cloned();
    lines.push(Text::new(
        &format!(
            "Sitemap: {}",
            if sitemap.as_ref().is_some_and(values::truthy) {
                value_lines(sitemap.as_ref().unwrap(), "").join(" · ")
            } else if sitemap_step.as_ref().is_some_and(values::truthy) {
                human_label(
                    values::get(sitemap_step.as_ref().unwrap(), "state").unwrap_or(&Value::Null),
                )
            } else {
                ui("Not measured")
            }
        ),
        "",
    ));
    let completion = values::obj(progress, "audit_task_completion");
    let completion_text = values::get(completion, "percent")
        .filter(|v| !v.is_null())
        .map(|p| format!("{}%", values::scalar_text(p)))
        .unwrap_or_else(|| "scope not measured".into());
    lines.push(Text::new(
        &format!(
            "Agreed tasks: {} · Saved scans: {}",
            completion_text,
            values::path(snapshot, &["scans", "total"])
                .map(values::scalar_text)
                .unwrap_or_default()
        ),
        "",
    ));
    let methods = values::path(snapshot, &["sites", "items"])
        .and_then(Value::as_array)
        .and_then(|items| items.first().cloned())
        .map(|item| {
            values::path(&item, &["methods", "kinds"])
                .cloned()
                .unwrap_or(json!({}))
        })
        .unwrap_or(json!({}));
    let mut parts = Vec::new();
    for kind in ["scenario", "skill"] {
        let record = values::get(&methods, kind).cloned().unwrap_or(json!({}));
        let expected = values::get(&record, "expected");
        let coverage = match expected.and_then(Value::as_i64) {
            None => "scope unknown".into(),
            Some(0) => "none agreed".into(),
            Some(n) => format!(
                "{}/{}",
                values::int_of(&record, "completed").unwrap_or(0),
                n
            ),
        };
        parts.push(format!("{}s: {}", human_title(kind), coverage));
    }
    lines.push(Text::new(&parts.join(" · "), ""));
    let work: Vec<Value> = values::path(snapshot, &["active_tasks", "items"])
        .and_then(Value::as_array)
        .cloned()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| values::list_of(progress, "next_actions").to_vec());
    for item in work.iter().take(2) {
        let title = values::get(item, "title")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                ui(values::get(item, "action")
                    .or_truthy(|| values::get(item, "id"))
                    .map(values::scalar_text)
                    .as_deref()
                    .unwrap_or(""))
            });
        lines.push(Text::new(
            &format!(
                "Work: {} · {}",
                title,
                human_label(
                    values::get(item, "display_state")
                        .or_else(|| values::get(item, "state"))
                        .unwrap_or(&Value::Null)
                )
            ),
            "",
        ));
    }
    if work.is_empty() {
        lines.push(Text::new("No active task recorded.", ""));
    }
    let inbox_key = if values::get(snapshot, "latest_inbox").is_some() {
        "latest_inbox"
    } else {
        "inbox"
    };
    let latest_notes = values::list_of(values::obj(snapshot, inbox_key), "entries");
    if let Some(last) = latest_notes.last() {
        lines.push(Text::new(
            &format!(
                "Latest note: {}",
                values::str_of(last, "text").replace('\n', " ")
            ),
            "",
        ));
    }
}

/// `_watch_detail_lines` — the detail view for a selected row.
pub fn watch_detail_lines(
    project: &str,
    state: &mut ShellState,
    palette: &Palette,
    snapshot: Option<&Value>,
    data: Option<&Value>,
) -> Vec<Text> {
    let owned;
    let snapshot = match snapshot {
        Some(s) => s,
        None => {
            let (s, failure) = crate::app::watch_snapshot(project);
            match s {
                Some(snap) => {
                    owned = snap;
                    &owned
                }
                None => return failure,
            }
        }
    };
    let owned_data;
    let data = match data {
        Some(d) => d.clone(),
        None => {
            crate::app::read_view(project, state, snapshot).unwrap_or_else(|e| json!({"error": e}))
        }
    };
    owned_data = data;
    let data = &owned_data;
    if values::truthy(values::get(data, "loading").unwrap_or(&Value::Null))
        || values::get(data, "error").is_some()
    {
        return vec![Text::new(
            &values::get(data, "error")
                .map(values::scalar_text)
                .unwrap_or_else(|| "Loading retained details…".into()),
            "",
        )];
    }
    let kind = state.watch_detail_kind.clone();
    let title_style = if palette.color { &palette.title } else { "" };
    match kind.as_str() {
        "site" => {
            let site = local_entries(snapshot, "sites").into_iter().find(|item| {
                values::str_of(item, "project_uuid")
                    == state.watch_site_detail_uuid.as_deref().unwrap_or("")
            });
            let Some(site) = site else {
                return vec![Text::new(
                    "Selected site is unavailable; return and refresh.",
                    "",
                )];
            };
            state.watch_site_openable = !values::get(&site, "directory")
                .unwrap_or(&Value::Null)
                .is_null();
            let mut lines = vec![
                Text::new(
                    &values::path(&site, &["site", "label"])
                        .or_truthy(|| values::path(&site, &["site", "target"]))
                        .map(values::scalar_text)
                        .unwrap_or_default(),
                    title_style,
                ),
                Text::new(
                    &human_label(values::get(&site, "role").unwrap_or(&Value::Null)),
                    "",
                ),
                Text::new(
                    if state.watch_site_openable {
                        "s  Browse this site's complete scan history"
                    } else {
                        "Candidate suggestion only; no workspace or scan is implied."
                    },
                    "",
                ),
                Text::blank(),
            ];
            for key in [
                "project_uuid",
                "directory",
                "candidate",
                "source_note_id",
                "reason",
                "recorded_at",
                "coverage",
                "methods",
                "scans",
                "runs",
            ] {
                if let Some(value) = values::get(&site, key) {
                    lines.extend(
                        value_lines(&json!({key: value}), "")
                            .into_iter()
                            .map(|line| Text::new(&line, "")),
                    );
                }
            }
            return lines;
        }
        "task" => {
            let item = values::obj(data, "item");
            let mut lines = vec![
                Text::new(&human_title(values::str_of(item, "title")), title_style),
                Text::new(
                    &format!(
                        "{} · {}",
                        human_label(
                            values::get(item, "display_state")
                                .or_else(|| values::get(item, "state"))
                                .unwrap_or(&Value::Null)
                        ),
                        values::str_of(item, "id")
                    ),
                    "",
                ),
                Text::blank(),
            ];
            for key in [
                "reason",
                "scope",
                "priority",
                "applicability",
                "blocked_by",
                "attempt_status",
                "measurement",
                "attempts",
                "complete",
                "stale",
            ] {
                if let Some(value) = values::get(item, key) {
                    lines.extend(
                        value_lines(&json!({key: value}), "")
                            .into_iter()
                            .map(|line| Text::new(&line, "")),
                    );
                }
            }
            for key in [
                "definition",
                "history",
                "history_total",
                "history_truncated",
                "evidence",
                "review",
                "catalogue",
                "content",
            ] {
                let value = values::get(data, key).or_else(|| values::get(item, key));
                if let Some(value) = value.filter(|v| !v.is_null()) {
                    lines.push(Text::blank());
                    lines.push(Text::new(&human_label(&json!(key)), title_style));
                    lines.extend(
                        value_lines(value, "")
                            .into_iter()
                            .map(|line| Text::new(&line, "")),
                    );
                }
            }
            return lines;
        }
        "inbox" => {
            let entry = values::list_of(data, "entries").iter().find(|item| {
                values::str_of(item, "id") == state.watch_inbox_entry_id.as_deref().unwrap_or("")
            });
            let Some(entry) = entry else {
                return vec![Text::new(
                    "Selected inbox entry is unavailable; return and refresh.",
                    "",
                )];
            };
            let mut lines = vec![
                Text::new(
                    &human_label(values::get(entry, "kind").unwrap_or(&Value::Null)),
                    title_style,
                ),
                Text::new(
                    &format!(
                        "{} · {}",
                        values::str_of(entry, "created_at"),
                        values::str_of(entry, "id")
                    ),
                    "",
                ),
                Text::new(values::str_of(entry, "text"), ""),
                Text::blank(),
            ];
            lines.extend(
                value_lines(
                    &json!({
                        "goal_state": values::get(entry, "goal_state").cloned().unwrap_or(Value::Null),
                        "references": values::get(entry, "references").cloned().unwrap_or(json!([])),
                        "triage": values::get(entry, "triage").cloned().unwrap_or(json!([])),
                    }),
                    "",
                )
                .into_iter()
                .map(|line| Text::new(&line, "")),
            );
            if values::list_of(entry, "triage").is_empty() {
                lines.push(Text::new(
                    "Saved locally; waiting for an agent triage receipt.",
                    "",
                ));
            }
            return lines;
        }
        "scan" => {
            let scan = values::list_of(data, "items")
                .iter()
                .find(|item| {
                    values::str_of(item, "uuid")
                        == state.watch_selected_scan_uuid.as_deref().unwrap_or("")
                })
                .cloned()
                .unwrap_or_else(|| json!({"state": "Selected scan unavailable"}));
            return value_lines(&scan, "")
                .into_iter()
                .map(|line| Text::new(&line, ""))
                .collect();
        }
        "view" => {
            let page = values::obj(data, "result");
            let pagination = values::obj(page, "pagination");
            state.watch_view_page_size = values::int_of(pagination, "page_size").unwrap_or(50);
            state.watch_view_next = values::int_of(pagination, "next_offset");
            let mut lines = vec![
                Text::new(
                    &format!(
                        "Saved view · {}",
                        state.watch_view_name.as_deref().unwrap_or("")
                    ),
                    "",
                ),
                Text::new(
                    &format!(
                        "Matched {} / {} findings",
                        values::path(page, &["counts", "matched"])
                            .map(values::scalar_text)
                            .unwrap_or_default(),
                        values::path(page, &["counts", "source"])
                            .map(values::scalar_text)
                            .unwrap_or_default()
                    ),
                    "",
                ),
            ];
            lines.extend(
                value_lines(values::obj(page, "items"), "")
                    .into_iter()
                    .map(|line| Text::new(&line, "")),
            );
            lines.push(Text::new(
                &format!(
                    "Results offset {} · [ Previous / ] Next result page",
                    values::int_of(pagination, "offset").unwrap_or(0)
                ),
                "",
            ));
            return lines;
        }
        "activity" | "sf" | "log" => {
            let entries = local_entries(snapshot, &state.watch_section);
            let selected = (state.watch_offset + state.watch_index) as usize;
            return value_lines(
                entries
                    .get(selected)
                    .unwrap_or(&Value::String("Entry unavailable".into())),
                "",
            )
            .into_iter()
            .map(|line| Text::new(&line, ""))
            .collect();
        }
        _ => {}
    }
    let finding = values::obj(data, "finding");
    let mut lines =
        vec![
        Text::new(
            &values::get(finding, "message")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    human_label(
                        values::get(finding, "check")
                            .or_truthy(|| values::get(finding, "id"))
                            .unwrap_or(&Value::Null),
                    )
                }),
            title_style,
        ),
        Text::new(
            &format!(
                "{} · {}",
                human_label(values::get(finding, "severity").unwrap_or(&Value::Null)),
                values::get(finding, "target_url")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Target not recorded")
            ),
            "",
        ),
        Text::new(
            &format!(
                "Check {} · scan {} · ordinal {}",
                values::get(finding, "check").map(values::scalar_text).unwrap_or_default(),
                values::get(data, "scan_uuid").map(values::scalar_text).unwrap_or_default(),
                values::int_of(finding, "ordinal").unwrap_or(0)
            ),
            "",
        ),
        Text::blank(),
        Text::new("Retained evidence:", ""),
        Text::new(
            "Preview limits: 4,096 characters/value, 20 list entries, 40 object fields, 3 levels.",
            "",
        ),
    ];
    lines.extend(
        value_lines(values::obj(data, "evidence"), "")
            .into_iter()
            .map(|line| Text::new(&line, "")),
    );
    lines.push(Text::blank());
    lines.push(Text::new("Retained projection; no check was rerun.", ""));
    lines.extend(
        value_lines(values::get(data, "truncation").unwrap_or(&json!({})), "")
            .into_iter()
            .map(|line| Text::new(&line, "")),
    );
    lines
}
