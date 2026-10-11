"""Provider capability and specialist workflow matrix (issue #779, epic #753).

Data-only module. The provider inventory table is rendered straight from
``seohead.data_sources.providers.provider_registry()``, so it can never drift
ahead of the declared registry. The workflow rows are a finite curated
catalogue whose command and provider references are kept honest by
``tests/test_provider_matrix.py`` — the same boundary pattern as
``seohead/core/input_contracts.py`` — rather than by importing the interface layers
at runtime.

Two distinctions the matrix exists to keep visible:

- ``declared`` is not ``verified``. A provider listed in the registry is a
  declared contract; a present credential is configuration state. Only an
  explicit ``provider-verify`` read that returns ``target_access=verified``
  proves live access, and this document asserts none.
- ``registered operation`` is not ``shipped surface``. An operation the
  registry declares but no handler or collection dispatch reaches stays
  ``unsupported`` here, by name.

Provider identity is typed, not a bare name, because the same vendor reaches
the code through different routes. ``registry`` references are ids in
``provider_registry()`` reached through ``provider-verify`` /
``provider-collect``; ``dedicated`` references are integration modules the
handlers call directly, bypassing the registry dispatch
(``keywords-expand``/``keywords-seasonality``/``regions-tree``/``serp-fetch``
reach ``seohead.data_sources.yandex_cloud``, ``keywords-exact`` reaches
``seohead.data_sources.arsenkin``, ``indexnow-submit`` reaches
``seohead.data_sources.indexnow``, ``topvisor-read`` reaches
``seohead.data_sources.topvisor``, and ``google-keywords``/``google-serp``
reach ``seohead.data_sources.dataforseo`` — none of which is the
``dataforseo_backlinks`` registry entry); ``local`` references are in-process
tools with no provider transport at all; ``declared`` references name a
registry contract no shipped route reaches, so they mark declared — never
shipped — surface.
"""

from __future__ import annotations

from dataclasses import dataclass

from seohead.data_sources.providers import provider_registry

SUPPORT_STATES = ("supported", "partial", "unsupported", "unverified")

PROVIDER_REF_KINDS = ("registry", "dedicated", "local", "declared")

# Providers whose credentials are optional-by-default or paid per response;
# the registry's ``default_enabled=False`` already marks them.
_ACCESS_LABELS = {
    "read_only": "read-only",
    "read_only_paid": "read-only, paid",
    "read_only_optional_paid": "read-only, paid, off by default",
    "confirmed_write": "confirmed write, off by default",
}

_PRIVACY_LABELS = {
    "public": "public data",
    "public_url_list": "caller-supplied URL list, public endpoint",
    "aggregate": "aggregate statistics",
    "restricted": "restricted site/account data",
}


@dataclass(frozen=True)
class ProviderRef:
    """One typed route to a provider or in-process capability.

    ``kind`` is one of :data:`PROVIDER_REF_KINDS`: ``registry`` (an id in
    ``provider_registry()``), ``dedicated`` (a named integration the handlers
    call directly, bypassing the registry), ``local`` (an in-process tool
    with no provider transport), or ``declared`` (a registry contract with no
    shipped route — a declaration, not a callable path).
    """

    kind: str
    name: str


def _reg(name: str) -> ProviderRef:
    return ProviderRef("registry", name)


def _ded(name: str) -> ProviderRef:
    return ProviderRef("dedicated", name)


def _loc(name: str) -> ProviderRef:
    return ProviderRef("local", name)


def _decl(name: str) -> ProviderRef:
    return ProviderRef("declared", name)


@dataclass(frozen=True)
class WorkflowRow:
    """One specialist workflow against the shipped provider surface.

    ``providers`` holds typed :class:`ProviderRef` routes; ``surface`` holds CLI
    command names; each is verified against the command registry or the code it
    names by tests. ``status`` is one of :data:`SUPPORT_STATES` and reflects
    code and test evidence, never an assumed provider capability.
    """

    workflow: str
    use_case: str
    providers: tuple[ProviderRef, ...]
    surface: tuple[str, ...]
    status: str
    auth: str
    cost_quota: str
    privacy: str
    limitations: str
    csv_fallback: str


