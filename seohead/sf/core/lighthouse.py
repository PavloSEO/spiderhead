"""Lighthouse audit correspondence (issue #59).

Lighthouse (github.com/GoogleChrome/lighthouse, Apache-2.0) is the audit
engine behind PageSpeed Insights. Every audit ships a stable ``id`` plus a
public documentation URL; where one of our checks independently reimplements
a documented rule *from its description* (never from Lighthouse's source —
see PROVENANCE.md/THIRD_PARTY_NOTICES.md on the MIT/Apache-2.0 boundary), this
module records the correspondence so the claim is inspectable and testable,
per the issue's acceptance criteria.

``LIGHTHOUSE_AUDIT_IDS`` is the ground truth ``tests/test_lighthouse_map.py``
checks every cited id against: every ``id: '...'`` a ``static get meta()``
declares under ``core/audits/**`` in GoogleChrome/lighthouse. Regenerate it
with the snippet in ``LIGHTHOUSE_SNAPSHOT`` if the mapping test ever needs a
newer snapshot.

Two Lighthouse audits named in the issue (``uses-text-compression``,
``uses-long-cache-ttl``) plus ``viewport`` and ``modern-http`` no longer exist
at that snapshot: Lighthouse's 2025 "Insights" migration folded them into
trace-gated successors (``document-latency-insight``, ``cache-insight``,
``viewport-insight``, ``modern-http-insight``) that read a performance trace
Lighthouse itself records. Our checks below never run a trace; each one
reimplements the *retired* classic audit's static rule from its own
description (still live at the linked ``doc_url``) against the response and
markup a crawl already has. A check's ``note`` says so explicitly, and the
hard rule from the issue still holds: none of this is, or can be mistaken
for, that Insight's actual score.
"""

from __future__ import annotations

LIGHTHOUSE_SNAPSHOT = "GoogleChrome/lighthouse @ v13.4.1 (2026-07-20), core/audits/**"

