"""Bounded standard-library codecs and readers for retained scan bodies."""

from __future__ import annotations

import codecs
import hashlib
import json
import re
import sqlite3
import zlib
from email.message import Message

from seohead.crawl.settings import MAX_RENDER_PAGE_CONCURRENCY

from . import ScanError

_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_DECODER_VERSION = "scan_decoder.v1"
_LINE_BOUNDARIES = re.compile(r"\r\n|[\n\r\v\f\x1c-\x1e\x85\u2028\u2029]")


def _line_count(text: str) -> int:
    """Count splitlines() records without materializing a list of substrings."""
    if not text:
        return 0
    boundaries = sum(1 for _match in _LINE_BOUNDARIES.finditer(text))
    return boundaries + (text[-1] not in "\n\r\v\f\x1c\x1d\x1e\x85\u2028\u2029")


def _navigation_evidence(renderer: dict):
    """Read the compatible encoding or the earlier prerelease top-level form."""
    settings = renderer["settings"]
    if "navigation" in renderer and "navigation_evidence" in settings:
        raise ScanError("rendered navigation evidence has conflicting encodings")
    if "navigation_evidence" in settings:
        return True, settings["navigation_evidence"]
    return "navigation" in renderer, renderer.get("navigation")


def _renderer(document: dict[str, object], con=None) -> dict[str, object]:
    raw = document["renderer_json"]
    if not isinstance(raw, str):
        raise ScanError("document renderer provenance is invalid")
    try:
        value = json.loads(raw)
    except (TypeError, ValueError) as exc:
        raise ScanError("document renderer provenance is invalid") from exc
    representation = document["representation"]
    if document["body_state"] == "complete" and (
        (representation == "rendered") != (document["fidelity"] == "serialized_dom")
    ):
        raise ScanError("document representation and body fidelity disagree")
    if representation == "static":
        if value != {}:
            raise ScanError("static document renderer must be empty")
        return {}
    required = {
        "engine",
        "engine_version",
        "settings",
        "flattened_iframes",
        "capture_limitations",
        "navigation_url_id",
        "final_url_id",
        "navigation_transform",
    }
    if (
        not isinstance(value, dict)
        or not required <= set(value) <= required | {"page_concurrency", "navigation"}
        or type(value["engine"]) is not str
        or type(value["engine_version"]) is not str
        or not isinstance(value["settings"], dict)
        or type(value["flattened_iframes"]) is not bool
        or not isinstance(value["capture_limitations"], list)
        or any(type(item) is not str for item in value["capture_limitations"])
        or value["navigation_transform"] not in {"direct", "legacy_escaped_fragment", "unknown"}
    ):
        raise ScanError("rendered document renderer provenance is invalid")
    has_navigation, navigation = _navigation_evidence(value)
    if has_navigation:
        from seohead.checks.navigation import validate_navigation

        def resolve_url(url_id):
            row = con.execute("SELECT url FROM urls WHERE url_id=?", (url_id,)).fetchone()
            return row[0] if row else None

        try:
            validate_navigation(
                navigation,
                stored=True,
                resolve_url=resolve_url if con is not None else None,
            )
        except ValueError as exc:
            raise ScanError(f"rendered navigation evidence is invalid: {exc}") from exc
    if "page_concurrency" in value and (
        type(value["page_concurrency"]) is not int
        or not 1 <= value["page_concurrency"] <= MAX_RENDER_PAGE_CONCURRENCY
    ):
        raise ScanError("rendered page_concurrency provenance is invalid")
    if (
        representation == "legacy_fragment"
        and value["navigation_transform"] != "legacy_escaped_fragment"
    ):
        raise ScanError("legacy fragment requires a reproducible navigation transform")
    for name in ("navigation_url_id", "final_url_id"):
        if value[name] is not None and (type(value[name]) is not int or value[name] < 1):
            raise ScanError("renderer URL reference is invalid")
        if (
            con is not None
            and value[name] is not None
            and con.execute("SELECT 1 FROM urls WHERE url_id=?", (value[name],)).fetchone() is None
        ):
            raise ScanError("renderer URL reference is absent")
    transform = value["navigation_transform"]
    if document["body_state"] == "complete" and (
        transform == "unknown"
        or value["navigation_url_id"] is None
        or value["final_url_id"] is None
    ):
        raise ScanError("complete document requires known renderer navigation")
    if transform == "direct" and value["navigation_url_id"] != document["url_id"]:
        raise ScanError("direct renderer navigation differs from the logical page")
    if transform == "legacy_escaped_fragment" and con is not None:
        from seohead.checks.render import legacy_fragment_target

        logical = con.execute(
            "SELECT url FROM urls WHERE url_id=?", (document["url_id"],)
        ).fetchone()
        navigation = con.execute(
            "SELECT url FROM urls WHERE url_id=?", (value["navigation_url_id"],)
        ).fetchone()
        # This declared transform selects the existing meta-fragment branch;
        # its URL calculation must reproduce the recorded navigation.
        expected = (
            legacy_fragment_target(logical[0], '<meta name="fragment" content="!">')
            if logical
            else None
        )
        if navigation is None or navigation[0] != expected:
            raise ScanError("legacy fragment navigation does not reproduce the declared transform")
    return value


