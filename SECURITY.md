# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub private vulnerability
reporting:

<https://github.com/PavloSEO/spiderhead/security/advisories/new>

Include the affected command or MCP tool, version (`spiderhead --version`), minimal reproduction,
expected impact, and any safe mitigation you have already tested. Do not include real credentials,
access logs or client crawl data.

## Security model

SpiderHead is a local CLI, a stdio MCP server and a desktop app. It has no hosted control plane,
user account, telemetry collector, automatic updater, or inbound network port.

The main trust boundaries are:

- remote HTML, XML, headers, redirects, robots.txt, and provider responses are untrusted data;
- URL tools block private and non-public network targets unless explicitly enabled for an
  authorized staging or intranet environment;
- operations that probe paths, rewrite files, verify bots over DNS, or spend provider credits
  require explicit input;
- file-producing tools should run inside a dedicated working directory;
- credentials are loaded from the local environment or configuration and should never appear in
  prompts, issue reports, transcripts, or committed files.

## Supported versions

Security fixes target the latest `0.9.x` release and the current `main` branch. Include the
affected version and commit SHA in reports.

## Out of scope

- findings that require deliberately disabling documented guardrails;
- third-party service outages or inaccurate provider datasets;
- vulnerabilities already fixed by a current direct dependency release;
- SEO disagreements without a confidentiality, integrity, or availability impact.
