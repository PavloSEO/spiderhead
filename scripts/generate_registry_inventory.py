#!/usr/bin/env python3
"""Synchronize public registry counts and generated command inventories.

The command and check registries change independently.  This script keeps the
derived overview prose in TOOLS.md and SKILLS.md synchronized without rewriting
their hand-authored route explanations.
"""

from __future__ import annotations

import re
import sys
from collections import Counter
from pathlib import Path
from textwrap import wrap

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from seohead.cli import COMMANDS  # noqa: E402
from seohead.mcp.tool_reference import (  # noqa: E402
    load_semantics_tools,
    load_seo_tools,
    load_sf_tools,
)
from seohead.sf.core.registry import CHECKS  # noqa: E402

_INVENTORY_START = "<!-- generated-command-inventory:start -->"
_INVENTORY_END = "<!-- generated-command-inventory:end -->"


def _skills() -> list[Path]:
    return [
        *sorted((ROOT / ".claude" / "skills").glob("*/SKILL.md")),
        *sorted((ROOT / "seohead" / "skills").glob("*/SKILL.md")),
    ]


def _without_skill() -> list[str]:
    mentioned: set[str] = set()
    for path in _skills():
        text = path.read_text(encoding="utf-8")
        for command in COMMANDS:
            if re.search(rf"`{re.escape(command)}`", text) or re.search(
                rf"seohead\s+{re.escape(command)}\b", text
            ):
                mentioned.add(command)
    return sorted(set(COMMANDS) - mentioned)


def _write(path: Path, content: str, *, check: bool) -> bool:
    current = path.read_text(encoding="utf-8")
    if current == content:
        return False
    if check:
        raise ValueError(f"{path.relative_to(ROOT)} is stale; run {Path(__file__).name}")
    path.write_text(content, encoding="utf-8")
    return True


def _replace(text: str, pattern: str, value: str, path: Path) -> str:
    updated, count = re.subn(pattern, value, text, count=1, flags=re.MULTILINE)
    if count != 1:
        raise ValueError(f"could not locate generated inventory anchor in {path.relative_to(ROOT)}")
    return updated


def _tools_content() -> str:
    path = ROOT / "docs" / "TOOLS.md"
    text = path.read_text(encoding="utf-8")
    commands = sorted(COMMANDS)
    callable_tools = len(load_seo_tools()) + len(load_sf_tools()) + len(load_semantics_tools())
    severity = Counter(meta["severity"] for meta in CHECKS.values())
    text = _replace(
        text,
        r"The current registry has \d+ commands and \d+ callable tools,\nwith \d+ audit checks\. These are inventories, not coverage on every input\.",
        f"The current registry has {len(commands)} commands and {callable_tools} callable tools,\n"
        f"with {len(CHECKS)} audit checks. These are inventories, not coverage on every input.",
        path,
    )
    text = _replace(
        text,
        r"\*\*\d+ checks\*\*: \d+ critical, \d+ warnings, \d+ notices\.",
        f"**{len(CHECKS)} checks**: {severity['critical']} critical, "
        f"{severity['warning']} warnings, {severity['notice']} notices.",
        path,
    )
    text = _replace(
        text,
        r"\(\d+ \+ \d+(?: \+ \d+)?\):\n\n```bash\nseohead mcp",
        f"({len(load_seo_tools())} + {len(load_sf_tools())} + {len(load_semantics_tools())}):"
        "\n\n```bash\nseohead mcp",
        path,
    )
    inventory = "\n".join(
        [
            _INVENTORY_START,
            "## Generated command inventory",
            "",
            "The command names below are generated from the shared CLI/MCP registry; detailed behavior",
            "and inputs remain in the nearby route sections and generated tool reference.",
            "",
            *wrap(
                " · ".join(f"`{command}`" for command in commands),
                width=110,
                break_long_words=False,
                break_on_hyphens=False,
            ),
            _INVENTORY_END,
            "",
        ]
    )
    marker = re.compile(rf"{re.escape(_INVENTORY_START)}.*?{re.escape(_INVENTORY_END)}\n?", re.S)
    if marker.search(text):
        return marker.sub(inventory, text)
    return text.replace("## Offline BI projection\n", inventory + "## Offline BI projection\n", 1)


def _skills_content() -> str:
    path = ROOT / "docs" / "SKILLS.md"
    text = path.read_text(encoding="utf-8")
    without_skill = _without_skill()
    section = "\n".join(
        [
            "## Tools without a skill of their own",
            "",
            f"{len(without_skill)} of the {len(COMMANDS)} commands are not named in any skill body.",
            "",
            "Commands without their own skill:",
            " · ".join(f"`{command}`" for command in without_skill),
            "",
        ]
    )
    updated, count = re.subn(
        r"## Tools without a skill of their own\n.*?(?=## Skill rules)", section, text, flags=re.S
    )
    if count != 1:
        raise ValueError(
            f"could not locate generated skill inventory anchor in {path.relative_to(ROOT)}"
        )
    return updated


def _provenance_content() -> str:
    path = ROOT / "docs" / "legal" / "PROVENANCE.md"
    text = path.read_text(encoding="utf-8")
    text = _replace(
        text,
        r"- \d+ shared handlers exposed through the CLI and `seo_\*` MCP tools;",
        f"- {len(COMMANDS)} shared handlers exposed through the CLI and `seo_*` MCP tools;",
        path,
    )
    return _replace(
        text,
        r"- \d+ audit checks in the crawl registry;",
        f"- {len(CHECKS)} audit checks in the crawl registry;",
        path,
    )


def _replace_check_counts(*, check: bool) -> list[Path]:
    paths = [
        ROOT / "AGENTS.md",
        *sorted((ROOT / ".claude" / "skills").glob("**/*.md")),
        *sorted((ROOT / "docs").glob("**/*.md")),
    ]
    changed = []
    for path in paths:
        if path.name == "CHANGELOG.md":
            continue
        text = path.read_text(encoding="utf-8")
        updated = re.sub(r"\b\d+(?= shared handlers\b)", str(len(COMMANDS)), text)
        updated = re.sub(r"\b\d+(?= handlers\b)", str(len(COMMANDS)), updated)
        updated = re.sub(r"\b\d+(?= core tools\b)", str(len(COMMANDS)), updated)
        updated = re.sub(r"\b\d+(?= seo_\* tools\b)", str(len(COMMANDS)), updated)
        updated = re.sub(r"\b\d+(?=-check\s+(?:registry|analyz|import))", str(len(CHECKS)), updated)
        updated = "".join(
            line
            if "96 checks (the count before" in line or "139 checks when this audit was written" in line
            else re.sub(r"\b\d{2,4}(?=\s+checks\b)", str(len(CHECKS)), line)
            for line in updated.splitlines(keepends=True)
        )
        if _write(path, updated, check=check):
            changed.append(path)
    return changed


def main(argv: list[str] | None = None) -> int:
    check = "--check" in (argv if argv is not None else sys.argv[1:])
    changed = []
    for path, content in (
        (ROOT / "docs" / "TOOLS.md", _tools_content()),
        (ROOT / "docs" / "SKILLS.md", _skills_content()),
        (ROOT / "docs" / "legal" / "PROVENANCE.md", _provenance_content()),
    ):
        if _write(path, content, check=check):
            changed.append(path)
    changed.extend(_replace_check_counts(check=check))
    if not check:
        for path in changed:
            print(f"wrote {path.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