def encode_body(raw: bytes) -> dict[str, object]:
    """Encode exact content-decoded bytes using identity or smaller zlib-6 data."""
    if type(raw) is not bytes:
        raise ScanError("body bytes must be an exact bytes value")
    compressed = zlib.compress(raw, 6)
    codec, data = ("zlib", compressed) if len(compressed) < len(raw) else ("identity", raw)
    return {
        "sha256": hashlib.sha256(raw).hexdigest(),
        "codec": codec,
        "decoded_bytes": len(raw),
        "stored_bytes": len(data),
        "data": data,
    }


def read_body(con: sqlite3.Connection, sha256: str, *, max_decoded_bytes: int) -> bytes:
    """Read and verify one body without allowing a compressed expansion bomb."""
    if not isinstance(con, sqlite3.Connection):
        raise ScanError("body reader requires a sqlite3 connection")
    if not isinstance(sha256, str) or not _SHA256.fullmatch(sha256):
        raise ScanError("body SHA-256 must be lowercase hex")
    if type(max_decoded_bytes) is not int or max_decoded_bytes < 0:
        raise ScanError("body decoded-byte limit must be a nonnegative integer")
    row = con.execute(
        "SELECT sha256, codec, decoded_bytes, stored_bytes, length(data) "
        "FROM bodies WHERE sha256=?",
        (sha256,),
    ).fetchone()
    if row is None:
        raise ScanError("body unavailable: not_in_corpus")
    values = tuple(row)
    if len(values) != 5:
        raise ScanError("body row is malformed")
    stored_sha, codec, decoded_bytes, stored_bytes, actual_stored_bytes = values
    if (
        stored_sha != sha256
        or type(codec) is not str
        or type(decoded_bytes) is not int
        or type(stored_bytes) is not int
        or type(actual_stored_bytes) is not int
        or decoded_bytes < 0
        or stored_bytes < 0
        or stored_bytes != actual_stored_bytes
    ):
        raise ScanError("body metadata or stored length is invalid")
    if decoded_bytes > max_decoded_bytes:
        raise ScanError("body unavailable: decoded byte limit exceeded")
    if stored_bytes > decoded_bytes:
        raise ScanError("body stored size exceeds its decoded-byte declaration")
    data_row = con.execute("SELECT data FROM bodies WHERE sha256=?", (sha256,)).fetchone()
    if data_row is None or type(data_row[0]) is not bytes:
        raise ScanError("body data is unavailable or malformed")
    data = data_row[0]
    if codec == "identity":
        raw = data
        if len(raw) != decoded_bytes:
            raise ScanError("identity body decoded length disagrees")
    elif codec == "zlib":
        decoder = zlib.decompressobj()
        try:
            raw = decoder.decompress(data, max_decoded_bytes + 1)
        except zlib.error as exc:
            raise ScanError("compressed body has an invalid zlib stream") from exc
        if len(raw) > max_decoded_bytes or decoder.unconsumed_tail:
            raise ScanError("compressed body exceeds decoded byte limit")
        if not decoder.eof or decoder.unused_data or len(raw) != decoded_bytes:
            raise ScanError("compressed body is truncated, trailing, or length-mismatched")
    else:
        raise ScanError("body codec is unsupported")
    if hashlib.sha256(raw).hexdigest() != sha256:
        raise ScanError("body SHA-256 disagrees with decoded bytes")
    return raw


