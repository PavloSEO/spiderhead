"""HTTPX trace phase measurements retain explicit unavailable reasons."""

from __future__ import annotations

from seohead.crawl.capture import TransportTrace


def test_transport_trace_measures_only_observed_phases(monkeypatch):
    ticks = iter([1.0, 1.01, 1.02, 1.05, 1.06, 1.08, 1.10, 1.15, 1.20])
    monkeypatch.setattr("seohead.crawl.capture.time.monotonic", lambda: next(ticks))
    trace = TransportTrace()
    trace("connection.connect_tcp.started", {"host": "example.test"})
    trace("connection.connect_tcp.complete", {})
    trace("connection.start_tls.started", {})
    trace("connection.start_tls.complete", {})
    trace("http11.send_request_headers.started", {})
    trace("http11.send_request_headers.complete", {})
    trace("http11.receive_response_headers.complete", {})
    trace("http11.receive_response_body.started", {})
    trace("http11.receive_response_body.complete", {})

    phases = trace.snapshot()

    assert phases["dns"]["state"] == "unavailable"
    assert "separate DNS" in phases["dns"]["reason"]
    assert phases["connect"]["seconds"] == 0.01
    assert phases["tls"]["seconds"] == 0.03
    assert phases["ttfb"]["seconds"] == 0.04
    assert phases["download"]["seconds"] == 0.05
