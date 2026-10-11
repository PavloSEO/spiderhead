- Retain document-bound static AMP and accessibility evidence in existing scan context
  rows, preserving the frozen `scan.v1` SQL schema. Structural AMP findings inspect the
  original doctype, head/body, UTF-8 charset, viewport, runtime script and boilerplate
  declarations; paired AMP targets without an AMP root marker are identified separately.
  Full AMP validator rules remain unavailable pending a licence decision.
- Start accessibility findings for document language, form labels, image alternatives,
  heading jumps and the main landmark. Findings preserve bounded examples and full
  occurrence counts. Computed contrast and rendered axe-core rules remain unmeasured.
- Validate Schema.org Microdata and common RDFa subjects/properties with the bundled
  vocabulary and existing rich-result rules. Retained per-URL errors, warnings and missing
  required properties become native findings. Unsupported RDFa constructs are explicit
  partial evidence, not a clean validation result.
- Serve parsed JSON-LD trees alongside per-block source, position and syntax errors in
  `scan-structured-blocks`; cap wide property trees and expose their limit. Read the latest
  document for a requested URL and representation without changing its scan.
- Preserve partial/unavailable structured evidence without hiding confirmed sibling
  errors. Resolve supported JSON-LD identities against the same document base as HTML
  markup while preserving source JSON; reject scalar/empty JSON blocks as valid graphs
  and share normalized live JSON-LD tag selection across parser and served projections.
