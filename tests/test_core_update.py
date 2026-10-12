"""Consent, bounds and release identity for issue #1291 update contracts."""

from __future__ import annotations

import hashlib
import json
from typing import ClassVar

from seohead.core import update


def test_update_check_requires_consent_without_network_or_state_write(monkeypatch):
    monkeypatch.setattr(update, "_read_state", lambda: None)
    monkeypatch.setattr(
        update,
        "_write_state",
        lambda _value: (_ for _ in ()).throw(AssertionError("must not persist without consent")),
    )

    result = update.check_updates(consent=False)

    assert result == {
        "ok": True,
        "format": update.UPDATE_FORMAT,
        "state": "consent_required",
        "checked_at": None,
        "reason": "release checks require explicit consent",
        "result": None,
    }


def test_automatic_check_reuses_a_fresh_consent_backed_result(monkeypatch):
    saved = {
        "format": update.UPDATE_FORMAT,
        "checked_at": update._now(),
        "result": {"state": "available", "update_available": False},
    }
    monkeypatch.setattr(update, "_read_state", lambda: saved)
    monkeypatch.setattr(
        "seohead.recon.net.http_client",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(AssertionError("must not request")),
    )

    result = update.check_updates(consent=True, automatic=True)

    assert result["state"] == "not_due"
    assert result["result"] == saved["result"]


def test_release_feed_is_bounded_and_assets_are_host_and_tag_bound():
    digest = "a" * 64
    payload = [
        {
            "tag_name": "v3.1.0",
            "draft": False,
            "prerelease": False,
            "html_url": "https://github.com/PavloSEO/spiderhead/releases/tag/v3.1.0",
            "published_at": "2026-10-12T12:00:00Z",
            "body": "x" * (update.MAX_CHANGELOG_CHARS + 10),
            "assets": [
                {
                    "name": "seohead_seotools-3.1.0-py3-none-any.whl",
                    "size": 10,
                    "digest": f"sha256:{digest}",
                    "browser_download_url": "https://github.com/PavloSEO/spiderhead/releases/download/v3.1.0/seohead_seotools-3.1.0-py3-none-any.whl",
                },
                {
                    "name": "malicious.whl",
                    "size": 10,
                    "digest": f"sha256:{digest}",
                    "browser_download_url": "https://attacker.example/malicious.whl",
                },
            ],
        }
    ]

    result = update._normalize_releases(json.loads(json.dumps(payload)), limit=1)

    assert result[0]["tag"] == "v3.1.0"
    assert len(result[0]["changelog"]) == update.MAX_CHANGELOG_CHARS
    assert [asset["name"] for asset in result[0]["assets"]] == [
        "seohead_seotools-3.1.0-py3-none-any.whl"
    ]
    assert result[0]["assets"][0]["sha256"] == digest


def test_update_check_persists_only_a_bounded_valid_release_result(monkeypatch):
    payload = [
        {
            "tag_name": "v3.1.0",
            "draft": False,
            "prerelease": False,
            "html_url": "https://github.com/PavloSEO/spiderhead/releases/tag/v3.1.0",
            "assets": [],
        }
    ]

    class Response:
        status_code = 200

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        @staticmethod
        def iter_bytes(_size):
            yield json.dumps(payload).encode()

    class Client:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        @staticmethod
        def stream(_method, _url, params):
            assert params == {"per_page": 1}
            return Response()

    monkeypatch.setattr("seohead.recon.net.http_client", lambda *_a, **_kw: (Client(), False))
    saved = {}
    monkeypatch.setattr(update, "_write_state", lambda value: saved.update(value) or None)

    result = update.check_updates(consent=True, max_releases=1)

    assert result["state"] == "available"
    assert result["result"]["update_available"] is True
    assert saved["format"] == update.UPDATE_FORMAT
    assert saved["result"]["latest"]["tag"] == "v3.1.0"


def test_download_requires_consent_before_reading_cached_release(monkeypatch):
    monkeypatch.setattr(
        update,
        "_read_state",
        lambda: (_ for _ in ()).throw(AssertionError("must not inspect state without consent")),
    )

    result = update.download_update(
        consent=False,
        tag="v3.1.0",
        asset_name="seohead_seotools-3.1.0-py3-none-any.whl",
        out="update.whl",
    )

    assert result["state"] == "consent_required"


def test_download_reports_malformed_cached_release_rows(monkeypatch):
    monkeypatch.setattr(update, "_read_state", lambda: {"result": {"releases": [None]}})

    result = update.download_update(
        consent=True,
        tag="v3.1.0",
        asset_name="seohead_seotools-3.1.0-py3-none-any.whl",
        out="update.whl",
    )

    assert result["state"] == "unavailable"
    assert result["reason"] == "checked release metadata is malformed"


def test_download_cancellation_removes_partial_file(tmp_path, monkeypatch):
    body = b"synthetic wheel bytes"
    asset = {
        "name": "seohead_seotools-3.1.0-py3-none-any.whl",
        "url": "https://github.com/PavloSEO/spiderhead/releases/download/v3.1.0/seohead_seotools-3.1.0-py3-none-any.whl",
        "size": len(body),
        "sha256": hashlib.sha256(body).hexdigest(),
    }
    release = {"tag": "v3.1.0", "assets": [asset]}
    monkeypatch.setattr(update, "_read_state", lambda: {"result": {"releases": [release]}})

    class Response:
        status_code = 200
        headers: ClassVar[dict[str, str]] = {"Content-Length": str(len(body))}

        @staticmethod
        def iter_bytes(_size):
            yield body[:5]
            yield body[5:]

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

    class Client:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        @staticmethod
        def stream(_method, _url):
            return Response()

    monkeypatch.setattr("seohead.recon.net.http_client", lambda *_a, **_kw: (Client(), False))
    calls = 0

    def cancelled():
        nonlocal calls
        calls += 1
        return calls > 3

    destination = tmp_path / asset["name"]
    progress = []
    result = update.download_update(
        consent=True,
        tag="v3.1.0",
        asset_name=asset["name"],
        out=str(destination),
        progress=lambda received, total: progress.append((received, total)),
        cancelled=cancelled,
    )

    assert result["state"] == "cancelled"
    assert result["bytes"] == 5
    assert progress == [(5, len(body))]
    assert not destination.exists()
    assert list(tmp_path.iterdir()) == []
