# Scenario 26 — Image geometry and weight: what the browser has to guess

## The question

> The page jumps around while it loads and the client noticed. Which images are doing it?

Missing declared dimensions can prevent the browser from reserving space before an image
arrives. CSS can independently reserve that space: this notice records missing declarations,
not a measured layout shift. Image weight and rendered geometry need their own evidence.

## Covers

- **Images** — Missing Size Attributes · Over 100 kb · Incorrectly Sized Images

## The chain

**1. Crawl, and read what the crawl declares it cannot answer.**

```bash
seohead crawl-site --url https://example.com --out-dir ./run --max-urls 200
```

```bash
seohead log-scan --run ./run
```

Without retained resource declarations and responses, the image checks explain their missing
inputs in `run.checks_skipped`. Native scan.v2 resource capture supplies those declarations,
headers and captured byte sizes; omitted or unfetched references remain unavailable. Older
scans or export sets without the image filters can report:

```
{"id": "IMG_MISSING_DIMENSIONS", "reason": "missing export: images_missing_size"}
{"id": "IMG_OVER_KB",            "reason": "missing export: images_over_kb"}
```

`log-scan` is worth its twenty seconds here specifically: a recorded size that disagrees with
the file on disk is one of its eight rules, and it exists because a 739 KB WebP was once reported
as 1.27 MB. Every weight-based conclusion drawn from that run was wrong by a factor that differed
per file.

Native SQLite audits also read active retained HTML/DOM for per-element declarations.
`IMG_MISSING_DIMENSIONS` reports absent, zero or invalid width/height attributes on images
not explicitly hidden from layout. Visible decorative or ARIA-hidden images still need size
declarations. `IMG_MISSING_ALT_ATTRIBUTE` records literal attribute absence even when ARIA
supplies a name. Plain `alt=""` stays valid decoration; `IMG_MISSING_ALT` covers whitespace-only
alt and empty informative/unnamed-interactive images without an accessible name.

The analysis accepts at most 2 MiB per active document and 2,000 image elements. Missing or
bounded evidence stays partial. Known positive page-count facts survive unavailable bodies
as explicitly unlocalized page aggregates. Findings retain source, target, status scope,
reason, document/representation and ordinal locations. Unfetched image status stays
unavailable rather than borrowing HTTP 200 from its source. Source-bound image identities
preserve all affected pages; repeated elements keep exact counts and up to 200 location samples.
`scan-url-query --issue-check IMG_MISSING_DIMENSIONS` selects their retained source pages.

**2. Activate both checks from a Screaming Frog export.**

```bash
seohead sf run --exports-dir ./exports --out report --tasks
```

- In export mode, `IMG_MISSING_DIMENSIONS` comes from the `Images:Missing Size Attributes` filter. It is a
  **notice**, not a warning — the page still works, it just moves — and its fix hint says the
  actual reason: declare intrinsic width and height to reserve layout space and reduce CLS.
- `IMG_OVER_KB` comes from the `Images:Over X KB` filter. Note the X: the catalogue's issue is
  named "Over 100 kb", and this toolkit's default threshold is **150 kb**, set in the config's
  `thresholds.img_max_kb`. Screaming Frog's own export filter has its own limit too. Quote the
  threshold that ran, not the one in the issue's name.

**3. Cross-check the sizes against the wire.** A crawl records `size_bytes` per URL measured
**before** the body is decoded, which is the number a browser waits for.

```bash
seohead parse --input '{"url": "https://example.com/page", "options": {"url_sources": true}}'
```

Every image carrier on the page comes back with the tag and attribute it was found on, including
CSS backgrounds — and a CSS background has no width and height attributes to be missing at all,
so it will never appear in the dimensions list however much it shifts the layout.

**4. Build the task.**

```bash
seohead sf tasks --json ./report/audit.json --out ./report
```

**5. Fix the weight half properly.** This chain names the heavy files. It does not make them
lighter. The [images scenario](images.md) downloads them, re-encodes them, and ends with an archive and
the per-file saving — on one live construction site, 82 files, 69.7 MB down to 27.4 MB.

## What comes out

A notice-level list of pages with undimensioned images, and a warning-level list of files over
the threshold, both keyed by page so a developer fixes one template rather than one image:

```
| check                  | severity | threshold | fix                                  |
| IMG_MISSING_DIMENSIONS | notice   | —         | declare intrinsic width and height    |
| IMG_OVER_KB            | warning  | 150 kb    | compress, and consider WebP or AVIF   |
```

## What it costs

Nothing beyond the crawl and the export you already have, plus one request per page inspected
with `parse`. No paid API. The re-encoding half, if you run it, is CPU-bound and local.

## What it cannot answer

- **How much layout shift actually happens.** CLS is measured in a browser under a real
  connection. A missing attribute is a cause; it is not a score. `render-check` gives lab
  timings from one machine, which are not field data either.
- **Sizing without supported browser layout and physical response dimensions.** Requested renders
  retain `IMG_INTRINSIC_OVERSIZED` and `IMG_UPSCALED` for offline audit replay. File pixels are
  checked against device-pixel width; density-corrected `naturalWidth` does not substitute for
  physical dimensions. The existing oversized-image floor is 10 KiB. Unsupported object-fit,
  transformed/bordered/padded geometry, formats or missing response bodies stay unavailable.
- **Backgrounds and layout shift.** CSS backgrounds cannot carry dimensions, so they are absent
  from the finding by construction rather than by omission.
- **Whether the deploy keeps the saving.** A CMS that regenerates derivatives on upload can undo
  the whole thing.
- **Images written by JavaScript.** See the [rendering scenario](rendering.md).
