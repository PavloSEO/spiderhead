"""Render the per-tool reference documentation from the MCP tool definitions.

Every ``seo_*``/``sf_*`` MCP tool in :mod:`seohead.mcp.mcp_server` and
:mod:`seohead.mcp.sf_mcp` is already a complete, reviewed specification: a name,
argument names with type annotations and defaults, a docstring stating what it does
and what it costs, and a ``ToolAnnotations`` profile stating whether it touches the
network, writes files, or is idempotent. Restating any of that by hand in a Markdown
file would just be a second copy that can silently disagree with the first — the exact
failure this module exists to remove.

This parses both files with :mod:`ast` rather than importing them, so building the
reference needs neither the optional ``mcp`` dependency nor a running server.
``docs/TOOL_REFERENCE.md`` is generated from it and ``tests/test_docs_drift.py`` fails
the build the moment it stops matching.
"""

from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path

MCP_SERVER_SOURCE = Path(__file__).with_name("mcp_server.py")
SF_MCP_SOURCE = Path(__file__).with_name("sf_mcp.py")
SEMANTICS_MCP_SOURCE = Path(__file__).with_name("semantics_mcp.py")


@dataclass(frozen=True)
class Argument:
    name: str
    type: str
    default: str  # the literal source text, or "required" when there is none


@dataclass(frozen=True)
class ToolSpec:
    name: str  # the MCP tool name, e.g. "seo_parse"
    command: str  # its CLI form, e.g. "parse" ("" when there is no 1:1 command)
    summary: str  # the docstring's first sentence
    notes: str  # the rest of the docstring: behavior, cost, and failure-mode prose
    arguments: tuple[Argument, ...]
    network: bool  # openWorldHint: reaches beyond the process (network or an external API)
    writes: bool  # not readOnlyHint: creates or modifies files
    destructive: bool  # destructiveHint: can overwrite or remove existing data
    idempotent: bool  # idempotentHint: repeating the call changes nothing further
    paid: bool  # uses the "paid" annotations profile: spends an external provider's quota


def _profiles(func_node: ast.FunctionDef) -> dict[str, dict[str, bool]]:
    """Map each local ``ToolAnnotations`` variable name to its boolean flags."""
    profiles: dict[str, dict[str, bool]] = {}
    for node in func_node.body:
        if not (
            isinstance(node, ast.Assign)
            and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name)
            and isinstance(node.value, ast.Call)
            and getattr(node.value.func, "id", "") == "ToolAnnotations"
        ):
            continue
        flags = {
            kw.arg: kw.value.value
            for kw in node.value.keywords
            if kw.arg is not None and isinstance(kw.value, ast.Constant)
        }
        profiles[node.targets[0].id] = flags
    return profiles


def _arguments(func: ast.FunctionDef) -> tuple[Argument, ...]:
    args = func.args
    defaults = [None] * (len(args.args) - len(args.defaults)) + list(args.defaults)
    out = []
    for arg, default in zip(args.args, defaults, strict=True):
        type_str = ast.unparse(arg.annotation) if arg.annotation is not None else "Any"
        if type_str.endswith("Context"):
            continue
        default_str = "required" if default is None else ast.unparse(default)
        out.append(Argument(arg.arg, type_str, default_str))
    return tuple(out)


def _docstring_parts(func: ast.FunctionDef) -> tuple[str, str]:
    doc = ast.get_docstring(func, clean=True) or ""
    if not doc:
        return "", ""
    summary, _, rest = doc.partition("\n\n")
    return " ".join(summary.split()), rest.strip()


def _tool_specs(source: Path, register_func_name: str, command_prefix: str) -> list[ToolSpec]:
    tree = ast.parse(source.read_text(encoding="utf-8"))
    target = next(
        (
            node
            for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef) and node.name == register_func_name
        ),
        None,
    )
    if target is None:
        raise ValueError(f"{register_func_name}() not found in {source}")

    profiles = _profiles(target)
    specs = []
    for node in target.body:
        # A registered tool may be `async def` (sf_audit_run, #369) as well as a plain
        # `def`; only whether it is decorated with @mcp.tool matters here.
        if not isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
            continue
        tool_decorator = next(
            (
                d
                for d in node.decorator_list
                if isinstance(d, ast.Call) and getattr(d.func, "attr", "") == "tool"
            ),
            None,
        )
        if tool_decorator is None:
            continue
        profile_name = next(
            (
                kw.value.id
                for kw in tool_decorator.keywords
                if kw.arg == "annotations" and isinstance(kw.value, ast.Name)
            ),
            None,
        )
        flags = profiles.get(profile_name, {})
        summary, notes = _docstring_parts(node)
        command = (
            node.name[len(command_prefix) :].replace("_", "-")
            if command_prefix and node.name.startswith(command_prefix)
            else ""
        )
        specs.append(
            ToolSpec(
                name=node.name,
                command=command,
                summary=summary,
                notes=notes,
                arguments=_arguments(node),
                network=bool(flags.get("openWorldHint")),
                writes=not flags.get("readOnlyHint", True),
                destructive=bool(flags.get("destructiveHint")),
                idempotent=bool(flags.get("idempotentHint")),
                paid=profile_name == "paid",
            )
        )
    return specs


