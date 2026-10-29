//! Yandex region IDs for the Wordstat and SERP `regions[]` parameters.
//!
//! Static tables are the fast path; `fetch_tree` reads the live region tree.
//! `getRegionsTree` is the only free Wordstat method, so it never touches the spend ledger.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::http::Transport;
use crate::yandex_cloud::{Cloud, CloudArgs, CloudEnv, HOST};
use crate::{pyjson, pystr};

/// Region ID for the whole of Russia.
pub const RUSSIA: &str = "225";

/// Top-level regions. Keys are the localized labels the API uses.
pub const TOP: &[(&str, &str)] = &[
    ("Россия", "225"),
    ("Москва", "213"),
    ("Москва и область", "1"),
    ("Санкт-Петербург", "2"),
];

/// Federal districts. Note: ID 73 is a district, and 75 is the city of Vladivostok.
pub const DISTRICTS: &[(&str, &str)] = &[
    ("Центр", "3"),
    ("Северо-Запад", "17"),
    ("Юг", "26"),
    ("Поволжье", "40"),
    ("Урал", "52"),
    ("Сибирь", "59"),
    ("Дальний Восток", "73"),
    ("Северный Кавказ", "102444"),
];

/// Official district names mapped to the labels the API returns.
pub const DISTRICT_ALIASES: &[(&str, &str)] = &[
    ("Центральный", "Центр"),
    ("Северо-Западный", "Северо-Запад"),
    ("Южный", "Юг"),
    ("Приволжский", "Поволжье"),
    ("Уральский", "Урал"),
    ("Сибирский", "Сибирь"),
    ("Дальневосточный", "Дальний Восток"),
    ("Северо-Кавказский", "Северный Кавказ"),
];

/// Frequently used city regions.
pub const CITIES: &[(&str, &str)] = &[
    ("Екатеринбург", "54"),
    ("Казань", "43"),
    ("Новосибирск", "65"),
    ("Самара", "51"),
    ("Уфа", "172"),
    ("Пермь", "50"),
    ("Краснодар", "35"),
    ("Ростов-на-Дону", "39"),
    ("Челябинск", "56"),
    ("Нижний Новгород", "47"),
    ("Красноярск", "62"),
    ("Воронеж", "193"),
    ("Тюмень", "55"),
    ("Махачкала", "28"),
    ("Минеральные Воды", "11063"),
    ("Калининград", "22"),
    ("Владивосток", "75"),
    ("Сочи", "239"),
];

/// Date the static IDs were last checked against the live region tree.
pub const VERIFIED_AT: &str = "2026-08-21";

/// All city IDs, in table order.
pub fn all_city_ids() -> Vec<&'static str> {
    CITIES.iter().map(|(_, id)| *id).collect()
}

/// All district IDs, in table order.
pub fn all_district_ids() -> Vec<&'static str> {
    DISTRICTS.iter().map(|(_, id)| *id).collect()
}

/// Resolves a region ID from an official district name or an API name. Returns `None` when unknown, so callers can fall back to `fetch_tree`.
pub fn by_name(name: &str) -> Option<String> {
    let name = DISTRICT_ALIASES
        .iter()
        .find(|(official, _)| *official == name)
        .map(|(_, api)| *api)
        .unwrap_or(name);
    for table in [TOP, DISTRICTS, CITIES] {
        if let Some((_, id)) = table.iter().find(|(label, _)| *label == name) {
            return Some((*id).to_owned());
        }
    }
    None
}

