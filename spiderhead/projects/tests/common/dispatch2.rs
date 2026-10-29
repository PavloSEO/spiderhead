//! Step dispatch part 2: runtime, archive, exports, evidence, catalogue,
//! competitor_scans, execution, run_observation, run_control, schedule_install,
//! monitoring, observer, service_delivery, projection, pyjson, evidence_cache.

use serde_json::{json, Value};
use spiderhead_projects as p;
use spiderhead_projects::error::{value, ProjectsResult};
use std::collections::HashMap;
use std::path::PathBuf;

use super::dispatch::{i64_or, opt_i64, opt_str, optv, path, unit};
use super::Ctx;

fn runtime_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "read_document" => {
            p::runtime::read_document(&dir, args["name"].as_str().unwrap_or_default())
                .map(|v| v.unwrap_or(Value::Null))
        }
        "write_document" => unit(p::runtime::write_document(
            &dir,
            args["name"].as_str().unwrap_or_default(),
            &args["data"],
            opt_i64(args, "expected_revision"),
        )),
        "validate_policy" => p::runtime::validate_policy(&args["policy"]),
        "project_policy" => p::runtime::project_policy(
            &dir,
            optv(args, "policy"),
            args["apply"].as_bool().unwrap_or(false),
            opt_i64(args, "expected_revision"),
            opt_str(args, "operation_id"),
        ),
        "admission" => p::runtime::admission(
            &dir,
            &args["settings"],
            args["approved"].as_bool().unwrap_or(false),
        ),
        "playbook_outline" => Ok(p::playbooks::playbook_outline(
            args["content"].as_str().unwrap_or_default(),
        )),
        "playbook_list" => p::playbooks::playbook_list(opt_str(args, "kind")),
        "playbook_show" => p::playbooks::playbook_show(
            args["name"].as_str().unwrap_or_default(),
            args["kind"].as_str().unwrap_or("skill"),
        ),
        "preparation_status" => p::runtime::preparation_status(&dir),
        "add_competitors" => p::runtime::add_competitors(&dir, &args["competitors"]),
        "list_competitors" => p::runtime::list_competitors(&dir),
        "remove_competitor" => {
            p::runtime::remove_competitor(&dir, args["url"].as_str().unwrap_or_default())
        }
        "latest_project_scan" => {
            p::runtime::latest_project_scan(&dir).map(|p_| json!(p_.to_string_lossy()))
        }
        "aggregate_coverage" => p::runtime::aggregate_coverage(
            &dir.to_string_lossy(),
            args["primary"].clone(),
            None,
            args["verify_evidence"].as_bool().unwrap_or(true),
        ),
        "resolve_item_scope" => p::runtime::resolve_item_scope(
            &dir,
            args["item_id"].as_str().unwrap_or_default(),
            args["verify_evidence"].as_bool().unwrap_or(true),
        )
        .map(|(a, b)| json!([a, b])),
        "prepare_project" => {
            let spec = args.get("tool").cloned().unwrap_or_else(|| json!({}));
            let scan = spec["scan"]
                .as_str()
                .map(|s| ctx.path(&json!(s)).to_string_lossy().to_string());
            let kind = spec["kind"].as_str().unwrap_or("scan").to_owned();
            let error = spec["error"].as_str().unwrap_or("tool failed").to_owned();
            let tool = move |_project: &str,
                             _overrides: &Value,
                             _approve: bool,
                             _build: Option<&str>|
                  -> Result<Value, String> {
                match kind.as_str() {
                    "raise" => Err(error.clone()),
                    "invalid" => Ok(json!("not a dict")),
                    "error" => Ok(json!({"ok": false, "error": error})),
                    _ => {
                        let mut out = json!({"ok": true});
                        if let Some(scan) = &scan {
                            out["scan"] = json!(scan);
                        }
                        Ok(out)
                    }
                }
            };
            p::runtime::prepare_project(
                &dir,
                &tool,
                optv(args, "template"),
                optv(args, "competitors"),
                args["approve_large_crawl"].as_bool().unwrap_or(false),
                opt_str(args, "producer_build"),
                opt_i64(args, "max_urls"),
            )
        }
        _ => Err(value(format!("unknown runtime op {tail}"))),
    }
}

