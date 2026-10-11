# DataForSEO confirmation and cost evidence

Every public Google production operation checks `confirm_paid=true` before creating a client or
reading credentials. Sandbox remains the default and uses synthetic data. Backlink-index operations
are disabled by default and additionally require account eligibility and a local cache key;
production needs explicit production/cost approval. A cache key is provenance, not proof of a hit.

`dataforseo-estimate` reads only supplied pricing. Obtain account-specific base prices from the
free [appendix/user_data endpoint](https://docs.dataforseo.com/v3/appendix-user-data/); the existing
`DataForSEOClient.balance()` read now returns its `prices` object. `dataforseo-estimate --refresh-prices` explicitly reads this free endpoint. No price lookup happens
implicitly before paid transport. The account endpoint excludes extra paid parameters; these
bounded adapters do not send those parameters. Stale snapshots can differ from actual billing.

```sh
seohead dataforseo-estimate --input '{"operation":"backlinks","env":"prod","row_limit":100,"query_count":1,"cache_hits":0,"prices":{"backlinks":{"backlinks":{"live":{"priority_normal":[{"cost_type":"per_request","cost":0.02},{"cost_type":"per_result","cost":0.00003}]}}}}}'
```

The example prices are synthetic fixture values, not current provider tariffs. `cache_hits` is a
caller-declared input; the quote does not inspect or invent cached responses. Unknown pricing is
`state=unavailable`, `cost_usd=null`. A declared per-call `spend_ceiling_usd` refuses unavailable or
excess estimates before the request. Actual higher cost returns partial and remains in the journal;
a ceiling cannot retroactively refund a provider charge.

Each response is journaled before result normalization. Missing/invalid cost stays an uncertain
receipt excluded from measured totals. Network loss is journaled and is never retried automatically.
A malformed response also has an uncertain receipt. The journal keeps native USD costs, never an
assumed conversion to RUB. Project and monthly currency policy is still an explicit owner decision
under #949/#1310; this release does not claim those limits are enforced.

Backlink history sends an explicit date range (last 30 days by default), counts the possible monthly
rows against the declared limit, and never fetches another page automatically. The adapters expose
`backlinks_summary`, `backlinks`, `referring_domains`, `backlinks_history` and
`domain_pages_summary`; [Backlinks API documentation](https://docs.dataforseo.com/v3/backlinks-backlinks-live/)
describes the indexed population, which is not a claim of complete coverage of the web.
