# Tool reference

Generated from the MCP tool definitions in `seohead/mcp/mcp_server.py`, `seohead/mcp/sf_mcp.py` and `seohead/mcp/semantics_mcp.py` — do not edit by hand. Regenerate with:

```bash
python scripts/generate_tool_reference.py
```

**175 core tools** (`seohead <command>` / `seo_<command>` on the MCP server) plus **5 crawl-audit tools** (`sf_<command>`, driven by `seohead sf ...`) plus **4 semantic-core tools** (`seo_semantics_*`, driven by `seohead semantics <stage>`) — 184 in total.

Every tool shares one contract: JSON in, JSON out. A target that could not be reached comes back as `{"ok": false, "error": "..."}` instead of raising, so an unreachable site is data, not a crash.

- **Cost** — network/file/spend flags read from the tool's `ToolAnnotations` profile: whether it reaches beyond the process, whether it creates or changes files, whether repeating the call is safe, and whether it spends an external provider's quota.
- **Behavior and failure modes** — the remainder of the tool's own docstring: what it deliberately skips, what a degraded answer looks like, and what to use instead when it is the wrong tool for the job.

---

## Live URL, recon, and data-source tools

### `parse`

MCP name: `seo_parse`

Parse SEO data (title, meta description, canonical, OG/Twitter, H1-H6, JSON-LD, links, visible text, word count) from one URL or a list of URLs.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `''` |
| `urls` | `list[str] | None` | `None` |
| `options` | `dict[str, Any] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `redirects-generate`

MCP name: `seo_redirects_generate`

Generate redirect rules (Apache mod_rewrite/Redirect, Nginx, or a custom template) from a list of {old_url, new_url} pairs.

| Argument | Type | Default |
|---|---|---|
| `redirects` | `list[dict]` | `required` |
| `fmt` | `str` | `'apache-rewrite-rule'` |
| `default_url` | `str` | `'/'` |
| `custom_template` | `str` | `''` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `redirects-check`

MCP name: `seo_redirects_check`

Follow a live redirect chain for a URL and report each hop (status, location).

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `options` | `dict[str, Any] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `scan-reanalyze`

MCP name: `seo_scan_reanalyze`

Reparse retained HTML/DOM and rerun existing checks without network.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `out` | `str` | `required` |
| `producer_build` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Reads a native or derived SQLite scan and creates a new artifact with
parent UUID and current analyzer provenance. The source is unchanged;
an existing output is refused. Missing required bodies produce named
unavailable errors. Missing live-only context remains skipped. No HTTP,
DNS, browser, provider, or cache-miss fallback is permitted.

### `crawl-site`

MCP name: `seo_crawl_site`

Crawl a site from a start URL by following links, or fetch an explicit ``urls`` list instead of following links at all, then audit the result through the same checks used for Screaming Frog exports. One of ``url`` or ``urls``/``urls_file`` is required. Same host only when following links, politeness adapts to the origin. Checks whose evidence a native crawl cannot produce are reported as skipped, never as clean.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `''` |
| `urls` | `list[str] | None` | `None` |
| `urls_file` | `str | None` | `None` |
| `sitemap` | `str | None` | `None` |
| `sitemap_only` | `bool` | `False` |
| `config` | `str | None` | `None` |
| `max_urls` | `int | None` | `None` |
| `max_depth` | `int | None` | `None` |
| `min_delay` | `float | None` | `None` |
| `robots` | `str | None` | `None` |
| `concurrency` | `int | None` | `None` |
| `out_dir` | `str | None` | `None` |
| `scan_out` | `str | None` | `None` |
| `producer_build` | `str | None` | `None` |
| `overrides` | `dict[str, Any] | None` | `None` |
| `resume` | `str | None` | `None` |
| `project` | `str | None` | `None` |
| `approve_large_crawl` | `bool` | `False` |
| `user_agent` | `str | None` | `None` |
| `observer_run_id` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Pass ``urls`` or a local ``urls_file`` instead of ``url`` for list mode: fetch exactly that set,
depth 0, no link discovery -- the migration-audit shape (a redirect map,
a Search Console export). ``urls_file`` scans TXT, CSV, XLSX, or XML for
absolute HTTP(S) URLs in source order. ``max_depth`` and ``concurrency``
have nothing to discover in that mode and are ignored.

``robots`` is "respect" (obey), "report_only" (fetch robots.txt, crawl
anyway, and report what a compliant crawler would have missed) or
"ignore" (do not fetch it at all) -- applied in list mode too, and named
in the result's ``discovery.directive_policy``, not only enforced
silently. ``concurrency`` is a per-origin ceiling the adaptive throttle
grows into, not a fixed thread count. ``sitemap`` seeds the crawl from a
sitemap's declared URLs in addition to following links from ``url``, and
reconciles the two sources (declared vs. observed). ``config`` is a path
to a crawler config file (JSON) on this machine, the same file
``crawl-site --config`` reads. ``max_urls``, ``max_depth``, ``min_delay``,
``robots`` and ``concurrency`` are left unset by default: an omitted
override preserves whatever ``config`` (or its own defaults) already
says, exactly like the CLI's flags -- pass one explicitly only to
change that one setting. ``seo_crawl_describe_settings`` lists the
defaults each of them falls back to. Explicit ``max_urls`` admits up to
1,000,000 URLs; larger values fail before I/O. Request, render, time and
disk budgets remain independent: use ``limits.max_requests`` in overrides
for explicit overhead headroom, at most 2,000,000 HTTP attempts.

``http.proxy`` in ``config`` or ``overrides`` selects an explicit HTTP
forward proxy for the entire native crawl, including sitemap, resource
and pinned browser requests. Credentials require an ``env:VARIABLE``
URL reference; ``http.proxy_allow_private`` authorizes only a private
proxy endpoint, not private targets. Ambient proxy variables are ignored.
Proxied runs require cache off and a fresh artifact; failures never
fall back to direct egress.

A URL crawl with neither ``scan_out`` nor ``out_dir`` writes a collision-safe
SQLite scan below the caller's ``scans/`` directory. ``scan_out`` overrides
that destination; ``out_dir`` selects the explicit legacy directory route.
Explicit URL lists can select ``scan_out`` for disk-backed, resumable
capture in exact input order without link discovery, including multiple hosts.
Large legacy-directory lists use the same native spool, while directory
audit materialization remains separately bounded. List capture is static-only;
rendering and resource fetching are refused. ``cache.mode`` live/replay
requires the bounded eager ``out_dir`` route. Native scans retain bounded HTML entities and separate
DOM; their policy records disabled, sensitive, no-store and budget omissions.
In the config file, ``resources.fetch=true`` opts into direct same-origin
script/stylesheet capture (20,000 HTTP attempts and 5 MiB per response by
default). Resource requests share total crawl-time and storage budgets but
do not consume the page URL limit. CSS imports, JavaScript modules and browser
network recording are excluded. Offline reanalysis is a separate
``seo_scan_reanalyze`` operation over retained evidence; it never contacts
the network and names unavailable live-only inputs. Audit
creation has a finite population/output limit and may return unavailable
while preserving the scan. Supply ``producer_build`` when source provenance
cannot be determined from a clean checkout.

``resume`` names an interrupted ``scan_out`` artifact to continue from its
stored frontier and throttle state. It is the whole input: the start URL
and every crawler setting are read back from the file, since they are what
that frontier was built under, so no other crawl argument may accompany it.
A scan written by a different producing build, for a different start URL,
or already finished is refused by name before the first request.

### `crawl-describe-settings`

MCP name: `seo_crawl_describe_settings`

List every crawl-site config setting: dotted path, type, default value, description, and whether it changes what the audit finds (results-affecting) or only cost/duration. The same source ``crawl-site --config-help`` reads, so an agent can discover the configuration surface without a filesystem.

Takes no arguments.

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `log-scan`

MCP name: `seo_log_scan`

Report claims a finished run makes that cannot all be true at once: a recorded size that disagrees with the file, a check firing more often than there are pages to fire on, a finding about a URL the run never fetched, a summary that disagrees with its own rows. Not a second audit and not a threshold — only contradictions, each naming both values and where each came from, so a surprising number can be traced instead of trusted. ``run`` is a native scan path, or a directory holding scan.sqlite, audit.json or pages.jsonl. Native scans without retained decision logs report those events as unavailable; none are invented. ``images_dir`` is an images-download directory whose manifest lets a recorded size be checked against the bytes on disk.

| Argument | Type | Default |
|---|---|---|
| `run` | `str` | `required` |
| `images_dir` | `str | None` | `None` |
| `max_per_rule` | `int` | `20` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `crawl-diagnose`

MCP name: `seo_crawl_diagnose`

Explain a small or unfinished native crawl from retained scan or run evidence. This MCP tool is read-only and makes no network request. To deliberately write a new redacted JSON file, use ``seo_crawl_diagnose_export`` or the CLI's ``crawl-diagnose-export --export`` command.

| Argument | Type | Default |
|---|---|---|
| `scan` | `str | None` | `None` |
| `run` | `str | None` | `None` |
| `max_decisions` | `int` | `20` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `crawl-diagnose-export`

MCP name: `seo_crawl_diagnose_export`

Create one new redacted crawl-diagnostic JSON file from retained evidence. This tool writes a file with no overwrite and makes no network request. Use the read-only ``seo_crawl_diagnose`` when a file is not needed.

| Argument | Type | Default |
|---|---|---|
| `export` | `str` | `required` |
| `scan` | `str | None` | `None` |
| `run` | `str | None` | `None` |
| `max_decisions` | `int` | `20` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `sitemap-crawl`

MCP name: `seo_sitemap_crawl`

Recursively parse a sitemap (index/urlset, gzip supported) into a URL tree.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `concurrency` | `int` | `3` |
| `project` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Fetches sitemap evidence. An explicit local project records an observation
for the same origin; otherwise no project files are written. Declared URLs
are not fetched HTML pages. Counters count parsed sitemap documents only.

### `images-download`

MCP name: `seo_images_download`

Download images from a URL list, setting the correct extension by content-type and skipping already-downloaded files.

| Argument | Type | Default |
|---|---|---|
| `urls` | `list[str]` | `required` |
| `output_dir` | `str` | `required` |
| `options` | `dict[str, Any] | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

### `images-optimize`

MCP name: `seo_images_optimize`

Compress, convert, or resize raster images and conservatively minify SVG.

| Argument | Type | Default |
|---|---|---|
| `files` | `list[str]` | `required` |
| `settings` | `dict[str, Any] | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

**Behavior and failure modes**

Safe default: settings.out_dir is required. Source mutation needs
settings.in_place=true and creates a backup by default. Existing destinations
require settings.overwrite=true. Animated and multipage images are rejected.

### `keywords-cluster`

MCP name: `seo_keywords_cluster`

Cluster keywords into topic groups (K-Means, DBSCAN, Agglomerative).

| Argument | Type | Default |
|---|---|---|
| `keywords` | `list[str]` | `required` |
| `algorithm` | `str` | `'kmeans'` |
| `n_clusters` | `int | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `robots-check`

MCP name: `seo_robots_check`

Fetch and analyze a site's robots.txt: user-agent groups, declared sitemaps, and whether given paths are crawlable.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `user_agent` | `str` | `'*'` |
| `paths` | `list[str] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `headers-check`

MCP name: `seo_headers_check`

Inspect SEO-relevant response headers (X-Robots-Tag, canonical Link, Cache-Control, HSTS, ...), HTTP version, TTFB, and body size.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `method` | `str` | `'GET'` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `asset-weight-check`

MCP name: `seo_asset_weight_check`

Fetch a page's linked CSS/JS and report delivery problems: render-blocking resources in <head>, oversized files, duplicate libraries bundled more than once (by content hash), missing minification, missing font-display, legacy polyfilled JS, and resources served without compression or a long-lived Cache-Control. Unused-code and cross-page outlier detection need a rendered DOM / a multi-page run and are reported under `skipped`, not silently clean.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `file_size_threshold` | `int | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `links-check`

MCP name: `seo_links_check`

Check a page's outbound links for broken (4xx/5xx) targets and links that point at redirects (wasted crawl hops).

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `internal_only` | `bool` | `False` |
| `limit` | `int` | `200` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `hreflang-check`

MCP name: `seo_hreflang_check`

Extract and validate a page's hreflang alternates (x-default, self-reference, duplicates, malformed codes).

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `domain-profile`

MCP name: `seo_domain_profile`

Infrastructure profile of a domain: registrar and domain age (RDAP, whois fallback), DNS records with DNS/mail provider, hosting IP with ASN, owner and country, reverse DNS, TLS certificate and its expiry, plus risk flags.

| Argument | Type | Default |
|---|---|---|
| `domain` | `str` | `required` |
| `with_tls` | `bool` | `True` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `cdn-check`

MCP name: `seo_cdn_check`

Which CDN sits in front of a URL and whether caching actually works: cache status on a repeat request (MISS->HIT), Cache-Control, ETag/Last-Modified, 304 revalidation, HTTP version, HTTP/3 advertisement, brotli/gzip and TTFB.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `tech-detect`

