//! Row structs of the ledger.v1 tables (schema after migrations v2..v5).
//!
//! One struct per table, one field per column (`Option` for nullable columns, TEXT dates and JSON
//! kept as `String`, BLOB as `Vec<u8>`). Generated from the DDL in `sql/`; do not edit by hand.

/// Row of table `ledger`.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerRow {
    /// Column `singleton`.
    pub singleton: i64,
    /// Column `ledger_uuid`.
    pub ledger_uuid: String,
    /// Column `format_version`.
    pub format_version: String,
    /// Column `created_at`.
    pub created_at: String,
    /// Column `writer_version`.
    pub writer_version: String,
    /// Column `writer_revision`.
    pub writer_revision: String,
    /// Column `ledger_revision`.
    pub ledger_revision: i64,
}

impl LedgerRow {
    /// Table name.
    pub const TABLE: &'static str = "ledger";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "singleton, ledger_uuid, format_version, created_at, writer_version, writer_revision, ledger_revision";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            singleton: row.get(0)?,
            ledger_uuid: row.get(1)?,
            format_version: row.get(2)?,
            created_at: row.get(3)?,
            writer_version: row.get(4)?,
            writer_revision: row.get(5)?,
            ledger_revision: row.get(6)?,
        })
    }
}

/// Row of table `site`.
#[derive(Debug, Clone, PartialEq)]
pub struct SiteRow {
    /// Column `site_id`.
    pub site_id: i64,
    /// Column `project_uuid`.
    pub project_uuid: String,
    /// Column `target`.
    pub target: String,
    /// Column `host`.
    pub host: String,
    /// Column `netloc`.
    pub netloc: String,
    /// Column `role`.
    pub role: String,
    /// Column `created_at`.
    pub created_at: String,
}

impl SiteRow {
    /// Table name.
    pub const TABLE: &'static str = "site";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str =
        "site_id, project_uuid, target, host, netloc, role, created_at";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            site_id: row.get(0)?,
            project_uuid: row.get(1)?,
            target: row.get(2)?,
            host: row.get(3)?,
            netloc: row.get(4)?,
            role: row.get(5)?,
            created_at: row.get(6)?,
        })
    }
}

/// Row of table `source_scan`.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceScanRow {
    /// Column `source_scan_id`.
    pub source_scan_id: i64,
    /// Column `site_id`.
    pub site_id: i64,
    /// Column `scan_uuid`.
    pub scan_uuid: String,
    /// Column `format_version`.
    pub format_version: String,
    /// Column `source_kind`.
    pub source_kind: String,
    /// Column `lifecycle`.
    pub lifecycle: String,
    /// Column `evidence_revision`.
    pub evidence_revision: i64,
    /// Column `audit_sha256`.
    pub audit_sha256: String,
    /// Column `audit_schema_version`.
    pub audit_schema_version: String,
    /// Column `audit_created_at`.
    pub audit_created_at: Option<String>,
    /// Column `config_fingerprint`.
    pub config_fingerprint: String,
    /// Column `writer_version`.
    pub writer_version: String,
    /// Column `writer_revision`.
    pub writer_revision: String,
    /// Column `analyzer_version`.
    pub analyzer_version: String,
    /// Column `analyzer_revision`.
    pub analyzer_revision: String,
    /// Column `scan_sha256`.
    pub scan_sha256: String,
    /// Column `crawl_partial`.
    pub crawl_partial: i64,
    /// Column `corpus_partial`.
    pub corpus_partial: i64,
    /// Column `artifact_state`.
    pub artifact_state: String,
    /// Column `missing_reason`.
    pub missing_reason: String,
    /// Column `ingested_at`.
    pub ingested_at: String,
    /// Column `group_memberships_state`.
    pub group_memberships_state: String,
    /// Column `canonical_audit_sha256`.
    pub canonical_audit_sha256: Option<String>,
}