def load_seo_tools() -> list[ToolSpec]:
    """The 45 ``seo_*`` tools, in the order they are registered."""
    return _tool_specs(MCP_SERVER_SOURCE, "build_server", "seo_")


def load_sf_tools() -> list[ToolSpec]:
    """The 5 ``sf_*`` crawl-audit tools, in the order they are registered.

    These do not have a 1:1 CLI command the way ``seo_*`` tools do (``sf run``
    dispatches by ``mode``, not by tool name), so no ``command`` is derived for them.
    """
    return _tool_specs(SF_MCP_SOURCE, "register", "")


def load_semantics_tools() -> list[ToolSpec]:
    """The ``seo_semantics_*`` tools; ``seohead semantics <stage>`` dispatches by stage."""
    return _tool_specs(SEMANTICS_MCP_SOURCE, "register", "")


def _cost_line(tool: ToolSpec) -> str:
    parts = [
        f"network: {'yes' if tool.network else 'no'}",
        f"writes files: {'yes' if tool.writes else 'no'}",
        f"idempotent: {'yes' if tool.idempotent else 'no'}",
        f"spends money: {'yes, external provider quota' if tool.paid else 'no'}",
    ]
    if tool.destructive:
        parts.append("can overwrite/remove existing data")
    return " · ".join(parts)


def _render_tool(tool: ToolSpec) -> list[str]:
    heading = f"### `{tool.command}`" if tool.command else f"### `{tool.name}`"
    lines = [heading, ""]
    if tool.command:
        lines += [f"MCP name: `{tool.name}`", ""]
    if tool.summary:
        lines += [tool.summary, ""]
    if tool.arguments:
        lines += ["| Argument | Type | Default |", "|---|---|---|"]
        for arg in tool.arguments:
            lines.append(f"| `{arg.name}` | `{arg.type}` | `{arg.default}` |")
        lines.append("")
    else:
        lines += ["Takes no arguments.", ""]
    lines += [f"**Cost** — {_cost_line(tool)}", ""]
    if tool.notes:
        lines += ["**Behavior and failure modes**", "", tool.notes, ""]
    return lines


def render() -> str:
    """Build the full TOOL_REFERENCE.md content from the live MCP tool definitions."""
    seo_tools = load_seo_tools()
    sf_tools = load_sf_tools()
    semantics_tools = load_semantics_tools()
    lines = [
        "# Tool reference",
        "",
        "Generated from the MCP tool definitions in `seohead/mcp/mcp_server.py`, "
        "`seohead/mcp/sf_mcp.py` and `seohead/mcp/semantics_mcp.py` — do not edit by hand. "
        "Regenerate with:",
        "",
        "```bash",
        "python scripts/generate_tool_reference.py",
        "```",
        "",
        f"**{len(seo_tools)} core tools** (`seohead <command>` / "
        "`seo_<command>` on the MCP server) plus "
        f"**{len(sf_tools)} crawl-audit tools** (`sf_<command>`, driven by `seohead sf ...`) "
        f"plus **{len(semantics_tools)} semantic-core tools** (`seo_semantics_*`, driven by "
        "`seohead semantics <stage>`) "
        f"— {len(seo_tools) + len(sf_tools) + len(semantics_tools)} in total.",
        "",
        "Every tool shares one contract: JSON in, JSON out. A target that could not be "
        'reached comes back as `{"ok": false, "error": "..."}` instead of raising, so '
        "an unreachable site is data, not a crash.",
        "",
        "- **Cost** — network/file/spend flags read from the tool's `ToolAnnotations` "
        "profile: whether it reaches beyond the process, whether it creates or changes "
        "files, whether repeating the call is safe, and whether it spends an external "
        "provider's quota.",
        "- **Behavior and failure modes** — the remainder of the tool's own docstring: "
        "what it deliberately skips, what a degraded answer looks like, and what to use "
        "instead when it is the wrong tool for the job.",
        "",
        "---",
        "",
        "## Live URL, recon, and data-source tools",
        "",
    ]
    for tool in seo_tools:
        lines += _render_tool(tool)
    lines += ["---", "", "## Crawl-audit tools (Screaming Frog)", ""]
    for tool in sf_tools:
        lines += _render_tool(tool)
    lines += ["---", "", "## Semantic-core tools", ""]
    for tool in semantics_tools:
        lines += _render_tool(tool)

    return "\n".join(lines).rstrip() + "\n"
