CREATE TABLE verification_artifact (
  verification_id INTEGER PRIMARY KEY,
  artifact_sha256 TEXT NOT NULL UNIQUE CHECK (length(artifact_sha256) = 64),
  schema_version TEXT NOT NULL CHECK (schema_version = 'verification.v1'),
  artifact_path TEXT NOT NULL,
  task_id TEXT NOT NULL,
  baseline_audit_sha256 TEXT NOT NULL CHECK (length(baseline_audit_sha256) = 64),
  selection_sha256 TEXT NOT NULL CHECK (length(selection_sha256) = 64),
  collection_sha256 TEXT NOT NULL CHECK (length(collection_sha256) = 64),
  collection_state TEXT NOT NULL CHECK (collection_state IN ('measured','offline','partial','not_run','not_verifiable')),
  recorded_at TEXT NOT NULL
);

CREATE TABLE verification_result (
  verification_result_id INTEGER PRIMARY KEY,
  verification_id INTEGER NOT NULL REFERENCES verification_artifact(verification_id),
  occurrence_id INTEGER NOT NULL REFERENCES occurrence(occurrence_id),
  row_index INTEGER NOT NULL CHECK (row_index >= 0),
  outcome TEXT NOT NULL CHECK (outcome IN ('resolved','persisting','regressed','unverifiable')),
  measured INTEGER NOT NULL CHECK (measured IN (0,1)),
  before_sha256 TEXT NOT NULL CHECK (length(before_sha256) = 64),
  after_sha256 TEXT,
  scope_sha256 TEXT NOT NULL CHECK (length(scope_sha256) = 64),
  reason TEXT NOT NULL,
  UNIQUE (verification_id, occurrence_id),
  UNIQUE (verification_id, row_index)
);

ALTER TABLE decision ADD COLUMN verification_result_id INTEGER REFERENCES verification_result(verification_result_id);

CREATE INDEX verification_result_occurrence ON verification_result(occurrence_id);
CREATE INDEX verification_artifact_task ON verification_artifact(task_id);
