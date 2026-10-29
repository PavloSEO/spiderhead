//!
//! The reference tests also read `.claude/skills/js-render-check/SKILL.md` and
//! `docs/scenarios/*.md` from the reference repository. Those files are not part of this
//! repository, so the file-content assertions are deferred (see the slice report), and only the
//! check-registry assertions are ported here.

use spiderhead_formats::check_registry::{builtin, Severity};

#[test]
fn notranslate_remains_notice_and_names_search_feature_opt_out() {
    let registry = builtin().expect("embedded check registry loads");
    let check = registry
        .get("NOTRANSLATE")
        .expect("NOTRANSLATE is registered");

    assert_eq!(check.severity, Severity::Notice);
    assert_eq!(
        check.fix.as_deref(),
        Some(
            "Confirm that opting out of translation-related Google Search features is intentional."
        )
    );
}

#[test]
fn encoded_space_guidance_keeps_the_warning_but_excludes_mechanical_deletion() {
    let registry = builtin().expect("embedded check registry loads");
    let check = registry
        .get("URL_CONTAINS_SPACE")
        .expect("URL_CONTAINS_SPACE is registered");

    assert_eq!(check.severity, Severity::Warning);
    let fix = check
        .fix
        .as_deref()
        .expect("URL_CONTAINS_SPACE has a fix text");
    assert!(fix.contains("q=red%20shoes"), "fix: {fix}");
    assert!(
        fix.contains("do not mechanically remove percent-encoded values"),
        "fix: {fix}"
    );
}