def decode_entity(raw: bytes, content_type: str) -> tuple[str, dict[str, str]]:
    """Decode entity bytes under the explicit ``scan_decoder.v1`` policy."""
    if type(raw) is not bytes or not isinstance(content_type, str):
        raise ScanError("entity decoder requires bytes and a content-type string")
    message = Message()
    message["content-type"] = content_type
    requested = message.get_content_charset()
    try:
        charset = codecs.lookup(requested).name if requested else "utf-8"
        source = "content_type_charset" if requested else "utf8_fallback"
        # Binary transformations such as base64 are codecs, but not charsets.
        b"x".decode(charset, errors="replace")
    except LookupError:
        charset = "utf-8"
        source = "utf8_fallback"
    return raw.decode(charset, errors="replace"), {
        "decoder_version": _DECODER_VERSION,
        "decoder_source": source,
        "decoder_charset": charset,
        "decoder_errors": "replace",
    }


def read_document(con: sqlite3.Connection, document_id: int, *, max_decoded_bytes: int) -> str:
    """Read one stored document only when its state and response lineage are complete."""
    text, _decoder, _raw = _read_document_parts(
        con, document_id, max_decoded_bytes=max_decoded_bytes
    )
    return text


def read_document_body(
    con: sqlite3.Connection, document_id: int, *, max_decoded_bytes: int
) -> bytes:
    """Read exact response bytes through the selected document's verified lineage."""
    _text, _decoder, raw = _read_document_parts(
        con, document_id, max_decoded_bytes=max_decoded_bytes
    )
    return raw


def read_page_document(con: sqlite3.Connection, url: str, *, max_decoded_bytes: int) -> str:
    """Read the page's selected document by exact URL, preserving response lineage.

    URL normalization is deliberately forbidden here: slash and query variants
    can select different captured bodies. ``read_document`` verifies the
    document-to-response-to-body hash join and its declared retention state.
    """
    row = con.execute(
        "SELECT p.document_id FROM pages p JOIN urls u USING(url_id) WHERE u.url=?", (url,)
    ).fetchone()
    if row is None or row[0] is None:
        raise ScanError("document unavailable: not_in_corpus")
    return read_document(con, row[0], max_decoded_bytes=max_decoded_bytes)


MAX_WINDOW_CHARS = 64 * 1024
MAX_DOCUMENT_BYTES = 64 * 1024 * 1024
_DOCUMENT_CANDIDATE = (
    "(p.document_id=d.document_id OR i.item_key IS NOT NULL OR ("
    "NOT EXISTS(SELECT 1 FROM pages selected_page JOIN documents selected_doc "
    "ON selected_doc.document_id=selected_page.document_id "
    "WHERE selected_doc.url_id=d.url_id AND selected_doc.representation=d.representation) "
    "AND NOT EXISTS(SELECT 1 FROM context_items active JOIN documents selected_doc "
    "ON active.item_key='document:'||selected_doc.document_id "
    "WHERE active.kind='resource_inventory' AND selected_doc.url_id=d.url_id "
    "AND selected_doc.representation=d.representation)))"
)


def read_document_window(
    con: sqlite3.Connection,
    document_id: int,
    *,
    offset: int = 0,
    length: int = 4096,
    max_decoded_bytes: int,
) -> dict[str, object]:
    """Return a bounded character window of one verified document.

    Offsets and lengths count decoded characters, so a multibyte sequence is never split.
    ``sha256`` hashes the whole decoded text as UTF-8, not the stored bytes.
    """
    if type(offset) is not int or offset < 0:
        raise ScanError("window offset must be a nonnegative integer")
    if type(length) is not int or not 1 <= length <= MAX_WINDOW_CHARS:
        raise ScanError(f"window length must be an integer from 1 to {MAX_WINDOW_CHARS}")
    text, decoder, _raw = _read_document_parts(
        con, document_id, max_decoded_bytes=max_decoded_bytes
    )
    window = text[offset : offset + length]
    return {
        "document_id": document_id,
        "offset": offset,
        "length": len(window),
        "total_chars": len(text),
        "truncated": offset + len(window) < len(text),
        "lines": _line_count(text),
        "encoding": decoder["decoder_charset"],
        "sha256": hashlib.sha256(text.encode("utf-8")).hexdigest(),
        "raw_body_sha256": con.execute(
            "SELECT body_sha256 FROM documents WHERE document_id=?", (document_id,)
        ).fetchone()[0],
        "text": window,
    }


