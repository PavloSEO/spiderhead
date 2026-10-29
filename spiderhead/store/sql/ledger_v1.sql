-- ledger.v1 is the remediation ledger baseline (see docs/LEDGER.md).
-- It is a separate artifact from scan.v1/scan.v2: it stores project/site,
-- source-scan bindings, finding and occurrence identity, affected-URL
-- membership, group membership and append-only observation history.

PRAGMA application_id = 1397051212; -- ASCII SEOL, identifies the application, not compatibility.
PRAGMA user_version = 1;
PRAGMA foreign_keys = ON;

CREATE TABLE ledger (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  ledger_uuid TEXT NOT NULL UNIQUE,
  format_version TEXT NOT NULL CHECK (format_version = 'ledger.v1'),
  created_at TEXT NOT NULL,
  writer_version TEXT NOT NULL,
  writer_revision TEXT NOT NULL,
  -- Optimistic write revision reserved for the lifecycle transition lane (#789):
  -- it counts committed write transactions that changed ledger content, never
  -- source evidence revisions, which live on source_scan.evidence_revision.
  ledger_revision INTEGER NOT NULL CHECK (ledger_revision >= 0)
);

CREATE TABLE site (
  site_id INTEGER PRIMARY KEY,
  project_uuid TEXT NOT NULL,
  target TEXT NOT NULL,
  host TEXT NOT NULL,
  netloc TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('primary','secondary')),
  created_at TEXT NOT NULL,
  UNIQUE (project_uuid, netloc)
);

CREATE TABLE source_scan (
  source_scan_id INTEGER PRIMARY KEY,
  site_id INTEGER NOT NULL REFERENCES site(site_id),
  scan_uuid TEXT NOT NULL,
  format_version TEXT NOT NULL CHECK (format_version IN ('scan.v1','scan.v2')),
  source_kind TEXT NOT NULL,
  lifecycle TEXT NOT NULL,
  evidence_revision INTEGER NOT NULL CHECK (evidence_revision >= 0),
  audit_sha256 TEXT NOT NULL CHECK (length(audit_sha256) = 64),
  audit_schema_version TEXT NOT NULL,
  audit_created_at TEXT,
  config_fingerprint TEXT NOT NULL,
  writer_version TEXT NOT NULL,
  writer_revision TEXT NOT NULL,
  analyzer_version TEXT NOT NULL,
  analyzer_revision TEXT NOT NULL,
  -- First observed digest of the artifact container bytes. Pin, checkpoint or
  -- copy operations may change container bytes without touching evidence, so
  -- this is provenance, not identity: identity is (scan_uuid, evidence_revision,
  -- audit_sha256) plus the producer/configuration columns.
  scan_sha256 TEXT NOT NULL CHECK (length(scan_sha256) = 64),
  crawl_partial INTEGER NOT NULL CHECK (crawl_partial IN (0,1)),
  corpus_partial INTEGER NOT NULL CHECK (corpus_partial IN (0,1)),
  -- 'missing' records that the artifact itself is gone (pruned or moved away).
  -- The binding and its digests remain so history stays resolvable.
  artifact_state TEXT NOT NULL DEFAULT 'present' CHECK (artifact_state IN ('present','missing')),
  missing_reason TEXT NOT NULL DEFAULT '',
  ingested_at TEXT NOT NULL,
  UNIQUE (scan_uuid, evidence_revision)
);

CREATE TABLE check_def (
  check_id INTEGER PRIMARY KEY,
  check_key TEXT NOT NULL UNIQUE
);

CREATE TABLE finding (
  finding_id INTEGER PRIMARY KEY,
  site_id INTEGER NOT NULL REFERENCES site(site_id),
  check_id INTEGER NOT NULL REFERENCES check_def(check_id),
  subject_type TEXT NOT NULL CHECK (subject_type IN ('url','scope')),
  subject_value TEXT NOT NULL,
  finding_key TEXT NOT NULL UNIQUE,
  -- Reserved for #789 lifecycle transitions. 'detected' is the only state this
  -- version writes and no transition may be inferred from a later scan.
  current_state TEXT NOT NULL DEFAULT 'detected',
  first_seen_at TEXT NOT NULL
);

CREATE TABLE occurrence (
  occurrence_id INTEGER PRIMARY KEY,
  finding_id INTEGER NOT NULL REFERENCES finding(finding_id),
  check_id INTEGER NOT NULL REFERENCES check_def(check_id),
  subject_type TEXT NOT NULL CHECK (subject_type IN ('url','scope')),
  subject_value TEXT NOT NULL,
  representation TEXT NOT NULL CHECK (representation IN ('static','rendered','legacy_fragment','legacy_unknown','unknown','scope')),
  discriminator_type TEXT NOT NULL CHECK (discriminator_type IN ('primary','subject','locator')),
  discriminator_value TEXT NOT NULL DEFAULT '',
  occurrence_key TEXT NOT NULL UNIQUE,
  current_state TEXT NOT NULL DEFAULT 'detected',
  first_seen_at TEXT NOT NULL
);

