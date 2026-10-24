"""``crawl-site --resume``: what it reads from the artifact, and what it refuses by name.

Issue #619. ``NativeScan`` already carried the frontier, the throttle state and
the settings a resume needs, and ``crawl_to_scan`` already continued an existing
file -- but only for a caller that reproduced the original settings exactly, and
no interface offered that. These tests pin the surface that closes the gap: the
start URL and configuration come from the file, and a scan this run must not
continue is refused before a single request leaves the machine.
"""

from __future__ import annotations

import json

import pytest

from seohead import cli
from seohead.crawl.settings import fingerprint
from seohead.crawl.settings import load as load_config
from seohead.mcp import handlers
from seohead.mcp.scan_handlers import resume_inputs, resume_scan
from seohead.storage.native_scan import NativeScan

BUILD = "a" * 40
OTHER_BUILD = "b" * 40
START = "https://example.test/"


def _scan(path, *, config=None, start_url=START, writer_revision=BUILD):
    config = config or load_config(overrides={"speed.min_delay_seconds": 0})
    return NativeScan.create(
        path,
        start_url=start_url,
        config=config,
        config_fingerprint=fingerprint(config),
        writer_version="3.0.0",
        writer_revision=writer_revision,
        runtime_versions={
            "python": "test",
            "sqlite": "test",
            "httpx": "test",
            "lxml": "test",
            "beautifulsoup4": "test",
        },
        limitations=["browser-network response capture is unavailable"],
    )


def _interrupted(path, **metadata):
    """An artifact in exactly the state a killed crawl leaves behind: work still queued."""
    start_url = metadata.get("start_url", START)
    with _scan(path, **metadata) as scan:
        scan.seed_frontier(
            [
                {
                    "requested_url": start_url,
                    "frontier_url": start_url,
                    "depth": 0,
                    "reason": "",
                    "source": "start",
                    "reserve_query": False,
                    "seed": False,
                }
            ]
        )
        scan.interrupt("killed")
    return str(path)


def _finished(path, **metadata):
    """The opposite state: a crawl that ran out of frontier and was finalized."""
    with _scan(path, **metadata) as scan:
        assert scan.finish_without_audit() is True
    return str(path)


def _never_crawls(monkeypatch):
    monkeypatch.setattr(
        "seohead.mcp.scan_handlers.crawl_site_scan",
        lambda *args, **kwargs: pytest.fail("a refused resume must not start a crawl"),
    )


def test_resume_reads_the_start_url_and_settings_back_from_the_artifact(tmp_path):
    config = load_config(overrides={"speed.min_delay_seconds": 0, "limits.max_urls": 37})
    inputs = resume_inputs(_interrupted(tmp_path / "scan.sqlite", config=config))

    assert inputs["start_url"] == START
    assert inputs["writer_revision"] == BUILD
    # Read back, not restated: the limit the stored frontier was built under.
    assert inputs["settings"]["limits"]["max_urls"] == 37
    assert fingerprint(inputs["settings"]) == fingerprint(config)


def test_resume_refuses_a_scan_written_by_a_different_producing_build(tmp_path, monkeypatch):
    _never_crawls(monkeypatch)
    scan = _interrupted(tmp_path / "scan.sqlite", writer_revision=OTHER_BUILD)

    with pytest.raises(ValueError) as raised:
        resume_scan(scan, producer_build=BUILD)

    message = str(raised.value)
    assert "mixed-build resume" in message
    assert OTHER_BUILD in message and BUILD in message


def test_resume_refuses_a_scan_crawled_from_a_different_start_url(tmp_path, monkeypatch):
    _never_crawls(monkeypatch)
    scan = _interrupted(tmp_path / "scan.sqlite")

    with pytest.raises(ValueError) as raised:
        resume_scan(scan, url="https://other.test/", producer_build=BUILD)

    message = str(raised.value)
    assert "refusing to resume one crawl as another" in message
    # Both URLs must be named, and named whole: the refusal is only useful if the
    # operator can read which crawl the artifact holds and which one they asked for.
    named = set(message.translate(str.maketrans(",;", "  ")).split())
    assert {START, "https://other.test/"} <= named


