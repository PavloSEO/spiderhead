//! Declined and partly covered rows: the accessibility rules and the AMP rules.
//!
//! The full rule lists stay here so a reader can see exactly what is declined.

use super::Entry;

pub(super) const ACCESSIBILITY_NOTE: &str = "First static HTML accessibility checks are available. The remaining WCAG/axe-core rules need a rendered DOM or computed styles and are not measured by this subset.";
pub(super) const AMP_NOTE: &str = "AMP is effectively retired: Google dropped the Top Stories carousel requirement in 2021 and the format is in maintenance. The AMP validator's document model is a separate project. Structural source checks are available; the complete validator rule set remains unavailable pending an owner licence decision.";

/// Accessibility rule names, in catalogue order.
pub(super) const ACCESSIBILITY_RULES: &[&str] = &[
    "Best Practice - Accesskey Attribute Value Must Be Unique",
    "Best Practice - Elements Must Not Have Tabindex Greater Than Zero",
    "Best Practice - ARIA Dialog & Alertdialog Require Accessible Name",
    "Best Practice - ARIA Treeitem Nodes Require Accessible Name",
    "Best Practice - Role=text Should Have No Focusable Descendants",
    "Best Practice - Form Elements Should Have Visible Label",
    "Best Practice - Frames Should Be Tested With axe-core",
    "Best Practice - Scope Attribute Should Be Used Correctly On Tables",
    "WCAG 2.0 A - Scrollable Region Requires Keyboard Access",
    "WCAG 2.0 A - Required ARIA Attributes Must Be Provided",
    "WCAG 2.0 A - ARIA Attribute Must Be Used As Specified For Role",
    "WCAG 2.0 A - ARIA Attributes Require Valid Values",
    "WCAG 2.0 A - ARIA Attributes Require Valid Names",
    "WCAG 2.0 A - ARIA Commands Require Accessible Name",
    "WCAG 2.0 A - ARIA Input Fields Require Accessible Name",
    "WCAG 2.0 A - ARIA Meter Nodes Require Accessible Name",
    "WCAG 2.0 A - ARIA Progressbar Nodes Require Accessible Name",
    "WCAG 2.0 A - ARIA Roles Must Be Contained By Required Parent",
    "WCAG 2.0 A - ARIA Roles Require Valid Values",
    "WCAG 2.0 A - ARIA Toggle Fields Require Accessible Name",
    "WCAG 2.0 A - ARIA Tooltip Nodes Require Accessible Name",
    "WCAG 2.0 A - Certain ARIA Roles Must Contain Specific Children",
    "WCAG 2.0 A - Aria-braille Require Non-braille Equivalent",
    "WCAG 2.0 A - Aria-hidden Elements Contains Focusable Elements",
    "WCAG 2.0 A - Aria-hidden=true Must Not Be Used In <body>",
    "WCAG 2.0 A - Elements Must Only Use Permitted ARIA Attributes",
    "WCAG 2.0 A - Elements Must Use Allowed ARIA Attributes",
    "WCAG 2.0 A - IDs Used In ARIA & Labels Must Be Unique",
    "WCAG 2.0 A - Page Requires Means To Bypass Repeated Blocks",
    "WCAG 2.0 A - Form <input> Elements Require Labels",
    "WCAG 2.0 A - Frames Require Title Attribute",
    "WCAG 2.0 A - Frames Require Unique Title Attribute",
    "WCAG 2.0 A - Frames With Focusable Content Must Not Use tabindex=-1",
    "WCAG 2.0 A - Page Must Contain <title>",
    "WCAG 2.0 A - HTML Element Lang Attribute Value Must Be Valid",
    "WCAG 2.0 A - HTML Element Requires Lang Attribute",
    "WCAG 2.0 A - Image Button Requires Alternate Text",
    "WCAG 2.0 A - Images Require Alternate Text",
    "WCAG 2.0 A - <object> Elements Require Alternate Text",
    "WCAG 2.0 A - Active <area> Elements Require Alternate Text",
    "WCAG 2.0 A - Elements Marked role=img Require Alternate Text",
    "WCAG 2.0 A - SVG Images & Graphics Require Accessible Text",
    "WCAG 2.0 A - <video> Elements Require <track> For Captions",
    "WCAG 2.0 A - <video> or <audio> Elements Must Not Auto-play",
    "WCAG 2.0 A - Buttons Require Discernible Text",
    "WCAG 2.0 A - Input Buttons Require Discernible Text",
    "WCAG 2.0 A - Links Require Discernible Text",
    "WCAG 2.0 A - Links Must Be Distinguishable",
    "WCAG 2.0 A - Select Element Requires Accessible Name",
    "WCAG 2.0 A - Summary Elements Require Discernible Text",
    "WCAG 2.0 A - Deprecated <marquee> Element Must Not Be Used",
    "WCAG 2.0 A - Interactive Controls Must Not Be Nested",
    "WCAG 2.0 A - List Items Must Be Contained In List Elements",
    "WCAG 2.0 A - Lists Must Only Contain <li> Content Elements",
    "WCAG 2.0 A - <dt> & <dd> Elements Must Be Contained by <dl>",
    "WCAG 2.0 A - <dl> Must Only Have Ordered <dt> & <dd> Groups",
    "WCAG 2.0 A - <blink> Elements Deprecated & Must Not Be Used",
    "WCAG 2.0 A - <th> Element Requires Associated Data Cells",
    "WCAG 2.0 A - Table Header Attr Must Refer To Cell In Same Table",
    "WCAG 2.0 AA - Meta Viewport Zoom & Scaling Disabled",
    "WCAG 2.0 AA - Lang Attribute Requires Valid Value",
    "WCAG 2.0 AA - Text Requires Higher Color Contrast to Background",
    "WCAG 2.0 AAA - Text Requires Higher Color Contrast Ratio",
    "WCAG 2.1 AA - Autocomplete Attribute Must Be Used Correctly",
    "WCAG 2.1 AA - Inline Text Spacing Must Be Adjustable",
    "WCAG 2.2 AA - Touch Targets Require Sufficient Size & Spacing",
    "Best Practice - Skip-link Target Should Exist & Be Focusable",
    "Best Practice - All Page Content Must Be Contained By Landmarks",
    "Best Practice - Page Requires One Main Landmark",
    "Best Practice - Page Must Not Have More Than One Banner Landmark",
    "Best Practice - Banner Landmark Must Not Be In Another Landmark",
    "Best Practice - Page Must Not Have Multiple Contentinfo Landmarks",
    "Best Practice - Page Requires At Most One Main Landmark",
    "Best Practice - Complementary Landmarks & Asides Must Be Top Level",
    "Best Practice - Contentinfo Landmark Must Be Top Level Landmark",
    "Best Practice - Main Landmark Must Not Be In Another Landmark",
    "Best Practice - Landmarks Require Unique Role Or Accessible Name",
    "Best Practice - Page Must Contain <h1>",
    "Best Practice - Heading Levels Should Only Increase By One",
    "WCAG 2.0 A - Form Field Must Not Have Multiple Label Elements",
    "WCAG 2.0 A - HTML Lang & XML Lang Value Should Match",
    "Best Practice - Ensure Elements Marked Presentational Are Ignored",
    "Best Practice - ARIA Role Should Be Appropriate For Element",
    "Best Practice - Headings Should Not Be Empty",
    "Best Practice - Meta Viewport Should Allow Zoom & Scale Up to 500%",
    "Best Practice - Alt Text Should Not Be Repeated As Text",
    "Best Practice - Table Headers Require Discernible Text",
    "Best Practice - Table With Identical Summary & Caption Text",
    "WCAG 2.0 A - Deprecated ARIA Roles Must Not Be Used",
    "WCAG 2.0 A - Server-Side Image Maps Must Not Be Used",
    "WCAG 2.0 AAA - Delayed Meta Refresh Must Not Be Used",
    "WCAG 2.0 AAA - Links With Same Accessible Name",
];