impl SourceScanRow {
    /// Table name.
    pub const TABLE: &'static str = "source_scan";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "source_scan_id, site_id, scan_uuid, format_version, source_kind, lifecycle, evidence_revision, audit_sha256, audit_schema_version, audit_created_at, config_fingerprint, writer_version, writer_revision, analyzer_version, analyzer_revision, scan_sha256, crawl_partial, corpus_partial, artifact_state, missing_reason, ingested_at, group_memberships_state, canonical_audit_sha256";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            source_scan_id: row.get(0)?,
            site_id: row.get(1)?,
            scan_uuid: row.get(2)?,
            format_version: row.get(3)?,
            source_kind: row.get(4)?,
            lifecycle: row.get(5)?,
            evidence_revision: row.get(6)?,
            audit_sha256: row.get(7)?,
            audit_schema_version: row.get(8)?,
            audit_created_at: row.get(9)?,
            config_fingerprint: row.get(10)?,
            writer_version: row.get(11)?,
            writer_revision: row.get(12)?,
            analyzer_version: row.get(13)?,
            analyzer_revision: row.get(14)?,
            scan_sha256: row.get(15)?,
            crawl_partial: row.get(16)?,
            corpus_partial: row.get(17)?,
            artifact_state: row.get(18)?,
            missing_reason: row.get(19)?,
            ingested_at: row.get(20)?,
            group_memberships_state: row.get(21)?,
            canonical_audit_sha256: row.get(22)?,
        })
    }
}

/// Row of table `check_def`.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckDefRow {
    /// Column `check_id`.
    pub check_id: i64,
    /// Column `check_key`.
    pub check_key: String,
}

impl CheckDefRow {
    /// Table name.
    pub const TABLE: &'static str = "check_def";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "check_id, check_key";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            check_id: row.get(0)?,
            check_key: row.get(1)?,
        })
    }
}

/// Row of table `finding`.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingRow {
    /// Column `finding_id`.
    pub finding_id: i64,
    /// Column `site_id`.
    pub site_id: i64,
    /// Column `check_id`.
    pub check_id: i64,
    /// Column `subject_type`.
    pub subject_type: String,
    /// Column `subject_value`.
    pub subject_value: String,
    /// Column `finding_key`.
    pub finding_key: String,
    /// Column `current_state`.
    pub current_state: String,
    /// Column `first_seen_at`.
    pub first_seen_at: String,
}

impl FindingRow {
    /// Table name.
    pub const TABLE: &'static str = "finding";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "finding_id, site_id, check_id, subject_type, subject_value, finding_key, current_state, first_seen_at";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            finding_id: row.get(0)?,
            site_id: row.get(1)?,
            check_id: row.get(2)?,
            subject_type: row.get(3)?,
            subject_value: row.get(4)?,
            finding_key: row.get(5)?,
            current_state: row.get(6)?,
            first_seen_at: row.get(7)?,
        })
    }
}

/// Row of table `occurrence`.
#[derive(Debug, Clone, PartialEq)]
pub struct OccurrenceRow {
    /// Column `occurrence_id`.
    pub occurrence_id: i64,
    /// Column `finding_id`.
    pub finding_id: i64,
    /// Column `check_id`.
    pub check_id: i64,
    /// Column `subject_type`.
    pub subject_type: String,
    /// Column `subject_value`.
    pub subject_value: String,
    /// Column `representation`.
    pub representation: String,
    /// Column `discriminator_type`.
    pub discriminator_type: String,
    /// Column `discriminator_value`.
    pub discriminator_value: String,
    /// Column `occurrence_key`.
    pub occurrence_key: String,
    /// Column `current_state`.
    pub current_state: String,
    /// Column `first_seen_at`.
    pub first_seen_at: String,
}

