# Scenario 61 — AMP pairing: the desktop page and its AMP twin agree, or they do not

## The question

> Our articles have AMP versions. Do the desktop pages and their AMP twins point at each other
> correctly, and does the AMP version deserve to be indexed at all?

A desktop page that declares `<link rel="amphtml">` makes a claim about another URL. The AMP page
should answer, return to the page that declared it, and hand its canonical back. This chain
reads that pairing from one crawl and stops at the judgement a person has to make.

## Covers

- **AMP** — Indexable · Missing Canonical · Missing Canonical to Non-AMP · Missing Non-AMP Return Link · Non-200 Response · Non-Indexable Canonical
- **AMP** — Missing <html amp> Tag · Missing/Invalid Doctype HTML Tag · Missing Head Tag · Missing Body Tag · Missing/Invalid Meta Charset Tag · Missing/Invalid Meta Viewport Tag · Missing/Invalid AMP Script · Missing/Invalid AMP Boilerplate

## The chain

**1. Crawl the desktop pages and their AMP targets in the same run.**

```bash
seohead crawl-site --url https://example.com --scan-out ./run.sqlite
```

The pairing is read from the AMP URL that a desktop page declares, so the AMP page has to be
captured too. If the crawl never reached it, the AMP checks are reported as skipped with the
reason, not as a clean result.

**2. Read `AMP_NON_200` first.**

A declared AMP URL that answers anything but 200 breaks the pair before any other judgement
applies. The fix is either to serve the AMP URL or to remove the `rel="amphtml"` that names it.

**3. Read the return link and the canonical on the AMP page.**

`AMP_MISSING_CANONICAL` means the AMP page declares no `rel="canonical"` at all.
`AMP_MISSING_RETURN_LINK` means it declares one that does not point back at the non-AMP URL
that claimed it. Both are warnings in the audit, and both are the same typo class as the
canonical basics scenario: the element is there and points at the wrong place.

**4. Read `AMP_NON_INDEXABLE_CANONICAL`.**

When the AMP page's canonical resolves to a URL the crawl found non-indexable, the pair points
at a page search engines are told to ignore. Fix the target or the canonical; do not rely on the
AMP page alone.

**5. Read `AMP_INDEXABLE` last, as a notice rather than a defect.**

It means the AMP page is itself indexable. That is only a problem when the non-AMP page is the
intended canonical. Decide that with the owner of the site before changing anything.

## What comes out

The retained scan also reports original-source AMP structure: the root marker,
doctype, explicit head/body, UTF-8 charset, viewport, async runtime script and
boilerplate declarations. Boilerplate CSS and full validator rules remain unavailable;
a structural check passing is not proof of full AMP validity.

`audit.json` carries one issue per AMP check, with the desktop URL and the AMP URL in the
details, under the `AMP` category. A check that could not run because its target was not
captured is listed with its reason, so an empty AMP section never reads as a passed one.

## What it costs

- Pairing adds no request of its own: it reads the AMP targets the crawl already captured.
- No paid API at any step.
- Minutes on a small site; the crawl pace is set by `speed.min_delay_seconds`.

## What it cannot answer

- **Whether Google treats the pair as valid AMP.** The checks read the markup the crawl
  received. Validation against the AMP specification is a separate step.
- **Whether the AMP template renders the same content.** Pairing compares declarations and
  status, not the body text of the two pages.
- **Which version a search engine chooses to show.** Nothing in this loop reads the index.
- **A declaration injected by JavaScript.** The crawl reads the served HTML; run the
  [rendering scenario](rendering.md) first if the `rel="amphtml"` link is added after load.
