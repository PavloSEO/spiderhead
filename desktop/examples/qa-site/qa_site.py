#!/usr/bin/env python3
"""Deterministic loopback-only website for SEOHEAD capture and recheck demos.

This module is deliberately a small stdlib HTTP fixture, not a crawler or an
SEO analyser.  It exists so the existing SEOHEAD CLI/MCP can collect the same
known corpus and exercise retained, offline analysis repeatedly.
"""

from __future__ import annotations

import argparse
import hashlib
import html
import ipaddress
import json
import os
import signal
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

FORMAT = "seohead.qa-site.v1"
PROFILES = ("broken", "clean", "fix-delta", "tracking")
SOURCE = Path(__file__).resolve()


def _page(title: str, body: str, *, head: str = "", lang: str = "en") -> str:
    return (
        f"<!doctype html><html lang={lang!r}><head><meta charset='utf-8'>"
        f"<title>{html.escape(title)}</title>{head}</head><body>"
        "<header><a href='/'>QA fixture</a></header>"
        f"{body}</body></html>"
    )


def _clean_page(slug: str, extra: str = "", *, canonical: str | None = None) -> str:
    canonical = canonical or f"/clean/{slug}"
    return _page(
        f"Clean {slug.title()} | SEOHEAD QA",
        f"<main><h1>Clean {slug.title()}</h1><p>Unique stable content for {slug}.</p>{extra}</main>",
        head=(
            f"<meta name='description' content='Unique description for {slug}.'>"
            f"<link rel='canonical' href='{canonical}'>"
            f"<link rel='alternate' hreflang='en' href='{canonical}'>"
        ),
    )


def _broken_home() -> str:
    links = [
        "/broken/duplicate-a", "/broken/duplicate-b", "/broken/thin", "/broken/no-main",
        "/broken/noindex", "/broken/canonical-source", "/broken/hreflang", "/broken/images",
        "/broken/structured", "/broken/links", "/broken/js-rendered", "/missing", "/gone",
        "/server-error", "/redirect-one", "/loop-a", "/optional/rate-limited",
    ]
    anchors = "".join(f"<li><a href='{path}'>Fixture {path}</a></li>" for path in links)
    return _page(
        "QA broken home | SEOHEAD",
        "<main><h1>QA broken home</h1><h2>Known scenarios</h2>"
        f"<ul>{anchors}</ul><a rel='nofollow' href='/broken/noindex'>nofollow internal</a>"
        "<a href='#missing-anchor'>missing anchor</a>"
        "<a href='https://external.example.test/outbound'>external fixture</a></main>",
        head="<meta name='description' content='A deliberately broken SEO QA fixture.'><link rel='canonical' href='/'>",
    )


def _fixed_home() -> str:
    """Keep the broken frontier while repairing the pages it reaches."""
    links = [
        "/broken/duplicate-a", "/broken/duplicate-b", "/broken/thin", "/broken/no-main",
        "/broken/noindex", "/broken/canonical-source", "/broken/hreflang", "/broken/images",
        "/broken/structured", "/broken/links", "/broken/js-rendered", "/missing", "/gone",
        "/server-error", "/redirect-one", "/loop-a", "/optional/rate-limited",
    ]
    anchors = "".join(f"<li><a href='{path}'>Fixture {path}</a></li>" for path in links)
    return _page(
        "QA fixed delta home | SEOHEAD",
        "<main><h1>QA fixed delta home</h1><h2>Same bounded route frontier</h2>"
        f"<ul>{anchors}</ul><a rel='nofollow' href='/broken/noindex'>nofollow internal</a>"
        "<a href='#fixed-anchor'>fixed anchor</a><span id='fixed-anchor'>Anchor target</span>"
        "<a href='https://external.example.test/outbound'>external fixture</a></main>",
        head="<meta name='description' content='A repaired delta against the QA broken fixture.'><link rel='canonical' href='/'>",
    )