fn archive_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    match tail {
        "archive_project" => match optv(args, "out") {
            None => Err(p::error::py_named(
                "TypeError",
                "argument should be a str or an os.PathLike object where __fspath__ \
                 returns a str, not 'NoneType'",
            )),
            Some(_) => {
                let exclude_bodies = match args.get("exclude_bodies") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(value("exclude_bodies must be a boolean")),
                };
                p::archive::archive_project(
                    &path(ctx, args),
                    &ctx.path(&args["out"]),
                    args["dry_run"].as_bool().unwrap_or(false),
                    exclude_bodies,
                    opt_i64(args, "exclude_bodies_older_than_days"),
                    opt_i64(args, "keep_newest_scans"),
                )
            }
        },
        "archive_list" => p::archive::archive_list(&ctx.path(&args["archive"])),
        "restore_project" => {
            p::archive::restore_project(&ctx.path(&args["archive"]), &ctx.path(&args["to"]))
        }
        _ => Err(value(format!("unknown archive op {tail}"))),
    }
}

fn exports_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "has_active_jobs" => p::exports::has_active_jobs(&dir).map(Value::from),
        "record_export" => p::exports::record_export(
            &dir,
            args["dataset"].as_str().unwrap_or_default(),
            args["format"].as_str().unwrap_or_default(),
            args["export_path"].as_str().unwrap_or_default(),
            i64_or(args, "row_count", 0),
            i64_or(args, "byte_count", 0),
            opt_str(args, "sha256"),
            optv(args, "source"),
            opt_str(args, "job_id"),
        ),
        "exports_page" => {
            p::exports::exports_page(&dir, i64_or(args, "offset", 0), i64_or(args, "limit", 50))
        }
        "export_status" => {
            p::exports::export_status(&dir, args["job_id"].as_str().unwrap_or_default())
        }
        "cancel_export" => {
            p::exports::cancel_export(&dir, args["job_id"].as_str().unwrap_or_default())
        }
        "start_export" => p::exports::start_export(
            &dir,
            args["dataset"].as_str().unwrap_or_default(),
            args["format"].as_str().unwrap_or_default(),
            args["output"].as_str().unwrap_or_default(),
            opt_str(args, "compare_manifest"),
            opt_str(args, "job_id"),
            args["wait"].as_bool().unwrap_or(false),
        ),
        _ => Err(value(format!("unknown exports op {tail}"))),
    }
}

fn evidence_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "artifact_path" => {
            p::evidence::artifact_path(&dir, &args["value"]).map(|p_| json!(p_.to_string_lossy()))
        }
        "artifact_identity" => {
            p::evidence::artifact_identity(&ctx.path(&args["file"])).map(|id| json!(id))
        }
        "validate_record" => {
            p::evidence::validate_record(&dir, &args["definition"], &args["record"])
        }
        "evidence_stale" => {
            let catalogue = match args.get("catalogue") {
                Some(Value::Object(m)) => m.clone(),
                _ => p::catalogue::load_catalogue()?,
            };
            let mut digests: HashMap<(String, [i64; 5]), String> = HashMap::new();
            Ok(json!(p::evidence::evidence_stale(
                &dir,
                &args["record"],
                &catalogue,
                &mut digests,
                args["verify_bytes"].as_bool().unwrap_or(true),
            )))
        }
        _ => Err(value(format!("unknown evidence op {tail}"))),
    }
}

