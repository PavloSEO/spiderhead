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

With native scan.v2 resource capture, retained image responses and declarations support image
weight and missing-dimension findings. Missing or budget-limited captures retain their native
unavailable reasons. SF exports remain another supported input for these filters.

`HEAVY_PAGE_WEIGHT` uses actual pinned-browser response bodies on requested renders, including
image, font and third-party buckets. Its unit is content-decoded bytes within that navigation
window; captured HTML alone is an explicit lower bound on unrendered pages. It does not sum
unused `srcset`, media or `nomodule` alternatives. `RESOURCE_OVER_2MB` uses a configurable
individual captured-resource threshold, and HTTP 206 fragments preserve lower-bound scope.

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

The DOM checks read stored HTML. Without `input.html_store_dir` — a Screaming Frog crawl run with
Store HTML enabled — they are reported as skipped, with that reason attached, rather than as
clean. A skipped check that looks like a passed check is how an audit lies without saying
anything false.

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

One request per page and per asset checked, one crawl for the site-wide picture, and — only if
you want the DOM checks — a Screaming Frog crawl with HTML storage, which is disk rather than
money. No paid API at any step.

## What it cannot answer

- **Compressed transfer totals or requests outside the captured navigation.** Browser weight
  measures delivered content-decoded bodies, with pending/omitted responses marked partial.
- **Image sizing without supported layout and delivered physical dimensions.** Requested renders
  support `IMG_INTRINSIC_OVERSIZED` and `IMG_UPSCALED`; unsupported geometry or image formats,
  missing bodies and invisible/unloaded elements remain unavailable. CSS background sizing is
  a separate owner decision.
- **Whether a large DOM is actually slow.** Node count is a proxy. Supplied matching Lighthouse
  JSON supports the named reflow, layout-shift and delivery findings; no general CPU profile or
  field-performance verdict follows from this chain.
- **Whether a short cache lifetime is a mistake.** A hashed filename wants a year; an unversioned
  one cannot have it. The check reports the header, not the deployment strategy behind it.
