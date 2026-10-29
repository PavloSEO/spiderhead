CREATE TABLE source_group (
    source_scan_id INTEGER NOT NULL REFERENCES source_scan(source_scan_id),
    group_ref TEXT NOT NULL,
    group_check TEXT NOT NULL,
    group_value TEXT,
    group_count INTEGER,
    member_count INTEGER NOT NULL CHECK (member_count >= 0),
    members_sha256 TEXT NOT NULL,
    PRIMARY KEY (source_scan_id, group_ref)
);
CREATE TABLE source_group_member (
    source_scan_id INTEGER NOT NULL,
    group_ref TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    value_json TEXT NOT NULL,
    PRIMARY KEY (source_scan_id, group_ref, ordinal),
    FOREIGN KEY (source_scan_id, group_ref) REFERENCES source_group(source_scan_id, group_ref)
);
