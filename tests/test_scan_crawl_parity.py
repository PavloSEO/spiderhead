"""Observation-level parity between the legacy spider and SQLite adapter."""

from __future__ import annotations

import dataclasses
import json
import sqlite3
from collections import Counter

from seohead.crawl.collect import PageRecord
from seohead.crawl.settings import load
from seohead.crawl.spider import LinkEdge, crawl_site
from seohead.crawl.sqlite_adapter import crawl_to_scan


class _Response:
    def __init__(self, status_code, text="", headers=None):
        self.status_code = status_code
        self.text = text
        self.content = text.encode("utf-8")
        self.headers = headers or {"content-type": "text/html; charset=utf-8"}


def _runtime_versions():
    return {
        "python": "test",
        "sqlite": "test",
        "httpx": "test",
        "lxml": "test",
        "beautifulsoup4": "test",
    }


def _fetcher(responses):
    def fetch(url):
        return responses.get(url, _Response(404, ""))

    return fetch


def _legacy(settings, fetcher, decisions_path, content_area_config):
    return crawl_site(
        "https://example.test/",
        max_urls=settings["limits"]["max_urls"],
        max_depth=settings["limits"]["max_depth"],
        max_seconds=settings["limits"]["max_crawl_seconds"],
        min_delay=settings["speed"]["min_delay_seconds"],
        timeout=settings["http"]["timeout_seconds"],
        robots_policy=settings["robots"]["policy"],
        scope=settings["scope"],
        decisions_path=str(decisions_path),
        fetcher=fetcher,
        sleeper=lambda _seconds: None,
        credential_headers=settings["http"]["credential_headers"],
        config_fingerprint="parity",
        concurrency=settings["speed"]["concurrency"],
        max_response_bytes=settings["limits"]["max_response_bytes"],
        max_url_length=settings["limits"]["max_url_length"],
        max_query_variants_per_path=settings["limits"]["max_query_variants_per_path"],
        retry_on_timeout=settings["http"]["retry_on_timeout"],
        user_agent=settings["http"]["user_agent"],
        robots_token=settings["robots"]["user_agent_token"],
        unavailable_means_stop=settings["robots"]["unavailable_means_stop"],
        stop_after_consecutive_timeouts=settings["speed"]["stop_after_consecutive_timeouts"],
        max_delay_seconds=settings["speed"]["max_delay_seconds"],
        follow_nofollow=settings["discovery"]["follow_nofollow"],
        classify_links=settings["link_position"]["classify"],
        link_position_rules=settings["link_position"]["rules"],
        content_area_config=content_area_config,
        cache=None,
        extra_request_headers=settings["http"]["headers"],
        adaptive=settings["speed"]["adaptive"],
        store_hyperlinks=settings["discovery"]["hyperlinks"]["store"],
        crawl_hyperlinks=settings["discovery"]["hyperlinks"]["crawl"],
        store_external_links=settings["discovery"]["external"]["store"],
        crawl_redirects=settings["discovery"]["redirects"]["crawl"],
        capture_link_attributes=settings["link_attributes"]["capture"],
    )


def _scan_records(path):
    bools = {
        "head_not_first",
        "title_outside_head",
        "meta_description_outside_head",
        "canonical_outside_head",
        "directives_outside_head",
        "hreflang_outside_head",
    }
    con = sqlite3.connect(path)
    con.row_factory = sqlite3.Row
    try:
        pages = []
        for row in con.execute(
            "SELECT p.*, u.url FROM pages p JOIN urls u USING(url_id) ORDER BY page_ordinal"
        ):
            data = {}
            for field in dataclasses.fields(PageRecord):
                if field.name == "render_blocking":
                    continue  # Derived source-head evidence has no frozen pages column.
                source = {
                    "url": "url",
                    "redirect_chain": "redirect_chain_json",
                    "hreflang": "hreflang_json",
                    "heading_outline": "heading_outline_json",
                    "link_placement": "link_placement_json",
                    "trust_signals": "trust_signals_json",
                    "duplicate_ids": "duplicate_ids_json",
                    "canonical_chain": "canonical_chain_json",
                    "invalid_link_targets": "invalid_link_targets_json",
                }.get(field.name, field.name)
                value = row[source]
                if source in {"link_placement_json", "trust_signals_json", "duplicate_ids_json"}:
                    # Nullable by design: a scan written before link placement
                    # was recorded measured none, which is not "found none".
                    value = json.loads(value) if value is not None else None
                elif source in {
                    "redirect_chain_json",
                    "hreflang_json",
                    "heading_outline_json",
                    "canonical_chain_json",
                    "invalid_link_targets_json",
                }:
                    value = json.loads(value) if value is not None else []
                elif field.name in bools and value is not None:
                    value = bool(value)
                data[field.name] = value
            pages.append(PageRecord(**data))
        links = [
            LinkEdge(
                source=row["source"],
                destination=row["destination"],
                anchor=row["anchor"],
                nofollow=bool(row["nofollow"]),
                position=row["position"],
                rel=tuple(json.loads(row["rel_json"])),
                target=row["target"],
                raw_href=row["raw_href"],
            )
            for row in con.execute(
                "SELECT l.*, s.url AS source, d.url AS destination FROM links l "
                "JOIN urls s ON s.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id "
                "ORDER BY l.link_id"
            )
        ]
        forms = [
            (row["page"], row["method"], row["action"], bool(row["has_password"]))
            for row in con.execute(
                "SELECT u.url AS page, method, action, has_password FROM forms "
                "JOIN urls u ON u.url_id=forms.page_url_id ORDER BY form_id"
            )
        ]
        decisions = Counter(
            (row["url"], row["reason"])
            for row in con.execute("SELECT url, reason FROM decisions ORDER BY decision_id")
        )
        from seohead.storage.native_scan import read_effective_config

        config = read_effective_config(con)
    finally:
        con.close()
    return pages, links, forms, decisions, config


