# Coverage gap reconciliation

Generated from `docs/COVERAGE_GAPS.md` by `scripts/generate_coverage_gap_reconciliation.py`.
A row is covered only when its actual Mode cell names a shipped check/tool; partial and out-of-scope are not readiness claims.

**100 rows:** covered_registry_or_tool=56, missing=28, out_of_scope=8, partial=8.

| Row | Name | Reconciled state |
|---|---|---|
| 1.1 | Real Core Web Vitals (LCP/INP/CLS) | partial |
| 1.2 | TTFB separate from `response_time` | partial |
| 1.3 | FCP / render speed | partial |
| 1.4 | Response compression (Brotli/gzip) | partial |
| 1.5 | Cache-Control / cacheability | covered_registry_or_tool |
| 1.6 | Render-blocking resources | covered_registry_or_tool |
| 1.7 | Page weight (total) | partial |
| 2.1 | Authorship (byline) | covered_registry_or_tool |
| 2.2 | Publish/update dates | covered_registry_or_tool |
| 2.3 | About / Contact pages | covered_registry_or_tool |
| 2.4 | Privacy Policy / Terms | covered_registry_or_tool |
| 2.5 | Outbound citations | covered_registry_or_tool |
| 2.6 | YMYL detection | partial |
| 2.7 | Trust signals / disclaimers | missing |
| 3.1 | ARIA roles and labels | out_of_scope |
| 3.2 | Color contrast | out_of_scope |
| 3.3 | Focus-visible / keyboard | out_of_scope |
| 3.4 | Form labels | out_of_scope |
| 3.5 | Heading order (a11y) | out_of_scope |
| 3.6 | Skip link | out_of_scope |
| 3.7 | Link text (descriptive) | covered_registry_or_tool |
| 3.8 | Touch-target size | out_of_scope |
| 3.9 | Table headers | out_of_scope |
| 4.1 | raw/render diff (title, desc, h1, canonical, noindex) | partial |
| 4.2 | content/links diff | covered_registry_or_tool |
| 4.3 | SSR vs CSR detect | covered_registry_or_tool |
| 4.4 | Render-blocking resources for bots | missing |
| 5.1 | JS redirect | covered_registry_or_tool |
| 5.2 | URL case normalization | missing |
| 5.3 | Soft 404 | covered_registry_or_tool |
| 6.1 | Canonical chain | covered_registry_or_tool |
| 6.2 | Canonical -> redirect | covered_registry_or_tool |
| 6.3 | Canonical -> 4xx/5xx | covered_registry_or_tool |
| 6.4 | Canonical -> homepage (stamp) | covered_registry_or_tool |
| 6.5 | Canonical header vs tag | missing |
| 6.6 | Canonical contains a fragment | covered_registry_or_tool |
| 6.7 | Canonical outside `<head>` | covered_registry_or_tool |
| 6.8 | Invalid attribute in canonical annotation | missing |
| 7.1 | No x-default | covered_registry_or_tool |
| 7.2 | No self-reference | covered_registry_or_tool |
| 7.3 | lang ≠ page language | missing |
| 7.4 | Relative URL in hreflang | missing |
| 7.5 | No return link | covered_registry_or_tool |
| 7.6 | hreflang -> non-canonical | covered_registry_or_tool |
| 7.7 | hreflang -> noindex | covered_registry_or_tool |
| 7.8 | hreflang -> redirect/4xx | covered_registry_or_tool |
| 7.9 | Duplicate lang per target | missing |
| 7.10 | Duplicate lang per source | covered_registry_or_tool |
| 7.11 | Malformed language/region code | covered_registry_or_tool |
| 7.12 | Outside `<head>` | covered_registry_or_tool |
| 8.1 | follow/nofollow conflict per target | covered_registry_or_tool |
| 8.2 | Nofollow onto an indexable page | covered_registry_or_tool |
| 8.3 | External without nofollow | missing |
| 8.4 | HTTP links on an HTTPS page | covered_registry_or_tool |
| 8.5 | Localhost/127.0.0.1 in links | covered_registry_or_tool |
| 8.6 | Generic anchor text | covered_registry_or_tool |
| 8.7 | Anchor without title (duplicating) | missing |
| 9.1 | "no alt" vs "empty alt" | covered_registry_or_tool |
| 9.2 | Long alt (>100 chars) | covered_registry_or_tool |
| 9.3 | `<picture>` without `<img>` | missing |
| 9.4 | Modern format (WebP/AVIF) | missing |
| 9.5 | Responsiveness (srcset) | missing |
| 9.6 | Lazy-loading | missing |
| 9.7 | Filename quality | missing |
| 10.1 | Tracking parameters | covered_registry_or_tool |
| 10.2 | Session ID in URL | covered_registry_or_tool |
| 10.3 | Stop words in slug | missing |
| 10.4 | Trailing-slash desync | covered_registry_or_tool |
| 10.5 | WWW canonicalization | covered_registry_or_tool |
| 11.1 | Duplicate `id`s | covered_registry_or_tool |
| 11.2 | No `<!DOCTYPE>` | covered_registry_or_tool |
| 11.3 | No charset | covered_registry_or_tool |
| 11.4 | Multiple `<head>`/structural dupes | covered_registry_or_tool |
| 11.5 | Block elements in `<head>` | covered_registry_or_tool |
| 11.6 | Lorem ipsum / placeholder | covered_registry_or_tool |
| 11.7 | MIME vs extension | covered_registry_or_tool |
| 12.1 | Invalid rel=next/prev | missing |
| 12.2 | Canonical chain on pagination | missing |
| 12.3 | Pagination loop | missing |
| 12.4 | Sequence gap | covered_registry_or_tool |
| 12.5 | Pagination orphan | missing |
| 13.1 | Required fields per type | partial |
| 13.2 | Type-specific scenarios | missing |
| 13.3 | Schema drift | missing |
| 14.1 | No CSP | covered_registry_or_tool |
| 14.2 | No X-Content-Type-Options | covered_registry_or_tool |
| 14.3 | No Referrer-Policy | covered_registry_or_tool |
| 14.4 | No Permissions-Policy | covered_registry_or_tool |
| 14.5 | SSL certificate lifetime | covered_registry_or_tool |
| 14.6 | Secrets leaked in HTML | missing |
| 14.7 | Forms on HTTP | covered_registry_or_tool |
| 15.1 | OG:title/description/url/image present | covered_registry_or_tool |
| 15.2 | OG:image pixel size | missing |
| 15.3 | OG:url vs canonical | missing |
| 15.4 | Twitter Card | covered_registry_or_tool |
| 16.1 | Cookie-consent / CMP | missing |
| 17.1 | AI bots: training vs retrieval | covered_registry_or_tool |
| 17.2 | Semantic structure / citability | covered_registry_or_tool |
| 18.1 | Keyword stuffing | missing |
| 18.2 | Title = brand only | missing |