fn execution_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "start" => p::execution::start(
            &dir,
            &args["scenario_id"],
            &args["steps"],
            i64_or(args, "expected_revision", 0),
            &args["context"],
        ),
        "checkpoint" => p::execution::checkpoint(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            args["step_id"].as_str().unwrap_or_default(),
            args["state"].as_str().unwrap_or_default(),
            &args["evidence"],
            args["expected_revision"].as_i64().unwrap_or_default(),
            &args["review"],
            opt_str(args, "phase"),
        ),
        "resume" => p::execution::resume(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "status" => p::execution::status(
            &dir,
            optv(args, "coverage"),
            args["verify_evidence"].as_bool().unwrap_or(true),
        ),
        "execute" => p::execution::execute(
            &dir,
            &args["scenario_id"],
            &args["steps"],
            &args["outcomes"],
            &args["context"],
        ),
        _ => Err(value(format!("unknown execution op {tail}"))),
    }
}

fn run_observation_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "live_holder" => {
            p::run_observation::live_holder(&dir).map(|v| v.map_or(Value::Null, Value::from))
        }
        "start" => p::run_observation::start(
            &dir,
            args["kind"].as_str().unwrap_or_default(),
            args["mode"].as_str().unwrap_or_default(),
            i64_or(args, "max_urls", 0),
            i64_or(args, "max_requests", 0),
            i64_or(args, "max_crawl_seconds", 0),
            args["max_requests_per_second"].as_f64(),
            args["config_fingerprint"].as_str().unwrap_or_default(),
            opt_str(args, "artifact"),
            args["resumed"].as_bool().unwrap_or(false),
            opt_str(args, "origin"),
            args["aggregate_max_requests_per_second"].as_f64(),
            opt_str(args, "run_id"),
            args["counters"].as_object(),
        ),
        "phase" => unit(p::run_observation::phase(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            args["name"].as_str().unwrap_or_default(),
            args["code"].as_str().unwrap_or("entered"),
        )),
        "collector_started" => unit(p::run_observation::collector_started(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            i64_or(args, "pid", 0),
        )),
        "progress" => unit(p::run_observation::progress(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            opt_i64(args, "fetched"),
            opt_i64(args, "queued"),
            opt_i64(args, "inflight"),
            opt_i64(args, "excluded"),
            args["rate_per_second"].as_f64(),
            args["rate_window_seconds"].as_f64(),
            args["queue_semantics"].as_str().unwrap_or("separate"),
            args["source"].as_str().unwrap_or("collector"),
        )),
        "finish" => unit(p::run_observation::finish(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            args["state"].as_str().unwrap_or_default(),
            args["reason"].as_str().unwrap_or_default(),
            args["counters"].as_object(),
        )),
        "finish_sitemap" => unit(p::run_observation::finish_sitemap(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            &args["result"],
        )),
        "status" => p::run_observation::status_page(
            &dir,
            i64_or(args, "limit", 20),
            i64_or(args, "offset", 0),
        ),
        _ => Err(value(format!("unknown run_observation op {tail}"))),
    }
}

fn run_control_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "checked_rate" => {
            let rate = args["rate"].as_f64().unwrap_or(f64::NAN);
            p::run_control::checked_rate(rate, opt_i64(args, "concurrency"))
                .map(|(r, c)| json!([r, c]))
        }
        "request_rate" => p::run_control::request_rate(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            args["rate"].as_f64().unwrap_or_default(),
            opt_i64(args, "concurrency"),
        ),
        "consume_rate" => {
            p::run_control::consume_rate(&dir, args["run_id"].as_str().unwrap_or_default())
                .map(|v| v.map_or(Value::Null, |(r, c)| json!([r, c])))
        }
        "checked_operator_urls" => {
            p::run_control::checked_operator_urls(&args["urls"]).map(Value::from)
        }
        "request_scope_narrow" => p::run_control::request_scope_narrow(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            opt_str(args, "exclude"),
            opt_str(args, "include"),
        ),
        "consume_scope" => {
            p::run_control::consume_scope(&dir, args["run_id"].as_str().unwrap_or_default())
                .map(|v| v.unwrap_or(Value::Null))
        }
        "request_urls" => p::run_control::request_urls(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            &args["urls"],
        ),
        "consume_urls" => {
            p::run_control::consume_urls(&dir, args["run_id"].as_str().unwrap_or_default())
                .map(|v| v.map_or(Value::Null, Value::from))
        }
        "request" => p::run_control::request(
            &dir,
            args["run_id"].as_str().unwrap_or_default(),
            args["action"].as_str().unwrap_or_default(),
        ),
        "consume" => p::run_control::consume(&dir, args["run_id"].as_str().unwrap_or_default())
            .map(|v| v.map_or(Value::Null, Value::from)),
        _ => Err(value(format!("unknown run_control op {tail}"))),
    }
}

