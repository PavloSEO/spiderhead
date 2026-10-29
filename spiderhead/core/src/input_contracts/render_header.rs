//! Fixed header of the generated command-input reference.

/// Fixed lines that open the generated command-input reference.
pub(super) const RENDER_HEADER: &[&str] = &[
    "# Command inputs",
    "",
    "This reference is generated from `seohead.core.input_contracts`. Each row inventories consumed",
    "source inputs rather than inferring them from a command's output. Forms on one row can be",
    "required together; the notes name those relationships.",
    "",
    "A **scan artifact** is a retained local `scan.v1` SQLite file. Read-only analysis and",
    "history operations do not replay a crawl or promise retained page bodies. `crawl-site --resume`",
    "is the explicit exception: it continues network collection. `duplicate-check` and",
    "`boilerplate-report` may instead read one retained scan corpus with `--scan`.",
    "",
    "## Operational-store decision",
    "",
    "`scan.v1` is the authoritative retained evidence store for an artifact-mode crawl.",
    "The HTTP cache remains sharded `http_cache.v3` files because concurrent crawl workers",
    "can independently replace one cache entry. In live cache modes a cache miss is a safe",
    "network fallback; a replay-mode miss remains offline and is reported. The run",
    "journal remains an append-only local `runs.jsonl` record. Neither store is a second scan corpus,",
    "and this decision makes no backend migration.",
    "",
    "## Catalogue",
    "",
    "| Command | Accepted input forms | Notes |",
    "| --- | --- | --- |",
];