def _fixed_response(path: str, headers: dict[str, str]) -> tuple[int, dict[str, str], bytes] | None:
    """Corrections for the same measurable paths as the broken capture."""
    if path == "/":
        return _html(_fixed_home(), headers)
    if path in {"/missing", "/gone", "/server-error", "/optional/rate-limited"}:
        slug = path.strip("/").replace("/", "-")
        return _html(_clean_page(slug, canonical=path), headers)
    if path == "/broken/duplicate-a":
        return _html(_clean_page("duplicate-a", "<p>Distinct repaired content A.</p>", canonical=path), headers)
    if path == "/broken/duplicate-b":
        return _html(_clean_page("duplicate-b", "<p>Distinct repaired content B.</p>", canonical=path), headers)
    if path == "/broken/thin":
        return _html(_clean_page("thin", "<p>This repaired page has enough distinct explanatory text to be a useful QA document.</p>", canonical=path), headers)
    if path == "/broken/no-main":
        return _html(_clean_page("no-main", "<h2>Landmark restored</h2><p>Visible content now belongs to main.</p>", canonical=path), headers)
    if path == "/broken/noindex":
        return _html(_clean_page("noindex", "<p>The indexability directive has been removed.</p>", canonical=path), headers)
    if path in {"/broken/canonical-source", "/broken/canonical-middle", "/broken/canonical-final", "/sitemap-noncanonical"}:
        slug = path.rsplit("/", 1)[-1]
        return _html(_clean_page(slug, canonical=path), headers)
    if path == "/broken/hreflang":
        head = "<link rel='canonical' href='/broken/hreflang'><link rel='alternate' hreflang='en' href='/broken/hreflang'><link rel='alternate' hreflang='x-default' href='/broken/hreflang'>"
        return _html(_page("Hreflang repaired | QA", "<main><h1>Hreflang repaired</h1><p>Self and x-default point to the captured URL.</p></main>", head=head), headers)
    if path == "/broken/images":
        body = "<main><h1>Images repaired</h1><img src='/image-ok.svg' width='40' height='30' alt='QA fixture'><img src='/image-ok.svg' width='40' height='30' alt='QA alternate'></main>"
        return _html(_page("Images repaired | QA", body, head="<link rel='canonical' href='/broken/images'>"), headers)
    if path == "/broken/structured":
        script = "<script type='application/ld+json'>{\"@context\":\"https://schema.org\",\"@type\":\"WebPage\",\"name\":\"QA repaired\"}</script>"
        return _html(_page("Structured data repaired | QA", "<main><h1>Structured data repaired</h1></main>", head=script + "<link rel='canonical' href='/broken/structured'>"), headers)
    if path == "/broken/links":
        body = "<main><h1>Links repaired</h1><a href='/broken/duplicate-a'>Working link</a><a href='/broken/links#target'>Working anchor</a><span id='target'>Target</span><a rel='nofollow' href='/broken/duplicate-b'>Nofollow working link</a></main>"
        return _html(_page("Links repaired | QA", body, head="<link rel='canonical' href='/broken/links'>"), headers)
    return None


def _clean_home() -> str:
    return _page(
        "SEOHEAD QA clean home",
        "<main><h1>SEOHEAD QA clean home</h1><p>Fixed baseline for comparison.</p>"
        "<a href='/clean/alpha'>Alpha</a><a href='/clean/bravo'>Bravo</a></main>",
        head="<meta name='description' content='Clean local QA fixture.'><link rel='canonical' href='/'>",
    )


def _tracking_head(marker: str) -> str:
    """Passive literals only: these do not load or initialize analytics."""
    return (
        f"<script type='application/json' data-qa-tracking='{marker}'>"
        '{"gtm_container":"GTM-QADEMO","ga4_measurement_id":"G-QADEMO123",'
        '"metrika_counter_id":98765432}</script>'
    )


