# Changelog

## 0.9.0

First public snapshot. Earlier history was a raw prototype and is not published; history starts at 0.9.

- CLI with 250+ commands: log analysis, crawl, parse, projects, data sources, semantics, reports.
- Log analysis: import, summary and bot reports with IP-range, reverse-DNS and ASN verification.
- `spiderhead mcp` runs the stdio MCP server from the same binary; `spiderhead-mcp` stays as an alias.
- Project model: `seo/` folder, `spiderhead init [--site URL]`, autodiscovery from the current directory, `schema_version` in `project.json`.
- Desktop app (early): launcher with log analysis enabled, menu actions to install the command in the terminal and to connect an MCP client.
