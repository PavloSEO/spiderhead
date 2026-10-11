---
name: backlinks-check
description: >-
  Check backlinks against a provided list of donor pages: whether the target domain
  is linked, what anchor text and rel values the link uses, whether it passes link
  equity (nofollow/ugc/sponsored prevent it), and whether the donor page itself is
  indexable. It answers not only whether a link exists, but WHY it is missing or why
  it does not work. Use when asked to "check my backlinks," "are the links still
  live," "dofollow or nofollow," "is there a link to the site," "check the donor
  pages," or "was the link removed." It verifies a KNOWN donor list. For explicitly supplied index/CSV evidence use
  backlinks-profile; this check does not discover donors on its own. Triggers:
  backlinks, links from donor pages, dofollow, nofollow, check links, link removed,
  is the link live, donor page.
---

# Backlinks Check — are links from donor pages live and do they pass link equity?

The presence of an `<a href>` is not enough. A link does not pass link equity if it
uses `rel=nofollow` (or `ugc`/`sponsored`), if the donor page is blocked from indexing
with `meta robots noindex`, if it applies `nofollow` to all links at once, or if it
is canonicalized to another URL. The tool checks all of these conditions and groups
the results by cause.

Important scope boundary: this checks **your list** of donor pages (purchased links,
crowd-marketing links, guest posts, directories); it does not investigate someone
else's backlink profile. The separate `backlinks-profile` route merges explicit index, first-party and CSV evidence;
no provider proves a complete list of every link to a site.

The tool is `seohead backlinks-check` (CLI + local MCP `seo_backlinks_check`).

## Trigger
- "check my backlinks," "are the links still live," "was the link removed";
- "dofollow or nofollow," "does it pass link equity," "is there a link to the site";
- "does page X link to our site," "check the donor pages";
- regular monitoring of purchased backlinks.

## Anti-trigger
- The ask is to discover an unknown backlink profile ("who links to us," "find new backlinks")
  rather than verify a known list — that requires Ahrefs/Majestic/GSC data this toolkit does not
  provide; this skill only checks URLs already in hand.
- The ask is about the user's OWN outbound links breaking (broken links, redirect chains on the
  user's own site) — that is `sf-analyzer`/`links-check`, not this skill.
- No donor URL list exists yet and none can be supplied — there is nothing to check; do not
  substitute a guess or a crawl of the target domain for a real donor list.
- The question is about a donor site's overall authority or trustworthiness rather than one
  specific link's state — that is `domain-profile`/`seo-recon` on the donor, not `backlinks-check`.

## Preconditions
- [ ] A concrete list of donor URLs exists (inline `--donors` or a `--donors-file`), at or under
  the 500-page limit.
- [ ] The target is defined precisely as either a domain (subdomain matches count) or an exact URL
  (exact match only) — know which is intended before running.
- [ ] Donor pages are expected to be reachable; if the list is stale, expect a mix of `missing` and
  errored results rather than treating every miss as a confirmed lost link.

## Workflow
**Target: a domain** (any link to the domain or one of its subdomains will match):
```bash
seohead backlinks-check --target example.com --donors "https://donor1.example/post,https://donor2.example/page"
```
**Target: an exact URL** (the address must match exactly):
```bash
seohead backlinks-check --target https://example.com/landing --donors-file donors.txt
```
`--donors-file` is a file containing one URL per line, with `#` marking comments.
`--concurrency N` is the number of parallel requests (default: 3; maximum: 10).
The limit is 500 donor pages per run.

## Response contents
- `summary`: `found` (link exists), `missing` (link is absent), `dofollow`, `nofollow`,
  `on_noindex_page` (the donor page is blocked from indexing, so no equity is passed).
- `results[]` for each donor page: `found`, `status_code`, `donor_indexable`,
  `canonical`/`canonical_elsewhere`, `links[]` (for every link found: `href`, `anchor`,
  `rel`, independent `normal`/`nofollow`/`sponsored`/`ugc` flags, `follow`, `blocked_by`), and `reason`, which explains why the link is absent
  or why it does not work.

## Decision points
- **`missing` link vs `on_noindex_page`.** A donor page returning the link is not enough — check
  `donor_indexable`/`canonical` before concluding equity passes; a noindexed or
  canonicalized-elsewhere donor page passes no equity even when `found: true`.
- **Link-level `nofollow` vs page-wide `nofollow`.** The `rel` on the specific `<a>` may look clean
  while the whole donor page blocks equity another way — check `blocked_by` and the page-level
  flags, not only the individual link's `rel`.
- **Target defined as domain vs exact URL.** If the link appears "missing" but the donor links to a
  different URL on the same domain (a redirected or alternate page), decide with the user whether
  that still satisfies the requirement — do not let it silently lower the "found" count.
- **Donor page unreachable (4xx/5xx/timeout).** Distinguish "link removed" from "donor page itself
  died" using the `reason` field — the former is a lost backlink, the latter may just need
  revisiting later; do not merge both into one "missing" bucket.

## Definition of done
- [ ] Every donor URL in the input list has a result row (`found`/`missing`/error) — none silently
  dropped.
- [ ] Each `found` link is classified dofollow/nofollow and cross-checked against donor-page
  indexability and canonical.
- [ ] The summary counts (found/missing/dofollow/nofollow/on_noindex_page) reconcile against the
  per-donor table.
- [ ] Dead and nofollow/no-equity links are listed separately under "requires attention."

## Cost
One HTTP request per donor page, up to the 500-page limit, run with `--concurrency` (default 3,
max 10) — a typical run of a few dozen donors is a few dozen requests taking well under a minute.
No paid API is touched: `seohead backlinks-check` is a local crawler against the donor pages
themselves, not a third-party backlink-index API.

## What to deliver to the user
Provide a table by donor page: donor page → link present → anchor text →
dofollow/nofollow → donor page indexable → conclusion. At the top, provide a summary:
how many links are live, how many pass equity, how many were lost, and for what reason.
Put dead and nofollow links in a separate "requires attention" list.

Combination: **seo-recon** identifies the donor sites (stack, age, hosting), while
**sf-analyzer** checks the user's own site for broken outbound links.
