// Catalogue data only.

//! Direct CLI command contracts 4/6 (project-inbox-triage .. provider-readiness).

use super::{command, form, CommandContract, InputKind};

/// Input contracts for the direct CLI commands in part 4 of 6.
pub(super) const COMMANDS: &[CommandContract] = &[
    command("project-inbox-triage", Some("project_inbox_triage"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["entry_id", "outcome", "actor", "expected_revision"], "Explicit controller outcome for a specialist note; it never reads, acknowledges, or executes the note."),
    ], ""),
    command("project-inbox-unread", Some("project_inbox_unread"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["consumer", "limit"], ""),
    ], ""),
    command("remediation-create", Some("remediation_create"), &[
        form(InputKind::LocalFile, &["path"], "New ledger path; an existing file is refused."),
        form(InputKind::ProjectDirectory, &["project_dir"], ""),
        form(InputKind::Selector, &["producer_build"], "Full lowercase 40-character Git SHA."),
    ], ""),
    command("remediation-ingest", Some("remediation_ingest"), &[
        form(InputKind::LocalFile, &["ledger"], ""),
        form(InputKind::LocalFile, &["scan"], "Read-only saved scan.v1 SQLite artifact."),
    ], ""),
    command("project-event-append", Some("project_event_append"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["source", "actor"], ""),
        form(InputKind::InlineText, &["text"], ""),
    ], ""),
    command("project-event-page", Some("project_event_page"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["source", "offset", "limit", "query"], ""),
    ], ""),
    command("remediation-summary", Some("remediation_summary"), &[
        form(InputKind::LocalFile, &["ledger"], ""),
    ], ""),
    command("remediation-cases", Some("remediation_cases"), &[
        form(InputKind::LocalFile, &["ledger"], ""),
        form(InputKind::Selector, &["check", "url", "finding_key", "limit", "offset", "source_scan_id", "group_ref", "max_bytes"], "Use source_scan_id and group_ref together for a complete ordered group member page; mutually exclusive with finding selectors."),
    ], ""),
    command("remediation-transition", Some("remediation_transition"), &[
        form(InputKind::LocalFile, &["ledger"], ""),
        form(InputKind::Selector, &["occurrence_key", "state", "actor", "reason", "expected_revision"], ""),
        form(InputKind::InlineJson, &["observation_id", "decided_at"], ""),
    ], ""),
    command("remediation-record-verification", Some("remediation_record_verification"), &[
        form(InputKind::LocalFile, &["ledger", "verification_path"], ""),
        form(InputKind::Selector, &["actor", "expected_revision"], ""),
    ], ""),
    command("remediation-recheck", Some("remediation_recheck"), &[
        form(InputKind::LocalFile, &["ledger", "baseline", "out_dir"], ""),
        form(InputKind::AuditDocument, &["after"], "Optional retained later audit for offline verification."),
        form(InputKind::Selector, &["occurrence_keys", "actor", "expected_revision", "task_id"], ""),
        form(InputKind::LocalConfig, &["config"], "Required when the baseline redacted credentials."),
    ], ""),
    command("remediation-report", Some("remediation_report"), &[
        form(InputKind::LocalFile, &["ledger"], ""),
        form(InputKind::LocalDirectory, &["out_dir"], ""),
        form(InputKind::Selector, &["limit", "offset"], ""),
    ], ""),
    command("project-facts", Some("project_facts"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["facts"], "Operator-entered facts; preview by default, recorded with apply."),
    ], "detect fetches the project's own target once after robots.txt; it is never implicit."),
    command("project-checklist-init", Some("project_checklist_init"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["template"], "Optional reusable data-only checklist template."),
        form(InputKind::InlineJson, &["plan"], "Optional agreed scope plan fixing the URL-population and task denominators."),
    ], ""),
    command("project-checklist-update", Some("project_checklist_update"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["item"], "Requires expected_revision for optimistic concurrency."),
    ], ""),
    command("project-checklist-record", Some("project_checklist_record"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["item_id"], ""),
        form(InputKind::InlineJson, &["record"], "Requires expected_revision; records supplied evidence only. Automatic checks take status succeeded, reason and a saved scan artifact; manual items take a reviewer with signoff or an approved reviewed artifact; not_applicable needs reason, reviewer and an evidence basis."),
    ], ""),
    command("project-view-list", Some("project_view_list"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("project-view-show", Some("project_view_show"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["name"], ""),
    ], ""),
    command("project-view-save", Some("project_view_save"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["view"], ""),
        form(InputKind::Selector, &["expected_revision"], "Required; use 0 for the first saved view."),
    ], ""),
    command("project-view-delete", Some("project_view_delete"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["name"], ""),
        form(InputKind::Selector, &["expected_revision"], "Required; current view config revision."),
    ], ""),
    command("project-view-rename", Some("project_view_rename"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["name"], ""),
        form(InputKind::Selector, &["new_name"], ""),
        form(InputKind::Selector, &["expected_revision"], "Required; current view config revision."),
    ], ""),
    command("findings-view", Some("findings_view"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["name"], ""),
        form(InputKind::AuditDocument, &["audit"], "Audit JSON, inline audit object, or retained scan.v1."),
        form(InputKind::Selector, &["offset"], "Optional stable finding-view page offset."),
    ], ""),
    command("project-priorities", Some("project_priorities"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["policy"], "Optional data-only priority policy; preview by default. Apply requires expected_revision."),
    ], ""),
    command("project-policy", Some("project_policy"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["policy"], "Optional data-only policy; preview by default. Apply requires expected_revision."),
    ], ""),
    command("project-prepare", Some("project_prepare"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["template"], "Optional data-only project template."),
        form(InputKind::InlineJson, &["competitors"], "Optional bounded competitor inputs."),
    ], ""),
    command("project-start", Some("project_start"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::LiveUrl, &["target"], ""),
        form(InputKind::InlineJson, &["facts"], "Optional supplied project facts."),
        form(InputKind::InlineJson, &["template"], "Optional data-only project template."),
        form(InputKind::InlineJson, &["competitors"], "Optional bounded competitor inputs."),
    ], ""),
    command("project-competitors-add", Some("project_competitors_add"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["competitors"], "Required list of candidates with url, source and observed_at."),
        form(InputKind::LiveUrl, &["url"], "Shorthand: one candidate URL, same as a one-item list."),
    ], ""),
    command("schedule-show", Some("schedule_show"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("schedule-set", Some("schedule_set"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["expected_revision"], "Required; current schedule revision."),
        form(InputKind::InlineJson, &["settings"], "Partial schedule.json settings to merge."),
    ], ""),
    command("schedule-run-due", Some("schedule_run_due"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("schedule-install", Some("schedule_install"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["kind"], "launchd, cron or windows."),
        form(InputKind::LocalFile, &["dest", "python"], ""),
    ], ""),
    command("schedule-uninstall", Some("schedule_uninstall"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["kind"], ""),
        form(InputKind::LocalFile, &["dest"], ""),
    ], ""),
    command("project-competitors-list", Some("project_competitors_list"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("project-competitors-remove", Some("project_competitors_remove"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::LiveUrl, &["url"], "The recorded competitor URL to drop from the shortlist."),
    ], ""),
    command("skill-list", Some("skill_list"), &[
        form(InputKind::NoInput, &[], ""),
    ], ""),
    command("skill-show", Some("skill_show"), &[
        form(InputKind::Selector, &["name"], ""),
    ], ""),
    command("scenario-list", Some("scenario_list"), &[
        form(InputKind::NoInput, &[], ""),
    ], ""),
    command("scenario-show", Some("scenario_show"), &[
        form(InputKind::Selector, &["name"], ""),
    ], ""),
    command("provider-registry", Some("provider_registry"), &[
        form(InputKind::NoInput, &[], ""),
    ], ""),
    command("provider-readiness", Some("provider_readiness"), &[
        form(InputKind::InlineJson, &["provider", "operation"], "Offline readiness and operation discovery; no provider requests."),
    ], ""),
];
