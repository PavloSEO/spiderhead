# Choosing a crawl rate, and telling whose fault the errors are

Decide this before the first request. A rate chosen after a host starts refusing connections is
a rate chosen by the host.

## The numbers

| Situation | Rate | Why |
|---|---|---|
| Your own site, known capacity | 10–20 URL/s | nothing to protect but yourself |
| Somebody else's site, no information | **3 URL/s** | survives shared hosting; finishes a 3000-page site in twenty minutes |
| A host that has already shown strain | 1 URL/s or stop | see below |

Set it in the config, not in flags:

```json
{"speed": {"min_delay_seconds": 0.333, "concurrency": 3, "adaptive": true}}
```

`effective_request_rate()` in `seohead.crawl.settings` prints the worst case before a request is
spent. `crawl-site --config-help` lists every setting and marks the ones that change **what is
found**, as opposed to only what it costs.

## The lesson that cost a crawl

A WordPress blog crawled at 10 URL/s started returning 502 after 249 pages, and the circuit
breaker stopped the run (`finish_reason: errors`). The same six URLs, re-fetched one every three
seconds with a browser user agent, all answered 301 normally.

**The 502s were caused by the crawl.** At 3 URL/s the same site completed all 3387 URLs.

So: **the breaker stopping is the tool working. Treat it as a finding about the rate, not about
the site.** Reporting "this host returns 502 under load" as a site defect, when the load was
yours, is the most embarrassing thing an audit can do.

## Why latency, not status, is the signal

Measured on a shared-hosting catalogue: under a polite 1.5 URL/s the origin degraded from
1196 ms to 16455 ms TTFB and then began refusing TLS handshakes — **without ever returning an
error status**. A throttle that only widens on non-200 would have kept pushing all the way
down.

That is why `speed.adaptive` widens the delay on latency and widens it hard on a timeout, and
why concurrency collapses to one on the first timeout or server refusal. Turning `adaptive` off
freezes the delay where you set it; the timeout and server-error counters keep running, because
giving up on a failing origin is a separate mechanism from backing off.

## Before blaming the site

First decide whether the errors are rate damage or a deliberate block.

A 403 or 429 carrying WAF markers — `cf-mitigated`, `server: cloudflare`, a `Retry-After`
header, a challenge page in the body — is a block, not a finding about the site. The toolkit
does not bypass it: no user-agent spoofing, no proxies, no challenge solving. Respect
`Retry-After`, lower the rate, ask the site's owner for an allowlist, or work from a supplied
export or Search Console data.

For 5xx and timeouts under our own load, re-fetch five of the failing URLs, three seconds
apart, with a browser user agent. That re-fetch is a diagnostic comparison — "does this URL
answer a different client once we are not hammering it" — not a way to continue crawling: if
they answer normally, the failures were yours. Lower the rate and re-crawl. If they still fail,
it is a finding — and now you can say so with evidence.
