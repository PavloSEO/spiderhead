//! Split out of `metrika_traffic`.

use super::*;

/// `_Collector` — the shared query state: request counters, failures, and the
/// `auth_error` that short-circuits later queries.
pub(super) struct Collector<'x, 'c> {
    /// The injected `client.report` seam.
    pub(super) client: &'x ReportCall<'c>,
    /// The normalized request.
    pub(super) request: &'x Request,
    /// Resolved attribution roles `(source, engine, phrase)`.
    pub(super) roles: (&'static str, &'static str, &'static str),
    /// The `filters` expression the API sees, or `None`.
    pub(super) traffic_filter: Option<String>,
    /// Report calls issued.
    pub(super) requests: i64,
    /// `failures` — every `Metrica N: message` reason seen.
    pub(super) failures: Vec<String>,
    /// A 401/403 reason that short-circuits the rest of the collection.
    pub(super) auth_error: Option<String>,
}

impl<'x, 'c> Collector<'x, 'c> {
    pub(super) fn new(client: &'x ReportCall<'c>, request: &'x Request) -> Self {
        let roles = match request.attribution.as_str() {
            "last_click" => (
                "ym:s:lastTrafficSource",
                "ym:s:lastSearchEngineRoot",
                "ym:s:lastSearchPhrase",
            ),
            _ => (
                "ym:s:lastSignTrafficSource",
                "ym:s:lastSignSearchEngineRoot",
                "ym:s:lastSignSearchPhrase",
            ),
        };
        let mut parts: Vec<String> = Vec::new();
        if request.traffic == "organic" {
            parts.push(format!("{}=='organic'", roles.0));
        }
        if let Some(filters) = &request.filters {
            parts.push(format!("({filters})"));
        }
        Collector {
            client,
            request,
            roles,
            traffic_filter: if parts.is_empty() {
                None
            } else {
                Some(parts.join(" AND "))
            },
            requests: 0,
            failures: Vec::new(),
            auth_error: None,
        }
    }

    /// `collector.dimension(spec)` — `@role` → the attribution dimension.
    pub(super) fn dimension(&self, spec: &str) -> String {
        match spec.strip_prefix('@') {
            Some("source") => self.roles.0.to_owned(),
            Some("engine") => self.roles.1.to_owned(),
            Some("phrase") => self.roles.2.to_owned(),
            _ => spec.to_owned(),
        }
    }

    /// `collector.query` — one `client.report` call with the shared params.
    pub(super) async fn query(
        &mut self,
        period: (i64, i64),
        metrics: &[&str],
        dimension: Option<&str>,
        filtered: bool,
        sort: Option<&str>,
        limit: i64,
    ) -> Result<Value, CollectErr> {
        if let Some(reason) = &self.auth_error {
            return Err(CollectErr::Unavailable(reason.clone()));
        }
        let mut params = Map::new();
        params.insert("ids".to_owned(), json!(self.request.ids.join(",")));
        params.insert("date1".to_owned(), json!(date_iso(period.0)));
        params.insert("date2".to_owned(), json!(date_iso(period.1)));
        params.insert("metrics".to_owned(), json!(metrics.join(",")));
        params.insert("lang".to_owned(), json!(self.request.lang));
        if let Some(dimension) = dimension {
            params.insert("dimensions".to_owned(), json!(dimension));
        }
        if filtered {
            if let Some(filter) = &self.traffic_filter {
                params.insert("filters".to_owned(), json!(filter));
            }
        }
        if let Some(sort) = sort {
            params.insert("sort".to_owned(), json!(sort));
        }
        self.requests += 1;
        match (self.client)(params, limit, API_OFFSET).await {
            Err(MetrikaFail::Metrika(exc)) => {
                let reason = exc.to_string();
                if matches!(exc.status, 401 | 403) {
                    self.auth_error = Some(reason.clone());
                }
                self.failures.push(reason.clone());
                Err(CollectErr::Unavailable(reason))
            }
            Err(other) => Err(CollectErr::Fail(other)),
            Ok(body) if body.is_object() => Ok(body),
            Ok(_) => Err(CollectErr::Unavailable(
                "Metrica returned a malformed response".to_owned(),
            )),
        }
    }

