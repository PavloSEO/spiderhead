//! Step dispatch for the scenario-replay engine (part 1: workspace, io, logs,
//! settings, bindings, facts, priorities, finding_views, coverage, pure helpers).

use serde_json::{json, Map, Value};
use spiderhead_projects as p;
use spiderhead_projects::error::{value, ProjectsResult};
use std::path::PathBuf;

use super::{Ctx, FixtureServer};

/// Mirror of `Ctx.path()` for string args that are literal paths.
pub(crate) fn path(ctx: &Ctx, args: &Value) -> PathBuf {
    ctx.path(&args["path"])
}

pub(crate) fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// `query` arg with reference `str`-type semantics: absent -> "", non-string -> error.
pub(crate) fn arg_query<'a>(args: &'a Value, message: &'static str) -> ProjectsResult<&'a str> {
    match args.get("query") {
        None => Ok(""),
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(value(message)),
    }
}

/// `args.get(key)` treating `null` as absent, like `dict.get` on the reference side.
pub(crate) fn optv<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| !v.is_null())
}

pub(crate) fn opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

pub(crate) fn i64_or(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

pub(crate) fn bool_or(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// Wrap a `ProjectsResult<()>` as `Ok(null)` for the outcome comparison.
pub(crate) fn unit(r: ProjectsResult<()>) -> ProjectsResult<Value> {
    r.map(|_| Value::Null)
}

fn write_file(ctx: &Ctx, args: &Value) -> ProjectsResult<Value> {
    let p_ = ctx.path(&args["path"]);
    if let Some(parent) = p_.parent() {
        std::fs::create_dir_all(parent).map_err(p::error::ProjectsError::Io)?;
    }
    let text = match args.get("json") {
        Some(v) => format!("{}\n", p::pyjson::dumps_indent2(v)),
        None => args["text"].as_str().unwrap_or_default().to_owned(),
    };
    std::fs::write(&p_, text).map_err(p::error::ProjectsError::Io)?;
    Ok(Value::Null)
}

/// `io::Error` with a filename suffix, the way `str(exc)` renders it in reference.
fn io_named(err: &std::io::Error, path: &std::path::Path) -> p::error::ProjectsError {
    match err.kind() {
        std::io::ErrorKind::NotFound => p::error::py_named(
            "FileNotFoundError",
            format!("[Errno 2] No such file or directory: '{}'", path.display()),
        ),
        _ => p::error::ProjectsError::Io(std::io::Error::new(err.kind(), err.to_string())),
    }
}

fn read_file(ctx: &Ctx, args: &Value) -> ProjectsResult<Value> {
    let p_ = ctx.path(&args["path"]);
    match std::fs::read_to_string(&p_) {
        Ok(text) => Ok(serde_json::from_str(&text).unwrap_or(Value::String(text))),
        Err(e) => Err(io_named(&e, &p_)),
    }
}

fn mkdir(ctx: &Ctx, args: &Value) -> ProjectsResult<Value> {
    std::fs::create_dir_all(ctx.path(&args["path"])).map_err(p::error::ProjectsError::Io)?;
    Ok(Value::Null)
}

fn tree(ctx: &Ctx, args: &Value) -> ProjectsResult<Value> {
    let base = match args.get("path") {
        Some(v) => ctx.path(v),
        None => ctx.scratch.clone(),
    };
    let mut out = Map::new();
    if base.is_dir() {
        tree_into(&base, &base, &mut out)?;
    }
    Ok(Value::Object(out))
}

fn tree_into(base: &PathBuf, dir: &PathBuf, out: &mut Map<String, Value>) -> ProjectsResult<()> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map_err(p::error::ProjectsError::Io)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    names.sort();
    for p_ in names {
        let rel = p_
            .strip_prefix(base)
            .unwrap_or(&p_)
            .to_string_lossy()
            .to_string();
        let meta = std::fs::symlink_metadata(&p_).map_err(p::error::ProjectsError::Io)?;
        if meta.file_type().is_symlink() {
            out.insert(
                rel,
                json!({"symlink": std::fs::read_link(&p_).unwrap_or_default().to_string_lossy()}),
            );
        } else if meta.is_dir() {
            out.insert(rel.clone(), json!({"dir": true}));
            tree_into(base, &p_, out)?;
        } else {
            let data = std::fs::read(&p_).map_err(p::error::ProjectsError::Io)?;
            match serde_json::from_slice::<Value>(&data) {
                Ok(v) => out.insert(rel, json!({"json": v})),
                Err(_) => match String::from_utf8(data.clone()) {
                    Ok(t) => out.insert(rel, json!({"text": t})),
                    Err(_) => out.insert(rel, json!({"bytes": data.len()})),
                },
            };
        }
    }
    Ok(())
}

/// Copy `oracle/raw-scans/<site>/scan.sqlite{,.audit-v2.sqlite}` into a directory.
fn copy_scan(ctx: &Ctx, args: &Value) -> ProjectsResult<Value> {
    let site = args["site"].as_str().unwrap_or_default();
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../oracle/raw-scans")
        .join(site);
    let dest = ctx.path(&args["path"]);
    std::fs::create_dir_all(&dest).map_err(p::error::ProjectsError::Io)?;
    let mut copied = Vec::new();
    for name in ["scan.sqlite", "scan.sqlite.audit-v2.sqlite"] {
        let from = src.join(name);
        if from.is_file() {
            std::fs::copy(&from, dest.join(name)).map_err(p::error::ProjectsError::Io)?;
            copied.push(name);
        }
    }
    Ok(json!(copied))
}

fn fixture_server(ctx: &mut Ctx, args: &Value) -> ProjectsResult<Value> {
    let server = FixtureServer::start(args["site"].as_str().unwrap_or_default());
    let base = server.base.clone();
    ctx.servers.push(server);
    Ok(json!(base))
}

fn urlsplit(args: &Value) -> ProjectsResult<Value> {
    let input = args["input"].as_str().unwrap_or_default();
    let r = p::urls::urlsplit(input)?;
    let mut out = Map::new();
    out.insert("scheme".into(), json!(r.scheme));
    out.insert("netloc".into(), json!(r.netloc));
    out.insert("path".into(), json!(r.path));
    out.insert("query".into(), json!(r.query));
    out.insert("fragment".into(), json!(r.fragment));
    out.insert(
        "username".into(),
        r.username().map_or(Value::Null, |u| json!(u)),
    );
    out.insert(
        "password".into(),
        r.password().map_or(Value::Null, |u| json!(u)),
    );
    out.insert(
        "hostname".into(),
        r.hostname().map_or(Value::Null, |u| json!(u)),
    );
    match r.port() {
        Ok(port) => out.insert("port".into(), port.map_or(Value::Null, |p_| json!(p_))),
        Err(e) => out.insert("port".into(), json!({"raised": e.to_string()})),
    };
    Ok(Value::Object(out))
}

fn fromisoformat(args: &Value) -> ProjectsResult<Value> {
    let text = args["text"].as_str().unwrap_or_default();
    let dt = p::pytime::fromisoformat(text)?;
    Ok(json!({
        "civil": dt.civil.to_string(),
        "offset_seconds": dt.offset_seconds,
    }))
}

fn epoch_z(args: &Value) -> ProjectsResult<Value> {
    let seconds = args["seconds"].as_f64().unwrap_or_default();
    Ok(json!(p::pytime::epoch_z(seconds)))
}

/// Canned detection tools for `facts.project_facts`.
struct FixtureTools {
    robots_check: Value,
    tech_detect: Value,
}

impl p::facts::DetectTools for FixtureTools {
    fn robots_check(&self, _url: &str, _paths: &[String]) -> Value {
        self.robots_check.clone()
    }

    fn tech_detect(&self, _url: &str) -> Value {
        self.tech_detect.clone()
    }
}

fn project_facts(ctx: &Ctx, args: &Value) -> ProjectsResult<Value> {
    let tools_value = optv(args, "tools");
    let fixture = tools_value.map(|t| FixtureTools {
        robots_check: t["robots_check"].clone(),
        tech_detect: t["tech_detect"].clone(),
    });
    p::facts::project_facts(
        &path(ctx, args),
        optv(args, "facts"),
        bool_or(args, "detect", false),
        bool_or(args, "apply", false),
        fixture.as_ref().map(|f| f as &dyn p::facts::DetectTools),
    )
}

/// Parse an ISO-8601 string into a `Zoned` at its own fixed offset.
pub(crate) fn py_zoned(args: &Value, key: &str) -> ProjectsResult<jiff::Zoned> {
    let text = args[key].as_str().unwrap_or_default();
    let parsed = p::pytime::fromisoformat(text)?;
    let offset = parsed
        .offset_seconds
        .ok_or_else(|| value("datetime must be timezone-aware"))?;
    let tz = jiff::tz::TimeZone::fixed(
        jiff::tz::Offset::from_seconds(offset as i32).map_err(|e| value(e.to_string()))?,
    );
    parsed.civil.to_zoned(tz).map_err(|e| value(e.to_string()))
}

fn coverage_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "initialize" => p::coverage::initialize_coverage_full(
            &dir,
            optv(args, "template"),
            opt_i64(args, "expected_revision"),
            optv(args, "plan"),
        ),
        "ensure_goal_task" => unit(p::coverage::ensure_goal_task(
            &dir,
            args["item_id"].as_str().unwrap_or_default(),
            args["title"].as_str().unwrap_or_default(),
            opt_str(args, "description"),
        )),
        "update_item" => p::coverage::update_item(
            &dir,
            &args["item"],
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "append_task_comment" => p::coverage::append_task_comment(
            &dir,
            args["item_id"].as_str().unwrap_or_default(),
            args["text"].as_str().unwrap_or_default(),
            args["actor"].as_str().unwrap_or("agent"),
            args["expected_revision"].as_i64().unwrap_or_default(),
            optv(args, "reference"),
        ),
        "record_execution" => p::coverage::record_execution(
            &dir,
            args["item_id"].as_str().unwrap_or_default(),
            &args["record"],
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "status" => p::coverage::coverage_status(&dir, bool_or(args, "verify_evidence", true)),
        _ => Err(value(format!("unknown coverage op {tail}"))),
    }
}

fn finding_views_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "list_views" => p::finding_views::list_views(&dir),
        "show_view" => p::finding_views::show_view(&dir, args["name"].as_str().unwrap_or_default()),
        "save_view" => p::finding_views::save_view(
            &dir,
            &args["view"],
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "delete_view" => p::finding_views::delete_view(
            &dir,
            args["name"].as_str().unwrap_or_default(),
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "rename_view" => p::finding_views::rename_view(
            &dir,
            args["name"].as_str().unwrap_or_default(),
            args["new_name"].as_str().unwrap_or_default(),
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "apply_view_to_audit" => p::finding_views::apply_view_to_audit(
            &dir,
            args["name"].as_str().unwrap_or_default(),
            &args["document"],
            i64_or(args, "offset", 0),
        ),
        _ => Err(value(format!("unknown finding_views op {tail}"))),
    }
}

fn settings_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "read_settings" => p::project_settings::read_settings(&dir),
        "update_label" => p::project_settings::update_label(
            &dir,
            args["label"].as_str().unwrap_or_default(),
            args["expected_revision"].as_i64().unwrap_or_default(),
            args["client"].as_str().unwrap_or("cli"),
            opt_str(args, "operation_id"),
            bool_or(args, "log_action", true),
        ),
        "propose_settings" => p::project_settings::propose_settings(
            &dir,
            &args["changes"],
            args["client"].as_str().unwrap_or("mcp"),
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "list_proposals" => p::project_settings::list_proposals(
            &dir,
            i64_or(args, "offset", 0),
            i64_or(args, "limit", 50),
        ),
        "review_proposal" => p::project_settings::review_proposal(
            &dir,
            args["proposal_id"].as_str().unwrap_or_default(),
            args["decision"].as_str().unwrap_or_default(),
            args["expected_revision"].as_i64().unwrap_or_default(),
            args["client"].as_str().unwrap_or("cli"),
        ),
        "delete_project" => {
            p::project_settings::delete_project(&dir, args["client"].as_str().unwrap_or("cli"))
        }
        _ => Err(value(format!("unknown project_settings op {tail}"))),
    }
}

fn inbox_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "submit" => p::inbox::submit(
            &dir,
            args["text"].as_str().unwrap_or_default(),
            args["kind"].as_str().unwrap_or("note"),
            optv(args, "references"),
            args["author_role"].as_str().unwrap_or("specialist"),
            opt_i64(args, "expected_revision"),
        ),
        "list_entries" => p::inbox::list_entries(
            &dir,
            args["consumer"].as_str().unwrap_or_default(),
            i64_or(args, "offset", 0),
            i64_or(args, "limit", 20),
            bool_or(args, "include_acknowledged", true),
        ),
        "mark_read" => p::inbox::mark_read(
            &dir,
            args["consumer"].as_str().unwrap_or_default(),
            &args["entry_ids"],
            opt_i64(args, "expected_revision"),
        ),
        "acknowledge" => p::inbox::acknowledge(
            &dir,
            args["consumer"].as_str().unwrap_or_default(),
            &args["entry_ids"],
            opt_i64(args, "expected_revision"),
        ),
        "set_goal_state" => p::inbox::set_goal_state(
            &dir,
            args["entry_id"].as_str().unwrap_or_default(),
            args["state"].as_str().unwrap_or_default(),
            opt_i64(args, "expected_revision"),
        ),
        "triage" => p::inbox::triage(
            &dir,
            args["entry_id"].as_str().unwrap_or_default(),
            &args["outcome"],
            args["actor"].as_str().unwrap_or("agent"),
            opt_i64(args, "expected_revision"),
        ),
        "unread_summary" => p::inbox::unread_summary(
            &dir,
            args["consumer"].as_str().unwrap_or_default(),
            i64_or(args, "limit", 10),
        ),
        "unread_goal_tasks" => p::inbox::unread_goal_tasks(
            &dir,
            args["consumer"].as_str().unwrap_or_default(),
            i64_or(args, "limit", 10),
        ),
        "fingerprint" => p::inbox::fingerprint(&dir).map(Value::from),
        _ => Err(value(format!("unknown inbox op {tail}"))),
    }
}