MCP name: `seo_tech_detect`

Detect the technologies behind a page: CMS, framework, server stack, analytics and ad pixels, chat widgets, consent tools, fonts and third-party script hosts. Every hit carries the marker it was detected by.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `security-check`

MCP name: `seo_security_check`

Security headers with a score and grade (HSTS, CSP, X-Frame-Options, X-Content-Type-Options, Referrer-Policy, Permissions-Policy), software version disclosure, cookie flags and the http->https upgrade. Set probe_paths=true to also check whether .git/.env and similar service files are exposed.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `probe_paths` | `bool` | `False` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `backlinks-check`

MCP name: `seo_backlinks_check`

Verify backlinks from a list of donor pages: is the link still there, its anchor and rel, whether it passes weight (nofollow/ugc/sponsored), and whether the donor page itself is indexable.

| Argument | Type | Default |
|---|---|---|
| `target` | `str` | `required` |
| `donors` | `list[str]` | `required` |
| `concurrency` | `int` | `3` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `schema-check`

MCP name: `seo_schema_check`

Validate a page's structured data in two layers. Layer one is the Schema.org vocabulary itself (1010 types, 1676 properties, shipped with the toolkit): does the type exist, is the property allowed on it once inheritance is resolved, does the value type match, is the term deprecated or still in the pending layer. Layer two is Google rich-result eligibility per type. Also analyses the JSON-LD as a GRAPH: which entities carry @id, which are linked, which hang as islands, and whether any @id reference dangles. Pass html to check markup offline.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `''` |
| `html` | `str` | `''` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `schema-build`

MCP name: `seo_schema_build`

Suggest a connected Schema.org @graph for a page. Classifies the page (Article/Product/Service/LocalBusiness/...) from URL + content + existing JSON-LD, shows the signals it used and its confidence, then builds a linked graph (Organization <- WebSite <- WebPage <- <type>) using ONLY facts actually visible on the page. Also diffs the suggestion against markup the page already carries: which recommended fields are missing, which it can fill now. Set override_type when the classifier is unsure (confidence=low).

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `''` |
| `html` | `str` | `''` |
| `override_type` | `str` | `''` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `duplicate-check`

MCP name: `seo_duplicate_check`

