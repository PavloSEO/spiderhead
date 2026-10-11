# Guideline: operating this toolkit

This is the document to read first. It is written for a person meeting the tool, in the order a
person meets it — not a reference, and not a recipe for a question you already know how to ask.

If you know what you want, the other three layers are faster:

| You want | Read |
|---|---|
| a chain, end to end | [scenarios/](scenarios/README.md) |
| one command's arguments | [TOOL_REFERENCE.md](TOOL_REFERENCE.md) |
| what a check means | [CHECKS.md](CHECKS.md) |
| what an agent should run | [.claude/skills/control/SKILL.md](../.claude/skills/control/SKILL.md) |
| a crawl that stopped early | [RECOVERY.md](RECOVERY.md) |

---

## 1. What this is, and what it is not

**It is a crawler and an analysis layer.** It fetches a site the way a search engine's crawler
would — one host, at a rate you choose, honouring robots.txt by default — records what came
back, and applies 208 checks to that evidence. Then a set of specialised tools answer questions
the crawl raised: does JavaScript change what a crawler sees, is the structured data a connected
graph, how heavy are the images really.

**It is not a rank tracker.** Nothing here knows what any page ranks for.

**It is not Search Console.** Nothing here knows whether a page is indexed, how often it is
shown, or what Google chose as its canonical. Reachability is not indexation, and the two are
confused constantly. If a page is reachable, indexable and in the sitemap, this tool will tell
you that and nothing more.

**It is not a judge of quality.** Every check is structural. "This page has 800 words in its
content region" is a measurement; "this page is worth reading" is not, and no output here should
be read as claiming it.

**It has no web interface.** Its surfaces are a CLI and a local MCP server, both over the same handlers, and the optional `seohead watch` terminal observer. There is no web panel and there will not be one.

The point of the whole thing is narrow and worth stating plainly: checking every page of a site
by hand gives the best analysis and costs the most; a script is cheap and usually stupid. This
repository exists to close that gap — to encode what a careful person would check by hand into
configs and scripts, test them against real sites, and get that quality at any number of pages.

---

## 2. Your first run

Install into a virtual environment and confirm it answers:

```bash
seohead --version
```

Then parse one page. Not a crawl — one page, so you can see the shape of everything before
committing to a site:

```bash
seohead parse --url https://example.com
```

That prints a JSON document: title, description, headings, canonical, robots directives,
Open Graph, JSON-LD, links, word count, and `content_area_strategy` — which region of the page
the word count came from. Read that field. It is the difference between counting a page and
counting a page plus its navigation.

Now crawl, into a directory:

```bash
seohead crawl-site --url https://example.com --out-dir ./run --max-urls 200
```

Two files land in `./run`:

- **`pages.jsonl`** — one line per fetched URL: status, content type, size in bytes, title,
  canonical, word count, how it was measured.
- **`audit.json`** — the run manifest, the summary, and the findings.

Everything else in this toolkit reads one of those two.

A third file, `crawl_state.json`, lands in the same directory as a checkpoint — invisible in a
run that finishes, and the whole story when one does not. If the process is interrupted partway
through, rerunning the identical command resumes from it instead of starting over. A
`--scan-out` run checkpoints inside the scan artifact instead, and resumes with
`crawl-site --resume <scan>`. See [RECOVERY.md](RECOVERY.md) for both routes, the exact
requirement of each, and how to tell a resume from an intentional fresh start.

---

## 3. Choosing a rate, and whose fault the errors are

Decide this **before** the first request. A rate chosen after a host starts refusing is a rate
chosen by the host.

| Situation | Rate |
|---|---|
| Your own site, known capacity | 10–20 URL/s |
| Somebody else's site, no information | **3 URL/s** |
| A host that has already shown strain | 1 URL/s, or stop |

Set it in a config file rather than flags, because the config is the record of what was
measured:

```json
{"limits": {"max_urls": 5000, "max_depth": 20, "max_crawl_seconds": 5400},
 "speed":  {"min_delay_seconds": 0.333, "concurrency": 3, "adaptive": true},
 "robots": {"policy": "respect"}, "sitemaps": {"auto_discover": true}}
```

