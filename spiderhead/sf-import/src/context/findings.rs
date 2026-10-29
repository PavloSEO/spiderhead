//! Config-aware check lifecycle: `enabled`, `severity`, `add`,
//! `add_site_wide`, `add_group`, `skip_unsupported`, `retract`, `skip`.

use serde_json::Value;
use spiderhead_formats::models::{Group, Issue, SkippedCheck};
use std::collections::HashSet;

use super::{AddOpts, AuditContext, ContextError, GroupsStore, IssuesStore};

impl AuditContext {
    // -- config-aware helpers ------------------------------------------------

    /// Whether a check already has a finding or a skip reason.
    pub fn settled(&self, check_id: &str) -> bool {
        self.fired_ids.contains(check_id) || self.skipped_ids.contains(check_id)
    }

    /// `config["checks"][check_id]` when both levels are objects.
    fn check_cfg<'a>(&'a self, check_id: &str) -> Option<&'a Value> {
        self.config
            .get("checks")
            .and_then(Value::as_object)
            .and_then(|c| c.get(check_id))
    }

    /// `enabled`: the configured `enabled` value, raw — reference returns
    /// `cfg.get("enabled", True)` unconverted, so a `"yes"` stays a string.
    /// Truthiness is the caller's concern (`enabled_flag`).
    pub fn enabled(&self, check_id: &str) -> Value {
        self.check_cfg(check_id)
            .and_then(|c| c.get("enabled"))
            .cloned()
            .unwrap_or(Value::Bool(true))
    }

    /// `if self.enabled(check_id)`: reference truthiness of the raw config value.
    fn enabled_flag(&self, check_id: &str) -> bool {
        crate::pyvalue::truthy(&self.enabled(check_id))
    }

    /// `severity`: severity override, then the check's configured severity,
    /// then the registry default. Configured values are converted with
    /// reference `str()` semantics (a non-string override does not crash).
    pub fn severity(&self, check_id: &str) -> String {
        if let Some(v) = self
            .config
            .get("severity_overrides")
            .and_then(Value::as_object)
            .and_then(|o| o.get(check_id))
        {
            return crate::pyvalue::str_or_dumps(v);
        }
        if let Some(v) = self.check_cfg(check_id).and_then(|c| c.get("severity")) {
            return crate::pyvalue::str_or_dumps(v);
        }
        self.registry
            .meta_or_fallback(check_id)
            .severity
            .as_str()
            .to_owned()
    }

    /// `add`: create and record an `Issue`, filling defaults from the
    /// registry. Returns `None` for a disabled check.
    pub fn add(&mut self, check_id: &str, kw: AddOpts) -> Result<Option<Issue>, ContextError> {
        if !self.enabled_flag(check_id) {
            return Ok(None);
        }
        let meta = self.registry.meta_or_fallback(check_id);
        let mut issue = Issue::new(
            check_id,
            self.severity(check_id),
            kw.source.unwrap_or(meta.source),
            kw.message.unwrap_or(meta.message),
        );
        issue.id = kw.id;
        issue.fingerprint = kw.fingerprint;
        issue.target_url = kw.target_url;
        issue.status_code = kw.status_code;
        if let Some(n) = kw.occurrences_count {
            issue.occurrences_count = n;
        }
        issue.locations = kw.locations;
        issue.details = kw.details;
        issue.group_id = kw.group_id;
        issue.fix_hint = kw.fix_hint.unwrap_or(meta.fix);
        issue.evidence = kw.evidence;
        match &mut self.issues {
            IssuesStore::Mem(v) => v.push(issue.clone()),
            IssuesStore::Disk(d) => d.append(&issue)?,
        }
        // A check with more than one evidence source (e.g. BROKEN_EXTERNAL_LINK,
        // declared for both inlinks_4xx and inlinks_5xx) can have one source
        // missing and declare a skip before a sibling source later fires it, or
        // the reverse. Either way, a check that ever fires has evidence and is
        // no longer "skipped" -- retract any earlier skip so ctx.skipped and
        // ctx.issues can never name the same check_id at once.
        self.fired_ids.insert(check_id.to_owned());
        if self.skipped_ids.remove(check_id) {
            self.skipped.retain(|s| s.id != check_id);
        }
        Ok(Some(issue))
    }

    /// `add_site_wide`: one finding for a defect the listed pages share,
    /// carrying the page count. A lone page keeps its own row.
    pub fn add_site_wide(
        &mut self,
        check_id: &str,
        urls: &[String],
        details: super::Object,
        occurrences: Option<i64>,
    ) -> Result<(), ContextError> {
        if urls.len() == 1 {
            let mut opts = AddOpts {
                target_url: Some(urls[0].clone()),
                details,
                ..AddOpts::default()
            };
            opts.occurrences_count = occurrences;
            self.add(check_id, opts)?;
            return Ok(());
        }
        let mut ordered = urls.to_vec();
        ordered.sort();
        let group = self.add_group(check_id, None, ordered.clone())?;
        // ``occurrences`` is per page; the group keeps the total so sums over
        // findings stay true.
        let total = occurrences.map_or(ordered.len() as i64, |n| n * ordered.len() as i64);
        let mut d = details;
        d.insert("page_count".into(), Value::from(ordered.len()));
        d.insert(
            "sample_urls".into(),
            Value::Array(ordered.iter().take(5).cloned().map(Value::String).collect()),
        );
        self.add(
            check_id,
            AddOpts {
                target_url: ordered.first().cloned(),
                occurrences_count: Some(total),
                group_id: group.map(|g| g.group_id),
                details: d,
                ..AddOpts::default()
            },
        )?;
        Ok(())
    }

    /// `add_group`: record a `Group` named `GRP-<prefix>-<seq>`.
    pub fn add_group(
        &mut self,
        check_id: &str,
        value: Option<String>,
        urls: Vec<String>,
    ) -> Result<Option<Group>, ContextError> {
        if !self.enabled_flag(check_id) {
            return Ok(None);
        }
        let prefix: String = check_id
            .split('_')
            .next()
            .unwrap_or_default()
            .chars()
            .take(6)
            .collect::<String>()
            .to_uppercase();
        self.group_seq += 1;
        let group = Group {
            group_id: format!("GRP-{prefix}-{:04}", self.group_seq),
            check: check_id.to_owned(),
            value,
            count: urls.len() as i64,
            urls,
        };
        match &mut self.groups {
            GroupsStore::Mem(v) => v.push(group.clone()),
            GroupsStore::Disk(d) => d.append(&group)?,
        }
        Ok(Some(group))
    }

    /// `skip_unsupported`: skip every check whose declared export frame is
    /// absent. A check in `native` is measured from the stored scan itself,
    /// so the absence of its export says nothing and is not declared.
    pub fn skip_unsupported(&mut self, available: &HashSet<&str>, native: &HashSet<&str>) {
        let ids: Vec<String> = self
            .registry
            .requires()
            .iter()
            .map(|r| r.id.clone())
            .collect();
        for check_id in ids {
            if native.contains(check_id.as_str()) {
                continue;
            }
            let gone = self.registry.missing_requirements(&check_id, available);
            if gone.is_empty() {
                continue;
            }
            let reason = if self.scan_con.is_some() {
                "not measured by this native crawl".to_owned()
            } else {
                format!("missing export: {}", gone.join(", "))
            };
            self.skip(&check_id, &reason);
        }
    }

    /// `retract`: withdraw a check's findings and record why it cannot be
    /// answered. `skip` refuses a check that already fired, so moving a check
    /// between the buckets has to be one operation.
    pub fn retract(&mut self, check_id: &str, reason: &str) -> Result<(), ContextError> {
        match &mut self.issues {
            IssuesStore::Mem(v) => v.retain(|i| i.check != check_id),
            IssuesStore::Disk(d) => {
                d.remove_check(check_id);
            }
        }
        self.fired_ids.remove(check_id);
        self.skip(check_id, reason);
        Ok(())
    }

    /// `skip`: declare a check unanswered with a reason (idempotent; a check
    /// that already fired keeps its evidence and is not skipped).
    pub fn skip(&mut self, check_id: &str, reason: &str) {
        if self.fired_ids.contains(check_id) || self.skipped_ids.contains(check_id) {
            return;
        }
        self.skipped_ids.insert(check_id.to_owned());
        self.skipped.push(SkippedCheck {
            id: check_id.to_owned(),
            reason: reason.to_owned(),
        });
    }
}
