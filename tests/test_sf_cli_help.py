"""`seohead sf` must expose the forwarded flags, not a forwarding stub (#1406).

The crawl-audit subsystem owns its argument parser; the top-level CLI forwards
the whole tail to it. That forwarding used to make ``seohead sf --help`` a dead
end, so every documented ``sf`` invocation is cross-checked against the help of
the subcommand it names, using the same documentation extractor CI runs.
"""

from __future__ import annotations

import contextlib
import io
import re
from pathlib import Path

from scripts.doc_commands import extract_commands, to_argv
from seohead.cli import main

ROOT = Path(__file__).resolve().parent.parent

SUBCOMMANDS = ("run", "tasks", "doctor", "save-config", "snippet-edits")


def _sf_help(tail: list[str]) -> str:
    """Run ``seohead sf <tail...> --help`` and capture what argparse prints."""
    out = io.StringIO()
    try:
        with contextlib.redirect_stdout(out):
            main(["sf", *tail, "--help"])
    except SystemExit as exc:  # --help exits through argparse itself
        assert exc.code == 0, out.getvalue()
    return out.getvalue()


def test_bare_sf_help_lists_the_forwarded_subcommands():
    """The former stub showed only a placeholder; the real help names every subcommand."""
    text = _sf_help([])
    for subcommand in SUBCOMMANDS:
        assert subcommand in text


def test_documented_sf_flags_appear_in_the_subcommand_help():
    """Every flag the documentation uses must be visible in that subcommand's help."""
    documented = [to_argv(command.raw) for command in extract_commands(ROOT)]
    sf_calls = [argv for argv in documented if argv[:1] == ["sf"]]
    assert sf_calls, "the documentation corpus no longer contains any `seohead sf` examples"

    help_by_subcommand: dict[str, str] = {}
    flag_re = re.compile(r"^-{1,2}[A-Za-z]")
    for argv in sf_calls:
        subcommand = argv[1]
        assert subcommand in SUBCOMMANDS, f"undocumented sf subcommand: {subcommand}"
        if subcommand not in help_by_subcommand:
            help_by_subcommand[subcommand] = _sf_help([subcommand])
        used = {token for token in argv[2:] if flag_re.match(token)}
        missing = {flag for flag in used if flag not in help_by_subcommand[subcommand]}
        assert not missing, f"`{subcommand}` help hides flags the documentation uses: {missing}"
