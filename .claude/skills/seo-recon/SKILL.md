---
name: seo-recon
description: >-
  Investigate the domain and site infrastructure using data a page crawl does not
  provide: domain age and registration, DNS records, hosting and ASN by IP, server
  location, TLS certificate, CDN and cache operation detection, CMS/framework/stack,
  analytics, and pixels. Use when asked "what hosting provider does this site use,"
  "where is the server," "what CMS / site engine does it use," "what is the site
  built with," "does it use Cloudflare," "does the cache work," "how is traffic
  measured," whois, "domain age," hosting/CDN detection, or stack detection.
  Triggers: whois, DNS, ASN, Cloudflare, Fastly, Vercel, WordPress, Next.js, Tilda,
  Bitrix, Shopify, cache, cache-control, HTTP/2, HTTP/3, brotli, site engine, domain
  age, and hosting provider.
---

# SEO Recon — Domain, Hosting, Stack, and Cache Reconnaissance

External domain and infrastructure reconnaissance covers what an **sf-analyzer**
crawl does not provide: the registrar and domain age, where the server is hosted and
on which ASN, which CDN sits in front of the site and **whether its cache actually
works**, which CMS/framework the site is built with, and how traffic is measured.

These are no longer manual Bash steps, but three tools in the `seohead` toolkit
(CLI + MCP + HTTP). Each returns ready-to-use JSON with a `findings` block containing
plain-language conclusions. In MCP, they are `seo_domain_profile`, `seo_cdn_check`,
and `seo_tech_detect`.

## Trigger
- "what hosting provider does the site use," "where is the server," "which ASN / provider hosts it";
- "does it use Cloudflare," "which CDN," "does caching work," "why is it slow";
- "which CMS," "which site engine," "what is the site built with," "how is traffic measured," stack detection;
- "domain whois," "domain age," "when was it registered," "who is the registrar," TLS/expiration;
- a quick competitor profile before a crawl audit;
- stack keyword triggers: Cloudflare, Fastly, Vercel, WordPress, Next.js, Tilda,
  Bitrix, Shopify, cache-control, HTTP/2, HTTP/3, brotli.

## Anti-trigger
- The need is on-page/crawl data — status codes, redirect chains, Title/meta
  robots/canonicals, broken links, sitemap discrepancies. SF sees the whole
  site at once; use `sf-analyzer`, not this skill, for anything that scales
  per-page.
- The need is security posture — HSTS/CSP headers, cookie flags, exposed
  `.git`/`.env` paths. These three tools do not evaluate security; use
  `security-audit` (`seohead security-check`) instead.
- The need is "does this page need JS to render" — `tech-detect` only names
  the framework, it does not diff raw vs rendered output; use `js-render-check`
  for that comparison.
- The need is Core Web Vitals / Lighthouse scores — `cdn-check` measures TTFB
  and cache behavior, not CWV; that is the PSI API's job (see `sf-boundaries`).

## Preconditions
- [ ] A domain (for `domain-profile`) or a full URL with scheme (for
  `cdn-check`/`tech-detect`) is available.
- [ ] Network access exists to make live HTTP/DNS requests to the target — all
  three tools perform real requests, not a lookup against cached data.
- [ ] For ccTLDs without RDAP (`.by`, some `.ru`) the system `whois` binary is
  present as a fallback, or the report will honestly show `source: none`
  rather than fabricated registration data.

## Workflow (All Three Tools by Default)

**1. Domain profile.** Registration, DNS, hosting, ASN, location, and TLS in one call:
```bash
seohead domain-profile --domain example.com
```
The response contains: `registration` (registrar, `age_years`, `expires_in_days`, statuses),
`dns` (A/AAAA/NS/MX/TXT, `dns_provider`, `mail_provider`, `spf`, `dmarc`),
`hosting` (`ip`, `asn`, `as_name`, `network`, `country`, `reverse_dns`),
`tls` (issuer, `expires`, `days_left`), and `flags` — risks summarized in one line.
Registration data comes from RDAP; for ccTLDs without RDAP (`.by` and some `.ru`
domains), the tool automatically falls back to the system `whois` command. If it is
unavailable, the tool honestly marks `source: none` instead of inventing data.

