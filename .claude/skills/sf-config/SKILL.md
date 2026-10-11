---
name: sf-config
description: >-
  Configure Screaming Frog ONCE so mode A can maximize applicable coverage from
  the 205-check registry for any site.
  Explains how to create audit.seospiderconfig (enable Spelling & Grammar,
  Structured Data, Security, Store HTML/Rendered HTML, JS Rendering, Crawl Linked
  XML Sitemaps, and Crawl Analysis), where to place it, and which checks it
  unlocks. Use when asked: "configure SF for a complete audit," "why is
  MIXED_CONTENT / STRUCTURED_DATA / SPELLING skipped?", "obtain every check from
  SF," or "create one SF config for all sites." Triggers: seospiderconfig,
  Screaming Frog config, audit config, SF structured data, SF security, SF spelling
  and grammar, complete SF audit.
---

# SF Config — Configure Screaming Frog Once for Every Site

SF calculates some checks **only when the corresponding modules are enabled**.
They are disabled by default -> the report honestly marks those checks as
`skipped` (instead of falsely reporting zero). To maximize the checks that can run
from the 205-check registry in a single `--crawl` run, create an
`audit.seospiderconfig` profile once; the tool will then load it automatically.
Site type, crawl scope, and available exports still determine which checks apply.

> The `.seospiderconfig` format is binary and can be created **only by SF itself**
> (it cannot be generated programmatically). Therefore, the step below must be
> completed manually once in the SF GUI (~1 minute).

## Trigger
- "Configure SF for a complete audit," "why is MIXED_CONTENT / STRUCTURED_DATA
  / SPELLING skipped?", "obtain every check from SF," or "create one SF config
  for all sites."
- An `sf-analyzer` or `seo-deep-audit` run came back with many `skipped`
  results and the fix is enabling SF modules, not re-crawling.
- First-time toolkit setup, before ever running `sf run --crawl`.
- keyword triggers: seospiderconfig, Screaming Frog config, audit config, SF
  structured data, SF security, SF spelling and grammar, complete SF audit.

## Anti-trigger
- An audit needs to run right now and SF already produces useful results
  without the extra modules — full coverage is nice-to-have, not blocking;
  once `audit.seospiderconfig` exists, Step 3 is automatic and this skill does
  not need revisiting before every crawl.
- No SF GUI is available at all (pure Mode B, exports-only workflow with no
  SF installation) — Step 1 requires SF's own GUI; if SF isn't installed,
  skip this skill and use Mode B exports directly via `sf-analyzer`.
- The question is about SF licensing or CLI discovery ("is SF installed,"
  "where's the CLI") — that is `seohead sf doctor` / `sf-analyzer`'s
  environment check, not this skill.

## Preconditions
- [ ] Screaming Frog desktop GUI is installed and licensed locally (the
  config can only be authored inside SF's own GUI, never generated
  programmatically).
- [ ] Write access to the repository root (to save `audit.seospiderconfig`
  next to `config.json`).
- [ ] If the goal is targeted rather than maximal coverage: a prior
  `audit.json` showing which checks are currently `skipped`.

## Step 1 — Enable the Modules in SF (Once)
Open Screaming Frog, go to the **Configuration** menu, and enable:

- **Spelling & Grammar** -> `Configuration → Content → Spelling & Grammar` ->
  Enable (+ select a language) -> unlocks `SPELLING_ERRORS`, `GRAMMAR_ERRORS`,
  `LONG_SENTENCES`, `READABILITY_DIFFICULT` (the columns appear in Internal:All).
- **Structured Data** -> `Configuration → Spider → Extraction → Structured Data`
  -> enable JSON-LD + Microdata + RDFa + Schema.org Validation + Google Rich
  Results -> unlocks `SCHEMA_VALIDATION_ERROR`, `STRUCTURED_DATA_MISSING`.
- **Security** -> the Security tab is populated during a normal crawl, but for
  resources, enable `Configuration → Spider → Crawl → Check Links Outside of Start
  Folder` and rendering (below) -> unlocks `MIXED_CONTENT`, `MISSING_HSTS`.
- **Store HTML + Rendered HTML** -> `Configuration → Spider → Extraction → Store
  HTML` and `Store Rendered HTML` -> unlocks `DOM_TOO_DEEP`,
  `DOM_TOO_MANY_NODES` (pass the directory through `--config` during the crawl +
  `input.html_store_dir`).
- **JavaScript Rendering** -> `Configuration → Spider → Rendering → JavaScript` ->
  for SPA/Next.js sites; otherwise, the raw HTML contains less content.
- **Crawl Linked XML Sitemaps** -> `Configuration → Spider → Crawl → XML Sitemaps`
  -> Crawl Linked XML Sitemaps -> populates the `Sitemaps:*` tabs (Orphan, Non-200
  URLs in the sitemap).
- **Crawl Analysis (automatic)** -> `Crawl Analysis → Configure → Auto Analyse At
  End of Crawl` -> without it, Near Duplicates, Orphan URLs, and Link Score are
  empty.

The exact checkbox checklist is in `reference/sf_config_checklist.md`.

## Step 2 — Save the Profile in the Repository
`File → Configuration → Save Configuration As…` -> save it as
**`audit.seospiderconfig`** in the repository root (next to `config.json`).

## Step 3 — Done; It Is Automatic from This Point On
The tool passes `--config audit.seospiderconfig` to SF automatically **if the file
exists** (see `sf_cli.seospiderconfig` in `config.json`). Use the same config for
every site:
```bash
seohead sf run --crawl https://example.com --out report --tasks
```
Check that SF and the config are available with `seohead sf doctor`. If the config
is absent, the tool simply runs SF with its defaults (without errors), while the
module-dependent checks remain `skipped`.

## Decision points
- **A check is `skipped`** — decide whether the module is genuinely disabled
  (fix via this skill) or the crawl simply had no matching content (e.g. no
  structured data anywhere on the site); check
  `reference/sf_config_checklist.md` before assuming it's a config gap.
- **Whether to enable JS Rendering** — it makes crawls much slower; turn it on
  only when the site actually needs it (SPA/Next.js per `seo-recon`'s
  `tech-detect`), not by default for every site.
- **Whether to enable Store HTML + Rendered HTML** — disk usage scales with
  the crawl; decide if `DOM_TOO_DEEP`/`DOM_TOO_MANY_NODES` are worth that cost
  for this site's size, or can be left off.
- **Config already exists but coverage is still incomplete** — decide whether
  the gap is a missing module (redo Step 1) or a check that's simply
  inapplicable to this site (no fix needed, not a bug).

## Definition of done
- [ ] `audit.seospiderconfig` exists at the repository root, next to
  `config.json`.
- [ ] `seohead sf doctor` confirms the config is discovered and will be
  passed automatically to `sf run --crawl`.
- [ ] A subsequent `sf run --crawl` shows previously `skipped` checks (that
  this site actually has content for) now populated, not still `skipped`.
- [ ] The one-time nature is communicated: this config is reused for every
  site, not recreated per audit.

## Cost
This skill itself costs nothing measurable in the toolkit: a one-time ~1
minute manual step inside the SF GUI (Step 1), a file save (Step 2), and zero
ongoing overhead after that (Step 3 is automatic on every crawl). The only
recurring cost it introduces is to `sf-analyzer`'s Mode A crawls: enabling JS
Rendering and Store HTML/Rendered HTML make each crawl slower and use more
disk, proportional to site size — but that is SF's own local licensed crawl
time, not a paid API call.

## Table of Which Checks Each Module Unlocks
See `reference/sf_config_checklist.md` (module -> checks -> how to verify that it
works).
