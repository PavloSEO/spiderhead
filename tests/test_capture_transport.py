"""Bounded, offline transport capture hook tests."""

from __future__ import annotations

import gzip
import zlib

import httpx
import pytest

from seohead.crawl.capture import EntityDecodeError, EntityLimitError, bounded_entity, decode_entity
from seohead.crawl.collect import fetch_one


class Response:
    def __init__(self, content: bytes, status: int = 200) -> None:
        self.content = content
        self.text = content.decode("utf-8", "replace")
        self.status_code = status
        self.headers = {"content-type": "text/html; charset=utf-8", "set-cookie": "secret"}


def test_capture_observer_receives_exact_bytes_and_no_secret_headers():
    events = []
    record, _ = fetch_one(
        "https://example.test/",
        fetcher=lambda _url: Response(b"<html><title>x</title></html>"),
        capture_observer=events.append,
    )
    assert record.status_code == 200
    assert len(events) == 1
    event = events[0]
    assert event.entity_bytes == b"<html><title>x</title></html>"
    assert event.body_fidelity == "entity_bytes"
    assert event.body_state == "complete"
    # The name of a redacted header survives so a reader can tell why a body was
    # kept or dropped (#647); its value never does.
    assert event.response_headers == (
        ("content-type", "text/html; charset=utf-8"),
        ("x-seohead-redacted-headers", "set-cookie"),
    )
    assert not any("secret" in value for _name, value in event.response_headers)
    assert event.request_headers and event.request_headers[0][0] == "user-agent"


@pytest.mark.parametrize(
    ("encoding", "encoded"),
    [("gzip", gzip.compress(b"hello")), ("deflate", zlib.compress(b"hello"))],
)
def test_bounded_entity_decodes_compressed_input(encoding, encoded):
    assert bounded_entity(encoded, encoding, 5) == b"hello"
    with pytest.raises(EntityLimitError):
        bounded_entity(encoded, encoding, 4)


def test_bounded_entity_supports_raw_deflate_and_rejects_unknown_content_coding():
    compressor = zlib.compressobj(wbits=-zlib.MAX_WBITS)
    raw_deflate = compressor.compress(b"raw deflate") + compressor.flush()
    assert bounded_entity(raw_deflate, "deflate", 100) == b"raw deflate"
    with pytest.raises(EntityDecodeError, match="unsupported content encoding"):
        bounded_entity(b"encoded elsewhere", "br", 100)


def test_decoder_matches_scan_decoder_policy_for_utf8_and_legacy_charset():
    assert decode_entity(b"hello", "text/html")[0] == "hello"
    assert decode_entity("Привет".encode("cp1251"), "text/html; charset=cp1251")[0] == "Привет"


def test_http_error_and_timeout_still_emit_one_event():
    events = []
    fetch_one(
        "https://example.test/error",
        fetcher=lambda _url: Response(b"no", 404),
        capture_observer=events.append,
    )
    assert events[-1].status_code == 404 and events[-1].entity_bytes == b"no"
    events.clear()
    fetch_one(
        "https://example.test/timeout",
        fetcher=lambda _url: (_ for _ in ()).throw(TimeoutError("no")),
        capture_observer=events.append,
    )
    assert len(events) == 1 and events[0].body_state == "unavailable"


def test_non_html_response_is_observed_without_a_parser_result():
    events = []
    response = Response(b"%PDF", 200)
    response.headers["content-type"] = "application/pdf"
    record, parsed = fetch_one(
        "https://example.test/file.pdf",
        fetcher=lambda _url: response,
        capture_observer=events.append,
    )
    assert parsed is None and record.content_type == "application/pdf"
    assert events[0].entity_bytes == b"%PDF"


def test_httpx_gzip_observer_receives_decoded_entity_and_parser_matches(monkeypatch):
    import seohead.crawl.collect as collect

    monkeypatch.setattr(collect, "validate_url", lambda _url: None)
    monkeypatch.setattr(collect, "pinned_target", lambda url: (url, {}, {}))
    html = b"<html><head><title>gzip</title></head><body>x</body></html>"
    client = httpx.Client(
        transport=httpx.MockTransport(
            lambda request: httpx.Response(
                200,
                headers={"content-type": "text/html", "content-encoding": "gzip"},
                stream=httpx.ByteStream(gzip.compress(html)),
                request=request,
            )
        )
    )
    events = []
    try:
        record, parsed = fetch_one(
            "https://example.test/", client=client, capture_observer=events.append
        )
    finally:
        client.close()
    assert record.title == "gzip" and parsed is not None
    assert events[0].entity_bytes == html