def _tracking_response(path: str, headers: dict[str, str]) -> tuple[int, dict[str, str], bytes] | None:
    if path == "/":
        links = ("/tracking/static-head", "/tracking/body-only", "/tracking/absent", "/tracking/raw-rendered", "/tracking/no-store")
        body = "<main><h1>Tracking QA routes</h1><ul>" + "".join(f"<li><a href='{link}'>{link}</a></li>" for link in links) + "</ul></main>"
        return _html(_page("Tracking QA fixture", body, head="<link rel='canonical' href='/'>"), headers)
    if path == "/tracking/static-head":
        body = "<main><h1>Static head marker</h1><p>Passive source-only tracking syntax.</p></main>"
        return _html(_page("Static tracking marker | QA", body, head=_tracking_head("GTM-QADEMO") + "<link rel='canonical' href='/tracking/static-head'>"), headers)
    if path == "/tracking/body-only":
        body = "<main><h1>Body-only marker</h1><p>GTM-BODY-QADEMO</p></main>"
        return _html(_page("Body tracking marker | QA", body, head="<link rel='canonical' href='/tracking/body-only'>"), headers)
    if path == "/tracking/absent":
        return _html(_page("No tracking marker | QA", "<main><h1>Absent marker</h1><p>No tracking marker is present on this route.</p></main>", head="<link rel='canonical' href='/tracking/absent'>"), headers)
    if path == "/tracking/raw-rendered":
        body = "<main><h1>Rendered-only marker</h1><div id='rendered-marker'>raw shell</div><script>document.getElementById('rendered-marker').textContent='GTM-' + 'RENDERED-QADEMO';</script></main>"
        return _html(_page("Rendered tracking marker | QA", body, head="<link rel='canonical' href='/tracking/raw-rendered'>"), headers)
    if path == "/tracking/no-store":
        body = "<main><h1>No-store marker</h1><p>GTM-NOSTORE-QADEMO</p></main>"
        return _html(_page("No-store tracking marker | QA", body, head=_tracking_head("GTM-NOSTORE-QADEMO") + "<link rel='canonical' href='/tracking/no-store'>"), {"Cache-Control": "no-store", **headers})
    return None


