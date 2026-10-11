"""Generate protocol-limited XML sitemaps from one validated, read-only scan.

No URL is fetched. Page eligibility comes from retained response/directive/canonical
facts; optional image locations come from complete retained HTML. Missing lastmod
is omitted, and missing image bodies remain explicit coverage gaps.
"""

from __future__ import annotations

import fnmatch
import json
import os
import re
import sqlite3
import xml.etree.ElementTree as ET
from collections import Counter
from contextlib import suppress
from datetime import date, datetime, timezone
from email.utils import parsedate_to_datetime
from pathlib import Path
from typing import Any
from urllib.parse import quote, urljoin, urlsplit, urlunsplit

from seohead.checks.parser import robots_directives
from seohead.core.filesystem import fsync_directory
from seohead.storage import ScanError, open_scan
from seohead.storage.bodies import read_document
from seohead.storage.exports import _unlink_owned, _write_file

FORMAT = "seohead.scan-sitemaps.v1"
NAMESPACE = "http://www.sitemaps.org/schemas/sitemap/0.9"
IMAGE_NAMESPACE = "http://www.google.com/schemas/sitemap-image/1.1"
MAX_URLS = 50_000
MAX_BYTES = 50 * 1024 * 1024
MAX_IMAGES = 1000
MAX_DOCUMENT_BYTES = 5 * 1024 * 1024
_DATE = re.compile(r"\d{4}-\d{2}-\d{2}\Z")
_DATETIME = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:\d{2})\Z")
_MODIFIED_SIGNALS = {
    "meta_article_modified",
    "meta_og_updated",
    "meta_date:dcterms.modified",
    "meta_date:datemodified",
    "meta_date:last-modified",
    "microdata_date:datemodified",
    "jsonld_date:datemodified",
}


def _loc(value: str) -> str:
    if not isinstance(value, str) or any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise ValueError("invalid sitemap URL")
    parts = urlsplit(value)
    if parts.scheme.lower() not in {"http", "https"} or not parts.hostname:
        raise ValueError("sitemap URLs must be absolute HTTP(S) URLs")
    if parts.username is not None or parts.password is not None or parts.fragment:
        raise ValueError("sitemap URLs cannot contain credentials or fragments")
    host = parts.hostname.encode("idna").decode("ascii").lower()
    if any(c.isspace() or c in "\\/?#" for c in host):
        raise ValueError("invalid sitemap URL host")
    if ":" in host:
        host = f"[{host}]"
    port = parts.port
    if port is not None and port != {"http": 80, "https": 443}[parts.scheme.lower()]:
        host += f":{port}"
    value = urlunsplit(
        (
            parts.scheme.lower(),
            host,
            quote(parts.path or "/", safe="/%:@!$&'()*+,;=-._~"),
            quote(parts.query, safe="/%?:@!$&'()*+,;=-._~"),
            "",
        )
    )
    if len(value) >= 2048:
        raise ValueError("sitemap URL must contain fewer than 2048 characters")
    return value


def _origin(value: str) -> tuple[str, str]:
    parts = urlsplit(value)
    return parts.scheme, parts.netloc


def _date(value: str, *, header: bool = False) -> str | None:
    try:
        if _DATE.fullmatch(value):
            return date.fromisoformat(value).isoformat()
        if _DATETIME.fullmatch(value):
            parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        elif header:
            parsed = parsedate_to_datetime(value)
        else:
            return None
        if parsed.tzinfo is None:
            return None
        return parsed.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")
    except (ValueError, TypeError, OverflowError):
        return None


def _lastmod(page: dict[str, Any]) -> tuple[str | None, str]:
    for name, value in json.loads(page["headers"] or "[]"):
        if name.lower() == "last-modified":
            valid = _date(value.strip(), header=True)
            if valid:
                return valid, "last-modified"
    raw_signals = page.get("trust_signals_json")
    signals = json.loads(raw_signals) if raw_signals else None
    signals = signals or {}
    for item in signals.get("dates", []):
        if item["confidence"] == "high" and item["signal"].lower() in _MODIFIED_SIGNALS:
            valid = _date(item["value"].strip())
            if valid:
                return valid, item["signal"]
    return None, "unavailable"


