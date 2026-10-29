use super::*;

pub(super) fn public(entry: &Value, consumer_name: Option<&str>) -> Value {
    let mut result = Map::new();
    for key in [
        "id",
        "kind",
        "text",
        "references",
        "author_role",
        "created_at",
        "goal_state",
    ] {
        result.insert(key.to_owned(), entry[key].clone());
    }
    result.insert(
        "accepted_at".to_owned(),
        entry.get("accepted_at").cloned().unwrap_or(Value::Null),
    );
    result.insert(
        "completed_at".to_owned(),
        entry.get("completed_at").cloned().unwrap_or(Value::Null),
    );
    result.insert(
        "triage".to_owned(),
        entry.get("triage").cloned().unwrap_or(json!([])),
    );
    if let Some(name) = consumer_name {
        let receipt = entry["delivery"][name].clone();
        result.insert(
            "read_at".to_owned(),
            receipt.get("read_at").cloned().unwrap_or(Value::Null),
        );
        result.insert(
            "acknowledged_at".to_owned(),
            receipt
                .get("acknowledged_at")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    Value::Object(result)
}

/// `submit`: store a note or proposed goal without executing anything.
pub fn submit(
    directory: &Path,
    text_input: &str,
    kind: &str,
    references_value: Option<&Value>,
    author_role: &str,
    expected_revision: Option<i64>,
) -> ProjectsResult<Value> {
    if !["note", "proposed_goal", "question"].contains(&kind) {
        return Err(crate::error::value(
            "kind must be note, proposed_goal or question",
        ));
    }
    if !["specialist", "agent"].contains(&author_role) {
        return Err(crate::error::value(
            "author_role must be specialist or agent",
        ));
    }
    transaction(directory, expected_revision, |_root, document| {
        let text_value = text_arg(text_input, "text", MAX_TEXT)?;
        let refs = references(references_value)?;
        let entry = json!({
            "id": format!("inbox:{}", uuid::Uuid::new_v4()),
            "kind": kind,
            "text": text_value,
            "references": refs,
            "author_role": author_role,
            "created_at": crate::coverage::now(),
            "delivery": {},
            "goal_state": if kind == "proposed_goal" { json!("proposed") } else { Value::Null },
            "triage": [],
        });
        document["entries"]
            .as_array_mut()
            .unwrap()
            .push(entry.clone());
        Ok(json!({
            "ok": true,
            "revision": document["revision"].as_i64().unwrap_or(0) + 1,
            "entry": public(&entry, None),
        }))
    })
}

/// `list_entries`: read a bounded page without changing delivery state.
pub fn list_entries(
    directory: &Path,
    consumer_name: &str,
    offset: i64,
    limit: i64,
    include_acknowledged: bool,
) -> ProjectsResult<Value> {
    let consumer_name = consumer(consumer_name)?;
    if offset < 0 || !(1..=MAX_PAGE as i64).contains(&limit) {
        return Err(crate::error::value(
            "offset must be nonnegative and limit must be from 1 to 100",
        ));
    }
    let (_, document) = read_document(directory)?;
    let entries: Vec<&Value> = document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| {
            include_acknowledged
                || falsy(&entry["delivery"][consumer_name.as_str()]["acknowledged_at"])
        })
        .collect();
    let total = entries.len() as i64;
    let page: Vec<&&Value> = entries
        .iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "entries": page.iter().map(|e| public(e, Some(&consumer_name))).collect::<Vec<_>>(),
        "pagination": {
            "offset": offset,
            "limit": limit,
            "total": total,
            "next_offset": if offset + page.len() as i64 <= total && offset + page.len() as i64 != total { json!(offset + page.len() as i64) } else { Value::Null },
        },
    }))
}

pub(super) fn falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// `entry_ids` shape: a unique bounded nonempty list.
pub(super) fn valid_entry_ids(entry_ids: &Value, limit_msg: &str) -> ProjectsResult<Vec<Value>> {
    let Some(list) = entry_ids
        .as_array()
        .filter(|l| !l.is_empty() && l.len() <= MAX_PAGE)
    else {
        return Err(crate::error::value(limit_msg));
    };
    let mut seen: Vec<&Value> = Vec::with_capacity(list.len());
    for item in list {
        if seen.contains(&item) {
            return Err(crate::error::value(limit_msg));
        }
        seen.push(item);
    }
    Ok(list.clone())
}