**2. CDN and cache.** Not whether caching is configured on paper, but whether it works:
```bash
seohead cdn-check --url https://example.com
```
The tool makes three requests: an initial request, a repeat request (to catch
`MISS → HIT`), and a conditional request (to check for `304`). The response contains:
`cdn`, `transport` (HTTP version — the `http_version_measurable` field honestly states
when it cannot be measured without the `h2` package; HTTP/3 in Alt-Svc, brotli/gzip,
and the TTFB of the first and second requests), and `cache` (parsed `cache_control`,
`etag`/`last_modified`, `hit_first`/`hit_second`, `warmed_up`, and `revalidation`).

**3. Technologies.** CMS, framework, server, analytics, pixels, widgets, and third-party CDNs:
```bash
seohead tech-detect --url https://example.com
```
The response contains: `technologies` (each with `category`, `evidence` — the marker
that identified it — and `version` when exposed in `generator`/`x-powered-by`),
`by_category`, `scripts_total`, and `third_party_hosts`. Categories: cms, ecommerce,
framework, library, server, runtime, analytics, pixel, widget, consent, fonts,
cdn-lib, protection.

## Emergency Fallback (If the Toolkit Is Unavailable)
Use this only when `seohead` is not installed and cannot be installed. Run manually:
`whois "$DOM"` (registrar/dates/NS), `dig +short A/AAAA/NS/MX/TXT "$DOM"`,
`whois "$IP"` (OrgName/netname/ASN), `curl -sIL "https://$DOM"` (CDN headers:
`cf-ray`/`server`/`x-vercel-id`/`x-amz-cf-id`), `<meta name=generator>` and paths
(`/wp-content/`, `/_next/`, `tildacdn.com`, `/bitrix/`) to identify the site engine,
and `openssl s_client -connect "$DOM:443"` for the certificate. The logic is the
same, but performed manually and without structured output.

## Decision points
- **RDAP is unavailable for a ccTLD** — the tool falls back to system `whois`;
  if that is also unavailable, report `source: none` rather than guessing
  registration data.
- **`http_version_measurable` is false in `cdn-check`** (the `h2` package is
  missing) — report the HTTP version as "not measurable," never silently
  assume HTTP/1.1.
- **`tech-detect` finds an SPA/Next.js/Nuxt stack** — flag JS rendering as
  required for downstream work (`sf-analyzer`'s crawl, `js-render-check`);
  this changes how later audit phases must be run.
- **The `seohead` toolkit itself is unavailable** — fall back to the manual
  commands above, but tell the user the result is unstructured (no
  `findings` block, no automatic risk flags) rather than presenting it as
  equivalent output.

## Definition of done
- [ ] All three tools ran — `domain-profile`, `cdn-check`, `tech-detect` — or
  a stated reason exists for skipping one.
- [ ] Each tool's `findings` block (not just raw JSON) is reflected in the
  summary handed to the user.
- [ ] Risk flags (young domain, expiring cert/registration, missing
  SPF/DMARC, offshore hosting, geography mismatched to the target market)
  are explicitly called out or explicitly ruled absent.
- [ ] The user is pointed at the correct next skill for anything out of
  scope here — `sf-analyzer` for crawl data, `security-audit` for security,
  `seo-deep-audit` for a full automated audit — rather than this skill
  trying to cover those itself.

## Cost
Three toolkit calls, each a small, fixed number of requests: `domain-profile`
does RDAP/whois plus DNS lookups; `cdn-check` makes exactly three HTTP
requests to the target URL (initial, repeat, conditional); `tech-detect` makes
one request and parses it. Total well under 15 requests, seconds end-to-end,
no paid API involved. The manual Emergency Fallback commands cost the same
handful of requests run by hand, just without the structured `findings`
output.

## What to Deliver to the User
A concise profile in a single block, assembled from the three tools' `findings`:
- **Domain:** registrar, age, time until expiration, DNS and mail providers, SPF/DMARC.
- **Server:** IP, ASN and `as_name`, the actual provider behind the CDN, country, reverse DNS.
- **CDN/cache:** which CDN, whether the repeat request hits the cache, HTTP/2–3, compression, TTFB.
- **Stack:** CMS/framework + backend with version, analytics and pixels, number of third-party scripts.
- **Risk flags:** young domain, expiring registration/TLS, hold status, missing SPF/DMARC,
  offshore hosting, or a server location that does not match the target market.

Next, handle crawling and on-page analysis through **sf-analyzer** (`audit.json`),
security through **security-audit**, readable analysis through **sf-report**, and the
backlog through **sf-tasks**. For a complete automated domain audit, use the
**seo-deep-audit** orchestrator.
