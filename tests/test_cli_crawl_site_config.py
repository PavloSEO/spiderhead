"""crawl-site's flag surface: --help stays short, --config-help covers the rest.

See issue #27: every subsequent crawler build-out issue was adding a new direct
CLI flag, putting crawl-site on track for 25+ flags and an unreadable --help.
The fix is not fewer settings, it is fewer of them as *flags* — --config carries
the rest, and --config-help makes that config surface discoverable.
"""

import io

import pytest

from seohead import cli
from seohead.crawl import settings as crawl_settings
from seohead.mcp import handlers

# A generous but real ceiling: CI fails if crawl-site --help grows past this,
# which is the point — a new setting from here on must go through --config
# rather than becoming another direct flag.
#
# Raised by one for --urls (issue #21), and the reason is the distinction this
# guard rests on: --urls names where the crawl's input comes from, alongside
# --url, --sitemap and --input, rather than configuring how the crawl behaves.
# Settings still go through --config; if this number moves again for anything
# that is not a new input source, the flag is the thing to reconsider, not the
# ceiling.
#
# Raised again for --set and --max-urls-per-second, and neither is an input
# source, so each owes an argument.
#
# --set is not an exception to #27, it is what makes #27 hold. The fear there was
# "every subsequent crawler build-out issue adding a new direct flag, on track for
# 25+". One flag reaches all thirty-nine settings, so a setting added tomorrow
# needs no CLI change at all -- a one-time cost that removes the pressure
# permanently rather than paying it per setting.
#
# --max-urls-per-second is a safety flag, not a convenience one. A site owner
# states a rate; expressing it as speed.min_delay_seconds makes the operator
# invert it, and a decimal point in the wrong place is somebody's site under
# load. 'sf run' has carried the same flag for the same reason.
#
# -q/--quiet is neither an input source nor a setting: it turns off output this
# command writes to stderr, which no config key can reach because the config
# describes the crawl, not the terminal it runs in. It is the same flag, spelled
# the same way, that 'sf run' has always had -- an operator who pipes one of
# these two commands should not have to learn that the other silences itself
# differently. Its help text is one line for exactly this ceiling's sake.
#
# The rule above still stands for anything else.
#
# --urls-file (#21) is list mode's file-input counterpart to --urls and wraps
# to its own line for the same reason as --max-urls-per-second: the option
# plus its metavar does not fit the terminal width argparse wraps against.
#
# --resume (#619) is an input source, the case the rule above already allows:
# it names where this crawl's input comes from -- an artifact holding the
# frontier, the settings and the start URL -- rather than configuring how the
# crawl behaves. It cannot be a --config setting for the same reason --url
# cannot: --config is one of the things it supplies.
#
# -q/--quiet (#619, progress half) is the one line above that: it turns the crawl's
# stderr off, and that is not a crawler setting either -- it says what this process
# prints, not what it fetches.
#
# Raised by five for --profile and --save-profile (issue #941). A saved profile is
# a named --config file: it selects the input configuration the way --config does,
# so it passes the same test as --config itself. The two flags are one feature --
# select a profile, store one -- and the save side cannot read from --config alone.
# Listing and deleting profiles is not a flag: it waits for its own command rather
# than growing this help again.
HELP_LINE_CEILING = 47


def _help_lines(capsys):
    with pytest.raises(SystemExit):
        cli.main(["crawl-site", "--help"])
    return capsys.readouterr().out.splitlines()


def test_help_output_stays_under_the_line_ceiling(capsys):
    lines = _help_lines(capsys)
    assert len(lines) <= HELP_LINE_CEILING, "\n".join(lines)


def test_help_points_at_config_and_config_help(capsys):
    lines = "\n".join(_help_lines(capsys))
    assert "--config" in lines
    assert "--config-help" in lines


def test_help_advertises_max_depth_and_min_delay(capsys):
    """Legacy crawl settings remain discoverable alongside config help."""
    lines = "\n".join(_help_lines(capsys))
    assert "--max-depth" in lines
    assert "--min-delay" in lines


def test_config_help_lists_every_setting_from_the_config_module(capsys):
    rc = cli.main(["crawl-site", "--config-help"])
    assert rc == 0
    out = capsys.readouterr().out
    for row in crawl_settings.describe_settings():
        assert row["path"] in out, row["path"]


def test_config_help_advertises_the_new_rendering_browser_settings(capsys):
    """#744: a setting that cannot be discovered is a setting that cannot be
    used -- the generic describe_settings() loop would pass on a green helper
    even if these paths were missing."""
    rc = cli.main(["crawl-site", "--config-help"])
    assert rc == 0
    out = capsys.readouterr().out
    for path in (
        "rendering.browser.engine",
        "rendering.browser.viewport_width",
        "rendering.browser.viewport_height",
        "rendering.browser.page_concurrency",
    ):
        assert path in out, path