def test_resume_refuses_a_scan_that_already_finished(tmp_path, monkeypatch):
    _never_crawls(monkeypatch)
    scan = _finished(tmp_path / "scan.sqlite")

    with pytest.raises(ValueError, match="already finished"):
        resume_scan(scan, producer_build=BUILD)


def test_resume_refuses_a_file_that_is_not_a_scan(tmp_path, monkeypatch):
    _never_crawls(monkeypatch)
    foreign = tmp_path / "notes.txt"
    foreign.write_text("not a scan", encoding="utf-8")

    with pytest.raises(Exception, match="cannot inspect native scan"):
        resume_scan(str(foreign), producer_build=BUILD)

    with pytest.raises(ValueError, match="does not exist"):
        resume_scan(str(tmp_path / "absent.sqlite"), producer_build=BUILD)


def test_resume_refuses_a_credentialed_scan_it_cannot_reconstruct(tmp_path, monkeypatch):
    """Redaction is why the stored settings are not always the settings that ran."""
    _never_crawls(monkeypatch)
    monkeypatch.setenv("SEOHEAD_RESUME_FLAG_TOKEN", "Bearer local-test-secret")
    config = load_config(
        overrides={
            "speed.min_delay_seconds": 0,
            "http.credential_headers": [
                {
                    "host": "example.test",
                    "headers": {"Authorization": "env:SEOHEAD_RESUME_FLAG_TOKEN"},
                }
            ],
            "http.credentials_acknowledged": True,
        }
    )
    scan = _interrupted(tmp_path / "scan.sqlite", config=config)

    with pytest.raises(ValueError) as raised:
        resume_scan(scan, producer_build=BUILD)

    message = str(raised.value)
    assert "redacted" in message
    # An artifact that cannot be resumed this way still names the route that works.
    assert "--config" in message and "--scan-out" in message
    # And the reason it cannot be reconstructed: the artifact kept the header's name,
    # never the environment reference that resolves to its value.
    assert "local-test-secret" not in message
    stored = json.loads(NativeScan.inspect(scan)["scan"]["config_json"])
    assert stored["http"]["credential_headers"][0]["headers"]["Authorization"] == "REDACTED"


@pytest.mark.parametrize(
    "argument,value",
    [
        ("max_urls", 10),
        ("config", "crawl.json"),
        ("sitemap", "https://example.test/sitemap.xml"),
        ("out_dir", "run"),
        ("urls", ["https://example.test/one"]),
        ("overrides", {"limits.max_urls": 50}),
        # Stated-as-zero, not absent: a falsy check here would let a resume run
        # under a delay it never applies.
        ("min_delay", 0),
        ("max_depth", 0),
    ],
)
def test_resume_refuses_to_be_combined_with_a_crawl_shaping_argument(argument, value, tmp_path):
    scan = _interrupted(tmp_path / "scan.sqlite")

    with pytest.raises(ValueError) as raised:
        handlers.crawl_site(resume=scan, producer_build=BUILD, **{argument: value})

    assert argument in str(raised.value)


def test_resume_applies_politeness_overrides_as_a_live_rate_change(tmp_path, monkeypatch):
    """Politeness decides how hard the site is hit, not what the crawl collects.

    The two speed keys are applied as a live rate change on resume (see
    crawl_site); every other override still conflicts, as the parameterized
    refusal test above asserts.
    """
    scan = _interrupted(tmp_path / "scan.sqlite")
    seen = {}
    monkeypatch.setattr(
        "seohead.mcp.scan_handlers.crawl_site_scan",
        lambda url, **kwargs: seen.update(url=url, **kwargs) or {"ok": True},
    )

    result = handlers.crawl_site(
        resume=scan,
        scan_out=scan,
        producer_build=BUILD,
        overrides={"speed.min_delay_seconds": 2, "speed.concurrency": 2},
    )

    assert result["ok"] is True
    assert seen["url"] == START