def test_pinned_transport_keeps_capture_effective_url_at_the_logical_request(monkeypatch):
    import seohead.crawl.collect as collect

    logical = "https://example.test/path"
    target = "https://93.184.216.34/path"
    monkeypatch.setattr(collect, "validate_url", lambda _url: None)
    monkeypatch.setattr(
        collect, "pinned_target", lambda _url: (target, {"Host": "example.test"}, {})
    )

    def transport(request):
        assert str(request.url) == target
        assert request.headers["host"] == "example.test"
        return httpx.Response(
            200,
            headers={"content-type": "text/html"},
            stream=httpx.ByteStream(b"<html><title>logical</title></html>"),
            request=request,
        )

    events = []
    client = httpx.Client(transport=httpx.MockTransport(transport))
    try:
        record, _ = fetch_one(logical, client=client, capture_observer=events.append)
    finally:
        client.close()
    assert record.title == "logical"
    assert events[0].requested_url == logical
    assert events[0].effective_url == logical


def test_source_304_without_a_prior_capture_is_named_not_in_corpus():
    events = []
    record, parsed = fetch_one(
        "https://example.test/not-modified",
        fetcher=lambda _url: Response(b"", 304),
        capture_observer=events.append,
    )
    assert record.status_code == 304 and parsed is None
    assert events[0].entity_bytes is None
    assert (events[0].body_state, events[0].body_reason) == ("unavailable", "not_in_corpus")


def test_legacy_fetch_does_not_import_capture_work(monkeypatch):
    import seohead.crawl.capture as capture

    monkeypatch.setattr(
        capture, "bounded_entity", lambda *_args: (_ for _ in ()).throw(AssertionError())
    )
    record, _ = fetch_one("https://example.test/", fetcher=lambda _url: Response(b"<html></html>"))
    assert record.status_code == 200


def test_injected_decoded_gzip_bytes_are_not_decompressed_again():
    events = []
    response = Response(b"<html><title>decoded</title></html>")
    response.headers["content-encoding"] = "gzip"
    record, _ = fetch_one(
        "https://example.test/", fetcher=lambda _url: response, capture_observer=events.append
    )
    assert record.title == "decoded"
    assert events[0].entity_bytes == response.content
    assert events[0].body_state == "complete"


def test_cp1251_injected_bytes_keep_exact_size_measurement():
    raw = "Привет".encode("cp1251")
    response = Response(raw)
    response.headers["content-type"] = "text/html; charset=cp1251"
    record, _ = fetch_one(
        "https://example.test/", fetcher=lambda _url: response, capture_observer=lambda _event: None
    )
    assert record.size_bytes == len(raw)


def test_timeout_retry_emits_two_attempt_events():
    events = []
    outcomes = iter([TimeoutError("first"), Response(b"<html><title>ok</title></html>")])

    def fetcher(_url):
        item = next(outcomes)
        if isinstance(item, Exception):
            raise item
        return item

    record, _ = fetch_one(
        "https://example.test/", fetcher=fetcher, retry_on_timeout=1, capture_observer=events.append
    )
    assert record.title == "ok"
    assert len(events) == 2
    assert events[0].requested_at and events[1].received_at


def test_outgoing_secrets_are_redacted_but_credentials_state_is_true():
    events = []
    fetch_one(
        "https://example.test/",
        fetcher=lambda _url: Response(b"<html></html>"),
        extra_headers={
            "Authorization": "Bearer secret",
            "Cookie": "sid=secret",
            "Accept": "text/html",
        },
        capture_observer=events.append,
    )
    assert events[0].credentials_used is True
    assert all("secret" not in value for _, value in events[0].request_headers)


def test_streamed_oversize_keeps_200_and_emits_truncated(monkeypatch):
    import seohead.crawl.collect as collect

    monkeypatch.setattr(collect, "validate_url", lambda _url: None)
    monkeypatch.setattr(collect, "pinned_target", lambda url: (url, {}, {}))
    client = httpx.Client(
        transport=httpx.MockTransport(
            lambda request: httpx.Response(
                200,
                headers={"content-type": "text/html", "content-encoding": "gzip"},
                stream=httpx.ByteStream(gzip.compress(b"x" * 64)),
                request=request,
            )
        )
    )
    events = []
    try:
        record, _ = fetch_one(
            "https://example.test/",
            client=client,
            max_response_bytes=8,
            capture_observer=events.append,
        )
    finally:
        client.close()
    assert record.status_code == 200
    assert events[0].status_code == 200
    assert events[0].body_state == "truncated" and events[0].body_reason == "truncated"
