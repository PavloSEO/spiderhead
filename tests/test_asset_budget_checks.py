"""Asset findings use retained measurements and preserve unavailable inputs."""

from __future__ import annotations

import json

import pandas as pd
import pytest

from seohead.checks.asset_weight import resource_delivery_evidence
from seohead.crawl.settings import load
from seohead.sf.config import ConfigError as AuditConfigError
from seohead.sf.config import load_config, validate_config
from seohead.sf.core.asset_checks import (
    RENDER_CHECKS,
    RESOURCE_CHECKS,
    TRACE_CHECKS,
    check_lighthouse_reports,
    check_render_assets,
    check_resource_assets,
)
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.storage import ScanError, open_scan
from seohead.storage.native_scan import NativeScan
from seohead.storage.resource_graph import capture
from tests.test_batch_resource_graph import (
    _Response,
    _v2_metadata,
)
from tests.test_batch_resource_graph import (
    _store_html as _original_store_html,
)
from tests.test_native_capture import _renderer
from tests.test_scan_native import _record

URL = "https://example.test/"


@pytest.fixture(autouse=True)
def offline_dns(monkeypatch):
    monkeypatch.setattr(
        "socket.getaddrinfo", lambda *args, **kwargs: [(2, 1, 6, "", ("93.184.216.34", 443))]
    )


def _store_html(scan, html):
    page_id = _original_store_html(scan, html)
    scan.con.execute(
        "UPDATE pages SET status_code=200,size_bytes=? WHERE url_id=?",
        (len(html.encode()), page_id),
    )
    from seohead.storage.resources import put_declarations

    document_id = scan.con.execute(
        "SELECT document_id FROM pages WHERE url_id=?", (page_id,)
    ).fetchone()[0]
    put_declarations(
        scan.con,
        page_url_id=page_id,
        document_id=document_id,
        representation="static",
        declarations=[],
        fetch_enabled=True,
    )
    scan._sync_corpus()
    return page_id


def _ctx(con=None):
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {
                "Address": URL,
                "Status Code": 200,
                "Content Type": "text/html",
                "Indexability": "Indexable",
            }
        ]
    )
    ctx = AuditContext(exports, load_config(None))
    ctx.scan_con = con
    return ctx


def _codes(ctx):
    return {i.check for i in ctx.issues}


def _skips(ctx):
    return {i.id: i.reason for i in ctx.skipped}


def test_missing_asset_evidence_is_never_clean():
    ctx = _ctx()
    check_resource_assets(ctx)
    check_render_assets(ctx)
    check_lighthouse_reports(ctx)
    assert ctx.issues == []
    assert set(_skips(ctx)) == set(RESOURCE_CHECKS) | set(RENDER_CHECKS) | set(TRACE_CHECKS)
    assert all("native crawl" in reason for reason in _skips(ctx).values())


def test_retained_resource_delivery_fires_and_reopens_without_refetch(tmp_path):
    path = tmp_path / "assets.sqlite"
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    script = b"/* core-js */\nfunction f() {\n  return 1;\n}\n" * 200
    css = b"@font-face { font-family: Test; src: url('/font.woff2'); }"
    bodies = {
        "a.js": (script, "text/javascript"),
        "b.js": (script, "text/javascript"),
        "app.css": (css, "text/css"),
        "big.png": (b"x" * (2 * 1024 * 1024 + 1), "image/png"),
        "font.woff2": (b"font", "font/woff2"),
    }
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(
            scan,
            '<script src="/a.js"></script><script src="/b.js"></script>'
            '<link rel="stylesheet" href="/app.css"><img src="/big.png">',
        )

        def fetcher(url):
            body, media = bodies[url.rsplit("/", 1)[-1]]
            return _Response(200, body, {"content-type": media})

        capture(scan, settings, fetcher=fetcher, wait=lambda: None, clock=lambda: 0.0)
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert _codes(ctx) == set(RESOURCE_CHECKS) - {"HEAVY_PAGE_WEIGHT"}
        assert {
            "ASSET_SHORT_CACHE",
            "FONT_DISPLAY_MISSING",
            "UNMINIFIED_CSS_JS",
            "LEGACY_JS",
        } <= _codes(ctx)
        assert ctx.asset_measurements["HEAVY_PAGE_WEIGHT"]["state"] == "partial"
        assert "HEAVY_PAGE_WEIGHT" in _skips(ctx)
        large = next(i for i in ctx.issues if i.check == "RESOURCE_OVER_2MB")
        assert large.details["source_page"] == URL
        assert large.details["status_code"] == 200
        assert large.target_url == URL + "big.png"
    with open_scan(path, require_audit=False) as con:
        replay = _ctx(con)
        check_resource_assets(replay)
        assert _codes(replay) == _codes(ctx)


