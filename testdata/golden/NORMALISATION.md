# Baseline normalisation

Applied by the reference suite (class `Norm`) to every baseline JSON before writing. Anything not listed is kept verbatim.
Rust-side output must pass through the same rules (or an equivalent) before structural diff.

| # | What | Rule | Why |
|---|---|---|---|
| 1 | Fixture origin | `http://127.0.0.1:<port>` -> `{BASE}`; any other `127.0.0.1:<digits>` -> `127.0.0.1:{PORT}` | port is an implementation detail of the run (ports are fixed per site anyway) |
| 2 | Work dir | scratch path of the run -> `<WORK>` | absolute temp paths |
| 3 | UUIDs | each distinct UUID -> `<UUID:n>`, n by first appearance in the file | scan_uuid etc. are random per run |
| 4 | Evidence ids | `evidence:v1:<UUID:n>:<table>:<24 hex>` -> `...:<HASH>` | hash is derived from the scan uuid |
| 5 | Timestamps | ISO-8601 date-time strings (with optional fraction / zone) -> `<TS>` | wall clock |
| 6 | Durations | numeric values of keys `elapsed_ms`, `ttfb_ms`, `response_time`, `elapsed_seconds`, `duration`, `duration_ms`, `latency_ms`, `total_ms`, `time_ms`, `crawl_delay_applied_at` -> `"<DURATION>"` | timing noise |
| 7 | Volatile hashes | string values of keys `config_fingerprint` -> `<FINGERPRINT>`, `sha256` -> `<SHA256>`, `input` -> `<INPUT>` | depend on config incl. port / on volatile content / on temp paths |
| 8 | Body hashes | in `scan.tables.json` bodies are decoded (zlib/identity), `{BASE}` substituted, re-hashed; every 64-hex string anywhere that matched an original body sha256 is replaced by the new one. `codec`, `stored_bytes` and raw blob bytes are dropped | body bytes embed the port; codec is a storage choice |
| 9 | JSON columns | SQLite columns ending `_json` are parsed and stored as JSON objects | structural diff by path |
| 10 | Blob columns | other BLOB values -> `{"$blob_len": n}` | |
| 11 | Key order | all JSON written with `sort_keys=True`, indent 1, UTF-8 | determinism; array order is NOT changed (row order = `ORDER BY rowid`, audit items by ordinal) |
| 12 | Subprocess env | `LC_ALL=C`, `TZ=UTC`, `SEOHEAD_ALLOW_PRIVATE_HOSTS=127.0.0.1` | the oracle blocks loopback targets otherwise |
| 13 | Non-JSON stdout | wrapped as `{"$non_json_stdout": "..."}`; exit code stored next to stdout (`{"exit", "stdout"}`) | |

Not normalised (and therefore must match exactly): writer_version, writer_revision (the oracle commit), schema/format versions, check ids,
severities, counts, URLs after `{BASE}`, ordering of rows, link graph, finding messages.

`raw-scans/` holds the real, un-normalised SQLite files of one capture (uuids, timestamps and port inside); they are for read tests (G2) and
are NOT part of the determinism guarantee. Their hashes are in `raw-scans/SHA256SUMS.txt`.
