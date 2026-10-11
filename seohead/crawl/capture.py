"""Bounded, redacted transport observations for native body capture."""

from __future__ import annotations

import zlib
from collections.abc import Iterable
from dataclasses import dataclass
from io import BytesIO
import time
from typing import Any

from seohead.core.common import utc_iso_z as now_utc
from seohead.storage.bodies import decode_entity

_ALLOW = frozenset(
    {
        "content-type",
        "content-encoding",
        "location",
        "etag",
        "last-modified",
        "cache-control",
        "vary",
        "x-robots-tag",
        "content-language",
        "content-length",
        "user-agent",
        "accept",
        "accept-language",
        "accept-encoding",
        "host",
        "date",
        "expires",
        "age",
        "pragma",
        "if-none-match",
        "if-modified-since",
        "range",
        "content-range",
        "link",
        # Judged by the security-header checks (seohead.crawl.security_headers). Their
        # values are policy text, not credentials, so they are stored as sent.
        "strict-transport-security",
        "content-security-policy",
        "x-frame-options",
        "x-content-type-options",
        "referrer-policy",
        "permissions-policy",
    }
)


# Headers whose value must never be stored, but whose presence explains a response:
# a request that carried one and a request that carried none are otherwise identical
# once redaction has run.
_SENSITIVE = frozenset(
    {"authorization", "proxy-authorization", "cookie", "set-cookie", "x-api-key", "x-auth-token"}
)

# The name the redacted header list uses to report which of those were present. Not a
# header any site sends, and outside every sensitive-name check the artifact validates
# against, so a reader can never confuse the marker with the header it stands for.
REDACTED_NAMES_HEADER = "x-seohead-redacted-headers"

TRANSPORT_PHASES = ("dns", "connect", "tls", "ttfb", "download")


class TransportTrace:
    """Measure only HTTPX trace events emitted by the request's own transport."""

    def __init__(self) -> None:
        self.started: dict[str, float] = {}
        self.seconds: dict[str, float] = {name: 0.0 for name in TRANSPORT_PHASES}
        self.observed: set[str] = set()
        self.reasons: dict[str, str] = {}
        self.request_started: float | None = None

    def __call__(self, event_name: str, _info: dict[str, Any]) -> None:
        if not isinstance(event_name, str):
            return
        event, separator, state = event_name.rpartition(".")
        if not separator or state not in {"started", "complete", "failed"}:
            return
        now = time.monotonic()
        if event in {"http11.send_request_headers", "http2.send_request_headers"}:
            if state == "started":
                self.request_started = now
            return
        mapping = {
            "connection.connect_tcp": "connect",
            "connection.start_tls": "tls",
            "http11.receive_response_body": "download",
            "http2.receive_response_body": "download",
        }
        phase = mapping.get(event)
        if phase is not None:
            if state == "started":
                self.started[phase] = now
            elif state == "complete" and phase in self.started:
                self.seconds[phase] += max(0.0, now - self.started.pop(phase))
                self.observed.add(phase)
            elif state == "failed" and phase in self.started:
                self.seconds[phase] += max(0.0, now - self.started.pop(phase))
                self.observed.add(phase)
                self.reasons[phase] = "transport phase failed"
            return
        if event in {"http11.receive_response_headers", "http2.receive_response_headers"}:
            if state == "complete" and self.request_started is not None:
                self.seconds["ttfb"] += max(0.0, now - self.request_started)
                self.observed.add("ttfb")
                self.request_started = None
            elif state == "failed":
                self.reasons["ttfb"] = "response headers were not received"

    def snapshot(self) -> dict[str, dict[str, Any]]:
        """Return explicit measurements and reasons; trace internals never enter the scan."""
        result: dict[str, dict[str, Any]] = {}
        for name in TRANSPORT_PHASES:
            if name == "dns":
                result[name] = {
                    "seconds": None,
                    "state": "unavailable",
                    "reason": "HTTPX trace exposes no separate DNS timing event",
                }
            elif name in self.observed:
                result[name] = {
                    "seconds": round(self.seconds[name], 6),
                    "state": "partial" if name in self.reasons else "measured",
                    "reason": self.reasons.get(name)
                    or (
                        "DNS timing is not separate and may be included here"
                        if name == "connect"
                        else ""
                    ),
                }
            else:
                result[name] = {
                    "seconds": None,
                    "state": "unavailable",
                    "reason": "transport trace event was not observed",
                }
        return result


def unavailable_transport_phases(reason: str) -> dict[str, dict[str, Any]]:
    """Name unmeasured phases for injected, cached or unsupported transports."""
    return {
        name: {"seconds": None, "state": "unavailable", "reason": reason}
        for name in TRANSPORT_PHASES
    }


def header_pairs(headers: Any) -> tuple[tuple[str, str], ...]:
    items = (
        headers.multi_items()
        if hasattr(headers, "multi_items")
        else headers.items()
        if hasattr(headers, "items")
        else headers
    )
    return tuple((str(name).lower(), str(value)) for name, value in items)


def redact_headers(headers: Any) -> tuple[tuple[str, str], ...]:
    """Keep the allow-listed headers, and name -- never quote -- the sensitive ones.

    Dropping a credential or a cookie outright left two responses indistinguishable
    in the artifact: the one whose body was omitted as ``credentialed`` and the one
    that was retained recorded byte-identical header lists, so an operator reading
    the scan could not tell why a body was missing (#647). The marker carries names
    only, under a name of our own so it can never be mistaken for a header the site
    sent or read back as one; no value ever reaches the artifact.
    """
    pairs = header_pairs(headers)
    kept = tuple((name, value) for name, value in pairs if name in _ALLOW)
    redacted = sorted({name for name, value in pairs if name in _SENSITIVE and value})
    if redacted:
        kept += ((REDACTED_NAMES_HEADER, ",".join(redacted)),)
    return kept


