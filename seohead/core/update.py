"""Consent-gated checks and verified downloads of published core distributions."""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import tempfile
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import quote, urlsplit

from seohead import __version__

UPDATE_FORMAT = "seohead.update-check.v1"
REPOSITORY = "PavloSEO/spiderhead"
RELEASES_URL = f"https://api.github.com/repos/{REPOSITORY}/releases"
MAX_RELEASES = 10
MAX_ASSETS = 20
MAX_CHANGELOG_CHARS = 4_000
MAX_FEED_BYTES = 2 * 1024 * 1024
MAX_STATE_BYTES = 512 * 1024
MAX_CHECKSUM_BYTES = 128 * 1024
DEFAULT_MAX_DOWNLOAD_BYTES = 250 * 1024 * 1024
MAX_DOWNLOAD_BYTES = 1024 * 1024 * 1024
AUTO_CHECK_SECONDS = 24 * 60 * 60
_VERSION = re.compile(r"v?(\d+)\.(\d+)\.(\d+)(?:[-+][0-9A-Za-z.-]+)?\Z")
_SHA256_LINE = re.compile(r"([0-9a-f]{64})\s+\*?([^\r\n]+)\Z")


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _version(value: str) -> tuple[int, int, int] | None:
    match = _VERSION.fullmatch(value)
    return tuple(map(int, match.groups())) if match else None


def _state_path() -> Path:
    override = os.environ.get("SEOHEAD_UPDATE_STATE_PATH")
    if override:
        return Path(override).expanduser()
    if os.name == "nt":
        base = Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local")
        return base / "seohead" / "update-state.json"
    if sys_platform() == "darwin":
        return Path.home() / "Library" / "Application Support" / "seohead" / "update-state.json"
    base = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state")
    return base / "seohead" / "update-state.json"


def sys_platform() -> str:
    """Small indirection to keep platform selection straightforward to fixture."""
    return sys.platform


def _read_state() -> dict[str, Any] | None:
    path = _state_path()
    try:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_STATE_BYTES:
            return None
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, UnicodeDecodeError):
        return None
    if (
        not isinstance(value, dict)
        or value.get("format") != UPDATE_FORMAT
        or not isinstance(value.get("checked_at"), str)
        or not isinstance(value.get("result"), dict)
    ):
        return None
    releases = value["result"].get("releases")
    if releases is not None and not isinstance(releases, list):
        return None
    return value


def _write_state(value: dict[str, Any]) -> str | None:
    path = _state_path()
    try:
        if path.is_symlink():
            return "update state path must not be a symbolic link"
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        descriptor, temporary = tempfile.mkstemp(prefix=".update-state-", dir=path.parent)
        try:
            if hasattr(os, "fchmod"):
                os.fchmod(descriptor, 0o600)
            with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
                descriptor = -1
                content = json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n"
                if len(content.encode("utf-8")) > MAX_STATE_BYTES:
                    return "update state exceeds its byte limit"
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            if descriptor != -1:
                os.close(descriptor)
            Path(temporary).unlink(missing_ok=True)
    except OSError as exc:
        return f"could not persist update state ({type(exc).__name__})"
    return None


def _asset(item: Any, tag: str) -> dict[str, Any] | None:
    if not isinstance(item, dict):
        return None
    name = item.get("name")
    url = item.get("browser_download_url")
    if (
        not isinstance(name, str)
        or not name
        or len(name) > 255
        or "/" in name
        or "\\" in name
        or not isinstance(url, str)
        or len(url) > 512
    ):
        return None
    parts = urlsplit(url)
    expected_path = f"/{REPOSITORY}/releases/download/{tag}/{quote(name, safe='')}"
    if (
        parts.scheme != "https"
        or parts.hostname != "github.com"
        or parts.netloc not in {"github.com", "github.com:443"}
        or parts.username
        or parts.password
        or parts.query
        or parts.fragment
        or parts.path != expected_path
    ):
        return None
    digest = item.get("digest")
    digest = digest.removeprefix("sha256:") if isinstance(digest, str) else None
    if digest is not None and not re.fullmatch(r"[0-9a-f]{64}", digest):
        digest = None
    size = item.get("size")
    if type(size) is not int or size < 0:
        size = None
    return {"name": name, "url": url, "size": size, "sha256": digest}


