//! Handlers of the keyless and single-credential provider commands
//! (the reference suite): every call goes through [`net::ProviderTransport`],
//! so a loopback stand-in can serve them without any real endpoint.

use std::path::PathBuf;

use serde_json::{json, Map, Value};

use spiderhead_core::host::SystemHost;
use spiderhead_sources::credentials::default_config_root;

use crate::error::ToolError;

use super::net::{block_on, ProviderTransport};
use super::{kwargs, opt, opt_bool, opt_str};

/// The host and credential root every handler resolves secrets against.
pub(super) fn environment() -> (SystemHost, PathBuf) {
    let host = SystemHost;
    let root = default_config_root(&host);
    (host, root)
}

/// The provider transport, or the handler failure when no runtime can be built.
pub(super) fn transport() -> Result<ProviderTransport, ToolError> {
    ProviderTransport::new().map_err(|error| ToolError::Failed(error.to_string()))
}

/// Run one provider future to completion.
pub(super) fn run<F: std::future::Future>(future: F) -> Result<F::Output, ToolError> {
    block_on(future).map_err(ToolError::Failed)
}

/// Any domain error as the handler failure the reference CLI prints (`str(exc)`).
pub(super) fn fail(error: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(error.to_string())
}

/// A non-empty string argument (reference `if not value`).
pub(super) fn present<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    opt_str(args, name).filter(|value| !value.is_empty())
}

/// A list-of-strings argument; non-string items are skipped.
pub(super) fn str_list(args: &Map<String, Value>, name: &str) -> Option<Vec<String>> {
    opt(args, name).and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    })
}

/// `wayback_history`: every recorded Wayback snapshot of a URL.
pub fn wayback_history(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(url) = present(&args, "url") else {
        return Err(fail("url required"));
    };
    let transport = transport()?;
    run(spiderhead_sources::wayback::history(
        &transport,
        url,
        opt(&args, "limit"),
        opt_str(&args, "from_date"),
        opt_str(&args, "to_date"),
    ))?
    .map_err(fail)
}

/// `crtsh_subdomains`: hosts named in public certificate-transparency logs.
pub fn crtsh_subdomains(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(domain) = present(&args, "domain") else {
        return Err(fail("domain required"));
    };
    let transport = transport()?;
    run(spiderhead_sources::crtsh::subdomains(&transport, domain))?.map_err(fail)
}

/// `regions_tree`: the free Yandex region tree, optionally saved to a file.
pub fn regions_tree(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_sources::yandex_cloud::{CloudArgs, CloudEnv};
    let args = kwargs(args);
    let (host, root) = environment();
    let transport = transport()?;
    let env = CloudEnv::live(&host, &root, &transport);
    run(spiderhead_sources::yandex_regions::fetch_tree(
        &env,
        &CloudArgs::default(),
        opt(&args, "save_to"),
    ))?
    .map_err(fail)
}

/// `indexnow_submit`: push changed URLs to the IndexNow engines.
pub fn indexnow_submit(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let urls = str_list(&args, "urls").unwrap_or_default();
    if urls.is_empty() {
        return Err(fail("urls required"));
    }
    let Some(host_arg) = present(&args, "host") else {
        return Err(fail("host required"));
    };
    let (host, root) = environment();
    let transport = transport()?;
    run(spiderhead_sources::indexnow::submit(
        &transport,
        &host,
        &root,
        &urls,
        host_arg,
        None,
        opt_str(&args, "key_location"),
    ))?
    .map_err(fail)
}

/// `topvisor_read`: one bounded page of existing Topvisor data.
pub fn topvisor_read(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let (host, root) = environment();
    let transport = transport()?;
    let operation = opt_str(&args, "operation").unwrap_or("projects");
    let params = opt(&args, "params").and_then(Value::as_object);
    match run(spiderhead_sources::topvisor::fetch(
        &host, &root, &transport, operation, params,
    ))? {
        Ok(page) => Ok(page),
        Err(error) => Ok(json!({"ok": false, "error": error.to_string()})),
    }
}

/// `crux_report`: field Core Web Vitals for a URL, an origin or a URL sample.
pub fn crux_report(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_sources::crux;
    let args = kwargs(args);
    let (host, root) = environment();
    let transport = transport()?;
    let truthy = |name: &str| match args.get(name) {
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    };
    if let Some(urls) = opt(&args, "urls") {
        if truthy("url") || truthy("origin") || truthy("metrics") {
            return Err(fail("urls cannot be combined with url, origin, or metrics"));
        }
        let cache_dir = present(&args, "cache_dir").map(PathBuf::from);
        let sample = crux::Sample {
            urls,
            form_factor: present(&args, "form_factor"),
            max_samples: opt(&args, "max_samples"),
            cache_dir: cache_dir.as_deref(),
            cache_max_age_hours: opt(&args, "cache_max_age_hours"),
            api_key: None,
            now: None,
        };
        return run(crux::sample(&host, &root, &transport, &sample))?.map_err(fail);
    }
    let default_budget = opt(&args, "max_samples").is_none_or(|v| v.as_i64() == Some(25));
    let default_age = opt(&args, "cache_max_age_hours").is_none_or(|v| v.as_f64() == Some(24.0));
    if truthy("cache_dir") || !default_budget || !default_age {
        return Err(fail("sample budget and cache options require urls"));
    }
    let query = crux::Query {
        url: present(&args, "url"),
        origin: present(&args, "origin"),
        form_factor: present(&args, "form_factor"),
        metrics: opt(&args, "metrics"),
        api_key: None,
        now: None,
    };
    run(crux::query(&host, &root, &transport, &query))?.map_err(fail)
}

/// `miratext_analyze`: start or resume a bounded text-similarity analysis.
pub fn miratext_analyze(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_sources::miratext;
    let args = kwargs(args);
    let (host, root) = environment();
    let transport = transport()?;
    let urls = opt(&args, "urls").and_then(Value::as_array);
    let texts = opt(&args, "texts").and_then(Value::as_array);
    let call = miratext::Call {
        urls: urls.map(Vec::as_slice),
        texts: texts.map(Vec::as_slice),
        my: opt_str(&args, "my"),
        hash: opt(&args, "hash"),
        check_type: opt_str(&args, "check_type").unwrap_or("url"),
        keywords: opt_str(&args, "keywords"),
        paid: opt_bool(&args, "paid").unwrap_or(false),
        confirm_paid: opt_bool(&args, "confirm_paid").unwrap_or(false),
        timeout: opt(&args, "timeout"),
        top: opt(&args, "top"),
        api_key: opt_str(&args, "api_key"),
    };
    run(miratext::analyze(&transport, &host, &root, &call))?.map_err(fail)
}
