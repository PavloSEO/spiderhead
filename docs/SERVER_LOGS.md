# Server access logs

`log-analyze` reads a supplied Apache/Nginx Common/Combined or IIS W3C file, including
gzip detected by content. It keeps exact distinct bot-IP counts in a temporary disk
index and returns a bounded page of per-IP records. Path summaries keep their existing
explicit caps. Limits describe the parsed prefix, not the entire site.

Retain supplied logs in an existing project:

```bash
seohead logs import --path ./synthetic-logs --project ./shop --max-lines 10000000
seohead logs summary --source ./shop/logs/IMPORT.sqlite --view daily
seohead logs bots --source ./shop/logs/IMPORT.sqlite --family googlebot --limit 100
seohead logs join --source ./shop/logs/IMPORT.sqlite --scan ./shop/scans/audit.sqlite \
  --view scan_without_bot_visits --family googlebot --date-from 2026-10-01 --date-to 2026-10-10
```

The equivalent flat commands are `logs-import`, `logs-summary`, `logs-bots` and
`logs-join`; local MCP exposes `seo_logs_import`, `seo_logs_summary`, `seo_logs_bots`
and `seo_logs_join`. The import result returns the actual fresh artifact path.
Imports never overwrite an existing artifact, rewrite `project.json`, change a scan,
or perform DNS/site/provider requests. Files and directories have explicit input
budgets; invalid/corrupt input fails without publishing partial success. The source
identity includes file metadata and a SHA-256 digest of consumed decoded text, with
its digest scope stated separately from compressed-file bytes. Each import is a
separate observation; importing overlapping logs twice does not deduplicate visits
across those artifacts.

The separate `logs.v1` SQLite artifact stores exact bot/IP and bot/IP/URL/UTC-date/status
aggregates, source provenance and a DNS cache. It does not retain original lines,
user-agent strings or referrers. Raw IPs and requested paths can still identify people;
keep these artifacts local and never commit real access logs or put IP tables into
public/client reports. Files are created with private permissions where supported.
Missing Apache timezone offsets and IIS timestamps explicitly assume UTC; known
offsets are normalized to UTC. Unknown timestamps and user agents remain coverage gaps.
The importer supports the built-in formats; arbitrary custom field layouts require
an explicit future format contract.

`logs-summary` views are `urls`, `bot_ips`, `bots`, `daily`, `statuses` and `sections`.
Pages contain exact matched totals, `has_more` and `next_offset`; limit is 1..500.
Filters include bot, family, UTC dates, HTTP status, section and IP/subnet. Bot names
are user-agent claims. A missing/partial log observation does not prove zero visits.

`logs-join` supports `scan_without_bot_visits`, `bot_urls_absent_scan`, `status_waste`,
`nonindexable` and `sections`. Relative requests match exact path plus query on the
scan origin (or an explicit compatible `base_url`), which must match the retained
import project target. A missing log-project origin is unavailable; it is never
guessed from matching relative paths. Query variants stay separate;
there is no guessed canonical, host or URL correspondence. URLs absent from a scan
are candidates for inspection, not proven orphans. Absence views require complete
import/bot-detection evidence and a completed compatible scan. Missing native scan,
unknown dates, incomplete capture and missing indexability produce named unavailable
states. Status waste describes observed 3xx/4xx/5xx requests; the `nonindexable` view uses
recorded response status and explicit noindex/none directives. Its basis is stated
separately from complete indexing eligibility: canonicals and robots.txt access are
not inferred. Unknown status remains an explicit unknown count. There is no automatic crawl, inference of traffic loss,
or provider fallback.

DNS verification remains explicit:

```bash
seohead logs bots --source ./shop/logs/IMPORT.sqlite --family googlebot \
  --limit 20 --max-dns 20 --verify-bots
seohead logs bots --source ./shop/logs/IMPORT.sqlite --verdict fake
```

Every IP/bot pair has its own PTR hostname, forward addresses/match, verified/fake/
unchecked verdict, visits and last visit. No sample IP represents its whole family.
An official suffix and matching forward IPv4/IPv6 address are both required.
Bots with no supported official PTR suffix remain unchecked. A canary distinguishes
unavailable reverse DNS from an absent bot record; lookup failures and hard deadlines
remain unchecked. Verification checks at most `max_dns` uncached records (0..100), plus
one canary, on the selected page. `dns_checks` counts per-IP attempts, each of which
can issue a PTR and a forward query. Attempts are sequential, separated by 50 milliseconds,
and each resolver worker has a three-second caller deadline. A system DNS call may
outlive that deadline in a daemon worker; it does not block return or shutdown.
At most four resolver workers may exist across calls; exhausted capacity returns
unchecked without launching another worker. No process-wide socket timeout is changed.

Retained and bounded process caches expire after one hour. Ordinary reads never
refresh them, and stale entries read as unchecked. Cached verdict filters select
`verified`, `fake` or `unchecked` without DNS; combining a verdict filter with
verification is refused to prevent changing the selection during evaluation. The
DNS cache writes only to the supplied log artifact after explicit verification.
The Desktop Logs/LogsCompare/LogsBots wiring is a separate presentation follow-up.