## Remaining priority order

Rows below are generated from unresolved map rows: stated value first, then the declared B/B+/A feasibility, then row number. The order is a planning aid, not a product-readiness claim.

1. Secrets leaked in HTML (14.6) — missing; A (HTML + regex)
2. Required fields per type (13.1) — partial; **mostly DONE** in the live `schema-check` (vocabulary + Google rich-result eligibility per type); audit ids still absent
3. Real Core Web Vitals (LCP/INP/CLS) (1.1) — partial; **partially DONE**: opt-in CrUX current record via `crux-report` and supplied `site-audit` evidence; no automatic crawl-registry check
4. YMYL detection (2.6) — partial; **Partial** — `YMYL_REVIEW_CANDIDATE`, never a classification
5. raw/render diff (title, desc, h1, canonical, noindex) (4.1) — partial; **PARTIAL** — `render-check` compares title, h1 and canonical; description and meta-robots/noindex differences remain unmeasured
6. Render-blocking resources for bots (4.4) — missing; B (`ROBOTS_BLOCKS_RESOURCES` exists; extend)
7. Duplicate lang per target (7.9) — missing; B (graph) — still open (distinct from 7.10: this is one *target* with conflicting incoming langs, not one *source* repeating a lang)
8. Invalid rel=next/prev (12.1) — missing; B (Internal:All: rel_next/rel_prev)
9. Canonical chain on pagination (12.2) — missing; B (canonical x rel_next graph)
10. Pagination loop (12.3) — missing; B (graph)
