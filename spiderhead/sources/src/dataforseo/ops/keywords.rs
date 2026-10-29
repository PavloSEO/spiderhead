//! Keyword, ideas and SERP operation wrappers with their geo and argument types.
use super::*;

/// Geo + gate keyword parameters shared by the Google wrappers.
#[derive(Debug, Default)]
pub struct GeoArgs<'a> {
    /// `location_code` (`2840` — the United States).
    pub location_code: Option<&'a Value>,
    /// `language` (`"en"` → `language_code`).
    pub language: Option<&'a Value>,
    /// `country` — advisory text for `geo_guard`.
    pub country: Option<&'a Value>,
    /// `env`.
    pub env: Option<&'a Value>,
    /// `confirm_paid`.
    pub confirm_paid: Option<&'a Value>,
    /// `spend_ceiling_usd`.
    pub spend_ceiling_usd: Option<&'a Value>,
    /// `prices` snapshot.
    pub prices: Option<&'a Value>,
}

/// `keyword_ideas` adds `limit`.
#[derive(Debug, Default)]
pub struct IdeasArgs<'a> {
    /// `location_code`.
    pub location_code: Option<&'a Value>,
    /// `language`.
    pub language: Option<&'a Value>,
    /// `limit` (`100`), converted to an integer for the payload; the raw value feeds `row_limit`.
    pub limit: Option<&'a Value>,
    /// `country`.
    pub country: Option<&'a Value>,
    /// `env`.
    pub env: Option<&'a Value>,
    /// `confirm_paid`.
    pub confirm_paid: Option<&'a Value>,
    /// `spend_ceiling_usd`.
    pub spend_ceiling_usd: Option<&'a Value>,
    /// `prices`.
    pub prices: Option<&'a Value>,
}

/// `serp` adds `depth`.
#[derive(Debug, Default)]
pub struct SerpArgs<'a> {
    /// `location_code`.
    pub location_code: Option<&'a Value>,
    /// `language`.
    pub language: Option<&'a Value>,
    /// `depth` (`10`, bounded 1–200).
    pub depth: Option<&'a Value>,
    /// `country`.
    pub country: Option<&'a Value>,
    /// `env`.
    pub env: Option<&'a Value>,
    /// `confirm_paid`.
    pub confirm_paid: Option<&'a Value>,
    /// `spend_ceiling_usd`.
    pub spend_ceiling_usd: Option<&'a Value>,
    /// `prices`.
    pub prices: Option<&'a Value>,
}

pub(super) fn geo_gate<'a>(args: &'a GeoArgs<'a>) -> GateArgs<'a> {
    GateArgs {
        env: args.env,
        confirm_paid: args.confirm_paid,
        spend_ceiling_usd: args.spend_ceiling_usd,
        prices: args.prices,
        row_limit: None,
    }
}

/// Field of a task item; task items are always objects.
pub(super) fn field(item: &Value, key: &str) -> Value {
    item.get(key).cloned().unwrap_or(Value::Null)
}

/// Nested field of a task item; a truthy non-object value is an error.
pub(super) fn nested_get(item: &Value, key: &str, inner: &str) -> Result<Value, DfsError> {
    let v = field(item, key);
    if !pystr::truthy(Some(&v)) {
        return Ok(Value::Null);
    }
    match v.as_object() {
        Some(m) => Ok(m.get(inner).cloned().unwrap_or(Value::Null)),
        None => Err(attr_err(&v, "get")),
    }
}

/// `search_volume(keywords, ...)` — Google search volume for keywords.
pub async fn search_volume<T: Transport>(
    env: &DfsEnv<'_, T>,
    keywords: &Value,
    args: &GeoArgs<'_>,
) -> Result<Value, DfsError> {
    let default_code = json!(2840);
    let default_lang = json!("en");
    let code = args.location_code.unwrap_or(&default_code);
    if let Some(blocked) = geo_guard(args.country, Some(code))? {
        return Ok(blocked);
    }
    let list = py_iter(keywords)?;
    let count = list.len() as i64;
    let row_limit = json!(count);
    let mut gargs = geo_gate(args);
    gargs.row_limit = Some(&row_limit);
    let called = call(
        env,
        "search_volume",
        vec![json!({
            "keywords": list,
            "location_code": code,
            "language_code": args.language.unwrap_or(&default_lang),
        })],
        count,
        &gargs,
    )
    .await?;
    match called {
        Called::Envelope(v) => Ok(v),
        Called::Ran { env_name, outcome } => Ok(json!({
            "ok": !outcome.failed,
            "env": env_name,
            "cost_usd": outcome.cost,
            "errors": outcome.errors,
            "keywords": outcome.items.iter().map(|i| json!({
                "phrase": field(i, "keyword"),
                "volume": field(i, "search_volume"),
                "cpc": field(i, "cpc"),
                "competition": field(i, "competition"),
            })).collect::<Vec<_>>(),
        })),
    }
}

