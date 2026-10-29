//! Stage execution glue for the runner: region count parsing, engine availability
//! per stage, and stage dispatch with captured output.

use super::*;

pub(super) fn regions_int(cfg: &Cfg) -> i64 {
    let first = cfg
        .get("regions")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or(json!(225));
    match first {
        Value::Number(n) => n.as_i64().unwrap_or(225),
        Value::String(text) => text.parse().unwrap_or(225),
        _ => 225,
    }
}

pub(super) fn engine_available(stage: &str) -> Value {
    match stage {
        "collect" | "synonyms" => json!(["yandex_wordstat"]),
        "cluster" => json!(["yandex_search"]),
        _ => json!(["arsenkin_wordstat"]),
    }
}

/// Run one registered stage and return its document plus captured stdout
/// Printed lines are rebuilt from the stage results with fixed wording.
pub fn execute(stage: &str, project: &str, options: &Options) -> Result<Execution, Error> {
    if !STAGES.contains(&stage) {
        return Err(Error::Config(format!(
            "stage must be one of: {}",
            STAGES.join(", ")
        )));
    }
    let directory = config::data_dir(project)?;
    if PAID_STAGES.contains(&stage) {
        let estimate = paid_estimate(stage, project, options)?;
        if options.dry_run || !options.confirm_paid {
            return Ok(Execution {
                result: json!({
                    "contract": "spiderhead.semantics.operation.v1",
                    "mode": if options.dry_run { "estimate" } else { "confirmation_required" },
                    "state": "complete",
                    "project": project,
                    "stage": stage,
                    "paid": true,
                    "estimate": estimate,
                    "requires_confirmation": !options.dry_run,
                }),
                logs: Vec::new(),
            });
        }
        return Err(Error::Config(format!(
            "{stage} paid execution is not yet available in this build"
        )));
    }
    if stage == "mine" {
        return Err(Error::Config(
            "mine is not yet available in this build".to_owned(),
        ));
    }
    if stage == "insights" || stage == "insights-tasks" {
        return Err(Error::Config(format!(
            "{stage} is not yet available in this build"
        )));
    }
    if stage == "init" {
        config::ensure_project(project)?;
        let store = open_store(project, true, false)?;
        let run_id = store
            .start_stage("init", None)
            .map_err(|e| Error::Config(e.to_string()))?;
        let result = json!({"directory": directory.to_string_lossy()});
        store
            .finish_stage(run_id, "done", Some(&result), None)
            .map_err(|e| Error::Config(e.to_string()))?;
        return Ok(Execution {
            result: json!({
                "project": project,
                "stage": "init",
                "directory": directory.to_string_lossy(),
            }),
            logs: vec![format!("init OK: {}\n", directory.display())],
        });
    }
    let out: PathBuf = match &options.out {
        Some(out) => PathBuf::from(out),
        None => directory.clone(),
    };
    let readonly_stages = [
        "status",
        "clusters",
        "cluster-show",
        "cluster-queries",
        "insights",
    ];
    let dry_read =
        options.dry_run && matches!(stage, "import" | "export" | "excel" | "insights-tasks");
    let store = open_store(project, false, readonly_stages.contains(&stage) || dry_read)?;
    match stage {
        "status" => {
            let result = status_doc(&store, project, &config::db_path(project)?)?;
            let mut logs = Vec::new();
            if !options.json {
                let counts = result
                    .get("counts")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let pairs: Vec<String> = counts
                    .iter()
                    .map(|(key, value)| {
                        let rendered = match value {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        };
                        format!("{key}={rendered}")
                    })
                    .collect();
                logs.push(format!("=== {project} ===\n"));
                logs.push(format!("{}\n", pairs.join(" | ")));
                let spend = result.get("spend").cloned().unwrap_or(Value::Null);
                let spend_text = match spend.as_object() {
                    Some(object) => py_repr_flat(object),
                    None => spend.to_string(),
                };
                logs.push(format!("provider units: {spend_text}\n"));
                logs.push(format!(
                    "next: seohead semantics {} --project {project}\n",
                    result
                        .get("next_stage")
                        .and_then(Value::as_str)
                        .unwrap_or("cluster")
                ));
            }
            Ok(Execution { result, logs })
        }
        "export" => {
            let scope = options.scope.clone();
            let mut result = store
                .export(&out, &scope, options.dry_run)
                .map_err(|e| Error::Config(e.to_string()))?;
            if !options.dry_run {
                // The write path returns the bare row count.
                let rows = if result.is_number() {
                    result.clone()
                } else {
                    result.get("rows").cloned().unwrap_or(json!(0))
                };
                result = json!({
                    "contract": "spiderhead.semantics.operation.v1",
                    "mode": "write",
                    "state": "complete",
                    "scope": scope,
                    "rows": rows,
                    "files": [
                        out.join("pool.csv").to_string_lossy(),
                        out.join("pool.json").to_string_lossy(),
                    ],
                });
            }
            // Project and stage keys come first, then the export document's keys;
            // the result is an ordered object.
            let mut merged = Map::new();
            merged.insert("project".into(), json!(project));
            merged.insert("stage".into(), json!("export"));
            if let Some(object) = result.as_object() {
                for (key, value) in object {
                    merged.insert(key.clone(), value.clone());
                }
            }
            Ok(Execution {
                result: Value::Object(merged),
                logs: Vec::new(),
            })
        }
        "import" if options.dry_run => {
            let file = options
                .file
                .as_deref()
                .ok_or_else(|| Error::Config("import requires --file".to_owned()))?;
            let result = crate::stages::import::plan(
                &store,
                &config::load(project)?,
                Path::new(file),
                options.sheet.as_deref(),
                options.mapping.as_ref(),
                &options.merge,
            )
            .map_err(|e| Error::Config(e.to_string()))?;
            Ok(Execution {
                result,
                logs: Vec::new(),
            })
        }
        "excel" if options.dry_run => {
            let (clause, params) = store
                .scope_clause(&options.scope, "p")
                .map_err(|e| Error::Config(e.to_string()))?;
            let count = store
                .q1(
                    &format!("SELECT COUNT(*) n FROM phrases p {clause}"),
                    &params,
                )?
                .and_then(|r| r["n"].as_i64())
                .unwrap_or(0);
            let project_name = config::load(project)?
                .get("_project")
                .and_then(Value::as_str)
                .unwrap_or("project")
                .to_owned();
            Ok(Execution {
                result: json!({
                    "contract": "spiderhead.semantics.operation.v1",
                    "mode": "preview",
                    "state": "complete",
                    "format": "spiderhead.semantics.export-plan.v1",
                    "scope": options.scope,
                    "rows": count,
                    "planned_files": [
                        out.join(format!("{project_name}_semantic_core.xlsx")).to_string_lossy()
                    ],
                }),
                logs: Vec::new(),
            })
        }
        "clusters" | "cluster-show" | "cluster-queries" | "set-landing" => {
            let journaled = stage == "set-landing";
            let run_id = if journaled {
                Some(
                    store
                        .start_stage(stage, None)
                        .map_err(|e| Error::Config(e.to_string()))?,
                )
            } else {
                None
            };
            let built: Result<Value, Error> = {
                match stage {
                    "clusters" => read_model::clusters(&store, options.offset, options.limit),
                    "cluster-show" => match reference_of(options) {
                        Some(reference) => read_model::cluster_show(&store, &reference),
                        None => Err(Error::Config("set-landing requires --cluster".to_owned())),
                    },
                    "cluster-queries" => match reference_of(options) {
                        Some(reference) => read_model::cluster_queries(
                            &store,
                            &reference,
                            options.offset,
                            options.limit,
                            Some(project),
                        ),
                        None => Err(Error::Config(
                            "cluster-queries requires --cluster".to_owned(),
                        )),
                    },
                    _ => match reference_of(options) {
                        Some(reference) => match options.url.clone() {
                            Some(url) => read_model::set_landing(&store, &reference, &url),
                            None => Err(Error::Config("set-landing requires --url".to_owned())),
                        },
                        None => Err(Error::Config("set-landing requires --cluster".to_owned())),
                    },
                }
            };
            match built {
                Ok(result) => {
                    if let Some(run_id) = run_id {
                        store
                            .finish_stage(run_id, "done", Some(&result), None)
                            .map_err(|e| Error::Config(e.to_string()))?;
                    }
                    Ok(Execution {
                        result,
                        logs: Vec::new(),
                    })
                }
                Err(error) => {
                    if let Some(run_id) = run_id {
                        let _ = store.rollback();
                        let _ =
                            store.finish_stage(run_id, "failed", None, Some(&error.to_string()));
                    }
                    Err(error)
                }
            }
        }
        "init" => unreachable!("init handled above"),
        _ => {
            let cfg = config::load(project)?;
            let estimate = if PAID_STAGES.contains(&stage) {
                Some(
                    paid_estimate(stage, project, options)
                        .map_err(|e| Error::Config(e.to_string()))?,
                )
            } else {
                None
            };
            let run_id = store
                .start_stage(stage, estimate.as_ref())
                .map_err(|e| Error::Config(e.to_string()))?;
            let summary = run_stage(stage, &store, &cfg, &out, options);
            match summary {
                Ok(summary) => {
                    store
                        .finish_stage(run_id, "done", Some(&summary), None)
                        .map_err(|e| Error::Config(e.to_string()))?;
                    if EXPORTING.contains(&stage) {
                        store
                            .export(&out, "all", false)
                            .map_err(|e| Error::Config(e.to_string()))?;
                    }
                    // The stage modules print their own progress lines (the CLI's
                    // human output); the runner carries no copies of them.
                    let logs = Vec::new();
                    Ok(Execution {
                        result: json!({
                            "contract": "spiderhead.semantics.operation.v1",
                            "mode": "write",
                            "state": "complete",
                            "project": project,
                            "stage": stage,
                            "result": summary,
                            "estimate": estimate,
                            "counts": store.counts().map_err(|e| Error::Config(e.to_string()))
                                .map(|c| Value::Object(
                                    c.iter().map(|(k, v)| (k.clone(), json!(v))).collect()
                                ))?,
                            "spend": store.spend().map_err(|e| Error::Config(e.to_string()))?,
                        }),
                        logs,
                    })
                }
                Err(error) => {
                    let _ = store.rollback();
                    let _ = store.finish_stage(run_id, "failed", None, Some(&error.to_string()));
                    Err(error)
                }
            }
        }
    }
}