def test_sqlite_adapter_matches_legacy_observations_and_decisions(monkeypatch, tmp_path):
    import seohead.crawl.collect as collect

    tick = iter(range(1_000_000))
    monkeypatch.setattr(collect.time, "monotonic", lambda: next(tick))
    long_path = "/" + "x" * 80
    responses = {
        "https://example.test/robots.txt": _Response(
            200, "User-agent: SEOHEAD-Tools\nAllow: /\n", {"content-type": "text/plain"}
        ),
        "https://example.test/": _Response(
            200,
            "<html><body><div id='custom'><a href='/facet?a=1'>A</a>"
            "<a href='/facet?b=2'>B</a><a href='/nofollow' rel='nofollow'>N</a>"
            "<a href='https://off.example/out'>Off</a><a href='" + long_path + "'>Long</a>"
            "<a href='/redir'>Redirect</a><form method='post' action='/send'>"
            "<input type='password'></form></div></body></html>",
        ),
        "https://example.test/facet?a=1": _Response(200, "<html><body>Facet</body></html>"),
        "https://example.test/redir": _Response(
            301, "", {"content-type": "text/html", "location": "https://off.example/target"}
        ),
    }
    settings = load(
        overrides={
            "speed.min_delay_seconds": 0,
            "speed.concurrency": 3,
            "limits.max_urls": 10,
            "limits.max_query_variants_per_path": 1,
            "limits.max_url_length": 50,
            "link_attributes.capture": True,
            "link_position.classify": True,
        }
    )
    content_area = {"include_selector": "#custom"}
    legacy = _legacy(settings, _fetcher(responses), tmp_path / "decisions.jsonl", content_area)
    scan_path = tmp_path / "scan.sqlite"
    crawl_to_scan(
        "https://example.test/",
        scan_out=str(scan_path),
        settings=settings,
        content_area_config=content_area,
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        fetcher=_fetcher(responses),
        sleeper=lambda _seconds: None,
    )
    pages, links, forms, decisions, config = _scan_records(scan_path)
    # Compare shared stored scalar observations; retained asset checks separately
    # verify source-head render-blocking evidence rather than a physical column.
    assert [
        {key: value for key, value in dataclasses.asdict(page).items() if key != "render_blocking"}
        for page in pages
    ] == [
        {key: value for key, value in dataclasses.asdict(page).items() if key != "render_blocking"}
        for page in legacy.pages
    ]
    assert [dataclasses.asdict(link) for link in links] == [
        dataclasses.asdict(link) for link in legacy.links
    ]
    assert forms == [
        (form.page, form.method, form.action, form.has_password) for form in legacy.forms
    ]
    legacy_decisions = Counter(
        (row["url"], row["reason"])
        for row in (
            json.loads(line) for line in (tmp_path / "decisions.jsonl").read_text().splitlines()
        )
    )
    assert decisions == legacy_decisions
    assert config == settings


def test_sqlite_adapter_uses_guarded_redirect_destination_resolution(tmp_path):
    responses = {
        "https://example.test/robots.txt": _Response(
            200, "User-agent: SEOHEAD-Tools\nAllow: /\n", {"content-type": "text/plain"}
        ),
        "https://example.test/": _Response(
            301, "", {"content-type": "text/html", "location": "/land"}
        ),
        "https://example.test/land": _Response(200, "<html><body>Land</body></html>"),
    }
    settings = load(
        overrides={
            "speed.min_delay_seconds": 0,
            "discovery.resolve_redirect_destination": True,
        }
    )
    scan_path = tmp_path / "resolved.sqlite"
    crawl_to_scan(
        "https://example.test/",
        scan_out=str(scan_path),
        settings=settings,
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        fetcher=_fetcher(responses),
        sleeper=lambda _seconds: None,
    )
    con = sqlite3.connect(scan_path)
    try:
        chain, final = con.execute(
            "SELECT redirect_chain_json, final_url FROM pages ORDER BY page_ordinal LIMIT 1"
        ).fetchone()
    finally:
        con.close()
    hops = json.loads(chain)
    assert [(hop["url"], hop["status_code"], hop["error"]) for hop in hops] == [
        ("https://example.test/land", 200, "")
    ]
    assert hops[0]["location"] == ""
    assert isinstance(hops[0]["response_time"], float | int)
    assert final == "https://example.test/land"
