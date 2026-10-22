# Native tab and panel contracts

This change prepares reusable Qt Widgets and a runnable offline component gallery.
It does not connect all panels to the core or implement an analyzer; the tab set follows
the audit areas of SpiderHead's own check registry. The working application adapter remains owned
by `app.py` / `mcp_gateway.py`; the new widgets accept projections and emit intents.

## Design skeleton

The existing SEOHEAD light theme, Roboto, rounded controls, blue active indicator,
and packaged Material Symbols are reused. A native Qt refresh icon fills the one
missing refresh asset. `component_stylesheet(tokens)` consumes the existing
canonical token dictionary; it introduces no second color palette. Final visual
design remains a separate stage.

The main workspace is two nested `QSplitter`s: main table above the URL inspector,
with an independent right deck. Decks are native `QTabBar` plus a lazy stacked
panel. A hidden SERP form cannot force the visible table's minimum height. At a
narrow width, each table toolbar puts search on its own row. Wide tables scroll
horizontally, columns can be moved/resized/hidden, and long cell values have a
bounded tooltip. The user can hide and restore the two auxiliary panes.

Every table uses `QAbstractTableModel` / `QSortFilterProxyModel` / `QTableView`.
There are no row widgets, web views, databases, backend imports, or hidden reads.
The model consumes at most 101 iterator entries to validate the **100-row limit**;
an oversized page raises without replacing existing rows. This establishes a
presentation bound, not million-row or large-crawl acceptance.

The search field filters the currently loaded page and says so. Sorting is also
local to that page. Global filtering/paging is an explicit `query` intent. A
catalogued filter stays disabled unless the adapter lists its stable ID in
`available_filters`; duplicate/global/graph findings are never recomputed from
the visible 100 rows.

States are `ready`, `loading`, `unavailable`, and `error`, plus a measured empty
result represented by `ready` and zero rows. `total=None` means unknown. An absent
field renders as **Not measured**, while `0`, `False`, and an empty string remain
distinct. Switching source/page or moving to a tab without a selected URL clears
stale inspector evidence. Sensitive cookie/header/config values are redacted at
the presentation boundary as an additional defense.

## Capability to UI matrix

The full column/filter definitions and stable IDs are in
`src/seohead_desktop/ui/tabcatalogue.py`; they are the single catalogue source.
All rows below have a runnable widget. “Required projection” describes the input
the adapter must actually supply before the widget can claim measured data.

| Pane / IDs | Widget and behavior | Required projection / boundary |
|---|---|---|
| Main: `internal` | URL table, MIME selector, paging, selection | Normalized retained `scan-inspect --table pages` page: `url`, `status`, `type`, `indexability`, `title`, `issues`; missing extra columns stay unknown |
| Main: `external` | URL / content type / response / depth / inlinks | Retained external link/resource projection; never copy internal rows as external evidence |
| Main: `response_codes` | Responses & indexing: HTTP / indexability / response time | Retained response evidence; robots and indexability filters require their own coverage |
| Main: `redirects` | Redirect kind / target / final URL / hops table | Retained redirect evidence; chains and loops come from the core, not from the page |
| Main: `links` | Inlink/outlink/depth/score table | Retained link graph; counts cannot be inferred from a page of links |
| Main: `page_titles` | Title and meta description values, lengths and widths | Retained extracted values and analyzer findings; pixel widths never estimated by Qt |
| Main: `headings` | H1/H2 value, length and count table; outline filters | Retained heading evidence and analyzer findings |
| Main: `canonicals`, `pagination`, `hreflang` | Canonical, robots-directive, series and alternate tables | Core graph/directive findings and provenance; repeated values are retained lists, not fabricated numbered fields |
| Main: `content` | Word/readability/similarity/language table | Retained complete analysis; a missing language run is unavailable |
| Main: `images` | MIME/size/dimensions/usage table | Retained image occurrence/resource evidence, including missing-vs-empty alt |
| Main: `structured_data` | Types/formats/errors/warnings table | Retained extraction and validation; eligibility for rich results is not inferred |
| Main: `mobile`, `rendering` | Viewport and raw-versus-rendered tables | Retained mobile layout and rendering evidence; unavailable without a rendered crawl |
| Main: `url`, `performance`, `security` | URL hygiene, response/size/DOM, transport and header tables | Core URL, performance and security findings plus allowed response headers |
| Main: `sitemaps` | Sitemap URL/response/indexability table | Retained sitemap reconciliation and completeness |
| Main: `validation` | HTML structure finding table, including AMP findings | Retained HTML validation output |
| Lower: `url_details` | Name/value table | Only fields from the actual selected record; absent fields remain unknown |
| Lower: `inlinks`, `outlinks` | Source/destination/anchor/rel/follow/placement table; outlinks also list resources | Direction-aware retained link/resource query |
| Lower: `http_headers`, `cookies` | Metadata tables | Retained redacted request/response headers and cookie metadata |
| Lower: `view_source` | Read-only plain text with find and 65,536-character cap | Retained source supplied by adapter; no HTML or JavaScript execution |
| Lower: `serp_snippet` | Editable local title/description preview, reset | Retained fields; edits affect only the preview, and actual search appearance is explicitly unmeasured |
| Lower: `image_details` | Image / alt / dimensions / savings table | Retained image measurements; no invented thumbnails or savings |
| Lower: `structured_data_details` | Property/value/check/severity/details | Retained schema validation |
| Lower: `duplicate_details` | URL / similar page / similarity / indexability table | Retained duplicate analysis |
| Lower: `spelling_grammar_details` | Text/kind/explanation/suggestions/area/URL | Retained language-check evidence |
| Right: `overview` | Category / URL count / percentage table | Core aggregates and scope, never a percentage calculated from the loaded page |
| Right: `issues` | Check/severity/URL/share table | Retained findings; issue selection emits a semantic intent |
| Right: `site_structure` | Folder / URLs / indexable / non-indexable table | Bounded core structure projection; hierarchy/chart rendering is a later enhancement |
| Right: `response_times` | Timing-bucket/count/% table | Core histogram; chart rendering is a later enhancement |
| Right: `spelling_grammar` | Top-error/count/coverage/language/sample table | Bounded retained error projection |
| Right: `api` | Provider/state/reason/observed-at table | Existing readiness projection; opening the tab makes no provider request |
| Project: `work`, `tasks` | State/dependency/reason tables, task selection | `project-progress`, `project-checklist-page`, `project-task-detail` |
| Project: `scans` | Source/lifecycle/finished/partial table, scan selection | Trusted `project-scans` items; preserve UUID and trusted path in selection payload |
| Project: `reanalyze` | Retained-scan selection table | Selection/preview only; execution belongs to the core adapter and its explicit action |
| Project: `compare` | Two retained-scan choosers, preview intent, result table | Require two distinct IDs; core must check scope/config/provenance before comparison |
| Project: `scenarios`, `skills` | Registry metadata tables and selection intents | Actual existing core registry, no fabricated installed/readiness status |
| Project: `competitors` | Domain/source/date/scope/state table | Retained authorized competitor records; no discovery or paid call |
| Project: `inbox` | Message table plus explicit note/question form | `project-inbox-list/unread`; disabled submission until adapter enables it; text remains until success acknowledgment |
| Project: `remediation` | Finding/state/owner/recheck/evidence table | Existing finding/work-item lifecycle; “fixed” needs actual recheck evidence |
| Project: `schema` | URL/types/validation/errors/warnings/evidence table | Retained schema results, no CMS publication |
| Project: `providers` | Readiness/reason/observed/scope table | Existing source readiness; secrets stay hidden |
| Project: `logs` | Bounded redacted event table | Existing core event page |
| Project: `config` | Effective parameter/value/origin/state/reason table | Validated config projection; no implicit writes or credential values |
| Project: `reports` | Artifact/format/state/date/coverage/omissions table | Existing validated artifacts and complete machine-readable evidence |