def test_resource_budget_thresholds_are_configurable(tmp_path):
    path = tmp_path / "small.sqlite"
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/small.png">')
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"123456", {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        ctx = _ctx(scan.con)
        ctx.thresholds.update(resource_max_bytes=5, page_weight_max_bytes=5)
        check_resource_assets(ctx)
        assert {"RESOURCE_OVER_2MB", "HEAVY_PAGE_WEIGHT"} <= _codes(ctx)
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert not {"RESOURCE_OVER_2MB", "HEAVY_PAGE_WEIGHT"} & _codes(ctx)


def test_different_string_literal_whitespace_is_not_duplicate_javascript():
    headers = (("content-type", "text/javascript"),)
    a = resource_delivery_evidence(b'const x="a b";', "script", headers)
    b = resource_delivery_evidence(b'const x="ab";', "script", headers)
    assert a["sha256"] != b["sha256"]


def test_measured_healthy_assets_are_clean(tmp_path):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "healthy.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, '<script src="/a.js"></script><link rel="stylesheet" href="/app.css">')

        def fetcher(url):
            if url.endswith(".js"):
                return _Response(
                    200,
                    b"const x=1;",
                    {"content-type": "text/javascript", "cache-control": "max-age=31536000"},
                )
            if url.endswith(".css"):
                return _Response(
                    200,
                    b"@font-face{font-family:Test;src:url('/font.woff2');font-display:swap}",
                    {"content-type": "text/css", "cache-control": "max-age=31536000"},
                )
            return _Response(
                200, b"font", {"content-type": "font/woff2", "cache-control": "max-age=31536000"}
            )

        capture(scan, settings, fetcher=fetcher, wait=lambda: None, clock=lambda: 0.0)
        resource_ctx = _ctx(scan.con)
        check_resource_assets(resource_ctx)
        assert not {
            "ASSET_SHORT_CACHE",
            "FONT_DISPLAY_MISSING",
            "UNMINIFIED_CSS_JS",
            "LEGACY_JS",
        } & _codes(resource_ctx)
        for code in ("ASSET_SHORT_CACHE", "FONT_DISPLAY_MISSING", "UNMINIFIED_CSS_JS", "LEGACY_JS"):
            assert resource_ctx.asset_measurements[code]["state"] == "complete"
        _render(
            scan,
            {
                "layout": {"state": "complete", "image_sizing": {"oversized": [], "upscaled": []}},
                "coverage": {
                    "state": "complete",
                    "js": {
                        "resources": [
                            {
                                "url": URL + "a.js",
                                "total_chars": 10,
                                "unused_chars": 0,
                                "unused_ratio": 0,
                            }
                        ]
                    },
                    "css": {"resources": []},
                },
            },
        )
        ctx = _ctx(scan.con)
        check_render_assets(ctx)
        assert not {
            "IMG_INTRINSIC_OVERSIZED",
            "UNUSED_CSS_JS",
        } & _codes(ctx)
        for code in (
            "IMG_INTRINSIC_OVERSIZED",
            "UNUSED_CSS_JS",
        ):
            assert ctx.asset_measurements[code]["state"] == "complete"


def test_untrusted_delivery_headers_and_font_samples_are_bounded():
    body = b"@font-face { font-family: Test; src: url('/font.woff2'); }" * 100
    evidence = resource_delivery_evidence(
        body,
        "stylesheet",
        (("Cache-Control", "max-age=" + "9" * 5000), ("content-type", "text/css")),
    )
    assert evidence["cache"]["max_age"] is None
    assert evidence["font_display_count"] == 100
    assert len(evidence["font_display"]) == 20
    assert evidence["font_display_omitted"] == 80


def test_html_response_to_script_reference_is_unavailable_source_evidence():
    evidence = resource_delivery_evidence(
        b"<html>not found</html>", "script", (("content-type", "text/html"),)
    )
    assert evidence["source_state"] == "unavailable"
    assert evidence["unminified"] is None
    assert evidence["legacy_js"] is None
    assert evidence["sha256"] is None


@pytest.mark.parametrize("value", [0, -1, True, "500", 2.5, float("inf")])
def test_invalid_byte_budgets_are_rejected_at_both_config_boundaries(value):
    from seohead.crawl.settings import ConfigError

    with pytest.raises(ConfigError):
        load(overrides={"analysis.asset_thresholds.page_weight_max_bytes": value})
    config = load_config(None)
    config["thresholds"]["resource_max_bytes"] = value
    with pytest.raises(AuditConfigError):
        validate_config(config)


def test_old_recorded_asset_defaults_keep_the_original_fingerprint():
    import copy

    from seohead.crawl.settings import fingerprint
    from seohead.storage.native_scan import _native_config, _resume_fingerprint

    current = load()
    recorded = copy.deepcopy(current)
    del recorded["analysis"]["asset_thresholds"]
    assert _native_config(recorded, recorded=True) == recorded
    assert _resume_fingerprint(current, recorded) == fingerprint(recorded)
    current["analysis"]["asset_thresholds"]["resource_max_bytes"] = 5
    assert _resume_fingerprint(current, recorded) != fingerprint(recorded)


def test_resource_budget_override_is_preserved_at_both_config_boundaries():
    native = load(overrides={"analysis.asset_thresholds.resource_max_bytes": 12345})
    assert native["analysis"]["asset_thresholds"]["resource_max_bytes"] == 12345
    config = load_config(None)
    config["thresholds"]["resource_max_bytes"] = 12345
    validate_config(config)
    assert config["thresholds"]["resource_max_bytes"] == 12345


def test_render_findings_survive_scan_reopening(tmp_path):
    path = tmp_path / "layout.sqlite"
    evidence = {
        "layout": {
            "state": "complete",
            "min_encoded_bytes": 10240,
            "oversized_ratio": 2,
            "image_sizing": {
                "oversized": [
                    {
                        "src": URL + "large.jpg",
                        "natural": [3000, 100],
                        "rendered": [300, 100],
                        "dpr": 1,
                    }
                ],
                "upscaled": [
                    {
                        "src": URL + "blur.jpg",
                        "natural": [300, 100],
                        "rendered": [600, 100],
                        "dpr": 2,
                    }
                ],
            },
        },
        "coverage": {
            "state": "complete",
            "unit": "UTF-16 code units",
            "js": {
                "resources": [
                    {
                        "url": URL + "cold.js",
                        "total_chars": 40000,
                        "unused_chars": 30000,
                        "unused_ratio": 0.75,
                    }
                ]
            },
            "css": {"resources": []},
        },
    }
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, "<html><body>static</body></html>")
        renderer = _renderer(URL)
        renderer["settings"]["asset_evidence"] = evidence
        scan.commit_render(
            URL,
            {**_record(URL), "representation": "rendered"},
            html="<html><body>rendered</body></html>",
            renderer=renderer,
            captured_at="2026-10-11T12:00:00Z",
        )
    with open_scan(path, require_audit=False) as con:
        ctx = _ctx(con)
        check_render_assets(ctx)
        assert _codes(ctx) == set(RENDER_CHECKS)
        assert {"IMG_INTRINSIC_OVERSIZED", "UNUSED_CSS_JS"} <= _codes(ctx)
        oversized = next(i for i in ctx.issues if i.check == "IMG_INTRINSIC_OVERSIZED")
        assert oversized.details["source_page"] == URL
        assert oversized.details["natural"] == [3000, 100]
        unused = next(i for i in ctx.issues if i.check == "UNUSED_CSS_JS")
        assert unused.details["unit"] == "UTF-16 code units"


