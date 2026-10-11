"""Live crawl progress (#619): the numbers on screen must be the crawl's real ones.

A progress line is only worth printing if it can be believed. The load-bearing
test here crawls the offline fixture site into a SQLite scan and, at every point
the crawler reports progress, opens a *separate* read-only connection to the
artifact and counts the rows it actually holds. A reported count that came from
a parallel in-memory tally rather than the artifact would drift from that and
fail here.

The rest guards the other half of believing it: percentage and ETA cover only
the frontier currently known, not the crawl's eventual workload; an unmeasured rate must
not read as a stalled crawl, ``-q`` must be silent, and a non-TTY must get whole
lines rather than carriage returns.
"""

from __future__ import annotations

import io
import socket
import sqlite3

import pytest

from seohead.crawl.progress import CrawlProgress
from seohead.crawl.settings import load
from seohead.crawl.sqlite_adapter import crawl_to_scan


class _Response:
    def __init__(self, status_code, text="", headers=None):
        self.status_code = status_code
        self.text = text
        self.content = text.encode("utf-8")
        self.headers = headers or {"content-type": "text/html; charset=utf-8"}


def _page(title: str, *links: str) -> _Response:
    body = "".join(f'<a href="{href}">{href}</a>' for href in links)
    return _Response(
        200, f"<html><head><title>{title}</title></head><body><h1>{title}</h1>{body}</body></html>"
    )


# Six fetchable URLs discovered over three link hops, so a crawl of it reports
# progress several times with a frontier that is still growing at each one --
# plus one link robots disallows. That last one is the whole point: a queued URL
# the crawl never fetches is what left a finished run reporting 87%, and a
# fixture where nothing is ever excluded cannot fail the assertions below.
FIXTURE_SITE = {
    "https://example.test/robots.txt": _Response(
        200, "User-agent: *\nDisallow: /private/\n", {"content-type": "text/plain"}
    ),
    "https://example.test/": _page("Home", "/a", "/b", "/private/page"),
    "https://example.test/a": _page("A", "/a1", "/a2"),
    "https://example.test/b": _page("B", "/b1"),
    "https://example.test/a1": _page("A1"),
    "https://example.test/a2": _page("A2"),
    "https://example.test/b1": _page("B1"),
    "https://example.test/private/page": _page("Private"),
}


def _fetcher(responses):
    def fetch(url):
        return responses.get(url, _Response(404, ""))

    return fetch


def _runtime_versions():
    return dict.fromkeys(("python", "sqlite", "httpx", "lxml", "beautifulsoup4"), "test")


def _pages_in_artifact(path) -> int:
    """Count the pages the scan file holds, through a connection the crawl does not own.

    Read-only and separate on purpose: asking the writer's own connection what
    it has written would prove nothing about the artifact an operator is
    watching grow on disk.
    """
    con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
    finally:
        con.close()


def _crawl(tmp_path, progress, **overrides):
    settings = load(overrides={"speed.min_delay_seconds": 0, "limits.max_urls": 50, **overrides})
    scan_path = tmp_path / "scan.sqlite"
    run = crawl_to_scan(
        "https://example.test/",
        scan_out=str(scan_path),
        settings=settings,
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        fetcher=_fetcher(FIXTURE_SITE),
        sleeper=lambda _seconds: None,
        progress=progress,
    )
    return scan_path, run


# ── the numbers are the artifact's own ───────────────────────────────────────


def test_every_reported_count_matches_what_the_scan_artifact_holds(tmp_path):
    """The acceptance criterion of #619: reported fetched == rows in the artifact, then."""
    samples: list[tuple[int, int]] = []
    scan_path = tmp_path / "scan.sqlite"

    def progress(fetched: int, queued: int) -> None:
        samples.append((fetched, _pages_in_artifact(scan_path)))

    _, run = _crawl(tmp_path, progress)

    assert samples, "the crawl reported no progress at all"
    drifted = [(reported, actual) for reported, actual in samples if reported != actual]
    assert not drifted, f"reported page counts that the artifact did not hold: {drifted}"
    # robots.txt is not a page, and the disallowed link is never fetched: the artifact
    # holds what the crawl collected, not what it discovered.
    assert samples[-1][0] == run.pages == len(FIXTURE_SITE) - 2


def test_the_reported_count_moves_rather_than_sitting_still(tmp_path):
    """A line that never changes is the bug this issue was filed about."""
    reported: list[int] = []
    _crawl(tmp_path, lambda fetched, queued: reported.append(fetched))

    assert reported == sorted(reported), f"fetched count went backwards: {reported}"
    assert len(set(reported)) > 2, f"progress barely moved across the crawl: {reported}"
    # Six fetchable pages: the fixture also carries a robots-disallowed link, which
    # is reported but never fetched.
    assert reported[0] == 0 and reported[-1] == 6


