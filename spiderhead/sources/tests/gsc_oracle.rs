//! Oracle-diff test for the GSC client against `gsc_oracle.json`.
//!
//! Every scenario replays the reference client's recorded stub-server exchange:
//! `stub_server.run_scenario` patched `gsc.open_no_redirect` and `gsc.datetime`, so the
//! recorded requests carry the real Authorization header and payload the reference client sent.

mod common;

use common::{assert_scenario, load_fixture, scenario_host, StubTransport};
use serde_json::Value;
use spiderhead_sources::gsc::{self, inspect_urls, search_analytics_page, search_analytics_pages};
use spiderhead_sources::gsc::{PageArgs, PagesArgs};

fn arg<'a>(args: &'a Value, name: &str) -> Option<&'a Value> {
    args.get(name).filter(|v| !v.is_null())
}

// the reference suite + the reference suite +
// the reference suite — recorded stub-server scenarios.
#[tokio::test(flavor = "current_thread")]
async fn gsc_oracle_parity() {
    for scenario in load_fixture("gsc_oracle.json") {
        let name = scenario.get("name").and_then(Value::as_str).unwrap_or("?");
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let (host, root) = scenario_host(&scenario);
        let args = scenario.get("args").cloned().unwrap_or_default();
        let now = scenario.get("frozen_now_micros").and_then(Value::as_i64);
        let site_url = args.get("site_url").and_then(Value::as_str).unwrap_or("");
        let token = arg(&args, "token");
        let call = scenario
            .get("call")
            .and_then(Value::as_str)
            .unwrap_or("search_analytics");
        let actual = match call {
            "search_analytics" => {
                gsc::search_analytics(
                    &host,
                    &root,
                    site_url,
                    &gsc::Analytics {
                        start_date: arg(&args, "start_date").and_then(Value::as_str),
                        end_date: arg(&args, "end_date").and_then(Value::as_str),
                        dimensions: arg(&args, "dimensions").cloned(),
                        row_limit: arg(&args, "row_limit")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!(1000)),
                        token,
                        now,
                        service_account: None,
                    },
                    &transport,
                )
                .await
            }
            "inspect_url" => {
                gsc::inspect_url(
                    &host,
                    &root,
                    site_url,
                    args.get("inspection_url")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    token,
                    None,
                    &transport,
                )
                .await
            }
            "discover_properties" => {
                gsc::discover_properties(&host, &root, token, None, &transport).await
            }
            "search_analytics_pages" => {
                search_analytics_pages(
                    &host,
                    &root,
                    site_url,
                    &PagesArgs {
                        start_date: arg(&args, "start_date").and_then(Value::as_str),
                        end_date: arg(&args, "end_date").and_then(Value::as_str),
                        dimensions: arg(&args, "dimensions").cloned(),
                        row_limit: arg(&args, "row_limit")
                            .and_then(Value::as_i64)
                            .unwrap_or(1000),
                        max_rows: arg(&args, "max_rows")
                            .and_then(Value::as_i64)
                            .unwrap_or(gsc::MAX_ANALYTICS_ROWS),
                        token,
                        now,
                        service_account: None,
                    },
                    &transport,
                )
                .await
            }
            "search_analytics_page" => {
                search_analytics_page(
                    &host,
                    &root,
                    site_url,
                    &PageArgs {
                        start_date: arg(&args, "start_date").and_then(Value::as_str),
                        end_date: arg(&args, "end_date").and_then(Value::as_str),
                        dimensions: arg(&args, "dimensions").cloned(),
                        search_type: args.get("search_type"),
                        aggregation_type: args.get("aggregation_type"),
                        data_state: args.get("data_state"),
                        row_limit: args.get("row_limit"),
                        start_row: args.get("start_row"),
                        dimension_filter_groups: arg(&args, "dimension_filter_groups").cloned(),
                        token,
                        service_account: None,
                    },
                    &transport,
                )
                .await
            }
            "inspect_urls" => {
                inspect_urls(
                    &host,
                    &root,
                    site_url,
                    arg(&args, "urls")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice),
                    token,
                    None,
                    &transport,
                )
                .await
            }
            "sitemap_status" => {
                gsc::sitemap_status(&host, &root, site_url, token, None, &transport).await
            }
            other => panic!("{name}: unknown call {other}"),
        }
        .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
    }
}