def _normalize_releases(payload: Any, limit: int) -> list[dict[str, Any]]:
    if not isinstance(payload, list):
        raise ValueError("release feed must be a JSON array")
    releases = []
    for item in payload[:limit]:
        if not isinstance(item, dict) or item.get("draft") is True or item.get("prerelease") is True:
            continue
        tag = item.get("tag_name")
        parsed_version = _version(tag) if isinstance(tag, str) else None
        html_url = item.get("html_url")
        url_parts = urlsplit(html_url) if isinstance(html_url, str) else None
        if (
            parsed_version is None
            or url_parts is None
            or url_parts.scheme != "https"
            or url_parts.hostname != "github.com"
            or url_parts.path != f"/{REPOSITORY}/releases/tag/{tag}"
        ):
            continue
        assets = item.get("assets")
        if not isinstance(assets, list):
            assets = []
        releases.append(
            {
                "tag": tag,
                "version": tag.removeprefix("v"),
                "version_key": parsed_version,
                "url": html_url,
                "published_at": item.get("published_at")
                if isinstance(item.get("published_at"), str)
                else None,
                "changelog": (item.get("body") or "")[:MAX_CHANGELOG_CHARS]
                if isinstance(item.get("body") or "", str)
                else "",
                "assets": [
                    asset
                    for raw in assets[:MAX_ASSETS]
                    if (asset := _asset(raw, tag)) is not None
                ],
            }
        )
    releases.sort(key=lambda release: release["version_key"], reverse=True)
    for release in releases:
        release.pop("version_key", None)
    return releases


def check_updates(
    *, consent: bool, automatic: bool = False, max_releases: int = 5
) -> dict[str, Any]:
    """Check the public GitHub release feed only after explicit caller consent."""
    if type(consent) is not bool or type(automatic) is not bool:
        raise ValueError("consent and automatic must be booleans")
    if type(max_releases) is not int or not 1 <= max_releases <= MAX_RELEASES:
        raise ValueError(f"max_releases must be 1..{MAX_RELEASES}")
    cached = _read_state()
    if not consent:
        return {
            "ok": True,
            "format": UPDATE_FORMAT,
            "state": "consent_required",
            "checked_at": cached.get("checked_at") if cached else None,
            "reason": "release checks require explicit consent",
            "result": cached.get("result") if cached else None,
        }
    if automatic and cached:
        try:
            checked = datetime.fromisoformat(cached["checked_at"].replace("Z", "+00:00"))
            age = (datetime.now(timezone.utc) - checked).total_seconds()
        except (TypeError, ValueError):
            age = AUTO_CHECK_SECONDS + 1
        if 0 <= age < AUTO_CHECK_SECONDS:
            return {
                "ok": True,
                "format": UPDATE_FORMAT,
                "state": "not_due",
                "checked_at": cached["checked_at"],
                "result": cached.get("result"),
                "reason": "the consented daily check was completed within the last 24 hours",
            }

    from seohead.recon.net import http_client

    checked_at = _now()
    try:
        client, _ = http_client(
            10,
            follow_redirects=True,
            headers={"Accept": "application/vnd.github+json", "User-Agent": "SpiderHead"},
        )
        with client:
            with client.stream("GET", RELEASES_URL, params={"per_page": max_releases}) as response:
                if response.status_code != 200:
                    result = {
                        "state": "error",
                        "error": {
                            "code": "release_feed_http_error",
                            "message": f"release feed returned HTTP {response.status_code}",
                        },
                    }
                else:
                    chunks = []
                    size = 0
                    for chunk in response.iter_bytes(64 * 1024):
                        size += len(chunk)
                        if size > MAX_FEED_BYTES:
                            break
                        chunks.append(chunk)
                    if size > MAX_FEED_BYTES:
                        result = {
                            "state": "error",
                            "error": {
                                "code": "release_feed_too_large",
                                "message": "release feed exceeded its response byte limit",
                            },
                        }
                    else:
                        try:
                            payload = json.loads(b"".join(chunks))
                            releases = _normalize_releases(payload, max_releases)
                        except (ValueError, TypeError):
                            releases = None
                        if releases is None:
                            result = {
                                "state": "error",
                                "error": {
                                    "code": "invalid_release_feed",
                                    "message": "release feed was not valid bounded JSON",
                                },
                            }
                        elif not releases:
                            result = {
                                "state": "unavailable",
                                "reason": "the repository has no usable stable release metadata",
                                "latest": None,
                                "releases": [],
                            }
                        else:
                            latest = releases[0]
                            current = _version(__version__)
                            released = _version(latest["version"])
                            result = {
                                "state": "available",
                                "current_version": __version__,
                                "latest": latest,
                                "update_available": bool(current and released and released > current),
                                "releases": releases,
                            }
    except Exception as exc:
        result = {
            "state": "error",
            "error": {
                "code": "release_feed_unreachable",
                "message": f"release feed could not be read ({type(exc).__name__})",
            },
        }
    persisted = _write_state({"format": UPDATE_FORMAT, "checked_at": checked_at, "result": result})
    return {
        "ok": result.get("state") != "error",
        "format": UPDATE_FORMAT,
        "state": result["state"],
        "checked_at": checked_at,
        "persisted": persisted is None,
        "persistence_reason": persisted,
        "result": result,
        "reason": result.get("reason"),
    }