impl OccurrenceRow {
    /// Table name.
    pub const TABLE: &'static str = "occurrence";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "occurrence_id, finding_id, check_id, subject_type, subject_value, representation, discriminator_type, discriminator_value, occurrence_key, current_state, first_seen_at";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            occurrence_id: row.get(0)?,
            finding_id: row.get(1)?,
            check_id: row.get(2)?,
            subject_type: row.get(3)?,
            subject_value: row.get(4)?,
            representation: row.get(5)?,
            discriminator_type: row.get(6)?,
            discriminator_value: row.get(7)?,
            occurrence_key: row.get(8)?,
            current_state: row.get(9)?,
            first_seen_at: row.get(10)?,
        })
    }
}

/// Row of table `affected_url`.
#[derive(Debug, Clone, PartialEq)]
pub struct AffectedUrlRow {
    /// Column `finding_id`.
    pub finding_id: i64,
    /// Column `url`.
    pub url: String,
    /// Column `role`.
    pub role: String,
    /// Column `first_seen_at`.
    pub first_seen_at: String,
}

impl AffectedUrlRow {
    /// Table name.
    pub const TABLE: &'static str = "affected_url";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "finding_id, url, role, first_seen_at";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            finding_id: row.get(0)?,
            url: row.get(1)?,
            role: row.get(2)?,
            first_seen_at: row.get(3)?,
        })
    }
}

/// Row of table `finding_observation`.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingObservationRow {
    /// Column `finding_id`.
    pub finding_id: i64,
    /// Column `source_scan_id`.
    pub source_scan_id: i64,
    /// Column `issue_ordinal`.
    pub issue_ordinal: String,
    /// Column `audit_fingerprint`.
    pub audit_fingerprint: Option<String>,
    /// Column `severity`.
    pub severity: String,
    /// Column `status_code`.
    pub status_code: Option<i64>,
    /// Column `message`.
    pub message: String,
    /// Column `occurrences_count`.
    pub occurrences_count: i64,
    /// Column `enumerated_count`.
    pub enumerated_count: i64,
    /// Column `coverage_state`.
    pub coverage_state: String,
    /// Column `payload_json`.
    pub payload_json: String,
    /// Column `payload_sha256`.
    pub payload_sha256: String,
}

impl FindingObservationRow {
    /// Table name.
    pub const TABLE: &'static str = "finding_observation";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "finding_id, source_scan_id, issue_ordinal, audit_fingerprint, severity, status_code, message, occurrences_count, enumerated_count, coverage_state, payload_json, payload_sha256";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            finding_id: row.get(0)?,
            source_scan_id: row.get(1)?,
            issue_ordinal: row.get(2)?,
            audit_fingerprint: row.get(3)?,
            severity: row.get(4)?,
            status_code: row.get(5)?,
            message: row.get(6)?,
            occurrences_count: row.get(7)?,
            enumerated_count: row.get(8)?,
            coverage_state: row.get(9)?,
            payload_json: row.get(10)?,
            payload_sha256: row.get(11)?,
        })
    }
}

/// Row of table `finding_group`.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingGroupRow {
    /// Column `finding_id`.
    pub finding_id: i64,
    /// Column `source_scan_id`.
    pub source_scan_id: i64,
    /// Column `group_ref`.
    pub group_ref: String,
    /// Column `group_check`.
    pub group_check: String,
    /// Column `group_value`.
    pub group_value: Option<String>,
    /// Column `group_count`.
    pub group_count: Option<i64>,
    /// Column `group_urls_json`.
    pub group_urls_json: String,
}

impl FindingGroupRow {
    /// Table name.
    pub const TABLE: &'static str = "finding_group";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "finding_id, source_scan_id, group_ref, group_check, group_value, group_count, group_urls_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            finding_id: row.get(0)?,
            source_scan_id: row.get(1)?,
            group_ref: row.get(2)?,
            group_check: row.get(3)?,
            group_value: row.get(4)?,
            group_count: row.get(5)?,
            group_urls_json: row.get(6)?,
        })
    }
}