# Every audit id Lighthouse's own source defines at LIGHTHOUSE_SNAPSHOT.
# Regenerate with (from a checkout of the tagged release):
#   grep -rEoh "id:\s*'[a-zA-Z0-9_-]+'" core/audits | sed -E "s/id:\s*'([^']+)'/\1/" | sort -u
LIGHTHOUSE_AUDIT_IDS: frozenset[str] = frozenset(
    {
        "accesskeys",
        "agent-accessibility-tree",
        "aria-allowed-attr",
        "aria-allowed-role",
        "aria-command-name",
        "aria-conditional-attr",
        "aria-deprecated-role",
        "aria-dialog-name",
        "aria-hidden-body",
        "aria-hidden-focus",
        "aria-input-field-name",
        "aria-meter-name",
        "aria-progressbar-name",
        "aria-prohibited-attr",
        "aria-required-attr",
        "aria-required-children",
        "aria-required-parent",
        "aria-roles",
        "aria-text",
        "aria-toggle-field-name",
        "aria-tooltip-name",
        "aria-treeitem-name",
        "aria-valid-attr",
        "aria-valid-attr-value",
        "autocomplete",
        "autocomplete-valid",
        "baseline",
        "bf-cache",
        "bootup-time",
        "button-name",
        "bypass",
        "cache-insight",
        "canonical",
        "charset",
        "clickjacking-mitigation",
        "cls-culprits-insight",
        "color-contrast",
        "crawlable-anchors",
        "csp-xss",
        "cumulative-layout-shift",
        "custom-controls-labels",
        "custom-controls-roles",
        "definition-list",
        "deprecations",
        "diagnostics",
        "dlitem",
        "doctype",
        "document-latency-insight",
        "document-title",
        "dom-size-insight",
        "duplicate-id-aria",
        "duplicated-javascript-insight",
        "empty-heading",
        "errors-in-console",
        "final-screenshot",
        "first-contentful-paint",
        "focus-traps",
        "focusable-controls",
        "font-display-insight",
        "forced-reflow-insight",
        "form-field-multiple-labels",
        "frame-title",
        "geolocation-on-start",
        "has-hsts",
        "heading-order",
        "hreflang",
        "html-has-lang",
        "html-lang-valid",
        "html-xml-lang-mismatch",
        "http-status-code",
        "identical-links-same-purpose",
        "image-alt",
        "image-aspect-ratio",
        "image-delivery-insight",
        "image-redundant-alt",
        "image-size-responsive",
        "inp-breakdown-insight",
        "input-button-name",
        "input-image-alt",
        "inspector-issues",
        "interaction-to-next-paint",
        "interactive",
        "interactive-element-affordance",
        "is-crawlable",
        "is-on-https",
        "js-libraries",
        "label",
        "label-content-name-mismatch",
        "landmark-one-main",
        "largest-contentful-paint",
        "layout-shifts",
        "lcp-breakdown-insight",
        "lcp-discovery-insight",
        "legacy-javascript-insight",
        "link-in-text-block",
        "link-name",
        "link-text",
        "list",
        "listitem",
        "llms-txt",
        "logical-tab-order",
        "long-tasks",
        "main-thread-tasks",
        "mainthread-work-breakdown",
        "managed-focus",
        "max-potential-fid",
        "meta-description",
        "meta-refresh",
        "meta-viewport",
        "metrics",
        "modern-http-insight",
        "network-dependency-tree-insight",
        "network-requests",
        "network-rtt",
        "network-server-latency",
        "non-composited-animations",
        "notification-on-start",
        "object-alt",
        "offscreen-content-hidden",
        "oopif-iframe-test-audit",
        "origin-isolation",
        "paste-preventing-inputs",
        "predictive-perf",
        "presentation-role-conflict",
        "redirects",
        "redirects-http",
        "render-blocking-insight",
        "resource-summary",
        "robots-txt",
        "screenshot-thumbnails",
        "script-treemap-data",
        "select-name",
        "server-response-time",
        "skip-link",
        "slow-css-selector-insight",
        "speed-index",
        "structured-data",
        "svg-img-alt",
        "tabindex",
        "table-duplicate-name",
        "table-fake-caption",
        "target-size",
        "td-has-header",
        "td-headers-attr",
        "th-has-data-cells",
        "third-parties-insight",
        "third-party-cookies",
        "total-blocking-time",
        "total-byte-weight",
        "trusted-types-xss",
        "unminified-css",
        "unminified-javascript",
        "unsized-images",
        "unused-css-rules",
        "unused-javascript",
        "use-landmarks",
        "user-timings",
        "valid-lang",
        "valid-source-maps",
        "video-caption",
        "viewport-insight",
        "visual-order-follows-dom",
        "webmcp-form-coverage",
        "webmcp-registered-tools",
        "webmcp-schema-validity",
    }
)