def test_resume_accepts_the_same_scan_out_path_it_was_given(tmp_path, monkeypatch):
    scan = _interrupted(tmp_path / "scan.sqlite")
    seen = {}
    monkeypatch.setattr(
        "seohead.mcp.scan_handlers.crawl_site_scan",
        lambda url, **kwargs: seen.update(url=url, **kwargs) or {"ok": True},
    )

    result = handlers.crawl_site(resume=scan, scan_out=scan, producer_build=BUILD)
    assert result["ok"] is True
    assert "effective_max_requests_per_second" in result
    assert seen["url"] == START
    assert seen["scan_out"] == scan
    assert seen["producer_build"] == BUILD


def test_cli_resume_reaches_the_handler_without_a_url_or_stdin(tmp_path, monkeypatch, capsys):
    scan = _interrupted(tmp_path / "scan.sqlite")
    seen = {}
    monkeypatch.setitem(
        handlers.HANDLERS,
        "crawl_site",
        lambda **kwargs: (
            seen.update(kwargs)
            or {"scan": scan, "urls_collected": 4, "partial": False, "finish_reason": "finished"}
        ),
    )

    assert cli.main(["crawl-site", "--resume", scan]) == 0

    # The progress line rides along because a resumed crawl is exactly the long run
    # worth watching; nothing else does, and no --url was invented from the artifact.
    assert seen.pop("progress") is not None
    assert seen == {"resume": scan}
    # The pre-run rate line describes flags a resume refuses, so it is not printed;
    # the frontier line is, because loading a stored frontier is the pause worth
    # explaining; the outcome line is, because that is the question a long run
    # leaves open.
    err = capsys.readouterr().err
    assert "worst-case request rate" not in err
    assert err.splitlines() == [
        "seohead: crawl-site",
        f"resuming {scan} (0 MB): loading the stored frontier...",
        "crawl-site: finished; 4 URLs fetched",
    ]


def test_cli_says_a_crawl_stopped_early_and_how_to_continue_it(tmp_path, monkeypatch, capsys):
    scan = str(tmp_path / "scan.sqlite")
    monkeypatch.setitem(
        handlers.HANDLERS,
        "crawl_site",
        lambda **_kwargs: {
            "scan": scan,
            "urls_collected": 2,
            "partial": True,
            "finish_reason": "errors",
            "stopped_reason": "origin stopped responding or refused repeatedly",
        },
    )

    assert cli.main(["crawl-site", "--url", "https://example.test/", "--scan-out", scan]) == 0

    err = capsys.readouterr().err
    assert "crawl-site: stopped early (origin stopped responding" in err
    assert "2 URLs fetched" in err
    assert f"seohead crawl-site --resume {scan}" in err


@pytest.mark.parametrize(
    "finish_reason,setting",
    [("url_limit", "limits.max_urls"), ("duration_limit", "limits.max_crawl_seconds")],
)
def test_cli_does_not_offer_a_resume_that_cannot_pass_the_budget_that_stopped_it(
    finish_reason, setting, tmp_path, monkeypatch, capsys
):
    """A resume reads its limits back from the artifact, so it cannot get past them."""
    scan = str(tmp_path / "scan.sqlite")
    monkeypatch.setitem(
        handlers.HANDLERS,
        "crawl_site",
        lambda **_kwargs: {
            "scan": scan,
            "urls_collected": 2,
            "partial": True,
            "finish_reason": finish_reason,
            "stopped_reason": "limit reached",
        },
    )

    assert cli.main(["crawl-site", "--url", "https://example.test/", "--scan-out", scan]) == 0

    err = capsys.readouterr().err
    assert "stopped early (limit reached); 2 URLs fetched" in err
    assert setting in err
    assert "--resume" not in err


def test_cli_marks_a_finished_run_that_continued_an_earlier_one(tmp_path, monkeypatch, capsys):
    scan = _interrupted(tmp_path / "scan.sqlite")
    monkeypatch.setitem(
        handlers.HANDLERS,
        "crawl_site",
        lambda **_kwargs: {
            "scan": scan,
            "urls_collected": 7,
            "partial": False,
            "finish_reason": "finished",
            "resumed": True,
        },
    )

    assert cli.main(["crawl-site", "--resume", scan]) == 0

    assert "continued an earlier one" in capsys.readouterr().err