pub(super) fn reference_of(options: &Options) -> Option<String> {
    options.cluster.clone()
}

/// Dispatch one free stage to its module.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_stage(
    stage: &str,
    store: &Store,
    cfg: &Cfg,
    out: &Path,
    options: &Options,
) -> Result<Value, Error> {
    match stage {
        "clean" => crate::stages::clean::run(store, cfg),
        "graph" => crate::stages::graph::run(store, cfg, out),
        "competitors" => crate::stages::competitors::run(store, cfg, out),
        "report" => crate::stages::report::run(store, cfg, out),
        "excel" => crate::stages::excel::run(store, cfg, out, &options.scope),
        "sitematch" => crate::stages::sitematch::run(store, cfg),
        "relevance" => crate::stages::relevance::run(store, cfg, 10),
        "import" => {
            let file = options
                .file
                .as_deref()
                .ok_or_else(|| Error::Config("import requires --file".to_owned()))?;
            crate::stages::import::run(
                store,
                cfg,
                Path::new(file),
                options.sheet.as_deref(),
                options.mapping.as_ref(),
                &options.merge,
            )
            .map_err(|e| Error::Config(e.to_string()))
        }
        other => Err(Error::Config(format!(
            "stage {other} is not wired in this build"
        ))),
    }
}

/// Quiet MCP wrapper. Paid stages answer the estimate unless
/// `confirm_paid` is set; failures return `ok: false` instead of raising.
pub fn run_quiet(
    stage: &str,
    project: &str,
    allowed: &[&str],
    confirm_paid: bool,
    options: &Options,
) -> Value {
    if !allowed.contains(&stage) {
        return json!({
            "ok": false,
            "error": format!(
                "stage must be one of: {}",
                {
                    let mut names: Vec<&str> = allowed.to_vec();
                    names.sort_unstable();
                    names.join(", ")
                }
            ),
        });
    }
    if PAID_STAGES.contains(&stage) && !confirm_paid {
        return match paid_estimate(stage, project, options) {
            Ok(estimate) => json!({
                "ok": true,
                "result": {
                    "stage": stage,
                    "paid": true,
                    "estimate": estimate,
                    "requires_confirmation": true,
                    "paid_calls_made": 0,
                },
                "logs": "",
            }),
            Err(error) => json!({"ok": false, "error": error.to_string(), "paid": true}),
        };
    }
    let mut options = options.clone();
    options.json = true;
    options.resume = false;
    if options.max_seeds == 0 {
        options.max_seeds = 0;
    }
    if options.max_phrases == 0 {
        options.max_phrases = 100_000;
    }
    options.confirm_paid = confirm_paid;
    match execute(stage, project, &options) {
        Ok(execution) => json!({
            "ok": true,
            "result": execution.result,
            // The raw stream is captured: each line already ends with \n.
            "logs": execution.logs.concat(),
        }),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
            "logs": "",
        }),
    }
}