def _body(profile: str, path: str) -> tuple[int, dict[str, str], bytes]:
    """Return a complete deterministic response without performing any I/O."""
    common_headers = {"X-Content-Type-Options": "nosniff", "Referrer-Policy": "same-origin"}
    if profile == "fix-delta":
        fixed = _fixed_response(path, common_headers)
        if fixed is not None:
            return fixed
    if profile == "tracking":
        tracked = _tracking_response(path, common_headers)
        if tracked is not None:
            return tracked
    if path == "/robots.txt":
        return 200, {"Content-Type": "text/plain; charset=utf-8", **common_headers}, (
            b"User-agent: *\nAllow: /\nDisallow: /private/\nSitemap: /sitemap.xml\n"
        )
    if path == "/sitemap.xml":
        return 200, {"Content-Type": "application/xml", **common_headers}, (
            b"<?xml version='1.0' encoding='UTF-8'?><sitemapindex>"
            b"<sitemap><loc>/sitemap-main.xml</loc></sitemap>"
            b"<sitemap><loc>/sitemap-extra.xml</loc></sitemap></sitemapindex>"
        )
    if path == "/sitemap-main.xml":
        urls = ["/", "/broken/duplicate-a", "/broken/duplicate-b", "/broken/canonical-source", "/orphan"]
        if profile == "clean":
            urls = ["/", "/clean/alpha", "/clean/bravo"]
        return 200, {"Content-Type": "application/xml", **common_headers}, _urlset(urls).encode()
    if path == "/sitemap-extra.xml":
        urls = ["/broken/duplicate-a", "/sitemap-noncanonical", "/sitemap-missing"]
        if profile == "clean":
            urls = ["/clean/bravo"]
        return 200, {"Content-Type": "application/xml", **common_headers}, _urlset(urls).encode()
    if path == "/favicon.ico":
        return 204, common_headers, b""
    if path == "/image-ok.svg":
        return 200, {"Content-Type": "image/svg+xml", **common_headers}, b"<svg xmlns='http://www.w3.org/2000/svg' width='40' height='30'/>"
    if path == "/missing-image.jpg":
        return 404, {"Content-Type": "text/plain", **common_headers}, b"missing image"
    if path == "/gone":
        return 410, {"Content-Type": "text/plain", **common_headers}, b"gone intentionally"
    if path == "/missing" or path == "/sitemap-missing":
        return 404, {"Content-Type": "text/plain", **common_headers}, b"missing intentionally"
    if path == "/server-error":
        return 503, {"Content-Type": "text/plain", "Retry-After": "1", **common_headers}, b"temporary fixture failure"
    if path == "/optional/rate-limited":
        return 429, {"Content-Type": "text/plain", "Retry-After": "1", **common_headers}, b"bounded optional retry fixture"
    if path == "/redirect-one":
        return 301, {"Location": "/redirect-two", **common_headers}, b""
    if path == "/redirect-two":
        return 302, {"Location": "/clean/alpha" if profile == "clean" else "/broken/duplicate-a", **common_headers}, b""
    if path == "/loop-a":
        return 302, {"Location": "/loop-b", **common_headers}, b""
    if path == "/loop-b":
        return 302, {"Location": "/loop-a", **common_headers}, b""
    if path == "/optional/slow":
        return 200, {"Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store", **common_headers}, _page("Optional slow fixture", "<main><h1>Optional slow fixture</h1></main>").encode()

    if profile == "clean":
        if path == "/":
            return _html(_clean_home(), common_headers)
        if path == "/clean/alpha":
            return _html(_clean_page("alpha", "<a href='/clean/bravo'>Bravo</a>"), common_headers)
        if path == "/clean/bravo":
            return _html(_clean_page("bravo", "<a href='/clean/alpha'>Alpha</a>"), common_headers)
        if path == "/orphan":
            return _html(_clean_page("orphan"), common_headers)
        return 404, {"Content-Type": "text/plain", **common_headers}, b"clean profile missing"

    if path == "/":
        return _html(_broken_home(), common_headers)
    duplicate = "<main><h1>Duplicate fixture heading</h1><p>Exactly repeated body text proves duplicate content.</p></main>"
    duplicate_head = "<meta name='description' content='Repeated description.'><link rel='canonical' href='{path}'>"
    if path in {"/broken/duplicate-a", "/broken/duplicate-b"}:
        return _html(_page("Repeated title | QA", duplicate, head=duplicate_head.format(path=path)), common_headers)
    if path == "/broken/thin":
        return _html(_page("Thin page | QA", "<main><h1>Thin page</h1>x</main>", head="<link rel='canonical' href='/broken/thin'>"), common_headers)
    if path == "/broken/no-main":
        return _html(_page("No main | QA", "<h1>No main landmark</h1><p>Visible content outside main.</p>"), common_headers)
    if path == "/broken/noindex":
        return _html(_page("Noindex | QA", "<main><h1>Noindex</h1></main>", head="<meta name='robots' content='noindex,nofollow'><link rel='canonical' href='/broken/noindex'>"), common_headers)
    if path == "/broken/canonical-source":
        return _html(_page("Canonical chain source | QA", "<main><h1>Canonical chain</h1></main>", head="<link rel='canonical' href='/broken/canonical-middle'>"), common_headers)
    if path == "/broken/canonical-middle":
        return _html(_page("Canonical chain middle | QA", "<main><h1>Canonical middle</h1></main>", head="<link rel='canonical' href='/broken/canonical-final'>"), common_headers)
    if path == "/broken/canonical-final":
        return _html(_page("Canonical final | QA", "<main><h1>Canonical final</h1></main>", head="<link rel='canonical' href='/broken/canonical-final'>"), common_headers)
    if path == "/sitemap-noncanonical":
        return _html(_page("Sitemap noncanonical | QA", "<main><h1>Sitemap noncanonical</h1></main>", head="<link rel='canonical' href='/'>"), common_headers)
    if path == "/broken/hreflang":
        return _html(_page("Hreflang mismatch | QA", "<main><h1>Hreflang mismatch</h1></main>", head="<link rel='canonical' href='/broken/hreflang'><link rel='alternate' hreflang='en' href='/broken/hreflang'><link rel='alternate' hreflang='fr' href='/missing'>"), common_headers)
    if path == "/broken/images":
        return _html(_page("Images | QA", "<main><h1>Images</h1><img src='/image-ok.svg'><img src='/missing-image.jpg' alt='Missing image'><img src='/image-ok.svg' width='0' height='0' alt='bad dimensions'></main>", head="<link rel='canonical' href='/broken/images'>"), common_headers)
    if path == "/broken/structured":
        scripts = "<script type='application/ld+json'>{\"@context\":\"https://schema.org\",\"@type\":\"WebPage\",\"name\":\"QA\"}</script><script type='application/ld+json'>{not valid json}</script>"
        return _html(_page("Structured data | QA", "<main><h1>Structured data</h1></main>", head=scripts + "<link rel='canonical' href='/broken/structured'>"), common_headers)
    if path == "/broken/links":
        return _html(_page("Links | QA", "<main><h1>Links</h1><a href='/missing'>Broken link</a><a href='/broken/links#missing'>Broken anchor</a><a rel='nofollow' href='/gone'>Nofollow gone</a></main>", head="<link rel='canonical' href='/broken/links'>"), common_headers)
    if path == "/broken/js-rendered":
        body = "<main><h1>Raw shell</h1><div id='rendered'>unrendered</div><script>console.error('qa intentional console error'); document.getElementById('rendered').textContent='rendered content';</script></main>"
        return _html(_page("JS rendered | QA", body, head="<link rel='canonical' href='/broken/js-rendered'>"), common_headers)
    if path == "/orphan":
        return _html(_page("Sitemap orphan | QA", "<main><h1>Sitemap-only orphan</h1><p>This page has no internal inlink.</p></main>", head="<link rel='canonical' href='/orphan'>"), common_headers)
    return 404, {"Content-Type": "text/plain", **common_headers}, b"broken profile missing"


