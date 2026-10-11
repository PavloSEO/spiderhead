# Scenario 46 — Delivery and weight: caching, images, document size and DOM complexity

## The question

> The pages are not that big and they still feel heavy. Where is the weight?

In four different places, measured four different ways. This chain separates how assets are
delivered from how much document there is to deliver, because they have different owners and
different fixes.

## Covers

- **PageSpeed** — Use Efficient Cache Lifetimes · Improve Image Delivery · Avoid Enormous Network Payloads · Optimize DOM Size

## The chain

**1. Cache lifetimes, for the document and for the assets.**

```bash
seohead headers-check --url https://example.com/page
seohead asset-weight-check --url https://example.com/page
```

The first reports the document's own headers. The second applies the same rule to every
stylesheet and script it fetched: `no-store` or `no-cache` on a static asset fails, a missing
`max-age` fails, a `max-age` under a week fails as short, and `immutable` or a week or more
passes. Each failure names the resource and the reason, so the task is a server-configuration
line rather than an adjective.

**2. Whether anything in front of the origin is caching at all.**

```bash
seohead cdn-check --url https://example.com
```

A CDN that passes every request through to the origin measures like no CDN. Knowing which of the
two you have decides whether cache headers are a five-minute change or a project.

**3. Image delivery — which is not the same question as image weight.**

```bash
seohead crawl-site --url https://example.com --out-dir ./run
```

The crawl records every URL it fetched with its content type and its size **on the wire**,
measured before the body is decoded. That detail is load-bearing: measuring after decoding turned
a 739 KB WebP into a recorded 1.27 MB, by a factor that differs per file, and every weight-based
conclusion drawn from it was unusable.

The two image findings with check ids — files over the size ceiling (150 KB by default) and
images served without width and height attributes — come from a Screaming Frog export set, not
from the native crawl, which declares those frames missing so the checks are skipped rather than
reported clean. Read image weight from `pages.jsonl`; read the two findings from `sf run`.

Re-encoding those files, measuring the saving and shipping an archive is a different chain and it
already exists: The [images scenario](images.md). Do not run both halves at once and report one number.

**4. Document weight and DOM complexity.** Both come out of the audit. `LARGE_HTML` fires on an
absolute ceiling (200 KB by default) or on a multiple of the site's own median, and `HTML_BLOAT`
on bytes per word against that same median — a document can be under any absolute limit and still
be mostly markup. DOM depth and node count have their own thresholds (32 levels, 1500 nodes) and
one prerequisite worth stating in advance:

```bash
seohead sf run --exports-dir docs/examples/exports --out report --tasks
```

The DOM checks read the selected document retained in a native scan or the SF stored HTML
directory (`input.html_store_dir`). A non-stored crawl has no retained body. Disabled retention,
size limits and incomplete or invalid bodies produce explicit reasons; partial findings carry
the measured coverage. They never turn unavailable HTML into a clean result.

**5. Verify, then hand it over.**

```bash
seohead log-scan --run ./run --images-dir ./images
seohead report-build --audit ./run/audit.json --format xlsx --out ./weight.xlsx
```

`log-scan` compares recorded sizes against files on disk only with `--images-dir`, the output
directory of `images-download`,
which is the check that would have caught the decoded-size defect above on the day it shipped.

## What comes out

Four separable lists, each with its own owner:

| Finding | Read it as | Who fixes it |
|---|---|---|
| `cache_findings` rows | server configuration | infrastructure |
| CDN present but not caching | delivery | infrastructure |
| images over the size ceiling, images without dimensions | assets | content or build |
| `LARGE_HTML`, `HTML_BLOAT`, `DOM_TOO_DEEP`, `DOM_TOO_MANY_NODES` | the template itself | front end |

Splitting them this way is most of the value. A single "page weight" number goes to nobody in
particular and is therefore fixed by nobody in particular.

## What it costs

One request per page and per asset checked, one crawl for the site-wide picture, and retained
HTML for the DOM checks (native scan storage or SF Store HTML). Offline reanalysis reuses those
bodies without fetching pages again. No paid API at any step.

## What it cannot answer

- **The total transfer size of a real page load.** Nothing here assembles a waterfall. Images,
  fonts, third-party tags and anything a script requests later are not summed into one figure,
  and a network dependency tree needs a real navigation.
- **Whether an image is the right size for its box.** Rendered dimensions against intrinsic
  dimensions, `srcset` selection and lazy-loading behaviour all need a rendered layout.
- **Whether a large DOM is actually slow.** Node count is a proxy. Forced reflow, layout shift
  culprits and main-thread work need instrumentation this toolkit does not have, and
  `docs/COVERAGE_SF_ISSUES.md` names each of them as out of scope rather than implying parity
  with Lighthouse.
- **Whether a short cache lifetime is a mistake.** A hashed filename wants a year; an unversioned
  one cannot have it. The check reports the header, not the deployment strategy behind it.
