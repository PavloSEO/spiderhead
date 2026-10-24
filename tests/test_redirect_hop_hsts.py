"""MISSING_HSTS_ON_REDIRECT_HOP: the hops the crawl itself never fetches (#1414).

MISSING_HSTS judges the final response of a crawled page, so a redirecting host
(www to apex, http to https) is invisible to it: the crawl starts on the start
URL's own host and never requests the variant. The opt-in probe
(``analysis.redirect_hop_hsts_probe``) walks the host variants' redirect chains
and records each hop's status and Strict-Transport-Security. These tests run the
probe through the real HTTP client against an owned loopback server, with the
www host faked the same way ``tests/test_native_external_capture.py`` fakes its
virtual hosts.
"""

from __future__ import annotations

import contextlib
import json
import socket
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

import pytest

from seohead.crawl import security_headers
from seohead.mcp import handlers
from seohead.mcp.reanalysis_handlers import reanalyze_scan
from seohead.mcp.scan_handlers import crawl_site_scan
from seohead.storage import read_audit

# The page every crawl starts from: links nowhere, so the crawl is one request
# and the probe is the only extra traffic.
_PAGE = b"<!doctype html><html><head><title>home</title></head><body>home</body></html>"


@pytest.fixture(autouse=True)
def owned_loopback_dns(monkeypatch):
    """Keep the bare and www spellings of the virtual host on the owned server."""
    original = socket.getaddrinfo

    def resolve(host, port, *args, **kwargs):
        if host in {"site.localhost", "www.site.localhost"}:
            host = "127.0.0.1"
        return original(host, port, *args, **kwargs)

    monkeypatch.setattr(socket, "getaddrinfo", resolve)
    monkeypatch.setenv("SEOHEAD_ALLOW_PRIVATE_NETWORKS", "1")


class _Site(BaseHTTPRequestHandler):
    """Apex answers 200; the www host answers 301 to the apex, with or without HSTS."""

    hsts_on_hops = False  # class flag flipped per test

    def _send(self, status: int, extra: dict[str, str] | None = None) -> None:
        body = b"" if status in (301, 302, 303, 307, 308) else _PAGE
        self.send_response(status)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        for key, value in (extra or {}).items():
            self.send_header(key, value)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self) -> None:
        host = (self.headers.get("Host") or "").split(":", 1)[0].lower()
        if host == "www.site.localhost":
            extra = {"Location": f"http://site.localhost:{self.server.server_port}/"}
            if self.hsts_on_hops:
                extra["Strict-Transport-Security"] = "max-age=31536000"
            return self._send(301, extra)
        self._send(200)

    def log_message(self, *_args) -> None:
        return


@contextlib.contextmanager
def _run_site(hsts_on_hops: bool) -> Iterator[str]:
    _Site.hsts_on_hops = hsts_on_hops
    server = ThreadingHTTPServer(("127.0.0.1", 0), _Site)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        yield f"http://site.localhost:{server.server_port}/"
    finally:
        server.shutdown()
        server.server_close()


def _probe_overrides() -> dict:
    from seohead.crawl.settings import load

    settings = load(
        overrides={"speed.min_delay_seconds": 0, "analysis.redirect_hop_hsts_probe": True}
    )
    return settings


def _crawl(start: str, tmp_path, settings=None) -> dict:
    return handlers.crawl_site(
        url=start,
        out_dir=str(tmp_path),
        max_urls=5,
        overrides=settings
        or {"speed.min_delay_seconds": 0, "analysis.redirect_hop_hsts_probe": True},
    )


def _audit(tmp_path) -> dict:
    return json.loads((tmp_path / "audit.json").read_text(encoding="utf-8"))


def _issues(audit: dict, check: str) -> list[dict]:
    return [issue for issue in audit["issues"] if issue["check"] == check]


def _skipped(audit: dict, check: str) -> list[dict]:
    return [s for s in audit["run"]["checks_skipped"] if s["id"] == check]


# ------------------------------------------------------------- the finding


