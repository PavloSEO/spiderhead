"""Public MCP catalog metadata is stable and derived from registrations."""

from seohead.mcp.tool_reference import ToolSpec, _category, load_seo_tools


def test_registered_tools_have_a_stable_category_and_project_flag():
    tools = load_seo_tools()
    assert tools
    assert all(tool.category and isinstance(tool.project_aware, bool) for tool in tools)
    assert _category("project_status") == "project"
    assert _category("scan_status") == "scan"
    assert _category("unknown_future_tool") == "other"


def test_tool_spec_exposes_the_new_metadata_fields():
    assert {"category", "project_aware"} <= set(ToolSpec.__dataclass_fields__)