def _urlset(paths: list[str]) -> str:
    entries = "".join(f"<url><loc>{path}</loc></url>" for path in paths)
    return f"<?xml version='1.0' encoding='UTF-8'?><urlset xmlns='http://www.sitemaps.org/schemas/sitemap/0.9'>{entries}</urlset>"


def _html(body: str, headers: dict[str, str]) -> tuple[int, dict[str, str], bytes]:
    return 200, {"Content-Type": "text/html; charset=utf-8", **headers}, body.encode("utf-8")


def catalogue(profile: str, base_url: str = "") -> dict[str, Any]:
    """Facts deliberately encoded in this fixture, with honest prerequisites."""
    if profile not in PROFILES:
        raise ValueError(f"unknown profile: {profile}")
    if profile == "tracking":
        return {
            "format": f"{FORMAT}.catalogue",
            "profile": profile,
            "origin": base_url.rstrip("/"),
            "deterministic": True,
            "scenarios": [
                {"id": "static-head", "paths": ["/tracking/static-head"], "expected": {"literal": ["GTM-QADEMO", "G-QADEMO123", "metrika_counter_id"], "scope": "static head script"}, "evidence": {"availability": "measured when the retained static body is available", "source": "retained native document body"}},
                {"id": "body-only", "paths": ["/tracking/body-only"], "expected": {"literal": "GTM-BODY-QADEMO", "scope": "static body only"}, "evidence": {"availability": "measured when the retained static body is available", "source": "retained native document body"}},
                {"id": "absent", "paths": ["/tracking/absent"], "expected": {"literal": "GTM-QADEMO", "match": False}, "evidence": {"availability": "measured when the retained static body is available", "source": "retained native document body"}},
                {"id": "rendered-only", "paths": ["/tracking/raw-rendered"], "expected": {"raw_literal": "GTM-RENDERED-QADEMO", "raw_match": False, "rendered_match": "unknown"}, "prerequisite": "A configured existing JS renderer is required to capture the injected text.", "unavailable_without": "rendering.mode=js and a working renderer"},
                {"id": "no-store", "paths": ["/tracking/no-store"], "expected": {"literal": "GTM-NOSTORE-QADEMO", "offline_match": "unknown"}, "prerequisite": "Captured-body retention must explicitly acknowledge no-store responses.", "unavailable_without": "storage retains no-store evidence; this capture uses the default acknowledgement=false"},
            ],
            "limits": {"network": "No route loads Google, Yandex, GTM, GA4, Metrika, or any external script.", "meaning": "Literal code presence is not evidence that analytics collection or a tag manager works."},
        }
    broken = profile == "broken"
    return {
        "format": f"{FORMAT}.catalogue",
        "profile": profile,
        "origin": base_url.rstrip("/"),
        "deterministic": True,
        "scenarios": [
            {"id": "http-statuses", "paths": ["/missing", "/gone", "/server-error"], "expected": {"/missing": 404 if broken else 200, "/gone": 410 if broken else 200, "/server-error": 503 if broken else 200}, "active": broken, "evidence": {"availability": "measured", "source": "native scan pages.status_code"}},
            {"id": "redirects", "paths": ["/redirect-one", "/redirect-two", "/loop-a", "/loop-b"], "expected": {"chain": [301, 302], "loop": [302, 302]}, "active": True},
            {"id": "robots-and-sitemap", "paths": ["/robots.txt", "/sitemap.xml", "/sitemap-main.xml", "/sitemap-extra.xml"], "expected": {"robots_disallow": "/private/", "sitemap_duplicate": "/broken/duplicate-a" if profile != "clean" else "/clean/bravo", "sitemap_missing": "/sitemap-missing" if profile != "clean" else None}, "active": True, "evidence": {"availability": "unavailable in ordinary raw capture", "reason": "the retained native captures were not seeded with --sitemap and save no sitemap declarations"}},
            {"id": "indexability-canonical-hreflang", "paths": ["/broken/noindex", "/broken/canonical-source", "/broken/canonical-middle", "/broken/hreflang"], "expected": {"noindex": "/broken/noindex" if broken else None, "canonical_chain": ["/broken/canonical-source", "/broken/canonical-middle", "/broken/canonical-final"] if broken else [], "hreflang_missing_target": "/missing" if broken else None}, "active": broken, "evidence": {"availability": "measured or partial", "source": "native pages.meta_robots/canonical and audit checks NOINDEX, HREFLANG_BROKEN_TARGET"}},
            {"id": "content", "paths": ["/broken/duplicate-a", "/broken/duplicate-b", "/broken/thin", "/broken/no-main"], "expected": {"duplicate_pair": ["/broken/duplicate-a", "/broken/duplicate-b"] if broken else [], "thin": "/broken/thin" if broken else None, "no_main": "/broken/no-main" if broken else None}, "active": broken, "evidence": {"availability": "measured", "source": "native pages.title/meta_description/h1 and audit TITLE_DUPLICATE, DESC_DUPLICATE, H1_DUPLICATE"}},
            {"id": "links-and-orphans", "paths": ["/broken/links", "/orphan"], "expected": {"broken_target": "/missing" if broken else None, "nofollow_target": "/gone" if broken else "/broken/duplicate-b", "orphan": "/orphan", "external": "https://external.example.test/outbound"}, "active": broken, "evidence": {"availability": "partial", "source": "native links and fragment-link derivation; sitemap orphan requires saved sitemap declarations"}},
            {"id": "images", "paths": ["/broken/images", "/image-ok.svg", "/missing-image.jpg"], "expected": {"missing_alt": "/image-ok.svg" if broken else None, "missing_resource": "/missing-image.jpg" if broken else None, "zero_dimensions": broken}, "active": broken, "evidence": {"availability": "measured", "source": "native pages.images_missing_alt_attr and audit IMG_MISSING_ALT_ATTRIBUTE"}},
            {"id": "structured-data", "paths": ["/broken/structured"], "expected": {"valid_json_ld": 1, "malformed_json_ld": 1 if broken else 0}, "active": broken, "evidence": {"availability": "measured", "source": "native pages.jsonld_blocks_found/jsonld_blocks_parsed and audit STRUCTURED_DATA_PARSE_ERROR"}},
            {"id": "rendering", "paths": ["/broken/js-rendered"], "expected": {"raw_marker": "unrendered", "rendered_marker": "rendered content", "console_error": "qa intentional console error"}, "active": broken, "prerequisite": "A configured existing JS renderer; raw native capture cannot prove rendered DOM or browser console output.", "unavailable_without": "Rendering evidence is unavailable without rendering.mode=js and a working renderer."},
            {"id": "headers-and-privacy", "paths": ["/broken/images", "/optional/slow"], "expected": {"cache_control": "no-store", "x_content_type_options": "nosniff", "referrer_policy": "same-origin"}, "active": True},
            {"id": "bounded-optional-transients", "paths": ["/optional/rate-limited", "/optional/slow"], "expected": {"status": 429, "retry_after": "1", "slow_seconds": "configured 0..5"}, "active": True, "prerequisite": "Enable /optional/slow explicitly with --slow-seconds; it is zero-delay by default.", "unavailable_without": "a capture policy that admits retries/timeouts"},
        ],
    }


