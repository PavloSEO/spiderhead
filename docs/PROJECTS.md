# Projects

A project is the durable handoff between an SEO engineer and an agent: the agreed scope,
collection policy, saved scans, tasks, review decisions and outputs. Start with
[the project-control scenario](scenarios/project-control.md) for a working sequence.

## Which state answers which question?

| State | Question it answers | Read or act through | What it does not establish |
|---|---|---|---|
| Checklist (`coverage.json`) | What agreed work remains, is blocked or needs review? | `project-progress`, `project-checklist-page`, `project-task-detail`; explicit update/record | Whether every page was measured or a website defect repaired |
| Crawl evidence and coverage | What was actually fetched, extracted or unavailable? | `project-scans`, `scan-status`, `scan-inspect`, `crawl-diagnose` | Business priority or completed developer work |
| Remediation ledger | Which finding cases persist, were rechecked or resolved? | [ledger](LEDGER.md), `remediation-*` | Audit-task completion or clean unobserved URLs |
| Workflow/checkpoints | Which accepted, task-bound execution is active or resumable? | `workflow-status`, explicit start/checkpoint/execute/resume | A background agent or arbitrary execution of note text |
| Inbox | What did the specialist suggest, and how was it processed? | `project-inbox-*` | Reading, triage, acknowledgement and goal acceptance are separate explicit actions |
| Worker jobs / monitor claims | What bounded execution was submitted or claimed? | [remote jobs](REMOTE_JOBS.md), `monitor-status` | Completion of the audit or an installed recurring scheduler |

`project-inbox-read` records a read receipt for one consumer; a triage receipt links the decision to work;
`project-inbox-acknowledge` records processing for one consumer; `project-inbox-goal` separately
accepts or rejects a proposed goal. None of these is permission to execute free text. A workflow
requires the accepted goal, registered prompt and current incomplete custom task IDs.

Use the revision returned by the latest read when writing. Old definitions, changed artifacts,
changed scope or stale dependencies can leave work stale or unfinished; a handoff must include
those reasons, the next action and its expected evidence, not just a completion count. Competitor
candidates stay separate site identities and require their own authorized scope and evidence.

## Observation dashboard

`seohead watch --project ./example-project` opens a local terminal dashboard.
Wide windows keep a navigation sidebar; compact windows show numbered navigation
in the footer. The overview displays retained URL and finding counts, discovered
URL coverage, queued/in-flight URLs, sitemap evidence, declared scenario/skill
completion, and, where space permits, severity bars, notes and competitors.

Discovered URL coverage is `done / (done + queued + inflight)` and its denominator
can grow during discovery. A URL-limit stop remains partial. Scenario and skill
meters count declared checklist entries, not whole-site audit coverage. Missing
sitemap observations and absent measurement denominators stay explicitly unknown.

Use `1`–`8` to switch sections, arrows to browse, and Enter to inspect a scan,
finding or saved view. Finding lists support `f` filter, `s` sort and `r` reverse;
detail views support arrow/page scrolling. `n` opens a note and `g` a proposed
goal; Enter saves that draft and Escape discards it. Other views only read data.

Evidence refresh runs outside the keyboard loop. Bounded caches reuse unchanged
scan evidence and invalidate when the scan, WAL, SHM, audit companion or retained
configuration/state changes. The terminal uses an alternate screen and restores
its input/output state on exit. It never starts or resumes a scan automatically.

A project groups independent `scan.v1` files, reports and site facts in a portable
local directory. Create it once, then keep successive scans and competitor scans
under its `scans/` directory. Each scan keeps its own site, build and configuration.

```bash
seohead project new --directory ./example-project --target https://example.test/ --label "Example"
seohead project open --directory ./example-project
seohead project status --directory ./example-project
seohead project progress --directory ./example-project --limit 20 --offset 0
```

