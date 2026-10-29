//! Scheduled project scans: settings in `schedule.json`, due rules, and a headless
//! runner.
//!
//! Settings are a revisioned project document, edited through `write_document` with
//! `expected_revision`. Runtime facts (last window, last artifact, lock) live in
//! `schedule-state.json`. `run_due` is safe to call often: it does nothing outside
//! the configured window, refuses overlap, and records one journal entry per decision.
//! The scan itself is injected by the interface layer, so the core never imports MCP.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use jiff::{civil::Date, civil::Time, tz::TimeZone, Span, Zoned};
use serde_json::{json, Map, Value};

use crate::error::{value, ProjectsError, ProjectsResult};
use crate::runtime::{competitor_identity, read_document, write_document};
use crate::workspace::load as workspace_load;

mod scanfailure;
mod state_default;
pub use scanfailure::*;
pub use state_default::*;

/// Settings document format.
pub const FORMAT: &str = "spiderhead.schedule.v1";
/// State document format.
pub const STATE_FORMAT: &str = "spiderhead.schedule-state.v1";
/// Settings filename.
pub const NAME: &str = "schedule.json";
/// State filename.
pub const STATE_NAME: &str = "schedule-state.json";
/// Lock filename.
pub const LOCK_NAME: &str = ".schedule.lock";
/// Allowed crawl modes.
pub const MODES: &[&str] = &["spider", "sitemap", "list"];
const WEEKDAYS: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const MAX_URLS: i64 = 1_000_000;
const MAX_LIST: usize = 500;

fn weekday_of(name: &str) -> i64 {
    match name {
        "mon" => 0,
        "tue" => 1,
        "wed" => 2,
        "thu" => 3,
        "fri" => 4,
        "sat" => 5,
        _ => 6,
    }
}

/// Host suffix -> IANA zone. Only a site's TLD is consulted.
fn tld_zone(suffix: &str) -> Option<&'static str> {
    Some(match suffix {
        "by" => "Europe/Minsk",
        "ru" => "Europe/Moscow",
        "ua" => "Europe/Kyiv",
        "kz" => "Asia/Almaty",
        "de" => "Europe/Berlin",
        "fr" => "Europe/Paris",
        "uk" => "Europe/London",
        "pl" => "Europe/Warsaw",
        "it" => "Europe/Rome",
        "es" => "Europe/Madrid",
        "nl" => "Europe/Amsterdam",
        "tr" => "Europe/Istanbul",
        "jp" => "Asia/Tokyo",
        "cn" => "Asia/Shanghai",
        "in" => "Asia/Kolkata",
        "au" => "Australia/Sydney",
        "br" => "America/Sao_Paulo",
        _ => return None,
    })
}

/// Default settings for a project UUID.
pub fn default_settings(project_uuid: &str) -> Value {
    json!({
        "format": FORMAT,
        "project_uuid": project_uuid,
        "revision": 0,
        "enabled": false,
        "time_zone": "auto",
        "weekdays": [],
        "month_days": [],
        "window": {"start": "01:00", "end": "06:00"},
        "include_competitors": false,
        "crawl": {
            "mode": "spider",
            "sitemap_url": null,
            "urls": [],
            "max_urls": 500,
            "max_requests": 2_000,
            "min_delay_seconds": 1.0,
            "slice": false,
        },
        "user_agent": {"preset": "spiderhead", "custom": null},
    })
}

/// Parse a strict `HH:MM` time.
fn clock(raw: &Value, label: &str) -> ProjectsResult<Time> {
    let Some(text) = raw.as_str() else {
        return Err(value(format!("{label} must be HH:MM")));
    };
    let bytes = text.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return Err(value(format!("{label} must be HH:MM")));
    }
    let hour: i8 = text[..2]
        .parse()
        .map_err(|_| value(format!("{label} must be HH:MM")))?;
    let minute: i8 = text[3..]
        .parse()
        .map_err(|_| value(format!("{label} must be HH:MM")))?;
    Time::new(hour, minute, 0, 0).map_err(|_| value(format!("{label} must be HH:MM")))
}