/// Row of table `observation`.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationRow {
    /// Column `observation_id`.
    pub observation_id: i64,
    /// Column `occurrence_id`.
    pub occurrence_id: i64,
    /// Column `source_scan_id`.
    pub source_scan_id: i64,
    /// Column `observation_revision`.
    pub observation_revision: i64,
    /// Column `role`.
    pub role: String,
    /// Column `issue_ordinal`.
    pub issue_ordinal: String,
    /// Column `evidence_state`.
    pub evidence_state: String,
    /// Column `evidence_reason`.
    pub evidence_reason: String,
    /// Column `evidence_json`.
    pub evidence_json: String,
    /// Column `payload_sha256`.
    pub payload_sha256: String,
    /// Column `config_fingerprint`.
    pub config_fingerprint: String,
    /// Column `producer_version`.
    pub producer_version: String,
    /// Column `producer_revision`.
    pub producer_revision: String,
    /// Column `observed_at`.
    pub observed_at: Option<String>,
    /// Column `observed_at_state`.
    pub observed_at_state: String,
    /// Column `ingested_at`.
    pub ingested_at: String,
}

impl ObservationRow {
    /// Table name.
    pub const TABLE: &'static str = "observation";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "observation_id, occurrence_id, source_scan_id, observation_revision, role, issue_ordinal, evidence_state, evidence_reason, evidence_json, payload_sha256, config_fingerprint, producer_version, producer_revision, observed_at, observed_at_state, ingested_at";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            observation_id: row.get(0)?,
            occurrence_id: row.get(1)?,
            source_scan_id: row.get(2)?,
            observation_revision: row.get(3)?,
            role: row.get(4)?,
            issue_ordinal: row.get(5)?,
            evidence_state: row.get(6)?,
            evidence_reason: row.get(7)?,
            evidence_json: row.get(8)?,
            payload_sha256: row.get(9)?,
            config_fingerprint: row.get(10)?,
            producer_version: row.get(11)?,
            producer_revision: row.get(12)?,
            observed_at: row.get(13)?,
            observed_at_state: row.get(14)?,
            ingested_at: row.get(15)?,
        })
    }
}

/// Row of table `decision`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRow {
    /// Column `decision_id`.
    pub decision_id: i64,
    /// Column `occurrence_id`.
    pub occurrence_id: i64,
    /// Column `state`.
    pub state: String,
    /// Column `actor`.
    pub actor: String,
    /// Column `reason`.
    pub reason: String,
    /// Column `decided_at`.
    pub decided_at: Option<String>,
    /// Column `decided_at_state`.
    pub decided_at_state: String,
    /// Column `observation_id`.
    pub observation_id: Option<i64>,
    /// Column `ledger_revision`.
    pub ledger_revision: i64,
    /// Column `verification_result_id`.
    pub verification_result_id: Option<i64>,
}

impl DecisionRow {
    /// Table name.
    pub const TABLE: &'static str = "decision";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "decision_id, occurrence_id, state, actor, reason, decided_at, decided_at_state, observation_id, ledger_revision, verification_result_id";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            decision_id: row.get(0)?,
            occurrence_id: row.get(1)?,
            state: row.get(2)?,
            actor: row.get(3)?,
            reason: row.get(4)?,
            decided_at: row.get(5)?,
            decided_at_state: row.get(6)?,
            observation_id: row.get(7)?,
            ledger_revision: row.get(8)?,
            verification_result_id: row.get(9)?,
        })
    }
}

/// Row of table `verification_artifact`.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationArtifactRow {
    /// Column `verification_id`.
    pub verification_id: i64,
    /// Column `artifact_sha256`.
    pub artifact_sha256: String,
    /// Column `schema_version`.
    pub schema_version: String,
    /// Column `artifact_path`.
    pub artifact_path: String,
    /// Column `task_id`.
    pub task_id: String,
    /// Column `baseline_audit_sha256`.
    pub baseline_audit_sha256: String,
    /// Column `selection_sha256`.
    pub selection_sha256: String,
    /// Column `collection_sha256`.
    pub collection_sha256: String,
    /// Column `collection_state`.
    pub collection_state: String,
    /// Column `recorded_at`.
    pub recorded_at: String,
}