/// `mark_read`: record inspection; a read message remains unread until acknowledgment.
pub fn mark_read(
    directory: &Path,
    consumer_name: &str,
    entry_ids: &Value,
    expected_revision: Option<i64>,
) -> ProjectsResult<Value> {
    let consumer_name = consumer(consumer_name)?;
    let ids = valid_entry_ids(
        entry_ids,
        "entry_ids must be a unique bounded nonempty list",
    )?;
    transaction(directory, expected_revision, |_root, document| {
        let entries = document["entries"].as_array_mut().unwrap();
        if ids.iter().any(|id| !entries.iter().any(|e| e["id"] == *id)) {
            return Err(crate::error::value("unknown inbox entry"));
        }
        let stamp = crate::coverage::now();
        let mut changed = false;
        for item in &ids {
            let entry = entries.iter_mut().find(|e| e["id"] == *item).unwrap();
            let receipt = entry["delivery"]
                .as_object_mut()
                .unwrap()
                .entry(consumer_name.clone())
                .or_insert(json!({}));
            if receipt.get("read_at").is_none() {
                receipt["read_at"] = json!(stamp);
                changed = true;
            }
        }
        let entries_ref = document["entries"].as_array().unwrap();
        Ok(json!({
            "ok": true,
            "revision": document["revision"].as_i64().unwrap_or(0) + i64::from(changed),
            "entries": ids.iter().map(|item| {
                public(entries_ref.iter().find(|e| e["id"] == *item).unwrap(), Some(&consumer_name))
            }).collect::<Vec<_>>(),
        }))
    })
}

/// `acknowledge`: explicitly acknowledge entries; idempotent retries keep the receipt.
pub fn acknowledge(
    directory: &Path,
    consumer_name: &str,
    entry_ids: &Value,
    expected_revision: Option<i64>,
) -> ProjectsResult<Value> {
    let consumer_name = consumer(consumer_name)?;
    transaction(directory, expected_revision, |_root, document| {
        let ids = valid_entry_ids(
            entry_ids,
            "entry_ids must name unique existing inbox entries",
        )?;
        let entries = document["entries"].as_array_mut().unwrap();
        if ids.iter().any(|id| !entries.iter().any(|e| e["id"] == *id)) {
            return Err(crate::error::value(
                "entry_ids must name unique existing inbox entries",
            ));
        }
        let stamp = crate::coverage::now();
        let mut changed = false;
        for item in &ids {
            let entry = entries.iter_mut().find(|e| e["id"] == *item).unwrap();
            let receipt = entry["delivery"]
                .as_object_mut()
                .unwrap()
                .entry(consumer_name.clone())
                .or_insert(json!({}));
            if receipt.get("read_at").is_none() {
                receipt["read_at"] = json!(stamp);
            }
            if receipt.get("acknowledged_at").is_none() {
                receipt["acknowledged_at"] = json!(stamp);
                changed = true;
            }
        }
        let entries_ref = document["entries"].as_array().unwrap();
        Ok(json!({
            "ok": true,
            "revision": document["revision"].as_i64().unwrap_or(0) + i64::from(changed),
            "entries": ids.iter().map(|item| {
                public(entries_ref.iter().find(|e| e["id"] == *item).unwrap(), Some(&consumer_name))
            }).collect::<Vec<_>>(),
        }))
    })
}

/// `goal_task_id`: the checklist task an accepted goal creates, derived from its id.
pub fn goal_task_id(entry_id: &str) -> String {
    format!(
        "custom:goal-{}",
        entry_id.strip_prefix("inbox:").unwrap_or(entry_id)
    )
}

pub(super) fn goal_task_title(text: &str) -> String {
    let joined: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title = format!("Owner goal: {joined}");
    truncate_chars(&mut title, 512);
    title
}

pub(super) fn truncate_chars(text: &mut String, max: usize) {
    if text.chars().count() > max {
        let byte = text
            .char_indices()
            .nth(max)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        text.truncate(byte);
    }
}