@pytest.mark.parametrize("score,expected", [(0, True), (0.5, True), (1, False), (None, False)])
def test_lighthouse_per_url_results_preserve_scores_and_unavailable(tmp_path, score, expected):
    path = tmp_path / "lighthouse.json"
    path.write_text(
        json.dumps(
            {
                "finalUrl": URL,
                "lighthouseVersion": "13.4.1",
                "audits": {
                    ident: {"score": score, "details": {"items": [{"url": URL + "asset.js"}]}}
                    for ident in TRACE_CHECKS.values()
                },
            }
        )
    )
    ctx = _ctx()
    ctx.config["input"]["lighthouse_reports"] = {URL: str(path)}
    check_lighthouse_reports(ctx)
    assert _codes(ctx) == (set(TRACE_CHECKS) if expected else set())
    assert set(_skips(ctx)) == (set(TRACE_CHECKS) if score is None else set())
    if expected:
        assert {
            "LCP_DISCOVERY",
            "NETWORK_DEPENDENCY_TREE",
            "LAYOUT_SHIFT_CULPRITS",
            "IMAGE_DELIVERY",
            "FORCED_REFLOW",
            "JS_EXECUTION_TIME",
        } <= _codes(ctx)
    elif score == 1:
        assert not {
            "LCP_DISCOVERY",
            "NETWORK_DEPENDENCY_TREE",
            "LAYOUT_SHIFT_CULPRITS",
            "IMAGE_DELIVERY",
            "FORCED_REFLOW",
            "JS_EXECUTION_TIME",
        } & _codes(ctx)
    for item in ctx.issues:
        assert item.details["input_sha256"]
        assert str(tmp_path) not in json.dumps(item.details)


def test_lighthouse_cannot_attach_another_urls_measurement(tmp_path):
    path = tmp_path / "wrong.json"
    path.write_text(json.dumps({"finalUrl": URL + "other", "audits": {}}))
    ctx = _ctx()
    ctx.config["input"]["lighthouse_reports"] = {URL: str(path)}
    check_lighthouse_reports(ctx)
    assert not ctx.issues
    assert set(_skips(ctx)) == set(TRACE_CHECKS)


def _second_page(scan, *, status=200, content_type="text/html"):
    """Add a crawl page without renderer evidence to exercise the denominator."""
    original = scan.con.execute("SELECT * FROM pages LIMIT 1").fetchone()
    scan.con.execute("INSERT INTO urls(url) VALUES (?)", (URL + "second",))
    page_id = scan.con.execute("SELECT url_id FROM urls WHERE url=?", (URL + "second",)).fetchone()[
        0
    ]
    columns = [r[1] for r in scan.con.execute("PRAGMA table_info(pages)")]
    values = dict(zip(columns, original, strict=True))
    values.update(
        url_id=page_id,
        page_ordinal=1,
        document_id=None,
        status_code=status,
        content_type=content_type,
    )
    scan.con.execute(
        "INSERT INTO pages("
        + ",".join(columns)
        + ") VALUES("
        + ",".join("?" for _ in columns)
        + ")",
        list(values.values()),
    )


def _render(scan, evidence):
    renderer = _renderer(URL)
    renderer["settings"]["asset_evidence"] = evidence
    scan.commit_render(
        URL,
        {**_record(URL), "status_code": 200, "representation": "rendered"},
        html="<html>rendered</html>",
        renderer=renderer,
        captured_at="2026-10-11T12:00:00Z",
    )
    scan.con.execute("UPDATE pages SET status_code=200")


def _scope(ctx, code, eligible, measured, partial=0):
    summary = ctx.asset_measurements[code]
    assert summary["eligible"] == eligible
    assert summary["measured"] == measured
    assert summary["partial"] == partial
    assert (
        summary["unavailable"] == eligible - measured
        if eligible is not None
        else summary["unavailable"] is None
    )
    return summary


