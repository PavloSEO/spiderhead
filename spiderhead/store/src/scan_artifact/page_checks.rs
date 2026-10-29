//! Structural checks of the JSON columns of page rows (outlines, link placement, trust signals).

use super::*;

// slice: storage-__init__
pub fn recovered(limitations: &[String]) -> bool {
    limitations
        .iter()
        .any(|note| note.contains("recovered a truncated final line"))
}

// slice: storage-__init__
/// Names of late page fields whose column is entirely NULL (scan written before the field).
pub fn legacy_fields_missing(con: &Connection) -> Result<Vec<String>, StoreError> {
    let select: Vec<String> = LATE_PAGE_FIELDS
        .iter()
        .map(|(_, column)| format!("MAX({column} IS NULL)"))
        .collect();
    let sql = format!("SELECT {} FROM pages", select.join(","));
    let row: Vec<Option<i64>> = con.query_row(&sql, [], |r| {
        (0..LATE_PAGE_FIELDS.len())
            .map(|i| r.get::<_, Option<i64>>(i))
            .collect()
    })?;
    Ok(LATE_PAGE_FIELDS
        .iter()
        .zip(row)
        .filter(|(_, missing)| matches!(missing, Some(v) if *v != 0))
        .map(|((name, _), _)| (*name).to_string())
        .collect())
}

// slice: storage-__init__
pub fn has_negative_page_counts(con: &Connection) -> Result<bool, StoreError> {
    let conditions: Vec<String> = PAGE_NONNEGATIVE_INTS
        .iter()
        .map(|name| format!("{name}<0"))
        .collect();
    let sql = format!(
        "SELECT 1 FROM pages WHERE {} LIMIT 1",
        conditions.join(" OR ")
    );
    Ok(con.prepare(&sql)?.exists([])?)
}

// slice: storage-__init__
pub fn heading_outline(value: &Value) -> Result<(), StoreError> {
    let ok = value.as_array().is_some_and(|items| {
        items.iter().all(|item| {
            item.as_object().is_some_and(|o| {
                o.len() == 3
                    && o.get("text").is_some_and(Value::is_string)
                    && o.get("region").is_some_and(Value::is_string)
                    && o.get("level")
                        .and_then(Value::as_i64)
                        .is_some_and(|l| (1..=6).contains(&l))
            })
        })
    });
    if ok {
        Ok(())
    } else {
        Err(bad(
            "heading_outline must be an ordered list of level/text/region objects, level between 1 and 6",
        ))
    }
}

// slice: storage-__init__
/// Validate one page's rejected-href evidence (#1530).
///
/// ``None`` never reaches here -- a crawl that did not opt into recording stores
/// SQL NULL, and every caller keeps that distinction (see PageRecord).
pub fn invalid_link_targets(value: &Value) -> Result<(), StoreError> {
    let ok = value.as_array().is_some_and(|items| {
        items.iter().all(|item| {
            item.as_object().is_some_and(|o| {
                o.len() == 2
                    && o.get("raw_href").is_some_and(Value::is_string)
                    && o.get("reason")
                        .and_then(Value::as_str)
                        .is_some_and(|r| r == "unparsable_syntax" || r == "unsupported_scheme")
            })
        })
    });
    if ok {
        Ok(())
    } else {
        Err(bad(
            "invalid_link_targets must be a list of raw_href/reason objects, reason unparsable_syntax or unsupported_scheme",
        ))
    }
}