/// Accessibility rules answered by the static DOM check, with their check ids.
pub(super) const STATIC_ACCESSIBILITY: &[(&str, &[&str])] = &[
    (
        "WCAG 2.0 A - HTML Element Requires Lang Attribute",
        &["A11Y_HTML_LANG_MISSING"],
    ),
    (
        "WCAG 2.0 A - Form <input> Elements Require Labels",
        &["A11Y_FORM_LABEL_MISSING"],
    ),
    (
        "WCAG 2.0 A - Select Element Requires Accessible Name",
        &["A11Y_FORM_LABEL_MISSING"],
    ),
    (
        "WCAG 2.0 A - Images Require Alternate Text",
        &["A11Y_IMAGE_ALT_MISSING"],
    ),
];

/// AMP pairing rules and the check id each one reads.
pub(super) const AMP_PAIRING_RULES: &[(&str, &[&str])] = &[
    ("Non-200 Response", &["AMP_NON_200"]),
    ("Missing Non-AMP Return Link", &["AMP_MISSING_RETURN_LINK"]),
    ("Missing Canonical to Non-AMP", &["AMP_MISSING_RETURN_LINK"]),
    ("Non-Indexable Canonical", &["AMP_NON_INDEXABLE_CANONICAL"]),
    ("Missing Canonical", &["AMP_MISSING_CANONICAL"]),
    ("Indexable", &["AMP_INDEXABLE"]),
];