WORKFLOWS: tuple[WorkflowRow, ...] = (
    WorkflowRow(
        workflow="yandex-demand",
        use_case="Expand a seed phrase and read demand seasonality for Yandex",
        providers=(_ded("yandex_cloud"),),
        surface=("keywords-expand", "keywords-seasonality", "regions-tree"),
        status="supported",
        auth="API key + folder ID",
        cost_quota="Paid; Wordstat hourly quota; charges journaled in spend-report",
        privacy="aggregate",
        limitations=(
            "Base frequency only — the API has no ! / + / [] operators and base "
            "counts run roughly 9x exact; use Arsenkin for exact values. The "
            "commands call the dedicated yandex_cloud module (Wordstat) "
            "directly; provider-collect deliberately refuses this paid contract"
        ),
        csv_fallback="not applicable — demand data is not URL-keyed",
    ),
    WorkflowRow(
        workflow="yandex-exact-frequency",
        use_case="Exact !W frequency the Wordstat API does not expose",
        providers=(_ded("arsenkin"),),
        surface=("keywords-exact",),
        status="supported",
        auth="API token",
        cost_quota="Paid; consumes Arsenkin account limits; task_id journaled at billing time",
        privacy="aggregate",
        limitations=(
            "Billed at task creation; a timed-out poll is retrievable by "
            "task_id. The command calls ArsenkinClient.set_task directly; "
            "provider-collect deliberately refuses this paid contract"
        ),
        csv_fallback="not applicable",
    ),
    WorkflowRow(
        workflow="google-demand",
        use_case="Search volume, seed expansion, and keyword difficulty for Google",
        providers=(_ded("dataforseo"),),
        surface=("google-keywords",),
        status="supported",
        auth="Login + password",
        cost_quota="Paid per response; sandbox is the default and is free",
        privacy="restricted",
        limitations=(
            "The handler calls seohead.data_sources.dataforseo (source `dataforseo`) "
            "directly — a dedicated integration, not the `dataforseo_backlinks` registry "
            "entry, which declares only backlink operations; Russia and Belarus locations "
            "are refused by the coverage guard before any paid call"
        ),
        csv_fallback="not applicable",
    ),
    WorkflowRow(
        workflow="serp-collection",
        use_case="Fetch ranked results for queries on Yandex or Google",
        providers=(_ded("yandex_cloud"), _ded("dataforseo")),
        surface=("serp-fetch", "google-serp"),
        status="supported",
        auth="Yandex: API key + folder ID; DataForSEO: login + password",
        cost_quota="Metered; Yandex async endpoint only — the ~16x-costlier sync endpoint is excluded",
        privacy="aggregate",
        limitations=(
            "Both routes are dedicated modules the handlers call directly — "
            "serp-fetch uses yandex_cloud.WebSearch, google-serp uses the "
            "dataforseo integration, not the registry; queries billed but not "
            "returned before timeout stay visible in the spend journal as "
            "named operations"
        ),
        csv_fallback="not applicable",
    ),
    WorkflowRow(
        workflow="keyword-text-clustering",
        use_case="Draft-group a caller-supplied keyword list by text similarity",
        providers=(_loc("seohead.checks.clusterer"),),
        surface=("keywords-cluster",),
        status="supported",
        auth="None — in-process computation",
        cost_quota="Free; requires the optional 'cluster' dependency extra (scikit-learn)",
        privacy="caller-supplied keyword list stays in-process; no provider transport exists",
        limitations=(
            "TF-IDF similarity over keyword text only — it never fetches or compares "
            "search results and has no provider route; SERP-based clustering is the "
            "separate serp-clustering workflow below"
        ),
        csv_fallback="a user-supplied keyword list is the input itself",
    ),
    WorkflowRow(
        workflow="serp-clustering",
        use_case="Cluster a keyword set by overlapping search results",
        providers=(_decl("arsenkin"),),
        surface=(),
        status="unsupported",
        auth="Arsenkin API token would be required for the declared provider operation",
        cost_quota="Declared paid operation; consumes Arsenkin account limits",
        privacy="aggregate",
        limitations=(
            "The registry declares arsenkin serp_clustering, but provider-collect "
            "deliberately refuses the paid Arsenkin contract and no dedicated handler "
            "ships it — keywords-cluster is text-similarity clustering, not this workflow"
        ),
        csv_fallback="none",
    ),
    WorkflowRow(
        workflow="search-console-evidence",
        use_case="Clicks, impressions, position, indexing verdicts, and sitemap status for a property you own",
        providers=(_reg("gsc"),),
        surface=("gsc-query", "provider-auth", "provider-verify", "provider-collect"),
        status="supported",
        auth="OAuth bearer or service account; durable grant via provider-auth",
        cost_quota="Free within Search Console row and request limits",
        privacy="restricted",
        limitations=(
            "Verification distinguishes an authenticated account from verified access "
            "to the requested property"
        ),
        csv_fallback="yes — an exported Search Console CSV joins the crawl via provider-join",
    ),
    WorkflowRow(
        workflow="webmaster-evidence",
        use_case="Yandex and Bing webmaster data: hosts, indexing, diagnostics, search performance, history, and Yandex URL queries",
        providers=(_reg("yandex_webmaster"), _reg("bing_webmaster")),
        surface=("provider-verify", "provider-collect", "webmaster-url-queries"),
        status="supported",
        auth="Yandex: OAuth bearer; Bing: API key",
        cost_quota="Free within each webmaster API quota",
        privacy="restricted",
        limitations=(
            "Yandex URL-query analysis has a bounded dedicated route; other provider "
            "operations remain on provider-verify/provider-collect"
        ),
        csv_fallback="yes — URL-keyed exports join via provider-join",
    ),
    WorkflowRow(
        workflow="traffic-analytics",
        use_case="Counter configuration, aggregate reports, landing-page sessions and visited-page views",
        providers=(_reg("metrika"), _reg("ga4")),
        surface=(
            "metrika-counters",
            "metrika-setup",
            "metrika-report",
            "metrika-traffic-pdf",
            "provider-collect",
        ),
        status="partial",
        auth="Metrika: OAuth bearer; GA4: OAuth bearer or shared Google service account",
        cost_quota="Free within Metrika and GA4 Data API quotas",
        privacy="restricted — may contain personal identifiers; kept out of reports and commits",
        limitations=(
            "Metrika raw Logs API is intentionally unreachable; GA4 landing_pages and page_views "
            "are read-only provider-collect operations. Visited-page views preserve date/host/path "
            "grain and are not sessions; missing or incompatible evidence never becomes zero"
        ),
        csv_fallback="yes — URL-keyed analytics exports join via provider-join",
    ),
    WorkflowRow(
        workflow="field-vitals",
        use_case="Core Web Vitals as real users measured them — CrUX field data at origin or URL level",
        providers=(_reg("crux"),),
        surface=("crux-report", "provider-collect", "site-audit"),
        status="supported",
        auth="Google Cloud API key",
        cost_quota="Free within Google API quotas",
        privacy="aggregate",
        limitations=(
            "Field p75 verdicts require an eligible CrUX current record and valid collection "
            "period; no data or missing metrics are unavailable, not passing. Site-audit "
            "consumes explicitly supplied evidence and never calls CrUX automatically"
        ),
        csv_fallback="no provider CSV join; CrUX has no user-export path here",
    ),
    WorkflowRow(
        workflow="lab-vitals",
        use_case="Lighthouse lab samples via PageSpeed Insights — categories and audits",
        providers=(_reg("pagespeed"),),
        surface=("provider-collect",),
        status="supported",
        auth="Google API key",
        cost_quota="Free within Google API quotas",
        privacy="aggregate",
        limitations=(
            "The PSI parser deliberately returns Lighthouse lab data marked "
            "lab_only: true — a synthetic measurement, not real-user field data; "
            "mobile_samples/desktop_samples run only via provider-collect, no "
            "dedicated command ships"
        ),
        csv_fallback="no provider CSV join; PSI has no user-export path here",
    ),
    WorkflowRow(
        workflow="link-evidence",
        use_case="Merge explicit backlink evidence, classify full rel tokens and retain history",
        providers=(
            _reg("dataforseo_backlinks"),
            _reg("yandex_webmaster"),
            _reg("bing_webmaster"),
            _ded("backlink_indexes"),
        ),
        surface=(
            "provider-collect",
            "backlinks-profile",
            "backlinks-disavow",
            "dataforseo-estimate",
        ),
        status="partial",
        auth="Login + password",
        cost_quota="Paid per provider response; provider is off by default",
        privacy="restricted",
        limitations=(
            "DataForSEO has bounded summary, backlinks, referring domains, history and "
            "domain-page summary routes with confirmation; free property samples and CSV/WAT "
            "imports are merged locally. No source proves whole-web completeness; "
            "Ahrefs/Majestic index reads are explicit; Moz is legacy V2 metrics only, "
            "not a backlink enumerator. Project/month currency limits await an owner decision"
        ),
        csv_fallback="GSC, Ahrefs, Majestic, Moz, Semrush, DataForSEO CSV and Common Crawl WAT/domain-edge extracts",
    ),
    WorkflowRow(
        workflow="rank-tracking",
        use_case="Read existing Topvisor projects, keywords, competitors, recorded position history and summaries",
        providers=(_ded("topvisor"),),
        surface=("topvisor-read",),
        status="supported",
        auth="API token + user ID from the central credential files",
        cost_quota="Reads within the existing Topvisor account; paid position-check launches are structurally unreachable",
        privacy="restricted",
        limitations=(
            "One bounded page per call — the continuation signal is the provider's "
            'nextOffset, not len(result) == limit; a "--" position is unavailable, '
            "not rank 0; only fixed get endpoints are wired, so add/edit/del and "
            "paid checker launches cannot be reached"
        ),
        csv_fallback="not applicable — Topvisor rows have no provider-join route",
    ),
    WorkflowRow(
        workflow="content-gap-analysis",
        use_case="Compare supplied competitor pages or texts and surface author-facing word-density gaps",
        providers=(_ded("miratext"),),
        surface=("miratext-analyze",),
        status="supported",
        auth="Miratext API key",
        cost_quota="Free queue by default; paid or keyword search needs explicit confirmation and is journaled with unknown provider units",
        privacy="restricted — supplied URLs, texts, and result tables stay local",
        limitations=(
            "Only explicitly supplied URLs/texts are analysed; the optional provider-built "
            "keyword competitor set is paid and never starts without confirmation. Final tables "
            "retain provider-reported units and mark an unknown response shape unavailable"
        ),
        csv_fallback="yes — use supplied competitor URLs/texts; author tables are returned as JSON",
    ),
    WorkflowRow(
        workflow="url-submission",
        use_case="Notify Bing, Yandex, Naver, and Seznam that URLs changed",
        providers=(_ded("indexnow"),),
        surface=("indexnow-submit",),
        status="supported",
        auth="Self-generated key hosted on the target site",
        cost_quota="Free; provider submission quota; off by default",
        privacy="caller-supplied URL list sent to a public endpoint",
        limitations=(
            "Confirmed write, not a collection — the command calls the "
            "dedicated indexnow.submit directly and provider-collect refuses "
            "it by contract; Google has not joined IndexNow"
        ),
        csv_fallback="not applicable — write operation",
    ),
    WorkflowRow(
        workflow="public-recon",
        use_case="Snapshot history of a URL and subdomains named in public certificate logs",
        providers=(_reg("wayback"), _reg("crtsh")),
        surface=("wayback-history", "crtsh-subdomains"),
        status="supported",
        auth="None — public services",
        cost_quota="Free; public service pacing and availability",
        privacy="public",
        limitations="provider-verify reports not_required — there is no access contract to verify",
        csv_fallback="not applicable",
    ),
    WorkflowRow(
        workflow="evidence-join",
        use_case="Attach external URL-keyed rows to a crawl and surface orphan candidates for explicit review",
        providers=(
            _reg("gsc"),
            _reg("ga4"),
            _reg("metrika"),
            _reg("yandex_webmaster"),
            _reg("bing_webmaster"),
        ),
        surface=("provider-join", "provider-replay"),
        status="supported",
        auth="None — operates on already-collected or user-supplied rows",
        cost_quota="Free; local operation",
        privacy="restricted inputs stay local; artifact directory is created mode 0700",
        limitations=(
            "Never changes the crawl frontier; unkeyable, crawl-only, and "
            "external-only populations stay named and separate"
        ),
        csv_fallback="yes — this is the CSV fallback path itself",
    ),
)