@dataclass(frozen=True)
class CaptureEvent:
    method: str
    requested_url: str
    effective_url: str
    redirect_history: tuple[dict[str, Any], ...]
    requested_at: str
    received_at: str
    status_code: int | None
    request_headers: tuple[tuple[str, str], ...]
    credentials_used: bool
    response_headers: tuple[tuple[str, str], ...]
    content_type: str
    content_encoding: str
    entity_bytes: bytes | None
    body_fidelity: str
    body_state: str
    body_reason: str
    error: str
    error_kind: str
    effective_status_code: int | None = None
    effective_headers: tuple[tuple[str, str], ...] = ()
    response_time: float | None = None
    session_changed: bool = False
    http_version: str | None = None
    timing_state: str = "unavailable"
    transport_phases: dict[str, dict[str, Any]] | None = None


class EntityLimitError(ValueError):
    """The response entity is truncated or exceeds its declared capture limit."""


class EntityDecodeError(EntityLimitError):
    """The declared HTTP content coding cannot be decoded as an entity."""


def bounded_entity(raw: bytes, content_encoding: str, limit: int) -> bytes:
    """Return a capped content-decoded entity without allocating a compression bomb."""
    if type(raw) is not bytes or type(limit) is not int or limit < 0:
        raise ValueError("entity capture requires bytes and a nonnegative limit")
    return bounded_entity_chunks((raw,), content_encoding, limit)


def bounded_entity_chunks(chunks: Iterable[bytes], content_encoding: str, limit: int) -> bytes:
    """Decode raw HTTP chunks incrementally under a finite decoded-byte cap."""
    encoding = (content_encoding or "").strip().lower()
    if type(limit) is not int or limit < 0:
        raise ValueError("entity capture limit must be nonnegative")
    if encoding in ("", "identity"):
        out = bytearray()
        for chunk in chunks:
            if len(chunk) > limit - len(out):
                raise EntityLimitError("entity exceeds capture limit")
            out.extend(chunk)
        return bytes(out)
    if encoding == "zstd":
        try:
            from compression import zstd

            standard_library = True
        except ImportError as exc:
            try:
                import zstandard as zstd

                standard_library = False
            except ImportError:
                raise EntityDecodeError(
                    "zstd response requires Python 3.14+ or the optional zstandard package"
                ) from exc
        encoded = bytearray()
        for chunk in chunks:
            if len(chunk) > limit + 64 * 1024 - len(encoded):
                raise EntityLimitError("encoded response exceeds capture limit")
            encoded.extend(chunk)
        try:
            if standard_library:
                options = {
                    zstd.DecompressionParameter.window_log_max: max(10, (limit + 1).bit_length())
                }
                reader_context = zstd.open(BytesIO(encoded), options=options)
            else:
                window_kib = max(1, (limit + 1023) // 1024)
                reader_context = zstd.ZstdDecompressor(
                    max_window_size=window_kib
                ).stream_reader(BytesIO(encoded), read_across_frames=True)
            with reader_context as reader:
                out = bytearray()
                while chunk := reader.read(64 * 1024):
                    if len(chunk) > limit - len(out):
                        raise EntityLimitError("compressed entity exceeds capture limit")
                    out.extend(chunk)
            return bytes(out)
        except EntityLimitError:
            raise
        except (zstd.ZstdError, EOFError, ValueError) as exc:
            raise EntityDecodeError("compressed entity is invalid") from exc
    wbits = (
        16 + zlib.MAX_WBITS
        if encoding == "gzip"
        else zlib.MAX_WBITS
        if encoding == "deflate"
        else None
    )
    if wbits is None:
        raise EntityDecodeError("unsupported content encoding")
    try:
        decoder = None
        out = bytearray()
        prefix = bytearray()
        encoded_bytes = 0
        for chunk in chunks:
            encoded_bytes += len(chunk)
            if encoded_bytes > limit + 64 * 1024:
                raise EntityLimitError("encoded response exceeds capture limit")
            if decoder is None:
                prefix.extend(chunk)
                if len(prefix) < 2:
                    continue
                if encoding == "deflate" and not (
                    prefix[0] & 15 == 8 and (prefix[0] * 256 + prefix[1]) % 31 == 0
                ):
                    wbits = -zlib.MAX_WBITS
                decoder = zlib.decompressobj(wbits)
                chunk = bytes(prefix)
                prefix.clear()
            while chunk:
                if decoder.eof:
                    if encoding != "gzip":
                        raise EntityDecodeError("compressed entity contains trailing data")
                    # gzip permits concatenated members. Each member shares
                    # the same aggregate decoded/encoded byte limits.
                    decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
                out.extend(decoder.decompress(chunk, limit + 1 - len(out)))
                if len(out) > limit or decoder.unconsumed_tail:
                    raise EntityLimitError("compressed entity exceeds capture limit")
                chunk = decoder.unused_data
        if decoder is None or not decoder.eof:
            raise EntityLimitError("compressed entity is truncated")
        return bytes(out)
    except zlib.error as exc:
        raise EntityDecodeError("compressed entity is invalid") from exc


__all__ = [
    "REDACTED_NAMES_HEADER",
    "CaptureEvent",
    "bounded_entity",
    "bounded_entity_chunks",
    "decode_entity",
    "now_utc",
    "redact_headers",
]