def _checksum(client: Any, release: dict[str, Any], asset: dict[str, Any]) -> tuple[str | None, str | None]:
    if asset.get("sha256"):
        return asset["sha256"], None
    sums = next((item for item in release["assets"] if item["name"] == "SHA256SUMS.txt"), None)
    if sums is None:
        return None, "the release does not provide a SHA-256 checksum for this asset"
    try:
        with client.stream("GET", sums["url"]) as response:
            if response.status_code != 200:
                return None, "the release checksum file was unavailable or exceeded its byte limit"
            chunks = []
            size = 0
            for chunk in response.iter_bytes(16 * 1024):
                size += len(chunk)
                if size > MAX_CHECKSUM_BYTES:
                    return None, "the release checksum file was unavailable or exceeded its byte limit"
                chunks.append(chunk)
        checksum_text = b"".join(chunks).decode("utf-8")
    except (OSError, UnicodeDecodeError):
        return None, "the release checksum file was unavailable or exceeded its byte limit"
    for line in checksum_text.splitlines():
        match = _SHA256_LINE.fullmatch(line.strip())
        if match and match.group(2) == asset["name"]:
            return match.group(1), None
    return None, "the release checksum file has no entry for this asset"


def download_update(
    *,
    consent: bool,
    tag: str,
    asset_name: str,
    out: str,
    max_bytes: int = DEFAULT_MAX_DOWNLOAD_BYTES,
    progress: Callable[[int, int | None], None] | None = None,
    cancelled: Callable[[], bool] | None = None,
) -> dict[str, Any]:
    """Download one checksummed asset from the last checked release without installing it."""
    if consent is not True:
        return {"ok": False, "state": "consent_required", "reason": "download requires explicit consent"}
    if not isinstance(tag, str) or not _VERSION.fullmatch(tag):
        raise ValueError("tag must be a bounded stable release tag")
    if (
        not isinstance(asset_name, str)
        or not asset_name
        or len(asset_name) > 255
        or "/" in asset_name
        or "\\" in asset_name
    ):
        raise ValueError("asset_name must be a bounded filename")
    if type(max_bytes) is not int or not 1 <= max_bytes <= MAX_DOWNLOAD_BYTES:
        raise ValueError(f"max_bytes must be 1..{MAX_DOWNLOAD_BYTES}")
    state = _read_state()
    releases = (state or {}).get("result", {}).get("releases")
    if not isinstance(releases, list) or any(
        not isinstance(item, dict)
        or not isinstance(item.get("tag"), str)
        or not isinstance(item.get("assets"), list)
        for item in releases
    ):
        return {
            "ok": False,
            "state": "unavailable",
            "reason": "checked release metadata is malformed",
        }
    release = next((item for item in releases if item["tag"] == tag), None)
    if release is None:
        return {
            "ok": False,
            "state": "unavailable",
            "reason": "run an update check for this release first",
        }
    cached_assets = release.get("assets")
    if not isinstance(cached_assets, list):
        return {"ok": False, "state": "unavailable", "reason": "checked release asset metadata is invalid"}
    assets = []
    for item in cached_assets[:MAX_ASSETS]:
        if not isinstance(item, dict):
            continue
        digest = item.get("sha256")
        raw = {
            "name": item.get("name"),
            "size": item.get("size"),
            "browser_download_url": item.get("url"),
            "digest": f"sha256:{digest}" if isinstance(digest, str) else None,
        }
        validated = _asset(raw, tag)
        if validated is not None:
            assets.append(validated)
    asset = next((item for item in assets if item["name"] == asset_name), None)
    if asset is None:
        return {"ok": False, "state": "unavailable", "reason": "asset is not in the checked release metadata"}
    if not (asset_name.endswith(".whl") or asset_name.endswith(".tar.gz")):
        return {"ok": False, "state": "unsupported_asset", "reason": "only Python wheel and source archive assets are supported"}
    if not isinstance(out, str) or not out or len(out) > 4096:
        raise ValueError("out must be a bounded output path")
    destination = Path(out).expanduser()
    parent = destination.parent
    if destination.name in {"", ".", ".."} or not parent.is_dir() or os.path.lexists(destination):
        raise ValueError("out must be a new file in an existing directory")
    if destination.is_symlink():
        raise ValueError("out must not be a symbolic link")

    from seohead.recon.net import http_client

    client, _ = http_client(
        30,
        follow_redirects=True,
        headers={"Accept": "application/octet-stream", "User-Agent": "SpiderHead"},
    )
    temporary_path: Path | None = None
    try:
        with client:
            if cancelled is not None and cancelled():
                return {"ok": False, "state": "cancelled", "bytes": 0}
            expected, reason = _checksum(client, {**release, "assets": assets}, asset)
            if expected is None:
                return {"ok": False, "state": "checksum_unavailable", "reason": reason}
            if cancelled is not None and cancelled():
                return {"ok": False, "state": "cancelled", "bytes": 0}
            with client.stream("GET", asset["url"]) as response:
                if response.status_code != 200:
                    return {
                        "ok": False,
                        "state": "error",
                        "error": {"code": "download_http_error", "message": f"asset returned HTTP {response.status_code}"},
                    }
                raw_length = response.headers.get("Content-Length")
                try:
                    total = int(raw_length) if raw_length is not None else None
                except ValueError:
                    total = None
                if total is not None and (total < 0 or total > max_bytes):
                    return {"ok": False, "state": "size_limit", "reason": "asset exceeds the configured byte limit"}
                descriptor, temporary = tempfile.mkstemp(prefix=".seohead-update-", dir=parent)
                temporary_path = Path(temporary)
                digest = hashlib.sha256()
                received = 0
                with os.fdopen(descriptor, "wb") as stream:
                    for chunk in response.iter_bytes(64 * 1024):
                        if cancelled is not None and cancelled():
                            return {"ok": False, "state": "cancelled", "bytes": received}
                        received += len(chunk)
                        if received > max_bytes:
                            return {"ok": False, "state": "size_limit", "reason": "asset exceeded the configured byte limit"}
                        digest.update(chunk)
                        stream.write(chunk)
                        if progress is not None:
                            progress(received, total)
                    stream.flush()
                    os.fsync(stream.fileno())
                if digest.hexdigest() != expected:
                    return {"ok": False, "state": "checksum_mismatch", "reason": "downloaded asset did not match the release SHA-256"}
                if cancelled is not None and cancelled():
                    return {"ok": False, "state": "cancelled", "bytes": received}
                os.link(temporary_path, destination, follow_symlinks=False)
                temporary_path.unlink()
                temporary_path = None
                return {
                    "ok": True,
                    "state": "downloaded",
                    "tag": tag,
                    "asset": asset_name,
                    "path": str(destination),
                    "bytes": received,
                    "sha256": expected,
                    "installed": False,
                }
    except Exception as exc:
        return {
            "ok": False,
            "state": "error",
            "error": {"code": "download_failed", "message": f"asset download failed ({type(exc).__name__})"},
        }
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)
