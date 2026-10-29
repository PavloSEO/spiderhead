//! Oracle parity for `gsc_inspection_history` — replays the step scripts recorded by
//! the reference suite (seed → reserve/settle/latest) and compares every
//! result or error class/message with the reference.
//!

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_sources::gsc_inspection_history as gih;
use spiderhead_sources::pytime;

/// `uuid.UUID(int=n)` rendered like `str(...)`.
fn uuid_of(index: u64) -> String {
    let hex = format!("{index:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The generator's `$repeat`/`$range` markers.
fn expand(value: &Value) -> Value {
    if let Some(text) = value.as_str() {
        if let Some(rest) = text.strip_prefix("$repeat:") {
            if let Some((body, count)) = rest.rsplit_once(':') {
                return Value::String(body.repeat(count.parse::<usize>().unwrap()));
            }
        }
        return value.clone();
    }
    if let Some(map) = value.as_object() {
        if let Some(range) = map.get("$range").and_then(Value::as_array) {
            let prefix = range[0].as_str().unwrap_or("");
            let count = range[1].as_u64().unwrap_or(0);
            return json!((0..count)
                .map(|i| format!("{prefix}{i}"))
                .collect::<Vec<_>>());
        }
        if let Some(count) = map.get("$repeat").and_then(Value::as_u64) {
            let row = expand(&map["row"]);
            return json!(vec![row; count as usize]);
        }
        return json!(map
            .iter()
            .map(|(k, v)| (k.clone(), expand(v)))
            .collect::<serde_json::Map<_, _>>());
    }
    if let Some(items) = value.as_array() {
        return json!(items.iter().map(expand).collect::<Vec<_>>());
    }
    value.clone()
}

fn write_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .unwrap();
    }
    #[cfg(not(unix))]
    fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
}

fn chmod(path: &Path, mode: u32) {
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode)).unwrap();
    let _ = (path, mode);
}

/// `seed_quota` — build the ledger file with an arbitrary format/schema and insert rows.
fn seed_quota(config_root: &Path, spec: &Value) {
    let dir = config_root.join("spiderhead");
    if dir.symlink_metadata().is_err() {
        fs::create_dir_all(&dir).unwrap();
        chmod(&dir, 0o700);
    }
    let path = dir.join("gsc-inspection-quota.sqlite");
    if path.symlink_metadata().is_err() {
        write_private(&path);
    }
    let con = Connection::open(&path).unwrap();
    con.execute_batch("BEGIN").unwrap();
    if spec.get("tables").and_then(Value::as_str) == Some("other") {
        con.execute_batch("CREATE TABLE junk(x)").unwrap();
    } else {
        let format = spec
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or(gih::QUOTA_FORMAT);
        con.execute_batch("CREATE TABLE metadata(format TEXT PRIMARY KEY)")
            .unwrap();
        con.execute("INSERT INTO metadata VALUES (?)", [format])
            .unwrap();
        let urls_col = if spec
            .get("no_urls")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            ""
        } else {
            ",urls TEXT NOT NULL DEFAULT '[]'"
        };
        con.execute_batch(&format!(
            "CREATE TABLE reservations(id TEXT PRIMARY KEY,site_url TEXT NOT NULL,\
             reserved_at TEXT NOT NULL,count INTEGER NOT NULL,state TEXT NOT NULL{urls_col});\
             CREATE INDEX reservations_site_time ON reservations(site_url,reserved_at);"
        ))
        .unwrap();
    }
    for row in expand(&spec["rows"])
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let map = row.as_object().unwrap();
        let cols: Vec<&String> = map.keys().collect();
        let sql = format!(
            "INSERT INTO reservations({}) VALUES({})",
            cols.iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(","),
            cols.iter().map(|_| "?").collect::<Vec<_>>().join(",")
        );
        let params: Vec<rusqlite::types::Value> = cols
            .iter()
            .map(|c| match &map[*c] {
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                other => rusqlite::types::Value::Integer(other.as_i64().unwrap()),
            })
            .collect();
        con.execute(&sql, rusqlite::params_from_iter(params))
            .unwrap();
    }
    con.execute_batch("COMMIT").unwrap();
}

/// `seed_history` — build the history file with an arbitrary format and insert rows.
fn seed_history(project: &Path, spec: &Value) {
    let path = project.join(gih::DB_NAME);
    if path.symlink_metadata().is_err() {
        write_private(&path);
    }
    let con = Connection::open(&path).unwrap();
    con.execute_batch("BEGIN").unwrap();
    if spec.get("tables").and_then(Value::as_str) == Some("other") {
        con.execute_batch("CREATE TABLE junk(x)").unwrap();
    } else {
        let format = spec
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or(gih::FORMAT);
        con.execute_batch("CREATE TABLE metadata(format TEXT PRIMARY KEY)")
            .unwrap();
        con.execute("INSERT INTO metadata VALUES (?)", [format])
            .unwrap();
        con.execute_batch(
            "CREATE TABLE inspections(id INTEGER PRIMARY KEY,site_url TEXT NOT NULL,\
             url TEXT NOT NULL,inspected_at TEXT NOT NULL,state TEXT NOT NULL,\
             index_status TEXT,amp_result TEXT,rich_results_result TEXT);\
             CREATE INDEX inspections_site_time \
             ON inspections(site_url,inspected_at DESC,id DESC);",
        )
        .unwrap();
    }
    for (index, row) in expand(&spec["rows"])
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let mut map = row.as_object().unwrap().clone();
        for value in map.values_mut() {
            if let Some(prefix) = value.get("$range").and_then(Value::as_str) {
                *value = json!(format!("{prefix}{index}"));
            }
        }
        let cols: Vec<&String> = map.keys().collect();
        let sql = format!(
            "INSERT INTO inspections({}) VALUES({})",
            cols.iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(","),
            cols.iter().map(|_| "?").collect::<Vec<_>>().join(",")
        );
        let params: Vec<rusqlite::types::Value> = cols
            .iter()
            .map(|c| match &map[*c] {
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                Value::Null => rusqlite::types::Value::Null,
                other => rusqlite::types::Value::Integer(other.as_i64().unwrap()),
            })
            .collect();
        con.execute(&sql, rusqlite::params_from_iter(params))
            .unwrap();
    }
    con.execute_batch("COMMIT").unwrap();
    if let Some(mode) = spec.get("mode").and_then(Value::as_str) {
        chmod(&path, u32::from_str_radix(mode, 8).unwrap());
    }
}