```bash
seohead crawl-site --url https://example.com --config ./crawl.json --out-dir ./run
```

### The lesson worth reading twice

A WordPress blog crawled at 10 URL/s started returning 502 after 249 pages, and the circuit
breaker stopped the run. The same six URLs, re-fetched one every three seconds with a browser
user agent, all answered 301 normally. **The 502s were caused by the crawl.** At 3 URL/s the
same site completed all 3387 URLs.

Two things follow. First, the breaker stopping is the tool working — treat it as a finding about
your rate, not about the site. Second, and worse: reporting "this host returns 502 under load"
as a site defect, when the load was yours, is the most embarrassing mistake an audit can make.

Before blaming a site for errors, re-fetch five of the failing URLs slowly. If they answer, the
failures were yours.

### Why latency, not status, is the signal

Measured on a shared-hosting catalogue: under a polite 1.5 URL/s the origin degraded from
1196 ms to 16455 ms and then began refusing TLS handshakes — **without ever returning an error
status.** A throttle that only widens on non-200 responses would have kept pushing all the way
down. That is why the delay here widens on latency and hard on a timeout, and why concurrency
collapses to one at the first refusal.

---

## 4. Reading `audit.json` without being misled

The most expensive mistake is reading the findings first. A list of problems from a run that
covered a third of the site, with a third of the checks, looks exactly like a list from a
complete one.

**Read these four fields, in this order:**

| Field | What it tells you |
|---|---|
| `run.crawl_finish_reason` | `finished`, or why not — `errors`, `budget`, `duration` |
| `run.crawl_partial` | whether the crawl covered the site |
| `summary.check_coverage` | how many checks *could* run |
| `summary.health_score_basis` | whether the score compares to anything |

Four words mean four different things and are constantly confused:

- **fired** — the check ran and found something.
- **skipped** — the check *could not run*, and the reason is named: a missing export column, a
  page property nobody recorded. This is not "zero issues".
- **disabled** — the operator turned the check off in config (`checks.<ID>.enabled: false`).
  Named separately from `skipped` so a deliberate switch is never read as missing evidence, and
  named at all so it is never read as `silent`.
- **silent** — the check *was invoked* and found nothing. This is the good one. `checks_silent_ids`
  names the population; a check no code path ever calls is a defect, not a silent one.

A health score computed from 16 of 208 checks is not a health score. The audit says so in
`health_score_basis`, and where coverage is too low the score is withheld rather than averaged
out of whatever happened to be available. **Report that sentence next to the score, always.**

**Then look at the shape before the list:**

```python
sorted(summary["by_check"].items(), key=lambda kv: -kv[1])[:10]
```

A check that fired on more than half the pages is almost always wrong — the tool exists to find
the *unusual*. On one live 124-page site, one check produced 392 of 529 findings: 74% of the
report, and every one false. That was visible in one line, before reading a single URL.