# check_id -> Lighthouse correspondence. Each note distinguishes independently
# implemented static rules from explicitly supplied Lighthouse lab results.
LIGHTHOUSE_MAP: dict[str, dict[str, str]] = {
    "MISSING_CHARSET": {
        "audit_id": "charset",
        "doc_url": "https://developer.chrome.com/docs/lighthouse/best-practices/charset/",
        "note": (
            "A character encoding declared via the Content-Type response "
            "header or an early <meta> tag, reimplemented from the audit's "
            "documented rule."
        ),
    },
    "MISSING_DOCTYPE": {
        "audit_id": "doctype",
        "doc_url": "https://developer.chrome.com/docs/lighthouse/best-practices/doctype/",
        "note": (
            "The document must declare exactly `<!DOCTYPE html>` with no "
            "PUBLIC/SYSTEM identifier, or the page renders in quirks mode."
        ),
    },
    "VIEWPORT_MISSING": {
        "audit_id": "viewport-insight",
        "doc_url": "https://developer.chrome.com/docs/lighthouse/pwa/viewport/",
        "note": (
            "Reimplements the retired classic `viewport` audit's static rule "
            "(a <meta name=viewport> tag with `width` or `initial-scale>=1`) "
            "from its documented description. Lighthouse's current "
            "`viewport-insight` computes the same verdict from a performance "
            "trace; this check never runs one, and its result is not that "
            "Insight's score."
        ),
    },
    "NO_COMPRESSION": {
        "audit_id": "document-latency-insight",
        "doc_url": "https://developer.chrome.com/docs/lighthouse/performance/uses-text-compression/",
        "note": (
            "Reimplements the retired classic `uses-text-compression` "
            "audit's static rule (the crawled HTML document itself served "
            "without gzip/br/deflate, above its own 1400-byte/10% ignore "
            "threshold) from its documented description. Lighthouse folds "
            "this into the trace-based `document-latency-insight` (which "
            "also covers redirects and server response time, not "
            "reimplemented here); this check never runs a trace, and its "
            "result is not that Insight's score."
        ),
    },
    # Correspondence only — no new code. See rules.check_tech_extra.
    "HTTP1_ONLY": {
        "audit_id": "modern-http-insight",
        "doc_url": "https://developer.chrome.com/docs/performance/insights/modern-http",
        "note": (
            "Already covers this audit's rule (the page's own HTTP version, "
            "read straight from Screaming Frog's `HTTP Version` column). "
            "Lighthouse's current `modern-http-insight` needs a trace; this "
            "existing check does not and predates this correspondence."
        ),
    },
    # Correspondence only — no new code. See rules.check_native_exports.
    "IMG_MISSING_DIMENSIONS": {
        "audit_id": "unsized-images",
        "doc_url": "https://web.dev/articles/optimize-cls#images_without_dimensions",
        "note": (
            "Already covers this audit's rule (images missing explicit "
            "width/height, which is a CLS risk on the served markup). "
            "`unsized-images` is still a classic, non-trace Lighthouse audit."
        ),
    },
    # Imported lab verdicts: no Lighthouse implementation or score calculation is copied.
    "LCP_DISCOVERY": {
        "audit_id": "lcp-discovery-insight",
        "doc_url": "https://developer.chrome.com/docs/performance/insights/lcp-discovery",
        "note": "Reads the supplied matching per-URL Lighthouse JSON lab result, with input hash and capture provenance. Missing, failed and unscored audits remain unavailable; this is not a native trace or field Core Web Vitals.",
    },
    "NETWORK_DEPENDENCY_TREE": {
        "audit_id": "network-dependency-tree-insight",
        "doc_url": "https://developer.chrome.com/docs/performance/insights/network-dependency-tree",
        "note": "Reads the supplied matching per-URL Lighthouse JSON lab result, with input hash and capture provenance. Missing, failed and unscored audits remain unavailable; this is not a native trace or field Core Web Vitals.",
    },
    "LAYOUT_SHIFT_CULPRITS": {
        "audit_id": "cls-culprits-insight",
        "doc_url": "https://developer.chrome.com/docs/performance/insights/cls-culprits",
        "note": "Reads the supplied matching per-URL Lighthouse JSON lab result, with input hash and capture provenance. Missing, failed and unscored audits remain unavailable; this is not a native trace or field Core Web Vitals.",
    },
    "IMAGE_DELIVERY": {
        "audit_id": "image-delivery-insight",
        "doc_url": "https://developer.chrome.com/docs/performance/insights/image-delivery",
        "note": "Reads the supplied matching per-URL Lighthouse JSON lab result, with input hash and capture provenance. Missing, failed and unscored audits remain unavailable; this is not a native trace or field Core Web Vitals.",
    },
    "FORCED_REFLOW": {
        "audit_id": "forced-reflow-insight",
        "doc_url": "https://developer.chrome.com/docs/performance/insights/forced-reflow",
        "note": "Reads the supplied matching per-URL Lighthouse JSON lab result, with input hash and capture provenance. Missing, failed and unscored audits remain unavailable; this is not a native trace or field Core Web Vitals.",
    },
    "JS_EXECUTION_TIME": {
        "audit_id": "bootup-time",
        "doc_url": "https://developer.chrome.com/docs/lighthouse/performance/bootup-time/",
        "note": "Reads the supplied matching per-URL Lighthouse JSON lab result, with input hash and capture provenance. Missing, failed and unscored audits remain unavailable; this is not a native trace or field Core Web Vitals.",
    },
}