def test_config_help_does_not_require_a_url_or_read_stdin(monkeypatch, capsys):
    class _NeverReadStdin(io.StringIO):
        def read(self, *a, **k):  # pragma: no cover - defensive guard
            raise AssertionError("--config-help must not touch stdin")

        def isatty(self):
            return False

    monkeypatch.setattr(cli.sys, "stdin", _NeverReadStdin())
    rc = cli.main(["crawl-site", "--config-help"])
    assert rc == 0
    assert capsys.readouterr().out


# ── compatibility flags remain available ───────────────────────────────────


def test_max_depth_and_min_delay_still_work_as_direct_flags(monkeypatch, capsys):
    """Old spellings remain visible and continue to map onto crawl settings.

    Scripts written against crawl-site before this change pass --max-depth and
    --min-delay directly; config files remain preferred for complete profiles.
    """
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO(""))
    monkeypatch.setitem(handlers.HANDLERS, "crawl_site", lambda **kw: {"ok": True, "echo": kw})
    rc = cli.main(
        [
            "crawl-site",
            "--url",
            "https://example.com/",
            "--max-depth",
            "3",
            "--min-delay",
            "1.5",
        ]
    )
    assert rc == 0
    echo = _read_echo(capsys)
    assert echo["max_depth"] == 3
    assert echo["min_delay"] == 1.5


def test_max_urls_out_dir_and_robots_still_work_as_direct_flags(monkeypatch, capsys):
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO(""))
    monkeypatch.setitem(handlers.HANDLERS, "crawl_site", lambda **kw: {"ok": True, "echo": kw})
    rc = cli.main(
        [
            "crawl-site",
            "--url",
            "https://example.com/",
            "--max-urls",
            "50",
            "--out-dir",
            "/tmp/out",
            "--robots",
            "ignore",
        ]
    )
    assert rc == 0
    echo = _read_echo(capsys)
    assert echo["max_urls"] == 50
    assert echo["out_dir"] == "/tmp/out"
    assert echo["robots"] == "ignore"


def _read_echo(capsys):
    import json

    return json.loads(capsys.readouterr().out)["echo"]


# ── effective rate printed at startup (#14 acceptance criteria) ─────────────


def test_the_effective_rate_is_printed_before_the_crawl_runs(monkeypatch, capsys):
    """Politeness is a combination; the derived number must be visible up front."""
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO(""))
    monkeypatch.setitem(handlers.HANDLERS, "crawl_site", lambda **kw: {"ok": True, "echo": kw})
    rc = cli.main(["crawl-site", "--url", "https://example.com/", "--min-delay", "2"])
    assert rc == 0
    err = capsys.readouterr().err
    assert "0.50 req/s" in err


def test_a_zero_delay_prints_as_unbounded_not_a_math_error(monkeypatch, capsys):
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO(""))
    monkeypatch.setitem(handlers.HANDLERS, "crawl_site", lambda **kw: {"ok": True, "echo": kw})
    rc = cli.main(["crawl-site", "--url", "https://example.com/", "--min-delay", "0"])
    assert rc == 0
    assert "unbounded" in capsys.readouterr().err


def test_the_rate_reflects_a_config_file_too_not_only_direct_flags(monkeypatch, capsys, tmp_path):
    import json as _json

    path = tmp_path / "crawl.json"
    path.write_text(_json.dumps({"speed": {"min_delay_seconds": 4}}))
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO(""))
    monkeypatch.setitem(handlers.HANDLERS, "crawl_site", lambda **kw: {"ok": True, "echo": kw})
    rc = cli.main(["crawl-site", "--url", "https://example.com/", "--config", str(path)])
    assert rc == 0
    assert "0.25 req/s" in capsys.readouterr().err


def test_a_config_file_and_set_resolve_the_rendering_browser_settings(
    monkeypatch, capsys, tmp_path
):
    """#744: the file supplies the engine and viewport pair, --set overrides
    the page concurrency -- and what the handler resolves is exactly what a
    render run will use."""
    import json as _json

    from seohead.crawl import settings as crawl_config

    path = tmp_path / "crawl.json"
    path.write_text(
        _json.dumps(
            {
                "rendering": {
                    "browser": {
                        "engine": "webkit",
                        "viewport_width": 1440,
                        "viewport_height": 900,
                        "page_concurrency": 2,
                    }
                }
            }
        )
    )
    received = []
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO(""))
    monkeypatch.setitem(
        handlers.HANDLERS,
        "crawl_site",
        lambda **kw: received.append(kw) or {"ok": True},
    )
    rc = cli.main(
        [
            "crawl-site",
            "--url",
            "https://example.com/",
            "--config",
            str(path),
            "--set",
            "rendering.browser.page_concurrency=4",
        ]
    )
    assert rc == 0
    browser = crawl_config.load(received[0]["config"], overrides=received[0]["overrides"])[
        "rendering"
    ]["browser"]
    assert browser["engine"] == "webkit"
    assert (browser["viewport_width"], browser["viewport_height"]) == (1440, 900)
    assert browser["page_concurrency"] == 4