The report now says it for you. Any check covering more than half the crawled pages is listed
under **"Look at these before trusting the rest"**, above the findings rather than in an
appendix, and the same list is in `audit.json` as `summary.implausible_checks`. It is not a
failure: a site really may have no meta description anywhere. It is the one minute of checking
that would have caught all three of the defects live crawls found (#94, #95, #96).

**And scan the run:**

```bash
seohead log-scan --run ./run
```

Nine rules, each written from a defect that shipped past the whole test suite. Exit 2 means
two numbers in the same run disagree with each other. Beside them, under `review`, sit the
checks that describe most of the site: not a contradiction, since a uniform site makes them
true, so they never change the exit code -- only ask for a minute of your attention. Twenty seconds here is cheaper than a
client asking why a 739 KB file is listed as 1.27 MB.

---

## 5. The config file: what changes findings, what changes only cost

```bash
seohead crawl-site --config-help
```

That prints every setting: dotted path, type, default, description, and whether it is
**results-affecting**. The distinction is the whole reason the file exists.

- **Results-affecting** settings change *what is found*: scope, robots policy, rendering mode,
  content-area selectors, link-position classification. Every one of them is written into the
  run manifest inside `audit.json`, because two crawls that disagree are only comparable if you
  can see which settings produced each.
- **Cost settings** change only how long it takes and how hard the host is pushed: delay,
  concurrency, timeouts, budgets.

Change a results-affecting setting between two runs and the difference you measure is partly
your own configuration. That is the single most common way a "before and after" becomes
meaningless.

---

## 6. The mistakes everybody makes first

**Trusting a partial crawl.** `crawl_partial: true` means absence proves nothing. "Nothing links
to this page" is unprovable on a crawl that stopped early, which is why orphan findings are
withheld — named as skipped — rather than guessed at.

**Comparing two runs with different settings.** See section 5. Compare like with like, or you
are measuring your own edit.

**Reading a count without its population.** Every check describes a specific set. "Missing from
the sitemap" is about *indexable pages*; comparing it against every link destination reports
images, outbound links and URLs that were never fetched. That mistake produced the 392 findings
above. The sets are written down in
[.claude/skills/control/reference/populations.md](../.claude/skills/control/reference/populations.md).

**Believing a dominant check.** If one check is more than half your findings, stop trusting it
for this report, verify five of its hits by hand, and file the bug.

**Reporting your own rate limit as a site defect.** Section 3.

**Mixing rendered and static numbers.** `summary.totals.pages_by_representation` says how each
page was measured. A figure averaged across both was never measured the same way twice.

---

## 7. When the tool is wrong

It will be. Four defects were found on three live sites in a single afternoon — and all four
were in the *checks*, none in the traversal: every URL was fetched exactly once, redirects were
observed and not followed, off-host links were recorded and never requested, the breaker held.
The crawl was right; the conclusions drawn from it were not.

**A finding you cannot reproduce yourself is not a finding.** Every critical that goes into a
deliverable gets checked against the live URL first, slowly, with a browser user agent:

```bash
curl -s -o /dev/null -w '%{http_code} -> %{redirect_url}\n' -A 'Mozilla/5.0' https://example.com
```

Both outcomes are worth the time. The same step confirmed an entire twelve-page services section
really returning 404, and refuted 78 findings that claimed a canonical was a redirect when it
answered 200.

**When it is wrong, the output is an issue, not a workaround.** Attach the real page as a
fixture. Never a local patch, never a throwaway script that routes around the defect — a
workaround makes the next run wrong in the same way, and nobody remembers why.

An issue here is a specification: the symptom, the cause with a file and a line, what it
corrupts downstream, what is requested, and acceptance criteria. Then one commit, one pull
request, one issue closed.

---

## 8. What it cannot answer at all

Say these out loud in anything you hand over. A limit nobody states is a limit the reader
assumes away.

- **It measures the site as served, not as ranked.** No rankings, no traffic, no indexation, no
  competitors, no before-and-after attribution.
- **It measures structure, not quality.** Expertise, usefulness, tone and accuracy are yours to
  judge.
- **Lab numbers, not field data.** Timings come from one headless browser on one connection.
- **One host, unless told otherwise.** Off-host links are recorded and never fetched.
- **Static markup, unless rendering ran.** Check `pages_by_representation`.
- **It cannot fix a server.** An archive of optimized images is not a deploy; a security grade
  is not a hardened server.
- **It does not know what matters.** Severity is not priority: a critical on a page nobody
  visits ranks below a warning on the page that earns the money, and nothing here knows which
  is which.

---

## 9. Where to go next

- **[scenarios/](scenarios/README.md)** — chains end to end, each with its commands, its output,
  its cost and its limits. Start here once you know what you want.
- **[COVERAGE_SF_ISSUES.md](COVERAGE_SF_ISSUES.md)** — every issue in the field's published
  catalogue, and whether this toolkit finds it.
- **[.claude/skills/control/SKILL.md](../.claude/skills/control/SKILL.md)** — the same loop,
  written for an agent to follow.
- **[TESTING.md](TESTING.md)** — how the suite is run, and why chain tests exist beside unit
  tests.