The directory contains `project.json`, `scans/`, `reports/`, `log.md`, and, after
the first saved view, `finding-views.json`, and, after the first event, `events.jsonl`.
`events.jsonl` is the structured project log: one JSON line per event with a sequence,
UTC time, source (`agent`, `user`, `scans`, `app`), actor (`user`, `agent`, `schedule`) and
bounded text. It is append-only, read newest-first in pages of at most 200 with source and
text filters, and refuses malformed lines instead of skipping them. The CLI
(`project-event-append`, `project-event-page`) and the matching `seo_project_event_*`
MCP tools expose the same two operations. Each event sequence is the project journal revision;
readers can pass `--since REVISION` (or `since_revision` over MCP) to receive only later events.
When the cursor is current, the response sets `not_modified: true`. `--follow` waits up to
30 seconds for a later event and requires a revision cursor; repeat the call with the returned
revision to continue polling. `log.md` stays the human narrative: every event is
also appended there as one bullet, and run start, the first measured progress update, run finish,
built project reports and checklist attempts write entries automatically.
`project.json` records format `seohead.project.v1`, integer version 1, a persistent
project UUID, UTC creation time, normalized target/host and an optional human label.
Unknown formats/versions refuse; opening never upgrades or rewrites the file.
Existing project directories are never overwritten. Move the entire directory to
preserve the relative artifact references.

Project settings live in revisioned `project-settings.json`; read them with
`seohead project-settings --directory DIR` or `seo_project_settings`. Label changes require
`--expected-revision`. A project delete refuses while a run is live, then moves the entire
directory into a sibling `Trash/` folder so scans and exports remain recoverable.

MCP agents can submit a proposal for a project label or crawl policy with
`seo_project_settings_propose`. Proposals capture the target store revision; an owner reviews
them with `seohead project-settings-review --directory DIR --proposal-id ID --decision DECISION --expected-revision REV` (DECISION is `approve` or `deny`). A stale proposal is recorded as stale and is not applied. Use
`seohead project-settings-proposals` to inspect the queue. The append-only `actions.jsonl` records
the interface, action, outcome and proposal reference; `project-action-log` and
`seo_project_action_log` provide bounded pages filtered by client, outcome or text.
An MCP client may call `seo_project_agent_heartbeat` to record a timestamped activity report.
Status shows its source but never claims that the client remains connected.
After a terminal run, the core stores a bounded byte/count measurement for regular files directly
inside `scans/`; project-status reads that cached record instead of walking the directory. Nested
scan directories or unsafe entries produce an unavailable state with a reason.

This MCP permission policy is an application workflow boundary, not an operating-system sandbox.
This local stdio server has no trusted per-caller owner identity, so MCP owner-only changes fail
closed; local CLI commands provide the explicit review path. The `client` field records which
interface wrote an action and does not establish who controlled that interface.