/// AMP structural rule names, in catalogue order.
pub(super) const AMP_RULES: &[&str] = &[
    "Missing <html amp> Tag",
    "Missing/Invalid Doctype HTML Tag",
    "Missing Head Tag",
    "Missing Body Tag",
    "Missing/Invalid Meta Charset Tag",
    "Missing/Invalid Meta Viewport Tag",
    "Missing/Invalid AMP Script",
    "Missing/Invalid AMP Boilerplate",
    "Contains Disallowed HTML",
    "Other Validation Errors",
];

/// Structural AMP rules answered by registry checks, with their check ids and note.
pub(super) const STRUCTURE_AMP: &[(&str, &[&str], &str)] = &[
    ("Missing <html amp> Tag", &["AMP_MISSING_HTML_ATTRIBUTE"], "read from the retained static markup of the AMP page an amphtml pairing declares; the validator's verdict rides along as provenance and validator-level AMP rules stay declined"),
    ("Missing/Invalid Doctype HTML Tag", &["MISSING_DOCTYPE", "INVALID_DOCTYPE"], "read on every crawled document as HTML hygiene; the full AMP validator stays declined"),
    ("Missing Head Tag", &["HEAD_MISSING"], "read from the retained document skeleton of every crawled page"),
    ("Missing Body Tag", &["BODY_MISSING"], "read from the retained document skeleton of every crawled page"),
    ("Missing/Invalid Meta Charset Tag", &["MISSING_CHARSET", "INVALID_CHARSET"], "declared encoding of every crawled page; the full AMP validator stays declined"),
    ("Missing/Invalid Meta Viewport Tag", &["VIEWPORT_MISSING", "INVALID_VIEWPORT"], "viewport of every crawled page; the full AMP validator stays declined"),
    ("Missing/Invalid AMP Script", &["AMP_SCRIPT_MISSING"], "checked on the AMP page an amphtml pairing declares; full AMP validity stays declined"),
    ("Missing/Invalid AMP Boilerplate", &["AMP_BOILERPLATE_INVALID"], "checked on the AMP page an amphtml pairing declares; full AMP validity stays declined"),
];

/// Note on the AMP pairing rows.
pub(super) const AMP_PAIRING_NOTE: &str =
    "pairing read from the AMP target the crawl captured (#1020)";

/// Note on the static accessibility rows.
pub(super) const STATIC_ACCESSIBILITY_NOTE: &str = "measured on the retained static DOM, including label, aria and decorative-alt semantics (#1577)";

/// The accessibility category: one entry per rule, in catalogue order.
///
/// Four rules are full static-DOM measurements; every other rule is declined as a decision
/// (rendered-DOM tooling and computed styles are not measured here).
pub(super) fn accessibility() -> Vec<Entry> {
    ACCESSIBILITY_RULES
        .iter()
        .map(|&name| match static_accessibility(name) {
            Some(refs) => Entry::check(name, refs).note(STATIC_ACCESSIBILITY_NOTE),
            None => Entry::out_of_scope(name).note(ACCESSIBILITY_NOTE),
        })
        .collect()
}

fn static_accessibility(name: &str) -> Option<&'static [&'static str]> {
    STATIC_ACCESSIBILITY
        .iter()
        .find(|(rule, _)| *rule == name)
        .map(|(_, refs)| *refs)
}

/// The AMP category: six pairing rules read from the crawl, then eight structural rows
/// answered by registry checks, and the two rows the validator alone can judge.
pub(super) fn amp() -> Vec<Entry> {
    let pairing = AMP_PAIRING_RULES
        .iter()
        .map(|&(name, refs)| Entry::check(name, refs).note(AMP_PAIRING_NOTE));
    let structural = AMP_RULES.iter().map(|&name| {
        match STRUCTURE_AMP.iter().find(|(rule, _, _)| *rule == name) {
            Some(&(_, refs, note)) => Entry::check(name, refs).note(note),
            None => Entry::out_of_scope(name).note(AMP_NOTE),
        }
    });
    pairing.chain(structural).collect()
}
