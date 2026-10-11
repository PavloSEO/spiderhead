- Native audits now expose render-blocking head markup from complete retained HTML without
  changing the frozen scan.v1 schema. Missing, incomplete and budget-limited bodies remain
  unavailable. Existing head resource detection and SF custom-extraction input are reused.
- Add configurable native observed-navigation and individual resource byte budgets, cache/font
  delivery signals, minification and legacy-script heuristics, and exact same-page script
  duplicates from retained scan.v2 evidence. Unfetched data and incomplete runtime payloads
  remain explicit; responsive declarations are not summed as runtime weight. HTML-only lower
  bounds, scalar coverage counts, font samples and source summaries stay explicit and bounded.
- Collect bounded Chromium JS/CSS precise coverage on requested renders, retain UTF-16
  usage summaries and rendered image-sizing findings, and read them after reopening or
  offline reanalysis. Unsupported engines and missing measurements do not become clean results.
- Accept bounded matching per-URL Lighthouse JSON for LCP discovery, dependency trees,
  layout-shift culprits, image delivery, forced reflow and script execution findings, with
  capture provenance and input hashes. No Lighthouse process, paid provider or field-CWV
  inference is added. Refresh registry, SF coverage and generated reference documentation.
