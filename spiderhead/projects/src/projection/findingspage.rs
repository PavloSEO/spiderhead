use super::*;

/// One SQL-bounded findings page with its group sizes.
pub struct FindingsPage {
    /// Total rows under `/issues`.
    pub total: i64,
    /// Rows matching the predicate before paging.
    pub matched: i64,
    /// `(ordinal, issue)` rows for the requested page.
    pub selected: Vec<(i64, Value)>,
    /// Member counts for the selected rows' groups.
    pub group_sizes: BTreeMap<String, i64>,
    /// Group ids that contain the requested member URL.
    pub via_group_ids: BTreeSet<String>,
}

/// SQL-bounded sorting/paging over `/issues`.
pub fn v2_findings_page(
    row: &Map<String, Value>,
    offset: i64,
    limit: i64,
    query: &str,
    url: Option<&str>,
    sort: &str,
    descending: bool,
) -> ProjectsResult<FindingsPage> {
    const FIELDS: [&str; 7] = [
        "id",
        "check",
        "severity",
        "target_url",
        "url",
        "message",
        "text",
    ];
    let path = row
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| value("retained scan row has no path"))?;
    let scan_path = Path::new(path);
    let reader = spiderhead_store::audit_v2::AuditV2Reader::new(scan_path, true, false, None)
        .map_err(ProjectsError::Store)?;
    let companion = reader.path.clone();
    let storage_version = reader.storage_version;
    let total = reader.count("/issues").map_err(ProjectsError::Store)?;
    reader.close();
    let con = open_companion(&companion)?;

    let expression = FIELDS
        .iter()
        .map(|field| format!("COALESCE(json_extract(value_json,'$.{field}'),'')"))
        .collect::<Vec<_>>()
        .join(" || ' ' || ");
    let mut predicate = format!("pointer='/issues' AND instr(casefold({expression}),?)>0");
    let needle = icu_casemap::CaseMapper::new()
        .fold_string(query)
        .to_string()
        .trim()
        .to_string();
    let mut parameters: Vec<rusqlite::types::Value> = vec![needle.into()];
    let mut via_group_ids = BTreeSet::new();
    if let Some(url) = url {
        let target = "COALESCE(NULLIF(json_extract(value_json,'$.target_url'),''),json_extract(value_json,'$.url'))=?";
        via_group_ids = v2_group_ids_including(&con, storage_version, url)?;
        if !via_group_ids.is_empty() {
            let placeholders = vec!["?"; via_group_ids.len()].join(",");
            predicate.push_str(&format!(
                " AND ({target} OR json_extract(value_json,'$.group_id') IN ({placeholders}))"
            ));
            parameters.push(rusqlite::types::Value::Text(url.to_string()));
            parameters.extend(
                via_group_ids
                    .iter()
                    .map(|id| rusqlite::types::Value::Text(id.clone())),
            );
        } else {
            predicate.push_str(&format!(" AND {target}"));
            parameters.push(rusqlite::types::Value::Text(url.to_string()));
        }
    }
    let matched: i64 = con
        .query_row(
            &format!("SELECT COUNT(*) FROM items WHERE {predicate}"),
            rusqlite::params_from_iter(parameters.iter()),
            |row| row.get(0),
        )
        .map_err(|e| value(e.to_string()))?;

    let mut raw_value = format!("json_extract(value_json,'$.{sort}')");
    if sort == "target_url" {
        raw_value = format!("COALESCE(NULLIF({raw_value},''),json_extract(value_json,'$.url'))");
    }
    let value_sql = if sort == "severity" {
        let arms = ["critical", "error", "warning", "notice", "info"]
            .iter()
            .map(|name| format!("WHEN '{name}' THEN {}", severity_rank(name)))
            .collect::<Vec<_>>()
            .join(" ");
        format!("CASE casefold({raw_value}) {arms} ELSE 99 END")
    } else {
        format!("casefold({raw_value})")
    };
    let direction = if descending { " DESC" } else { " ASC" };
    let order = format!(
        "({raw_value} IS NULL OR {raw_value}='') ASC,{value_sql}{direction},ordinal{direction}"
    );
    let mut statement = con
        .prepare(&format!(
            "SELECT ordinal,value_json FROM items WHERE {predicate} ORDER BY {order} LIMIT ? OFFSET ?"
        ))
        .map_err(|e| value(e.to_string()))?;
    parameters.push(limit.into());
    parameters.push(offset.into());
    let mut rows = statement
        .query(rusqlite::params_from_iter(parameters.iter()))
        .map_err(|e| value(e.to_string()))?;
    let mut selected = Vec::new();
    while let Some(item) = rows.next().map_err(|e| value(e.to_string()))? {
        let ordinal: i64 = item.get(0).map_err(|e| value(e.to_string()))?;
        let raw: String = item.get(1).map_err(|e| value(e.to_string()))?;
        let parsed: Value = serde_json::from_str(&raw).map_err(ProjectsError::Json)?;
        selected.push((ordinal, parsed));
    }
    drop(rows);
    drop(statement);
    let group_ids: Vec<Option<String>> = selected
        .iter()
        .map(|(_, issue)| {
            issue
                .get("group_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let group_sizes = v2_group_sizes(&con, &group_ids)?;
    Ok(FindingsPage {
        total,
        matched,
        selected,
        group_sizes,
        via_group_ids,
    })
}

/// Pagination envelope: `offset` nonnegative, `limit` in `1..=100`.
pub fn pagination(total: i64, offset: i64, limit: i64) -> ProjectsResult<Value> {
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(value("offset must be nonnegative and limit must be 1..100"));
    }
    Ok(serde_json::json!({
        "total": total,
        "offset": offset,
        "limit": limit,
        "next_offset": if offset + limit < total { Value::from(offset + limit) } else { Value::Null },
        "previous_offset": if offset != 0 { Value::from(std::cmp::max(0, offset - limit)) } else { Value::Null },
    }))
}