CREATE TABLE affected_url (
  finding_id INTEGER NOT NULL REFERENCES finding(finding_id),
  url TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('subject','location')),
  first_seen_at TEXT NOT NULL,
  PRIMARY KEY (finding_id, url, role)
);

CREATE TABLE finding_observation (
  finding_id INTEGER NOT NULL REFERENCES finding(finding_id),
  source_scan_id INTEGER NOT NULL REFERENCES source_scan(source_scan_id),
  -- Run-local audit projection id, never part of ledger identity but part of
  -- this key because one audit may project one logical finding twice (for
  -- example the same check+URL measured under two HTTP statuses).
  issue_ordinal TEXT NOT NULL,
  audit_fingerprint TEXT,
  severity TEXT NOT NULL,
  status_code INTEGER,
  message TEXT NOT NULL,
  occurrences_count INTEGER NOT NULL CHECK (occurrences_count >= 0),
  -- How much of the reported total the saved audit actually enumerated. The
  -- ledger never synthesizes occurrences for an unenumerated remainder.
  enumerated_count INTEGER NOT NULL CHECK (enumerated_count >= 0),
  coverage_state TEXT NOT NULL CHECK (coverage_state IN ('enumerated','capped','aggregate_only','unknown')),
  payload_json TEXT NOT NULL,
  payload_sha256 TEXT NOT NULL CHECK (length(payload_sha256) = 64),
  PRIMARY KEY (finding_id, source_scan_id, issue_ordinal)
);

CREATE TABLE finding_group (
  finding_id INTEGER NOT NULL REFERENCES finding(finding_id),
  source_scan_id INTEGER NOT NULL REFERENCES source_scan(source_scan_id),
  -- The run-local group id as recorded in this audit revision. Group identity
  -- is historical membership, never part of the finding key.
  group_ref TEXT NOT NULL,
  group_check TEXT NOT NULL,
  group_value TEXT,
  group_count INTEGER,
  group_urls_json TEXT NOT NULL,
  PRIMARY KEY (finding_id, source_scan_id, group_ref)
);

CREATE TABLE observation (
  observation_id INTEGER PRIMARY KEY,
  occurrence_id INTEGER NOT NULL REFERENCES occurrence(occurrence_id),
  source_scan_id INTEGER NOT NULL REFERENCES source_scan(source_scan_id),
  -- Append-only per-occurrence sequence. A new source revision appends, it
  -- never overwrites the baseline observation.
  observation_revision INTEGER NOT NULL CHECK (observation_revision >= 1),
  role TEXT NOT NULL CHECK (role IN ('target','source')),
  -- Run-local audit ordinal of the issue whose observation this is, stored as
  -- provenance so one audit's repeated firings of one occurrence stay distinct.
  issue_ordinal TEXT NOT NULL,
  evidence_state TEXT NOT NULL CHECK (evidence_state IN ('measured','imported_projection','unavailable')),
  evidence_reason TEXT NOT NULL DEFAULT '',
  evidence_json TEXT NOT NULL,
  payload_sha256 TEXT NOT NULL CHECK (length(payload_sha256) = 64),
  config_fingerprint TEXT NOT NULL,
  producer_version TEXT NOT NULL,
  producer_revision TEXT NOT NULL,
  -- UTC observation time, or NULL plus 'unknown' when the audit does not say.
  observed_at TEXT,
  observed_at_state TEXT NOT NULL CHECK (observed_at_state IN ('known','unknown')),
  ingested_at TEXT NOT NULL,
  UNIQUE (occurrence_id, source_scan_id, issue_ordinal),
  UNIQUE (occurrence_id, observation_revision)
);

-- Reserved for #789 decision history: validated lifecycle transitions append
-- immutable rows naming actor, reason, time and the linked observation. This
-- version writes none.
CREATE TABLE decision (
  decision_id INTEGER PRIMARY KEY,
  occurrence_id INTEGER NOT NULL REFERENCES occurrence(occurrence_id),
  state TEXT NOT NULL,
  actor TEXT NOT NULL,
  reason TEXT NOT NULL,
  decided_at TEXT,
  decided_at_state TEXT NOT NULL CHECK (decided_at_state IN ('known','unknown')),
  observation_id INTEGER REFERENCES observation(observation_id),
  ledger_revision INTEGER NOT NULL CHECK (ledger_revision >= 0)
);

CREATE INDEX finding_site_check ON finding(site_id, check_id);
CREATE INDEX occurrence_finding ON occurrence(finding_id);
CREATE INDEX occurrence_check_subject ON occurrence(check_id, subject_type, subject_value);
CREATE INDEX observation_occurrence ON observation(occurrence_id, source_scan_id);
CREATE INDEX observation_scan ON observation(source_scan_id);
CREATE INDEX affected_url_url ON affected_url(url);
CREATE INDEX finding_group_scan ON finding_group(source_scan_id);
CREATE INDEX decision_occurrence ON decision(occurrence_id);
CREATE INDEX source_scan_site ON source_scan(site_id, scan_uuid);
