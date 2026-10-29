//! Introspection of the scan.v1 DDL: expected objects and columns, measured once per process.

use super::*;

/// One `sqlite_master` row: (type, name, tbl_name, sql).
pub type ObjectRow = (String, String, String, Option<String>);

/// One column as reported by `PRAGMA table_info`.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnInfo {
    pub name: String,
    pub kind: String,
    pub required: bool,
    pub default: Option<String>,
    pub primary: bool,
}

/// The schema a scan.v1 database must have, measured on an in-memory database.
#[derive(Debug, PartialEq)]
pub struct Expected {
    pub objects: Vec<ObjectRow>,
    pub columns: BTreeMap<String, Vec<ColumnInfo>>,
}

// Measured once per process and cached.
static EXPECTED: Mutex<Option<Arc<Expected>>> = Mutex::new(None);

/// Every `sqlite_master` object of the scan.v1 DDL (not `sqlite_*`), ordered by type and name.
pub fn objects(con: &Connection) -> Result<Vec<ObjectRow>, StoreError> {
    let mut st = con.prepare(
        "SELECT type, name, tbl_name, sql FROM sqlite_master \
         WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The expected objects and table columns, measured once per process.
pub fn expected() -> Result<Arc<Expected>, StoreError> {
    runtime()?;
    let mut guard = EXPECTED
        .lock()
        .map_err(|_| bad("expected-schema cache poisoned"))?;
    if let Some(found) = guard.as_ref() {
        return Ok(Arc::clone(found));
    }
    let con = Connection::open_in_memory()?;
    con.execute_batch(schema())?;
    let objects = objects(&con)?;
    let mut columns = BTreeMap::new();
    for (kind, name, _, _) in &objects {
        if kind == "table" {
            columns.insert(name.clone(), table_columns(&con, name)?);
        }
    }
    let found = Arc::new(Expected { objects, columns });
    *guard = Some(Arc::clone(&found));
    Ok(found)
}

/// Columns of one table, as `PRAGMA table_info` reports them.
pub(super) fn table_columns(con: &Connection, table: &str) -> Result<Vec<ColumnInfo>, StoreError> {
    let mut st = con.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let rows = st.query_map([], |r| {
        Ok(ColumnInfo {
            name: r.get(1)?,
            kind: r.get(2)?,
            required: r.get::<_, i64>(3)? != 0,
            default: r.get(4)?,
            primary: r.get::<_, i64>(5)? != 0,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Columns of `table` in the expected schema.
pub(super) fn columns_of(table: &str) -> Result<Vec<ColumnInfo>, StoreError> {
    expected()?
        .columns
        .get(table)
        .cloned()
        .ok_or_else(|| bad(format!("{table}: table is not part of scan.v1")))
}