def _exclusion(page: dict[str, Any], loc: str, origin: tuple[str, str], exclude: list[str]) -> str:
    if page["blocked"]:
        return "robots_blocked"
    if page["status_code"] != 200:
        return "not_200"
    if page["error"] or page["redirect_url"]:
        return "response_error_or_redirect"
    if "noindex" in robots_directives(page["meta_robots"], page["x_robots"]):
        return "noindex"
    for name in ("canonical", "final_canonical", "final_url"):
        value = page.get(name)
        if value:
            try:
                if _loc(urljoin(loc, value)) != loc:
                    return "non_canonical" if name != "final_url" else "redirected"
            except ValueError:
                return "invalid_canonical" if name != "final_url" else "invalid_final_url"
    if _origin(loc) != origin:
        return "outside_origin"
    if any(fnmatch.fnmatchcase(loc, pattern) for pattern in exclude):
        return "user_excluded"
    return ""


def _entry(
    loc: str, lastmod: str | None = None, images: list[str] | None = None, *, tag: str = "url"
) -> bytes:
    root = ET.Element(tag)
    ET.SubElement(root, "loc").text = loc
    if lastmod:
        ET.SubElement(root, "lastmod").text = lastmod
    for image in images or []:
        node = ET.SubElement(root, "image:image")
        ET.SubElement(node, "image:loc").text = image
    return ET.tostring(root, encoding="utf-8") + b"\n"


class _Writer:
    """Stream chunks, retaining only one URL entry and bounded file metadata."""

    def __init__(self, destination: Path, prefix: str, owned: dict, *, images: bool = False):
        self.destination, self.prefix, self.owned = destination, prefix, owned
        self.header = (
            f'<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="{NAMESPACE}"'
            + (f' xmlns:image="{IMAGE_NAMESPACE}"' if images else "")
            + ">\n"
        ).encode()
        self.footer = b"</urlset>\n"
        self.files: list[dict[str, Any]] = []
        self.stream = None
        self.count = self.size = 0

    def add(self, entry: bytes) -> None:
        if len(self.header) + len(entry) + len(self.footer) > MAX_BYTES:
            raise ScanError("one sitemap entry exceeds the uncompressed byte limit")
        if self.stream and (
            self.count >= MAX_URLS or self.size + len(entry) + len(self.footer) > MAX_BYTES
        ):
            self.finish()
        if self.stream is None:
            if len(self.files) >= MAX_URLS:
                raise ScanError("sitemap index entry limit exceeded")
            self.path = self.destination / f"{self.prefix}-{len(self.files) + 1:04d}.xml"
            fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            state = os.fstat(fd)
            self.owned[self.path] = (state.st_dev, state.st_ino)
            self.stream = os.fdopen(fd, "wb")
            self.stream.write(self.header)
            self.size, self.count = len(self.header), 0
        self.stream.write(entry)
        self.size += len(entry)
        self.count += 1

    def finish(self) -> None:
        if self.stream is None:
            return
        self.stream.write(self.footer)
        self.stream.flush()
        os.fsync(self.stream.fileno())
        self.stream.close()
        self.stream = None
        self.files.append(
            {"name": self.path.name, "urls": self.count, "bytes": self.size + len(self.footer)}
        )

    def close(self) -> None:
        if self.stream:
            self.stream.close()
            self.stream = None


