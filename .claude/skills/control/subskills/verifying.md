# Verifying a finding before it reaches anybody

**A finding you cannot reproduce yourself is not a finding.** Every critical that goes into a
deliverable gets checked against the live URL first.

## How

Three seconds apart, with a browser user agent, recording what came back:

```bash
curl -s -o /dev/null -w '%{http_code} -> %{redirect_url}\n' -A 'Mozilla/5.0' https://example.com/page
```

The browser user agent is a diagnostic comparison — "is this answer specific to our crawler" —
not a way to keep crawling a site that just blocked it.

A 403 or 429 with WAF markers (`cf-mitigated`, `server: cloudflare`, `Retry-After`, a challenge
body) is a block, not a finding about the site, and the toolkit does not bypass it: no UA
spoofing, no proxies, no challenge solving. Respect `Retry-After`, lower the rate, ask the site's
owner for an allowlist, or verify from a supplied export or Search Console data.

Rate-limit yourself here too. Verification that hammers a host you have just finished crawling
is the same mistake twice.

## Both outcomes are worth the time

On one afternoon, the same step:

- **confirmed** an entire `/uslugi/fundament/` section — twelve pages — really returning 404 on
  a construction company's site, while `/uslugi/` answered 200. That went in the report as a
  critical, with the codes attached.
- **refuted** 78 `CANONICAL_TO_REDIRECT` findings on a blog: every canonical checked answered
  200. That became issue #95 instead of a report section.

Same command, opposite conclusions. Neither was knowable from the audit alone.

## When the tool is wrong

File an issue with the real page attached as a fixture. Never a local patch, never a throwaway
script that routes around the defect — a workaround makes the next run wrong in the same way
and nobody remembers why.

An issue is the specification: the symptom, the cause with a file and line, what it corrupts
downstream, what is requested, and acceptance criteria. Then one commit, one pull request, one
issue closed.

## What to verify, in order

1. Every `critical`.
2. Five hits of any check that dominates `by_check`.
3. Anything that would embarrass you if it were wrong in front of the site's owner.

Notices are not worth individual verification, but a notice that fired on 74% of the report is
not a notice, it is a bug.