def source_manifest(profile: str) -> dict[str, Any]:
    digest = hashlib.sha256(SOURCE.read_bytes()).hexdigest()
    return {
        "format": f"{FORMAT}.source-manifest",
        "profile": profile,
        "source": SOURCE.name,
        "source_sha256": digest,
        "python": f"{os.sys.version_info.major}.{os.sys.version_info.minor}",
        "profiles": list(PROFILES),
        "network_boundary": "Loopback binding only; this source neither crawls nor contacts external services.",
    }


class QaSiteServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address: tuple[str, int], profile: str, slow_seconds: float):
        self.profile = profile
        self.slow_seconds = slow_seconds
        super().__init__(address, QaRequestHandler)


class QaRequestHandler(BaseHTTPRequestHandler):
    server: QaSiteServer

    def do_HEAD(self) -> None:
        self._serve(write_body=False)

    def do_GET(self) -> None:
        self._serve(write_body=True)

    def _serve(self, *, write_body: bool) -> None:
        path = urlsplit(self.path).path or "/"
        if path == "/optional/slow" and self.server.slow_seconds:
            time.sleep(self.server.slow_seconds)
        status, headers, body = _body(self.server.profile, path)
        self.send_response(status)
        for name, value in headers.items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if write_body and body:
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

    def log_message(self, _format: str, *_args: object) -> None:
        pass