fn io_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "load_document" => {
            let default_doc = args.get("default").cloned();
            let (root, project, doc) =
                p::io::load_document(&dir, args["name"].as_str().unwrap_or_default(), &|_uuid| {
                    default_doc.clone().unwrap_or(Value::Null)
                })?;
            Ok(json!({"root": root.to_string_lossy(), "project": project, "document": doc}))
        }
        "check_owner" => unit(p::io::check_owner(
            &args["document"],
            args["project_uuid"].as_str().unwrap_or_default(),
            args["fmt"].as_str().unwrap_or_default(),
            args["label"].as_str().unwrap_or_default(),
        )),
        "save_revision" => unit(p::io::save_revision(
            &dir,
            args["name"].as_str().unwrap_or_default(),
            &mut args["document"].clone(),
        )),
        "save_checked" => unit(p::io::save_checked(
            &dir,
            args["name"].as_str().unwrap_or_default(),
            &mut args["document"].clone(),
            args["expected_revision"].as_i64().unwrap_or_default(),
            args["label"].as_str().unwrap_or_default(),
        )),
        _ => Err(value(format!("unknown io op {tail}"))),
    }
}

fn workspace_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    match tail {
        "directory" => p::workspace::directory(
            &ctx.path(&args["path"]),
            args["label"].as_str().unwrap_or("directory"),
        )
        .map(|p_| json!(p_.to_string_lossy())),
        "target" => {
            p::workspace::target(args["value"].as_str().unwrap_or_default()).map(Value::from)
        }
        "references" => p::workspace::references(
            optv(args, "value"),
            args["label"].as_str().unwrap_or("references"),
        )
        .map(Value::from),
        "facts" => p::workspace::facts(optv(args, "value")).map(Value::from),
        "create_project" => p::workspace::create_project(
            &ctx.path(&args["path"]),
            args["target"].as_str().unwrap_or_default(),
            opt_str(args, "label"),
            optv(args, "facts"),
            optv(args, "template_references"),
            optv(args, "profile_references"),
        ),
        "open_project" => {
            p::workspace::open_project(&ctx.path(&args["path"]), opt_str(args, "expected_site"))
        }
        "load" => {
            let (root, doc) = p::workspace::load(&ctx.path(&args["path"]))?;
            Ok(json!({"root": root.to_string_lossy(), "document": doc}))
        }
        "project_status" => p::workspace::project_status(
            &ctx.path(&args["path"]),
            None,
            bool_or(args, "verify_evidence", true),
        ),
        _ => Err(value(format!("unknown workspace op {tail}"))),
    }
}

