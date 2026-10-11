-- scan.v2 is an explicit write upgrade for retry/requeue history.
-- scan_v1.sql remains frozen and readers never apply this change implicitly.

CREATE TABLE retry_attempts (
  attempt_id INTEGER PRIMARY KEY,
  attempt_uuid TEXT NOT NULL UNIQUE,
  operation TEXT NOT NULL CHECK (operation IN ('requeue','import_urls')),
  requested_at TEXT NOT NULL,
  where_json TEXT NOT NULL,
  backup_path TEXT NOT NULL,
  backup_sha256 TEXT NOT NULL CHECK (length(backup_sha256) = 64),
  source_scan_uuid TEXT
);

CREATE TABLE retry_transitions (
  attempt_id INTEGER NOT NULL REFERENCES retry_attempts(attempt_id),
  url_id INTEGER NOT NULL,
  url TEXT NOT NULL,
  from_frontier_state TEXT NOT NULL,
  to_frontier_state TEXT NOT NULL,
  prior_page_json TEXT,
  prior_document_id INTEGER,
  prior_evidence_sha256 TEXT,
  removed_links INTEGER NOT NULL CHECK (removed_links >= 0),
  removed_forms INTEGER NOT NULL CHECK (removed_forms >= 0),
  removed_resource_refs INTEGER NOT NULL CHECK (removed_resource_refs >= 0),
  removed_contexts INTEGER NOT NULL CHECK (removed_contexts >= 0),
  PRIMARY KEY (attempt_id, url_id)
);

CREATE INDEX retry_transitions_url_id ON retry_transitions(url_id, attempt_id);

CREATE TABLE resource_graph_occurrences (
  occurrence_id INTEGER PRIMARY KEY,
  page_url_id INTEGER NOT NULL,
  source_document_id INTEGER NOT NULL,
  representation TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  kind TEXT NOT NULL,
  carrier TEXT NOT NULL,
  raw_url TEXT NOT NULL,
  resolved_url TEXT NOT NULL,
  integrity TEXT,
  integrity_state TEXT NOT NULL DEFAULT 'unknown',
  nesting_depth INTEGER NOT NULL,
  state TEXT NOT NULL,
  reason TEXT NOT NULL,
  UNIQUE(page_url_id, source_document_id, representation, ordinal)
);

CREATE TABLE resource_graph_fetches (
  resolved_url TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  reason TEXT NOT NULL,
  status_code INTEGER,
  content_type TEXT NOT NULL,
  bytes_received INTEGER NOT NULL,
  elapsed_seconds REAL,
  origin_host TEXT NOT NULL,
  redirects INTEGER NOT NULL,
  nesting_depth INTEGER NOT NULL,
  final_url TEXT NOT NULL DEFAULT '',
  compression TEXT NOT NULL DEFAULT 'unknown',
  cache_state TEXT NOT NULL DEFAULT 'unknown',
  integrity_state TEXT NOT NULL DEFAULT 'unknown',
  width INTEGER,
  height INTEGER,
  body_state TEXT NOT NULL DEFAULT 'unavailable'
);

CREATE INDEX resource_graph_occurrences_url ON resource_graph_occurrences(resolved_url);

CREATE TABLE discovery_occurrences (
  occurrence_key TEXT PRIMARY KEY,
  source_kind TEXT NOT NULL,
  relation TEXT NOT NULL,
  source_url_id INTEGER,
  source_document_id INTEGER,
  source_response_id INTEGER,
  representation TEXT NOT NULL,
  carrier TEXT NOT NULL,
  raw_value TEXT NOT NULL,
  resolved_value TEXT NOT NULL,
  target_url_id INTEGER,
  depth INTEGER,
  outcome TEXT NOT NULL,
  reason TEXT NOT NULL,
  attributes_json TEXT NOT NULL
);

CREATE INDEX discovery_occurrences_target
  ON discovery_occurrences(target_url_id, outcome);

CREATE TABLE discovery_ledger_coverage (
  source_document_id INTEGER NOT NULL,
  representation TEXT NOT NULL,
  captured INTEGER NOT NULL,
  omitted INTEGER NOT NULL,
  state TEXT NOT NULL,
  reason TEXT NOT NULL,
  PRIMARY KEY(source_document_id, representation)
);

CREATE TABLE response_transport_meta (
  response_id INTEGER PRIMARY KEY,
  protocol TEXT,
  protocol_state TEXT NOT NULL,
  total_seconds REAL,
  dns_seconds REAL,
  dns_state TEXT NOT NULL DEFAULT 'unavailable',
  dns_reason TEXT NOT NULL DEFAULT 'not captured',
  connect_seconds REAL,
  connect_state TEXT NOT NULL DEFAULT 'unavailable',
  connect_reason TEXT NOT NULL DEFAULT 'not captured',
  tls_seconds REAL,
  tls_state TEXT NOT NULL DEFAULT 'unavailable',
  tls_reason TEXT NOT NULL DEFAULT 'not captured',
  ttfb_seconds REAL,
  ttfb_state TEXT NOT NULL DEFAULT 'unavailable',
  ttfb_reason TEXT NOT NULL DEFAULT 'not captured',
  download_seconds REAL,
  download_state TEXT NOT NULL DEFAULT 'unavailable',
  download_reason TEXT NOT NULL DEFAULT 'not captured'
);

CREATE TABLE scan_events (
  sequence INTEGER PRIMARY KEY CHECK(sequence > 0),
  event_type TEXT NOT NULL,
  occurred_at TEXT,
  timestamp_state TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE scan_event_meta (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  cap INTEGER NOT NULL,
  captured INTEGER NOT NULL,
  dropped INTEGER NOT NULL
);

CREATE TABLE external_checks (
  ordinal INTEGER PRIMARY KEY CHECK (ordinal >= 0),
  outcome TEXT NOT NULL CHECK (outcome IN ('fetched','failed','blocked','skipped')),
  payload_json TEXT NOT NULL
);

CREATE TABLE external_check_summary (
  singleton INTEGER PRIMARY KEY CHECK (singleton=1),
  payload_json TEXT NOT NULL
);
