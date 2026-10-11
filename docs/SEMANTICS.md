# Semantic core

`seohead semantics` grows an accumulating semantic core (a keyword pool) for one project. Each
project is a directory with `project.yaml`, `seeds.txt` and its own SQLite database. Passing a
SEOHEAD project workspace (a directory with `project.json`) keeps the core under its
`semantics/` folder; `db:` in `project.yaml` names the database file the project uses.

```bash
seohead semantics init --project ./my-core
seohead semantics status --project ./my-core
```

The packaged `semantic-core` skill (`seohead skill-show --name general/semantic-core`) is the
method: stage order, which stages are paid, and how to avoid paying twice.

## Stages

| Stage | Reads | Writes | Money |
|---|---|---|---|
| `init` | — | `project.yaml`, `seeds.txt`, database | free |
| `import --file CSV/XLSX` | mapped columns, optional workbook sheet and merge policy | phrases | free |
| `collect [--resume]` | `seeds.txt`, strong phrases | phrases, expansions, provenance edges | Yandex Wordstat |
| `clean` | new phrases | status, route, lemma group | free |
| `graph` | the pool | `anomalies.md`, homonym phrases to review | free |
| `sitematch` | `catalog.txt` | off-catalog phrases to review | free |
| `cluster [--method serp\|louvain]` | kept phrases | SERP cache and receipts, clusters | Yandex Search API for uncached phrases |
| `relevance` | `vendors.txt`, cached SERP | phrases without a peer to review | free |
| `competitors` | cached SERP | `competitors.md`, `synonyms.txt` | free |
| `mine` | cached SERP, public competitor pages | `competitor_mining.md`, `synonyms.txt` | no provider quota |
| `synonyms` | `synonyms.txt` | checked phrases | Yandex Wordstat |
| `exact [--yes]` | kept phrases | `!W` and phrase frequency, provider task receipts | Arsenkin limits |
| `report`, `excel`, `export` | the store | `report.md`, `clusters.csv`, `<project>_semantic_core.xlsx`, `pool.csv`, `pool.json` | free |

## Preflight, imports and exports

`status --json` includes the latest state (`running`, `done` or `failed`) and UTC timestamps for
each stage when the additive stage journal is available. Older databases return a reason when no
stage history exists. The stage journal does not replace the original provider-unit ledger. JSON
operations share the `seohead.semantics.operation.v1` envelope fields (`mode`, `state`, project and
stage where applicable); operation-specific fields carry the measured detail.

Every paid stage has an offline plan. Inspect it before confirming a CLI run:

```bash
seohead semantics cluster --project ./my-core --dry-run --json
seohead semantics cluster --project ./my-core --confirm-paid
```

Plans name the selected region, engine and query source, estimate provider requests or tasks, and
keep money as `null` with a reason when the provider price cannot be established. Collection has a
bounded range because each Wordstat response can discover more phrases. No provider client is
constructed by a plan. MCP returns the same plan from `seo_semantics_paid`; a later call must set
`confirm_paid=true` to send work.

CSV remains supported. XLSX imports read one selected sheet (the active sheet by default), map
fields by header when unambiguous, and show the applied mapping before writing:

```bash
seohead semantics import --project ./my-core --file phrases.xlsx --sheet Queries \
  --map query=Keyword --map frequency=Searches --map group=Category \
  --map url=Landing --merge update --dry-run --json
```

Supported mappings are `query`, `frequency`, `group`, `subgroup`, `url` and `comment`, with the
existing `base`, `quoted`, `exact`, `impr`, `pos_y` and `pos_g` fields retained. A `Category /
Cluster` path is split into group and subgroup. `add` preserves existing phrases, `update` applies
provided fields, and `replace` explicitly replaces the phrase pool. The preview reports new and
existing counts, input duplicates, group counts and sample rows. No rows are changed by a preview.

`export` and `excel` accept `--cluster NAME` or `--scope no-landing`. Their JSON dry-runs return
the exact planned file paths and row counts:

```bash
seohead semantics export --project ./my-core --cluster "industrial pumps" --dry-run --json
seohead semantics excel --project ./my-core --scope no-landing --dry-run --json
```

## Cluster reads, landing evidence and insights

`clusters --offset N --limit N`, `cluster-show --cluster ID_OR_NAME` and
`cluster-queries --cluster ID_OR_NAME --offset N --limit N` are bounded JSON reads. Aggregates
keep absent frequencies as `null`; `exact_measured` says how many phrase rows supplied an exact
frequency. The query page carries the retained Yandex top 10 with region and fetch timestamp.
Legacy SERP rows without region remain labeled as legacy rather than receiving an inferred
region.

After a paid SERP cluster run, the toolkit stores candidate landing URLs only when the project's
own domain is known. Each score is the measured share of cluster phrases for which that URL
appeared in the retained top 10. Missing domain, query history or candidate data stays unavailable
with a reason. `set-landing --cluster ID_OR_NAME --url URL` (empty `--url` clears the value) writes
the selection and an audit record.

`insights --period YYYY-MM-DD..YYYY-MM-DD --sources gsc,metrika,webmaster` reads local
`sources.sqlite` and project bindings. It makes no provider calls. It returns weekly cluster
signals, binding and coverage states, nullable measurements, and versioned rules with thresholds
and evidence references. Non-additive values are weighted only where provider measures provide a
valid weight; otherwise they remain unavailable.

Selected matched rule/cluster pairs can be previewed and then written to the existing project
checklist. This creates manual checklist items and never executes them:

```bash
seohead semantics insights-tasks --project ./shop --period 2026-09-01..2026-10-01 \
  --select landing.unassigned:12 --dry-run --json
seohead semantics insights-tasks --project ./shop --period 2026-09-01..2026-10-01 \
  --select landing.unassigned:12 --expected-revision 8 --confirm --json
```

The write requires the checklist revision observed in the preview and an explicit confirmation.
Stable task IDs make the operation an update to the same selected recommendation rather than a
duplicate item. A changed checklist revision stops the write.

## Accounting and recovery

Provider clients record every charge in the shared spend journal (`seohead spend-report`); a
semantic stage tags its entries with `semantics_project` and `semantics_stage`. The project
database keeps provider units per stage (`runs`) and durable receipts: Wordstat attempts, SERP
fetches and Arsenkin tasks are reserved before the paid call, so an interrupted or ambiguous
request is never resubmitted automatically, and a paid Arsenkin result is fetched again by its
`task_id`.

## Filters

`preset` in `project.yaml` selects the topic filters (`generic` or the example `seo` preset);
`intent`, `stop` and `info` regular expressions override it for the project. A phrase with
observed impressions or positions is never cut by frequency rules.

## Example

`docs/examples/semantics/seohead-tech/` is a synthetic project for an SEO-tool topic: seeds, a
catalog and a few dozen phrases with invented base frequencies. It runs every free stage offline
and is exercised by `tests/test_semantics_cli.py`.