    /// `collector.totals` — the `totals` vector as `_number` cells.
    pub(super) async fn totals(
        &mut self,
        period: (i64, i64),
        metrics: &[&str],
    ) -> Result<Vec<Option<f64>>, CollectErr> {
        let body = self.query(period, metrics, None, true, None, 1).await?;
        let totals = py_get(&body, "totals").map_err(CollectErr::Fail)?;
        match totals {
            Some(Value::Array(items)) if items.len() == metrics.len() => {
                Ok(items.iter().map(|value| number(Some(value))).collect())
            }
            _ => Err(CollectErr::Unavailable(
                "Metrica returned no totals for this query".to_owned(),
            )),
        }
    }

    /// `collector.rows` — `(rows, total, complete)` for a one-dimension breakdown.
    pub(super) async fn rows(
        &mut self,
        period: (i64, i64),
        dimension: &str,
        filtered: bool,
        limit: i64,
        sort: &str,
    ) -> Result<(Vec<Map<String, Value>>, Option<f64>, bool), CollectErr> {
        let body = self
            .query(
                period,
                &["ym:s:visits"],
                Some(dimension),
                filtered,
                Some(sort),
                limit,
            )
            .await?;
        let data = py_get(&body, "data").map_err(CollectErr::Fail)?;
        let items = match data {
            Some(Value::Array(items)) => items.clone(),
            _ => {
                return Err(CollectErr::Unavailable(
                    "Metrica returned a malformed response".to_owned(),
                ));
            }
        };
        let mut rows = Vec::new();
        for item in &items {
            let dimensions = match or_get(item, "dimensions").map_err(CollectErr::Fail)? {
                Some(v) if pystr::truthy(Some(v)) => v.clone(),
                _ => json!([{}]),
            };
            let first = py_subscript(&dimensions, 0).map_err(CollectErr::Fail)?;
            let first = match first {
                Value::Object(_) => first,
                other => json!({"name": other}),
            };
            let name = first.get("name").cloned().unwrap_or(Value::Null);
            let row_id = first.get("id").cloned().unwrap_or(Value::Null);
            let metrics = match or_get(item, "metrics").map_err(CollectErr::Fail)? {
                Some(v) if pystr::truthy(Some(v)) => v.clone(),
                _ => json!([null]),
            };
            let cell = py_subscript(&metrics, 0).map_err(CollectErr::Fail)?;
            let key = if py_eq(&row_id, &Value::Null) || py_eq(&row_id, &json!("")) {
                py_str(&name)
            } else {
                py_str(&row_id)
            };
            let mut row = Map::new();
            row.insert("key".to_owned(), json!(key));
            row.insert(
                "id".to_owned(),
                match row_id {
                    Value::Null => Value::Null,
                    other => json!(py_str(&other)),
                },
            );
            row.insert(
                "name".to_owned(),
                match name {
                    Value::Null => Value::Null,
                    other => json!(py_str(&other)),
                },
            );
            row.insert(
                "visits".to_owned(),
                json!(number(Some(&cell)).unwrap_or(0.0)),
            );
            rows.push(row);
        }
        let totals = py_get(&body, "totals").map_err(CollectErr::Fail)?;
        let total = match totals {
            Some(Value::Array(items)) if !items.is_empty() => number(items.first()),
            _ => None,
        };
        let total_rows = py_get(&body, "total_rows").map_err(CollectErr::Fail)?;
        // `not isinstance(total_rows, int) or total_rows <= len(rows)` — bool is int.
        let complete = match total_rows {
            Some(Value::Bool(b)) => i64::from(*b) <= rows.len() as i64,
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => {
                n.as_i64().unwrap_or(i64::MAX) <= rows.len() as i64
            }
            _ => true,
        };
        Ok((rows, total, complete))
    }
}
