//! Provider lifecycle handlers (the reference suite, the reference suite):
//! credentials, OAuth grants, discovery, verification, bounded collection,
//! joins and replay. Provider calls go through one dispatch table backed by the
//! same clients the dedicated commands use.

use serde_json::{Map, Value};

use spiderhead_core::host::SystemHost;
use spiderhead_sources::credentials;
use spiderhead_sources::dataforseo::{Client as DfsClient, ClientArgs as DfsArgs, DfsEnv};
use spiderhead_sources::gsc;
use spiderhead_sources::oauth;
use spiderhead_sources::providers::{self, DispatchError, ProviderEnv};
use spiderhead_sources::providers_lifecycle;
use spiderhead_sources::topvisor;
use spiderhead_sources::yandex_webmaster::{collect as wm_collect, CollectArgs, WmEnv};

use crate::error::ToolError;

use super::net::ProviderTransport;
use super::sources_net::{environment, fail, run, transport};
use super::{kwargs, opt, opt_bool, opt_str};

/// Drive one provider future; a runtime that cannot start is a dispatch error.
fn go_<F: std::future::Future>(future: F) -> Result<F::Output, DispatchError> {
    super::net::block_on(future).map_err(|message| dispatch_error("RuntimeError", message))
}

fn dispatch_error(class: &str, message: String) -> DispatchError {
    let mut error = DispatchError::plain(class, &message);
    error.missing_credential = class == "MissingCredential";
    error
}

