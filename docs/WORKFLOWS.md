# Agent workflow runs, inbox and monitor records

A project workspace ([Projects](PROJECTS.md)) keeps three small, durable records that let a
specialist and one or more agents share work without relying on chat history:

| Record | File in the project | Commands (CLI = MCP with `seo_` prefix) | Answers |
|---|---|---|---|
| Workflow runs | `execution.json` (`seohead.workflow-execution.v1`) | `workflow-start`, `workflow-checkpoint`, `workflow-status`, `workflow-resume`, `workflow-execute` | Which registered step comes next, and what evidence closed the previous ones? |
| Inbox | `inbox.json` (`seohead.project-inbox.v1`) | `project-inbox-submit`, `-list`, `-read`, `-acknowledge`, `-goal`, `-triage`, `-unread` | What did the specialist ask for, and what did an agent decide about it? |
| Monitor | `monitor.json` (`seohead.monitor.v1`) | `monitor-configure`, `monitor-schedule`, `monitor-collect`, `monitor-run`, `monitor-local-deliver`, `monitor-status` | What changed on a bounded URL set since the last retained observation? |

None of these commands starts a crawl, calls a provider, or injects text into a model
conversation on its own. Collection stays with `crawl-site`, `project-prepare`,
`monitor-collect --apply` and the other explicitly networked tools listed in the
[CLI overview](CLI.md). Every write takes the record's current `revision` (`expected_revision`)
and fails on a conflict, so two agents cannot silently overwrite each other.

## Workflow runs

A workflow run is an ordered list of existing project checklist items attached to one registered
scenario or skill. It is a checkpoint ledger, not an executor.

1. **Start** — `workflow-start` takes `scenario_id` (a registered project scenario or skill in the
   checklist), `steps` (up to 200 unique checklist ids, in order) and a `context` object. The
   context is required and must name:
   - `goal_id` — a proposed goal in the inbox that has already been **accepted** with
     `project-inbox-goal`;
   - `prompt_reference` — a registered skill id from the packaged catalogue (see `skill-list`);
   - `task_ids` — up to 20 current, incomplete `custom:` checklist tasks the run serves;
   - optional `competitors` (up to 20 HTTP(S) URLs) and `phase`.
   The run records each step's definition hash, so a later change to a skill or check
   definition is detectable.
2. **Checkpoint** — `workflow-checkpoint` closes the next pending step, strictly in order, with one
   state: `succeeded`, `failed`, `unavailable`, `skipped` or `interrupted`. A `succeeded`
   checkpoint must carry evidence records `{reference, sha256}` that point at retained project
   artifacts; the hash is recomputed against the bytes on disk and must match. Steps whose
   execution kind is manual or deliverable also require an explicit approved review
   (`{actor, state: "approved", reason}`). A success is refused while a dependency is stale or
   blocked.
3. **Status** — `workflow-status` is read-only. It returns every run, the `next_action` of the
   running run (or of the most recent blocked/interrupted one) and `resumable_run`.
4. **Resume** — after a failure, an interruption or a handoff to another agent,
   `workflow-resume` reopens only the first non-successful step. When retained evidence or a
   definition changed underneath a completed step, it reopens from that step and keeps the old
   evidence as `stale_evidence`.
5. **Execute** — `workflow-execute` is a convenience that starts a run and checkpoints a supplied
   list of `outcomes` in declared order. It still executes no commands; it records outcomes the
   caller already produced, and stops at the first non-successful one.

A run's state is `running`, `blocked`, `interrupted` or `completed`. Completion means every
registered step has a verified checkpoint; it is not a claim that the site is healthy.

## Inbox

The inbox records the specialist's intent and the agent's explicit decisions about it.

| Command | Effect |
|---|---|
| `project-inbox-submit` | Store a `note`, a `proposed_goal` or a `question` (up to 8,000 characters) with optional references such as `task:…`, `scan:…`, `finding:…`. Starts no work. |
| `project-inbox-list` | Read a page of entries for a named `consumer`; reading does not mark anything read. |
| `project-inbox-unread` | Bounded unread summary for one consumer; does not change delivery state. |
| `project-event-append` | Append one structured project event; it records text and executes nothing. |
| `project-event-page` | Read a newest-first page of project events, filtered by source or text. |
| `project-inbox-read` | Record that a consumer inspected specific entries. |
| `project-inbox-acknowledge` | Record an explicit acknowledgment. It never accepts or completes a goal. |
| `project-inbox-goal` | Move a proposed goal to `accepted` or `completed` (completion requires acceptance first). Launches nothing. |
| `project-task-comment` | Append an attributed note to a custom task, optionally linked to a retained scan or finding. |
| `project-inbox-triage` | Append the agent's outcome for a note: created/updated `custom:` tasks, a stored goal, competitor candidates, or a blocked/rejected reason. Note text is never parsed into commands. |

The `watch` terminal observer can write a note (`n`) or a proposed goal (`g`); those are its only
writes. MCP agents receive unread summaries only when their process has an explicit consumer
identity and a matching project allowlist ([MCP profiles](MCP_PROFILES.md)).

Accepting a proposed goal creates one `custom:goal-…` checklist task with the original goal text
as its description. It does not execute the goal or grant manual-review approval. Use the ordinary
checklist revision and evidence rules to update and complete that task.

## Monitor records

Monitoring is disabled until a policy is configured and explicitly started. There is no timer,
daemon or notification transport.

| Command | Effect |
|---|---|
| `monitor-configure` | Save a bounded policy (URLs, request and render budgets, refresh mode). Starts nothing. |
| `monitor-schedule` | `start`, `cancel`, `backoff` or `recover` one local run claim with a short lease. |
| `monitor-collect` | Preview the claimed plan; with `--apply`, fetch exactly the claimed URLs through the guarded HTTP cache and retain bodies with verified hashes. |
| `monitor-run` | Retain an already-collected, caller-supplied set of observations for a scan id as a bounded diff (status, indexability, canonical, robots, metadata, content, links). It fetches nothing and refuses observations outside the planned scope or budgets. |
| `monitor-local-deliver` | Record a deduplicated `local:receipt` for a retained run; no external recipient. |
| `monitor-status` | Read the policy and the last retained checkpoint. |

The full claim → collect → receipt sequence is in
[Projects: explicit monitor collection](PROJECTS.md#explicit-monitor-collection).

## Related

- [Projects](PROJECTS.md) — workspace, checklist coverage, observer, saved finding views.
- [Project-control scenario](scenarios/project-control.md) — the specialist/agent loop end to end.
- [Remediation ledger](LEDGER.md) — finding cases and recheck evidence, kept separate from
  workflow completion.
- [Tool reference](TOOL_REFERENCE.md) — exact arguments for every command above.
