"""Public documentation must become noisy when product registries drift."""

from __future__ import annotations

import ast
import pathlib
import re
import tokenize

from scripts.build_changelog import fragment_paths
from seohead.cli import COMMANDS, DOCUMENTED_CLI_ENTRYPOINTS, URL_COMMANDS
from seohead.core.provider_matrix import render as render_provider_matrix
from seohead.mcp.handlers import HANDLERS
from seohead.mcp.tool_reference import load_semantics_tools, load_seo_tools, load_sf_tools
from seohead.mcp.tool_reference import render as render_tool_reference
from seohead.sf.core.checks_reference import render as render_checks_reference
from seohead.sf.core.registry import CHECKS

ROOT = pathlib.Path(__file__).resolve().parent.parent
TECHNICAL_SKILLS = sorted((ROOT / ".claude" / "skills").glob("*/SKILL.md"))
# A skill directory may carry sub-skills and a reference archive beside its SKILL.md (the
# controller does). They are not skills in their own right — nothing loads them by name — but
# they are public Markdown, so the English-only gate applies to them like anything else.
SKILL_SUPPORTING_MARKDOWN = sorted(
    p for p in (ROOT / ".claude" / "skills").glob("*/**/*.md") if p.name != "SKILL.md"
)
PACKAGED_SKILLS = sorted((ROOT / "seohead" / "skills").glob("*/SKILL.md"))
# Every level: docs/scenarios/ is part of the public contract too, so the English-only
# gate and the count checks apply to it like anything else under docs/.
# docs/examples/ holds generated sample output and fixtures, not prose documentation.
DOCS = sorted(
    path
    for path in (ROOT / "docs").glob("**/*.md")
    if path.relative_to(ROOT / "docs").parts[0] != "examples"
)
# One file per change, folded into CHANGELOG.md at release time (#638). A fragment is a
# changelog entry that has not been assembled yet, so every gate below that reads the changelog
# has to read these too -- otherwise moving entries out of CHANGELOG.md would quietly move them
# out of the English-only, command-name and stale-count gates with them.
CHANGELOG_FRAGMENTS = fragment_paths(ROOT / "changelog.d")
PUBLIC_MARKDOWN = [
    ROOT / "README.md",
    ROOT / "AGENTS.md",
    ROOT / "CHANGELOG.md",
    *CHANGELOG_FRAGMENTS,
    ROOT / "CODE_OF_CONDUCT.md",
    ROOT / "CONTRIBUTING.md",
    ROOT / "docs" / "legal" / "PROVENANCE.md",
    ROOT / "SECURITY.md",
    ROOT / "THIRD_PARTY_NOTICES.md",
    ROOT / "docs" / "legal" / "TRADEMARKS.md",
    *TECHNICAL_SKILLS,
    *SKILL_SUPPORTING_MARKDOWN,
    *PACKAGED_SKILLS,
    *DOCS,
]
PUBLIC_PYTHON = sorted((ROOT / "seohead").glob("**/*.py")) + sorted(
    (ROOT / "tests").glob("**/*.py")
)

EXTRA_COMMANDS = set(DOCUMENTED_CLI_ENTRYPOINTS)
# This literal Cyrillic range intentionally detects non-English public prose.
CYRILLIC = re.compile(r"[А-Яа-яЁё]")  # noqa: RUF001
ALLOWED_LOCALIZED_MARKDOWN = {
    ROOT / "seohead" / "skills" / "article-writer" / "SKILL.md": (
        "N минут чтения",
        "5 минут чтения",
    ),
}


def _records_a_moment_in_time(path: pathlib.Path) -> bool:
    """Whether a document states what was true when it was written rather than what is true now.

    A changelog entry does, so the live-count gates must not hold its numbers to today's
    registry -- and that is equally true of a `changelog.d/` fragment, which is the same entry
    before it has been folded in.
    """
    return path.name == "CHANGELOG.md" or path.parent.name == "changelog.d"


def _sf_tool_names() -> set[str]:
    source = (ROOT / "seohead" / "mcp" / "sf_mcp.py").read_text(encoding="utf-8")
    return set(re.findall(r"def (sf_[a-z0-9_]+)\(", source))


def test_every_command_has_a_handler():
    missing = sorted(command for command in COMMANDS if command.replace("-", "_") not in HANDLERS)
    assert not missing, f"CLI commands without handlers: {missing}"