def read_document_content(
    con: sqlite3.Connection,
    document_id: int,
    *,
    max_decoded_bytes: int,
) -> dict[str, object]:
    """Read one complete retained document under an explicit decoded-byte cap."""
    if type(max_decoded_bytes) is not int or not 1 <= max_decoded_bytes <= MAX_DOCUMENT_BYTES:
        raise ScanError(f"document byte limit must be 1..{MAX_DOCUMENT_BYTES}")
    text, decoder, _raw = _read_document_parts(
        con, document_id, max_decoded_bytes=max_decoded_bytes
    )
    row = con.execute(
        "SELECT d.representation,d.body_sha256,r.content_type FROM documents d "
        "LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE d.document_id=?",
        (document_id,),
    ).fetchone()
    if row is None or row[1] is None:
        raise ScanError("document body disappeared during read")
    scan = con.execute("SELECT scan_uuid,evidence_revision FROM scan WHERE singleton=1").fetchone()
    return {
        "scan_uuid": scan[0] if scan is not None else None,
        "evidence_revision": scan[1] if scan is not None else None,
        "document_id": document_id,
        "representation": row[0],
        "content_type": row[2],
        "raw_body_sha256": row[1],
        "sha256": hashlib.sha256(text.encode("utf-8")).hexdigest(),
        "encoding": decoder["decoder_charset"],
        "decoder": decoder,
        "total_chars": len(text),
        "lines": _line_count(text),
        "text": text,
    }


def read_page_document_content(
    con: sqlite3.Connection,
    url: str,
    *,
    representation: str = "static",
    variant_key: str | None = None,
    max_decoded_bytes: int,
) -> dict[str, object]:
    """Resolve one exact retained page document and return its content with identity."""
    if type(url) is not str or not url or len(url) > 8192:
        raise ScanError("document URL must be exact nonempty text of at most 8192 characters")
    if representation not in {"static", "rendered", "legacy_fragment"}:
        raise ScanError("representation must be static, rendered, or legacy_fragment")
    if variant_key is not None and type(variant_key) is not str:
        raise ScanError("variant_key must be a string or null")
    variants = [
        row[0]
        for row in con.execute(
            "SELECT DISTINCT r.variant_key FROM documents d JOIN urls u USING(url_id) "
            "LEFT JOIN responses r ON r.response_id=d.source_response_id "
            "LEFT JOIN pages p ON p.url_id=d.url_id "
            "LEFT JOIN context_items i ON i.kind='resource_inventory' "
            "AND i.item_key='document:'||d.document_id "
            "WHERE u.url=? AND d.representation=? AND " + _DOCUMENT_CANDIDATE + " "
            "ORDER BY r.variant_key LIMIT 2",
            (url, representation),
        )
    ]
    if not variants:
        return {"state": "unavailable", "reason": "document_not_in_corpus", "url": url}
    if variant_key is None and len(variants) > 1:
        return {
            "state": "unavailable",
            "reason": "multiple_http_variants_require_variant_key",
            "url": url,
            "representation": representation,
        }
    selected_variant = variant_key if variant_key is not None else variants[0]
    row = con.execute(
        "SELECT d.document_id,d.body_state,d.body_reason FROM documents d "
        "JOIN urls u USING(url_id) LEFT JOIN responses r ON r.response_id=d.source_response_id "
        "LEFT JOIN pages p ON p.url_id=d.url_id "
        "LEFT JOIN context_items i ON i.kind='resource_inventory' "
        "AND i.item_key='document:'||d.document_id "
        "WHERE u.url=? AND d.representation=? AND " + _DOCUMENT_CANDIDATE + " "
        "AND COALESCE(r.variant_key,'')=COALESCE(?,'') "
        "ORDER BY (p.document_id=d.document_id) DESC,(i.item_key IS NOT NULL) DESC,"
        "d.document_id DESC LIMIT 1",
        (url, representation, selected_variant),
    ).fetchone()
    if row is None:
        return {
            "state": "unavailable",
            "reason": "requested_http_variant_not_in_corpus",
            "url": url,
            "representation": representation,
            "variant_key": variant_key,
        }
    if row[1] != "complete":
        return {
            "state": "unavailable",
            "reason": f"{row[1]}/{row[2]}",
            "url": url,
            "document_id": row[0],
            "representation": representation,
            "variant_key": selected_variant,
        }
    return {
        "state": "available",
        "url": url,
        "variant_key": selected_variant,
        **read_document_content(con, row[0], max_decoded_bytes=max_decoded_bytes),
    }