fn bounded_int(raw: &Value, label: &str, ceiling: i64) -> ProjectsResult<i64> {
    match raw.as_i64() {
        Some(v) if (1..=ceiling).contains(&v) => Ok(v),
        _ => Err(value(format!(
            "{label} must be an integer from 1 to {ceiling}"
        ))),
    }
}

/// Return a normalized copy of a settings object, or an error.
pub fn validate(settings: &Value) -> ProjectsResult<Value> {
    let Some(out_obj) = settings.as_object() else {
        return Err(value("schedule settings must be an object"));
    };
    let allowed: BTreeSet<&str> = [
        "format",
        "project_uuid",
        "revision",
        "enabled",
        "time_zone",
        "weekdays",
        "month_days",
        "window",
        "include_competitors",
        "crawl",
        "user_agent",
    ]
    .into_iter()
    .collect();
    let unknown: Vec<String> = out_obj
        .keys()
        .filter(|k| !allowed.contains(k.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        let mut sorted = unknown;
        sorted.sort();
        return Err(value(format!(
            "schedule has unsupported fields: {}",
            sorted.join(", ")
        )));
    }
    let mut out = out_obj.clone();
    if !out.get("enabled").is_some_and(|v| v.is_boolean()) {
        return Err(value("schedule enabled must be boolean"));
    }
    if !out
        .get("include_competitors")
        .is_some_and(|v| v.is_boolean())
    {
        return Err(value("include_competitors must be boolean"));
    }
    match out.get("time_zone") {
        Some(v) if v.as_str() == Some("auto") => {}
        Some(v) if v.is_string() => {
            crate::pytime::zone(v.as_str().unwrap())
                .map_err(|_| value(format!("unknown time_zone '{}'", v.as_str().unwrap())))?;
        }
        _ => return Err(value("time_zone must be 'auto' or an IANA zone name")),
    }
    let weekdays = out
        .get("weekdays")
        .and_then(Value::as_array)
        .filter(|list| {
            list.iter().all(|item| {
                item.as_str()
                    .is_some_and(|s| WEEKDAYS.contains(&s.to_lowercase().as_str()))
            })
        })
        .ok_or_else(|| value("weekdays must be a list of mon..sun"))?;
    let mut seen = BTreeSet::new();
    let mut sorted_weekdays: Vec<String> = weekdays
        .iter()
        .filter_map(|item| item.as_str().map(|s| s.to_lowercase()))
        .filter(|name| seen.insert(weekday_of(name)))
        .collect();
    sorted_weekdays.sort_by_key(|name| weekday_of(name));
    out.insert("weekdays".into(), json!(sorted_weekdays));
    let month_days = out
        .get("month_days")
        .and_then(Value::as_array)
        .filter(|list| {
            list.iter()
                .all(|item| item.as_i64().is_some_and(|d| (1..=31).contains(&d)))
        })
        .ok_or_else(|| value("month_days must be integers 1..31"))?;
    let mut days: Vec<i64> = month_days.iter().filter_map(|item| item.as_i64()).collect();
    days.sort_unstable();
    days.dedup();
    out.insert("month_days".into(), json!(days));
    let window = out
        .get("window")
        .and_then(Value::as_object)
        .filter(|w| {
            w.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == ["start", "end"].into_iter().collect()
        })
        .ok_or_else(|| value("window requires start and end as HH:MM"))?;
    clock(&window["start"], "window.start")?;
    clock(&window["end"], "window.end")?;
    if window["start"] == window["end"] {
        return Err(value("window.start and window.end must differ"));
    }
    let crawl = out
        .get("crawl")
        .and_then(Value::as_object)
        .filter(|c| {
            c.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == [
                    "mode",
                    "sitemap_url",
                    "urls",
                    "max_urls",
                    "max_requests",
                    "min_delay_seconds",
                    "slice",
                ]
                .into_iter()
                .collect()
        })
        .ok_or_else(|| {
            value(format!(
                "crawl requires {}",
                [
                    "max_requests",
                    "max_urls",
                    "min_delay_seconds",
                    "mode",
                    "sitemap_url",
                    "slice",
                    "urls",
                ]
                .join(", ")
            ))
        })?;
    if !MODES.contains(&crawl["mode"].as_str().unwrap_or_default()) {
        return Err(value("crawl.mode must be spider, sitemap or list"));
    }
    if !crawl["sitemap_url"].is_null() && !is_http(&crawl["sitemap_url"]) {
        return Err(value("crawl.sitemap_url must be an absolute HTTP(S) URL"));
    }
    if crawl["mode"].as_str() == Some("sitemap") && crawl["sitemap_url"].is_null() {
        return Err(value("sitemap mode requires crawl.sitemap_url"));
    }
    let urls = crawl["urls"].as_array().filter(|list| {
        list.len() <= MAX_LIST
            && {
                let mut seen: Vec<&Value> = Vec::new();
                !list.iter().any(|item| {
                    let dup = seen.contains(&item);
                    seen.push(item);
                    dup
                })
            }
            && list.iter().all(is_http)
    });
    if urls.is_none() {
        return Err(value(
            "crawl.urls must be a unique bounded list of absolute HTTP(S) URLs",
        ));
    }
    bounded_int(&crawl["max_urls"], "crawl.max_urls", MAX_URLS)?;
    bounded_int(&crawl["max_requests"], "crawl.max_requests", MAX_URLS * 10)?;
    if !crawl["slice"].is_boolean() {
        return Err(value("crawl.slice must be boolean"));
    }
    match crawl["min_delay_seconds"].as_f64() {
        Some(delay) if (0.5..=60.0).contains(&delay) && crawl["min_delay_seconds"].is_number() => {}
        _ => return Err(value("crawl.min_delay_seconds must be between 0.5 and 60")),
    }
    let agent = out
        .get("user_agent")
        .and_then(Value::as_object)
        .filter(|a| {
            a.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == ["preset", "custom"].into_iter().collect()
        })
        .ok_or_else(|| value("user_agent requires preset and custom"))?;
    if !agent["custom"].is_null() {
        spiderhead_core::user_agents::resolve(agent["custom"].as_str())
            .map_err(|e| value(e.to_string()))?;
    } else {
        if !agent["preset"].is_string() {
            return Err(value("user_agent.preset must be a preset name"));
        }
        spiderhead_core::user_agents::resolve(agent["preset"].as_str())
            .map_err(|e| value(e.to_string()))?;
    }
    Ok(Value::Object(out))
}

fn is_http(value: &Value) -> bool {
    match value.as_str() {
        Some(s) if s.starts_with("http://") || s.starts_with("https://") => {
            crate::urls::urlsplit(s)
                .map(|p| !p.netloc.is_empty())
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// Return `(zone, source)`: a TLD-based guess, or UTC when unmapped.
pub fn infer_time_zone(host: &str) -> (&'static str, &'static str) {
    let suffix = if host.contains('.') {
        host.rsplit('.').next().unwrap_or_default().to_lowercase()
    } else {
        String::new()
    };
    match tld_zone(&suffix) {
        Some(zone) => (zone, "tld"),
        None => ("UTC", "fallback"),
    }
}

fn zone_for(settings: &Value, host: &str) -> ProjectsResult<(TimeZone, &'static str)> {
    if settings["time_zone"].as_str() == Some("auto") {
        let (name, source) = infer_time_zone(host);
        return Ok((crate::pytime::zone(name)?, source));
    }
    Ok((
        crate::pytime::zone(settings["time_zone"].as_str().unwrap_or("UTC"))?,
        "explicit",
    ))
}

fn clock_tuple(time: Time) -> (i8, i8, i8, i32) {
    (
        time.hour(),
        time.minute(),
        time.second(),
        time.subsec_nanosecond(),
    )
}

/// The local date the window belongs to, when `local` is inside it.
fn window_day(local: &Zoned, start: Time, end: Time) -> Option<Date> {
    let current = clock_tuple(local.datetime().time());
    if clock_tuple(start) < clock_tuple(end) {
        if clock_tuple(start) <= current && current < clock_tuple(end) {
            return Some(local.datetime().date());
        }
        return None;
    }
    if current >= clock_tuple(start) {
        return Some(local.datetime().date());
    }
    if current < clock_tuple(end) {
        let prev = crate::pytime::zoned_add(local, Span::new().days(-1)).ok()?;
        return Some(prev.datetime().date());
    }
    None
}

fn day_matches(settings: &Value, day: Date) -> bool {
    let weekdays: Vec<String> = settings["weekdays"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let month_days: Vec<i64> = settings["month_days"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    if weekdays.is_empty() && month_days.is_empty() {
        return true;
    }
    let weekday = i64::from(day.weekday().to_monday_zero_offset());
    let weekday_ok = weekdays.iter().any(|name| weekday_of(name) == weekday);
    weekday_ok || month_days.contains(&i64::from(day.day()))
}

/// Current instant for scheduling decisions. `SPIDERHEAD_NOW` (RFC3339) pins it so
/// time-of-day windows are reproducible in tests.
pub fn now() -> Zoned {
    if let Ok(raw) = std::env::var("SPIDERHEAD_NOW") {
        if let Ok(ts) = raw.trim().parse::<jiff::Timestamp>() {
            return ts.in_tz("UTC").unwrap_or_else(|_| Zoned::now());
        }
    }
    Zoned::now()
}

/// Whether a run is due at `now` (aware UTC), without side effects.
pub fn due_state(
    settings: &Value,
    host: &str,
    now: &Zoned,
    last_window: Option<&str>,
) -> ProjectsResult<Value> {
    if settings["enabled"].as_bool() != Some(true) {
        return Ok(json!({"due": false, "reason": "disabled"}));
    }
    let (zone, source) = zone_for(settings, host)?;
    let local = now.with_time_zone(zone.clone());
    let start = clock(&settings["window"]["start"], "window.start")?;
    let end = clock(&settings["window"]["end"], "window.end")?;
    let day = window_day(&local, start, end);
    let base = json!({
        "local_time": crate::pytime::isoformat_zoned(&local),
        "time_zone": zone.iana_name().unwrap_or("UTC"),
        "time_zone_source": source,
    });
    let Some(day) = day else {
        let mut out = base.as_object().unwrap().clone();
        out.insert("due".into(), json!(false));
        out.insert("reason".into(), json!("outside window"));
        return Ok(Value::Object(out));
    };
    if !day_matches(settings, day) {
        let mut out = base.as_object().unwrap().clone();
        out.insert("due".into(), json!(false));
        out.insert("reason".into(), json!("day not selected"));
        return Ok(Value::Object(out));
    }
    let key = crate::pytime::date_iso(day);
    let mut out = base.as_object().unwrap().clone();
    if last_window == Some(key.as_str()) {
        out.insert("due".into(), json!(false));
        out.insert("reason".into(), json!("already ran in this window"));
        out.insert("window".into(), json!(key));
        return Ok(Value::Object(out));
    }
    out.insert("due".into(), json!(true));
    out.insert("reason".into(), json!("due"));
    out.insert("window".into(), json!(key));
    Ok(Value::Object(out))
}

/// First window opening within 15 local days, as UTC ISO text.
pub fn next_due(settings: &Value, host: &str, now: &Zoned) -> ProjectsResult<Option<String>> {
    if settings["enabled"].as_bool() != Some(true) {
        return Ok(None);
    }
    let (zone, _) = zone_for(settings, host)?;
    let start = clock(&settings["window"]["start"], "window.start")?;
    let local = now.with_time_zone(zone.clone());
    let date = local.datetime().date();
    for offset in 0..15 {
        let day = date
            .checked_add(Span::new().days(offset))
            .map_err(|e| value(e.to_string()))?;
        if !day_matches(settings, day) {
            continue;
        }
        let opening = crate::pytime::combine_zoned(day, start, &zone)?;
        if opening.timestamp() >= now.timestamp() {
            let utc = opening.with_time_zone(TimeZone::UTC);
            let dt = utc.datetime();
            return Ok(Some(format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second()
            )));
        }
    }
    Ok(None)
}