UNSUPPORTED_WORK: tuple[tuple[str, str], ...] = (
    (
        "arsenkin serp_clustering via provider-collect",
        "declared in the registry but deliberately refused by the collection dispatch; "
        "tracked by the dedicated-operation contract rather than duplicated here",
    ),
    (
        "metrika raw_logs (Yandex Logs API)",
        "excluded by design — raw logs may contain personal identifiers and must not "
        "reach reports or commits",
    ),
    (
        "dedicated CLI commands for ga4, pagespeed, and bing_webmaster",
        "these providers are reachable only through provider-verify/provider-collect",
    ),
    (
        "Google IndexNow submission",
        "Google has not joined IndexNow; no operation can claim it",
    ),
    (
        "Topvisor mutations and paid position-check launches",
        "topvisor-read wires only fixed read-only get endpoints; add/edit/del and "
        "the paid checker stay unreachable by design",
    ),
)

RELATED_ISSUES = ("#716", "#718", "#719", "#724", "#730")


_PROVIDER_SUFFIX = {
    "registry": "",
    "dedicated": " (dedicated integration)",
    "local": " (in-process)",
    "declared": " (declared registry operation — no shipped route)",
}


def _provider_label(ref: ProviderRef) -> str:
    return f"`{ref.name}`{_PROVIDER_SUFFIX[ref.kind]}"