// slice: storage-__init__
/// Validate one page's stored link-placement evidence (#634).
///
/// ``None`` never reaches here -- an unmeasured page stores SQL NULL, and the
/// callers keep that distinction rather than collapsing it into an empty object.
pub fn link_placement(value: &Value) -> Result<(), StoreError> {
    let Some(o) = value.as_object() else {
        return Err(bad("link_placement must be an object with in_heading, in_heading_total, image_no_text and image_no_text_total"));
    };
    let keys: BTreeSet<&str> = o.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = [
        "in_heading",
        "in_heading_total",
        "image_no_text",
        "image_no_text_total",
    ]
    .into_iter()
    .collect();
    if keys != expected {
        return Err(bad("link_placement must be an object with in_heading, in_heading_total, image_no_text and image_no_text_total"));
    }
    for key in ["in_heading_total", "image_no_text_total"] {
        if !o.get(key).and_then(Value::as_i64).is_some_and(|v| v >= 0) {
            return Err(bad(format!(
                "link_placement.{key} must be a non-negative integer"
            )));
        }
    }
    let in_heading_ok = o["in_heading"].as_array().is_some_and(|items| {
        items.iter().all(|item| {
            item.as_object().is_some_and(|x| {
                x.len() == 3
                    && x.get("destination").is_some_and(Value::is_string)
                    && x.get("anchor").is_some_and(Value::is_string)
                    && x.get("level")
                        .and_then(Value::as_i64)
                        .is_some_and(|l| (1..=6).contains(&l))
            })
        })
    });
    if !in_heading_ok {
        return Err(bad("link_placement.in_heading must be a list of level/destination/anchor objects, level between 1 and 6"));
    }
    let image_ok = o["image_no_text"].as_array().is_some_and(|items| {
        items.iter().all(|item| {
            item.as_object()
                .is_some_and(|x| x.len() == 1 && x.get("destination").is_some_and(Value::is_string))
        })
    });
    if !image_ok {
        return Err(bad(
            "link_placement.image_no_text must be a list of destination objects",
        ));
    }
    Ok(())
}

// slice: storage-__init__
/// Validate one page's stored trust-signal evidence (#823).
///
/// ``None`` never reaches here -- a page whose body was never parsed stores
/// SQL NULL, and the callers keep that distinction rather than collapsing it
/// into an empty object.
pub fn trust_signals(value: &Value) -> Result<(), StoreError> {
    let Some(o) = value.as_object() else {
        return Err(bad(
            "trust_signals must be an object with author, dates and article",
        ));
    };
    if o.len() != 3
        || !o.contains_key("author")
        || !o.contains_key("dates")
        || !o.contains_key("article")
    {
        return Err(bad(
            "trust_signals must be an object with author, dates and article",
        ));
    }
    for key in ["author", "dates"] {
        let ok = o[key].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item.as_object().is_some_and(|x| {
                    x.len() == 3
                        && ["signal", "confidence", "value"]
                            .iter()
                            .all(|k| x.get(*k).is_some_and(Value::is_string))
                })
            })
        });
        if !ok {
            return Err(bad(format!(
                "trust_signals.{key} must be a list of signal/confidence/value string objects"
            )));
        }
    }
    if !o["article"]
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_string))
    {
        return Err(bad(
            "trust_signals.article must be a list of marker strings",
        ));
    }
    Ok(())
}

// slice: storage-__init__
/// Validate bounded, parser-observed repeated DOM ids (#828).
pub fn duplicate_ids(value: &Value) -> Result<(), StoreError> {
    let ok = value.as_array().is_some_and(|items| {
        items.len() <= 20
            && items.iter().all(|item| {
                item.as_object().is_some_and(|x| {
                    x.len() == 2
                        && x.get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| id.len() <= 256)
                        && x.get("count")
                            .and_then(Value::as_i64)
                            .is_some_and(|c| c >= 2)
                })
            })
    });
    if ok {
        Ok(())
    } else {
        Err(bad(
            "duplicate_ids must be a bounded list of id/count observations",
        ))
    }
}

// slice: storage-__init__
pub fn hreflang(value: &Value) -> Result<(), StoreError> {
    let ok = value.as_array().is_some_and(|items| {
        items.iter().all(|item| {
            item.as_object().is_some_and(|x| {
                x.len() == 3
                    && ["lang", "raw_href", "url"]
                        .iter()
                        .all(|k| x.get(*k).is_some_and(Value::is_string))
            })
        })
    });
    if ok {
        Ok(())
    } else {
        Err(bad(
            "hreflang must be an ordered list of lang/raw_href/url string objects",
        ))
    }
}
