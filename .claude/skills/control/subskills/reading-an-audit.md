# Reading an audit honestly: coverage before findings

The most expensive mistake is reading the findings first. A list of problems from a run that
covered a third of the site, with a third of the checks, looks identical to a list from a
complete one.

## The four fields to read, in this order

| Field | What it tells you |
|---|---|
| `run.crawl_finish_reason` | `finished`, or why not — `errors`, `budget`, `duration` |
| `run.crawl_partial` | whether the crawl covered the site |
| `summary.check_coverage` | how many checks *could* run: `checks_fired`, `checks_skipped`, `checks_disabled`, `checks_silent` |
| `summary.health_score_basis` | whether the score compares to anything |

Fired, skipped, disabled and silent are four different things:

- **fired** — the check ran and found something
- **skipped** — the check could not run, and the reason is named (a missing export column, a
  missing page property). Not "zero issues".
- **disabled** — the operator turned the check off in config. Not missing evidence, and never
  read as "clean" — `checks_disabled_ids` names which ones.
- **silent** — the check *was invoked* and found nothing. This is the good one.
  `checks_silent_ids` names the population; a check nothing ever invokes is a defect, not this.

A health score computed from 16 of 248 checks is not a health score. The audit says so in
`health_score_basis`; **report that sentence next to the score, always.** Where coverage is too
low to score at all, the score is withheld rather than averaged out of what happened to be
available.

## Then look at the shape, not the list

```python
sorted(summary["by_check"].items(), key=lambda kv: -kv[1])[:10]
```

A check that fired on more than half the pages is almost always wrong — the tool exists to find
the *unusual*. On one live 124-page site, `URL_NOT_IN_SITEMAP` was 392 of 529 findings: 74% of
the report, and every one false. That share was visible in one line, before reading a single
URL.

→ [reference/populations](../reference/populations.md) for which set each check describes, and
[reference/defects](../reference/defects.md) for the ones that have been wrong before.

## Never mix representations

`summary.totals.pages_by_representation` says how the pages were measured — `static`,
`rendered`, `legacy_fragment`. A number averaged across two of them was never measured the same
way twice. If the crawl escalated some patterns and not others, say which population each figure
describes.

## And scan it

```bash
seohead log-scan --run ./run
```

Eight rules, each written from a defect that shipped. Exit 2 means two numbers in this run
disagree with each other; do not report either until you know which is right.
