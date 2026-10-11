# Example audit report

`audit.json` and `audit.md` were generated from the exports in `docs/examples/exports/`.
They use the same fixtures as the test suite: a small synthetic site with deliberately
planted defects, including a broken internal link, two H1 elements, duplicate titles,
an oversized page, and thin content.

Reproduce the report:

```bash
seohead sf run --exports-dir docs/examples/exports --out docs/examples --tasks
```

What this example demonstrates:

- `BROKEN_INTERNAL_LINK` — a broken link to `/old-page` (404), with two source
  pages, anchor text, link position (Content/Footer), and link path (XPath);
- `H1_MULTIPLE` — a page with two H1 elements and both heading texts;
- `TITLE_DUPLICATE` — a group of two URLs that share the same title;
- `LARGE_HTML` — an HTML-size outlier relative to the site median;
- `THIN_CONTENT`, `LOW_TEXT_RATIO`, `CANONICAL_MISSING`, `SLOW_RESPONSE`, and
  other checks.

See [the JSON schema reference](../../.claude/skills/sf-analyzer/reference/json_schema.md) for the
complete `audit.json` contract.

## Project skeleton

`project-skeleton/` is a portable project workspace for the same synthetic site, shipped
with its checklist already initialized: `coverage.json` holds one definition per catalogue
item, every one of them `not_run` with no execution record, so `project status` on it
reports coverage counts rather than an uninitialized state. Regenerate that file with
`python scripts/generate_project_skeleton_coverage.py` when the catalogue changes. See
[the projects guide](../PROJECTS.md) for the commands that read and extend it.

`project-task-update.json` and `project-task-comment.json` show synthetic custom-task metadata,
per-competitor URL caps, optimistic revision writes and an attributed comment linked to a retained
finding identity. The reference is illustrative metadata; the example does not assert that the
synthetic scan or finding exists. The reserved example hosts are not contacted.

## Shop test site

`shop-site/` is a loopback furniture shop in three versions with planted, evolving defects and
a generated `ground_truth.json`, for building a realistic three-scan project with the native
crawler. See [shop-site/README.md](shop-site/README.md).