fn monitoring_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "configure" => {
            p::monitoring::configure(&dir, &args["policy"], i64_or(args, "expected_revision", 0))
        }
        "run" => p::monitoring::run(
            &dir,
            args["scan_id"].as_str().unwrap_or_default(),
            &args["observations"],
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "schedule" => p::monitoring::schedule(
            &dir,
            args["action"].as_str().unwrap_or_default(),
            args["expected_revision"].as_i64().unwrap_or_default(),
            opt_i64(args, "lease_seconds"),
        ),
        "collect_once" => {
            let stamp = args["now"].as_str().unwrap_or_default().to_owned();
            let now = move || stamp.clone();
            p::monitoring::collect_once(
                &dir,
                args["expected_revision"].as_i64().unwrap_or_default(),
                args["apply"].as_bool().unwrap_or(false),
                if args.get("now").is_some_and(|v| !v.is_null()) {
                    Some(&now)
                } else {
                    None
                },
                None,
            )
        }
        "local_deliver" => p::monitoring::local_deliver(
            &dir,
            args["scan_id"].as_str().unwrap_or_default(),
            args["expected_revision"].as_i64().unwrap_or_default(),
        ),
        "status" => p::monitoring::status(&dir),
        _ => Err(value(format!("unknown monitoring op {tail}"))),
    }
}

fn observer_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "findings_page" => p::observer::findings_page(
            &dir,
            opt_str(args, "scan_uuid"),
            i64_or(args, "offset", 0),
            i64_or(args, "limit", 50),
            args["query"].as_str().unwrap_or_default(),
            opt_str(args, "url"),
            args["sort"].as_str().unwrap_or("severity"),
            args["descending"].as_bool().unwrap_or(false),
        ),
        "finding_detail" => p::observer::finding_detail(
            &dir,
            opt_str(args, "scan_uuid"),
            args["ordinal"].as_i64().unwrap_or_default(),
        ),
        "saved_view_page" => p::observer::saved_view_page(
            &dir,
            args["name"].as_str().unwrap_or_default(),
            opt_str(args, "scan_uuid"),
            i64_or(args, "offset", 0),
        ),
        "observe" => p::observer::observe(
            &dir,
            opt_str(args, "consumer"),
            i64_or(args, "scan_limit", 20),
            i64_or(args, "run_offset", 0),
            i64_or(args, "run_limit", 20),
            opt_str(args, "since_revision"),
        ),
        "checklist_page" => {
            let states: Option<Vec<String>> = args["states"].as_array().map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            });
            p::observer::checklist_page(
                &dir,
                i64_or(args, "offset", 0),
                i64_or(args, "limit", 50),
                args["query"].as_str().unwrap_or_default(),
                opt_str(args, "kind"),
                opt_str(args, "state"),
                args["sort"].as_str().unwrap_or("id"),
                args["descending"].as_bool().unwrap_or(false),
                states.as_deref(),
            )
        }
        "task_detail" => {
            p::observer::task_detail(&dir, args["item_id"].as_str().unwrap_or_default())
        }
        "checklist_detail" => {
            p::observer::checklist_detail(&dir, args["item_id"].as_str().unwrap_or_default())
        }
        "scans_page" => p::observer::scans_page(
            &dir,
            i64_or(args, "offset", 0),
            i64_or(args, "limit", 20),
            args["full"].as_bool().unwrap_or(false),
        ),
        "observe_activity" => p::observer::observe_activity(&dir, opt_str(args, "since_revision")),
        _ => Err(value(format!("unknown observer op {tail}"))),
    }
}