def _cell(text: str) -> str:
    return text.replace("|", "\\|")


def render() -> str:
    """Build docs/PROVIDERS.md from the live registry and the workflow catalogue."""
    registry = provider_registry()["providers"]
    lines = [
        "# Provider capability and workflow matrix",
        "",
        "Generated from `seohead/data_sources/providers.py` and the workflow catalogue in "
        "`seohead/core/provider_matrix.py` — do not edit by hand. Regenerate with:",
        "",
        "```bash",
        "python scripts/generate_provider_matrix.py",
        "```",
        "",
        "Issue #779 (epic #753). Related implementation issues: "
        + ", ".join(RELATED_ISSUES)
        + " — this matrix links them instead of duplicating their scopes.",
        "",
        "## How to read the statuses",
        "",
        "- **supported** — a shipped command or collection dispatch reaches the operation, "
        "with offline tests behind it.",
        "- **partial** — the workflow works but the shipped surface is narrower than the "
        "declared registry entry; the gap is named in the limitations column.",
        "- **unsupported** — declared or plausible work the code deliberately does not do; "
        "see *Unsupported work* below.",
        "- **unverified** — no code path exists to confirm or deny; nothing in this matrix "
        "is labelled live-verified: a registry entry is a declared contract, a present "
        "credential is configuration state, and only an explicit `provider-verify` read "
        "returning `target_access=verified` proves live target access.",
        "",
        "Provider names in the workflow matrix are typed routes: a bare ``name`` is a "
        "registry id reached through `provider-verify`/`provider-collect`; "
        "``name`` *(dedicated integration)* is a module the handlers call directly "
        "outside the registry dispatch; ``name`` *(in-process)* is a local computation "
        "with no provider transport; ``name`` *(declared registry operation — no "
        "shipped route)* is a contract the registry declares but no shipped surface "
        "reaches.",
        "",
        "## Provider inventory",
        "",
        "Rendered from the registry exactly as `provider-registry` reports it.",
        "",
        "| Provider | Access | Operations | Credentials | Quota / cost | Privacy |",
        "|---|---|---|---|---|---|",
    ]
    for name, meta in registry.items():
        operations = list(meta["operations"])
        excluded = meta.get("excluded_operations")
        if excluded:
            operations.append(f"~~{', '.join(excluded)}~~ (excluded)")
        credentials = ", ".join(meta["credential_components"]) or "none"
        default_note = "" if meta.get("default_enabled", True) else " (off by default)"
        lines.append(
            "| "
            + " | ".join(
                _cell(value)
                for value in (
                    f"`{name}`{default_note}",
                    _ACCESS_LABELS.get(meta["access"], meta["access"]),
                    ", ".join(operations),
                    credentials,
                    meta["quota_mode"],
                    _PRIVACY_LABELS.get(meta["privacy_class"], meta["privacy_class"]),
                )
            )
            + " |"
        )
    lines += [
        "",
        "## Specialist workflow matrix",
        "",
        "| Workflow | Use case | Providers | Shipped surface | Status | Auth | Cost / quota | "
        "Privacy | Limitations | CSV fallback |",
        "|---|---|---|---|---|---|---|---|---|---|",
    ]
    for row in WORKFLOWS:
        lines.append(
            "| "
            + " | ".join(
                _cell(value)
                for value in (
                    f"`{row.workflow}`",
                    row.use_case,
                    ", ".join(_provider_label(p) for p in row.providers),
                    ", ".join(f"`{c}`" for c in row.surface) or "—",
                    row.status,
                    row.auth,
                    row.cost_quota,
                    row.privacy,
                    row.limitations,
                    row.csv_fallback,
                )
            )
            + " |"
        )
    lines += [
        "",
        "## Unsupported work, stated explicitly",
        "",
    ]
    lines += [f"- **{name}** — {reason}" for name, reason in UNSUPPORTED_WORK]
    lines += [
        "",
        "## Phased first release",
        "",
        "The first release phase covers the workflows whose evidence is free or already "
        "bounded by first-party quotas: `public-recon`, `evidence-join`, "
        "`keyword-text-clustering`, `search-console-evidence`, `webmaster-evidence`, "
        "`field-vitals`, `lab-vitals`, `traffic-analytics`, and `url-submission` behind "
        "its existing confirmed-write gate. Phase two adds the metered demand and SERP "
        "workflows (`yandex-demand`, `yandex-exact-frequency`, `google-demand`, "
        "`serp-collection`) once the caller deliberately configures the paid credentials "
        "they require — sandbox remains the DataForSEO default. Deferred by design, not "
        "by omission: `serp-clustering` via the paid Arsenkin contract (declared but "
        "unsupported), Metrika raw logs, dedicated CLI commands for the collect-only "
        "providers. Bounded backlink discovery requires explicit source selection and cost confirmation. This is a "
        "selected subset of the provider landscape, not a claim that every SEO data "
        "service is covered.",
        "",
        "Every row above reflects code and test evidence in this repository; no provider "
        "is described as live-verified by this document.",
    ]
    return "\n".join(lines).rstrip() + "\n"
