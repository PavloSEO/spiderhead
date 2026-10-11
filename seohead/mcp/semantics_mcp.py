"""MCP tools of the semantic core (``seohead semantics``) on the unified ``seohead`` server.

They are registered separately, like the ``sf_*`` tools, because a semantic stage is not a
1:1 ``seohead`` command: ``seohead semantics <stage> --project DIR`` dispatches by stage.
Paid stages share one tool that refuses to run without ``confirm_paid=true``.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any


def register(mcp, _checked: Callable[[Any], Any]):  # pragma: no cover - needs the SDK
    """Register the ``seo_semantics_*`` tools on an existing FastMCP server."""
    from mcp.types import ToolAnnotations

    read_files = ToolAnnotations(
        readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False
    )
    create_files = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=False
    )
    create_files_from_web = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=True
    )
    paid = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=True
    )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_semantics_status(project: str) -> dict[str, Any]:
        """Read a semantic core's phrase counters, provider units used and the next stage,
        without migrating or writing its database. project is the semantic project directory,
        or a SEOHEAD project workspace whose semantics/ subdirectory holds the core."""
        from seohead.semantics.runner import run_quiet

        return _checked(run_quiet("status", project, allowed=("status",)))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_semantics_run(
        stage: str,
        project: str,
        out: str | None = None,
        file: str | None = None,
        sheet: str | None = None,
        mapping: dict[str, str] | None = None,
        merge: str = "add",
        scope: str = "all",
        dry_run: bool = False,
    ) -> dict[str, Any]:
        """Run one free semantic-core stage or preview an import/export without writes.
        Import accepts CSV or XLSX, sheet selection, explicit field-to-column mapping and
        add/update/replace merge policy. Export and Excel accept all, cluster:<name> or
        no-landing scope. Phrases and evidence are retained; statuses change reversibly.
        No network access; paid stages are refused here (use seo_semantics_paid)."""
        from seohead.semantics import LOCAL_STAGES
        from seohead.semantics.runner import run_quiet

        return _checked(
            run_quiet(
                stage,
                project,
                allowed=tuple(
                    s
                    for s in LOCAL_STAGES
                    if s
                    not in {
                        "status",
                        "clusters",
                        "cluster-show",
                        "cluster-queries",
                        "set-landing",
                        "insights",
                        "insights-tasks",
                    }
                ),
                out=out,
                file=file,
                sheet=sheet,
                mapping=mapping,
                merge=merge,
                scope=scope,
                dry_run=dry_run,
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_semantics_mine(project: str, out: str | None = None) -> dict[str, Any]:
        """Fetch the most frequent competitor pages from the core's cached SERP and append
        title/heading 2-3-grams to synonyms.txt as candidate phrases. Reads public pages; spends
        no provider quota. Run seo_semantics_paid stage=synonyms afterwards to check them."""
        from seohead.semantics.runner import run_quiet

        return _checked(run_quiet("mine", project, allowed=("mine",), out=out))

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_semantics_paid(
        stage: str,
        project: str,
        confirm_paid: bool = False,
        dry_run: bool = False,
        resume: bool = False,
        max_seeds: int = 0,
        max_phrases: int = 100000,
        limit: int = 0,
        method: str = "serp",
        yes: bool = False,
    ) -> dict[str, Any]:
        """Preview or run one paid semantic-core stage: collect (Wordstat expansion of seeds.txt),
        synonyms (Wordstat check of synonyms.txt), cluster (Yandex Search SERP, cached per
        phrase; method=louvain needs the semantics extra) or exact (Arsenkin !W, once, at the
        end). The offline estimate is returned before confirmation; paid: without
        confirm_paid=true nothing is sent. Every submission is reserved
        before the call, so an interrupted or ambiguous request is never paid for twice; charges
        land in the shared spend journal (seo_spend_report)."""
        from seohead.semantics import PAID_STAGES
        from seohead.semantics.runner import run_quiet

        return _checked(
            run_quiet(
                stage,
                project,
                allowed=PAID_STAGES,
                confirm_paid=confirm_paid,
                dry_run=dry_run,
                resume=resume,
                max_seeds=max_seeds,
                max_phrases=max_phrases,
                limit=limit,
                method=method,
                yes=yes,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_semantics_clusters(project: str, offset: int = 0, limit: int = 50) -> dict[str, Any]:
        """Read paginated semantic clusters and nullable frequency aggregates."""
        from seohead.semantics.runner import run_quiet

        return _checked(run_quiet("clusters", project, allowed=("clusters",), offset=offset, limit=limit, json=True))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_semantics_cluster_show(project: str, cluster: str) -> dict[str, Any]:
        """Read one semantic cluster, its phrases aggregate and stored landing candidates."""
        from seohead.semantics.runner import run_quiet

        return _checked(run_quiet("cluster-show", project, allowed=("cluster-show",), cluster=cluster, json=True))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_semantics_cluster_queries(
        project: str, cluster: str, offset: int = 0, limit: int = 50
    ) -> dict[str, Any]:
        """Read a bounded page of phrases belonging to one semantic cluster."""
        from seohead.semantics.runner import run_quiet

        return _checked(run_quiet("cluster-queries", project, allowed=("cluster-queries",), cluster=cluster, offset=offset, limit=limit, json=True))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_semantics_set_landing(project: str, cluster: str, url: str) -> dict[str, Any]:
        """Set or clear a cluster landing URL and retain an audit record."""
        from seohead.semantics.runner import run_quiet

        return _checked(run_quiet("set-landing", project, allowed=("set-landing",), cluster=cluster, url=url, json=True))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_semantics_insights(
        project: str,
        period: str,
        sources: list[str] | None = None,
    ) -> dict[str, Any]:
        """Read weekly GSC, Metrika and Webmaster evidence by cluster and evaluate explicit rules.
        Reads local provider history and project bindings only; never fetches providers. Missing
        values stay null with a reason."""
        from seohead.semantics.runner import run_quiet

        return _checked(
            run_quiet(
                "insights",
                project,
                allowed=("insights",),
                period=period,
                sources=",".join(sources or ("gsc", "metrika", "webmaster")),
                json=True,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_semantics_insights_tasks(
        project: str,
        period: str,
        selections: list[dict[str, Any]],
        expected_revision: int,
        confirm: bool = False,
        dry_run: bool = False,
        sources: list[str] | None = None,
    ) -> dict[str, Any]:
        """Preview or write only selected matched rule/cluster pairs as project checklist items.

        The call never executes the task. A write requires confirm=true and the checklist
        expected_revision, preventing stale or duplicate task updates."""
        from seohead.semantics.runner import run_quiet

        return _checked(
            run_quiet(
                "insights-tasks",
                project,
                allowed=("insights-tasks",),
                period=period,
                sources=",".join(sources or ("gsc", "metrika", "webmaster")),
                selections=selections,
                expected_revision=expected_revision,
                confirm=confirm,
                dry_run=dry_run,
                json=True,
            )
        )