def test_the_frontier_is_reported_while_it_is_still_growing(tmp_path):
    """``queued`` must reflect discovery in flight, and reach zero on a finished crawl."""
    queued: list[int] = []
    _crawl(tmp_path, lambda fetched, q: queued.append(q))

    assert max(queued) > 1, f"a crawl of a six-page site never had a real frontier: {queued}"
    assert queued[-1] == 0, "a crawl that emptied its frontier still reported work outstanding"


def test_the_spider_reports_its_own_pages_and_frontier(tmp_path):
    """The directory-mode crawler is wired to the same callback, from its own structures."""
    from seohead.crawl.spider import crawl_site

    samples: list[tuple[int, int]] = []
    result = crawl_site(
        "https://example.test/",
        max_urls=50,
        min_delay=0,
        robots_policy="respect",
        fetcher=_fetcher(FIXTURE_SITE),
        sleeper=lambda _seconds: None,
        progress=lambda fetched, queued: samples.append((fetched, queued)),
    )

    assert samples[0] == (0, 1), "the start URL was not on the frontier before the first fetch"
    # The last word matters more than the rest: a crawl that emptied its frontier must
    # say so, or a finished run signs off short of 100% and reads as stalled. The
    # fixture's robots-disallowed link is what makes this assertion able to fail.
    assert samples[-1] == (len(result.pages), 0)
    fetched_counts = [fetched for fetched, _ in samples]
    assert fetched_counts == sorted(fetched_counts), "the fetched count went backwards"
    assert fetched_counts[-1] == len(result.pages)
    # A skipped URL reports without advancing the fetched count, so the sequence
    # repeats a value rather than stepping one per report.
    assert len(samples) > len(result.pages) + 1


# ── an honest percentage ─────────────────────────────────────────────────────


def _progress_lines(reporter) -> list[str]:
    """Only the refreshing lines, never the one-time header (which says "fetched" too)."""
    return [
        line
        for chunk in reporter.stream.getvalue().splitlines()
        for line in chunk.split("\r")
        if " known (" in line
    ]


def _reporter(**kwargs):
    kwargs.setdefault("budget", 200)
    kwargs.setdefault("tty", False)
    kwargs.setdefault("interval", 0)
    return CrawlProgress(io.StringIO(), **kwargs)


def test_the_percentage_is_of_the_known_set_not_of_a_total():
    line = _reporter().line(25, 75, elapsed=10, rate=2.5)
    assert "100 known (25%)" in line
    assert "known ETA ~30s" in line
    assert "total" not in line


def test_the_header_says_the_frontier_is_still_growing():
    header = _reporter(budget=40000)._header()
    assert "40000-URL budget" in header
    assert "grows" in header and "not of the site" in header
    assert "cover only the current known frontier" in header
    assert "total crawl duration" in header


def test_unlimited_progress_reports_the_discovered_population_without_a_zero_cap():
    reporter = _reporter(budget=0)
    assert "without a URL limit" in reporter._header()
    assert "not of the site" in reporter._header()
    assert "300 known (66%)" in reporter.line(200, 100, elapsed=1, rate=1.0)


def test_the_known_set_is_capped_at_the_url_budget():
    """34 000 URLs found under a 200-URL budget is 200 URLs of work, not 34 000."""
    assert "200 known (50%)" in _reporter(budget=200).line(100, 34000, elapsed=1, rate=1.0)


def test_an_unmeasured_rate_says_so_rather_than_reading_as_a_stall():
    assert "rate n/a" in _reporter().line(3, 4, elapsed=0.0, rate=None)
    assert "known ETA n/a" in _reporter().line(3, 4, elapsed=0.0, rate=None)
    assert "0.0 req/s" not in _reporter().line(3, 4, elapsed=0.0, rate=None)


def test_the_rate_is_measured_over_the_samples_it_was_given():
    ticks = iter([0.0, 0.0, 10.0])  # construction, then one sample at each end
    reporter = _reporter(clock=lambda: next(ticks))
    reporter(0, 5)
    reporter(20, 5)  # twenty pages in ten seconds
    assert "2.0 req/s" in _progress_lines(reporter)[-1]


def test_a_zero_known_set_reports_no_percentage_rather_than_a_wrong_one():
    assert "0 known (n/a)" in _reporter().line(0, 0, elapsed=1, rate=None)


# ── the scan artifact's size ─────────────────────────────────────────────────


def test_the_artifact_size_is_reported_while_one_is_being_written(tmp_path):
    artifact = tmp_path / "scan.sqlite"
    artifact.write_bytes(b"x" * 4096)
    (tmp_path / "scan.sqlite-wal").write_bytes(b"x" * 2048)
    line = _reporter(artifact_path=str(artifact)).line(1, 1, elapsed=1, rate=1.0)
    # The WAL sidecar counts: a running crawl's committed pages live there, and a
    # main-file-only number sits frozen for minutes on a crawl that is fine.
    assert "scan 6.0 KB" in line