fn parse_now(args: &Value) -> Option<i64> {
    args.get("now").and_then(Value::as_str).map(|s| {
        pytime::fromisoformat(&s.replace('Z', "+00:00"))
            .unwrap()
            .micros
    })
}

fn compare(scenario: &str, index: usize, want: &Value, got: Result<Value, gih::HistoryError>) {
    match (want.get("result"), want.get("error")) {
        (Some(expected), None) => {
            let got = got.unwrap_or_else(|e| panic!("{scenario}[{index}]: {e}"));
            assert_eq!(&got, expected, "{scenario}[{index}] result mismatch");
        }
        (None, Some(error)) => {
            let err = got
                .err()
                .unwrap_or_else(|| panic!("{scenario}[{index}]: expected error, got Ok"));
            let class = error["class"].as_str().unwrap();
            assert_eq!(
                err.python_class(),
                class,
                "{scenario}[{index}] error class: {err}"
            );
            // `ValueError` messages are stable; driver/OS messages are not portable.
            if class == "ValueError" {
                assert_eq!(
                    err.to_string(),
                    error["message"].as_str().unwrap(),
                    "{scenario}[{index}] message"
                );
            }
        }
        _ => panic!("{scenario}[{index}]: malformed oracle step"),
    }
}

fn run_scenario(scenario: &Value) {
    let name = scenario["name"].as_str().unwrap();
    if scenario
        .get("unix_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && !cfg!(unix)
    {
        return;
    }
    let base: PathBuf = std::env::temp_dir().join(format!("gih-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let config_root = base.join("config");
    fs::create_dir_all(&config_root).unwrap();
    chmod(&config_root, 0o700);
    let real_project = base.join("project");
    fs::create_dir_all(&real_project).unwrap();
    chmod(&real_project, 0o700);
    let setup = &scenario["setup"];
    let manifest = real_project.join("project.json");
    if setup
        .get("manifest")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        if setup
            .get("manifest_symlink")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let target = base.join("manifest-real.json");
            fs::write(&target, "{}").unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &manifest).unwrap();
        } else {
            write_private(&manifest);
            fs::write(&manifest, "{}").unwrap();
        }
    }
    if setup
        .get("manifest_hardlink")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        fs::hard_link(&manifest, real_project.join("manifest-link.json")).unwrap();
    }
    let project = if setup
        .get("project_symlink")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let link = base.join("project-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_project, &link).unwrap();
        link
    } else {
        real_project.clone()
    };
    let quota_dir = config_root.join("spiderhead");
    if setup
        .get("quota_dir_symlink")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let target = base.join("quota-real");
        fs::create_dir_all(&target).unwrap();
        chmod(&target, 0o700);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &quota_dir).unwrap();
    } else if let Some(mode) = setup.get("quota_dir_mode").and_then(Value::as_str) {
        fs::create_dir_all(&quota_dir).unwrap();
        chmod(&quota_dir, u32::from_str_radix(mode, 8).unwrap());
    }

    let mut uuid_counter = 0u64;
    for (index, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
        let args = expand(&step["args"]);
        match step["op"].as_str().unwrap() {
            "seed_quota" => seed_quota(&config_root, &step["spec"]),
            "seed_history" => seed_history(&real_project, &step["spec"]),
            "reserve" => {
                uuid_counter += 1;
                let urls = args.get("urls").and_then(Value::as_array);
                let got = gih::reserve(
                    &config_root,
                    &project,
                    args["site_url"].as_str().unwrap(),
                    urls.map(Vec::as_slice),
                    parse_now(&args),
                    Some(&uuid_of(uuid_counter)),
                );
                compare(name, index, step, got);
            }
            "settle" => {
                let got = gih::settle(
                    &config_root,
                    &project,
                    args["reservation_id"].as_str().unwrap(),
                    args.get("outcomes")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice),
                    args.get("requests_made"),
                    parse_now(&args),
                );
                compare(name, index, step, got);
            }
            "latest" => {
                let got = gih::latest(
                    &project,
                    args["site_url"].as_str().unwrap(),
                    args.get("limit"),
                    args.get("offset"),
                    args.get("since").and_then(Value::as_str),
                );
                compare(name, index, step, got);
            }
            other => panic!("unknown op {other}"),
        }
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn oracle_scenarios_match_python() {
    let scenarios = common::load_fixture("gsc_inspection_history_oracle.json");
    for scenario in &scenarios {
        run_scenario(scenario);
    }
}
