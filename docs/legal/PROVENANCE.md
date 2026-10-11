# Provenance

This public repository is a history-free release snapshot maintained by Pavel Barushka under the
`PavloSEO` GitHub account. Its first public commit contains only the files intended for users,
contributors, and auditors.

The private development repository remains separate. It contains experiments, discarded plans,
internal research notes, and historical migrations that are not part of the distributed product.
No private commit, branch, tag, release, reflog, client crawl, access log, credential file, paid-API
response, or generated client report is imported into this public history.

## Source boundary

- Product source is the Python package under `seohead/`.
- Public tests use synthetic or reserved-domain fixtures.
- Example crawl exports contain deliberately planted, fictional SEO problems.
- Configuration files contain names and paths for credentials, never credential values.
- The local MCP server uses stdio and does not expose a hosted service.

The project was developed by studying standards, vendor documentation, and compatible open-source
implementations. Publicly relevant inspirations and data licences are listed in
[`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md). GPL and unlicensed code is not included.

## Reproducible claims

Counts shown in `docs/TOOLS.md` and `docs/CHECKS.md` are checked against source registries:

- 175 shared handlers exposed through the CLI and `seo_*` MCP tools;
- five Screaming Frog-specific `sf_*` MCP tools;
- 202 audit checks in the crawl registry;
- 25 technical workflow skills plus eight packaged SEO playbooks;
- About 7,400 offline tests collected by pytest in the current suite.

When these registries change, tests and public counts must change in the same pull request.
