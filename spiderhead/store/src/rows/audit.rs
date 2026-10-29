//! Row structs of the audit.v2 tables.
//!
//! One struct per table, one field per column (`Option` for nullable columns, TEXT dates and JSON
//! kept as `String`, BLOB as `Vec<u8>`). Generated from the DDL in `sql/`; do not edit by hand.

/// Row of table `audit_meta`.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditMetaRow {
    /// Column `singleton`.
    pub singleton: i64,
    /// Column `format_version`.
    pub format_version: String,
    /// Column `binding_json`.
    pub binding_json: String,
    /// Column `header_json`.
    pub header_json: String,
    /// Column `sha256`.
    pub sha256: String,
}

impl AuditMetaRow {
    /// Table name.
    pub const TABLE: &'static str = "audit_meta";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str =
        "singleton, format_version, binding_json, header_json, sha256";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            singleton: row.get(0)?,
            format_version: row.get(1)?,
            binding_json: row.get(2)?,
            header_json: row.get(3)?,
            sha256: row.get(4)?,
        })
    }
}

/// Row of table `collections`.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionsRow {
    /// Column `pointer`.
    pub pointer: String,
    /// Column `item_count`.
    pub item_count: i64,
}

impl CollectionsRow {
    /// Table name.
    pub const TABLE: &'static str = "collections";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "pointer, item_count";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            pointer: row.get(0)?,
            item_count: row.get(1)?,
        })
    }
}

/// Row of table `items`.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemsRow {
    /// Column `pointer`.
    pub pointer: String,
    /// Column `ordinal`.
    pub ordinal: i64,
    /// Column `value_json`.
    pub value_json: String,
}

impl ItemsRow {
    /// Table name.
    pub const TABLE: &'static str = "items";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "pointer, ordinal, value_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            pointer: row.get(0)?,
            ordinal: row.get(1)?,
            value_json: row.get(2)?,
        })
    }
}

/// Row of table `group_members`.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupMembersRow {
    /// Column `pointer`.
    pub pointer: String,
    /// Column `group_ordinal`.
    pub group_ordinal: i64,
    /// Column `ordinal`.
    pub ordinal: i64,
    /// Column `value_json`.
    pub value_json: String,
}

impl GroupMembersRow {
    /// Table name.
    pub const TABLE: &'static str = "group_members";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "pointer, group_ordinal, ordinal, value_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            pointer: row.get(0)?,
            group_ordinal: row.get(1)?,
            ordinal: row.get(2)?,
            value_json: row.get(3)?,
        })
    }
}