def _read_document_parts(
    con: sqlite3.Connection, document_id: int, *, max_decoded_bytes: int
) -> tuple[str, dict[str, str], bytes]:
    """Read one document and verify both its exact body bytes and decoder metadata."""
    if type(document_id) is not int or document_id < 1:
        raise ScanError("document ID must be a positive integer")
    cursor = con.execute(
        "SELECT d.*, r.request_url_id AS response_request_url_id, "
        "r.body_sha256 AS response_body_sha256, r.content_type AS response_content_type "
        ", r.body_state AS response_body_state, r.body_fidelity AS response_body_fidelity "
        "FROM documents d LEFT JOIN responses r ON r.response_id=d.source_response_id "
        "WHERE d.document_id=?",
        (document_id,),
    )
    row = cursor.fetchone()
    if row is None:
        raise ScanError("document unavailable: not_in_corpus")
    document = {
        column[0]: value for column, value in zip(cursor.description or (), row, strict=True)
    }
    state = document["body_state"]
    renderer = _renderer(document, con)
    body_sha = document["body_sha256"]
    if state != "complete" or body_sha is None:
        raise ScanError(f"document unavailable: {state}/{document['body_reason']}")
    fidelity = document["fidelity"]
    raw = read_body(con, body_sha, max_decoded_bytes=max_decoded_bytes)
    if fidelity in {"entity_bytes", "reencoded_text"}:
        expected_request_url = document["url_id"]
        if document["representation"] == "legacy_fragment":
            expected_request_url = renderer["navigation_url_id"]
            if type(expected_request_url) is not int:
                raise ScanError("legacy fragment navigation URL provenance is invalid")
        if (
            document["source_response_id"] is None
            or document["response_request_url_id"] != expected_request_url
            or document["response_body_sha256"] != body_sha
            or document["response_body_state"] != "complete"
            or document["response_body_fidelity"] != fidelity
        ):
            raise ScanError(
                "document response state, fidelity, URL, or body hash lineage disagrees"
            )
        if fidelity == "entity_bytes":
            text, decoder = decode_entity(raw, document["response_content_type"])
        else:
            text = raw.decode("utf-8", errors="replace")
            decoder = {
                "decoder_version": _DECODER_VERSION,
                "decoder_source": "legacy_unknown",
                "decoder_charset": "unknown",
                "decoder_errors": "unknown",
            }
    elif fidelity == "serialized_dom":
        if document["representation"] != "rendered":
            raise ScanError("serialized DOM requires rendered representation")
        text = raw.decode("utf-8", errors="strict")
        decoder = {
            "decoder_version": _DECODER_VERSION,
            "decoder_source": "renderer_utf8",
            "decoder_charset": "utf-8",
            "decoder_errors": "not_applicable",
        }
    else:
        raise ScanError("document fidelity is unavailable or unsupported")
    if any(document[key] != value for key, value in decoder.items()):
        raise ScanError("document decoder metadata disagrees with stored bytes")
    return text, decoder, raw


def _read_document(
    con: sqlite3.Connection, document_id: int, *, max_decoded_bytes: int
) -> tuple[str, dict[str, str]]:
    """Keep the original private reader shape for storage extensions during integration."""
    text, decoder, _raw = _read_document_parts(
        con, document_id, max_decoded_bytes=max_decoded_bytes
    )
    return text, decoder


def read_document_navigation(con, document_id: int) -> dict[str, object]:
    """Read bounded navigation evidence without fetching or interpreting its cause."""
    from seohead.checks.navigation import expand_navigation, retain_navigation

    if type(document_id) is not int or document_id < 1:
        raise ValueError("document_id must be a positive integer")
    row = con.execute("SELECT * FROM documents WHERE document_id=?", (document_id,)).fetchone()
    if row is None:
        raise ScanError("navigation document is absent")
    document = dict(row)
    renderer = _renderer(document, con)
    has_navigation, navigation = _navigation_evidence(renderer) if renderer else (False, None)
    if not has_navigation:
        navigation = retain_navigation({}, lambda url: None)

    def resolve_url(url_id):
        row = con.execute("SELECT url FROM urls WHERE url_id=?", (url_id,)).fetchone()
        return row[0] if row else None

    public_renderer = {
        key: renderer[key]
        for key in ("engine", "engine_version", "settings", "navigation_transform")
        if key in renderer
    }
    if "settings" in public_renderer:
        public_renderer["settings"] = {
            key: value
            for key, value in public_renderer["settings"].items()
            if key != "navigation_evidence"
        }
    return {
        "document_id": document_id,
        "representation": document["representation"],
        "renderer": public_renderer,
        "requested_url": resolve_url(renderer["navigation_url_id"])
        if renderer.get("navigation_url_id")
        else None,
        "final_url": resolve_url(renderer["final_url_id"])
        if renderer.get("final_url_id")
        else None,
        "navigation": expand_navigation(navigation, resolve_url),
    }