/// `set_goal_state`: accept or complete a proposed goal; acceptance creates its task once.
pub fn set_goal_state(
    directory: &Path,
    entry_id: &str,
    state: &str,
    expected_revision: Option<i64>,
) -> ProjectsResult<Value> {
    if !["accepted", "completed"].contains(&state) {
        return Err(crate::error::value(
            "goal state must be accepted or completed",
        ));
    }
    transaction(directory, expected_revision, |_root, document| {
        let position = document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .position(|item| item["id"].as_str() == Some(entry_id));
        let Some(position) = position else {
            return Err(crate::error::value("entry is not a proposed goal"));
        };
        let entry = &document["entries"][position];
        if entry["kind"].as_str() != Some("proposed_goal") {
            return Err(crate::error::value("entry is not a proposed goal"));
        }
        if state == "completed" && entry["goal_state"].as_str() != Some("accepted") {
            return Err(crate::error::value(
                "a proposed goal must be accepted before completion",
            ));
        }
        if state == "accepted" {
            crate::coverage::ensure_goal_task(
                directory,
                &goal_task_id(entry_id),
                &goal_task_title(entry["text"].as_str().unwrap_or_default()),
                entry["text"].as_str(),
            )?;
        }
        let changed = document["entries"][position]["goal_state"].as_str() != Some(state);
        document["entries"][position]["goal_state"] = json!(state);
        if changed {
            document["entries"][position][format!("{state}_at").as_str()] =
                json!(crate::coverage::now());
        }
        Ok(json!({
            "ok": true,
            "revision": document["revision"].as_i64().unwrap_or(0) + i64::from(changed),
            "entry": public(&document["entries"][position], None),
        }))
    })
}

