- Restore human-report parity between streamed audit.v2 inputs and equivalent audit JSON.
  Markdown now shares one incremental renderer, including unavailable-check explanations,
  exclusion records and trailing coverage notes. XLSX shares one metadata layout and writes
  findings/pages with bounded write-only worksheets, preserving all worksheets, columns,
  filters, charts and source counts. Retained suppressed findings are replayable without
  materializing their collection, and missing run URLs use the same first-page fallback.
- Label PDF audit provenance independently of the shared audit schema: native crawler,
  Screaming Frog exports or an explicitly identified SF collector, and unknown collector.
  A native audit using the Export Analyzer schema no longer receives a Screaming Frog heading.