fn receipts_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    let receipts = p::service_delivery::DeliveryReceipts::new(&dir.join("receipts.sqlite"))?;
    match tail {
        "reserve" => receipts
            .reserve(
                args["job_id"].as_str().unwrap_or_default(),
                args["artifact_id"].as_str().unwrap_or_default(),
                args["destination"].as_str().unwrap_or_default(),
            )
            .map(|(r, s)| json!([r, s])),
        "uncertain" => unit(receipts.uncertain(
            args["job_id"].as_str().unwrap_or_default(),
            args["artifact_id"].as_str().unwrap_or_default(),
            args["destination"].as_str().unwrap_or_default(),
            args["receipt"].as_str().unwrap_or_default(),
        )),
        "reconcile" => receipts.reconcile(
            args["receipt"].as_str().unwrap_or_default(),
            args["provider_status"].as_str().unwrap_or_default(),
            args["sender_stopped"].as_bool().unwrap_or(false),
        ),
        "delivered" => unit(receipts.delivered(
            args["job_id"].as_str().unwrap_or_default(),
            args["artifact_id"].as_str().unwrap_or_default(),
            args["destination"].as_str().unwrap_or_default(),
            args["receipt"].as_str().unwrap_or_default(),
        )),
        _ => Err(value(format!("unknown receipts op {tail}"))),
    }
}

fn schedule_install_dispatch(ctx: &Ctx, tail: &str, args: &Value) -> ProjectsResult<Value> {
    let dir = path(ctx, args);
    match tail {
        "install" => p::schedule_install::install(
            &dir.to_string_lossy(),
            args["kind"].as_str().unwrap_or_default(),
            args.get("dest").map(|v| ctx.path(v)).as_deref(),
            opt_str(args, "python"),
        ),
        "uninstall" => p::schedule_install::uninstall(
            &dir.to_string_lossy(),
            args["kind"].as_str().unwrap_or_default(),
            args.get("dest").map(|v| ctx.path(v)).as_deref(),
        ),
        _ => Err(value(format!("unknown schedule_install op {tail}"))),
    }
}

fn projection_dispatch(ctx: &Ctx, args: &Value, tail: &str) -> ProjectsResult<Value> {
    match tail {
        "method_coverage" => Ok(p::projection::method_coverage(&args["checklist"])),
        "coverage_summary" => Ok(p::projection::coverage_summary(&args["checklist"])),
        "bounded_value" => Ok(p::projection::bounded_value(
            &args["item"],
            i64_or(args, "depth", 0) as usize,
        )),
        "pagination" => p::projection::pagination(
            i64_or(args, "total", 0),
            i64_or(args, "offset", 0),
            i64_or(args, "limit", 50),
        ),
        "projection_truncated" => Ok(json!(p::projection::projection_truncated(
            &args["item"],
            i64_or(args, "depth", 0) as usize
        ))),
        "legacy_groups" => {
            let mut row = args["row"]
                .as_object()
                .cloned()
                .ok_or_else(|| value("row must be an object"))?;
            match row.get("path").cloned() {
                None => Err(p::error::py_named("KeyError", "'path'")),
                Some(p) => {
                    row.insert("path".to_owned(), json!(ctx.path(&p).to_string_lossy()));
                    Ok(json!(p::projection::legacy_groups(&row)))
                }
            }
        }
        _ => Err(value(format!("unknown projection op {tail}"))),
    }
}

