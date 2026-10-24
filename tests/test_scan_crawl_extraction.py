"""Bounded parser observations and shared native-crawl extraction helpers."""

from __future__ import annotations

import pytest

from seohead.checks.parser import parse_html
from seohead.crawl.collect import _record_from_parsed
from seohead.crawl.spider import LinkEdge, apply_document_links, form_edges
from seohead.crawl.throttle import Throttle


def test_default_parser_output_is_unchanged_when_observation_caps_are_omitted():
    html = """
    <a href="/one">One</a><a href="https://outside.example/two">Two</a>
    <form method="post"><input type="password"></form>
    """
    baseline = parse_html(html, "https://example.test/")
    explicit_none = parse_html(
        html,
        "https://example.test/",
        {"max_link_observations": None, "max_form_observations": None},
    )
    assert explicit_none == baseline
    assert "link_observation" not in baseline
    assert "forms_omitted" not in baseline


def test_capped_parser_keeps_prefix_but_reports_full_link_counts(monkeypatch):
    import seohead.checks.link_position as positions

    calls = []
    original = positions.classify_link

    def counted(*args, **kwargs):
        calls.append(args[0].get("href"))
        return original(*args, **kwargs)

    monkeypatch.setattr(positions, "classify_link", counted)
    parsed = parse_html(
        """
        <a href="/one">One</a><a href="https://outside.example/two">Two</a>
        <a href="/three">Three</a>
        """,
        "https://example.test/",
        {"classify_links": True, "max_link_observations": 1},
    )

    assert [item["href"] for item in parsed["links"]] == ["https://example.test/one"]
    assert parsed["link_observation"] == {
        "stored": 1,
        "total": 3,
        "external_total": 1,
        "omitted": 2,
    }
    assert calls == ["/one"]
    assert _record_from_parsed(parsed)["outlinks"] == 3
    assert _record_from_parsed(parsed)["external_outlinks"] == 1


def test_capped_forms_do_not_build_past_the_retained_prefix():
    parsed = parse_html(
        """
        <form action="/one"><input type="password"></form>
        <form action="/two"></form><form action="/three"></form>
        """,
        "https://example.test/",
        {"max_form_observations": 1},
    )
    forms, omitted = form_edges(parsed, "https://example.test/")
    assert [(form.method, form.action, form.has_password) for form in forms] == [
        ("get", "https://example.test/one", True)
    ]
    assert omitted == 2


def test_invalid_observation_caps_are_rejected_at_the_parser_boundary():
    for options in (
        {"max_link_observations": -1},
        {"max_link_observations": "2"},
        {"max_form_observations": -1},
    ):
        with pytest.raises(ValueError, match="observations"):
            parse_html("<a href='/x'>x</a>", "https://example.test/", options)


def test_shared_link_application_preserves_storage_and_discovery_order():
    parsed = {
        "links": [
            {
                "href": "https://outside.example/x",
                "text": "outside",
                "nofollow": False,
                "rel": "",
                "target": "",
                "raw_href": "https://outside.example/x",
            },
            {
                "href": "https://example.test/no-follow",
                "text": "no follow",
                "nofollow": True,
                "rel": "nofollow",
                "target": "",
                "raw_href": "/no-follow",
            },
            {
                "href": "https://example.test/one#part",
                "text": "one",
                "nofollow": False,
                "rel": "noopener",
                "target": "_blank",
                "raw_href": "/one#part",
            },
            {
                "href": "https://example.test/one",
                "text": "duplicate",
                "nofollow": False,
                "rel": "",
                "target": "",
                "raw_href": "/one",
            },
        ]
    }
    edges: list[LinkEdge] = []
    discovered = []
    rejected = []

    def rejection(url, host):
        assert host == "example.test"
        return "outside_host" if "outside.example" in url else ""

    def discover(target, depth, requested_url):
        discovered.append((target, depth, requested_url))
        return None

    apply_document_links(
        parsed,
        "https://example.test/",
        0,
        depth_limit=2,
        host="example.test",
        rejection=rejection,
        discover=discover,
        store_hyperlinks=True,
        store_external_links=True,
        crawl_hyperlinks=True,
        follow_nofollow=False,
        capture_link_attributes=True,
        record_edge=edges.append,
        reject=lambda reason, url: rejected.append((reason, url)),
        mark_link_partial=lambda omitted: rejected.append(("partial", str(omitted))),
    )

    assert [edge.destination for edge in edges] == [item["href"] for item in parsed["links"]]
    assert edges[2].rel == ("noopener",)
    assert edges[2].target == "_blank"
    assert edges[2].raw_href == "/one#part"
    assert discovered == [
        ("https://example.test/one", 1, "https://example.test/one#part"),
        ("https://example.test/one", 1, "https://example.test/one"),
    ]
    assert rejected == [
        ("outside_host", "https://outside.example/x"),
        ("nofollow", "https://example.test/no-follow"),
    ]