/// `triage`: append an explicit agent decision to a human note without starting work.
pub fn triage(
    directory: &Path,
    entry_id: &str,
    outcome: &Value,
    actor: &str,
    expected_revision: Option<i64>,
) -> ProjectsResult<Value> {
    let entry_id = text_arg(entry_id, "entry_id", 256)?;
    let actor = text_arg(actor, "triage actor", 128)?;
    if !outcome.is_object() || outcome.get("kind").is_none() {
        return Err(crate::error::value(
            "triage outcome must be an object with a kind",
        ));
    }
    let kind = outcome["kind"].as_str().unwrap_or_default().to_owned();
    let reason = text(
        outcome.get("reason").unwrap_or(&Value::Null),
        "triage reason",
        512,
    )?;
    transaction(directory, expected_revision, |root, document| {
        let position = document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .position(|item| item["id"].as_str() == Some(entry_id.as_str()));
        let entry_ok = position.is_some_and(|p| {
            let e = &document["entries"][p];
            e["kind"].as_str() == Some("note") && e["author_role"].as_str() == Some("specialist")
        });
        if !entry_ok {
            return Err(crate::error::value(
                "triage applies only to a specialist note",
            ));
        }
        let mut receipt = json!({
            "kind": kind,
            "reason": reason,
            "actor": actor,
            "recorded_at": crate::coverage::now(),
        });
        match kind.as_str() {
            "task" => {
                let task_ids = outcome.get("task_ids").and_then(Value::as_array);
                let Some(task_ids) = task_ids.filter(|ids| !ids.is_empty() && ids.len() <= 20)
                else {
                    return Err(crate::error::value(
                        "task triage requires one or more custom task ids",
                    ));
                };
                let rows = crate::coverage::coverage_status(root, true)?;
                let items = rows["items"].as_array().cloned().unwrap_or_default();
                let id_set: std::collections::BTreeSet<&str> =
                    task_ids.iter().filter_map(Value::as_str).collect();
                let invalid = id_set.len() != task_ids.len()
                    || task_ids.iter().any(|task_id| {
                        let id = task_id.as_str().unwrap_or_default();
                        !id.starts_with("custom:")
                            || match items.iter().find(|row| row["id"].as_str() == Some(id)) {
                                None => true,
                                Some(row) => !falsy(&row["stale"]) || !falsy(&row["complete"]),
                            }
                    });
                if invalid {
                    return Err(crate::error::value(
                        "task triage requires current incomplete custom checklist tasks",
                    ));
                }
                receipt["task_ids"] = json!(task_ids);
            }
            "goal" => {
                let goal_id = outcome.get("goal_id");
                let goal = goal_id.and_then(|g| {
                    document["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["id"].as_str() == g.as_str())
                });
                let goal_state = goal
                    .map(|g| g["goal_state"].as_str().unwrap_or_default())
                    .unwrap_or_default();
                if goal_id.and_then(Value::as_str).is_none()
                    || goal.is_none()
                    || goal.unwrap()["kind"].as_str() != Some("proposed_goal")
                    || !["proposed", "accepted"].contains(&goal_state)
                {
                    return Err(crate::error::value(
                        "goal triage requires a current stored proposed goal id",
                    ));
                }
                receipt["goal_id"] = goal_id.unwrap().clone();
            }
            "competitor" => {
                let competitors = outcome.get("competitors").and_then(Value::as_array);
                let Some(competitors) = competitors.filter(|c| !c.is_empty() && c.len() <= 20)
                else {
                    return Err(crate::error::value(
                        "competitor triage requires one or more candidate URLs",
                    ));
                };
                let (_, project) = workspace_load(root)?;
                let mut normalized = Vec::with_capacity(competitors.len());
                for item in competitors {
                    normalized.push(crate::workspace::target(item.as_str().unwrap_or_default())?);
                }
                if normalized
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != normalized.len()
                    || normalized.contains(
                        &project["site"]["target"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    )
                {
                    return Err(crate::error::value(
                        "competitor candidates must be distinct non-primary sites",
                    ));
                }
                receipt["competitors"] = json!(normalized);
            }
            "blocked" | "rejected" => {}
            _ => {
                return Err(crate::error::value(
                    "triage kind must be task, goal, competitor, blocked, or rejected",
                ))
            }
        }
        let expected: std::collections::BTreeSet<&str> = match kind.as_str() {
            "task" => ["task_ids"].into_iter().collect(),
            "goal" => ["goal_id"].into_iter().collect(),
            "competitor" => ["competitors"].into_iter().collect(),
            _ => Default::default(),
        };
        let keys: std::collections::BTreeSet<&str> = outcome
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut allowed: std::collections::BTreeSet<&str> =
            ["kind", "reason"].into_iter().collect();
        allowed.extend(expected);
        if keys != allowed {
            return Err(crate::error::value("triage outcome has unsupported fields"));
        }
        let position = position.unwrap();
        if document["entries"][position]["triage"].is_null() {
            document["entries"][position]["triage"] = json!([]);
        }
        document["entries"][position]["triage"]
            .as_array_mut()
            .unwrap()
            .push(receipt.clone());
        triage_receipt(&receipt)?;
        Ok(json!({
            "ok": true,
            "revision": document["revision"].as_i64().unwrap_or(0) + 1,
            "entry": public(&document["entries"][position], None),
        }))
    })
}

/// `unread_summary`: bounded references for a tool response; never consumes messages.
pub fn unread_summary(directory: &Path, consumer_name: &str, limit: i64) -> ProjectsResult<Value> {
    let consumer_name = consumer(consumer_name)?;
    if !(1..=MAX_PAGE as i64).contains(&limit) {
        return Err(crate::error::value("limit must be from 1 to 100"));
    }
    let (_, document) = read_document(directory)?;
    let unread: Vec<&Value> = document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| falsy(&e["delivery"][consumer_name.as_str()]["acknowledged_at"]))
        .collect();
    Ok(json!({
        "count": unread.len(),
        "entries": unread.iter().take(limit as usize).map(|e| json!({
            "id": e["id"],
            "kind": e["kind"],
            "references": e["references"],
        })).collect::<Vec<_>>(),
        "truncated": unread.len() as i64 > limit,
    }))
}

/// `unread_goal_tasks`: accepted goals whose task this consumer has not acknowledged.
pub fn unread_goal_tasks(
    directory: &Path,
    consumer_name: &str,
    limit: i64,
) -> ProjectsResult<Value> {
    let consumer_name = consumer(consumer_name)?;
    if !(1..=MAX_PAGE as i64).contains(&limit) {
        return Err(crate::error::value("limit must be from 1 to 100"));
    }
    let (_, document) = read_document(directory)?;
    let unread: Vec<&Value> = document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["kind"].as_str() == Some("proposed_goal")
                && e["goal_state"].as_str() == Some("accepted")
                && falsy(&e["delivery"][consumer_name.as_str()]["acknowledged_at"])
        })
        .collect();
    Ok(json!({
        "count": unread.len(),
        "items": unread.iter().take(limit as usize).map(|e| json!({
            "entry_id": e["id"],
            "task_id": goal_task_id(e["id"].as_str().unwrap_or_default()),
            "title": goal_task_title(e["text"].as_str().unwrap_or_default()),
            "accepted_at": e["accepted_at"],
        })).collect::<Vec<_>>(),
        "truncated": unread.len() as i64 > limit,
    }))
}

/// `fingerprint`: a test-only observer aid — hash raw inbox bytes without modifying them.
pub fn fingerprint(directory: &Path) -> ProjectsResult<String> {
    let (root, _) = workspace_load(directory)?;
    let path = root.join("inbox.json");
    let bytes = if path.exists() {
        io(std::fs::read(&path))?
    } else {
        Vec::new()
    };
    use sha2::Digest;
    Ok(format!("{:x}", sha2::Sha256::digest(&bytes)))
}
