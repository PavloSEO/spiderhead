CREATE TABLE audit_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version TEXT NOT NULL CHECK (format_version = 'audit.v2'),
    binding_json TEXT NOT NULL,
    header_json TEXT NOT NULL,
    sha256 TEXT NOT NULL
);
CREATE TABLE collections (
    pointer TEXT PRIMARY KEY,
    item_count INTEGER NOT NULL CHECK (item_count >= 0)
);
CREATE TABLE items (
    pointer TEXT NOT NULL REFERENCES collections(pointer),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    value_json TEXT NOT NULL,
    PRIMARY KEY (pointer, ordinal)
);
CREATE TABLE group_members (
    pointer TEXT NOT NULL CHECK (pointer = '/groups'),
    group_ordinal INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    value_json TEXT NOT NULL,
    PRIMARY KEY (group_ordinal, ordinal),
    FOREIGN KEY (pointer, group_ordinal) REFERENCES items(pointer, ordinal)
);
