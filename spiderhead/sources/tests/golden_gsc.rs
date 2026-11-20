//! Golden-diff test for the GSC client against `gsc_oracle.json`.
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
async fn golden_gsc() {
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
                        dimension_filter_groups: arg(&args, "dimension_filter_groups").cloned(),
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
        .map_err(|e| (e.error_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
    }
}

// `search_analytics` forwards `dimension_filter_groups` into the request payload
// verbatim and omits the member entirely when no groups are supplied (issue #54:
// one page filter narrows a property-week from 4 aggregated rows to the 139
// filtered ones, so the payload must reach Google unchanged).
#[tokio::test(flavor = "current_thread")]
async fn search_analytics_passes_dimension_filter_groups_through() {
    let groups = serde_json::json!([
        {
            "filters": [
                {
                    "dimension": "page",
                    "operator": "equals",
                    "expression": "https://example.com/page/",
                }
            ]
        }
    ]);
    let token = serde_json::json!("fixture-token");
    let build_args = |filter: Option<Value>| gsc::Analytics {
        start_date: Some("2026-01-01"),
        end_date: Some("2026-01-28"),
        dimensions: Some(serde_json::json!(["query", "page"])),
        dimension_filter_groups: filter,
        row_limit: serde_json::json!(1000),
        token: Some(&token),
        now: None,
        service_account: None,
    };
    async fn run(args: &gsc::Analytics<'_>) -> (Value, Vec<Value>) {
        let transport = StubTransport::new(&[serde_json::json!({
            "status": 200,
            "body": {"rows": []},
        })]);
        let (host, root) = scenario_host(&serde_json::json!({}));
        let outcome = gsc::search_analytics(&host, &root, "https://example.com/", args, &transport)
            .await
            .unwrap();
        let recorded = transport.recorded();
        (outcome, recorded)
    }

    let (outcome, recorded) = run(&build_args(Some(groups.clone()))).await;
    assert_eq!(outcome["ok"], serde_json::json!(true));
    assert_eq!(recorded.len(), 1);
    let body: Value = serde_json::from_str(recorded[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["dimensionFilterGroups"], groups);

    let (outcome, recorded) = run(&build_args(None)).await;
    assert_eq!(outcome["ok"], serde_json::json!(true));
    let body: Value = serde_json::from_str(recorded[0]["body"].as_str().unwrap()).unwrap();
    assert!(body.get("dimensionFilterGroups").is_none());
}