/// Route `op` to the crate function; unknown ops error like the reference runner.
pub fn dispatch(ctx: &mut Ctx, op: &str, args: &Value) -> ProjectsResult<Value> {
    let Some((module, tail)) = op.split_once('.') else {
        return Err(value(format!("unknown op {op}")));
    };
    match (module, tail) {
        ("file", "write") => write_file(ctx, args),
        ("file", "read") => read_file(ctx, args),
        ("file", "mkdir") => mkdir(ctx, args),
        ("file", "tree") => tree(ctx, args),
        ("file", "copy_scan") => copy_scan(ctx, args),
        ("fixture", "server") => fixture_server(ctx, args),
        ("urls", "urlsplit") => urlsplit(args),
        ("urls", "urlunsplit") => Ok(json!(p::urls::urlunsplit(
            args["scheme"].as_str().unwrap_or_default(),
            args["netloc"].as_str().unwrap_or_default(),
            args["path"].as_str().unwrap_or_default(),
            args["query"].as_str().unwrap_or_default(),
            args["fragment"].as_str().unwrap_or_default(),
        ))),
        ("urls", "urldefrag") => {
            let (base, frag) = p::urls::urldefrag(args["input"].as_str().unwrap_or_default());
            Ok(json!([base, frag]))
        }
        ("pytime", "fromisoformat") => fromisoformat(args),
        ("pytime", "epoch_z") => epoch_z(args),
        ("permissions", "require_mcp_allowed") => unit(p::permissions::require_mcp_allowed(
            args["action"].as_str().unwrap_or_default(),
        )),
        ("workspace", _) => workspace_dispatch(ctx, tail, args),
        ("io", _) => io_dispatch(ctx, tail, args),
        ("event_log", "append") => p::event_log::append(
            &path(ctx, args),
            args["source"].as_str().unwrap_or_default(),
            args["actor"].as_str().unwrap_or_default(),
            args["text"].as_str().unwrap_or_default(),
        ),
        ("event_log", "page") => p::event_log::page(
            &path(ctx, args),
            i64_or(args, "offset", 0),
            i64_or(args, "limit", 50),
            opt_str(args, "source"),
            arg_query(args, "query must be at most 200 characters")?,
            opt_i64(args, "since_revision"),
            bool_or(args, "follow", false),
            i64_or(args, "wait_seconds", 0),
        ),
        ("journal", "record") => p::journal::record(
            &path(ctx, args),
            args["source"].as_str().unwrap_or_default(),
            args["actor"].as_str().unwrap_or_default(),
            args["text"].as_str().unwrap_or_default(),
        ),
        ("journal", "note") => {
            p::journal::note(
                &path(ctx, args),
                args["source"].as_str().unwrap_or_default(),
                args["actor"].as_str().unwrap_or_default(),
                args["text"].as_str().unwrap_or_default(),
            );
            Ok(Value::Null)
        }
        ("action_log", "record") => p::action_log::record(
            &path(ctx, args),
            args["client"].as_str().unwrap_or_default(),
            args["action"].as_str().unwrap_or_default(),
            args["outcome"].as_str().unwrap_or_default(),
            args["detail"].as_str().unwrap_or_default(),
            opt_str(args, "proposal_id"),
            opt_str(args, "operation_id"),
        ),
        ("action_log", "resolve_pending") => p::action_log::resolve_pending(
            &path(ctx, args),
            args["sequence"].as_i64().unwrap_or_default(),
            args["outcome"].as_str().unwrap_or_default(),
            args["detail"].as_str().unwrap_or_default(),
            opt_str(args, "proposal_id"),
            opt_str(args, "operation_id"),
        ),
        ("action_log", "page") => p::action_log::page(
            &path(ctx, args),
            i64_or(args, "offset", 0),
            i64_or(args, "limit", 50),
            opt_str(args, "client"),
            opt_str(args, "outcome"),
            arg_query(args, "query must be bounded text")?,
        ),
        ("action_log", "latest_agent_activity") => {
            p::action_log::latest_agent_activity(&path(ctx, args))
        }
        ("progress", "project_progress") => p::progress::project_progress(
            &path(ctx, args),
            i64_or(args, "limit", 20),
            i64_or(args, "offset", 0),
            optv(args, "status").cloned(),
            None,
        ),
        ("project_settings", _) => settings_dispatch(ctx, tail, args),
        ("bindings", "list_bindings") => {
            p::bindings::list_bindings(&path(ctx, args), opt_str(args, "as_of"))
        }
        ("bindings", "link") => p::bindings::link(
            &path(ctx, args),
            args["service"].as_str().unwrap_or_default(),
            args["resource"].as_str().unwrap_or_default(),
            opt_str(args, "label"),
        ),
        ("bindings", "unlink") => p::bindings::unlink(
            &path(ctx, args),
            args["service"].as_str().unwrap_or_default(),
            args["resource"].as_str().unwrap_or_default(),
        ),
        ("facts", "project_facts") => project_facts(ctx, args),
        ("priorities", "project_priorities") => p::priorities::project_priorities(
            &path(ctx, args),
            optv(args, "policy"),
            bool_or(args, "apply", false),
            opt_i64(args, "expected_revision"),
            bool_or(args, "full", false),
        ),
        ("status_measurement", "read") => {
            p::status_measurement::read_storage_measurement(&path(ctx, args))
        }
        ("status_measurement", "refresh") => {
            p::status_measurement::refresh_storage_measurement(&path(ctx, args))
        }
        ("finding_views", "validate_definition") => {
            p::finding_views::validate_definition(&args["value"])
        }
        ("finding_views", _) => finding_views_dispatch(ctx, tail, args),
        ("coverage", _) => coverage_dispatch(ctx, tail, args),
        ("inbox", "goal_task_id") => Ok(json!(p::inbox::goal_task_id(
            args["entry_id"].as_str().unwrap_or_default()
        ))),
        ("inbox", _) => inbox_dispatch(ctx, tail, args),
        ("schedule", "default_settings") => Ok(p::schedule::default_settings(
            args["project_uuid"].as_str().unwrap_or_default(),
        )),
        ("schedule", "validate") => p::schedule::validate(&args["settings"]),
        ("schedule", "due_state") => {
            let now = py_zoned(args, "now")?;
            p::schedule::due_state(
                &args["settings"],
                args["host"].as_str().unwrap_or_default(),
                &now,
                opt_str(args, "last_window"),
            )
        }
        ("schedule", "next_due") => {
            let now = py_zoned(args, "now")?;
            p::schedule::next_due(
                &args["settings"],
                args["host"].as_str().unwrap_or_default(),
                &now,
            )
            .map(|v| v.map_or(Value::Null, Value::from))
        }
        ("schedule", "show") => {
            let now = args
                .get("now")
                .filter(|v| !v.is_null())
                .map(|_| py_zoned(args, "now"))
                .transpose()?;
            p::schedule::show(&path(ctx, args), now.as_ref())
        }
        ("schedule", "configure") => {
            let empty = Map::new();
            let fields = args["fields"].as_object().unwrap_or(&empty);
            p::schedule::configure(
                &path(ctx, args),
                args["expected_revision"].as_i64().unwrap_or_default(),
                optv(args, "settings"),
                fields,
            )
        }
        ("schedule", "active") => p::schedule::active(&path(ctx, args)).map(Value::from),
        ("schedule", "infer_time_zone") => {
            let (zone, source) =
                p::schedule::infer_time_zone(args["host"].as_str().unwrap_or_default());
            Ok(json!([zone, source]))
        }
        _ => super::dispatch2::dispatch2(ctx, op, args),
    }
}