/// Returns the full `getRegionsTree` response as a `{name: region_id}` map.
/// Walks every nested value, because the API's nesting has changed over time. This is a free call: not billed and not recorded.
pub async fn fetch_tree<T: Transport>(
    env: &CloudEnv<'_, T>,
    args: &CloudArgs<'_>,
    save_to: Option<&Value>,
) -> Result<Value, crate::yandex_cloud::CloudError> {
    let client = match Cloud::new(env, args) {
        Ok(client) => client,
        // Only a missing credential becomes an error response; other errors propagate.
        Err(err @ crate::yandex_cloud::CloudError::MissingCredential(_)) => {
            return Ok(json!({"ok": false, "error": err.to_string()}))
        }
        Err(err) => return Err(err),
    };
    let url = format!("{HOST}/v2/wordstat/getRegionsTree");
    let body = json!({"folderId": client.folder});
    let (status, body) = client
        .request(&url, Some(&body), "POST", 5, false, None)
        .await?;
    let mut flat = Map::new();
    walk(&body, &mut flat);
    if flat.is_empty() {
        return Ok(json!({
            "ok": false,
            "error": format!("region tree could not be parsed (status {status})"),
            "raw": body,
        }));
    }
    // An empty `save_to` skips the write. A non-string value is a type error, checked after the request.
    let save_to = match save_to {
        Some(value) if pystr::truthy(Some(value)) => match value {
            Value::String(text) => Some(text.clone()),
            other => {
                return Err(crate::yandex_cloud::CloudError::python(
                    "TypeError",
                    format!(
                        "argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
                        pystr::type_name(other)
                    ),
                ))
            }
        },
        _ => None,
    };
    if let Some(text) = &save_to {
        let expanded = expand_tilde(Path::new(text), env.host);
        if let Some(parent) = expanded.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                crate::yandex_cloud::CloudError::python("OSError", err.to_string())
            })?;
        }
        // Writes the map as pretty-printed UTF-8 JSON.
        std::fs::write(&expanded, dumps_indent2(&flat))
            .map_err(|err| crate::yandex_cloud::CloudError::python("OSError", err.to_string()))?;
    }
    Ok(json!({
        "ok": true,
        "count": flat.len(),
        "regions": flat,
        "saved_to": save_to.map(|text| {
            expand_tilde(Path::new(&text), env.host)
                .to_string_lossy()
                .into_owned()
        }),
    }))
}

/// Recursively collects every node with a non-empty id (`id` or `value`) and name (`name` or `label`).
fn walk(node: &Value, flat: &mut Map<String, Value>) {
    match node {
        Value::Object(map) => {
            let region_id = map
                .get("id")
                .filter(|v| pystr::truthy(Some(v)))
                .or_else(|| map.get("value").filter(|v| pystr::truthy(Some(v))));
            let name = map
                .get("name")
                .filter(|v| pystr::truthy(Some(v)))
                .or_else(|| map.get("label").filter(|v| pystr::truthy(Some(v))));
            if let (Some(id), Some(name)) = (region_id, name) {
                flat.insert(pystr::repr_value(name), json!(pystr::repr_value(id)));
            }
            for value in map.values() {
                if matches!(value, Value::Object(_) | Value::Array(_)) {
                    walk(value, flat);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, flat);
            }
        }
        _ => {}
    }
}

/// Pretty-prints the map as JSON with a two-space indent and unescaped UTF-8.
fn dumps_indent2(flat: &Map<String, Value>) -> String {
    if flat.is_empty() {
        return "{}".to_owned();
    }
    let mut out = String::from("{");
    let mut first = true;
    for (key, value) in flat {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str("\n  ");
        out.push_str(&pyjson::dumps_opts(&json!(key), false, false));
        out.push_str(": ");
        out.push_str(&pyjson::dumps_opts(value, false, false));
    }
    out.push('\n');
    out.push('}');
    out
}

/// Expands a leading `~` to `HOME` and normalizes the path lexically (`.` and repeated slashes removed).
fn expand_tilde(path: &Path, host: &dyn spiderhead_core::host::Host) -> std::path::PathBuf {
    let text = path.to_string_lossy();
    let expanded = if text == "~" || text.starts_with("~/") {
        match host.var("HOME").filter(|v| !v.is_empty()) {
            Some(home) => format!("{home}{}", &text[1..]),
            None => text.into_owned(),
        }
    } else {
        text.into_owned()
    };
    std::path::PathBuf::from(normalize_posix(&expanded))
}

/// Lexical path normalization: repeated slashes collapse (a leading `//` is kept), `.` segments are dropped, and `..` is kept.
fn normalize_posix(text: &str) -> String {
    let mut out = String::new();
    let leading = text.len() - text.trim_start_matches('/').len();
    if leading == 2 {
        out.push_str("//");
    } else if leading >= 1 {
        out.push('/');
    }
    let mut first = true;
    for part in text.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if !first {
            out.push('/');
        }
        first = false;
        out.push_str(part);
    }
    if out.is_empty() && leading == 0 {
        return ".".to_owned();
    }
    out
}