/// `keyword_ideas(seed, ...)` — expand a seed phrase (Google's Wordstat left column).
pub async fn keyword_ideas<T: Transport>(
    env: &DfsEnv<'_, T>,
    seed: &Value,
    args: &IdeasArgs<'_>,
) -> Result<Value, DfsError> {
    let default_code = json!(2840);
    let default_lang = json!("en");
    let default_limit = json!(100);
    let code = args.location_code.unwrap_or(&default_code);
    if let Some(blocked) = geo_guard(args.country, Some(code))? {
        return Ok(blocked);
    }
    let limit = args.limit.unwrap_or(&default_limit);
    let limit_int = pystr::to_python_int(limit).map_err(|(c, m)| pyerr(c, m))?;
    let called = call(
        env,
        "keyword_ideas",
        vec![json!({
            "keywords": [seed],
            "location_code": code,
            "language_code": args.language.unwrap_or(&default_lang),
            "limit": limit_int,
        })],
        1,
        &GateArgs {
            env: args.env,
            confirm_paid: args.confirm_paid,
            spend_ceiling_usd: args.spend_ceiling_usd,
            prices: args.prices,
            row_limit: Some(limit), // the raw kwarg — `estimate` validates ints itself.
        },
    )
    .await?;
    match called {
        Called::Envelope(v) => Ok(v),
        Called::Ran { env_name, outcome } => {
            let mut keywords = Vec::new();
            for i in &outcome.items {
                keywords.push(json!({
                    "phrase": field(i, "keyword"),
                    "volume": nested_get(i, "keyword_info", "search_volume")?,
                    "difficulty": nested_get(i, "keyword_properties", "keyword_difficulty")?,
                }));
            }
            Ok(json!({
                "ok": !outcome.failed,
                "env": env_name,
                "seed": seed,
                "cost_usd": outcome.cost,
                "errors": outcome.errors,
                "found": outcome.items.len(),
                "keywords": keywords,
            }))
        }
    }
}

/// `keyword_difficulty(keywords, ...)` — bulk difficulty estimate.
pub async fn keyword_difficulty<T: Transport>(
    env: &DfsEnv<'_, T>,
    keywords: &Value,
    args: &GeoArgs<'_>,
) -> Result<Value, DfsError> {
    let default_code = json!(2840);
    let default_lang = json!("en");
    let code = args.location_code.unwrap_or(&default_code);
    if let Some(blocked) = geo_guard(args.country, Some(code))? {
        return Ok(blocked);
    }
    let list = py_iter(keywords)?;
    let count = list.len() as i64;
    let row_limit = json!(count);
    let mut gargs = geo_gate(args);
    gargs.row_limit = Some(&row_limit);
    let called = call(
        env,
        "keyword_difficulty",
        vec![json!({
            "keywords": list,
            "location_code": code,
            "language_code": args.language.unwrap_or(&default_lang),
        })],
        count,
        &gargs,
    )
    .await?;
    match called {
        Called::Envelope(v) => Ok(v),
        Called::Ran { env_name, outcome } => Ok(json!({
            "ok": !outcome.failed,
            "env": env_name,
            "cost_usd": outcome.cost,
            "errors": outcome.errors,
            "keywords": outcome.items.iter().map(|i| json!({
                "phrase": field(i, "keyword"),
                "difficulty": field(i, "keyword_difficulty"),
            })).collect::<Vec<_>>(),
        })),
    }
}

/// `serp(query, ...)` — live Google organic results for a query.
pub async fn serp<T: Transport>(
    env: &DfsEnv<'_, T>,
    query: &Value,
    args: &SerpArgs<'_>,
) -> Result<Value, DfsError> {
    let default_code = json!(2840);
    let default_lang = json!("en");
    let default_depth = json!(10);
    let depth = args.depth.unwrap_or(&default_depth);
    let depth_int = match pystr::as_strict_i64(Some(depth)) {
        Some(d) if (1..=200).contains(&d) => d,
        _ => return Err(value_err("depth must be between 1 and 200")),
    };
    let code = args.location_code.unwrap_or(&default_code);
    if let Some(blocked) = geo_guard(args.country, Some(code))? {
        return Ok(blocked);
    }
    let called = call(
        env,
        "serp",
        vec![json!({
            "keyword": query,
            "location_code": code,
            "language_code": args.language.unwrap_or(&default_lang),
            "depth": depth_int,
        })],
        1,
        &GateArgs {
            env: args.env,
            confirm_paid: args.confirm_paid,
            spend_ceiling_usd: args.spend_ceiling_usd,
            prices: args.prices,
            row_limit: Some(depth),
        },
    )
    .await?;
    match called {
        Called::Envelope(v) => Ok(v),
        Called::Ran { env_name, outcome } => {
            let organic: Vec<&Value> = outcome
                .items
                .iter()
                .filter(|i| i.get("type") == Some(&json!("organic")))
                .collect();
            let rows: Vec<&Value> = if organic.is_empty() {
                outcome.items.iter().collect()
            } else {
                organic
            };
            Ok(json!({
                "ok": !outcome.failed,
                "env": env_name,
                "query": query,
                "cost_usd": outcome.cost,
                "errors": outcome.errors,
                "docs": rows.iter().map(|&i| json!({
                    "pos": field(i, "rank_absolute"),
                    "url": field(i, "url"),
                    "domain": field(i, "domain"),
                    "title": field(i, "title"),
                })).collect::<Vec<_>>(),
            }))
        }
    }
}