@pytest.mark.parametrize("kind,media", [("module", "text/javascript"), ("css_import", "text/css")])
def test_module_and_css_import_delivery_use_source_analysis(kind, media):
    body = (
        b"/* core-js */\nfunction f() {\n  return 1;\n}\n" * 200
        if kind == "module"
        else b"@font-face { font-family: Test; src: url('/font.woff2'); }"
    )
    info = resource_delivery_evidence(body, kind, (("content-type", media),))
    assert info["source_state"] == "complete"
    if kind == "module":
        assert info["legacy_js"] is True
        assert info["sha256"]
    else:
        assert info["font_display_count"] == 1
    wrong = resource_delivery_evidence(body, kind, (("content-type", "text/html"),))
    assert wrong["source_state"] == "unavailable"


@pytest.mark.parametrize("absent", ["fetch-row", "delivery-json", "invalid-json", "source-mime"])
def test_missing_resource_measurements_do_not_report_clean(tmp_path, absent):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "missing.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, '<script src="/a.js"></script><script src="/b.js"></script>')
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(
                200,
                b"const x=1;",
                {"content-type": "text/javascript", "cache-control": "max-age=31536000"},
            ),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        if absent == "fetch-row":
            scan.con.execute(
                "DELETE FROM resource_graph_fetches WHERE resolved_url=?", (URL + "b.js",)
            )
            scan.con.execute(
                "UPDATE resource_graph_occurrences SET state='budget' WHERE resolved_url=?",
                (URL + "b.js",),
            )
        else:
            payload = (
                None
                if absent == "delivery-json"
                else "{"
                if absent == "invalid-json"
                else json.dumps(
                    resource_delivery_evidence(
                        b"<html>oops</html>", "script", (("content-type", "text/html"),)
                    )
                )
            )
            scan.con.execute(
                "UPDATE resource_graph_fetches SET delivery_json=? WHERE resolved_url=?",
                (payload, URL + "b.js"),
            )
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert _scope(ctx, "LEGACY_JS", 2, 1)["state"] == "partial"
        assert "LEGACY_JS" in _skips(ctx)
        assert _scope(ctx, "UNMINIFIED_CSS_JS", 2, 1)["state"] == "partial"
        if absent == "fetch-row":
            assert _scope(ctx, "RESOURCE_OVER_2MB", 2, 1)["state"] == "partial"
        assert "DUPLICATED_JS" in _skips(ctx)