def _images(
    con: sqlite3.Connection,
    page: dict[str, Any],
    loc: str,
    coverage: Counter[str],
    exclude: list[str],
) -> list[str]:
    from bs4 import BeautifulSoup

    from seohead.checks.parser import document_base_url, extract_url_sources, image_url_sources

    if not page["document_id"] or "html" not in page["content_type"].lower():
        coverage["body_unavailable"] += 1
        return []
    try:
        html = read_document(con, page["document_id"], max_decoded_bytes=MAX_DOCUMENT_BYTES)
    except ScanError as exc:
        coverage[f"body_unavailable: {exc}"] += 1
        return []
    soup = BeautifulSoup(html, "lxml")
    # A source element in audio/video is not image evidence.
    for source in soup.find_all("source"):
        if source.find_parent("picture") is None:
            source.decompose()
    sources = image_url_sources(extract_url_sources(soup, document_base_url(soup, loc)))
    coverage["bodies_read"] += 1
    images: list[str] = []
    seen: set[str] = set()
    has_resource_status = (
        con.execute(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_graph_fetches'"
        ).fetchone()
        is not None
    )
    for source in sources:
        if source["tag"] not in {"img", "source"}:
            continue  # CSS URLs can name fonts and other non-image resources.
        try:
            image = _loc(source["url"])
        except ValueError:
            coverage["invalid_image_url"] += 1
            continue
        if image in seen:
            continue
        seen.add(image)
        if any(fnmatch.fnmatchcase(image, pattern) for pattern in exclude):
            coverage["user_excluded_image"] += 1
            continue
        row = con.execute(
            "SELECT p.status_code,p.meta_robots,p.x_robots,"
            "EXISTS(SELECT 1 FROM context_items c WHERE c.kind='robots_blocked_url' "
            "AND c.item_key='url:'||u.url_id) AS blocked "
            "FROM urls u LEFT JOIN pages p USING(url_id) WHERE u.url=?",
            (image,),
        ).fetchone()
        if row and (
            row["blocked"]
            or (row["status_code"] is not None and row["status_code"] != 200)
            or "noindex" in robots_directives(row["meta_robots"] or "", row["x_robots"] or "")
        ):
            coverage["excluded_image"] += 1
            continue
        resource = (
            con.execute(
                "SELECT status_code FROM resource_graph_fetches WHERE resolved_url=?", (image,)
            ).fetchone()
            if has_resource_status
            else None
        )
        resource_status = resource[0] if resource else None
        if resource_status is not None and resource_status != 200:
            coverage["excluded_image"] += 1
            coverage["resource_non_200_image"] += 1
            continue
        if len(images) >= MAX_IMAGES:
            coverage["image_limit_omitted"] += 1
            continue
        if (not row or row["status_code"] is None) and resource_status is None:
            coverage["image_response_unavailable"] += 1
        images.append(image)
    if not images:
        coverage["pages_without_images"] += 1
    return images


