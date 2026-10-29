//! `_DiskGroups`: ordered duplicate/content groups held on disk.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use spiderhead_formats::models::Group;
use std::path::PathBuf;

use super::{temp_sqlite, unlink, DiskError};
use crate::normalize::Object;

pub struct DiskGroups {
    path: PathBuf,
    con: Connection,
    closed: bool,
}

impl DiskGroups {
    /// Create the backing store.
    pub fn new() -> Result<Self, DiskError> {
        let (path, con) = temp_sqlite("seohead-audit-groups-")?;
        con.execute_batch(
            "CREATE TABLE groups (ordinal INTEGER PRIMARY KEY, value_json TEXT);\
             CREATE TABLE members (group_ordinal INTEGER, ordinal INTEGER, url TEXT, \
             PRIMARY KEY(group_ordinal,ordinal))",
        )?;
        Ok(Self {
            path,
            con,
            closed: false,
        })
    }

    /// `groups.append(group)` — the group's fields minus `urls` go to
    /// `value_json`; `urls` become member rows.
    pub fn append(&mut self, group: &Group) -> Result<(), DiskError> {
        let ordinal: i64 = self.con.query_row(
            "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM groups",
            [],
            |r| r.get(0),
        )?;
        let value = json!({
            "group_id": group.group_id,
            "check": group.check,
            "value": group.value,
            "count": group.count,
        });
        self.con.execute(
            "INSERT INTO groups VALUES (?,?)",
            params![ordinal, serde_json::to_string(&value)?],
        )?;
        for (index, url) in group.urls.iter().enumerate() {
            self.con.execute(
                "INSERT INTO members VALUES (?,?,?)",
                params![ordinal, index as i64, url],
            )?;
        }
        Ok(())
    }

    /// Iterate groups in insertion order.
    pub fn iter(&self) -> Vec<Group> {
        let mut stmt = match self
            .con
            .prepare("SELECT ordinal,value_json FROM groups ORDER BY ordinal")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default();
        rows.into_iter()
            .map(|(ordinal, text)| {
                let v: Object = serde_json::from_str(&text).unwrap_or_default();
                let urls = self.member_urls(ordinal);
                Group {
                    group_id: v
                        .get("group_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    check: v
                        .get("check")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    value: v.get("value").and_then(Value::as_str).map(str::to_owned),
                    urls,
                    count: v.get("count").and_then(Value::as_i64).unwrap_or(0),
                }
            })
            .collect()
    }

    fn member_urls(&self, group_ordinal: i64) -> Vec<String> {
        let mut stmt = match self
            .con
            .prepare("SELECT url FROM members WHERE group_ordinal=? ORDER BY ordinal")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![group_ordinal], |r| r.get(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Group count.
    pub fn len(&self) -> i64 {
        self.con
            .query_row("SELECT COUNT(*) FROM groups", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Close and unlink.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        unlink(&self.path);
    }
}

impl Drop for DiskGroups {
    fn drop(&mut self) {
        self.close();
    }
}
