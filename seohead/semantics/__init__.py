"""Accumulating semantic-core pipeline: one SQLite database per project, resumable stages.

The package imports nothing heavy at module level. Paid stages reach providers only through
``seohead.data_sources``; Louvain clustering needs the optional ``semantics`` extra.
"""

STAGES = (
    "init",
    "import",
    "collect",
    "clean",
    "graph",
    "exact",
    "cluster",
    "competitors",
    "synonyms",
    "report",
    "excel",
    "mine",
    "sitematch",
    "relevance",
    "status",
    "export",
    "clusters",
    "cluster-show",
    "cluster-queries",
    "set-landing",
    "insights",
    "insights-tasks",
)
#: Stages that call a billed provider (Yandex Cloud or Arsenkin) and need explicit consent.
PAID_STAGES = frozenset({"collect", "synonyms", "cluster", "exact"})
#: Stages that fetch public web pages but spend no provider quota.
WEB_STAGES = frozenset({"mine"})
LOCAL_STAGES = tuple(s for s in STAGES if s not in PAID_STAGES and s not in WEB_STAGES)
