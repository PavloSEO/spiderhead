"""Security headers are stored by the native crawl, so the checks measure them (owner epic #1314).

A loopback fixture sends the headers a hardened site sends, one of them twice, and a session
cookie with flags. The crawl must keep the policy headers, report the repeated one, and never
store the cookie's name or value.
"""

from __future__ import annotations

import http.server
import json
import sqlite3
import threading
from collections.abc import Iterator

import pytest

from seohead.crawl import security_headers
from seohead.crawl.capture import redact_headers
from seohead.mcp import handlers

BUILD = "b" * 40
SECURITY = [
    ("Strict-Transport-Security", "max-age=31536000; includeSubDomains"),
    ("Content-Security-Policy", "default-src 'self'; frame-ancestors 'none'"),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "strict-origin-when-cross-origin"),
    ("Permissions-Policy", "geolocation=()"),
]
COOKIE = "session=s3cr3t-value; Path=/; Secure; HttpOnly; SameSite=Lax"


def _html(title: str, links: str = "") -> bytes:
    return (
        f"<html><head><title>{title}</title>"
        "<meta name='description' content='A fixture page described at length enough to pass.'>"
        f"</head><body><h1>{title}</h1><p>Body text.</p>{links}</body></html>"
    ).encode()


class _Handler(http.server.BaseHTTPRequestHandler):
    def _send(self, body: bytes, content_type: str, status: int, headers) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        path = self.path.split("?", 1)[0]
        if path == "/robots.txt":
            return self._send(b"User-agent: *\nAllow: /\n", "text/plain", 200, [])
        if path == "/":
            # X-Content-Type-Options is sent twice; the cookie carries flags.
            headers = [*SECURITY, ("X-Content-Type-Options", "nosniff"), ("Set-Cookie", COOKIE)]
            return self._send(
                _html("Home", "<a href='/p1/'>p1</a>"), "text/html; charset=utf-8", 200, headers
            )
        if path == "/p1/":
            return self._send(_html("p1", "<a href='/'>home</a>"), "text/html", 200, SECURITY)
        return self._send(b"not found", "text/plain", 404, [])

    def log_message(self, format: str, *args) -> None:
        pass


@pytest.fixture
def site(monkeypatch) -> Iterator[str]:
    monkeypatch.setenv("SEOHEAD_ALLOW_PRIVATE_NETWORKS", "1")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def _crawl(site: str, tmp_path) -> sqlite3.Connection:
    scan_out = str(tmp_path / "scan.sqlite")
    handlers.crawl_site(
        url=f"{site}/",
        scan_out=scan_out,
        max_urls=20,
        min_delay=0,
        concurrency=1,
        producer_build=BUILD,
    )
    return sqlite3.connect(scan_out)


def test_a_hardened_site_is_measured_not_reported_missing(site, tmp_path):
    con = _crawl(site, tmp_path)
    try:
        result = security_headers.evaluate(con)
        stored = [
            json.loads(raw)
            for (raw,) in con.execute(
                "SELECT effective_headers_redacted_json FROM responses WHERE purpose = 'page'"
            )
        ]
    finally:
        con.close()

    assert result["pages_measured"] == 2
    assert result["pages_unmeasured"] == 0
    for check_id in security_headers.HEADER_CHECKS:
        assert result["findings"][check_id] == [], check_id
    names = {name for pairs in stored for name, _value in pairs}
    assert {name.lower() for name, _value in SECURITY} <= names


def test_a_header_sent_twice_is_reported_on_that_page_only(site, tmp_path):
    con = _crawl(site, tmp_path)
    try:
        result = security_headers.evaluate(con)
    finally:
        con.close()

    assert result["findings"]["DUPLICATE_SECURITY_HEADER"] == [
        {"target_url": f"{site}/", "header": "x-content-type-options"}
    ]


def test_the_session_cookie_is_never_stored(site, tmp_path):
    con = _crawl(site, tmp_path)
    try:
        rows = con.execute(
            "SELECT response_headers_redacted_json, effective_headers_redacted_json FROM responses"
        ).fetchall()
    finally:
        con.close()

    blob = json.dumps(rows)
    assert "s3cr3t-value" not in blob
    assert "session=" not in blob


def test_redaction_keeps_policy_headers_and_drops_cookies_and_credentials():
    pairs = redact_headers(
        [
            ("Content-Security-Policy", "default-src 'self'"),
            ("Set-Cookie", "a=1; Secure"),
            ("Authorization", "Bearer x"),
        ]
    )
    assert ("content-security-policy", "default-src 'self'") in pairs
    assert not any("Bearer" in value or value == "a=1; Secure" for _name, value in pairs)
    assert ("x-seohead-redacted-headers", "authorization,set-cookie") in pairs