/// Keyword arguments of a dispatch request, rejecting names the call does not take.
fn checked_kwargs(
    request: Option<&Value>,
    call: &str,
    accepted: &[&str],
) -> Result<Map<String, Value>, DispatchError> {
    let kwargs = request
        .and_then(|r| r.get("kwargs"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in kwargs.keys() {
        if !accepted.contains(&key.as_str()) {
            return Err(dispatch_error(
                "TypeError",
                format!("{call}() got an unexpected keyword argument '{key}'"),
            ));
        }
    }
    Ok(kwargs)
}

/// The live provider-function dispatch: route name and request document in,
/// provider result out. Routes without a client in this build answer a plain
/// error rather than a guess.
fn live_dispatch(route: &str, request: Option<&Value>) -> Result<Value, DispatchError> {
    let (host, root) = environment();
    let transport = ProviderTransport::new()
        .map_err(|error| dispatch_error("RuntimeError", error.to_string()))?;
    let arg = |name: &str| request.and_then(|r| r.get(name));
    match route {
        "gsc.discover_properties" => go_(gsc::discover_properties(
            &host, &root, None, None, &transport,
        ))?
        .map_err(|e| dispatch_error(e.python_class(), e.to_string())),
        "gsc.sitemap_status" => {
            let kw = checked_kwargs(request, "sitemap_status", &["site_url"])?;
            go_(gsc::sitemap_status(
                &host,
                &root,
                kw.get("site_url").and_then(Value::as_str).unwrap_or(""),
                None,
                None,
                &transport,
            ))?
            .map_err(|e| dispatch_error(e.python_class(), e.to_string()))
        }
        "gsc.search_analytics_pages" => {
            let kw = checked_kwargs(
                request,
                "search_analytics_pages",
                &[
                    "site_url",
                    "start_date",
                    "end_date",
                    "dimensions",
                    "row_limit",
                    "max_rows",
                ],
            )?;
            let args = gsc::PagesArgs {
                start_date: kw.get("start_date").and_then(Value::as_str),
                end_date: kw.get("end_date").and_then(Value::as_str),
                dimensions: kw.get("dimensions").cloned(),
                row_limit: kw.get("row_limit").and_then(Value::as_i64).unwrap_or(1000),
                max_rows: kw
                    .get("max_rows")
                    .and_then(Value::as_i64)
                    .unwrap_or(gsc::MAX_ANALYTICS_ROWS),
                token: None,
                now: None,
                service_account: None,
            };
            go_(gsc::search_analytics_pages(
                &host,
                &root,
                kw.get("site_url").and_then(Value::as_str).unwrap_or(""),
                &args,
                &transport,
            ))?
            .map_err(|e| dispatch_error(e.python_class(), e.to_string()))
        }
        "yandex_webmaster.collect" => {
            let kw = checked_kwargs(
                request,
                "collect",
                &[
                    "user_id", "host_id", "query_id", "params", "paginate", "max_rows",
                ],
            )?;
            let env = WmEnv::live(&host, &root, &transport);
            let args = CollectArgs {
                user_id: kw.get("user_id"),
                host_id: kw.get("host_id"),
                query_id: kw.get("query_id"),
                params: kw.get("params"),
                paginate: kw.get("paginate"),
                max_rows: kw.get("max_rows"),
                token: None,
                transport: None,
            };
            let operation = arg("operation").and_then(Value::as_str).unwrap_or("");
            go_(wm_collect(operation, &args, &env))?
                .map_err(|e| dispatch_error(e.python_class(), e.to_string()))
        }
        "topvisor.fetch" => {
            let operation = arg("operation")
                .and_then(Value::as_str)
                .unwrap_or("projects");
            let params = arg("params").and_then(Value::as_object);
            go_(topvisor::fetch(&host, &root, &transport, operation, params))?
                .map_err(|e| dispatch_error(e.python_class(), e.to_string()))
        }
        "wayback.history" => {
            let kw = checked_kwargs(
                request,
                "history",
                &["url", "limit", "from_date", "to_date"],
            )?;
            go_(spiderhead_sources::wayback::history(
                &transport,
                kw.get("url").and_then(Value::as_str).unwrap_or(""),
                kw.get("limit"),
                kw.get("from_date").and_then(Value::as_str),
                kw.get("to_date").and_then(Value::as_str),
            ))?
            .map_err(|e| dispatch_error(e.python_class(), e.to_string()))
        }
        "crtsh.subdomains" => {
            let kw = checked_kwargs(request, "subdomains", &["domain"])?;
            go_(spiderhead_sources::crtsh::subdomains(
                &transport,
                kw.get("domain").and_then(Value::as_str).unwrap_or(""),
            ))?
            .map_err(|e| dispatch_error(e.python_class(), e.to_string()))
        }
        "dataforseo.balance" => {
            let env = DfsEnv::live(&host, &root, &transport);
            let client = DfsClient::new(
                &env,
                &DfsArgs {
                    env_name: arg("env"),
                    ..DfsArgs::default()
                },
            )
            .map_err(|e| dispatch_error(e.python_class(), e.to_string()))?;
            go_(client.balance())?.map_err(|e| {
                let mut error = dispatch_error(e.python_class(), e.to_string());
                if let spiderhead_sources::dataforseo::DfsError::Provider { status, .. } = &e {
                    error.status = Some(*status);
                }
                error
            })
        }
        other => Err(dispatch_error(
            "RuntimeError",
            format!("provider route {other} is not available in this build"),
        )),
    }
}

/// Run `f` with the live provider environment (real credentials root, real
/// grant state, live dispatch).
pub(super) fn with_live_env<T>(f: impl FnOnce(&ProviderEnv) -> T) -> T {
    let host = SystemHost;
    let root = credentials::default_config_root(&host);
    let service_account_available =
        || super::sources::gsc_service_account_status(&host, &root) == "configured_unverified";
    let service_account_status = || super::sources::gsc_service_account_status(&host, &root);
    let grant_available = |provider: &str| oauth::grant_available(&root, provider);
    let now = || spiderhead_sources::pytime::isoformat(spiderhead_sources::pytime::now_micros());
    let started = std::time::Instant::now();
    let monotonic = move || started.elapsed().as_secs_f64();
    let open_scan = |_path: &str| {
        Err(spiderhead_sources::cloudflare::CfError::Os(
            "open_scan is not available in this handler".to_owned(),
        ))
    };
    let env = ProviderEnv {
        host: &host,
        root: &root,
        service_account_available: &service_account_available,
        service_account_status: &service_account_status,
        grant_available: &grant_available,
        now: &now,
        monotonic: &monotonic,
        dispatch: &live_dispatch,
        open_scan: &open_scan,
    };
    f(&env)
}

/// `provider_credential`: inspect, set or revoke a registry-owned credential.
pub fn provider_credential(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let provider = opt_str(&args, "provider").unwrap_or("");
    let component = opt_str(&args, "component").unwrap_or("");
    let action = opt_str(&args, "action").unwrap_or("status");
    let value = opt(&args, "value");
    let replace = opt_bool(&args, "replace").unwrap_or(false);
    if action == "set" && value.is_none() {
        return Err(fail(
            "credential writes require the local CLI stdin channel",
        ));
    }
    if action != "set" && (value.is_some() || replace) {
        return Err(fail(
            "value and replace are valid only for local credential writes",
        ));
    }
    with_live_env(|env| {
        providers::manage_credential(
            env,
            provider,
            component,
            action,
            value,
            replace,
            opt_bool(&args, "confirm").unwrap_or(false),
        )
    })
    .map_err(fail)
}

/// `provider_discover`: account-visible resources, never selected or bound.
pub fn provider_discover(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let provider = opt_str(&args, "provider").unwrap_or("");
    let domain = opt(&args, "domain");
    with_live_env(|env| providers::provider_discover(env, provider, domain)).map_err(fail)
}

/// `provider_verify`: explicit, bounded, read-only credential verification.
pub fn provider_verify(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let provider = opt(&args, "provider").cloned().unwrap_or(Value::Null);
    let request = opt(&args, "request");
    with_live_env(|env| providers_lifecycle::provider_verify(env, &provider, request)).map_err(fail)
}

/// `provider_collect`: one declared read-only operation.
pub fn provider_collect(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let provider = opt_str(&args, "provider").unwrap_or("");
    let operation = opt_str(&args, "operation").unwrap_or("");
    let request = opt(&args, "request").cloned().unwrap_or(Value::Null);
    let artifact_dir = opt_str(&args, "artifact_dir");
    with_live_env(|env| {
        providers_lifecycle::provider_collect(
            env,
            provider,
            operation,
            &request,
            artifact_dir,
            None,
        )
    })
    .map_err(fail)
}

/// `provider_join`: crawl pages joined to supplied evidence rows.
pub fn provider_join(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let list = |name: &str| opt(&args, name).and_then(Value::as_array).cloned();
    let pages = list("crawl_pages").unwrap_or_default();
    let rows = list("evidence_rows").unwrap_or_default();
    let adjustments = list("adjustments");
    providers_lifecycle::provider_join(
        &pages,
        &rows,
        "url",
        "url",
        opt_bool(&args, "review_external_only").unwrap_or(false),
        adjustments.as_deref(),
    )
    .map_err(fail)
}

/// `provider_replay`: offline join of saved evidence to a saved scan.
pub fn provider_replay(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let text = |name: &str| opt_str(&args, name).unwrap_or("");
    with_live_env(|env| {
        providers_lifecycle::provider_replay(
            env,
            text("input_path"),
            text("evidence_file"),
            text("out_dir"),
            opt_str(&args, "url_column").unwrap_or("url"),
            opt_bool(&args, "review_external_only").unwrap_or(false),
        )
    })
    .map_err(fail)
}

/// `provider_auth`: the OAuth grant lifecycle for a provider.
pub fn provider_auth(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let (host, root) = environment();
    let transport = transport()?;
    let provider = opt_str(&args, "provider").unwrap_or("");
    let manage = oauth::Manage {
        action: opt_str(&args, "action").unwrap_or("status"),
        grant_file: opt_str(&args, "grant_file"),
        confirm: opt_bool(&args, "confirm").unwrap_or(false),
        state: opt_str(&args, "state"),
        code: opt_str(&args, "code"),
        now: None,
    };
    run(oauth::manage_grant(
        &host, &root, provider, &manage, &transport,
    ))?
    .map_err(fail)
}
