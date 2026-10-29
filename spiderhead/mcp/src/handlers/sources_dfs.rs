//! Google demand and SERP handlers over DataForSEO (the reference suite):
//! the offline price quote, keyword volume/ideas/difficulty and live SERP.

use serde_json::{json, Map, Value};

use spiderhead_sources::dataforseo::{
    estimate, keyword_difficulty, keyword_ideas, search_volume, serp, Client, ClientArgs, DfsEnv,
    EstimateArgs, GeoArgs, IdeasArgs, SerpArgs,
};

use crate::error::ToolError;

use super::sources_net::{environment, fail, present, run, transport};
use super::{kwargs, opt, opt_bool};

/// `dataforseo_estimate`: quote supplied prices offline, or read the free
/// account price list first when `refresh_prices` is set.
pub fn dataforseo_estimate(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let (host, root) = environment();
    let transport = transport()?;
    let env = DfsEnv::live(&host, &root, &transport);
    let Some(operation) = opt(&args, "operation") else {
        return Err(super::missing_option(
            "dataforseo-estimate",
            "operation",
            &[
                "operation",
                "query_count",
                "row_limit",
                "cache_hits",
                "prices",
                "env",
                "refresh_prices",
                "query",
            ],
        ));
    };
    let mut prices = opt(&args, "prices").cloned();
    if opt_bool(&args, "refresh_prices").unwrap_or(false) {
        let client = Client::new(
            &env,
            &ClientArgs {
                env_name: opt(&args, "env"),
                ..ClientArgs::default()
            },
        );
        let balance = match client {
            Ok(client) => run(client.balance())?,
            Err(error) => Err(error),
        };
        match balance {
            Ok(account) => prices = account.get("prices").cloned(),
            Err(error) => {
                return Ok(json!({
                    "ok": false,
                    "state": "unavailable",
                    "reason": "free account price lookup failed",
                    "error": error.to_string(),
                }))
            }
        }
    }
    let quote = estimate(
        &env,
        operation,
        &EstimateArgs {
            query_count: opt(&args, "query_count"),
            row_limit: opt(&args, "row_limit"),
            cache_hits: opt(&args, "cache_hits"),
            prices: prices.as_ref(),
            env: opt(&args, "env"),
            query: opt(&args, "query"),
        },
    )
    .map_err(fail)?;
    let complete = quote.get("state") == Some(&json!("complete"));
    let mut out = Map::new();
    out.insert("ok".to_owned(), json!(complete));
    if let Value::Object(fields) = quote {
        out.extend(fields);
    }
    Ok(Value::Object(out))
}

fn geo<'a>(args: &'a Map<String, Value>, prices: Option<&'a Value>) -> GeoArgs<'a> {
    GeoArgs {
        location_code: opt(args, "location_code"),
        language: opt(args, "language"),
        country: opt(args, "country"),
        env: opt(args, "env"),
        confirm_paid: opt(args, "confirm_paid"),
        spend_ceiling_usd: opt(args, "spend_ceiling_usd"),
        prices,
    }
}

/// `google_keywords`: Google volume for a keyword list, or ideas for a seed.
pub fn google_keywords(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let (host, root) = environment();
    let transport = transport()?;
    let env = DfsEnv::live(&host, &root, &transport);
    let prices = opt(&args, "prices");
    let ceiling = opt(&args, "spend_ceiling_usd");
    let seed = present(&args, "seed");
    let difficulty = opt_bool(&args, "difficulty").unwrap_or(false);
    if let (Some(_), true, Some(ceiling_value)) = (seed, difficulty, ceiling) {
        let mut quotes = Vec::new();
        for operation in ["keyword_ideas", "keyword_difficulty"] {
            quotes.push(
                estimate(
                    &env,
                    &json!(operation),
                    &EstimateArgs {
                        row_limit: opt(&args, "limit"),
                        prices,
                        env: opt(&args, "env"),
                        ..EstimateArgs::default()
                    },
                )
                .map_err(fail)?,
            );
        }
        let costs: Vec<Option<f64>> = quotes
            .iter()
            .map(|quote| quote.get("cost_usd").and_then(Value::as_f64))
            .collect();
        if costs.iter().any(Option::is_none) {
            return Ok(json!({
                "ok": false,
                "state": "unavailable",
                "reason": "both operation prices are required to enforce the combined ceiling",
                "estimate": quotes,
            }));
        }
        let total: f64 = costs.iter().flatten().sum();
        if total > ceiling_value.as_f64().unwrap_or(f64::INFINITY) {
            return Ok(json!({
                "ok": false,
                "state": "spend_limit_exceeded",
                "reason": "combined expansion and difficulty estimate exceeds ceiling before request",
                "estimate": quotes,
            }));
        }
    }
    if let Some(seed) = seed {
        let g = geo(&args, prices);
        let mut ideas = run(keyword_ideas(
            &env,
            &json!(seed),
            &IdeasArgs {
                location_code: g.location_code,
                language: g.language,
                limit: opt(&args, "limit"),
                country: g.country,
                env: g.env,
                confirm_paid: g.confirm_paid,
                spend_ceiling_usd: g.spend_ceiling_usd,
                prices: g.prices,
            },
        ))?
        .map_err(fail)?;
        if !difficulty || ideas.get("ok") != Some(&json!(true)) {
            return Ok(ideas);
        }
        let expanded: Vec<Value> = ideas
            .get("keywords")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.get("phrase"))
                    .filter(|phrase| truthy(phrase))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if expanded.is_empty() {
            return Ok(ideas);
        }
        let mut remaining = ceiling.cloned();
        if let Some(limit) = ceiling.and_then(Value::as_f64) {
            let Some(actual) = ideas.get("cost_usd").and_then(Value::as_f64) else {
                let mut partial = ideas.clone();
                partial["ok"] = json!(false);
                partial["state"] = json!("partial");
                partial["reason"] = json!(
                    "expansion cost is unknown; cannot enforce remaining ceiling for difficulty"
                );
                return Ok(partial);
            };
            remaining = Some(json!((limit - actual).max(0.0)));
        }
        let mut scored_geo = geo(&args, prices);
        scored_geo.spend_ceiling_usd = remaining.as_ref();
        let scored = run(keyword_difficulty(
            &env,
            &Value::Array(expanded),
            &scored_geo,
        ))?
        .map_err(fail)?;
        if scored.get("ok") == Some(&json!(false)) {
            ideas["ok"] = json!(false);
        }
        ideas["difficulty"] = scored;
        return Ok(ideas);
    }
    let Some(keywords) = opt(&args, "keywords").filter(|value| truthy(value)) else {
        return Err(fail("keywords or seed required"));
    };
    let g = geo(&args, prices);
    if difficulty {
        run(keyword_difficulty(&env, keywords, &g))?.map_err(fail)
    } else {
        run(search_volume(&env, keywords, &g))?.map_err(fail)
    }
}

/// `google_serp`: Google organic results for a query in one market.
pub fn google_serp(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(query) = present(&args, "query") else {
        return Err(fail("query required"));
    };
    let (host, root) = environment();
    let transport = transport()?;
    let env = DfsEnv::live(&host, &root, &transport);
    run(serp(
        &env,
        &json!(query),
        &SerpArgs {
            location_code: opt(&args, "location_code"),
            language: opt(&args, "language"),
            depth: opt(&args, "depth"),
            country: opt(&args, "country"),
            env: opt(&args, "env"),
            confirm_paid: opt(&args, "confirm_paid"),
            spend_ceiling_usd: opt(&args, "spend_ceiling_usd"),
            prices: opt(&args, "prices"),
        },
    ))?
    .map_err(fail)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}