def test_shared_link_application_stops_at_depth_and_marks_only_one_capped_tail():
    edges = []
    discovered = []
    rejected = []
    apply_document_links(
        {
            "links": [{"href": "https://example.test/a", "nofollow": False}],
            "link_observation": {"omitted": 9},
        },
        "https://example.test/",
        2,
        depth_limit=2,
        host="example.test",
        rejection=lambda _url, _host: "",
        discover=lambda target, depth, requested_url: discovered.append(
            (target, depth, requested_url)
        ),
        store_hyperlinks=True,
        store_external_links=True,
        crawl_hyperlinks=True,
        follow_nofollow=False,
        capture_link_attributes=False,
        record_edge=edges.append,
        reject=lambda reason, url: rejected.append((reason, url)),
        mark_link_partial=lambda omitted: rejected.append(("partial", omitted)),
    )
    assert edges == []
    assert discovered == []
    assert rejected == [("depth_limit", None)]

    apply_document_links(
        {"links": [], "link_observation": {"omitted": 9}},
        "https://example.test/",
        0,
        depth_limit=2,
        host="example.test",
        rejection=lambda _url, _host: "",
        discover=lambda _target, _depth, _requested_url: None,
        store_hyperlinks=True,
        store_external_links=True,
        crawl_hyperlinks=True,
        follow_nofollow=False,
        capture_link_attributes=False,
        record_edge=edges.append,
        reject=lambda reason, url: rejected.append((reason, url)),
        mark_link_partial=lambda omitted: rejected.append(("partial", omitted)),
    )
    assert rejected[-1] == ("partial", 9)


def test_throttle_snapshot_round_trips_only_adaptive_state():
    source = Throttle(min_delay=0.5, max_delay=5.0, max_concurrency=4)
    source.record_response(2.0, ok=True)
    source.record_response(2.0, ok=True)
    source.record_timeout()
    state = source.snapshot_state()
    assert set(state) == {"delay_seconds", "concurrency", "consecutive_ok"}

    restored = Throttle(min_delay=0.5, max_delay=5.0, max_concurrency=4)
    restored.restore_state(state)
    assert restored.snapshot_state() == state
    assert restored.timeouts == restored.server_errors == 0


@pytest.mark.parametrize("max_concurrency", [1, 3])
def test_throttle_snapshot_saturates_ceiling_successes_without_changing_future_behavior(
    max_concurrency,
):
    uninterrupted = Throttle(min_delay=0.5, max_delay=5.0, max_concurrency=max_concurrency)
    for _ in range(10):
        uninterrupted.record_response(1.0, ok=True)
    state = uninterrupted.snapshot_state()
    assert state["concurrency"] == max_concurrency
    assert state["consecutive_ok"] == 2

    resumed = Throttle(min_delay=0.5, max_delay=5.0, max_concurrency=max_concurrency)
    resumed.restore_state(state)
    for operation in (
        lambda throttle: throttle.record_response(1.5, ok=True),
        lambda throttle: throttle.record_response(2.0, ok=False),
        lambda throttle: throttle.record_success(),
        lambda throttle: throttle.record_server_error(503),
        lambda throttle: throttle.record_response(0.75, ok=True),
    ):
        operation(uninterrupted)
        operation(resumed)
        assert resumed.delay == uninterrupted.delay
        assert resumed.concurrency == uninterrupted.concurrency
        assert resumed.snapshot_state() == uninterrupted.snapshot_state()


@pytest.mark.parametrize(
    "state",
    [
        {},
        {"delay_seconds": float("inf"), "concurrency": 1, "consecutive_ok": 0},
        {"delay_seconds": 0.5, "concurrency": 0, "consecutive_ok": 0},
        {"delay_seconds": 0.5, "concurrency": 1, "consecutive_ok": 3},
    ],
)
def test_throttle_restore_rejects_invalid_closed_state(state):
    with pytest.raises(ValueError):
        Throttle(min_delay=0.5, max_delay=5.0, max_concurrency=4).restore_state(state)


def test_throttle_restore_clamps_out_of_bounds_delay_to_configured_politeness():
    # A live operator rate change may have persisted a delay outside the
    # configured bounds; a resume returns to the configured politeness
    # instead of refusing the scan (see restore_state).
    throttle = Throttle(min_delay=0.5, max_delay=5.0, max_concurrency=4)
    throttle.restore_state({"delay_seconds": 6.0, "concurrency": 1, "consecutive_ok": 0})
    assert throttle.delay == 5.0