def test_every_handler_is_reachable_from_the_cli():
    exposed = {command.replace("-", "_") for command in COMMANDS}
    orphaned = sorted(set(HANDLERS) - exposed)
    assert not orphaned, f"handlers unavailable through the CLI: {orphaned}"


def test_url_commands_are_registered_commands():
    unknown = sorted(set(URL_COMMANDS) - set(COMMANDS))
    assert not unknown, f"unknown entries in URL_COMMANDS: {unknown}"


def test_skill_name_matches_its_folder():
    bad = []
    for path in [*TECHNICAL_SKILLS, *PACKAGED_SKILLS]:
        match = re.search(r"^name:\s*(\S+)", path.read_text(encoding="utf-8"), re.M)
        if not match or match.group(1) != path.parent.name:
            bad.append(str(path.relative_to(ROOT)))
    assert not bad, f"skill name/folder mismatches: {bad}"


def test_every_skill_has_a_description():
    bad = [
        str(path.relative_to(ROOT))
        for path in [*TECHNICAL_SKILLS, *PACKAGED_SKILLS]
        if not re.search(r"^description:", path.read_text(encoding="utf-8"), re.M)
    ]
    assert not bad, f"skills without a discoverable description: {bad}"


def test_skills_do_not_require_gnu_only_grep():
    """The toolkit targets macOS and Linux both; a skill's shell snippet must not require
    GNU grep's `-P` (Perl regex), which macOS's stock BSD grep rejects with exit 2 (#256)."""
    bad = []
    for path in [*TECHNICAL_SKILLS, *PACKAGED_SKILLS]:
        text = path.read_text(encoding="utf-8")
        for match in re.finditer(r"grep\s+(?:-\w*\s+)*-\w*P\w*", text):
            bad.append(f"{path.relative_to(ROOT)}: {match.group(0)!r}")
    assert not bad, f"GNU-only `grep -P` in a public skill: {bad}"


def test_only_control_claims_the_unscoped_audit_entry_point():
    """`control` and `seo-deep-audit` used to both claim ownership of a bare, unscoped
    "audit this site" request (#258). Only one router may claim that job; the other must
    defer to it by name rather than presenting a second competing entry point."""
    control_text = (ROOT / ".claude" / "skills" / "control" / "SKILL.md").read_text(
        encoding="utf-8"
    )
    deep_audit_text = (ROOT / ".claude" / "skills" / "seo-deep-audit" / "SKILL.md").read_text(
        encoding="utf-8"
    )
    assert "entry point" in control_text.lower()
    # The old frontmatter opened with "SINGLE ENTRY POINT and orchestrator ... for a complete
    # domain SEO audit", claiming the same unscoped request `control` claims above. It may say
    # that of `control` (a deferral), just never claim it of itself.
    assert not re.search(r"seo-deep-audit.{0,80}single entry point", deep_audit_text, re.I | re.S)
    assert not re.search(r"single entry point.{0,80}orchestrator", deep_audit_text, re.I | re.S)
    assert re.search(r"`control`.{0,200}(entry point|delegat)", deep_audit_text, re.I | re.S), (
        "seo-deep-audit must defer to control by name instead of claiming the unscoped "
        "audit request for itself"
    )


def test_skills_and_docs_reference_only_existing_commands():
    known = set(COMMANDS) | EXTRA_COMMANDS
    bad: list[str] = []
    for path in PUBLIC_MARKDOWN:
        text = path.read_text(encoding="utf-8")
        for used in sorted(set(re.findall(r"(?<![.\w/-])seohead\s+([a-z0-9][a-z0-9-]+)", text))):
            if used not in known:
                bad.append(f"{path.relative_to(ROOT)}: seohead {used}")
    assert not bad, "references to non-existent commands: " + "; ".join(bad)


def _commands_without_a_dedicated_skill() -> set[str]:
    """Commands not named (as `` `cmd` `` or ``seohead cmd``) in any skill file's own body.

    docs/SKILLS.md's per-skill tables are a hand-curated summary; whether a skill's
    write-up actually names the command is the mechanical fact this recomputes, so the
    "N of the 60 commands have no skill of their own" line in docs/SKILLS.md cannot drift
    the way it did before (issue #22: it said "Twenty-one of the 42").
    """
    skill_files = [*TECHNICAL_SKILLS, *PACKAGED_SKILLS]
    mentioned: set[str] = set()
    for path in skill_files:
        text = path.read_text(encoding="utf-8")
        for command in COMMANDS:
            if re.search(rf"`{re.escape(command)}`", text) or re.search(
                rf"seohead\s+{re.escape(command)}\b", text
            ):
                mentioned.add(command)
    return set(COMMANDS) - mentioned