def generate_sitemaps(
    scan: str,
    out_dir: str,
    sitemap_base_url: str,
    *,
    include_images: bool = False,
    exclude: list[str] | None = None,
) -> dict[str, Any]:
    """Write a fresh directory of sitemap parts and an absolute-URL sitemap index.

    ``sitemap_base_url`` is the public directory where the generated files will
    be deployed. It must share the scan start URL's origin; deploy at the origin
    root to cover the full site. ``exclude`` holds absolute-URL shell globs.
    Existing output directories are never overwritten. Limits are uncompressed
    UTF-8 bytes including declarations, namespaces and closing tags.
    """
    con = None
    destination = Path(out_dir).absolute()
    owned: dict[Path, tuple[int, int]] = {}
    writers: list[_Writer] = []
    created = published = False
    try:
        if type(include_images) is not bool:
            raise ValueError("include_images must be a boolean")
        if exclude is None:
            exclude = []
        if (
            not isinstance(exclude, list)
            or len(exclude) > 100
            or any(not isinstance(p, str) or not p or len(p) > 2048 for p in exclude)
        ):
            raise ValueError("exclude must be a list of at most 100 nonempty URL globs")
        base = _loc(sitemap_base_url)
        if urlsplit(base).query or not urlsplit(base).path.endswith("/"):
            raise ValueError("sitemap_base_url must be an HTTP(S) directory URL ending in /")
        con = open_scan(scan, require_audit=False)
        header = dict(con.execute("SELECT * FROM scan").fetchone())
        origin = _origin(_loc(header["start_url"] or base))
        if _origin(base) != origin:
            raise ValueError("sitemap_base_url must share the scan start URL origin")
        # The main artifact remains mode=ro. Disk-backed TEMP storage deduplicates
        # normalized locations without retaining the URL population in Python.
        con.execute("PRAGMA query_only=OFF")
        con.execute("CREATE TEMP TABLE sitemap_emitted(loc TEXT PRIMARY KEY) WITHOUT ROWID")
        destination.mkdir(mode=0o700)
        created = True
        pages = _Writer(destination, "sitemap", owned)
        image_pages = _Writer(destination, "image-sitemap", owned, images=True)
        writers = [pages, image_pages]
        excluded: Counter[str] = Counter()
        lastmods: Counter[str] = Counter()
        image_coverage: Counter[str] = Counter()
        total = emitted = image_urls = image_page_count = 0
        # Joining one document-selected response avoids a second response population
        # or an in-memory page/URL list. Older scans fall back to their last page response.
        query = (
            "SELECT p.*,u.url,r.effective_headers_redacted_json AS headers,"
            "EXISTS(SELECT 1 FROM context_items c WHERE c.kind='robots_blocked_url' "
            "AND c.item_key='url:'||p.url_id) AS blocked FROM pages p JOIN urls u USING(url_id) "
            "LEFT JOIN documents d ON d.document_id=p.document_id LEFT JOIN responses r "
            "ON r.response_id=COALESCE(d.source_response_id,(SELECT rr.response_id FROM responses rr "
            "WHERE rr.request_url_id=p.url_id AND rr.purpose='page' AND rr.method='GET' "
            "ORDER BY rr.request_ordinal DESC LIMIT 1)) ORDER BY p.page_ordinal"
        )
        for row in con.execute(query):
            total += 1
            page = dict(row)
            try:
                loc = _loc(page["url"])
            except ValueError:
                excluded["invalid_url"] += 1
                continue
            reason = _exclusion(page, loc, origin, exclude)
            if reason:
                excluded[reason] += 1
                continue
            # Sitemap files have directory scope in the protocol.
            if not urlsplit(loc).path.startswith(urlsplit(base).path):
                excluded["outside_sitemap_directory"] += 1
                continue
            if not con.execute(
                "INSERT OR IGNORE INTO temp.sitemap_emitted VALUES(?)", (loc,)
            ).rowcount:
                excluded["duplicate_url"] += 1
                continue
            lastmod, source = _lastmod(page)
            lastmods[source] += 1
            pages.add(_entry(loc, lastmod))
            emitted += 1
            if include_images:
                images = _images(con, page, loc, image_coverage, exclude)
                if images:
                    image_pages.add(_entry(loc, lastmod, images))
                    image_urls += len(images)
                    image_page_count += 1
        for writer in writers:
            writer.finish()
        files = pages.files + image_pages.files
        if not files:
            raise ScanError("scan contains no eligible sitemap URLs")
        index_header = (
            f'<?xml version="1.0" encoding="UTF-8"?>\n<sitemapindex xmlns="{NAMESPACE}">\n'
        ).encode()
        index_footer = b"</sitemapindex>\n"
        if len(files) > MAX_URLS:
            raise ScanError("sitemap index exceeds protocol limits")

        def index_chunks():
            size = len(index_header) + len(index_footer)
            yield index_header
            for item in files:
                entry = _entry(_loc(urljoin(base, item["name"])), tag="sitemap")
                size += len(entry)
                if size > MAX_BYTES:
                    raise ScanError("sitemap index exceeds protocol limits")
                yield entry
            yield index_footer

        _write_file(destination / "sitemap-index.xml", index_chunks(), owned)
        fsync_directory(destination)
        published = True
        gaps = sum(
            value for key, value in image_coverage.items() if key.startswith("body_unavailable")
        )
        partial = bool(header["crawl_partial"]) or header["lifecycle"] != "finished"
        return {
            "ok": True,
            "format": FORMAT,
            "path": str(destination),
            "index": str(destination / "sitemap-index.xml"),
            "sitemap_base_url": base,
            "files": files,
            "counts": {
                "pages_read": total,
                "urls": emitted,
                "excluded": sum(excluded.values()),
                "image_pages": image_page_count,
                "images": image_urls,
            },
            "exclusions": dict(sorted(excluded.items())),
            "provenance": {
                key: header[key]
                for key in ("scan_uuid", "evidence_revision", "writer_revision", "lifecycle")
            },
            "coverage": {
                "state": "partial" if partial else "complete",
                "crawl_partial": partial,
                "lastmod_sources": dict(sorted(lastmods.items())),
                "images": {
                    "state": (
                        "not_requested"
                        if not include_images
                        else "unavailable"
                        if emitted and gaps == emitted
                        else "partial"
                        if gaps
                        or image_coverage["image_limit_omitted"]
                        or image_coverage["image_response_unavailable"]
                        else "complete"
                    ),
                    "evidence": "retained HTML references; images without an observed HTTP status remain unverified",
                    "counts": dict(sorted(image_coverage.items())),
                },
            },
            "limits": {
                "urls_per_file": MAX_URLS,
                "bytes_per_file": MAX_BYTES,
                "images_per_page": MAX_IMAGES,
                "document_bytes": MAX_DOCUMENT_BYTES,
            },
        }
    except (OSError, ScanError, ValueError, TypeError, sqlite3.Error) as exc:
        return {"ok": False, "format": FORMAT, "error": str(exc)}
    finally:
        for writer in writers:
            writer.close()
        if con is not None:
            con.close()
        if created and not published:
            for path in reversed(owned):
                _unlink_owned(path, owned)
            with suppress(OSError):
                destination.rmdir()