def test_resource_findings_survive_partial_resource_scope_and_aggregate(tmp_path):
    from seohead.sf.core.aggregate import aggregate

    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "partial.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, '<img src="/a.png"><img src="/missing.png">')
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"123456", {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        scan.con.execute(
            "DELETE FROM resource_graph_fetches WHERE resolved_url=?", (URL + "missing.png",)
        )
        ctx = _ctx(scan.con)
        ctx.thresholds["resource_max_bytes"] = 5
        check_resource_assets(ctx)
        assert "RESOURCE_OVER_2MB" in _codes(ctx)
        assert "RESOURCE_OVER_2MB" not in _skips(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", 2, 1)["state"] == "partial"
        result = aggregate(ctx, {}, {}, {})
        assert result.summary["asset_measurements"]["RESOURCE_OVER_2MB"]["unavailable"] == 1


def test_omitted_declarations_make_resource_denominator_unknown(tmp_path):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "omitted.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, '<img src="/a.png">')
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"123", {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        scan.con.execute(
            "UPDATE context_items SET completeness='partial' WHERE kind='resource_graph_coverage'"
        )
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", None, 1)["state"] == "partial"
        assert "RESOURCE_OVER_2MB" in _skips(ctx)


def test_module_css_import_and_duplicate_scope_in_retained_graph(tmp_path):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    script = b"/* core-js */\nfunction f() {\n  return 1;\n}\n" * 200
    with NativeScan.create(
        tmp_path / "modules.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(
            scan,
            '<script type="module" src="/a.js"></script><script src="/b.js"></script><link rel="stylesheet" href="/app.css">',
        )

        def fetcher(url):
            if url.endswith(".js"):
                return _Response(200, script, {"content-type": "text/javascript"})
            css = (
                b'@import "/nested.css";'
                if url.endswith("app.css")
                else b"@font-face { font-family: Test; src: url('/font.woff2'); }"
            )
            return (
                _Response(200, css, {"content-type": "text/css"})
                if url.endswith(".css")
                else _Response(200, b"font", {"content-type": "font/woff2"})
            )

        capture(scan, settings, fetcher=fetcher, wait=lambda: None, clock=lambda: 0.0)
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert "DUPLICATED_JS" in _codes(ctx)
        assert _scope(ctx, "LEGACY_JS", 2, 2)["state"] == "complete"
        assert _scope(ctx, "FONT_DISPLAY_MISSING", 2, 2)["state"] == "complete"
        assert any(
            i.check == "FONT_DISPLAY_MISSING" and i.target_url.endswith("nested.css")
            for i in ctx.issues
        )


@pytest.mark.parametrize("rendered", [False, True])
def test_runtime_weight_never_sums_unselected_declarations_or_html_twice(tmp_path, rendered):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    html = '<img srcset="/small.png 1x, /huge.png 2x"><script nomodule src="/legacy.js"></script><link rel="stylesheet" href="/app.css">'
    with NativeScan.create(
        tmp_path / "variants.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, html)
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"x" * 2000, {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        if rendered:
            _render(
                scan,
                {
                    "weight": {
                        "state": "complete",
                        "bytes": 500,
                        "html_bytes": len(html),
                        "image_bytes": 5,
                        "font_bytes": 0,
                        "third_party_bytes": 0,
                        "unit": "content-decoded bytes",
                        "scope": "current pinned-browser navigation window",
                    }
                },
            )
        ctx = _ctx(scan.con)
        ctx.thresholds["page_weight_max_bytes"] = 550
        check_resource_assets(ctx)
        assert "HEAVY_PAGE_WEIGHT" not in _codes(ctx)
        assert _scope(ctx, "HEAVY_PAGE_WEIGHT", 1, 1, 0 if rendered else 1)["state"] == (
            "complete" if rendered else "partial"
        )
        assert ("HEAVY_PAGE_WEIGHT" in _skips(ctx)) is not rendered


def test_html_only_heavy_page_fires_without_any_resource_occurrences(tmp_path):
    with NativeScan.create(
        tmp_path / "html.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, "x" * (3 * 1024 * 1024))
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        heavy = next(i for i in ctx.issues if i.check == "HEAVY_PAGE_WEIGHT")
        assert heavy.details["bytes"] == 3 * 1024 * 1024
        assert heavy.details["size_state"] == "lower_bound"
        assert heavy.details["image_bytes"] is None
        assert _scope(ctx, "HEAVY_PAGE_WEIGHT", 1, 1, 1)["state"] == "partial"


def test_unrendered_page_remains_in_render_coverage_denominator(tmp_path):
    with NativeScan.create(
        tmp_path / "two-pages.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, "<html>one</html>")
        _render(
            scan,
            {
                "layout": {"state": "complete", "image_sizing": {}},
                "coverage": {"state": "complete", "js": {}, "css": {}},
            },
        )
        _second_page(scan)
        ctx = _ctx(scan.con)
        check_render_assets(ctx)
        for code in RENDER_CHECKS:
            assert _scope(ctx, code, 2, 1)["state"] == "partial"
            assert code in _skips(ctx)


def test_unreported_html_page_keeps_clean_lighthouse_result_partial(tmp_path):
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {"Address": URL + suffix, "Status Code": status, "Content Type": media}
            for suffix, status, media in [
                ("", 200, "text/html"),
                ("second", 200, "text/html"),
                ("error", 404, "text/html"),
                ("file", 200, "text/plain"),
            ]
        ]
    )
    ctx = AuditContext(exports, load_config(None))
    report = tmp_path / "lhr.json"
    report.write_text(
        json.dumps(
            {"finalUrl": URL, "audits": {ident: {"score": 1} for ident in TRACE_CHECKS.values()}}
        )
    )
    ctx.config["input"]["lighthouse_reports"] = {URL: str(report)}
    check_lighthouse_reports(ctx)
    for code in TRACE_CHECKS:
        assert _scope(ctx, code, 2, 1)["state"] == "partial"
        assert code in _skips(ctx)
    assert not ctx.issues


def test_same_module_and_script_url_has_one_resource_measurement(tmp_path):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "same-url.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(
            scan, '<script type="module" src="/a.js"></script><script src="/a.js"></script>'
        )
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"const x=1;", {"content-type": "text/javascript"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", 1, 1)["state"] == "complete"
        assert _scope(ctx, "LEGACY_JS", 1, 1)["state"] == "complete"
        assert "DUPLICATED_JS" not in _codes(ctx)


def test_partial_browser_weight_fires_without_losing_scope(tmp_path):
    with NativeScan.create(
        tmp_path / "weight-partial.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, "<html>small</html>")
        _render(
            scan,
            {
                "weight": {
                    "state": "partial",
                    "bytes": 3 * 1024 * 1024,
                    "html_bytes": 10,
                    "responses_unmeasured": 1,
                }
            },
        )
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        heavy = next(i for i in ctx.issues if i.check == "HEAVY_PAGE_WEIGHT")
        assert heavy.details["size_state"] == "lower_bound"
        assert heavy.details["responses_unmeasured"] == 1
        assert _scope(ctx, "HEAVY_PAGE_WEIGHT", 1, 1, 1)["state"] == "partial"
        assert "HEAVY_PAGE_WEIGHT" not in _skips(ctx)


def test_partial_render_finding_and_unrendered_page_preserve_counts(tmp_path):
    with NativeScan.create(
        tmp_path / "layout-partial.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, "<html>one</html>")
        _render(
            scan,
            {
                "layout": {
                    "state": "partial",
                    "image_sizing": {
                        "upscaled": [
                            {
                                "src": URL + "small.jpg",
                                "natural": [100, 100],
                                "rendered": [500, 500],
                            }
                        ]
                    },
                },
                "coverage": {"state": "partial", "js": {}, "css": {}},
            },
        )
        _second_page(scan)
        ctx = _ctx(scan.con)
        check_render_assets(ctx)
        assert "IMG_UPSCALED" in _codes(ctx)
        assert _scope(ctx, "IMG_UPSCALED", 2, 1, 1)["state"] == "partial"
        assert "IMG_UPSCALED" not in _skips(ctx)
        assert _scope(ctx, "UNUSED_CSS_JS", 2, 1, 1)["state"] == "partial"
        assert "UNUSED_CSS_JS" in _skips(ctx)


def test_lighthouse_finding_keeps_unreported_page_in_summary(tmp_path):
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {"Address": URL + suffix, "Status Code": 200, "Content Type": "text/html"}
            for suffix in ("", "second")
        ]
    )
    ctx = AuditContext(exports, load_config(None))
    report = tmp_path / "lhr-finding.json"
    report.write_text(
        json.dumps(
            {"finalUrl": URL, "audits": {ident: {"score": 0} for ident in TRACE_CHECKS.values()}}
        )
    )
    ctx.config["input"]["lighthouse_reports"] = {URL: str(report)}
    check_lighthouse_reports(ctx)
    for code in TRACE_CHECKS:
        assert code in _codes(ctx)
        assert code not in _skips(ctx)
        assert _scope(ctx, code, 2, 1)["state"] == "partial"


@pytest.mark.parametrize("coverage", ["missing", "invalid-json", "omitted"])
def test_unusable_declaration_payload_keeps_denominator_unknown(tmp_path, coverage):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "declarations.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, '<img src="/a.png">')
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"123", {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        if coverage == "missing":
            scan.con.execute("DELETE FROM context_items WHERE kind='resource_graph_coverage'")
        else:
            payload = (
                "{"
                if coverage == "invalid-json"
                else json.dumps({"state": "complete", "omitted": 1})
            )
            scan.con.execute(
                "UPDATE context_items SET payload_json=? WHERE kind='resource_graph_coverage'",
                (payload,),
            )
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", None, 1)["state"] == "partial"


def test_render_without_weight_retains_captured_html_lower_bound(tmp_path):
    with NativeScan.create(
        tmp_path / "render-html.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, "x" * (3 * 1024 * 1024))
        _render(scan, {"weight": {"state": "unavailable", "reason": "no route observer"}})
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        heavy = next(i for i in ctx.issues if i.check == "HEAVY_PAGE_WEIGHT")
        assert heavy.details["bytes"] == 3 * 1024 * 1024
        assert heavy.details["html_bytes"] == 3 * 1024 * 1024
        assert heavy.details["source"] == "native-html-response"


def test_stale_render_declarations_do_not_enter_active_resource_scope(tmp_path):
    from seohead.storage.resource_graph import store_document

    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "stale-render.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        page_id = _store_html(scan, "<html>static</html>")
        _render(scan, {})
        old_document = scan.con.execute(
            "SELECT document_id FROM pages WHERE url_id=?", (page_id,)
        ).fetchone()[0]
        store_document(
            scan.con,
            page_url_id=page_id,
            source_document_id=old_document,
            representation="rendered",
            html='<img src="/old.png">',
        )
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"x" * 20, {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        _render(scan, {"layout": {"state": "complete", "image_sizing": {}}})
        new_document = scan.con.execute(
            "SELECT document_id FROM pages WHERE url_id=?", (page_id,)
        ).fetchone()[0]
        assert new_document != old_document
        store_document(
            scan.con,
            page_url_id=page_id,
            source_document_id=new_document,
            representation="rendered",
            html='<img src="/new.png">',
        )
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"x", {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        ctx = _ctx(scan.con)
        ctx.thresholds["resource_max_bytes"] = 10
        check_resource_assets(ctx)
        assert "RESOURCE_OVER_2MB" not in _codes(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", 1, 1)["state"] == "complete"


@pytest.mark.parametrize("rendered", [False, True])
def test_unselected_future_static_document_cannot_change_active_assets_or_html_weight(
    tmp_path, rendered
):
    from seohead.storage.corpus import store_response
    from seohead.storage.resource_graph import store_document
    from seohead.storage.resources import put_declarations
    from tests.test_native_capture import _event

    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "future-static.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        page_id = _store_html(scan, "<html>small</html>")
        if rendered:
            _render(scan, {})
        policy = json.loads(scan.con.execute("SELECT retention_json FROM scan").fetchone()[0])
        html = '<img src="/future.png">' + "x" * (3 * 1024 * 1024)
        _, future_id = store_response(
            scan.con, _event(URL, html.encode()), purpose="page", policy=policy
        )
        assert (
            future_id
            != scan.con.execute(
                "SELECT document_id FROM pages WHERE url_id=?", (page_id,)
            ).fetchone()[0]
        )
        store_document(
            scan.con,
            page_url_id=page_id,
            source_document_id=future_id,
            representation="static",
            html=html,
        )
        if not rendered:
            # Even a completed inventory for a different document cannot supersede
            # the explicitly selected static page.
            put_declarations(
                scan.con,
                page_url_id=page_id,
                document_id=future_id,
                representation="static",
                declarations=[],
                fetch_enabled=True,
            )
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(200, b"x" * 20, {"content-type": "image/png"}),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        ctx = _ctx(scan.con)
        ctx.thresholds["resource_max_bytes"] = 10
        check_resource_assets(ctx)
        assert not {"RESOURCE_OVER_2MB", "HEAVY_PAGE_WEIGHT"} & _codes(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", None if rendered else 0, 0)["state"] == (
            "unavailable" if rendered else "complete"
        )


def test_missing_page_document_envelope_keeps_asset_scope_unavailable(tmp_path):
    with NativeScan.create(
        tmp_path / "no-document.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, "<html>small</html>")
        scan.con.execute("UPDATE pages SET document_id=NULL")
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", None, 0)["state"] == "unavailable"
        check_render_assets(ctx)
        assert _scope(ctx, "IMG_UPSCALED", 1, 0)["state"] == "unavailable"


@pytest.mark.parametrize(
    "headers",
    [
        {"content-range": "bytes 0-1/10000000"},
        {"content-range": "invalid"},
        {},
        {"content-type": "multipart/byteranges; boundary=fixture"},
    ],
)
def test_http206_fragment_is_partial_resource_evidence_without_whole_source_analysis(
    tmp_path, headers
):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "range.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, '<script src="/range.js"></script>')
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(
                206,
                b"xx",
                {"content-type": "text/javascript", "cache-control": "max-age=31536000", **headers},
            ),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        row = scan.con.execute(
            "SELECT state,body_state,reason,bytes_received,delivery_json FROM resource_graph_fetches"
        ).fetchone()
        assert row["state"] == row["body_state"] == "partial"
        assert "206" in row["reason"]
        assert row["bytes_received"] == 2
        assert row["delivery_json"] is None
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", 1, 1, 1)["state"] == "partial"
        assert "RESOURCE_OVER_2MB" in _skips(ctx)
        assert "RESOURCE_OVER_2MB" not in _codes(ctx)
        for code in ("UNMINIFIED_CSS_JS", "LEGACY_JS", "DUPLICATED_JS"):
            assert _scope(ctx, code, 1, 0)["state"] == "unavailable"
            assert code in _skips(ctx)
        ctx = _ctx(scan.con)
        ctx.thresholds["resource_max_bytes"] = 1
        check_resource_assets(ctx)
        finding = next(i for i in ctx.issues if i.check == "RESOURCE_OVER_2MB")
        assert finding.details["bytes"] == 2
        assert finding.details["size_state"] == "lower_bound"
        assert _scope(ctx, "RESOURCE_OVER_2MB", 1, 1, 1)["state"] == "partial"
        # The server's declared representation size is never reported as observed bytes.
        assert 10000000 not in finding.details.values()
    with open_scan(tmp_path / "range.sqlite", require_audit=False) as con:
        replay = _ctx(con)
        check_resource_assets(replay)
        assert _scope(replay, "RESOURCE_OVER_2MB", 1, 1, 1)["state"] == "partial"
        assert _scope(replay, "UNMINIFIED_CSS_JS", 1, 0)["state"] == "unavailable"


def test_old_complete_http206_measurement_replays_as_partial_without_duplicate_claim(tmp_path):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    with NativeScan.create(
        tmp_path / "old-range.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, '<script src="/a.js"></script><script src="/b.js"></script>')
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(
                200,
                b"const x=1;",
                {"content-type": "text/javascript", "cache-control": "max-age=31536000"},
            ),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        scan.con.execute("UPDATE resource_graph_fetches SET status_code=206")
        ctx = _ctx(scan.con)
        check_resource_assets(ctx)
        assert _scope(ctx, "RESOURCE_OVER_2MB", 2, 2, 2)["state"] == "partial"
        assert _scope(ctx, "UNMINIFIED_CSS_JS", 2, 0)["state"] == "unavailable"
        assert _scope(ctx, "DUPLICATED_JS", 2, 0)["state"] == "unavailable"
        assert "DUPLICATED_JS" not in _codes(ctx)


@pytest.mark.parametrize(
    "value,expected",
    [
        ("s-max-age=604800, max-age=0", 0),
        ("max-age=0, s-max-age=604800", 0),
        ("s-max-age=604800", None),
    ],
)
def test_shared_cache_ttl_never_masquerades_as_browser_cache_ttl(value, expected):
    evidence = resource_delivery_evidence(
        b"const x=1;", "script", (("content-type", "text/javascript"), ("cache-control", value))
    )
    assert evidence["cache"]["ok"] is False
    assert evidence["cache"]["max_age"] == expected


def test_unicode_font_samples_fit_retained_envelope_and_scan_reopens(tmp_path):
    settings = load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True})
    css = ("@font-face { font-family: '" + "漢" * 200 + "'; }\n") * 100
    evidence = resource_delivery_evidence(
        css.encode(), "stylesheet", (("content-type", "text/css; charset=utf-8"),)
    )
    assert len(json.dumps(evidence, ensure_ascii=True)) <= 16 * 1024
    assert evidence["font_display_count"] == 100
    assert 0 < len(evidence["font_display"]) < 20
    assert evidence["font_display_omitted"] == 100 - len(evidence["font_display"])
    path = tmp_path / "unicode-font.sqlite"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<link rel="stylesheet" href="/font.css">')
        capture(
            scan,
            settings,
            fetcher=lambda _: _Response(
                200, css.encode(), {"content-type": "text/css; charset=utf-8"}
            ),
            wait=lambda: None,
            clock=lambda: 0.0,
        )
        payload = scan.con.execute("SELECT delivery_json FROM resource_graph_fetches").fetchone()[0]
        assert len(payload) <= 16 * 1024
        assert json.loads(payload)["font_display_count"] == 100
    with open_scan(path, require_audit=False) as con:
        ctx = _ctx(con)
        check_resource_assets(ctx)
        finding = next(i for i in ctx.issues if i.check == "FONT_DISPLAY_MISSING")
        assert finding.details["font_faces_count"] == 100
        assert finding.details["font_faces_omitted"] == 100 - len(finding.details["font_faces"])


@pytest.mark.parametrize("resource_limit", [2 * 1024 * 1024, 12345])
def test_native_asset_thresholds_keep_frozen_header_and_roundtrip(tmp_path, resource_limit):
    import copy

    from seohead.crawl.settings import fingerprint
    from seohead.mcp.scan_handlers import resume_inputs
    from seohead.storage.native_scan import NativeScan, read_effective_config
    from tests.test_scan_native import _metadata

    metadata = _metadata(**{"analysis.asset_thresholds.resource_max_bytes": resource_limit})
    expected = copy.deepcopy(metadata["config"])
    thresholds = expected["analysis"].pop("asset_thresholds")
    header_expected = copy.deepcopy(expected)
    header_expected["scope"].pop("normalize_tracking_parameters")
    for name in ("auth", "auth_acknowledged", "zstd"):
        header_expected["http"].pop(name)
    header_expected["rendering"].pop("actions")
    header_expected["rendering"].pop("presets")
    header_expected["rendering"]["artifacts"].pop("screenshot_mode")
    header_expected["analysis"].pop("redirect_hop_hsts_probe")
    path = tmp_path / "scan.sqlite"
    with NativeScan.create(path, **metadata) as scan:
        header = dict(scan.con.execute("SELECT * FROM scan").fetchone())
        assert json.loads(header["config_json"]) == header_expected
        assert header["config_fingerprint"] == fingerprint(header_expected)
        assert read_effective_config(scan.con)["analysis"]["asset_thresholds"] == thresholds
    assert resume_inputs(str(path))["settings"]["analysis"]["asset_thresholds"] == thresholds
    with NativeScan.open(path, expected_config=metadata["config"]):
        pass
    mismatch = copy.deepcopy(metadata["config"])
    mismatch["analysis"]["asset_thresholds"]["resource_max_bytes"] = resource_limit + 1
    with pytest.raises(ScanError, match="configuration differs"):
        NativeScan.open(path, expected_config=mismatch)


@pytest.mark.parametrize(
    "payload",
    [
        {"schema_version": "analysis_asset_thresholds.v0", "thresholds": {}},
        {"schema_version": "analysis_asset_thresholds.v1", "thresholds": {"resource_max_bytes": 5}},
        {
            "schema_version": "analysis_asset_thresholds.v1",
            "thresholds": {"resource_max_bytes": True, "page_weight_max_bytes": 5},
        },
    ],
)
def test_native_asset_threshold_adjunct_rejects_malformed_payload(tmp_path, payload):
    import sqlite3

    from seohead.storage import open_scan_mode
    from seohead.storage.native_scan import NativeScan
    from tests.test_scan_native import _metadata

    path = tmp_path / "scan.sqlite"
    with NativeScan.create(path, **_metadata()):
        pass
    with sqlite3.connect(path) as con:
        con.execute(
            "UPDATE context_items SET payload_json=? WHERE kind='analysis_asset_thresholds'",
            (json.dumps(payload),),
        )
    with pytest.raises(ScanError, match="asset thresholds context"):
        open_scan_mode(path, require_audit=False, light=True)


def test_offline_reanalysis_reuses_asset_threshold_adjunct(tmp_path, monkeypatch):
    from seohead.mcp import handlers
    from seohead.mcp.reanalysis_handlers import reanalyze_scan
    from seohead.storage import open_scan
    from seohead.storage.native_scan import read_effective_config
    from tests import test_scan_reanalysis_integration as fixture

    settings = fixture._settings()
    settings["analysis"]["asset_thresholds"]["resource_max_bytes"] = 12345
    monkeypatch.setattr(fixture, "_settings", lambda **_kwargs: settings)
    source = tmp_path / "source.sqlite"
    derived = tmp_path / "derived.sqlite"
    fixture._source(source)
    attempts = fixture._forbid_network(monkeypatch, [])
    original = handlers._audit_crawl_result
    observed = []

    def audit(result, **kwargs):
        observed.append(kwargs["settings"]["analysis"]["asset_thresholds"])
        return original(result, **kwargs)

    monkeypatch.setattr(handlers, "_audit_crawl_result", audit)
    reanalyze_scan(str(source), str(derived), producer_build="b" * 40)
    assert observed == [settings["analysis"]["asset_thresholds"]]
    assert attempts == {}
    con = open_scan(derived)
    try:
        assert read_effective_config(con)["analysis"]["asset_thresholds"] == observed[0]
    finally:
        con.close()


@pytest.mark.parametrize("legacy_manifest", [False, True])
@pytest.mark.parametrize("mismatched_manifest", [False, True])
def test_native_audit_validates_asset_adjunct_or_original_manifest(
    tmp_path, legacy_manifest, mismatched_manifest
):
    import hashlib
    import sqlite3

    from seohead.crawl.settings import manifest
    from seohead.storage.native_scan import read_effective_config
    from tests.test_scan_reanalysis_integration import _source

    path = tmp_path / "source.sqlite"
    _source(path)
    with sqlite3.connect(path) as con:
        con.row_factory = sqlite3.Row
        if legacy_manifest:
            con.execute("DELETE FROM context_items WHERE kind='analysis_asset_thresholds'")
            con.execute("DELETE FROM context_items WHERE kind='native_config_extension'")
            effective = json.loads(con.execute("SELECT config_json FROM scan").fetchone()[0])
            assert "asset_thresholds" not in effective["analysis"]
        else:
            effective = read_effective_config(con)
        if mismatched_manifest:
            if legacy_manifest:
                effective["limits"]["max_urls"] += 1
            else:
                effective["analysis"]["asset_thresholds"]["resource_max_bytes"] += 1
        document = json.loads(con.execute("SELECT document_json FROM audit").fetchone()[0])
        document["run"]["crawl_config"] = manifest(effective)
        raw = json.dumps(document, ensure_ascii=False, sort_keys=True)
        con.execute(
            "UPDATE audit SET document_json=?,sha256=?",
            (raw, hashlib.sha256(raw.encode("utf-8")).hexdigest()),
        )
    if mismatched_manifest:
        with pytest.raises(ScanError, match="effective configuration disagrees"):
            open_scan(path)
    else:
        con = open_scan(path)
        con.close()
