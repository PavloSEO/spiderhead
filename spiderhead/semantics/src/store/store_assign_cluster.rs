//! Store helpers: cluster assignment, audited landing-URL updates, SQL value
//! conversion, spend totals and the store directory.

use super::*;

impl Store {
    /// Assign a phrase to a cluster.
    pub fn assign_cluster(&self, norm: &str, cluster_id: i64) -> Result<(), crate::Error> {
        self.exec(
            "UPDATE phrases SET cluster_id=?, last_update=? WHERE norm=?",
            &[json!(cluster_id), json!(local_seconds()), json!(norm)],
        )?;
        Ok(())
    }

    /// Set the cluster landing URL and write an audit record.
    pub fn set_landing(&self, cluster_id: i64, url: Option<&str>) -> Result<(), crate::Error> {
        let before = self.q1(
            "SELECT landing_url FROM clusters WHERE id=?",
            &[json!(cluster_id)],
        )?;
        let Some(before) = before else {
            return Err(crate::Error::Config(format!(
                "unknown cluster id: {cluster_id}"
            )));
        };
        self.exec(
            "UPDATE clusters SET landing_url=? WHERE id=?",
            &[json!(url), json!(cluster_id)],
        )?;
        self.exec(
            "INSERT INTO semantic_audit(at,action,entity,before_json,after_json) VALUES(?,?,?,?,?)",
            &[
                json!(zulu_seconds()),
                json!("set_landing"),
                json!(format!("cluster:{cluster_id}")),
                json!(dumps(&json!({"landing_url": before.get("landing_url").cloned().unwrap_or(Value::Null)}))),
                json!(dumps(&json!({"landing_url": url}))),
            ],
        )?;
        Ok(())
    }

    /// Journal a stage run in `running` state; returns its id.
    pub fn start_stage(&self, stage: &str, estimate: Option<&Value>) -> Result<i64, crate::Error> {
        let est = estimate.map(dumps);
        self.exec(
            "INSERT INTO semantic_stage_runs(stage,state,started_at,estimate_json) \
             VALUES(?,'running',?,?)",
            &[json!(stage), json!(zulu_seconds()), json!(est)],
        )?;
        self.commit_soft();
        Ok(self.conn.last_insert_rowid())
    }