## Integration recipe

```python
from seohead_desktop.ui.panels import AuditWorkspace, ProjectPanels, component_stylesheet

# Reuse the existing load_theme result once; this does not mutate token files.
app.setStyleSheet(app.styleSheet() + component_stylesheet(tokens))
audit = AuditWorkspace(parent)
projects = ProjectPanels(parent)

audit.set_page(
    "internal", normalized_page_rows,
    total=known_total_or_none, offset=page_offset, has_more=has_more,
    source="Retained native scan · <actual scan identity>",
    available_filters=("all",),
)
projects.set_page("tasks", checklist_items, total=known_total_or_none, source="Project checklist")
projects.set_page("scans", trusted_scan_items, total=known_total_or_none, source="Project retained scans")

audit.intent_requested.connect(controller.handle_audit_intent)
projects.select_task.connect(controller.select_task)
projects.select_scan.connect(controller.select_scan)
projects.refresh.connect(controller.refresh)
projects.submit_note.connect(controller.submit_note)
# Connect projects.intent_requested for query/open_panel/preview_compare and other
# intents only. Do not dispatch the same typed intent twice through both routes.
```

`set_page` takes an iterable of mapping rows plus keyword-only `total`, `offset`,
`has_more`, `source`, `state`, `reason`, and `available_filters`. A page has at most
100 rows. Native URLs use the current adapter's normalized `type` / `status`
keys. Extra keys, including the trusted retained item/path, are preserved in the
selected row but do not create columns automatically.

Generic intents are `(name, payload)`:

| Name | Payload |
|---|---|
| `open_panel`, `refresh` | `{tab_id}` |
| `query` | `{tab_id, offset, limit: 100, filter_id}` |
| `select_url`, `select_issue`, `select_record`, `select_task`, `select_scan`, `select_skill`, `select_scenario`, `select_note`, `select_report` | `{tab_id, row}` |
| `submit_note` | `{text, kind}`; emitted only by an enabled form click |
| `preview_compare` | `{before, after}`; original trusted scan items |

Typed project signals are `refresh()`, `select_task(str)`, `select_scan(dict)`, and
`submit_note(str, str)`. `panel("inbox").set_submission_enabled(True)` is explicit.
Call `submission_succeeded()` only after the core confirms storage. Do not call it
merely because the signal was emitted. `panel("compare").set_scans(items)` fills
the two bounded choosers. A changed project/source calls `audit.clear()` and
`projects.clear()` before asynchronous replacement; normal generation guards stay
in the existing adapter.

`deck.visible_ids` is a serializable tuple. `set_visible_tabs(ids)` validates it,
`select_tab(id)` reveals a hidden tab if needed, and the menu restores all defaults.
The components do not persist user settings; the existing application owns that.
Both decks expose their `QTabBar` for normal keyboard navigation, and tables use
native row selection and Copy. `audit.right` and `audit.detail` may be hidden by
the existing View menu; `audit.restore_panels()` restores their visibility/sizes.

No proposed `scan-url-detail` or other unpublished core endpoint is required by
these widgets. A richer projection can be connected only after its core contract
exists and is verified.

## Validation boundary

Exercise the bounded component contracts through the integrated desktop suite. Runtime and visual acceptance must use the integrated application and compatible core. Synthetic component coverage does not establish native accessibility, Windows/Linux runtime or large-data readiness.
