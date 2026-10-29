//! AMP pairing checks (#1020): each reads the AMP target a desktop page
//! declares with `<link rel="amphtml">`, and only the AMP pages the crawl
//! actually captured.

use std::collections::HashSet;

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::normalize::norm_url;
use crate::pyvalue::truthy_opt;
use crate::rules::shared::{body_unavailable, has_column, rec};

// AMP pairing codes (#1020). Each reads the AMP target a desktop page declares with
// <link rel="amphtml">, and only the AMP pages the crawl actually captured.
/// `_AMP_CODES`.
const AMP_CODES: [&str; 5] = [
    "AMP_NON_200",
    "AMP_MISSING_CANONICAL",
    "AMP_MISSING_RETURN_LINK",
    "AMP_NON_INDEXABLE_CANONICAL",
    "AMP_INDEXABLE",
];

/// `check_amp_pairing`: AMP pairing checks (#1020): read each declared AMP
/// target's own record.
///
/// A target the crawl never captured is unavailable evidence, not a broken AMP
/// page, so it is named in a skip rather than reported as a finding. Pages are
/// grouped by normalized AMP URL: one AMP page reached from several desktop
/// pages is reported once.
pub fn check_amp_pairing(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if !has_column(ctx, "amphtml") {
        for code in AMP_CODES {
            ctx.skip(code, "no amphtml Link Element column in Internal:All");
        }
        return Ok(());
    }

    let mut targets: IndexMap<String, (String, Vec<String>)> = IndexMap::new();
    for page in ctx.html_pages() {
        let amp = page.map(|p| rec(p).and_then(|r| r.get("amphtml").cloned()));
        if truthy_opt(amp.as_ref()) {
            // `norm_url(amp)` does `(amp or "").strip()` — AttributeError on a
            // non-str, which the check does not catch.
            let Some(amp_str) = amp.as_ref().and_then(Value::as_str) else {
                return Err(ContextError::Value(
                    "expected string for amphtml declaration".to_owned(),
                ));
            };
            let key = norm_url(Some(amp_str))?;
            targets
                .entry(key)
                .or_insert_with(|| (amp_str.to_owned(), Vec::new()))
                .1
                .push(page.url());
        }
    }
    if targets.is_empty() {
        for code in AMP_CODES {
            ctx.skip(code, "no page declares an AMP target (rel=amphtml)");
        }
        return Ok(());
    }

    let mut found: HashSet<&str> = HashSet::new();
    let mut uncaptured = 0i64;
    for (key, (amp_url, desktop_urls)) in targets {
        let rows = ctx.pages_by_norm(&key);
        if rows.is_empty() {
            uncaptured += 1;
            continue;
        }
        let omitted = desktop_urls.len().saturating_sub(20);
        let mut base = Map::new();
        base.insert("amphtml".into(), Value::String(amp_url.clone()));
        base.insert(
            "desktop_urls".into(),
            Value::Array(
                desktop_urls
                    .iter()
                    .take(20)
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        base.insert("desktop_urls_omitted".into(), Value::from(omitted));

        let codes: Vec<Option<i64>> = rows.iter().map(|row| row.map(|p| p.status_code)).collect();
        if codes.iter().any(|c| c.is_some_and(|n| n != 200)) {
            let mut unique: Vec<i64> = codes.iter().flatten().copied().collect();
            unique.sort_unstable();
            unique.dedup();
            let mut details = base.clone();
            details.insert(
                "status_codes".into(),
                Value::Array(unique.iter().map(|n| Value::from(*n)).collect()),
            );
            ctx.add(
                "AMP_NON_200",
                AddOpts {
                    target_url: Some(amp_url.clone()),
                    status_code: if codes.iter().collect::<HashSet<_>>().len() == 1 {
                        codes[0]
                    } else {
                        None
                    },
                    details,
                    ..AddOpts::default()
                },
            )?;
            found.insert("AMP_NON_200");
        }

        for row in &rows {
            let (row_status, row_url, row_indexable) =
                row.map(|p| (p.status_code, p.url.clone(), p.is_indexable()));
            if row_status != Some(200) {
                continue;
            }
            let row_rec = row.map(|p| rec(p).cloned().unwrap_or_default());
            if body_unavailable(&row_rec) {
                continue; // unparsed body: no canonical evidence either way
            }
            let canonical = row_rec.get("canonical");
            if truthy_opt(canonical) && !canonical.is_some_and(Value::is_string) {
                // `norm_url(canonical)` calls `.strip()` — AttributeError on a
                // non-str, which the check does not catch.
                return Err(ContextError::Value(
                    "expected string for canonical".to_owned(),
                ));
            }
            let canonical_str = canonical.and_then(Value::as_str).unwrap_or("");
            if !truthy_opt(canonical) {
                ctx.add(
                    "AMP_MISSING_CANONICAL",
                    AddOpts {
                        target_url: Some(amp_url.clone()),
                        details: base.clone(),
                        ..AddOpts::default()
                    },
                )?;
                found.insert("AMP_MISSING_CANONICAL");
            } else {
                // `norm_url(canonical)` is recomputed per desktop URL in reference
                // — and never called at all when the list is empty.
                let mut matched = false;
                for d in &desktop_urls {
                    if norm_url(Some(canonical_str))? == norm_url(Some(d))? {
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    let mut details = base.clone();
                    details.insert(
                        "canonical".into(),
                        canonical.cloned().unwrap_or(Value::Null),
                    );
                    ctx.add(
                        "AMP_MISSING_RETURN_LINK",
                        AddOpts {
                            target_url: Some(amp_url.clone()),
                            details,
                            ..AddOpts::default()
                        },
                    )?;
                    found.insert("AMP_MISSING_RETURN_LINK");
                }
            }
            if truthy_opt(canonical) && norm_url(Some(canonical_str))? != norm_url(Some(&row_url))?
            {
                let canonical_norm = norm_url(Some(canonical_str))?;
                let canonical_targets = ctx.pages_by_norm(&canonical_norm);
                if !canonical_targets.is_empty()
                    && !canonical_targets
                        .iter()
                        .any(|t| t.map(|p| p.is_indexable()))
                {
                    let mut details = base.clone();
                    details.insert(
                        "canonical".into(),
                        canonical.cloned().unwrap_or(Value::Null),
                    );
                    ctx.add(
                        "AMP_NON_INDEXABLE_CANONICAL",
                        AddOpts {
                            target_url: Some(amp_url.clone()),
                            details,
                            ..AddOpts::default()
                        },
                    )?;
                    found.insert("AMP_NON_INDEXABLE_CANONICAL");
                }
            }
            if row_indexable {
                ctx.add(
                    "AMP_INDEXABLE",
                    AddOpts {
                        target_url: Some(amp_url.clone()),
                        details: base.clone(),
                        ..AddOpts::default()
                    },
                )?;
                found.insert("AMP_INDEXABLE");
            }
        }
    }

    if uncaptured > 0 {
        for code in AMP_CODES {
            if !found.contains(code) {
                ctx.skip(
                    code,
                    &format!(
                        "{uncaptured} AMP target(s) were declared but not captured by the crawl"
                    ),
                );
            }
        }
    }
    Ok(())
}