impl VerificationArtifactRow {
    /// Table name.
    pub const TABLE: &'static str = "verification_artifact";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "verification_id, artifact_sha256, schema_version, artifact_path, task_id, baseline_audit_sha256, selection_sha256, collection_sha256, collection_state, recorded_at";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            verification_id: row.get(0)?,
            artifact_sha256: row.get(1)?,
            schema_version: row.get(2)?,
            artifact_path: row.get(3)?,
            task_id: row.get(4)?,
            baseline_audit_sha256: row.get(5)?,
            selection_sha256: row.get(6)?,
            collection_sha256: row.get(7)?,
            collection_state: row.get(8)?,
            recorded_at: row.get(9)?,
        })
    }
}

/// Row of table `verification_result`.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationResultRow {
    /// Column `verification_result_id`.
    pub verification_result_id: i64,
    /// Column `verification_id`.
    pub verification_id: i64,
    /// Column `occurrence_id`.
    pub occurrence_id: i64,
    /// Column `row_index`.
    pub row_index: i64,
    /// Column `outcome`.
    pub outcome: String,
    /// Column `measured`.
    pub measured: i64,
    /// Column `before_sha256`.
    pub before_sha256: String,
    /// Column `after_sha256`.
    pub after_sha256: Option<String>,
    /// Column `scope_sha256`.
    pub scope_sha256: String,
    /// Column `reason`.
    pub reason: String,
}

impl VerificationResultRow {
    /// Table name.
    pub const TABLE: &'static str = "verification_result";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "verification_result_id, verification_id, occurrence_id, row_index, outcome, measured, before_sha256, after_sha256, scope_sha256, reason";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            verification_result_id: row.get(0)?,
            verification_id: row.get(1)?,
            occurrence_id: row.get(2)?,
            row_index: row.get(3)?,
            outcome: row.get(4)?,
            measured: row.get(5)?,
            before_sha256: row.get(6)?,
            after_sha256: row.get(7)?,
            scope_sha256: row.get(8)?,
            reason: row.get(9)?,
        })
    }
}

/// Row of table `source_group`.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceGroupRow {
    /// Column `source_scan_id`.
    pub source_scan_id: i64,
    /// Column `group_ref`.
    pub group_ref: String,
    /// Column `group_check`.
    pub group_check: String,
    /// Column `group_value`.
    pub group_value: Option<String>,
    /// Column `group_count`.
    pub group_count: Option<i64>,
    /// Column `member_count`.
    pub member_count: i64,
    /// Column `members_sha256`.
    pub members_sha256: String,
}

impl SourceGroupRow {
    /// Table name.
    pub const TABLE: &'static str = "source_group";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "source_scan_id, group_ref, group_check, group_value, group_count, member_count, members_sha256";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            source_scan_id: row.get(0)?,
            group_ref: row.get(1)?,
            group_check: row.get(2)?,
            group_value: row.get(3)?,
            group_count: row.get(4)?,
            member_count: row.get(5)?,
            members_sha256: row.get(6)?,
        })
    }
}

/// Row of table `source_group_member`.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceGroupMemberRow {
    /// Column `source_scan_id`.
    pub source_scan_id: i64,
    /// Column `group_ref`.
    pub group_ref: String,
    /// Column `ordinal`.
    pub ordinal: i64,
    /// Column `value_json`.
    pub value_json: String,
}

impl SourceGroupMemberRow {
    /// Table name.
    pub const TABLE: &'static str = "source_group_member";
    /// Column list in declaration order, usable in `SELECT`.
    pub const COLUMNS: &'static str = "source_scan_id, group_ref, ordinal, value_json";

    /// Read one row selected with [`Self::COLUMNS`].
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            source_scan_id: row.get(0)?,
            group_ref: row.get(1)?,
            ordinal: row.get(2)?,
            value_json: row.get(3)?,
        })
    }
}
