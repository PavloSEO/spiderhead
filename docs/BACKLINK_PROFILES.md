# Backlink profiles and local history

`backlinks-profile` merges explicitly supplied exports and bounded provider evidence. It does
not crawl donors, query a provider implicitly, or describe a sample as the entire web. Use the
existing `backlinks-check` separately when a live donor check is authorized.

## Offline imports

Supply a JSON object with handler arguments through `--input` (inline JSON) or standard input. Paths below
are synthetic examples; put real private exports and snapshots outside the repository.

```json
{
  "domain": "example.test",
  "imports": [
    {"source": "gsc", "path": "/data/links/gsc.csv"},
    {"source": "ahrefs", "path": "/data/links/ahrefs.csv"},
    {"source": "semrush", "path": "/data/links/semrush.csv"},
    {
      "source": "generic",
      "path": "/data/links/custom.tsv",
      "columns": {
        "source_url": "Donor URL",
        "target_url": "Destination URL",
        "anchor": "Link text",
        "rel": "Attributes"
      }
    }
  ],
  "db": "/data/links/backlinks.sqlite",
  "follow": "sponsored",
  "one_per_domain": true,
  "limit": 100
}
```

A standalone offline example uses an explicitly empty synthetic source:

```bash
seohead backlinks-profile --input '{"domain":"example.test","evidence":[{"source":"generic","rows":[],"complete":true}]}'
```

A retained synthetic history can be prepared and read as follows. For real work, replace the
empty evidence with your explicit imports or source rows and use a private database path.

```bash
echo '{"domain":"example.test","db":"./backlinks.sqlite","evidence":[{"source":"generic","rows":[],"complete":true}]}' | seohead backlinks-profile
seohead backlinks-profile --domain example.test --db ./backlinks.sqlite --history
```

Supported source names: `generic`, `gsc`, `ahrefs`, `majestic`, `moz`, `semrush`, `dataforseo`,
`yandex_webmaster`, `bing_webmaster`, `common_crawl`. CSV readers accept UTF-8 (including BOM),
UTF-16 BOM, comma, semicolon and tab delimiters, and quoted multiline anchors. Each import is
bounded to 64 MiB and 100,000 rows; the merged profile allows 100,000 observations.

Common export aliases include Ahrefs `Referring page URL`/`Target URL`, Majestic
`SourceURL`/`TargetURL`/`FlagNoFollow`/`FlagDeleted`/`FirstIndexedDate`/`LastSeenDate`/`DateLost`,
Semrush `Source URL`/`Target URL`, and GSC `Linking page`/`Target page`. Use `columns` for localized
or different provider headers. The map direction is normalized field to actual header.

GSC linking-page exports without targets retain `target_url: null` and partial coverage.
GSC `Site`/`External links` reports contribute domain evidence; `Target page` aggregates are
retained in `aggregate_rows`. Neither export invents URL pairs or root-page links. A header-only
CSV measures an empty export. A failed provider, malformed rows only, or no inputs yields
`ok: false`, `state: unavailable`, `summary: null` and no snapshot write.

Partial boolean evidence remains partial: `nofollow=false` alone does not establish normal
qualification, and `follow=false` alone never creates a nofollow token. Observations retain
`observed_follow`, `rel_complete` and `qualification_complete`; merged rows expose unknown
attribute observations and conflicting follow states for review.

## Provider evidence and explicit requests

MCP `seo_backlinks_profile` accepts the same core arguments plus `sources`. `evidence` accepts
normalized row lists or provider results without initiating a request:

```json
{
  "domain": "example.test",
  "evidence": [{
    "source": "generic",
    "complete": false,
    "scope": "supplied partner export",
    "rows": [{
      "source_url": "https://donor.test/article",
      "target_url": "https://example.test/page",
      "anchor": "Example",
      "rel": ["ugc", "nofollow", "external"],
      "first_seen": "2026-01-01",
      "last_seen": "2026-10-01",
      "state": "live"
    }]
  }]
}
```

`sources` entries select one source and a request object. Adding a source explicitly authorizes
collection, but paid sources still require their individual paid-call gate. Credentials are
loaded from existing local configuration/environment; never put their values in request files.

```json
{
  "domain": "example.test",
  "sources": [
    {"source": "yandex_webmaster", "request": {
      "operation": "external_links_samples",
      "host_id": "https:example.test:443", "paginate": true, "max_rows": 100
    }},
    {"source": "bing_webmaster", "request": {
      "site_url": "https://example.test/", "link": "https://example.test/page", "page": 0
    }},
    {"source": "ahrefs", "request": {"limit": 100}},
    {"source": "moz", "request": {"operation": "backlinks", "confirm_paid": true}},
    {"source": "majestic", "request": {"limit": 100, "offset": 0}}
  ]
}
```

The last two entries return cost/usage preflight without transport access until
`confirm_paid: true` is supplied. DataForSEO uses its separate gates and price model; see
[DATAFORSEO_SPEND.md](DATAFORSEO_SPEND.md). Default Ahrefs units are `max(50, 11 * limit)` for
the eleven explicitly selected one-unit fields. No 5-unit subnet/refdomain or 10-unit traffic
fields are selected. Majestic shows 5,000 analysis units and up to `limit` retrieval units,
using the fresh index without extra sorting/filtering. `max_units` checks every estimated
currency independently. These are usage estimates, not dollar charges or guarantees about an
account's plan. Ahrefs actual response headers are recorded before parsing. Unmeasured charges
remain uncertain receipts in the shared spend journal, including malformed responses.