Local portability: the project archive action writes one zip (a manifest with SHA-256 and size for
each member, plus project.json, log.md, scans/, reports/ and the top-level SQLite stores, which are
snapshotted through the SQLite backup API). A dry-run reports the file list, skipped entries and a
size estimate without writing. Credential-like file names are never archived. The restore action
verifies every member against the manifest, validates the project, and publishes it at a new path
only; it never replaces an existing directory. Raw HTML bodies pruning, event journal, bindings and
core version compatibility are not part of this first slice (#996). The `ledger.v1` remediation ledger
([LEDGER.md](LEDGER.md)) binds to this project UUID and normalized site target: it
tracks the project's findings and their observation history in a separate artifact,
never inside a scan.

Facts are optional scalar values with a name, source (`provenance`) and UTC
`observed_at` timestamp, or null when the observation time is unknown. Keep secrets
out of facts. Project data is local working material, not a public export.

Use the CLI's `--input` JSON argument (or JSON on stdin) or the corresponding MCP `seo_project_new`
parameters to pass `facts`, `template_references` and `profile_references`:

```json
{
  "directory": "./shop-project",
  "target": "https://shop.example.test/",
  "template_references": ["ecommerce/product-card", "ecommerce/category"],
  "profile_references": ["ecommerce/basic"],
  "facts": [{"name": "cms", "value": "WordPress", "provenance": "operator", "observed_at": null}]
}
```

References are portable identifiers; creation records them without loading or
executing template text. Checklist definitions, manual review/signoff and client
deliverable review are explicit local coverage operations described below; creating
or opening a project never runs them. Before checklist initialization, status says
`not_initialized`. Automatic project preparation remains `pending`; neither state
is a 0/0 result or a completed audit.

## Compact progress and next actions

`project progress` and MCP `seo_project_progress` return a bounded checklist page,
state counts and up to five next actions without returning the full scan history.
The `limit` is 1–100 (default 20); `offset` is a zero-based item offset. Pages
include the checklist revision so a caller can detect that the project changed
between reads. Item states separate completed, remaining, running, blocked, stale,
unavailable, review, deliverable, excluded and not-agreed work. Failed attempts
remain visible as blocked with their original attempt status; an excluded or
not-agreed item stays outside next actions.

The only percentage is labelled **Audit-task completion**. It reuses the
checklist's `coverage.audit_tasks` numerator and denominator, and appears only
when every included site has an explicitly recorded plan and that axis has a
measured, nonzero denominator. It is not a site-health or remediation percentage.
URL-population coverage is returned separately. Without an explicit plan, for an
unknown or partial task denominator, or when no applicable tasks remain, the
percentage is null with a reason; it is never presented as 100% by default.
The pagination total counts visible checklist rows, while the checklist's own
counts and coverage axes retain their applicability rules.

```bash
seohead project progress --directory ./example-project --limit 10 --offset 0
seohead project progress --directory ./example-project --limit 10 --offset 10
```

## Terminal observer and inbox

`seohead watch --project DIRECTORY` is an optional Rich terminal observer for a
second screen beside an AI chat. Install the `tui` extra when it is not already
present. The selected project is the explicit `--project` workspace; the command
does not discover or scan arbitrary directories.

An agent using the `control` or `full-audit-v1` workflow offers this observer once
before its first collection or long-running analysis for a project. It asks for an
explicit yes or no, remembers that answer for the current session and project, and
does not open a terminal or start a process before yes. A declined offer never
blocks the audit. The observer needs an existing project workspace; an agent must
not create one solely to display it.

```bash
seohead watch --project ./example-project
```

The numbered screens show saved checklist state, scenarios and skills, competitor
coverage, native/Screaming Frog scan state, a paginated finding browser, saved
finding views, workflow/monitor status, and a bounded log tail. In Findings,
`f` filters retained rows, `s` selects a sort field, `r` reverses it, page keys
move through the result set, and Enter opens a bounded source-evidence detail.
Selecting a retained scan changes only the observer's local selection; it does
not resume, cancel, or rerun work. Saved-view and review readiness screens do
not export or publish a report.

The shared `project-observe` CLI/MCP snapshot also has a bounded `sites` page:
one primary site plus every declared competitor. Each row retains its site identity,
candidate provenance and state, scenario/skill and checklist coverage, and up to the
requested per-site retained scans with project-relative artifact references and
evidence state. A configured candidate remains `candidate; audit not run` until it
has its own retained scan. The snapshot's read-only `policy` shows the selected
quick-crawl and approval budgets, while project template/profile references identify
the saved setup. Unknown or unavailable coverage remains named as such; it is never
reported as a completed audit.

Project-bound native crawls and optional `sf run --project DIRECTORY` attempts
also retain a small live-run record. It shows the collector mode, the recorded
URL budget, the current measured frontier counters when the collector exposes
them, its retained request/time budgets and configured request-rate ceiling,
the retained source kind, project-relative artifact path, config fingerprint,
and one stable run ID per capture. Several explicit native captures may be
active for a project at once; their records remain separate. Native captures
for the same host reserve turns through the project-local shared gate, which
caps their combined public dispatch rate at 2 requests per second while each
run preserves its own configuration and resumability evidence. A crawl without a project
that writes a SQLite scan artifact uses the same gate, stored in the user state directory
(`~/.config/seohead/.origin-pacing.sqlite`, or under `SEOHEAD_CONFIG_DIR` when set), so concurrent crawls of one host share one ceiling.
An explicitly slower per-scan delay remains slower; a delay above 60 seconds
remains valid and does not reserve the shared host slot until its own local
turn arrives. A stale local pacing record that would wait beyond 60 seconds is
named as unavailable rather than sleeping without a bounded operator-visible
outcome.
Each record also shows the measured recent request rate when available, phase events, a
project-relative artifact reference, and whether the
launching controller and, for a live Screaming Frog run, its spawned collector
PID are live, abandoned, stale, unknown, or retained after a terminal outcome.
The observer records a process-start identity where the OS exposes one, so a
reused PID is never presented as the original collector. A crawler's discovered frontier and configured URL budget are separate
values; neither is presented as a site-total percentage. Screaming Frog counters
are shown only when recognized collector progress output supplies them. Unsupported
or missing output leaves counters unavailable while collection/analysis phases
remain observable. The observer only reads these
records and never starts, cancels, resumes, or acknowledges a run.

`project-observe --run-offset 0 --run-limit 20` reads a terminal-run page;
MCP uses `run_offset` and `run_limit` with the same defaults. Offset is a
nonnegative integer and limit is 1–100. Each site's `runs` keeps every stored
`running` record visible alongside that site's selected terminal page, ordered
by newest admission, not finish time. The top-level `runs` is the primary site's
view. `active_total` counts stored running states even when their PID is abandoned
or unknown; observation never rewrites these states. `terminal_total` and
`pagination` describe terminal rows only. Use `next_offset` to continue, and
restart pagination when `revision` changes to avoid mixing different snapshots.
Named flags override explicitly supplied JSON values; absent flags preserve them.

Run history retains at most 100 records per project. Starting a run at capacity
evicts the oldest terminal record and preserves running records; if all 100 are
running, admission fails. This policy is unchanged by reading history pages.
`total` is the current retained count. Earlier evictions were not counted, so
`retention.evicted_total` is null and the snapshot does not claim lifetime history.

`n` writes one explicit project note and `g` writes one explicit proposed goal.
They accept ordinary terminal text, including OS dictation committed as text; no
speech-recognition integration is involved. These are the only writes from the
observer. Reading a screen never marks an inbox entry read, acknowledged, or
accepted. Agents receive a bounded unread summary only when their MCP process
has an explicit consumer identity and a matching project allowlist; see
[MCP_PROFILES.md](MCP_PROFILES.md) for the local stdio boundary.

An agent controller records a processed specialist note with `project-inbox-triage`.
The receipt names current `custom:` checklist tasks, a stored proposed goal, retained
competitor candidates, or a specific blocked/rejected reason. It never parses note text
into commands, accepts a goal, acknowledges a note, starts a crawl, or treats a candidate
as a completed competitor audit. Before an execution workflow starts, its controller must
create or update a current incomplete custom task and pass that task, its separately
accepted goal, and the registered prompt through the workflow context. Record in-progress
and completed task states only through the existing evidence and review contracts.

## Explicit monitor collection

A monitor policy is disabled until explicitly enabled and a `start` action
claims one bounded pass. The stored claim fixes its URLs and request/render
budgets and has a short local lease. `monitor_collect(..., apply=false)` only
previews that claim. `apply=true` fetches only its undispatched URLs through the
guarded HTTP cache, retains a body and validation artifact with verified hashes,
and records a complete, partial, or unavailable result. A conditional `304`
retains the cached effective representation and status; it is not recorded as a
new site `304` result.

The interval remains planning advice. This is an explicit one-shot collector,
not a timer, daemon, background scheduler, crawl launcher, or notification
service. An expired or interrupted claim is never replayed automatically: the
operator must create a new claim. Local receipt recording is explicit and has no
external destination or transport.

Use `monitor-configure`, then a `monitor-schedule` start action to create the claim.
Pass its current revision to `monitor-collect`; add `--apply` only for the explicit
collection call. After collection, pass the returned run identifier and current
revision to `monitor-local-deliver`. The same operations are available as
`seo_monitor_collect` and `seo_monitor_local_deliver` on the local MCP server.

## Recording stack facts

Facts entered at creation are not the only way in. `project-facts` records them
afterwards, and can read the site's stack instead of asking the operator to retype it:

```bash
seohead project facts --directory ./example-project \
  --input '{"facts":[{"name":"site_type","value":"publisher","provenance":"client brief","observed_at":null}]}' --apply
seohead project facts --directory ./example-project --detect
seohead project facts --directory ./example-project --detect --apply
```

Preview is the default; `--apply` is what writes `project.json`. An empty call —
neither supplied facts nor `--detect` — is refused; reading facts is `project-open`.

**Operator decisions outrank detected evidence.** A supplied fact is a decision and
always wins, including over a detection in the same call. A detection never
overwrites an operator-entered fact: it reports the value it saw as `detected_value`
with the action `kept_operator`, and the saved fact stays untouched. The record says
which is which through the fields it already has — a detected fact's `provenance`
starts with `detected by tech-detect` and carries the marker that matched and the URL
it was seen on, and its `observed_at` is the time of that observation. A supplied
fact that claims that provenance is rejected. Re-detection refreshes its own earlier
evidence and its observation time; nothing else is touched.

**`--detect` is the only thing that makes a request, and it is never implicit.**
It reads `robots.txt` first and fetches the project's own target once — two requests
at most, through the same guarded single-page tools `tech-detect` and `robots-check`
use. A disallowed path is not fetched at all, and rules that could not be read are a
refusal to fetch rather than permission. `project-prepare` does not run detection.

**An unavailable detection is not a clean result.** The `detection` block always
names its `state` (`run`, `partial`, `not_run` or `unavailable`) and a reason. A
failed fetch, a disallowed target, a category with no matching signature, and a
category where two candidates matched all leave the fact absent, each with its own
reason under `unavailable`. Nothing is guessed. Detection covers the `cms` and
`framework` facts the packaged priority policy consults; `site_type` and anything
else remain operator-entered.

Recording a fact does not reorder work by itself: apply the priority policy below
to act on it. The MCP equivalent is `seo_project_facts`, with the same `facts`,
`detect` and `apply` arguments.

### Linking a semantic core

A project keeps a link to its semantic core and a short summary, never the core
itself. The core stays in its own database; the project database is not merged with
it. Record the link as operator-entered facts, so it uses the same provenance and
precedence rules as any other fact:

```bash
seohead project facts --directory ./example-project \
  --input '{"facts":[
    {"name":"semcore_project","value":"semantics/semcore/example-shop","provenance":"operator: semantic core project directory","observed_at":null},
    {"name":"semcore_cluster_count","value":42,"provenance":"operator: semcore report summary","observed_at":null},
    {"name":"semcore_landing_mapping","value":"38 of 42 clusters mapped to a landing page","provenance":"operator: semcore landing-page mapping summary","observed_at":null}
  ]}' --apply
```

Re-recording a name replaces its value in place. Detection never writes these names,
and a supplied fact cannot claim detection provenance. The values are a relative
project-side identifier and plain summary numbers or text; keep absolute local paths
out of the project record.

## Checklist coverage

Checklist initialization records the built-in catalogue as local definitions; it
does not run a check, skill or scenario, and makes no network request.

```bash
seohead project checklist-init --directory ./example-project
```

`checklist-init` also accepts an optional `plan` recording the agreed audit
scope: who agreed it (`reviewer`), the URL `population` it covers, and the
agreed `tasks` set. A population declares a `kind` — `complete_set` for a
known full population, `sample` for a named agreed sample, or `unknown` when
no population was agreed — plus a `source` saying where it came from, and
either a `size` or an enumerated `urls` list it derives its size from
(per-template populations sit under `templates`). `unknown` carries no size
or URLs, only a reason. Template populations are agreed sub-populations of
the site population: when the site set is enumerated every template URL must
belong to it, and declared template membership — one template's `size` or the
union of all enumerated template URLs — can never exceed the agreed site
`size`, so an incoherent plan is refused rather than trimmed or
double-counted. `tasks` is `{kind: all_agreed}` — the default, every
checklist item is agreed — or `{kind: selection, ids, source}` naming the
agreed item IDs, which must already exist in the reconciled checklist. Items
outside a selection stay visible as `not_agreed` and sit outside every
denominator; they were never agreed, so they need no exclusion review.
Recording a plan upgrades the checklist to `seohead.coverage.v3` and appends
to a retained plan history; status returns the current `plan` plus earlier
agreements under `plan_history`. Re-recording an identical agreement is an
idempotent no-op, while a changed agreement starts a new plan revision:
evidence recorded under an earlier plan revision is marked stale rather than
re-evaluated against the new scope. Reading or reconciling without a plan
never upgrades or invents a population.

```bash
seohead project checklist-init --directory ./example-project \
  --input '{"plan":{"reviewer":"Lead auditor","population":{"kind":"complete_set","size":null,"urls":["https://example.test/"],"name":null,"source":"Agreed sitemap export","reason":null,"templates":null}}}'
```

[`docs/examples/project-skeleton`](examples/project-skeleton) is shipped with that
step already applied, so its committed `coverage.json` carries one definition per
catalogue item and its status reports counts instead of `not_initialized`:

```bash
seohead project status --directory docs/examples/project-skeleton
```

Every shipped item is `not_run` and carries no execution record: the example states
what the catalogue asks for, not a result, because nothing has been run against its
synthetic site. Its definition observation times are the project's own creation time
rather than a generating machine's clock. Regenerate the file with
`python scripts/generate_project_skeleton_coverage.py` when the catalogue changes.

The returned status includes `revision`, `counts`, `views` and `items`. Pass that
revision to every update or evidence record so a concurrent writer cannot replace
newer local history. Structured definitions and records use the normal `--input`
JSON convention:

```bash
seohead project checklist-update \
  --directory ./example-project \
  --expected-revision 1 \
  --input '{"item":{"id":"custom:client-copy-review","title":"Review client copy","scope":{"site":"https://example.test/","template":null,"urls":[]},"dependencies":[],"execution_kind":"manual","priority":"P1","enabled":true,"order":900,"operation":null,"source_hash":null}}'
```

Use `project-checklist-record --directory DIRECTORY --item-id ITEM_ID
--expected-revision N --input '{"record": ...}'` to store supplied evidence or an
explicit applicability review. It validates the record and dependencies, then
returns the same completion views. Recording never executes an item or turns a
missing measurement into a clean result.

`crawl-site --project DIRECTORY` defaults an absent start URL to the project's
target and a new artifact to `DIRECTORY/scans/`. Explicit URLs and scan paths win,
so a competitor retains its own scan identity. Explicit `--out-dir` or configured
`output.dir` keeps the legacy directory route. `--resume` continues the explicit
artifact without injecting a new URL or destination. A supplied project is always
validated, including with explicit paths.

`scan-list --project DIRECTORY` and `scan-prune --project DIRECTORY` default their
history directory to the validated project's `scans/`; an explicit directory wins.
Prune previews by default and still requires an explicit reviewed plan and apply.
Individual-file inspect/snapshot/pin/reanalysis commands continue to take explicit
scan paths. The CLI and MCP share these rules. Merely opening or listing a project
does not fetch pages, run a checklist, or contact a provider. The MCP equivalents
are `seo_project_checklist_init`, `seo_project_checklist_update` and
`seo_project_checklist_record`.

## Saved finding views

Named finding views are stored in project-local `finding-views.json` with a closed schema and
schema/config revisions. A view can select severities, check IDs, exact URLs and declared segment
names, then specify a registered sort field, display columns and page size. Values within a
filter are ORed; separate filters are ANDed. Expressions, SQL, code and regex filters are refused.

```bash
seohead project view-list --directory ./example-project
seohead project view-save --directory ./example-project --expected-revision 0 \
  --input '{"view":{"name":"critical-pages","filters":{"severity":["critical"]},"sort":{"field":"url","direction":"asc"},"columns":["severity","check","url","text"],"page_size":100}}'
seohead findings-view --directory ./example-project --name critical-pages \
  --audit ./example-project/scans/current.sqlite --offset 0
seohead report-build --audit ./example-project/scans/current.sqlite --project ./example-project \
  --view critical-pages --format md --out ./example-project/reports/critical-pages.md
```

The first save uses config revision `0`; each following save requires the most recent
`config_revision`. A view keeps its stable ID while its own revision increments. The apply result
includes source scan identity, schema and config revisions, total/matched/returned counts,
missing-field counts and stable pagination metadata. Missing values in a filter do not match and
are counted. Missing projected values are `null` and named on the row. Sort ties keep source order;
missing sort values are last. Segment filters reuse the segment definitions and existing evaluator
stored with the audit; absent definitions return an explicit unavailable error instead of an empty
result. View definitions themselves accept no regex expressions.

Views only affect displayed finding rows and fields. Reports label the view and returned page;
audit totals, evidence coverage and scores still describe the source audit. The saved scan and its
findings are unchanged. The MCP `seo_findings_view` operation exposes the same bounded projection
to terminal clients and future navigation surfaces. Saved finding views do not currently support
the streamed audit.v2 report path; that route refuses the combination rather than silently losing
filters. A BI cohort selection is a different dataset and does not replace a finding-segment view.

`report-build --project DIRECTORY` includes the validated checklist coverage, reasons,
scope and measurement in a human report without fetching or rerunning the audit. The
original JSON audit remains unchanged, and `--out` still controls the destination.

### Definitions, evidence, and reusable templates

The [synthetic ecommerce template](examples/ecommerce-checklist.json) is a
reusable data-only input for the `template` argument of checklist initialization.
Pass its parsed object through CLI `--input` or the MCP `template` argument; a JSON
filename is not an inline JSON argument. Replace its synthetic site and sample
URLs with the agreed scope. Template text never runs code.

Items have stable IDs, a site/template/URL scope, dependencies, execution kind,
priority, order, and an enabled flag. Updates append definition history; they do
not erase attempts. Explicit priority choices are preserved separately from
defaults. Reconciliation picks up new or changed catalogue definitions. New
entries remain pending; changed definitions or evidence remain visibly stale.
Disabling an item keeps its history and appears in the disabled count, separately
from a reasoned `not_applicable` decision.

Execution records use `running`, `failed`, `unavailable`, `succeeded`, or
`not_applicable`. The checklist states remain `run`, `not_run`, and
`not_applicable`: failed or unavailable attempts are unfinished. Every record
requires a reason. An applicability decision also requires a reviewer and an
inspectable evidence basis — a project-relative `artifact` (digested like any
other evidence) or an explicit `evidence` reference; missing data alone is not
an exclusion. A reviewed exclusion keeps its stable ID and history, exits the
task and check denominators only through that recorded decision, and is listed
separately under `exclusions` with its reason, reviewer, basis and revision. A
disabled item or an exclusion whose
record is stale or was written before the evidence-basis rule stays inside the
denominator as `pending_exclusion` until a specialist reviews it again, so
disabling, removal from the catalogue or reclassification can never silently
shrink agreed counts.

Automatic completion binds a registered check to a validated SQLite artifact
under `scans/` or `reports/`, verifies the saved check outcome and site identity,
and records its digest, producer/configuration, time, and measured population.
A template requires explicit sample URLs matching that artifact's population.
Partial measurements remain limited even when the step completed: an item
whose scope is the whole site stays unfinished until a complete observation,
while an item scoped to explicit sample URLs finishes once the artifact
covers exactly those URLs. This metadata
records provenance and detects changed local bytes; it is not independent
attestation of how an artifact was produced.

Manual completion requires a named reviewer and either `signoff: true` or an
artifact with `review: "approved"`. Deliverables always require an artifact and
approved review. A file's existence, opening a skill, or discovering a finding
does not establish completed work or an implemented client-site fix. Current
status lists running, blocked, waiting-for-manual-review, deliverable-ready,
pending-exclusion and remaining items from the same records. A previously run
step can become blocked when its dependency becomes stale; human reports show
that distinction.

Status also returns `coverage`, five named ratios that never share a
denominator. `audit_tasks` counts applicable tasks completed over the agreed
task set; `checks` the same for automatic checks only (with `measured_urls`
reported beside it); `manual_review` and `deliverable_review` count human
signoffs and approved deliverables over their own applicable sets; and
`url_population` counts distinct eligible URLs covered by fresh measurements
over the agreed plan population — never a row or finding count. Each axis
states its `basis`, `numerator`, `denominator` and a `state`; task axes also
carry `excluded`, `pending_exclusion`, `not_agreed` and `unfinished` counts,
and the URL axis carries `measured_urls`, `unverified_measurements` and the
population kind/name. A denominator that cannot be
justified — no recorded plan or an `unknown` population — is `null` with a
`state` of `unknown` and a reason, never coerced to 0/0 or reported as 100%;
measurements that cannot be verified against the agreed population keep the
axis `partial` and count only verified URLs. The URL axis is a single
site-level ratio: a template-scoped measurement verifies membership against
its template's declared population when one is recorded, otherwise against
the site population. Because template populations are validated as
sub-populations when the plan is recorded — enumerated URLs must belong to
the enumerated site set and declared membership cannot exceed the agreed
size — the numerator can never pass its denominator. Membership is verified
against the agreed enumeration: a population declared only by `size` cannot
prove which measured URLs belong to it, so its numerator stays at the
verified count and the axis reports the unverifiable measurements. A
measurement that covers its explicit sample
completes that task's scope but cannot establish full-site coverage; a
site-scoped check backed by a partial scan stays unfinished. Missing
inputs, unavailable checks, partial scans and stale evidence all remain
unfinished. The top-level `complete` flag requires every applicable agreed
task finished, no pending exclusions, and the agreed URL population fully
covered by verified measurements — a checklist with no recorded scope, an
`unknown` population, or uncovered eligible URLs never reports a completed
audit. Multi-site aggregation sums these axes per site and degrades to
`unknown` whenever any site cannot justify its denominator.

Writes use an exclusive `.coverage.lock`, optimistic revisions and atomic file
replacement. A concurrent writer refuses without discarding earlier work. After
an interrupted process leaves a lock, confirm that no writer is active before
removing that lock and retrying with the freshly read revision. Unknown coverage
schemas, unsafe paths and malformed history refuse rather than being migrated
on read.


### Preview and apply work priorities

```bash
seohead project priorities --directory ./example-project
seohead project priorities --directory ./example-project --apply --expected-revision 1
```

Initialize the checklist first and pass the revision returned by its current status.
Preview is offline and does not change files. Apply records the policy, saved fact
provenance, and per-item reasons in an atomic `seohead.coverage.v2` update. Reading a
v1 checklist never upgrades it; existing definitions, attempts and completion
hashes survive the explicit update. Reapplying unchanged decisions and inputs is a
byte-preserving no-op. Explicit operator/template priorities, including P1, win.

The [packaged policy](../seohead/data/project_priorities.json) assigns baseline
P0 response/robots/canonical reviews and P2 URL-style reviews; other defaults stay
P1. Saved `framework` facts raise JavaScript rendering work; `cms` facts raise PHP
and CMS URL/security work; `site_type: publisher` raises pagination/depth/sitemap
work. These are configurable specialist work priorities, not Google requirements
or changes to finding severity. Missing stack facts do not imply a detected stack.

A custom `policy` object can be supplied with `--input` JSON or through MCP. Its
format is `seohead.project-priorities.v1`; each rule has `id`, `facts`, `priority`
(P0/P1/P2), and a list of exact catalogue `items`. An empty facts object is an
unconditional rule. Other fact conditions compare declared string values without
case sensitivity; all conditions in a rule must match. Contradictory matching
rules refuse instead of silently choosing a winner. Policy text never executes
code, fetches a site, or contacts a provider.

Status and human reports display the saved priority, origin and reason. A priority
change does not complete work or invalidate a previously reviewed result; changes
to the work definition or evidence still follow the normal stale-evidence rules.

See [Terminal controls and observation](TERMINAL.md) for keyboard navigation,
read cadence, task detail, and retained scan history. The same bounded reads are
available through `project-activity`, `project-checklist-page`, `project-task-detail`,
and `project-scans` in the CLI and MCP.

Use `sitemap-crawl --url https://example.test/sitemap.xml --project ./project`
to record a standalone sitemap observation explicitly. The URL must share the
project origin and have no credentials, query or fragment. Counts distinguish
parsed sitemap documents from declared page URLs; no HTML crawl is inferred.

### Evidence verification budgets

A project policy may explicitly set `evidence_hash.max_bytes` and
`evidence_hash.max_seconds`. Defaults are 1 GiB per artifact and 5 seconds per
verification request; accepted ceilings are 64 GiB and 300 seconds. An explicit
large-artifact profile can use 32 GiB and 120 seconds. Older policy documents
without the field retain the defaults and are normalized only in memory.
Exceeding a budget refuses verification; it never marks unread bytes verified.

Successful workflow checkpoints validate retained project-contained artifact bytes
against the supplied SHA-256 and preserve a verification receipt. Explicit
`workflow-status` revalidates bytes. Passive workflow observations report
`evidence_verification_mode="metadata_only"`: matching file metadata is a receipt
observation, not a fresh byte verification. Changed metadata becomes stale; an old
completion without a receipt remains unverified in that passive view.

Passive checklist rows likewise expose `evidence_verification` with their mode,
state and verification time. `unverified`, `stale` and unavailable evidence never
mean a measured zero or a freshly verified completion. Older records are not
rewritten during reads; recording a new verification receipt is an explicit action.
Explicit coverage/project status retains byte verification by default.