def test_skills_map_command_coverage_is_current():
    without_own_skill = _commands_without_a_dedicated_skill()
    text = (ROOT / "docs" / "SKILLS.md").read_text(encoding="utf-8")
    assert f"{len(without_own_skill)} of the {len(COMMANDS)} commands" in text, (
        "docs/SKILLS.md's command-coverage count is stale, expected: "
        f"{len(without_own_skill)} of the {len(COMMANDS)}"
    )
    section = text.split("## Tools without a skill of their own", 1)[1]
    section = section.split("## Skill rules", 1)[0]
    listed = set(re.findall(r"`([a-z0-9][a-z0-9-]+)`", section)) & set(COMMANDS)
    assert listed == without_own_skill, (
        f"docs/SKILLS.md's command list drifted: missing {without_own_skill - listed}, "
        f"stale {listed - without_own_skill}"
    )


def test_documented_product_counts_match_the_registries():
    provenance = (ROOT / "docs" / "legal" / "PROVENANCE.md").read_text(encoding="utf-8")
    assert len(COMMANDS) == len(HANDLERS)
    assert len(_sf_tool_names()) == 6
    assert CHECKS
    assert len(TECHNICAL_SKILLS) == 26
    assert len(PACKAGED_SKILLS) == 8
    assert (
        str(len(COMMANDS)) in provenance
        and str(len(CHECKS)) in provenance
        and "six" in provenance.lower()
    )
    assert (ROOT / "CITATION.cff").is_file()


def test_stale_tool_counts_do_not_reappear():
    """Each of these substrings was found stale in a different file while fixing issue #22;
    each of these substrings was found stale in a different file while fixing issue #22
    and is pinned here so the next rename cannot silently reintroduce one of them."""
    combined = "\n".join(path.read_text(encoding="utf-8") for path in PUBLIC_MARKDOWN)
    for stale in (
        "42 core",
        "42 handlers",
        "42 seo_",
        "(44 + 5)",
        "reference for all 49",
        "Twenty-one of the 42",
        "all 42",
    ):
        assert stale not in combined, f"stale tool count reappeared: {stale!r}"
    assert not re.search(r"(?<!\d)44 \+ 5(?!\d)", combined), "stale tool count reappeared: '44 + 5'"


def test_public_markdown_is_english():
    bad = []
    for path in PUBLIC_MARKDOWN:
        text = path.read_text(encoding="utf-8")
        for allowed in ALLOWED_LOCALIZED_MARKDOWN.get(path, ()):
            text = text.replace(allowed, "")
        if CYRILLIC.search(text):
            bad.append(str(path.relative_to(ROOT)))
    assert not bad, f"Cyrillic prose remains in public Markdown: {bad}"


def test_python_comments_and_docstrings_are_english():
    """Allow localized test/data values while keeping explanatory prose English."""
    bad: list[str] = []
    for path in PUBLIC_PYTHON:
        with path.open("rb") as handle:
            for token in tokenize.tokenize(handle.readline):
                if token.type == tokenize.COMMENT and CYRILLIC.search(token.string):
                    bad.append(f"{path.relative_to(ROOT)}:{token.start[0]} comment")

        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        for node in ast.walk(tree):
            if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
                docstring = ast.get_docstring(node, clean=False)
                if docstring and CYRILLIC.search(docstring):
                    bad.append(f"{path.relative_to(ROOT)}:{getattr(node, 'lineno', 1)} docstring")

    assert not bad, "Cyrillic remains in comments/docstrings: " + "; ".join(bad)


def test_checks_reference_is_generated_and_current():
    """docs/CHECKS.md is generated output (scripts/generate_checks_reference.py); a hand
    edit or a registry change without regenerating it must fail here rather than ship as
    stale prose. This is the check catalogue the issue calls out as having no coverage."""
    committed = (ROOT / "docs" / "CHECKS.md").read_text(encoding="utf-8")
    assert committed == render_checks_reference(), (
        "docs/CHECKS.md is stale: run scripts/generate_checks_reference.py and commit the result"
    )
    documented = set(re.findall(r"^\| `([A-Z0-9_]+)`", committed, re.M))
    assert documented == set(CHECKS), "every check must appear in the generated reference"


def test_every_cli_command_is_documented_in_tools_reference():
    tools_doc = (ROOT / "docs" / "TOOLS.md").read_text(encoding="utf-8")
    missing = sorted(command for command in COMMANDS if f"`{command}`" not in tools_doc)
    assert not missing, f"commands missing from docs/TOOLS.md: {missing}"