def test_no_artifact_means_no_size_field_rather_than_a_zero(tmp_path):
    line = _reporter().line(1, 1, elapsed=1, rate=1.0)
    assert "scan" not in line
    missing = _reporter(artifact_path=str(tmp_path / "never-created.sqlite"))
    assert "scan 0 B" in missing.line(1, 1, elapsed=1, rate=1.0)


# ── where the line goes ──────────────────────────────────────────────────────


def test_a_tty_is_redrawn_in_place():
    reporter = _reporter(tty=True)
    reporter(1, 5)
    reporter(2, 5)
    written = reporter.stream.getvalue()
    assert written.count("\r") == 2
    assert written.count("\n") == 1  # the header's, and nothing else until close
    reporter.close()
    assert reporter.stream.getvalue().endswith("\n")


def test_a_non_tty_gets_plain_lines_and_no_carriage_returns():
    reporter = _reporter(tty=False)
    reporter(1, 5)
    reporter(2, 5)
    assert "\r" not in reporter.stream.getvalue()
    assert len(_progress_lines(reporter)) == 2


def test_a_shorter_line_erases_the_longer_one_it_replaced():
    """Without the erase, a wide "20000 known" leaves stray digits behind "6 known"."""
    reporter = _reporter(tty=True, budget=20000, clock=lambda: 0.0)
    reporter(5, 1_000_000)  # a huge frontier: "20000 known (0%)"
    reporter(6, 0)  # the frontier drained: "6 known (100%)", two characters shorter
    painted = reporter.stream.getvalue().split("\r")[1:]
    assert painted[1].rstrip() != painted[1], painted  # padded out with the difference
    assert len(painted[1]) == len(painted[0]), painted


def test_a_non_tty_only_prints_on_its_interval():
    ticks = iter([0.0, 0.0, 1.0, 40.0])  # construction, then one per report
    reporter = CrawlProgress(io.StringIO(), budget=200, tty=False, clock=lambda: next(ticks))
    reporter(1, 5)  # first report always prints
    reporter(2, 5)  # one second later: too soon for a log file
    reporter(3, 5)  # forty seconds in: due
    assert [line.split(",")[0] for line in _progress_lines(reporter)] == [
        "crawl-site: 1 fetched",
        "crawl-site: 3 fetched",
    ]


def test_a_broken_stream_does_not_take_the_crawl_down():
    class Broken(io.StringIO):
        def write(self, _text):
            raise OSError("broken pipe")

    reporter = CrawlProgress(Broken(), budget=200, tty=True)
    reporter(1, 5)
    reporter(2, 5)
    reporter.close()  # no exception reaches the crawl


def test_closing_before_anything_was_reported_prints_nothing():
    """A run that died before its first fetch must not leave a "0 fetched" line."""
    reporter = _reporter()
    reporter.close()
    assert reporter.stream.getvalue() == ""


def test_close_repaints_the_last_reported_numbers_and_invents_none():
    reporter = _reporter(tty=True)
    reporter(4, 9)
    reporter.close()
    assert _progress_lines(reporter)[-1].startswith("crawl-site: 4 fetched, 13 known (30%)")


# ── -q, end to end ───────────────────────────────────────────────────────────


@pytest.fixture
def offline_cli(monkeypatch):
    """``seohead crawl-site`` against the fixture site, with the network unplugged."""
    from seohead.crawl import spider
    from seohead.sf.core import sitemap_coverage

    def no_network(*_args, **_kwargs):
        raise AssertionError("this test must not use the network")

    monkeypatch.setattr(socket, "getaddrinfo", no_network)
    monkeypatch.setattr(socket, "create_connection", no_network)
    monkeypatch.setenv("SEOHEAD_HTTP_CACHE_DIR", "off")
    monkeypatch.setattr(sitemap_coverage, "run_sitemap", lambda *a, **k: {})
    original = spider.crawl_site

    def offline(*args, **kwargs):
        return original(*args, **kwargs, fetcher=_fetcher(FIXTURE_SITE), sleeper=lambda _: None)

    monkeypatch.setattr(spider, "crawl_site", offline)


def _run_cli(tmp_path, *flags):
    from seohead import cli

    return cli.main(
        [
            "crawl-site",
            "--url",
            "https://example.test/",
            "--out-dir",
            str(tmp_path / "run"),
            "--min-delay",
            "0",
            *flags,
        ]
    )


def test_quiet_prints_nothing_to_stderr(offline_cli, tmp_path, capsys):
    assert _run_cli(tmp_path, "-q") == 0
    captured = capsys.readouterr()
    assert captured.err == ""
    assert captured.out.strip().startswith("{"), "the JSON result is not what -q silences"


def test_without_quiet_the_operator_gets_a_moving_progress_line(offline_cli, tmp_path, capsys):
    assert _run_cli(tmp_path) == 0
    err = capsys.readouterr().err
    assert "effective worst-case request rate" in err
    assert "fetched" in err and "known" in err
    # capsys is not a terminal, so this run must have taken the log-file path.
    assert "\r" not in err