def test_a_www_redirect_without_hsts_fires_the_finding_exactly_once(tmp_path):
    with _run_site(hsts_on_hops=False) as start:
        _crawl(start, tmp_path)
    audit = _audit(tmp_path)
    found = _issues(audit, "MISSING_HSTS_ON_REDIRECT_HOP")
    assert len(found) == 1, "the site-level defect is one finding, not one per hop or per variant"
    assert found[0]["target_url"] == start
    missing = found[0]["details"]["missing_hops"]
    assert [hop["status"] for hop in missing] == [301]
    # Compare the parsed host, not a URL prefix: an exact name is the claim.
    assert urlsplit(missing[0]["url"]).hostname == "www.site.localhost"
    assert missing[0]["url"].startswith("http://")
    assert found[0]["details"]["hops_measured"] >= 1


def test_hsts_on_every_hop_is_a_clean_pass_not_a_finding(tmp_path):
    with _run_site(hsts_on_hops=True) as start:
        _crawl(start, tmp_path)
    audit = _audit(tmp_path)
    assert _issues(audit, "MISSING_HSTS_ON_REDIRECT_HOP") == []
    assert _skipped(audit, "MISSING_HSTS_ON_REDIRECT_HOP") == [], (
        "a measured clean hop set is a pass, not a skip"
    )


def test_without_the_opt_in_a_plain_scan_reports_no_new_finding(tmp_path):
    with _run_site(hsts_on_hops=False) as start:
        _crawl(start, tmp_path, settings={"speed.min_delay_seconds": 0})
    audit = _audit(tmp_path)
    assert _issues(audit, "MISSING_HSTS_ON_REDIRECT_HOP") == []
    assert _skipped(audit, "MISSING_HSTS_ON_REDIRECT_HOP"), "the probe states why it did not run"


# ------------------------------------------------------------- the judge


def test_evaluate_redirect_hops_judges_only_redirect_responses():
    probe = {
        "url": "https://e.test/",
        "variants": [
            {
                "variant": "http://www.e.test/",
                "hops": [
                    {
                        "url": "http://www.e.test/",
                        "status": 301,
                        "location": "https://e.test/",
                        "hsts": "",
                    },
                    {
                        "url": "https://e.test/",
                        "status": 308,
                        "location": "https://e.test/",
                        "hsts": "max-age=1",
                    },
                ],
            },
            # A final 200 without the header is MISSING_HSTS's verdict, not this check's.
            {"variant": "https://e.test/", "hops": []},
        ],
    }
    verdict = security_headers.evaluate_redirect_hops(probe)
    assert verdict["state"] == "measured"
    assert verdict["hops_measured"] == 2
    assert [hop["url"] for hop in verdict["missing"]] == ["http://www.e.test/"]


def test_evaluate_redirect_hops_with_no_redirect_hop_has_nothing_to_judge():
    verdict = security_headers.evaluate_redirect_hops(
        {"url": "https://e.test/", "variants": [{"variant": "https://e.test/", "hops": []}]}
    )
    assert verdict == {"state": "no_redirect_hops", "hops_measured": 0, "missing": []}


# ------------------------------------------------------------- retention


def test_the_probe_result_is_retained_for_offline_reanalysis(tmp_path):
    with _run_site(hsts_on_hops=False) as start:
        scan_path = tmp_path / "scan.sqlite"
        crawl_site_scan(
            start,
            scan_out=str(scan_path),
            settings=_probe_overrides(),
            producer_build="a" * 40,
        )
    audit = read_audit(scan_path)
    fired = [issue for issue in audit["issues"] if issue["check"] == "MISSING_HSTS_ON_REDIRECT_HOP"]
    assert len(fired) == 1, "the live capture saw the headerless www hop"

    # The owned listener is gone: offline reanalysis must re-judge the retained
    # observation, not try the hosts again and not lose the finding.
    derived = tmp_path / "derived.sqlite"
    reanalyze_scan(str(scan_path), str(derived), producer_build="b" * 40)
    reanalyzed = read_audit(derived)
    assert [
        issue for issue in reanalyzed["issues"] if issue["check"] == "MISSING_HSTS_ON_REDIRECT_HOP"
    ], "the retained probe row must keep the finding after the hosts are unreachable"