Find near-duplicate pages among a list of {id, text} documents using simhash + locality-sensitive hashing (no O(n^2) pairwise comparison). Returns exact duplicates (by content hash) separately from near-duplicate clusters (similarity at or above the threshold, with exact pairwise similarity inside each cluster), so a byte-identical pair is never reported twice. Feed it page texts from a crawl (SF export, sitemap + parse) to surface thin/duplicate content on large sites; ideally each item's text is already scoped to the page's content area (see seo_markdown_extract or parse's content_text field), so shared navigation and footer boilerplate does not create false matches. only_indexable=True (default) compares only items whose indexable flag is true or absent, since a page canonicalised to another is an intended twin, not a defect; set it to false to audit the canonical tags themselves. Pass scan for a validated read-only scan.v1 corpus; it streams retained page bodies and returns coverage. The corpus is capped at 10,000 analyzed documents and 16 MiB of retained input (extracted Markdown, not raw HTML); reaching a corpus bound reports the exact partial coverage. The analysis itself stays capped at one million shingles and 250,000 candidate comparisons; an exhausted analysis budget is unavailable, never clean.

| Argument | Type | Default |
|---|---|---|
| `items` | `list[dict] | None` | `None` |
| `scan` | `str | None` | `None` |
| `threshold` | `float` | `0.92` |
| `with_fingerprints` | `bool` | `False` |
| `only_indexable` | `bool` | `True` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `mirror-check`

MCP name: `seo_mirror_check`

Audit host consolidation across HTTP/HTTPS, ``www``, index.php/index.html, case, and trailing-slash variants. Returns every redirect hop and identifies live 200 duplicates, HTTPS-to-HTTP downgrades, chains longer than one hop, dead variants, and ``www`` DNS availability. DNS is checked through DNS-over-HTTPS rather than the machine's local resolver so local cache or split-DNS state does not create false evidence.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `timeout` | `float` | `12.0` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `ai-bots-check`

MCP name: `seo_ai_bots_check`

Which AI crawlers (GPTBot, ClaudeBot, Perplexity, Google-Extended, CCBot, Bytespider, Meta-ExternalAgent, …) the site lets in, and which it blocks in robots.txt. For each bot: its role (training/retrieval/user), whether it has an explicit robots group, and whether the root path is blocked. Pass robots_text to check offline; otherwise it fetches /robots.txt from the url's host.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `''` |
| `robots_text` | `str` | `''` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `llms-txt-check`

MCP name: `seo_llms_txt_check`

Fetch and score the site's /llms.txt (the LLM-facing manifest): 9 checks — H1 title, >=3 sections, >=3 links, brand mention, category mention, product/ proof/docs pages, size <= 60KB. Returns a 0-10 score, a letter grade, and the per-check breakdown. A missing llms.txt is itself a finding (no AI-ready context). Set brand to verify the project name is mentioned.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `brand` | `str` | `''` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `citability-check`

MCP name: `seo_citability_check`

Score how citable a piece of content is for AI answers (GEO/AEO): 0-100 across four dimensions (25 each) — Answer Blocks (self-contained 20-200 word paragraphs), Self-Containment (no context-dependent phrases like 'as mentioned above'), Statistical Density (numbers/percentages/dates + evidence markers per 100 words), and Structure Quality (headings/lists/TL;DR). Pass text to score a fragment exactly as given, or url to fetch the page and score it: fetching scores the resolved content area's Markdown (navigation and footer excluded, headings/lists/paragraph breaks preserved), not the raw whole-document text, since a flat text blob has no structure for the scorer to find. content_area configures that region — see seo_markdown_extract / seo_parse's content_area option for its keys.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `''` |
| `text` | `str` | `''` |
| `content_area` | `dict[str, Any] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `markdown-extract`

MCP name: `seo_markdown_extract`

Render a page as Markdown in two scopes. content_markdown strips navigation and footer (boilerplate) while keeping headings, lists, and links -- the representation worth diffing between crawls, feeding to content scoring (seo_citability_check, seo_duplicate_check), or handing to a model. full_markdown keeps header/nav/footer too, for reading -- Markdown has already lost the tag structure seo_boilerplate_report hashes, so it is not a valid input there; pass that tool the original html (or a hash precomputed from it) instead. content_area_strategy records how the region was resolved for this page. Pass html to render offline, or url to fetch it first. content_area configures the region (root/include CSS selectors, tag/selector exclusions); defaults exclude <nav> and <footer>.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `''` |
| `html` | `str` | `''` |
| `content_area` | `dict[str, Any] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `boilerplate-report`

MCP name: `seo_boilerplate_report`

Answer "is the boilerplate actually the same everywhere?" across a crawled corpus. Hashes each page's header/nav/footer markup (structure kept, not just text, so a link dropped from a menu still changes the hash), groups pages by that hash, and reports every group that is not the dominant one -- with its fraction of the corpus and a sample URL. Catches a nav block that lost links on one template, a footer never migrated on old pages, or a menu that renders differently under one language branch. Each page is {"url", "html"}, or {"url", "hash"} when the hash was already computed upstream. Pass scan for a validated read-only scan.v1 corpus with retained page HTML. The corpus is streamed and only each page's digest is retained, so coverage does not depend on total HTML size; reaching the 10,000-document or 16 MiB retained-input bound reports the exact partial coverage.

| Argument | Type | Default |
|---|---|---|
| `pages` | `list[dict] | None` | `None` |
| `scan` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `semantic-inputs`

MCP name: `seo_semantic_inputs`

Build the reproducible normalized-input manifest for semantic analysis over retained page content. Each document entry names the retained body hash, the exact decoded input hash, the normalized output hash, the content-area strategy that was applied, and the language evidence (the page's own <html lang> declaration plus letter-script shares over the normalized text) — the normalized text itself is never returned. Pass scan for a validated read-only scan.v1 corpus: it streams retained complete bodies offline with no refetch, under the crawl's recorded content_area config, and a missing or partial body stays an explicit omission — never an empty clean result. Or pass items as a list of {"url", "html"} to normalize supplied markup offline under an optional content_area config. The corpus is capped at 10,000 documents and 16 MiB of normalized retained input; reaching a bound reports the exact partial coverage.

| Argument | Type | Default |
|---|---|---|
| `items` | `list[dict] | None` | `None` |
| `scan` | `str | None` | `None` |
| `content_area` | `dict[str, Any] | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `semantic-similarity`

MCP name: `seo_semantic_similarity`

Group topical and internal-link review candidates from supplied vectors.

| Argument | Type | Default |
|---|---|---|
| `items` | `list[dict] | None` | `None` |
| `scan` | `str | None` | `None` |
| `embeddings` | `list[dict] | None` | `None` |
| `adapter` | `dict | None` | `None` |
| `cache_path` | `str` | `''` |
| `threshold` | `float` | `0.82` |
| `max_candidate_comparisons` | `int` | `250000` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Pass either supplied ``items`` ({url, html}) or a retained scan.v1
``scan``; scan selection and normalization reuse seo_semantic_inputs.
``embeddings`` contains {url, vector} rows and ``adapter`` declares the
model/version/settings and transfer policy. SEOHEAD does not download,
load or call a model here. A provider declaration requires explicit
external_authorized=true even though this route only consumes supplied
vectors. cache_path is a local SQLite cache keyed by source hash, model
identity and settings. Groups are review candidates, never duplicate or
cannibalization conclusions; missing vectors and bounded coverage stay
explicit in the response.

### `meta-description-drafts`

MCP name: `seo_meta_description_drafts`

Prepare or validate a resumable, page-grounded meta-description batch.

| Argument | Type | Default |
|---|---|---|
| `items` | `list[dict] | None` | `None` |
| `scan` | `str | None` | `None` |
| `context` | `dict | None` | `None` |
| `drafts` | `list[dict] | None` | `None` |
| `executor` | `dict | None` | `None` |
| `checkpoint_path` | `str` | `''` |
| `batch_size` | `int` | `20` |
| `json_path` | `str` | `''` |
| `csv_path` | `str` | `''` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

With no drafts this is a dry-run plan over supplied HTML or a retained
scan.v1 corpus. With structured supplied drafts it validates URL/source
hashes, Unicode length policy and review flags, then checkpoints only
local results. The calling or delegated agent owns generation; this tool
has no model key or provider call. executor declares its versioned
contract and runtime kind, while checkpoint_path enables resume. Optional
json_path and csv_path export a review artifact; neither path writes a
CMS or metadata.

### `ai-column`

MCP name: `seo_ai_column`

Plan a per-URL AI custom column over retained page evidence, then validate its values.

| Argument | Type | Default |
|---|---|---|
| `items` | `list[dict] | None` | `None` |
| `scan` | `str | None` | `None` |
| `prompt` | `str` | `''` |
| `urls` | `list[str] | None` | `None` |
| `column` | `str` | `'ai_column'` |
| `max_pages` | `int` | `100` |
| `rows` | `list[dict] | None` | `None` |
| `csv_path` | `str` | `''` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

With no rows this is a dry-run plan: a prompt, the selected URLs' title,
headings and text excerpt from a retained scan.v1 or supplied items, a
character-based size estimate with no price applied, and the consent scope.
With rows, each value must echo the page's source hash and is validated
for presence, staleness and length; missing or failed pages are reported,
never filled. The calling or delegated agent owns the prompt run and the
transfer of data; this tool has no model key and makes no provider call.
Optional csv_path writes a formula-safe local CSV and never writes back to
a site or CMS.

### `social-meta-check`

MCP name: `seo_social_meta_check`

Check OpenGraph and Twitter Card tags against the minimum needed for a link preview to render: which required tags (og:title/type/url/image/image:alt, twitter:card/title/description/image/image:alt) are missing, and which recommended ones. Pass url to fetch and check, or hand in pre-extracted og/twitter dicts.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `''` |
| `og` | `dict | None` | `None` |
| `twitter` | `dict | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `soft404-check`

MCP name: `seo_soft404_check`

Detect soft-404: whether the site returns an honest 404/410 for non-existent URLs, or silently serves 200/3xx (which pollutes the index with junk pages). Sends two deterministic probes (sha256 of origin, at ordinary root paths) and applies strict AND-logic: both 2xx/3xx -> soft-404 confirmed (warning); both 404/410 -> pass; anything else -> unknown. Screaming Frog cannot see this — it crawls known URLs, not invented ones, so this needs an active request.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `log-analyze`

MCP name: `seo_log_analyze`

Analyse a web server access log (Apache/Nginx Common or Combined, IIS W3C). Shows who actually crawls the site and how: hits per bot, bytes, unique IPs, response codes served to bots (they differ from what humans get more often than people expect), which site sections each bot family visits, top paths and a daily trend. Set verify_bots=true to check bot authenticity by forward-confirmed rDNS — a spoofed User-Agent is one line, a forged PTR of google.com is not. That check hits the network, and if reverse DNS is unavailable it says so instead of declaring every bot fake.

| Argument | Type | Default |
|---|---|---|
| `path` | `str` | `required` |
| `verify_bots` | `bool` | `False` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `regions-check`

MCP name: `seo_regions_check`

Audit a site's regional structure: subdomains (msk.site.ru), folders (site.ru/msk/) and satellite domains (site-msk.ru). Finds the city switcher on the page, recognises ~100 Russian cities by URL slug (msk/moskva/moscow all collapse to one region) and by Cyrillic anchor text, then fetches each regional page and reports what actually kills regional promotion: hosts that just redirect to the main domain (so the region does not exist), regional pages canonicalised to another host (self-removal from the index), noindex, identical content across cities, one shared phone number for the whole country, a missing city name in the title, and two schemes used at once. Satellite domains are invisible from the page — pass them in `extra`. Set render=true when the city switcher is drawn by JavaScript (needs Playwright) — on many large sites it is not in the raw HTML at all.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `extra` | `list[str] | None` | `None` |
| `limit` | `int` | `12` |
| `render` | `bool` | `False` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `render-check`

MCP name: `seo_render_check`

Compare the raw server HTML with the DOM after JavaScript runs — the gap between them is what a non-rendering crawler loses. Reports an empty SPA shell (<div id="root"></div> means a robot gets a blank page), the share of text and internal links that appear only after JS, a title/canonical rewritten by script, and Schema.org markup injected client-side. Also returns lab timings (TTFB, FCP, LCP, CLS, load) measured in one Chromium run — these are lab numbers, not field Core Web Vitals from CrUX, and are labelled metrics_lab for that reason. Also returns dual_crawl (schema dualcrawl.v1): per-URL image/link evidence seen by only the raw pass or only the rendered pass, a separate question from the raw/rendered diff above. Requires Playwright; if it is missing the tool says so and gives the install command instead of failing. A render that did not finish — a document with no title, no h1, no canonical and no links, far smaller than the raw response — comes back as ok:false with reason "incomplete_render" and both snapshots, never as findings about the site: an unmeasured page is not a defect. A requested wait milestone that times out (networkidle on a site with long-polling scripts) falls back to reading the DOM at domcontentloaded, recorded in wait_reached. `viewport="mobile"` uses a stable smartphone diagnostic identity; user_agent overrides that identity for both requests. `transport_config` opts into an operator-supplied remote Playwright connection using `transport=remote`, `remote_protocol=playwright`, `remote_endpoint_env` and `remote_playwright_version`. It never provisions a server or falls back to local launch after a remote failure.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `viewport` | `str` | `'desktop'` |
| `wait` | `str` | `'load'` |
| `user_agent` | `str | None` | `None` |
| `transport_config` | `dict[str, str] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `site-audit`

MCP name: `seo_site_audit`

Run the whole live toolkit over one site and return a single audit document (schema seohead.site-audit/1). Site-level tools run once (domain profile, CDN and cache, tech stack, security headers, robots, AI crawlers, llms.txt, regions, raw-vs-rendered, sitemap); page-level tools run per URL (parse, Schema.org, Open Graph). URLs come from the sitemap unless you pass `urls`. Every finding is collected into one sorted list with a severity assigned by aggregator rules — the document says so explicitly, because severity here is a rule, not a measurement. A tool that fails does NOT fail the audit: it lands in summary.tools_failed with its reason, so silence is never mistaken for a clean result. Feed the returned document straight into seo_report_build. Optional crux_evidence is an already collected CrUX current record or bounded sample; no Google request occurs here. URL and origin field scopes remain distinct from Lighthouse lab results.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `urls` | `list[str] | None` | `None` |
| `limit` | `int` | `25` |
| `concurrency` | `int` | `5` |
| `render` | `bool` | `False` |
| `skip` | `list[str] | None` | `None` |
| `crux_evidence` | `dict[str, Any] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `report-build`

MCP name: `seo_report_build`

Turn an audit document into a file: xlsx, docx, csv, md, json or pdf. Pass the dict returned by seo_site_audit, an SF Analyzer audit.json from sf_audit_run (or a path to either one's JSON, or a validated scan.v1 SQLite artifact) — both audit schemas are recognized and normalized before rendering. xlsx has four sheets with filters and a live Excel chart — for work; docx is prose with headings — for the client; csv writes separate findings, scope-evidence, and page tables for a tracker, listed under outputs; md is for reading and for git; pdf is a localized offline Chromium printout (en or ru). PDF requires the optional `pdf` dependencies and a local Chrome, Edge or Chromium. The generators compute nothing and reach no network: what is not in the JSON does not appear in the report. A document matching neither schema is refused with ok: false naming the mismatch, never rendered as an empty report. Pass project to include validated checklist coverage, reasons, scope and measurements in a human report. Optional view applies one saved finding view; it leaves health, evidence, coverage and source scan untouched. offset pages through the stable sorted view. This never makes a network request.

| Argument | Type | Default |
|---|---|---|
| `audit` | `dict | str` | `required` |
| `fmt` | `str` | `'xlsx'` |
| `out` | `str | None` | `None` |
| `project` | `str | None` | `None` |
| `view` | `str | None` | `None` |
| `offset` | `int` | `0` |
| `lang` | `str` | `'en'` |
| `pdf_policy` | `Literal['overview-v1'] | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

For retained audit.v2 only, explicit pdf_policy='overview-v1' writes a bounded
PDF overview with mandatory complete JSON/CSV companions and a hashed manifest.
Exact source, displayed and omitted counts remain visible. No policy is chosen
automatically; streamed PDF without this policy is refused. Other formats and
materialized inputs reject the policy. PDF and companion directory must be new.

### `facts-export`

MCP name: `seo_facts_export`

Build one comparable facts table (schema facts.v1) across several sites from crawl/site audits you already produced. Each site is {label, crawl_audit?, site_audit?} -- crawl_audit is an SF Analyzer audit.json (dict or path), site_audit is a seo_site_audit document (dict or path), both optional but at least one is needed for real facts; a site with neither still gets a full row of unavailable facts, named. Reads documents only: zero network requests, zero paid calls. Every leaf is a fact with one of five states -- measured, absent (a true 0/[]), partial (bounded, e.g. a crawl that stopped early), unavailable (the source did not answer), or not_requested (this tool never asks, as for Google/Yandex index counts: no configured provider returns a result count) -- plus an is_not line saying what it must not be read as. This tool computes no score, rank, ratio or winner: it exports operands, not quotients, so a language model can compare the sites itself. Refuses before doing any work when a document's own domain does not match the label it is filed under, or when two labels resolve to the exact same domain; two subdomains of one registrable domain are allowed, only noted.

| Argument | Type | Default |
|---|---|---|
| `sites` | `list[dict[str, Any]]` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `compare-crawls`

MCP name: `seo_compare_crawls`

Diff two audit documents (dict, JSON path, or scan.v1 SQLite path) into four disjoint sets per finding: entered (new problem on a page that existed before), left (the page is still crawled and no longer matches — a real fix), appeared (a genuinely new page with a finding), disappeared (the page is not in this crawl at all, so a missing finding proves nothing). "left" and "disappeared" look identical in a naive diff and mean opposite things. Optional ``correspondence`` is a closed url-correspondence.v1 object (or local JSON path) declaring origin and explicit URL pairs for a migration; it never infers pairs from titles or content and adds a release_review.v1 facts/finding artifact. Refuses a known difference in results-affecting settings unless ``force`` is true; partial-crawl warnings remain attached to the historical result.

| Argument | Type | Default |
|---|---|---|
| `before` | `Any` | `required` |
| `after` | `Any` | `required` |
| `force` | `bool` | `False` |
| `correspondence` | `Any` | `None` |
| `out_dir` | `str | None` | `None` |
| `compression` | `Literal['none', 'gzip']` | `'none'` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

With out_dir, writes a new compare.v2 package with partition manifests,
exact finding rows, checksums and before/after count conservation. Without
it, returns the small compare.v1 document; retained audit.v2 populations
above 10,000 pages or issues require explicit out_dir. Never overwrites
a package and never collects evidence or calls the network. Explicit
compression='gzip' requires out_dir and preserves every row in deterministic
.ndjson.gz files; manifests record compressed checksums and both byte counts.
Default compression='none' keeps uncompressed output.

### `verify-fixes`

MCP name: `seo_verify_fixes`

Recheck selected baseline findings in an explicit, bounded URL subset.

| Argument | Type | Default |
|---|---|---|
| `baseline` | `Any` | `required` |
| `out_dir` | `str` | `required` |
| `finding_ids` | `list[str] | None` | `None` |
| `view` | `Any` | `None` |
| `urls` | `list[str] | None` | `None` |
| `urls_file` | `str | None` | `None` |
| `after` | `Any` | `None` |
| `config` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Select baseline finding IDs, a saved verification_view.v1 JSON view, or
affected URLs. With ``after`` the comparison is offline and requires a
distinct scan UUID plus a later observation time. Otherwise the
recorded HTTP/robots/render policy is verified before the existing crawler
fetches selected pages; JS baselines use one URL per rendered crawl. A new
directory receives the recrawl evidence and immutable verification JSON
and Markdown report. Unfetched, skipped, site-wide and incomparable
findings remain not_verifiable, never resolved.

### `crawl-enrich`

MCP name: `seo_crawl_enrich`

Join an existing audit or scan to an offline URL-keyed CSV without a provider call. Matched rows retain both page and external data; crawl-only, external-only, and unkeyable rows remain separate. A completed crawl can write same-origin external-only URLs to ``out_urls`` for `crawl-site --urls-file`; a partial crawl never labels them as orphan URLs.

| Argument | Type | Default |
|---|---|---|
| `audit` | `Any` | `required` |
| `external_csv` | `str` | `required` |
| `url_column` | `str` | `'url'` |
| `ignore_query` | `bool` | `False` |
| `ignore_scheme` | `bool` | `False` |
| `casefold_path` | `bool` | `False` |
| `out_urls` | `str | None` | `None` |
| `visits_column` | `str | None` | `None` |
| `bounce_column` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `crawl-import`

MCP name: `seo_crawl_import`

Read a versioned third-party crawl CSV bundle from a local manifest.

| Argument | Type | Default |
|---|---|---|
| `manifest_path` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

The manifest maps source headers to page, link, status, and redirect
fields. The result is ``third_party_crawl.v1`` with source identity and
per-field coverage; it is not scan.v1 or SF audit evidence.

### `segment-diff`

MCP name: `seo_segment_diff`

Cross-segment counterpart diff (#358): which pages in the ``source`` segment (e.g. "en") have a counterpart in the ``target`` segment (e.g. "pl"), and which do not. Requires a native crawl whose scope.segments were declared and whose pages carry hreflang alternates (#357) — an audit missing either is refused by name. Classifies every eligible page into exactly one of: declared (hreflang names the counterpart and it was crawled), declared_not_crawled (hreflang names one the crawl never reached), inferred (no hreflang, but the mirrored path exists and this site's own mirror rate measured high enough to trust that), absent (no counterpart by either method), or undetermined (naming why neither method could safely answer — e.g. a translated slug on a site whose mirror rate is too low for path inference, or a target segment the crawl only partially reached, where "absent" would be an unsupported negative claim).

| Argument | Type | Default |
|---|---|---|
| `audit` | `Any` | `required` |
| `source` | `str` | `required` |
| `target` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `keywords-expand`

MCP name: `seo_keywords_expand`

Expand a seed phrase via Yandex Wordstat: refinements (left column) plus similar queries (right column), each with its base frequency. Frequency here is BASE, not exact — the API has no !/+/[] operators and base runs roughly 9x higher than exact; good enough for a first cut, then top up exact via seo_keywords_exact. A multi-region request SUMS frequency, so query regions one at a time. Paid and quota-bound: 100 Wordstat requests per hour.

| Argument | Type | Default |
|---|---|---|
| `phrase` | `str` | `required` |
| `limit` | `int` | `300` |
| `regions` | `list[str] | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota

### `keywords-seasonality`

MCP name: `seo_keywords_seasonality`

Demand over time for one phrase (Yandex Wordstat dynamics). Dates are RFC3339, e.g. 2026-01-01T00:00:00Z. Use it to tell a dead query from a seasonal one before deciding a page is not worth building.

| Argument | Type | Default |
|---|---|---|
| `phrase` | `str` | `required` |
| `from_date` | `str` | `required` |
| `to_date` | `str` | `required` |
| `period` | `str` | `'PERIOD_MONTHLY'` |
| `regions` | `list[str] | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota

### `keywords-exact`

MCP name: `seo_keywords_exact`

Exact frequency (!W) for a list of phrases via Arsenkin — the number Wordstat's API will not give you. Paid, spends account limits. The charge and task_id are journaled the moment the task is created, so a paid result is never lost: pass wait=false to get the task_id and collect the result later for free.

| Argument | Type | Default |
|---|---|---|
| `keywords` | `list[str]` | `required` |
| `region` | `int` | `225` |
| `wait` | `bool` | `True` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota

**Behavior and failure modes**

On success with wait=true: ok, task_id, cost, region, frequencies, result (the raw
provider payload). frequencies maps each phrase to {"base": N, "overal": N} —
overal is Arsenkin's field for !W (!WS, exact wordform); quoted is the "WS" phrase
operator and exact is the [!WS] strict-order operator, so neither appears here as
!W. A phrase with no data for the requested region is omitted and named in
warnings instead of borrowing another region's number; an absent frequency field
stays null rather than becoming zero. cleaned lists phrases whose punctuation was
stripped because the provider rejects them; those phrases are measured in their
rewritten form. With wait=false the task is only created: the reply is ok,
task_id, cost, region, cleaned, and a recovery note — no frequencies.

### `serp-fetch`

MCP name: `seo_serp_fetch`

Yandex search results for one query or a batch. Async only: the synchronous endpoint is materially more expensive and deliberately absent. A batch launches all operations at once and polls them together, so N queries take one batch's time, not N times one. Use it to see who actually ranks before promising a client a position.

| Argument | Type | Default |
|---|---|---|
| `query` | `str | None` | `None` |
| `queries` | `list[str] | None` | `None` |
| `region` | `str` | `'225'` |
| `top` | `int` | `10` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota

### `google-keywords`

MCP name: `seo_google_keywords`

Google demand via DataForSEO: pass `keywords` for search volume and competition on a list, or `seed` to expand semantics from one phrase (the Google counterpart of Wordstat's left column). `difficulty=true` returns keyword difficulty instead of volume.

| Argument | Type | Default |
|---|---|---|
| `keywords` | `list[str] | None` | `None` |
| `seed` | `str | None` | `None` |
| `location_code` | `int` | `2840` |
| `language` | `str` | `'en'` |
| `country` | `str | None` | `None` |
| `limit` | `int` | `100` |
| `difficulty` | `bool` | `False` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota

**Behavior and failure modes**

IMPORTANT: DataForSEO does not support locations in Russia or Belarus.
A request with such a country does NOT go out — it returns a refusal naming what to use
instead (Wordstat, Arsenkin). Use seo_keywords_expand / seo_keywords_exact for Yandex
and the Russian-speaking market; this tool is for Google and everywhere else.

Default environment is `sandbox`: real response shape, fake data, nothing charged.
Production is an explicit opt-in via DATAFORSEO_ENV=prod.

### `google-serp`

MCP name: `seo_google_serp`

Google organic results for a query — who actually ranks. Same geo rules as seo_google_keywords: Russia and Belarus are not covered, use seo_serp_fetch (Yandex) for those. Default environment is sandbox.

| Argument | Type | Default |
|---|---|---|
| `query` | `str` | `required` |
| `location_code` | `int` | `2840` |
| `language` | `str` | `'en'` |
| `depth` | `int` | `10` |
| `country` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota

### `topvisor-read`

MCP name: `seo_topvisor_read`

Read existing Topvisor projects, competitors, groups, keywords, history or summary. Uses topvisor/access_token and topvisor/user_id under the central credential root (or TOPVISOR_TOKEN and TOPVISOR_USER_ID). One page only: limit defaults to 100, maximum 1000; the continuation signal is the provider's nextOffset — present on non-final pages, absent on the last — not len(result) == limit. Provider total and limitedBy pass through when sent, separate from the echoed request limit/offset. projects/competitors/groups/keywords return arrays; history and summary return objects (history rows at result.keywords; summary covers the two requested dates). Non-project operations require project_id. History regions_indexes are project region indexes (projects with show_searchers_and_regions:2), not geographic region keys; summary takes the singular region_index. In positionsData, position is an integer ordinal rank; "--" means the query had no position inside the checked depth — unavailable, not rank 0 or 100 — and a requested date without an entry is a missing observation. headers.dates lists dates actually returned; topsByDepth is percent of queries in Top N; visitors, dynamics and tops are counts; avgs is an average rank. Does not launch checks, add/edit/delete records, or authorize paid operations. Transport redirects are refused and provider errors are redacted.

| Argument | Type | Default |
|---|---|---|
| `operation` | `str` | `'projects'` |
| `params` | `dict[str, Any] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `metrika-counters`

MCP name: `seo_metrika_counters`

List the Yandex Metrika counters this token can see (id, name, site). Start here to get the counter_id the report tools need. Requires a Metrika OAuth token.

Takes no arguments.

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `metrika-setup`

MCP name: `seo_metrika_setup`

How a counter is configured: goals, filters, data operations. Check this BEFORE drawing conclusions from traffic. Operations can silently reshape reports (URL parameter trimming, for instance), and with no goals configured there are no conversions in the data at all — so "zero conversion" in a report would be a consequence of setup, not a fact about the site.

| Argument | Type | Default |
|---|---|---|
| `counter_id` | `str` | `required` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `metrika-report`

MCP name: `seo_metrika_report`

What visitors actually did, as flat records. metrics and dimensions are comma-separated in API notation (ym:s:visits, ym:s:startURL); dates accept relative forms like 30daysAgo. This is the missing half of an audit: a page can be technically perfect and get no visits at all. paginate=true walks every page but stops at 100 000 rows, and says so via "capped". A "Query is too complicated" refusal is retried month by month and, when a month still refuses, at a sampled accuracy; "split", "accuracy", "sampled" and "sample_share" in the answer say what was actually used — a null "sampled" means the API did not report it, not "unsampled". Only count metrics additive over disjoint periods (ym:s:visits, ym:s:pageviews) can be merged — unique-visitor, ratio or average metrics fail rather than sum wrong.

| Argument | Type | Default |
|---|---|---|
| `counter_id` | `str` | `required` |
| `metrics` | `str` | `required` |
| `dimensions` | `str | None` | `None` |
| `date1` | `str` | `'30daysAgo'` |
| `date2` | `str` | `'today'` |
| `filters` | `str | None` | `None` |
| `sort` | `str | None` | `None` |
| `limit` | `int` | `100` |
| `paginate` | `bool` | `False` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `metrika-traffic-pdf`

MCP name: `seo_metrika_traffic_pdf`

Static A4-landscape traffic report (HTML + PDF) in the style of an analytics dashboard, built from Yandex Metrika: KPI cards with % change, daily dynamics against the previous period and a year earlier, 3/6/12-month windows, search engines, cities, countries, devices, age, gender, top landing pages and search phrases, and all traffic channels.

| Argument | Type | Default |
|---|---|---|
| `out_dir` | `str` | `required` |
| `counter_id` | `str | None` | `None` |
| `date1` | `str | None` | `None` |
| `date2` | `str | None` | `None` |
| `document` | `dict[str, Any] | str | None` | `None` |
| `attribution` | `Literal['last_significant', 'last_click']` | `'last_significant'` |
| `traffic` | `Literal['organic', 'all']` | `'organic'` |
| `filters` | `str | None` | `None` |
| `lang` | `Literal['en', 'ru'] | None` | `None` |
| `top` | `int` | `15` |
| `site_label` | `str | None` | `None` |
| `brand` | `dict[str, Any] | str | None` | `None` |
| `gsc_rows` | `list[dict[str, Any]] | None` | `None` |
| `gsc_site_url` | `str | None` | `None` |
| `render` | `bool` | `True` |
| `pdf` | `bool` | `True` |
| `overwrite` | `bool` | `False` |
| `timeout` | `float` | `120.0` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

**Behavior and failure modes**

Collection (counter_id, date1, date2 as YYYY-MM-DD) makes read-only Reporting API calls
(about 40 requests, free, Metrika quota) and writes metrika-traffic.json; pass an existing
document instead to render offline. Attribution defaults to the last significant source
(the figures Metrika's own interface shows); last_click is explicit. traffic=organic keeps
search-engine visits; the channels block always covers all traffic. Every block carries
status ok/unavailable/skipped with a reason; missing comparisons are null, never zero.
Optional Search Console queries come from gsc_rows or gsc_site_url (a Search Console
request). Writes only into out_dir; existing report files are refused unless
overwrite=true replaces them. The PDF needs an installed Chrome, Edge or Chromium
(SEOHEAD_CHROME overrides discovery); without one the HTML is still written and pdf is
reported as skipped. brand is a JSON object or file: name, accent, ink, card,
table_header, positive, negative, font_stack, logo_text.

### `sources-sync`

MCP name: `seo_sources_sync`

Accumulate provider data in one local SQLite database: fetch missing or explicitly forced days. source is gsc, ga4, metrika, webmaster (queries per day) or webmaster_history (daily shows/clicks, pages in search, crawl by HTTP class); resource is the property (sc-domain:example.com), GA4 property ID, Metrika counter ID or Webmaster host ID (https:example.com:443). Pass db (a file path) or project (its sources.sqlite is used). Default period is the last 28 eligible days. Requested lagged days remain visible without being fetched. force=true re-fetches stored days; a partial or failed retry cannot erase a complete day. Tokens are never stored.

| Argument | Type | Default |
|---|---|---|
| `source` | `str` | `required` |
| `resource` | `str` | `required` |
| `start_date` | `str | None` | `None` |
| `end_date` | `str | None` | `None` |
| `db` | `str | None` | `None` |
| `project` | `str | None` | `None` |
| `force` | `bool` | `False` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

### `sources-status`

MCP name: `seo_sources_status`

Offline local history: requested, complete, empty, partial, failed, pending and lagged dates, gaps, rows, metric additivity and reporting-timezone policy.

| Argument | Type | Default |
|---|---|---|
| `db` | `str | None` | `None` |
| `project` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `sources-export`

MCP name: `seo_sources_export`

Read stored rows of one source filtered by resource, date range and a substring of any dimension (match). Returns at most limit ordered rows with their daily coverage state; out writes the same bounded result to a new private CSV. The response names non-additive metrics and has_more when additional rows exist.

| Argument | Type | Default |
|---|---|---|
| `source` | `str` | `required` |
| `resource` | `str | None` | `None` |
| `start_date` | `str | None` | `None` |
| `end_date` | `str | None` | `None` |
| `match` | `str | None` | `None` |
| `limit` | `int` | `1000` |
| `out` | `str | None` | `None` |
| `db` | `str | None` | `None` |
| `project` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `regions-tree`

MCP name: `seo_regions_tree`

Authoritative tree of Yandex region IDs for the regions[] parameter. This is the only FREE Wordstat method, so it is not journaled. Note that a multi-region request SUMS frequency rather than reporting per region — query regions one at a time.

| Argument | Type | Default |
|---|---|---|
| `save_to` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

### `spend-report`

MCP name: `seo_spend_report`

What the paid sources have actually charged: totals by source, by operation and by day, read from the local journal. Estimating spend by eye has already missed the provider usage was recorded, so check here before and after a large run. since is YYYY-MM-DD. csv_path writes every journal row to that file instead of the totals.

| Argument | Type | Default |
|---|---|---|
| `since` | `str | None` | `None` |
| `csv_path` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `sources-doctor`

MCP name: `seo_sources_doctor`

Inspect redacted credential references, readiness and declared provider operations.

Takes no arguments.

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Configured credentials do not verify account or target permission. This local check makes
no provider requests; use seo_provider_verify for an explicit bounded read.

### `wayback-history`

MCP name: `seo_wayback_history`

Every recorded Wayback Machine snapshot of a URL, oldest first: timestamp, HTTP status, and MIME type at capture time. Free and keyless. Answers what a crawl cannot — *when* a page started returning its current status, and what preceded it. A URL the archive never captured is not an error; it comes back with an empty snapshot list.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `limit` | `int | None` | `None` |
| `from_date` | `str | None` | `None` |
| `to_date` | `str | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `cloudflare-traffic`

MCP name: `seo_cloudflare_traffic`

Bot and human traffic for a Cloudflare zone from edge GraphQL analytics (read-only, free): bots x URLs x status codes x cache status per day, in the same shape as seo_log_analyze. Use it when origin logs are absent. It is aggregated data (source=cloudflare-aggregated), not raw logs: no client IPs, so bots cannot be verified by reverse DNS. Dates are UTC YYYY-MM-DD; the Free plan keeps 32 days. The token is read from CLOUDFLARE_API_TOKEN or SEOHEAD_CLOUDFLARE_TOKEN_FILE.

| Argument | Type | Default |
|---|---|---|
| `zone` | `str` | `required` |
| `since` | `str | None` | `None` |
| `until` | `str | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `crtsh-subdomains`

MCP name: `seo_crtsh_subdomains`

Subdomains discovered from public Certificate Transparency logs (crt.sh). Free and keyless. Every TLS certificate ever issued for a domain is public record, so this finds hosts no page links to — the gap seo_mirror_check and seo_regions_check both currently rely on being told about by hand. crt.sh is a free public service without an SLA; a slow or unavailable response is reported as a failure, never as "zero subdomains".

| Argument | Type | Default |
|---|---|---|
| `domain` | `str` | `required` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `gsc-query`

MCP name: `seo_gsc_query`

Google Search Console: search performance for a verified property (mode=search_analytics: clicks, impressions, position, CTR) or Google's own indexing verdict for one URL (mode=inspect_url).

| Argument | Type | Default |
|---|---|---|
| `site_url` | `str` | `required` |
| `mode` | `str` | `'search_analytics'` |
| `start_date` | `str` | `'28daysAgo'` |
| `end_date` | `str` | `'today'` |
| `dimensions` | `list[str] | None` | `None` |
| `row_limit` | `int` | `1000` |
| `inspection_url` | `str | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Requires an OAuth2 bearer token for an own, verified property — see seo_sources_doctor
and docs/SETUP.md for how to obtain one. A missing token returns an explicit failure
naming what to configure; it never fabricates a result.

### `webmaster-url-queries`

MCP name: `seo_webmaster_url_queries`

Read bounded Yandex Webmaster URL-to-query evidence for a verified host.

| Argument | Type | Default |
|---|---|---|
| `host_id` | `str` | `required` |
| `url` | `str | None` | `None` |
| `url_contains` | `str | None` | `None` |
| `max_urls` | `int` | `100` |
| `max_queries_per_url` | `int` | `500` |
| `start_date` | `str | None` | `None` |
| `end_date` | `str | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

This is provider data, not crawl evidence. It preserves URL/query statistics without
summing CTR or average position across pages; caps remain explicit in the response.

### `miratext-analyze`

MCP name: `seo_miratext_analyze`

Start or resume Miratext analysis; paid and keyword modes need confirmation.

| Argument | Type | Default |
|---|---|---|
| `urls` | `list[str] | None` | `None` |
| `texts` | `list[str] | None` | `None` |
| `my` | `str | None` | `None` |
| `hash` | `str | None` | `None` |
| `check_type` | `str` | `'url'` |
| `keywords` | `str | None` | `None` |
| `paid` | `bool` | `False` |
| `confirm_paid` | `bool` | `False` |
| `timeout` | `int` | `120` |
| `top` | `int` | `100` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota

### `gsc-archive`

MCP name: `seo_gsc_archive`

Manage an explicit local GSC SQLite archive. Status is offline and never creates an absent archive. Prepare creates/extends the queue for a verified property and inclusive YYYY-MM-DD dates without calling Google. Run performs at most max_requests API calls (1..1000, default 1), writes checkpoints and obeys quota/retry waits. Backup makes a verified snapshot at a new backup_path. Only prepare creates a database. Search Analytics can omit anonymized/top-limited rows; never sum different datasets.

| Argument | Type | Default |
|---|---|---|
| `database` | `str` | `required` |
| `action` | `Literal['status', 'prepare', 'run', 'backup']` | `'status'` |
| `site_url` | `str | None` | `None` |
| `start_date` | `str | None` | `None` |
| `end_date` | `str | None` | `None` |
| `max_requests` | `int` | `1` |
| `pause` | `float` | `1.0` |
| `backup_path` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

### `crux-report`

MCP name: `seo_crux_report`

Field Core Web Vitals (LCP, INP, CLS) as real Chrome users experienced them, at the 75th percentile — the honest counterpart to seo_render_check's synthesized-score-free design (issue #59). Pass exactly one of url/origin. Requires a Chrome UX Report API key. No eligible field record and missing metrics remain unavailable. Optional urls samples at most 25 targets; cache_dir enables an explicit local cache. Never substitutes Lighthouse lab metrics for CrUX field data.

| Argument | Type | Default |
|---|---|---|
| `url` | `str | None` | `None` |
| `origin` | `str | None` | `None` |
| `urls` | `list[str] | None` | `None` |
| `form_factor` | `str | None` | `None` |
| `metrics` | `list[str] | None` | `None` |
| `max_samples` | `int` | `25` |
| `cache_dir` | `str | None` | `None` |
| `cache_max_age_hours` | `float` | `24` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `indexnow-submit`

MCP name: `seo_indexnow_submit`

Push up to 10,000 changed URLs to Bing, Yandex, Naver, and Seznam in one call.

| Argument | Type | Default |
|---|---|---|
| `urls` | `list[str]` | `required` |
| `host` | `str` | `required` |
| `key_location` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: yes · spends money: no

**Behavior and failure modes**

IMPORTANT: Google has not joined IndexNow as of 2026 — this does not affect Google's
crawl schedule. Requires a self-generated key published at https://<host>/<key>.txt
before the first call; see docs/SETUP.md. Natural pairing: submit exactly the URLs
seo_compare_crawls reports as new or changed.

### `project-open`

MCP name: `seo_project_open`

Open a local project workspace without executing template references.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `expected_site` | `str | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `project-new`

MCP name: `seo_project_new`

Create a local project workspace; no crawl, checklist execution, or network work starts.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `target` | `str` | `required` |
| `label` | `str | None` | `None` |
| `facts` | `list[dict[str, Any]] | None` | `None` |
| `template_references` | `list[str] | None` | `None` |
| `profile_references` | `list[str] | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `project-sources-link`

MCP name: `seo_project_sources_link`

Bind a provider resource ID to a local project; no provider call or discovery runs.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `service` | `str` | `required` |
| `resource` | `str` | `required` |
| `label` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `project-sources-unlink`

MCP name: `seo_project_sources_unlink`

Remove one resource binding from a local project; no provider call runs.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `service` | `str` | `required` |
| `resource` | `str` | `required` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

### `project-sources-list`

MCP name: `seo_project_sources_list`

List the resource bindings of a local project; reads sources.json only.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `project-status`

MCP name: `seo_project_status`

Show project scan history and named pending checklist/preparation states.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

A stable consumer optionally receives a bounded inbox notice.  Reading a
status never marks notes read or acknowledged.

### `project-progress`

MCP name: `seo_project_progress`

Show a compact, paginated project checklist view and its next actions.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `limit` | `int` | `20` |
| `offset` | `int` | `0` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

The page contains at most 100 checklist items. Audit-task completion is a
percentage only when every included site has an explicit agreed plan and
the shared coverage axis has a measured, nonzero denominator. It is
explicitly task completion, not a site-health or remediation percentage.

### `project-observe`

MCP name: `seo_project_observe`

Read the bounded project observer snapshot: tasks, methods, competitors, retained scan state and the execution-log tail. It never starts work or consumes inbox entries; a consumer only receives its own unread summary. Workflow/checklist receipt observations are metadata-only, not fresh byte verification. Missing historical receipts remain unverified. Explicit project-status and workflow-status retain byte verification by default. Run pagination applies independently per site: all stored running records plus one terminal page, newest admitted first. run_offset must be nonnegative and run_limit 1..100. Totals cover at most 100 retained records per site, not lifetime history; earlier evictions were not counted. A changed runs revision requires the caller to restart terminal pagination.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str | None` | `None` |
| `scan_limit` | `int` | `20` |
| `run_offset` | `StrictInt` | `0` |
| `run_limit` | `StrictInt` | `20` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `project-inbox-submit`

MCP name: `seo_project_inbox_submit`

Persist a specialist note, proposed goal or question without starting any work.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `text` | `str` | `required` |
| `kind` | `Literal['note', 'proposed_goal', 'question']` | `'note'` |
| `references` | `list[str] | None` | `None` |
| `author_role` | `Literal['specialist', 'agent']` | `'specialist'` |
| `expected_revision` | `int | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `project-inbox-list`

MCP name: `seo_project_inbox_list`

List a bounded project inbox page without consuming any entries.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str` | `required` |
| `offset` | `int` | `0` |
| `limit` | `int` | `20` |
| `include_acknowledged` | `bool` | `True` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `project-inbox-read`

MCP name: `seo_project_inbox_read`

Record an agent's explicit inspection; acknowledgment remains separate.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str` | `required` |
| `entry_ids` | `list[str]` | `required` |
| `expected_revision` | `int | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `project-inbox-acknowledge`

MCP name: `seo_project_inbox_acknowledge`

Explicitly acknowledge entries. This never accepts or completes a goal.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str` | `required` |
| `entry_ids` | `list[str]` | `required` |
| `expected_revision` | `int | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `project-inbox-goal`

MCP name: `seo_project_inbox_goal`

Explicitly accept or complete a stored proposed goal; no executor is launched.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `entry_id` | `str` | `required` |
| `state` | `Literal['accepted', 'completed']` | `required` |
| `expected_revision` | `int | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `project-inbox-triage`

MCP name: `seo_project_inbox_triage`

Append an explicit task, goal, competitor, blocked, or rejected note outcome.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `entry_id` | `str` | `required` |
| `outcome` | `dict[str, Any]` | `required` |
| `actor` | `str` | `required` |
| `expected_revision` | `int | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

This records no automated interpretation, does not read or acknowledge the
note, and never launches a scan or a provider operation.

### `project-inbox-unread`

MCP name: `seo_project_inbox_unread`

Return a bounded unread reference summary without changing delivery state.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str` | `required` |
| `limit` | `int` | `10` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `project-event-append`

MCP name: `seo_project_event_append`

Append one structured event to the project journal (events.jsonl).

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `source` | `Literal['agent', 'user', 'scans', 'app']` | `required` |
| `actor` | `Literal['user', 'agent', 'schedule']` | `required` |
| `text` | `str` | `required` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Earlier events are never edited. Nothing is executed; the event only records text.

### `project-event-page`

MCP name: `seo_project_event_page`

Read a newest-first bounded page of project events, optionally filtered.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `offset` | `int` | `0` |
| `limit` | `int` | `50` |
| `source` | `Literal['agent', 'user', 'scans', 'app'] | None` | `None` |
| `query` | `str` | `''` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `remediation-summary`

MCP name: `seo_remediation_summary`

Read explicit remediation and recheck coverage from retained local evidence.

| Argument | Type | Default |
|---|---|---|
| `ledger` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

The result keeps verified original cases, resolved, persisting,
regressed, false-positive-reviewed and unverifiable states separate.
It does not run a crawl or infer that omitted evidence is clean.

### `remediation-create`

MCP name: `seo_remediation_create`

Create one new empty remediation ledger bound to a validated local project.

| Argument | Type | Default |
|---|---|---|
| `path` | `str` | `required` |
| `project_dir` | `str` | `required` |
| `producer_build` | `str` | `required` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

producer_build must be the full lowercase 40-character Git SHA of the
build that writes the ledger. An existing path is refused; ledgers never
overwrite.

### `remediation-ingest`

MCP name: `seo_remediation_ingest`

Ingest one retained saved audit into a local ledger without rerunning a crawl.

| Argument | Type | Default |
|---|---|---|
| `ledger` | `str` | `required` |
| `scan` | `str` | `required` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

**Behavior and failure modes**

The scan is opened read-only. Re-ingesting the same revision is idempotent;
a new revision appends observation history.

### `workflow-start`

MCP name: `seo_workflow_start`

Start a local registered workflow; it performs no scan or provider call.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `scenario_id` | `str` | `required` |
| `steps` | `list[str]` | `required` |
| `expected_revision` | `int` | `0` |
| `context` | `dict[str, Any] | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `workflow-checkpoint`

MCP name: `seo_workflow_checkpoint`

Persist one registered-step result before the next step or agent handoff.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `run_id` | `str` | `required` |
| `step_id` | `str` | `required` |
| `state` | `str` | `required` |
| `evidence` | `list[dict] | None` | `None` |
| `expected_revision` | `int` | `0` |
| `review` | `dict[str, Any] | None` | `None` |
| `phase` | `str | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `workflow-status`

MCP name: `seo_workflow_status`

Recover the exact next registered step after interruption or handoff.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `workflow-execute`

MCP name: `seo_workflow_execute`

Checkpoint a supplied local sequence without executing network work.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `scenario_id` | `str` | `required` |
| `steps` | `list[str]` | `required` |
| `outcomes` | `list[dict]` | `required` |
| `context` | `dict[str, Any] | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `workflow-resume`

MCP name: `seo_workflow_resume`

Reopen only the interrupted registered step after a second-agent handoff.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `run_id` | `str` | `required` |
| `expected_revision` | `int` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `monitor-configure`

MCP name: `seo_monitor_configure`

Configure a disabled local incremental monitor; this starts no schedule or message delivery.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `policy` | `dict` | `required` |
| `expected_revision` | `int` | `0` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `monitor-run`

MCP name: `seo_monitor_run`

Record one bounded retained-scan diff; quiet runs do not notify anyone.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `scan_id` | `str` | `required` |
| `observations` | `list[dict]` | `required` |
| `expected_revision` | `int` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `monitor-schedule`

MCP name: `seo_monitor_schedule`

Claim or recover a local run; no timer, crawl, or delivery starts here.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `action` | `str` | `required` |
| `expected_revision` | `int` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `monitor-collect`

MCP name: `seo_monitor_collect`

Preview or explicitly collect one previously claimed bounded monitor plan.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `expected_revision` | `int` | `required` |
| `apply` | `bool` | `False` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

The default preview makes no HTTP request. apply=true collects only the
validated claim, retains body/validation provenance within its bounds and
records failed, partial or complete outcomes. Expired or interrupted claims
require an explicit new start. No timer, background service or delivery starts.

### `monitor-local-deliver`

MCP name: `seo_monitor_local_deliver`

Record a deduplicated local receipt for one retained monitoring run.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `scan_id` | `str` | `required` |
| `expected_revision` | `int` | `required` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

The destination is fixed to local:receipt. It uses no network or external
transport and does not claim that any recipient outside the project received it.

### `monitor-status`

MCP name: `seo_monitor_status`

Read local monitor policy and its last retained checkpoint.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `remediation-cases`

MCP name: `seo_remediation_cases`

Read a bounded page of exact remediation cases and decision history.

| Argument | Type | Default |
|---|---|---|
| `ledger` | `str` | `required` |
| `check` | `str | None` | `None` |
| `url` | `str | None` | `None` |
| `finding_key` | `str | None` | `None` |
| `limit` | `int` | `100` |
| `offset` | `int` | `0` |
| `source_scan_id` | `int | None` | `None` |
| `group_ref` | `str | None` | `None` |
| `max_bytes` | `int | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `remediation-transition`

MCP name: `seo_remediation_transition`

Append one revision-safe, evidence-bound lifecycle decision.

| Argument | Type | Default |
|---|---|---|
| `ledger` | `str` | `required` |
| `occurrence_key` | `str` | `required` |
| `state` | `str` | `required` |
| `actor` | `str` | `required` |
| `reason` | `str` | `required` |
| `expected_revision` | `int` | `required` |
| `observation_id` | `int | None` | `None` |
| `decided_at` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

**Behavior and failure modes**

Measured outcomes require a later retained observation. A user claim,
missing source or failed fetch cannot resolve a case.

### `remediation-record-verification`

MCP name: `seo_remediation_record_verification`

Attach a retained bounded verification artifact to pending cases.

| Argument | Type | Default |
|---|---|---|
| `ledger` | `str` | `required` |
| `verification_path` | `str` | `required` |
| `actor` | `str` | `required` |
| `expected_revision` | `int` | `required` |
| `occurrence_keys` | `list[str] | None` | `None` |
| `task_id` | `str` | `'unassigned'` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

**Behavior and failure modes**

Every result must map to exactly one pending ledger case. The artifact
byte digest is saved in the decision evidence; ambiguous or stale
batches are rejected atomically.

### `remediation-recheck`

MCP name: `seo_remediation_recheck`

Recheck exact pending ledger cases without widening to a site crawl.

| Argument | Type | Default |
|---|---|---|
| `ledger` | `str` | `required` |
| `baseline` | `str` | `required` |
| `occurrence_keys` | `list[str]` | `required` |
| `actor` | `str` | `required` |
| `expected_revision` | `int` | `required` |
| `out_dir` | `str` | `required` |
| `task_id` | `str` | `'unassigned'` |
| `after` | `str | None` | `None` |
| `config` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

**Behavior and failure modes**

The baseline must be the retained audit that produced each case. The
command writes a separate immutable verification artifact, then records
its hashes and measured outcome atomically in the local ledger.

### `remediation-report`

MCP name: `seo_remediation_report`

Render retained before/after remediation evidence without network access.

| Argument | Type | Default |
|---|---|---|
| `ledger` | `str` | `required` |
| `out_dir` | `str | None` | `None` |
| `limit` | `int` | `100` |
| `offset` | `int` | `0` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

With out_dir this creates a new immutable JSON/Markdown review snapshot;
without it, it returns the JSON-ready report document only.

### `project-facts`

MCP name: `seo_project_facts`

Preview or record project stack facts that stack-aware priorities then read.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `facts` | `list[dict[str, Any]] | None` | `None` |
| `detect` | `bool` | `False` |
| `apply` | `bool` | `False` |
| `consumer` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Supplied facts are operator decisions and always win. detect=true is the only
thing that makes a request: it reads robots.txt and then fetches the project
target once, and an unavailable or ambiguous detection leaves the fact absent
with its reason instead of guessing. The default is a read-only preview;
apply=true records the result in project.json.

### `project-checklist-init`

MCP name: `seo_project_checklist_init`

Initialize or reconcile a local checklist without executing a check, skill, or scenario.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `template` | `dict | None` | `None` |
| `expected_revision` | `int | None` | `None` |
| `plan` | `dict | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

template is an optional data-only ``seohead.checklist-template.v1`` document. plan is an
optional agreed audit scope {reviewer, population, tasks}: population declares kind
(``complete_set``, ``sample`` or ``unknown``), size or enumerated urls, a provenance
``source``, an optional ``name`` and ``reason``, and optional per-``templates``
populations; ``unknown`` keeps the URL denominator null with a reason. Template
populations are agreed sub-populations of the site population: enumerated
template URLs must belong to an enumerated site set, and declared template
membership can never exceed the agreed site size; incoherent plans are refused
rather than trimmed. tasks is
``{kind: all_agreed}`` or a sourced ``{kind: selection, ids, source}`` naming the agreed
checklist items; items outside a selection stay visible as ``not_agreed`` outside every
denominator. A size-only population cannot verify measured-URL membership, so its
numerator counts only enumerated URLs. Recording a plan upgrades the checklist to
``seohead.coverage.v3`` and appends to the retained plan history; an identical
agreement is an idempotent no-op, while a changed agreement starts a new revision and
stale-marks evidence recorded under an earlier agreement instead of shrinking
denominators. The result
returns the current state, revision, counts, coverage axes, views and items; use that
revision for a later conditional write. This never makes a network request.

### `project-checklist-update`

MCP name: `seo_project_checklist_update`

Add or update one local checklist definition without executing it.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `item` | `dict` | `required` |
| `expected_revision` | `int` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

expected_revision prevents a writer from silently replacing a newer checklist. Built-in
identity and source provenance remain validated by the project core. This never makes a
network request.

### `project-checklist-record`

MCP name: `seo_project_checklist_record`

Record supplied evidence for one checklist item without executing its operation.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `item_id` | `str` | `required` |
| `record` | `dict` | `required` |
| `expected_revision` | `int` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

expected_revision prevents an overwrite of newer checklist history. The record is
validated against the item's scope and evidence contract, then the returned status names
remaining, blocked and manual-review work. Accepted shapes, all with a ``reason``:
automatic check ``{"status": "succeeded", "reason", "artifact"}`` where artifact is a
saved scan under ``scans/`` that proves the check ran (scan.v1 or streamed audit.v2);
attempt ``{"status": "failed" | "unavailable" | "running", "reason"}``; manual signoff
``{"status": "succeeded", "reason", "reviewer", "signoff": true}``; reviewed artifact
``{"status": "succeeded", "reason", "reviewer", "review": "approved", "artifact"}``
under ``reports/``; reviewed exclusion ``{"status": "not_applicable", "reason",
"reviewer", "artifact" or "evidence"}``, which otherwise stays ``pending_exclusion``
inside the denominator. This never makes a network request.

### `project-view-list`

MCP name: `seo_project_view_list`

List saved declarative finding views and the current project view-config revision.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `project-view-show`

MCP name: `seo_project_view_show`

Read one saved finding view with its stable identity, schema version and revision.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `name` | `str` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `project-view-save`

MCP name: `seo_project_view_save`

Create or revise a bounded declarative finding view using an expected config revision.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `view` | `dict[str, Any]` | `required` |
| `expected_revision` | `int` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Filters are closed severity/check/URL/segment selections. Sorting and column projection
use registered fields only; no SQL, code, or regular expressions are accepted. This
changes project view configuration only; it does not edit scans or affect scores/tasks.

### `project-view-delete`

MCP name: `seo_project_view_delete`

Delete one saved finding view using the current project view-config revision.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `name` | `str` | `required` |
| `expected_revision` | `int` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

**Behavior and failure modes**

Changes project view configuration only; it does not edit scans or affect scores/tasks.

### `project-view-rename`

MCP name: `seo_project_view_rename`

Rename one saved finding view using the current project view-config revision.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `name` | `str` | `required` |
| `new_name` | `str` | `required` |
| `expected_revision` | `int` | `required` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Keeps the view definition; the new name must be unused. Changes project view configuration
only; it does not edit scans or affect scores/tasks.

### `findings-view`

MCP name: `seo_findings_view`

Apply one saved view to an audit object, JSON file, or validated scan.v1 SQLite artifact.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `name` | `str` | `required` |
| `audit` | `dict | str` | `required` |
| `offset` | `int` | `0` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Returns a deterministic projected page with total matches, missing-field counts,
truncation, source identity, and view/config revisions. Filtering never suppresses
findings or changes audit coverage/scoring; no crawl or provider call occurs.

### `project-priorities`

MCP name: `seo_project_priorities`

Preview stack-aware project priorities from saved facts without network requests.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `policy` | `dict | None` | `None` |
| `apply` | `bool` | `False` |
| `expected_revision` | `int | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

The default is read-only preview. apply=true requires expected_revision and atomically
saves policy provenance; it preserves explicit operator choices and completion evidence.
Read-only status and reports never upgrade coverage schemas. A custom policy is data,
not executable code; omitted policy uses the packaged defaults.

### `project-policy`

MCP name: `seo_project_policy`

Read or explicitly update operator crawl defaults and project admission thresholds.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `policy` | `dict | None` | `None` |
| `apply` | `bool` | `False` |
| `expected_revision` | `int | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `project-prepare`

MCP name: `seo_project_prepare`

Prepare an existing project with a bounded native crawl and saved sitemap coverage.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `template` | `dict | None` | `None` |
| `competitors` | `list | None` | `None` |
| `approve_large_crawl` | `bool` | `False` |
| `producer_build` | `str | None` | `None` |
| `consumer` | `str | None` | `None` |
| `max_urls` | `int | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Reuses the latest suitable saved scan when one answers the request. ``max_urls`` sets the
owner's URL cap for a new crawl. Competitors must be supplied candidates with provenance;
absent sources stay pending. All site checklists remain separate. Paid provider calls are
never hidden in preparation.

### `project-start`

MCP name: `seo_project_start`

Create and prepare a new bounded project; failures leave inspectable pending work.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `target` | `str` | `required` |
| `facts` | `list[dict[str, Any]] | None` | `None` |
| `template` | `dict | None` | `None` |
| `competitors` | `list | None` | `None` |
| `approve_large_crawl` | `bool` | `False` |
| `producer_build` | `str | None` | `None` |
| `consumer` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

### `skill-list`

MCP name: `seo_skill_list`

List the packaged, source-derived method playbooks without executing them.

Takes no arguments.

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `skill-show`

MCP name: `seo_skill_show`

Return a packaged skill's exact text and definition identity.

| Argument | Type | Default |
|---|---|---|
| `name` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `scenario-list`

MCP name: `seo_scenario_list`

List the packaged workflow scenarios with their ordered steps, without running them.

Takes no arguments.

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `scenario-show`

MCP name: `seo_scenario_show`

Return a packaged workflow scenario's text without running its commands.

| Argument | Type | Default |
|---|---|---|
| `name` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `provider-replay`

MCP name: `seo_provider_replay`

Join a saved private provider collection to a saved scan with no network; retain raw rows locally and return counts.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `evidence_file` | `str` | `required` |
| `out_dir` | `str` | `required` |
| `url_column` | `str` | `'url'` |
| `review_external_only` | `bool` | `False` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `provider-auth`

MCP name: `seo_provider_auth`

Manage GSC read-only OAuth grants: import private file, refresh, cancel a pending flow, or explicitly revoke. Never returns secrets.

| Argument | Type | Default |
|---|---|---|
| `provider` | `str` | `required` |
| `action` | `Literal['status', 'connect', 'refresh', 'cancel', 'disconnect', 'revoke']` | `'status'` |
| `grant_file` | `str | None` | `None` |
| `confirm` | `bool` | `False` |

**Cost** — network: yes · writes files: yes · idempotent: yes · spends money: no

### `provider-registry`

MCP name: `seo_provider_registry`

List provider operations, credential components, quota and privacy boundaries.

Takes no arguments.

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `provider-readiness`

MCP name: `seo_provider_readiness`

Inspect configured credential sources and declared operation routes offline.

| Argument | Type | Default |
|---|---|---|
| `provider` | `str | None` | `None` |
| `operation` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Credential configuration never proves account or target permission. Use
seo_provider_verify for an explicit bounded read-only access check.

### `provider-verify`

MCP name: `seo_provider_verify`

Explicitly verify bounded read-only account/target access; present credentials are not verification.

| Argument | Type | Default |
|---|---|---|
| `provider` | `str` | `required` |
| `request` | `dict[str, Any] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `provider-collect`

MCP name: `seo_provider_collect`

Collect a declared provider operation with versioned redacted evidence.

| Argument | Type | Default |
|---|---|---|
| `provider` | `str` | `required` |
| `operation` | `str` | `required` |
| `request` | `dict[str, Any]` | `required` |
| `artifact_dir` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota

**Behavior and failure modes**

Raw identifiers and rows belong only in an explicit restricted artifact directory.
Paid operations require their provider's explicit production and cost guards; the
backlink-index adapter is disabled by default. Collection never implies indexing.

### `provider-join`

MCP name: `seo_provider_join`

Join supplied URL evidence exactly and preserve unmatched populations and technical severity.

| Argument | Type | Default |
|---|---|---|
| `crawl_pages` | `list[dict[str, Any]]` | `required` |
| `evidence_rows` | `list[dict[str, Any]]` | `required` |
| `review_external_only` | `bool` | `False` |
| `adjustments` | `list[dict[str, Any]] | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `evidence-normalize`

MCP name: `seo_evidence_normalize`

Normalize a supplied CSV/XLSX/JSON or saved provider envelope, fully offline.

| Argument | Type | Default |
|---|---|---|
| `file` | `str` | `required` |
| `mapping` | `Any` | `None` |
| `sheet` | `str | None` | `None` |
| `site_origin` | `str | None` | `None` |
| `out_dir` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Every row keeps its declared grain, provenance and availability state: a
measured zero stays zero while missing, null, blank, suppressed,
uncollected and failed inputs stay unavailable. Restricted sources
return counts and redacted provenance; normalized rows live only in an
explicit private ``out_dir`` artifact. No provider, DNS or page fetch
ever runs here.

### `evidence-join`

MCP name: `seo_evidence_join`

Join normalized analytics/search evidence to crawl pages, offline only.

| Argument | Type | Default |
|---|---|---|
| `evidence` | `Any` | `required` |
| `audit` | `Any` | `None` |
| `scan` | `str | None` | `None` |
| `pages` | `Any` | `None` |
| `compare` | `Any` | `None` |
| `mapping` | `Any` | `None` |
| `compare_mapping` | `Any` | `None` |
| `policy` | `Any` | `None` |
| `sheet` | `str | None` | `None` |
| `compare_sheet` | `str | None` | `None` |
| `site_origin` | `str | None` | `None` |
| `compare_site_origin` | `str | None` | `None` |
| `ignore_query` | `bool` | `False` |
| `ignore_scheme` | `bool` | `False` |
| `casefold_path` | `bool` | `False` |
| `out_dir` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Retains matched, crawl-only, external-only and unkeyable populations
with per-field provenance, and reports key collisions instead of
multiplying rows. ``compare`` plus a declared ``policy`` yields a pure
compatible/incompatible/unknown decision across period, timezone,
identity, attribution, engine and grain; source metrics such as GSC
clicks and GA4 sessions stay distinct and are never summed. Restricted
inputs return counts only.

### `bi-filter`

MCP name: `seo_bi_filter`

Filter a verified local BI package into a new typed package and optional XLSX.

| Argument | Type | Default |
|---|---|---|
| `package` | `str` | `required` |
| `dataset` | `str` | `required` |
| `out_dir` | `str` | `required` |
| `where` | `dict[str, list[str]] | None` | `None` |
| `columns` | `list[str] | None` | `None` |
| `max_rows_per_file` | `int` | `250000` |
| `max_bytes_per_file` | `int` | `8388608` |
| `max_output_bytes` | `int` | `4294967296` |
| `xlsx_out` | `str | None` | `None` |
| `xlsx_max_rows_per_sheet` | `int` | `1048575` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Exact equality filters over declared fields only; source and selected row counts
and coverage are preserved. Creates new local outputs without network calls.

### `scan-navigation`

MCP name: `seo_scan_navigation`

Read bounded observed navigation evidence from a retained local scan.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `document_id` | `int | None` | `None` |
| `limit` | `int` | `100` |
| `offset` | `int` | `0` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Reads saved local evidence only; does not collect, fetch or modify it.

### `project-activity`

MCP name: `seo_project_activity`

Read lightweight current activity for a local project and its sites.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Reads saved local evidence only; does not collect, fetch or modify it.

### `project-checklist-page`

MCP name: `seo_project_checklist_page`

Read a bounded searchable page of project checklist evidence.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `offset` | `int` | `0` |
| `limit` | `int` | `50` |
| `query` | `str` | `''` |
| `kind` | `str | None` | `None` |
| `state` | `str | None` | `None` |
| `sort` | `str` | `'id'` |
| `descending` | `bool` | `False` |
| `states` | `list[str] | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

sort is one of id (stored order, default), priority, state or updated; states
keeps up to 8 display states. Each item carries updated, the newest record time.
Reads saved local evidence with metadata-only receipt status; this is not
fresh byte verification. Missing historical receipts remain unverified.
Does not collect, fetch or modify evidence.

### `project-task-detail`

MCP name: `seo_project_task_detail`

Read one project task definition, evidence and bounded history.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `item_id` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Reads saved local evidence with metadata-only receipt status; this is not
fresh byte verification. Missing historical receipts remain unverified.
Does not collect, fetch or modify evidence.

### `project-scans`

MCP name: `seo_project_scans`

Read a bounded page of retained project scans with evidence metadata.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `offset` | `int` | `0` |
| `limit` | `int` | `20` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Reads saved local evidence only; does not collect, fetch or modify it.

### `bi-export`

MCP name: `seo_bi_export`

Project a saved scan or audit and optional issue #781 joins into a local typed BI package.

| Argument | Type | Default |
|---|---|---|
| `out_dir` | `str` | `required` |
| `scan` | `str | None` | `None` |
| `audit` | `Any` | `None` |
| `provider_joins` | `list[str] | None` | `None` |
| `max_rows_per_file` | `int` | `25000` |
| `max_bytes_per_file` | `int` | `8 * 1024 * 1024` |
| `max_output_bytes` | `int` | `4 * 1024 * 1024 * 1024` |
| `max_scan_bytes` | `int` | `8 * 1024 * 1024 * 1024` |
| `search_metric` | `str | None` | `None` |
| `xlsx_out` | `str | None` | `None` |
| `xlsx_dataset` | `str | None` | `None` |
| `xlsx_max_rows_per_sheet` | `int` | `1048575` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Reads existing evidence only. CSVs are partitioned deterministically;
null values retain explicit states and reasons, and output limits fail
without publishing a partial package. No crawl or provider request runs.

### `bi-sheets-plan`

MCP name: `seo_bi_sheets_plan`

Preflight a complete local BI package for Sheets without Google access or writes.

| Argument | Type | Default |
|---|---|---|
| `package` | `str` | `required` |
| `max_cells` | `int` | `10000000` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `publication-cohorts`

MCP name: `seo_publication_cohorts`

Project saved publication metadata and normalized provider evidence locally.

| Argument | Type | Default |
|---|---|---|
| `out_dir` | `str` | `required` |
| `document` | `dict[str, Any] | None` | `None` |
| `file` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Publication, modification and first-observed dates stay distinct. GSC
and analytics observations stay separately labelled; this makes no
causal traffic claim and never calls a provider.

### `gsc-progress`

MCP name: `seo_gsc_progress`

Project saved GSC query evidence into branded/non-branded summaries.

| Argument | Type | Default |
|---|---|---|
| `out_dir` | `str` | `required` |
| `document` | `dict[str, Any] | None` | `None` |
| `file` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Matching is supplied by versioned aliases. Missing query rows remain
unknown, and average-position values are never rank-placement claims.

### `bi-bigquery-plan`

MCP name: `seo_bi_bigquery_plan`

Describe an optional BigQuery load offline; it never selects a project or writes data.

| Argument | Type | Default |
|---|---|---|
| `package` | `str` | `required` |
| `dataset` | `str` | `required` |
| `operation` | `str` | `'replace'` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `bi-destination-apply`

MCP name: `seo_bi_destination_apply`

Request a BI destination write; host authorization is required and credentials are never accepted.

| Argument | Type | Default |
|---|---|---|
| `package` | `str` | `required` |
| `target` | `str` | `required` |
| `destination` | `str` | `required` |
| `operation` | `str` | `'replace'` |
| `apply` | `bool` | `False` |
| `reconcile` | `bool` | `False` |

**Cost** — network: yes · writes files: yes · idempotent: yes · spends money: no

### `inspect-url`

MCP name: `seo_inspect_url`

Inspect one URL with bounded metadata/header/robots/redirect/structured/render steps.

| Argument | Type | Default |
|---|---|---|
| `url` | `str` | `required` |
| `checks` | `list[str] | None` | `None` |

**Cost** — network: yes · writes files: no · idempotent: yes · spends money: no

### `audit-workflow`

MCP name: `seo_audit_workflow`

Use a closed project workflow: status, bounded start/prepare, or an evidence-backed report.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str` | `required` |
| `action` | `Literal['status', 'start', 'prepare', 'report']` | `'status'` |
| `target` | `str | None` | `None` |
| `competitors` | `list | None` | `None` |
| `template` | `dict | None` | `None` |
| `audit` | `Any` | `None` |
| `fmt` | `str` | `'md'` |
| `out` | `str | None` | `None` |
| `approve_large_crawl` | `bool` | `False` |
| `max_urls` | `int | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

A report without ``audit`` is built from the project's latest complete audited scan.

### `tool-catalog`

MCP name: `seo_tool_catalog`

Search complete source-derived tool metadata and load argument details only on request.

| Argument | Type | Default |
|---|---|---|
| `query` | `str` | `''` |
| `limit` | `int` | `10` |
| `include_arguments` | `bool` | `False` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `scan-evidence`

MCP name: `seo_scan_evidence`

Read captured evidence, resource windows or the event timeline without fetching or migration.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `section` | `Literal['capabilities', 'corpus', 'structured', 'routes', 'resources', 'timeline', 'relations', 'browser', 'extraction']` | `'capabilities'` |
| `limit` | `int` | `1000` |
| `offset` | `int` | `0` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `scan-content-search`

MCP name: `seo_scan_content_search`

Search one closed retained scan offline and create an indexed local NDJSON package.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `query` | `str` | `required` |
| `out_dir` | `str` | `required` |
| `scope` | `Literal['raw_html', 'head_markup', 'body_text', 'selector_markup']` | `'raw_html'` |
| `mode` | `Literal['contains', 'not_contains']` | `'contains'` |
| `representation` | `Literal['static', 'rendered']` | `'static'` |
| `selector` | `str | None` | `None` |
| `case_sensitive` | `bool` | `False` |
| `include_snippets` | `bool` | `False` |
| `kind` | `Literal['literal', 'regex']` | `'literal'` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

The tool never fetches or reconstructs a body. Static and rendered are
separate requests; a missing body remains unavailable rather than an
absence. It returns only source identity, counts and output paths.
``include_snippets`` is opt-in and redacts common credential-shaped values.
``kind="regex"`` treats ``query`` as a regular expression; a document
that exceeds the per-document regex budget is unavailable, not absent.

### `scan-content-search-page`

MCP name: `seo_scan_content_search_page`

Read up to 100 indexed derived content-search records without rereading the scan.

| Argument | Type | Default |
|---|---|---|
| `package` | `str` | `required` |
| `offset` | `int` | `0` |
| `limit` | `int` | `100` |
| `status` | `Literal['matched', 'not_matched', 'unavailable'] | None` | `None` |
| `status_code` | `int | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Optional status or status_code filters the stream; offset then counts matching records.

### `scan-extract`

MCP name: `seo_scan_extract`

Run bounded data-only extraction rules on retained complete bodies, without network or writes.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `rules` | `list[dict[str, Any]]` | `required` |
| `url` | `str | None` | `None` |
| `representation` | `str` | `'static'` |
| `limit` | `int` | `100` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `scan-structured-blocks`

MCP name: `seo_scan_structured_blocks`

Describe one retained page's JSON-LD blocks: line, state, graph findings and source JSON, offline.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `url` | `str` | `required` |
| `representation` | `str` | `'static'` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `marketing-inventory`

MCP name: `seo_marketing_inventory`

Inventory supplied CTA/form DOM occurrences without fetching or submitting forms.

| Argument | Type | Default |
|---|---|---|
| `documents` | `list[dict[str, Any]]` | `required` |
| `cta_selector` | `str | None` | `None` |
| `form_selector` | `str | None` | `None` |
| `id_attributes` | `list[str] | None` | `None` |
| `id_parameters` | `list[str] | None` | `None` |
| `out_dir` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

### `scan-fragment-links`

MCP name: `seo_scan_fragment_links`

Evaluate every retained fragment anchor offline and page the results.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `state` | `Literal['resolved', 'missing', 'skipped'] | None` | `None` |
| `representation` | `Literal['static', 'rendered', 'legacy_fragment'] | None` | `None` |
| `offset` | `int` | `0` |
| `limit` | `int` | `100` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Only complete retained HTML/DOM is measured: a missing, truncated,
unsupported, failed or budget-exhausted body is a named skipped
occurrence or unavailable source, never a broken fragment. Nothing is
fetched and the artifact is not modified.

### `scan-requeue`

MCP name: `seo_scan_requeue`

Explicitly requeue selected saved URLs in the same SQLite with verified backup and attempt history.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `where` | `str` | `required` |
| `backup_path` | `str` | `required` |
| `from_scan` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

**Behavior and failure modes**

where is a restricted validated predicate, never arbitrary SQL. This operation can perform
an explicit write-time v1 to v2 upgrade; readers never upgrade. It makes no network request.

### `scan-import-urls`

MCP name: `seo_scan_import_urls`

Explicitly import a TXT/CSV/XLSX/XML seed list through stored scope and query guards with backup.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `urls_file` | `str` | `required` |
| `backup_path` | `str` | `required` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

### `scan-list`

MCP name: `seo_scan_list`

List saved SQLite scan metadata without loading retained bodies.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str | None` | `None` |
| `offset` | `int` | `0` |
| `limit` | `int` | `100` |
| `project` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `scan-inspect`

MCP name: `seo_scan_inspect`

Read a bounded, paginated table view from one saved scan.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `table` | `str` | `'pages'` |
| `offset` | `int` | `0` |
| `limit` | `int` | `100` |
| `max_bytes` | `int` | `1048576` |
| `columns` | `list[str] | None` | `None` |
| `total` | `bool` | `False` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

``columns`` projects the named table columns; ``total`` adds the table's row count.

### `scan-url-detail`

MCP name: `seo_scan_url_detail`

Read one exact native URL's retained headers, redirects, page fields, and forms.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `url` | `str` | `required` |
| `response_offset` | `int` | `0` |
| `response_limit` | `int` | `10` |
| `form_offset` | `int` | `0` |
| `form_limit` | `int` | `20` |
| `max_bytes` | `int` | `1048576` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

The query is offline and read-only. Header and query values stay redacted,
body bytes remain unavailable here, and legacy/Screaming Frog sources
return named unavailable evidence instead of an empty detail record.

### `scan-url-history`

MCP name: `seo_scan_url_history`

State of one exact URL across the retained scans of a project, newest first.

| Argument | Type | Default |
|---|---|---|
| `project` | `str` | `required` |
| `url` | `str` | `required` |
| `limit` | `int` | `50` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Read-only. For each of the newest ``limit`` scans returns state (present, absent or
unavailable), status_code, indexability, title_hash, canonical, redirect_target and
content_hash; ``number`` counts all scans from 1 for the oldest. Lookup is by exact
retained URL text through an index, not a scan of the page table.

### `scan-url-query`

MCP name: `seo_scan_url_query`

Filter, sort and paginate the page table of one saved scan across the whole scan.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `filters` | `list[dict[str, Any]] | None` | `None` |
| `sort` | `str | None` | `None` |
| `direction` | `str` | `'asc'` |
| `columns` | `list[str] | None` | `None` |
| `offset` | `int` | `0` |
| `limit` | `int` | `200` |
| `count_timeout_seconds` | `float` | `1.0` |
| `max_bytes` | `int` | `1048576` |
| `preset` | `str | None` | `None` |
| `facets` | `list[str] | str | None` | `None` |
| `issue_check` | `str | list[str] | None` | `None` |
| `issue_severity` | `str | None` | `None` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Read-only. Filters are {column, op, value} objects combined with AND; preset names a
ready-made filter set (for example status_4xx, title_missing, title_over_60, noindex_meta)
that is combined with them. Sorting by a non-indexed column needs a filter that leaves at
most 100,000 rows (else reason_code sort_not_indexed). Returns total, filtered_total (null
with state capped when the count exceeds count_timeout_seconds) and at most 200 rows of the
requested columns. facets (a list of group ids, or "all") adds facets: {group: count} over
the same filters, with facets_state exact or capped. issue_check (1..50 check ids) and
issue_severity (critical|warning|notice) filter by audit issue; until the per-issue index
exists they return state unavailable with reason_code issue_index_missing.

### `scan-link-inspect`

MCP name: `seo_scan_link_inspect`

Inspect saved shortest paths, reverse inlinks, per-link DOM context, or one URL's links.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `view` | `str` | `'path'` |
| `seed` | `str | None` | `None` |
| `target` | `str | None` | `None` |
| `representation` | `str` | `'all'` |
| `cursor` | `str | None` | `None` |
| `link_id` | `int | None` | `None` |
| `document_id` | `int | None` | `None` |
| `offset` | `int` | `0` |
| `limit` | `int` | `100` |
| `max_bytes` | `int` | `1048576` |
| `max_body_bytes` | `int` | `5 * 1024 * 1024` |
| `max_nodes` | `int` | `10000` |
| `max_edges` | `int` | `200000` |
| `max_depth` | `int` | `20` |
| `timeout_seconds` | `float` | `15.0` |
| `url` | `str | None` | `None` |
| `direction` | `str` | `'out'` |
| `link_type` | `str` | `'all'` |
| `follow` | `str` | `'all'` |
| `status_class` | `str` | `'all'` |
| `contains` | `str | None` | `None` |
| `sort` | `str` | `'order'` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

view=links pages the outgoing (direction=out) or incoming (direction=in) hyperlinks of
one URL (`url`, not a document id): anchor, rel with nofollow/sponsored/ugc flags,
internal/external, position when classified at scan time, and the HTTP status of the
target (outgoing) or source (incoming); the status is null when that page was not
scanned. Filters: link_type, follow, status_class (2xx..5xx, broken, error, unscanned),
contains; sort (out only): order, url, status. At most 200 rows per page with offset;
`total` and `filtered_total` are exact or flagged capped/null. The scan is opened
read-only without full validation; failures carry a machine `reason_code`
(url_not_found, scan_not_available, url_not_scanned, links_not_retained).

Path hops and inlinks cite exact link IDs and scan identity; absence in a
partial graph is never a confirmed orphan. Inlinks use a cursor bound to
scan/revision/target/representation. Context requires link_id or
document_id, and missing retained bodies return unavailable evidence.
No network request or scan mutation occurs. Path defaults to 10,000
visited nodes, 200,000 examined edges, 20 hops and 15 seconds (hard
maxima 100,000/2,000,000/100/30). Inlinks page at most 500 rows; context
pages at most 500 rows and one document, with an 8 MiB body hard cap.
max_bytes bounds serialized item output (4 KiB..8 MiB); an over-budget
full response returns a named limit result. Invalid scans and URLs are
error results, not empty or clean graph evidence.

### `scan-status`

MCP name: `seo_scan_status`

Summarize frontier work and committed page outcomes from one saved scan offline.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `full_validation` | `bool` | `False` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

The scan is accepted by a light header/schema check (`validation: "light"`);
full_validation=true runs the complete artifact validation (`validation: "full"`).

### `scan-rendered-routes`

MCP name: `seo_scan_rendered_routes`

Read stored static/rendered route evidence without fetching routes.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `scan-snapshot`

MCP name: `seo_scan_snapshot`

Create a consistent new SQLite snapshot without overwriting a destination.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `out` | `str` | `required` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `scan-export`

MCP name: `seo_scan_export`

Export retained scan data under scan_export.v1 as CSV, XLSX, JSON, or XML.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `out` | `str` | `required` |
| `format` | `str` | `'json'` |
| `records` | `list[str] | None` | `None` |
| `fields` | `dict[str, list[str]] | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `scan-pin`

MCP name: `seo_scan_pin`

Pin or unpin a finished scan; this is an explicit metadata mutation.

| Argument | Type | Default |
|---|---|---|
| `input_path` | `str` | `required` |
| `pinned` | `bool` | `True` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

### `scan-prune`

MCP name: `seo_scan_prune`

Preview candidates by default; deletion requires apply plus the reviewed plan.

| Argument | Type | Default |
|---|---|---|
| `directory` | `str | None` | `None` |
| `older_than_days` | `int` | `30` |
| `keep_newest` | `int` | `5` |
| `plan` | `dict[str, Any] | None` | `None` |
| `apply` | `bool` | `False` |
| `project` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no · can overwrite/remove existing data

### `scan-body-diff`

MCP name: `seo_scan_body_diff`

Compare compatible retained bodies offline; a change is not an SEO verdict.

| Argument | Type | Default |
|---|---|---|
| `left` | `str` | `required` |
| `right` | `str` | `required` |
| `url` | `str` | `required` |
| `variant_key` | `str | None` | `None` |
| `representation` | `str` | `'static'` |
| `text` | `bool` | `False` |
| `max_bytes` | `int` | `5 * 1024 * 1024` |
| `max_lines` | `int` | `10000` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

---

## Crawl-audit tools (Screaming Frog)

### `sf_audit_run`

Run an SF audit and write audit.json plus audit.md.

| Argument | Type | Default |
|---|---|---|
| `mode` | `str` | `required` |
| `input` | `str` | `required` |
| `profile` | `str` | `'full'` |
| `out` | `str` | `'report'` |
| `config` | `str | None` | `None` |
| `sitemap` | `str | None` | `None` |
| `project` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Use ``parse-exports`` with a directory of existing CSV/XLSX exports;
this mode does not require Screaming Frog to be installed. The
``load-crawl``, ``crawl``, and ``crawl-list`` modes invoke the separately
installed, licensed SF CLI. ``input`` is respectively an exports
directory, .seospider path, start URL, or URL-list file. Returns a compact
summary and absolute file paths instead of embedding the large reports.

A live crawl can run for a long time; this call runs off the server's
event loop, so other sf_* calls stay answerable while it is in
progress, and cancelling the request (an MCP CancelledNotification)
stops the underlying Screaming Frog process instead of leaving it
running. A cancelled or failed run never leaves a completed-looking
``audit.json``/``audit.md`` behind; on either outcome, or a lost
response, recover by pointing sf_list_exports/sf_audit_summary at the
same ``out`` path.

### `sf_audit_summary`

Read a compact health summary from audit.json or a scan.v1 SQLite artifact.

| Argument | Type | Default |
|---|---|---|
| `json_path` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Returns the project, health score, severity counts, the first 15 ranked
checks, and sitemap statistics without loading the complete issue list
into the agent context.

### `sf_audit_issues`

Return filtered issues from audit.json or a validated scan.v1 SQLite artifact.

| Argument | Type | Default |
|---|---|---|
| `json_path` | `str` | `required` |
| `check` | `str | None` | `None` |
| `severity` | `str | None` | `None` |
| `limit` | `int` | `50` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

The requested limit is clamped to 1..1000 to keep MCP payloads bounded.
Omit both filters to inspect the first issues in report order.

### `sf_list_exports`

Discover recognized SF exports in a directory.

| Argument | Type | Default |
|---|---|---|
| `exports_dir` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

**Behavior and failure modes**

Returns the matched files by logical export name and the logical exports
that are absent, allowing an agent to explain which checks may be skipped
before running the audit.

### `sf_audit_tasks`

Build tasks.json and tasks.md from audit.json or a scan.v1 SQLite artifact.

| Argument | Type | Default |
|---|---|---|
| `json_path` | `str` | `required` |
| `out` | `str` | `'report'` |
| `config` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

**Behavior and failure modes**

Priority, severity inclusion, grouping, effort estimates, and URL caps
come from the configured ``tasks_pipeline``. ``check_assignment`` uses
only declared template/component URLs or validated audit segments and
preserves grouping provenance and candidate/declared/confirmed state.
Returns a compact summary and absolute paths to both backlog files.

---

## Semantic-core tools

### `seo_semantics_status`

Read a semantic core's phrase counters, provider units used and the next stage, without migrating or writing its database. project is the semantic project directory, or a SEOHEAD project workspace whose semantics/ subdirectory holds the core.

| Argument | Type | Default |
|---|---|---|
| `project` | `str` | `required` |

**Cost** — network: no · writes files: no · idempotent: yes · spends money: no

### `seo_semantics_run`

Run one free, offline semantic-core stage: init, import (file = CSV with a norm/phrase/query column), clean, graph, competitors, report, excel, sitematch, relevance or export. Phrases and evidence are retained; statuses change reversibly. No network access; paid stages are refused here (use seo_semantics_paid).

| Argument | Type | Default |
|---|---|---|
| `stage` | `str` | `required` |
| `project` | `str` | `required` |
| `out` | `str | None` | `None` |
| `file` | `str | None` | `None` |

**Cost** — network: no · writes files: yes · idempotent: no · spends money: no

### `seo_semantics_mine`

Fetch the most frequent competitor pages from the core's cached SERP and append title/heading 2-3-grams to synonyms.txt as candidate phrases. Reads public pages; spends no provider quota. Run seo_semantics_paid stage=synonyms afterwards to check them.

| Argument | Type | Default |
|---|---|---|
| `project` | `str` | `required` |
| `out` | `str | None` | `None` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: no

### `seo_semantics_paid`

Run one paid semantic-core stage: collect (Wordstat expansion of seeds.txt), synonyms (Wordstat check of synonyms.txt), cluster (Yandex Search SERP, cached per phrase; method=louvain needs the semantics extra) or exact (Arsenkin !W, once, at the end). Paid: without confirm_paid=true nothing is sent. Every submission is reserved before the call, so an interrupted or ambiguous request is never paid for twice; charges land in the shared spend journal (seo_spend_report).

| Argument | Type | Default |
|---|---|---|
| `stage` | `str` | `required` |
| `project` | `str` | `required` |
| `confirm_paid` | `bool` | `False` |
| `resume` | `bool` | `False` |
| `max_seeds` | `int` | `0` |
| `limit` | `int` | `0` |
| `method` | `str` | `'serp'` |
| `yes` | `bool` | `False` |

**Cost** — network: yes · writes files: yes · idempotent: no · spends money: yes, external provider quota