def _loopback(host: str) -> bool:
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return host == "localhost"


def _write_json(path: Path | None, payload: dict[str, Any]) -> None:
    if path is None:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(path)


def serve(args: argparse.Namespace) -> int:
    if not _loopback(args.host):
        raise SystemExit("qa-site: --host must be a loopback address")
    server = QaSiteServer((args.host, args.port), args.profile, args.slow_seconds)
    host, port = server.server_address[:2]
    runtime = {"format": f"{FORMAT}.runtime", "pid": os.getpid(), "host": host, "port": port, "profile": args.profile, "state": "ready", "lifetime_seconds": args.lifetime_seconds}
    pid_path = Path(args.pid_file) if args.pid_file else None
    ready_path = Path(args.ready_file) if args.ready_file else None
    _write_json(pid_path, runtime)
    _write_json(ready_path, runtime)
    print(json.dumps(runtime, sort_keys=True), flush=True)
    timer = None
    if args.lifetime_seconds:
        timer = threading.Timer(args.lifetime_seconds, server.shutdown)
        timer.daemon = True
        timer.start()
    previous_term = signal.getsignal(signal.SIGTERM)
    shutdown_requested = threading.Event()

    def request_shutdown(*_ignored: object) -> None:
        if not shutdown_requested.is_set():
            shutdown_requested.set()
            threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, request_shutdown)
    try:
        server.serve_forever(poll_interval=0.2)
    finally:
        if timer:
            timer.cancel()
        server.server_close()
        stopped = {**runtime, "state": "stopped"}
        _write_json(pid_path, stopped)
        _write_json(ready_path, stopped)
        signal.signal(signal.SIGTERM, previous_term)
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    actions = result.add_subparsers(dest="action", required=True)
    server = actions.add_parser("serve", help="run the controlled loopback fixture")
    server.add_argument("--host", default="127.0.0.1")
    server.add_argument("--port", default=0, type=int)
    server.add_argument("--profile", choices=PROFILES, default="broken")
    server.add_argument("--slow-seconds", default=0.0, type=float, choices=[0, 0.25, 0.5, 1, 2, 5])
    server.add_argument("--lifetime-seconds", default=0.0, type=float)
    server.add_argument("--pid-file")
    server.add_argument("--ready-file")
    for name, help_text in (("catalogue", "write the scenario catalogue"), ("manifest", "write the source manifest")):
        command = actions.add_parser(name, help=help_text)
        command.add_argument("--profile", choices=PROFILES, default="broken")
        command.add_argument("--output", required=True)
        if name == "catalogue":
            command.add_argument("--base-url", default="")
    return result


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if args.action == "serve":
        return serve(args)
    output = Path(args.output)
    payload = catalogue(args.profile, args.base_url) if args.action == "catalogue" else source_manifest(args.profile)
    _write_json(output, payload)
    print(str(output))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