def test_every_cli_command_is_grouped_in_the_cli_overview():
    """docs/CLI.md is the by-job map the README links to; a new command must land in a group."""
    overview = (ROOT / "docs" / "CLI.md").read_text(encoding="utf-8")
    grouped = set(re.findall(r"^\| `([a-z0-9-]+)` \|", overview, re.M))
    missing = sorted(set(COMMANDS) - grouped)
    stale = sorted(grouped - set(COMMANDS) - EXTRA_COMMANDS)
    assert not missing, f"commands missing from docs/CLI.md: {missing}"
    assert not stale, f"docs/CLI.md names unregistered commands: {stale}"


def test_severity_breakdown_in_tools_reference_matches_the_registry():
    from collections import Counter

    counts = Counter(meta["severity"] for meta in CHECKS.values())
    tools_doc = (ROOT / "docs" / "TOOLS.md").read_text(encoding="utf-8")
    expected = (
        f"{counts['critical']} critical, {counts['warning']} warnings, {counts['notice']} notices"
    )
    assert expected in tools_doc, f"docs/TOOLS.md severity breakdown is stale, expected: {expected}"


def test_mcp_tool_count_in_tools_reference_matches_the_registry():
    """The prose count near the bottom of docs/TOOLS.md ('N + 5') is exactly the kind of
    number that rotted silently before (it read '44 + 5' while there were 45 seo_* tools).
    """
    tools_doc = (ROOT / "docs" / "TOOLS.md").read_text(encoding="utf-8")
    expected = f"{len(load_seo_tools())} + {len(load_sf_tools())}"
    assert expected in tools_doc, f"docs/TOOLS.md MCP tool count is stale, expected: {expected}"


def test_tool_reference_is_generated_and_current():
    """docs/TOOL_REFERENCE.md is generated output (scripts/generate_tool_reference.py); a
    hand edit or an MCP tool signature/docstring change without regenerating it must fail
    here rather than ship as stale prose. This is the per-tool reference the issue calls
    out as missing: arguments, types, defaults, cost, and failure modes, generated from
    the same tool definitions the MCP server itself exposes."""
    committed = (ROOT / "docs" / "TOOL_REFERENCE.md").read_text(encoding="utf-8")
    assert committed == render_tool_reference(), (
        "docs/TOOL_REFERENCE.md is stale: run scripts/generate_tool_reference.py "
        "and commit the result"
    )
    documented = set(re.findall(r"^### `([a-z0-9_-]+)`", committed, re.M))
    seo_tools = load_seo_tools()
    sf_tools = load_sf_tools()
    semantics_tools = load_semantics_tools()
    expected = {tool.command or tool.name for tool in (*seo_tools, *sf_tools, *semantics_tools)}
    assert documented == expected, "every MCP tool must appear in the generated reference"
    assert len(seo_tools) == len(COMMANDS)
    assert len(sf_tools) == 6


def test_provider_matrix_is_generated_and_current():
    """docs/PROVIDERS.md is generated output (scripts/generate_provider_matrix.py); a hand
    edit or a registry/workflow-catalogue change without regenerating it must fail here
    rather than ship stale capability claims."""
    committed = (ROOT / "docs" / "PROVIDERS.md").read_text(encoding="utf-8")
    assert committed == render_provider_matrix(), (
        "docs/PROVIDERS.md is stale: run scripts/generate_provider_matrix.py and commit the result"
    )


def test_storage_late_field_table_matches_the_importer_mapping():
    from seohead.storage import _LATE_PAGE_FIELDS

    storage = (ROOT / "docs" / "STORAGE.md").read_text(encoding="utf-8")
    section = storage.split("| PageRecord field | `pages` column |", 1)[1].split(
        "\n\n`NULL` means", 1
    )[0]
    rows = re.findall(r"\| `([^`]+)` \| `([^`]+)` \|", section)
    assert len(rows) == len(_LATE_PAGE_FIELDS), "missing or duplicated field rows"
    assert dict(rows) == _LATE_PAGE_FIELDS


def test_private_research_journal_is_not_part_of_the_snapshot():
    assert not (ROOT / "maybe").exists()
    combined = "\n".join(path.read_text(encoding="utf-8") for path in PUBLIC_MARKDOWN)
    assert "review of 85" not in combined.lower()
    assert "private development repository is stronger" not in combined.lower()