    /// Write the terminal `done` or `failed` state with result or error.
    pub fn finish_stage(
        &self,
        run_id: i64,
        state: &str,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Result<(), crate::Error> {
        if state != "done" && state != "failed" {
            return Err(crate::Error::Config(
                "terminal stage state must be done or failed".into(),
            ));
        }
        let res = result.map(dumps);
        self.exec(
            "UPDATE semantic_stage_runs SET state=?,finished_at=?,result_json=?,error=? WHERE id=?",
            &[
                json!(state),
                json!(zulu_seconds()),
                json!(res),
                json!(error),
                json!(run_id),
            ],
        )?;
        self.commit_soft();
        Ok(())
    }

    /// Latest state per stage plus the full history.
    pub fn stage_states(&self, limit: i64) -> Result<Value, crate::Error> {
        let rows = self.q(
            "SELECT id,stage,state,started_at,finished_at,estimate_json,result_json,error \
             FROM semantic_stage_runs ORDER BY id DESC LIMIT ?",
            &[json!(limit + 1)],
        )?;
        let has_more = rows.len() as i64 > limit;
        let rows: Vec<Row> = rows.into_iter().take(limit as usize).collect();
        let mut latest = serde_json::Map::new();
        let mut history = Vec::new();
        for row in rows {
            let mut item = row;
            let mut decode_errors = Vec::new();
            for name in ["estimate_json", "result_json"] {
                let value = item.shift_remove(name).unwrap_or(Value::Null);
                let short = name.trim_end_matches("_json");
                let parsed = match &value {
                    Value::String(s) if !s.is_empty() => {
                        serde_json::from_str(s).unwrap_or(Value::Null)
                    }
                    Value::Null => Value::Null,
                    other => other.clone(),
                };
                if matches!(&value, Value::String(s) if !s.is_empty()) && parsed == Value::Null {
                    decode_errors.push(json!(short));
                }
                item.insert(short.to_string(), parsed);
            }
            if !decode_errors.is_empty() {
                item.insert("unavailable_fields".into(), Value::Array(decode_errors));
            }
            let last = if !item["finished_at"].is_null() {
                item["finished_at"].clone()
            } else {
                item["started_at"].clone()
            };
            item.insert("last_run_at".into(), last);
            item.shift_remove("id");
            let stage = item["stage"].as_str().unwrap_or_default().to_string();
            if !latest.contains_key(&stage) {
                latest.insert(stage, Value::Object(item.clone().into_iter().collect()));
            }
            history.push(Value::Object(item.into_iter().collect()));
        }
        let total = self
            .q1("SELECT COUNT(*) AS n FROM semantic_stage_runs", &[])?
            .and_then(|r| r["n"].as_i64())
            .unwrap_or(0);
        Ok(json!({
            "latest": Value::Object(latest),
            "history": history,
            "limit": limit,
            "total": total,
            "has_more": has_more,
        }))
    }

    /// Cluster rows joined with phrase stats, paged.
    pub fn cluster_rows(&self, offset: i64, limit: i64) -> Result<Vec<Row>, crate::Error> {
        let columns = self.columns("clusters")?;
        let wanted = ["id", "name", "intent", "source", "coherence", "landing_url"];
        let select = wanted
            .iter()
            .map(|n| {
                if columns.iter().any(|c| c == n) {
                    format!("c.{n} AS {n}")
                } else {
                    format!("NULL AS {n}")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        self.q(
            &format!(
                "SELECT {select},COUNT(p.id) AS phrase_count,SUM(p.base) AS base_sum,\
                 SUM(p.exact) AS exact_sum,COUNT(p.exact) AS exact_measured \
                 FROM clusters c LEFT JOIN phrases p ON p.cluster_id=c.id \
                 AND p.status IN ('kept','review') GROUP BY c.id ORDER BY c.id LIMIT ? OFFSET ?"
            ),
            &[json!(limit), json!(offset)],
        )
    }

    /// One cluster row.
    pub fn cluster_row(&self, cluster_id: i64) -> Result<Option<Row>, crate::Error> {
        let columns = self.columns("clusters")?;
        let wanted = ["id", "name", "intent", "source", "coherence", "landing_url"];
        let select = wanted
            .iter()
            .map(|n| {
                if columns.iter().any(|c| c == n) {
                    format!("c.{n} AS {n}")
                } else {
                    format!("NULL AS {n}")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        self.q1(
            &format!(
                "SELECT {select},COUNT(p.id) AS phrase_count,SUM(p.base) AS base_sum,\
                 SUM(p.exact) AS exact_sum,COUNT(p.exact) AS exact_measured \
                 FROM clusters c LEFT JOIN phrases p ON p.cluster_id=c.id \
                 AND p.status IN ('kept','review') WHERE c.id=? GROUP BY c.id"
            ),
            &[json!(cluster_id)],
        )
    }

    /// Rebuild the `domains` table from SERP rows.
    /// `is_hub` is injected so the cluster stage keeps owning the hub list.
    pub fn refresh_domains(
        &self,
        own_domain: Option<&str>,
        is_hub: &dyn Fn(&str) -> bool,
    ) -> Result<usize, crate::Error> {
        self.exec("DELETE FROM domains", &[])?;
        let rows = self.q(
            "SELECT domain, COUNT(DISTINCT phrase_norm) n, AVG(pos) ap, MIN(pos) bp \
             FROM serp WHERE domain IS NOT NULL AND domain!='' GROUP BY domain",
            &[],
        )?;
        let own = own_domain
            .unwrap_or("")
            .to_lowercase()
            .trim_start_matches("www.")
            .to_string();
        for r in &rows {
            let dom = r["domain"].as_str().unwrap_or_default().to_string();
            let kind = if !own.is_empty() && (dom == own || dom.ends_with(&format!(".{own}"))) {
                "own"
            } else if is_hub(&dom) {
                "hub"
            } else {
                "competitor"
            };
            let ap = r["ap"].as_f64().unwrap_or(0.0);
            self.exec(
                "INSERT OR REPLACE INTO domains(domain,n_queries,avg_pos,best_pos,kind) \
                 VALUES(?,?,?,?,?)",
                &[
                    json!(dom),
                    r["n"].clone(),
                    json!(crate::pyfmt::py_round(ap, 2)),
                    r["bp"].clone(),
                    json!(kind),
                ],
            )?;
        }
        self.commit_soft();
        Ok(rows.len())
    }

    /// Whether a seed phrase has been expanded.
    pub fn is_expanded(&self, seed_norm: &str) -> Result<bool, crate::Error> {
        Ok(self
            .q1(
                "SELECT 1 AS x FROM expansions WHERE seed_norm=?",
                &[json!(seed_norm)],
            )?
            .is_some())
    }

    /// Mark a seed phrase as expanded.
    pub fn mark_expanded(
        &self,
        seed_norm: &str,
        results_n: i64,
        assoc_n: i64,
        region: &str,
    ) -> Result<(), crate::Error> {
        self.exec(
            "INSERT OR REPLACE INTO expansions(seed_norm,expanded_at,results_n,assoc_n,region) \
             VALUES(?,?,?,?,?)",
            &[
                json!(seed_norm),
                json!(local_seconds()),
                json!(results_n),
                json!(assoc_n),
                json!(region),
            ],
        )?;
        Ok(())
    }

    /// Phrases filtered by status, ordered by base frequency, highest first.
    pub fn phrases(
        &self,
        status: Option<&str>,
        needs_exact: bool,
        limit: Option<i64>,
    ) -> Result<Vec<Row>, crate::Error> {
        let mut q = "SELECT * FROM phrases".to_string();
        let mut cond = Vec::new();
        let mut args = Vec::new();
        if let Some(s) = status {
            cond.push("status=?");
            args.push(json!(s));
        }
        if needs_exact {
            cond.push("exact IS NULL");
        }
        if !cond.is_empty() {
            q.push_str(" WHERE ");
            q.push_str(&cond.join(" AND "));
        }
        q.push_str(" ORDER BY base DESC");
        if let Some(l) = limit {
            q.push_str(&format!(" LIMIT {}", l.max(0)));
        }
        self.q(&q, &args)
    }

    /// Kept or new phrases above `floor` that are not expanded yet.
    pub fn reexpand_candidates(&self, floor: i64) -> Result<Vec<String>, crate::Error> {
        Ok(self
            .q(
                "SELECT norm FROM phrases WHERE base>=? AND status IN ('new','kept') \
                 AND norm NOT IN (SELECT seed_norm FROM expansions) ORDER BY base DESC",
                &[json!(floor)],
            )?
            .into_iter()
            .map(|r| r["norm"].as_str().unwrap_or_default().to_string())
            .collect())
    }

    /// SERP rows of a phrase, ordered by position.
    pub fn serp_for(&self, phrase_norm: &str) -> Result<Vec<Row>, crate::Error> {
        self.q(
            "SELECT url,domain,pos FROM serp WHERE phrase_norm=? ORDER BY pos",
            &[json!(phrase_norm)],
        )
    }

    /// Replace the SERP rows of a phrase.
    pub fn add_serp(
        &self,
        phrase_norm: &str,
        docs: &[Row],
        fetched_at: Option<&str>,
    ) -> Result<(), crate::Error> {
        self.exec(
            "DELETE FROM serp WHERE phrase_norm=?",
            &[json!(phrase_norm)],
        )?;
        let ts = fetched_at
            .map(|s| s.to_string())
            .unwrap_or_else(local_seconds);
        for d in docs {
            self.exec(
                "INSERT INTO serp(phrase_norm,pos,url,domain,title,fetched_at) VALUES(?,?,?,?,?,?)",
                &[
                    json!(phrase_norm),
                    d.get("pos").cloned().unwrap_or(Value::Null),
                    d.get("url").cloned().unwrap_or(Value::Null),
                    d.get("domain").cloned().unwrap_or(Value::Null),
                    d.get("title").cloned().unwrap_or(Value::Null),
                    json!(ts),
                ],
            )?;
        }
        Ok(())
    }

    /// Journal provider units for a run.
    pub fn add_run(
        &self,
        stage: &str,
        n_requests: i64,
        cost_arsenkin: i64,
        note: &str,
    ) -> Result<(), crate::Error> {
        self.add_run_value(stage, n_requests, &json!(cost_arsenkin), note)
    }

    /// `add_run` with a provider-reported cost (receipts can carry floats).
    pub fn add_run_value(
        &self,
        stage: &str,
        n_requests: i64,
        cost_arsenkin: &Value,
        note: &str,
    ) -> Result<(), crate::Error> {
        self.exec(
            "INSERT INTO runs(ts,stage,n_requests,cost_rub,cost_arsenkin,note) VALUES(?,?,?,?,?,?)",
            &[
                json!(local_seconds()),
                json!(stage),
                json!(n_requests),
                Value::Null,
                cost_arsenkin.clone(),
                json!(note),
            ],
        )?;
        self.commit_soft();
        Ok(())
    }

    /// Spend totals across recorded runs: Yandex requests and Arsenkin cost.
    pub fn spend(&self) -> Result<Value, crate::Error> {
        let r = self.q1(
            "SELECT COALESCE(SUM(CASE WHEN stage IN ('collect','synonyms','cluster') \
             THEN n_requests END),0) req, COALESCE(SUM(cost_arsenkin),0) lim FROM runs",
            &[],
        )?;
        Ok(json!({
            "yandex_requests": r.as_ref().map(|x| &x["req"]).cloned().unwrap_or(json!(0)),
            "arsenkin": r.as_ref().map(|x| &x["lim"]).cloned().unwrap_or(json!(0)),
        }))
    }

    /// Status counts plus totals for exact, expanded and clusters.
    pub fn counts(&self) -> Result<IndexMap<String, i64>, crate::Error> {
        let mut out = IndexMap::new();
        for r in self.q(
            "SELECT status, COUNT(*) n FROM phrases GROUP BY status",
            &[],
        )? {
            let k = r["status"].as_str().unwrap_or_default().to_string();
            out.insert(k, r["n"].as_i64().unwrap_or(0));
        }
        for (key, sql) in [
            ("_total", "SELECT COUNT(*) n FROM phrases"),
            (
                "_exact",
                "SELECT COUNT(*) n FROM phrases WHERE exact IS NOT NULL",
            ),
            ("_expanded", "SELECT COUNT(*) n FROM expansions"),
            ("_clusters", "SELECT COUNT(*) n FROM clusters"),
        ] {
            let n = self
                .q1(sql, &[])?
                .and_then(|r| r["n"].as_i64())
                .unwrap_or(0);
            out.insert(key.into(), n);
        }
        Ok(out)
    }
}

// ---------- export lives in store_export.rs ----------

/// JSON → SQLite parameter binding (numbers, strings, bools, null only).
pub(super) fn to_sql(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        Value::Null => V::Null,
        Value::Bool(b) => V::Integer(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                V::Integer(i)
            } else {
                V::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => V::Text(s.clone()),
        other => V::Text(other.to_string()),
    }
}

/// Directory of the store's database file.
pub fn dir_of(store: &Store) -> PathBuf {
    Path::new(&store.path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
}
