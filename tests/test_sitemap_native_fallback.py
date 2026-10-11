"""Native project crawls fall back to /sitemap.xml when robots.txt declares no sitemap."""

from __future__ import annotations

from seohead.crawl.sitemap_capture import CaptureSummary
from seohead.mcp import scan_sitemaps


class FakeScan:
    def __init__(self, robots):
        self.robots = robots
        self.declared = []
        self.interrupted = None
        self.roots = []
        self.members = []
        self.finished = []

    def sitemap_roots(self):
        return list(self.roots)

    def read_context(self, kind, key="run"):
        return self.robots if kind == "robots_summary" else None

    def note_audit_unavailable(self, _reason):
        raise AssertionError("robots was fetched; no unavailability note expected")

    def declare_sitemap(self, url, source, ordinal):
        self.declared.append((url, source, ordinal))
        self.roots.append(
            {"url": url, "source": source, "ordinal": ordinal, "sitemap_url_id": ordinal + 1}
        )

    def write_sitemap_members(self, sid, entries):
        self.members.extend(entries)

    def finish_sitemap(self, sid, complete, reason):
        self.finished.append((sid, complete, reason))

    def interrupt(self, reason):
        self.interrupted = reason

    def read_sitemap_summary(self, sid):
        return None

    def iter_sitemap_members(self, sid):
        return iter(())


def _fetched(sitemaps):
    return {"fetch_state": "fetched", "parsed": {"groups": [], "sitemaps": sitemaps}}


def _run(monkeypatch, scan, outcome_complete, start_url="https://example.test/shop/page"):
    def capture(roots, **_kwargs):
        return [
            CaptureSummary(
                sitemap_id=root.sitemap_id,
                root=root.url,
                count=0,
                complete=outcome_complete,
                reason="" if outcome_complete else "404",
                errors=0 if outcome_complete else 1,
                truncated=False,
            )
            for root in roots
        ]

    monkeypatch.setattr(scan_sitemaps, "capture_declared_roots", capture)
    result = {}
    scan_sitemaps.load_sitemaps(
        scan,
        lambda _urls: None,
        settings={"sitemaps": {"auto_discover": True}, "limits": {"max_urls": 0}},
        result=result,
        start_url=start_url,
    )
    return result


def test_no_robots_sitemap_declares_the_origin_sitemap_xml(monkeypatch):
    scan = FakeScan(_fetched([]))
    _run(monkeypatch, scan, outcome_complete=True)

    assert scan.declared == [("https://example.test/sitemap.xml", "fallback", 0)]


def test_absent_guessed_sitemap_does_not_interrupt_the_crawl(monkeypatch):
    scan = FakeScan(_fetched([]))
    _run(monkeypatch, scan, outcome_complete=False)

    assert scan.interrupted is None


def test_robots_declared_sitemap_wins_and_skips_the_fallback(monkeypatch):
    scan = FakeScan(_fetched(["https://example.test/declared.xml"]))
    _run(monkeypatch, scan, outcome_complete=True)

    assert scan.declared == [("https://example.test/declared.xml", "robots", 0)]


def test_declared_incomplete_sitemap_still_interrupts(monkeypatch):
    scan = FakeScan(_fetched(["https://example.test/declared.xml"]))
    _run(monkeypatch, scan, outcome_complete=False)

    assert scan.interrupted == "sitemap expansion is incomplete"


def test_fallback_is_not_redeclared_on_resume(monkeypatch):
    scan = FakeScan(_fetched([]))
    scan.roots = [
        {
            "url": "https://example.test/sitemap.xml",
            "source": "fallback",
            "ordinal": 0,
            "sitemap_url_id": 1,
        }
    ]
    _run(monkeypatch, scan, outcome_complete=False)

    assert scan.declared == []