def test_hyphenated_check_count_claims_match_the_registry():
    """tests/test_doc_counts.py's live scanner requires whitespace between a number and
    "checks", so a hyphenated adjective ("104-check registry", "132-check analyzer") drifted
    silently past it in README.md, AGENTS.md and several skills (#259) while the number-space
    form elsewhere in the same files was kept current. Skills are covered here too, since
    test_doc_counts.py's own file list stops at the top level and docs/."""
    bad = []
    pattern = re.compile(r"(\d+)-check\s+(?:registry|analyz|import)")
    for path in PUBLIC_MARKDOWN:
        if _records_a_moment_in_time(path):
            continue  # a changelog records what was true at the time, like test_doc_counts.py
        for match in pattern.finditer(path.read_text(encoding="utf-8")):
            if int(match.group(1)) != len(CHECKS):
                bad.append(f"{path.relative_to(ROOT)}: {match.group(0)!r}")
    assert not bad, f"stale hyphenated check-count claims (live count is {len(CHECKS)}): {bad}"


_NUMBER_WORDS = {
    "one": 1,
    "two": 2,
    "three": 3,
    "four": 4,
    "five": 5,
    "six": 6,
    "seven": 7,
    "eight": 8,
    "nine": 9,
    "ten": 10,
    "eleven": 11,
    "twelve": 12,
    "thirteen": 13,
    "fourteen": 14,
    "fifteen": 15,
    "twenty": 20,
    "fifty-six": 56,
    "fifty six": 56,
}


def test_scenario_chain_count_claims_match_the_catalogue():
    """`control`'s own subskills said "ten chains" long after the scenario catalogue grew to
    56 (#259) — a claim that lives in `.claude/skills/control/subskills/`, outside every file
    test_doc_counts.py's live scanner walks, so nothing caught it drifting. Spelled-out numbers
    ("ten") are checked here too, since that is exactly the form the stale claim used."""
    scenario_count = len(
        [p for p in (ROOT / "docs" / "scenarios").glob("*.md") if p.name != "README.md"]
    )
    number = r"\d+|" + "|".join(sorted(_NUMBER_WORDS, key=len, reverse=True))
    pattern = re.compile(rf"\b({number})\s+(?:chains|scenarios)\b", re.I)
    bad = []
    for path in PUBLIC_MARKDOWN:
        if _records_a_moment_in_time(path):
            continue
        for match in pattern.finditer(path.read_text(encoding="utf-8")):
            raw = match.group(1).lower()
            value = int(raw) if raw.isdigit() else _NUMBER_WORDS[raw]
            if value != scenario_count:
                bad.append(f"{path.relative_to(ROOT)}: {match.group(0)!r}")
    assert not bad, f"stale scenario-count claims (live count is {scenario_count}): {bad}"


def test_canonical_basics_scenario_cross_references_do_not_embed_a_stale_number():
    """canonical-basics.md linked to rendering.md and canonical-conflicts.md as "scenario 4"
    and "scenario 12" — numbers that were true before the catalogue was renumbered/expanded;
    rendering.md's own heading is "Scenario 35" (#259). Descriptive link text sidesteps this
    whole class of drift, so the fix removes the embedded numeral rather than re-syncing it."""
    text = (ROOT / "docs" / "scenarios" / "canonical-basics.md").read_text(encoding="utf-8")
    assert "](rendering.md)" in text
    assert "](canonical-conflicts.md)" in text
    assert not re.search(r"scenario \d+\]\((rendering|canonical-conflicts)\.md\)", text, re.I)


def test_documented_clone_directory_matches_a_default_clone():
    """A default `git clone <url>` creates a directory named after the URL's basename, not
    whatever the repository used to be called. README.md and docs/SETUP.md must `cd` into
    that real name, or a fresh install fails on the very first command (#259)."""
    for path in (ROOT / "README.md", ROOT / "docs" / "SETUP.md"):
        text = path.read_text(encoding="utf-8")
        match = re.search(r"git clone (\S+?)(?:\.git)?\s*\n\s*cd (\S+)", text)
        assert match, f"{path.relative_to(ROOT)}: no git-clone-then-cd sequence found to check"
        url, cd_target = match.groups()
        expected_dir = url.rstrip("/").rsplit("/", 1)[-1].removesuffix(".git")
        assert cd_target == expected_dir, (
            f"{path.relative_to(ROOT)}: `git clone {url}` makes `{expected_dir}/`, "
            f"but the next line does `cd {cd_target}`"
        )