Moz keeps its legacy v2 per-URL metrics as the default. To explicitly request backlinks, set
`operation` to `backlinks`; this uses the current JSON-RPC `data.site.link.list` method with
`MOZ_API_TOKEN` (or `api_key` in the request), an external-link filter and a page size of 25 by
default, capped at 50. The request returns one page and its opaque `next_token`; it never follows
that token automatically. Set `page_token` explicitly to request a later page. Response size and
row count are bounded, JSON-RPC IDs are checked, and returned link fields (including rel, canonical
and redirect flags) are retained without relabeling unknown values. Price is account-dependent
and unavailable here, so receipts record `cost: null`; supplying `max_units` refuses this unpriced
request. Both Moz modes still require `confirm_paid: true` before transport.

The free Yandex operation returns samples with documented 100-row pages and a visible cap;
`external_links_history` retains aggregate history separately in coverage. Bing `url_links`
reads one `GetUrlLinks` page and reports `next_page`/`total_pages`; `links` is `GetLinkCounts`
aggregate evidence, not a donor URL list. Verified-property access requirements still apply.

## Common Crawl offline extracts

Domain graph evidence accepts a bounded, extracted CSV with `source_domain,target_domain`
columns and `format: domain_edges`. Convert native graph vertex IDs and reverse-domain vertex
labels with the Common Crawl graph tooling first; this importer does not download or decode a
multi-gigabyte BVGraph. It preserves domain granularity and never assigns guessed page URLs.

```json
{"domain":"example.test","imports":[
  {"source":"common_crawl","path":"/data/links/graph-edges.csv","format":"domain_edges"},
  {"source":"common_crawl","path":"/data/links/wat-extract.jsonl","format":"wat"}
]}
```

`format: wat` reads supplied JSONL WAT records, taking donor URLs from
`Envelope.WARC-Header-Metadata.WARC-Target-URI` and `A@/href` links from HTML metadata.
Non-anchor resource references are excluded. WAT does not provide a full rel set, so missing
attributes remain unknown. Coverage describes the supplied extract.

## Merge, filters and history

Pairs deduplicate by normalized `(source_url,target_url)`: host case, IDNA, default port and
fragments normalize, while schemes, queries and trailing-path differences remain distinct.
All observations keep source, dates, anchor, rel, reported metrics and collection time. Merged
rows retain anchor sets and the full union of rel tokens. Conflicting provider rel/state
observations are flagged; an unknown rel does not become a normal link. Filters accept
`normal`, `nofollow`, `sponsored`, `ugc`, `unknown`; combinations can match several categories.

Other local filters: `state` (`live`/`lost`/`unknown`), `target_prefix`, `min_spam_score`,
`max_spam_score`, `first_seen_from`, `first_seen_to`, `one_per_domain`, `offset`, `limit`.
Unmeasured spam scores are excluded from numeric filters. Transparent review heuristics use
provider spam score, very many outbound links, and potentially spam-related anchors; the output
states `penalty_evidence: false`. No score alone triggers disavow. Optional `target_status`
maps exact normalized target URLs to statuses from an existing scan; it does not fetch targets.

An explicit `db` writes isolated `seohead.backlink-history.v1` snapshots. An explicit `project`
with `project.json` uses `<project>/backlinks.sqlite`. The module never writes to `sources.sqlite`,
changes a project workflow, or alters frozen scan schemas. The snapshot stores all unfiltered
rows and coverage, so display filters do not destroy evidence. Identical timestamp/payload
retries deduplicate. History reports `newly_observed`, `confirmed_lost`, and
`unobserved_since_previous`; a later missing sample never proves loss. Python callers can use
`backlink_history.snapshot_history` and `read_snapshot` to inspect retained evidence.

## Reviewed disavow export

```bash
seohead backlinks-disavow --domains donor.test,another.test \
  --urls https://page-donor.test/article
```

MCP `seo_backlinks_disavow` accepts explicit `domains`, `urls`, and optional `output`. The result
includes text and counts. Pass `--output /data/links/disavow.txt` only after creating its parent
directory. File publication is exclusive, preserves existing lists, and never submits to a
search engine. The selection must be reviewed separately from spam heuristics.

Primary contracts: [Yandex samples](https://yandex.com/dev/webmaster/doc/en/reference/host-links-external-samples),
[Bing GetUrlLinks](https://learn.microsoft.com/en-us/dotnet/api/microsoft.bing.webmaster.api.interfaces.iwebmasterapi.geturllinks?view=bing-webmaster-dotnet),
[Ahrefs backlinks](https://docs.ahrefs.com/en/api/reference/site-explorer/get-all-backlinks),
[Ahrefs units](https://docs.ahrefs.com/en/api/docs/limits-consumption),
[Majestic backlinks](https://developer-support.majestic.com/api/commands/get-back-link-data.shtml),
[Moz legacy URL metrics](https://moz.com/help/links-api/making-calls/url-metrics),
[Moz backlink listing](https://moz.com/api/docs/methods/DataSiteLinkListAction),
[Common Crawl graph exploration](https://github.com/commoncrawl/cc-webgraph/blob/main/graph-exploration-README.md).
