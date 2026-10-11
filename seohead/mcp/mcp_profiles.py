"""Explicit MCP tool profiles that remove unadvertised schemas after registration."""

from __future__ import annotations

from typing import Any

PROFILES = frozenset({"full", "audit", "infra", "quick-check", "router"})
#: Russian captions for clients (SEOHEAD Desktop) so they keep no profile list of their own.
PROFILE_LABELS = {
    "full": "Полный",
    "audit": "Аудит",
    "quick-check": "Быстрая проверка",
    "infra": "Инфраструктура",
    "router": "Маршрутизатор",
}
HIGH_LEVEL_TOOLS = frozenset({"seo_inspect_url", "seo_audit_workflow", "seo_tool_catalog"})
_PROFILE_TOOLS = {
    "audit": frozenset(
        {
            "seo_inspect_url",
            "seo_audit_workflow",
            "seo_tool_catalog",
            "seo_doctor",
            "sf_audit_run",
            "sf_audit_summary",
            "sf_audit_issues",
            "sf_audit_tasks",
            "sf_list_exports",
        }
    ),
    "infra": frozenset(
        {
            "seo_inspect_url",
            "seo_tool_catalog",
            "seo_doctor",
            "seo_domain_profile",
            "seo_cdn_check",
            "seo_tech_detect",
            "seo_security_check",
            "seo_headers_check",
            "seo_robots_check",
            "seo_sitemap_crawl",
            "seo_regions_check",
        }
    ),
    "quick-check": frozenset(
        {
            "seo_inspect_url",
            "seo_tool_catalog",
            "seo_doctor",
            "seo_parse",
            "seo_headers_check",
            "seo_robots_check",
            "seo_redirects_check",
            "seo_url_check_batch",
            "seo_schema_check",
            "seo_hreflang_check",
        }
    ),
    # The router excludes seo_tool_run: an action:any schema could bypass real tool guards.
    "router": frozenset({"seo_inspect_url", "seo_audit_workflow", "seo_tool_catalog", "seo_doctor"}),
}


def profile_tools(profile: str) -> frozenset[str] | None:
    """Return advertised tool names, or None for compatibility-preserving full mode."""
    if profile not in PROFILES:
        raise ValueError(f"profile must be one of {sorted(PROFILES)}")
    return None if profile == "full" else _PROFILE_TOOLS[profile]


def configure_profile(server: Any, profile: str) -> dict[str, Any]:
    """Remove non-profile tools through FastMCP's public remove_tool API.

    This is a one-way startup operation. Build a fresh server for a different profile; re-adding
    removed private Tool objects would depend on unsupported SDK internals and stale schemas.
    """
    allowed = profile_tools(profile)
    existing = {tool.name for tool in server._tool_manager.list_tools()}
    if allowed is None:
        return {"profile": "full", "advertised": sorted(existing), "removed": []}
    required = {name for name in allowed if name in HIGH_LEVEL_TOOLS}
    missing = sorted(required - existing)
    if missing:
        raise ValueError(f"profile requires registered high-level tools: {', '.join(missing)}")
    removed = sorted(existing - allowed)
    for name in removed:
        server.remove_tool(name)
    return {"profile": profile, "advertised": sorted(allowed & existing), "removed": removed}