fn pyjson_dispatch(args: &Value, tail: &str) -> ProjectsResult<Value> {
    match tail {
        "dumps_compact" => Ok(json!(p::pyjson::dumps_compact(&args["value"]))),
        "dumps_indent2" => Ok(json!(p::pyjson::dumps_indent2(&args["value"]))),
        "dumps_default" => Ok(json!(p::pyjson::dumps_default(&args["value"]))),
        "python_float_repr" => Ok(json!(p::pyjson::python_float_repr(
            args["value"].as_f64().unwrap_or(f64::NAN)
        ))),
        _ => Err(value(format!("unknown pyjson op {tail}"))),
    }
}

/// Part-2 dispatch; reached when part 1 did not match.
pub fn dispatch2(ctx: &mut Ctx, op: &str, args: &Value) -> ProjectsResult<Value> {
    let Some((module, tail)) = op.split_once('.') else {
        return Err(value(format!("unknown op {op}")));
    };
    match module {
        "runtime" => runtime_dispatch(ctx, tail, args),
        "archive" => archive_dispatch(ctx, tail, args),
        "exports" => exports_dispatch(ctx, tail, args),
        "evidence" => evidence_dispatch(ctx, tail, args),
        "execution" => execution_dispatch(ctx, tail, args),
        "run_observation" => run_observation_dispatch(ctx, tail, args),
        "run_control" => run_control_dispatch(ctx, tail, args),
        "monitoring" => monitoring_dispatch(ctx, tail, args),
        "observer" => observer_dispatch(ctx, tail, args),
        "receipts" => receipts_dispatch(ctx, tail, args),
        "schedule_install" => schedule_install_dispatch(ctx, tail, args),
        "projection" => projection_dispatch(ctx, args, tail),
        "pyjson" => pyjson_dispatch(args, tail),
        "catalogue" => match tail {
            "load_catalogue" => p::catalogue::load_catalogue().map(Value::Object),
            "definition_hash" => Ok(json!(p::catalogue::definition_hash(&args["definition"]))),
            _ => Err(value(format!("unknown catalogue op {tail}"))),
        },
        "crawl_evidence" => match tail {
            "record_failure" => p::crawl_evidence::record_failure(
                &dir_of(ctx, args),
                args["reason"].as_str().unwrap_or_default(),
            ),
            "record_scan" => {
                p::crawl_evidence::record_scan(&dir_of(ctx, args), &ctx.path(&args["scan"]))
            }
            _ => Err(value(format!("unknown crawl_evidence op {tail}"))),
        },
        "competitor_scans" => match tail {
            "has_active_run" => {
                p::competitor_scans::has_active_run(&dir_of(ctx, args)).map(Value::from)
            }
            "task_competitor_run_reference" => p::competitor_scans::task_competitor_run_reference(
                &dir_of(ctx, args),
                args["task_id"].as_str().unwrap_or_default(),
                &args["task"],
            ),
            "get_competitor_scan_run" => p::competitor_scans::get_competitor_scan_run(
                &dir_of(ctx, args),
                args["run_id"].as_str().unwrap_or_default(),
            ),
            "cancel_competitor_scan_run" => p::competitor_scans::cancel_competitor_scan_run(
                &dir_of(ctx, args),
                args["run_id"].as_str().unwrap_or_default(),
            ),
            _ => Err(value(format!("unknown competitor_scans op {tail}"))),
        },
        "origin_pacing" => match tail {
            "origin" => p::origin_pacing::origin(args["target"].as_str().unwrap_or_default())
                .map(Value::from),
            _ => Err(value(format!("unknown origin_pacing op {tail}"))),
        },
        _ => Err(value(format!("unknown op {op}"))),
    }
}

fn dir_of(ctx: &Ctx, args: &Value) -> PathBuf {
    path(ctx, args)
}
