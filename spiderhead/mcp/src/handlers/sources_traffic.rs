//! Search-performance handlers (the reference suite, the reference suite):
//! Google Search Console, Yandex Webmaster URL queries and the local
//! provider-day store sync.

use serde_json::{json, Value};

use spiderhead_sources::gsc;
use spiderhead_sources::sources_db::{self, SyncArgs, SyncEnv};
use spiderhead_sources::yandex_webmaster::{url_queries, UrlQueriesArgs, WmEnv, WmError};

use crate::error::ToolError;

use super::sources_net::{environment, fail, present, run, transport};
use super::{kwargs, opt, opt_bool, opt_str};

/// `gsc_query`: search performance, or Google's indexing verdict for one URL.
pub fn gsc_query(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(site_url) = present(&args, "site_url") else {
        return Err(fail("site_url required"));
    };
    let (host, root) = environment();
    let transport = transport()?;
    let mode = opt_str(&args, "mode").unwrap_or("search_analytics");
    if mode == "inspect_url" {
        let Some(inspection_url) = present(&args, "inspection_url") else {
            return Err(fail("inspection_url required for mode=inspect_url"));
        };
        return run(gsc::inspect_url(
            &host,
            &root,
            site_url,
            inspection_url,
            None,
            None,
            &transport,
        ))?
        .map_err(fail);
    }
    if mode != "search_analytics" {
        return Err(fail("mode must be search_analytics or inspect_url"));
    }
    let analytics = gsc::Analytics {
        start_date: Some(opt_str(&args, "start_date").unwrap_or("28daysAgo")),
        end_date: Some(opt_str(&args, "end_date").unwrap_or("today")),
        dimensions: opt(&args, "dimensions").cloned(),
        row_limit: opt(&args, "row_limit").cloned().unwrap_or(json!(1000)),
        token: None,
        now: None,
        service_account: None,
    };
    run(gsc::search_analytics(
        &host, &root, site_url, &analytics, &transport,
    ))?
    .map_err(fail)
}

/// `webmaster_url_queries`: bounded URL-to-query evidence from an own Webmaster host.
pub fn webmaster_url_queries(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(host_id) = present(&args, "host_id") else {
        return Err(fail("host_id required"));
    };
    let (host, root) = environment();
    let transport = transport()?;
    let env = WmEnv::live(&host, &root, &transport);
    let url_args = UrlQueriesArgs {
        url: opt(&args, "url"),
        url_contains: opt(&args, "url_contains"),
        user_id: None,
        max_urls: opt(&args, "max_urls"),
        max_queries_per_url: opt(&args, "max_queries_per_url"),
        start_date: opt(&args, "start_date"),
        end_date: opt(&args, "end_date"),
        token: None,
        transport: None,
    };
    run(url_queries(&json!(host_id), &url_args, &env))?.map_err(|error: WmError| fail(error))
}

/// `sources_sync`: fetch missing or forced days into one local provider database.
pub fn sources_sync(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(source) = present(&args, "source") else {
        let mut names: Vec<&str> = sources_db::SOURCES.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let listed = names
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(fail(format!("source required: one of [{listed}]")));
    };
    let path =
        sources_db::db_path(opt_str(&args, "db"), opt_str(&args, "project")).map_err(fail)?;
    let (host, root) = environment();
    let transport = transport()?;
    let env = SyncEnv {
        host: &host,
        root: &root,
        transport: &transport,
        sleeper: None,
    };
    let resource = json!(opt_str(&args, "resource").unwrap_or(""));
    let sync_args = SyncArgs {
        start_date: opt_str(&args, "start_date"),
        end_date: opt_str(&args, "end_date"),
        force: opt_bool(&args, "force").unwrap_or(false),
        now: None,
    };
    run(sources_db::sync(
        &path, source, &resource, &sync_args, None, &env,
    ))?
    .map_err(fail)
}
