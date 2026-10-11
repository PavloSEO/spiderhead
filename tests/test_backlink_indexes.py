"""Paid backlink index calls are opt-in, bounded, redacted and journaled offline."""

import json
import urllib.error
import urllib.parse

import pytest

from seohead.data_sources import backlink_indexes, spend
from seohead.recon.backlink_profile import build_backlink_profile


@pytest.mark.parametrize("provider", ["ahrefs", "majestic", "moz"])
def test_preflight_has_no_network_or_credential_reads(monkeypatch, provider):
    def forbidden(*args, **kwargs):
        raise AssertionError("unexpected external access")

    monkeypatch.setattr(backlink_indexes.credentials, "read", forbidden)
    result = backlink_indexes.collect(provider, "example.test", transport=forbidden)
    assert result["state"] == "confirmation_required"
    assert result["estimate"]["currency_cost"] is None
    assert result["estimate"]["legacy"] is (provider == "moz")


def test_ahrefs_estimate_explicit_documented_default_field_weights():
    quote = backlink_indexes.estimate("ahrefs", limit=100)
    assert quote["estimated_units"] == {"api_units": 1100}
    assert set(quote["field_units"].values()) == {1}
    assert "traffic" not in quote["field_units"]
    assert backlink_indexes.estimate("ahrefs", limit=1)["estimated_units"] == {"api_units": 50}


@pytest.mark.parametrize(
    "provider,limit,ceiling", [("ahrefs", 100, 100), ("majestic", 100, 4999), ("moz", 1, 10000)]
)
def test_unit_ceilings_refuse_without_transport(provider, limit, ceiling):
    result = backlink_indexes.collect(
        provider,
        "example.test",
        limit=limit,
        max_units=ceiling,
        confirm_paid=True,
        transport=lambda request: pytest.fail("called transport"),
    )
    assert result["state"] == "cost_limit"


def test_ahrefs_real_shape_rel_and_receipt_before_parsing(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    requests = []

    def transport(request):
        requests.append(request)
        return json.dumps(
            {
                "backlinks": [
                    {
                        "url_from": "https://donor.test/a",
                        "url_to": "https://example.test/p",
                        "anchor": "Brand",
                        "is_nofollow": False,
                        "is_sponsored": True,
                        "is_ugc": True,
                        "is_lost": False,
                        "first_seen_link": "2026-01-01",
                        "last_visited": "2026-10-01",
                    }
                ]
            }
        ), {"X-Api-Units-Cost-Total-Actual": "50"}

    result = backlink_indexes.collect(
        "ahrefs",
        "example.test",
        limit=1,
        api_key="secret-canary",
        confirm_paid=True,
        transport=transport,
    )
    assert urllib.parse.urlsplit(requests[0].full_url).hostname == "api.ahrefs.com"
    assert requests[0].headers["Authorization"] == "Bearer secret-canary"
    assert "secret-canary" not in json.dumps(result)
    assert spend.read_all()[0]["cost"] == 50
    profile = build_backlink_profile(
        "example.test", evidence=[{"source": "ahrefs", "result": result}]
    )
    assert profile["rows"][0]["rel"] == ["sponsored", "ugc"]
    broken = backlink_indexes.collect(
        "ahrefs",
        "example.test",
        api_key="secret-canary",
        confirm_paid=True,
        transport=lambda request: ("not json", {"x-api-units-cost-total-actual": "5"}),
    )
    assert broken["state"] == "failed" and spend.read_all()[-1]["cost"] == 5


def test_majestic_fixed_post_and_documented_source_fields(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    requests = []

    def transport(request):
        requests.append(request)
        return json.dumps(
            {
                "Code": "OK",
                "DataTables": {
                    "BackLinks": {
                        "Data": [
                            {
                                "SourceURL": "https://donor.test/a",
                                "TargetURL": "https://example.test/p",
                                "AnchorText": "Name",
                                "FlagNoFollow": 1,
                                "FlagDeleted": 1,
                                "FirstIndexedDate": "2026-01-01",
                                "LastSeenDate": "2026-09-30",
                                "DateLost": "2026-10-01",
                                "SourceTrustFlow": 22,
                                "SourceOutLinksExternal": 1200,
                            }
                        ]
                    }
                },
            }
        )

    result = backlink_indexes.collect(
        "majestic",
        "example.test",
        offset=100,
        limit=2,
        api_key="secret-canary",
        confirm_paid=True,
        transport=transport,
    )
    params = urllib.parse.parse_qs(requests[0].data.decode())
    assert params["Count"] == ["2"] and params["From"] == ["100"]
    assert requests[0].method == "POST" and "secret-canary" not in requests[0].full_url
    profile = build_backlink_profile(
        "example.test", evidence=[{"source": "majestic", "result": result}]
    )
    row = profile["rows"][0]
    assert row["state"] == "lost" and row["lost_at"] == "2026-10-01"
    assert row["last_seen"] == "2026-09-30" and row["spam"]["score"] == 30
    assert spend.report()["uncertain"][0]["extra"]["cost_unknown"] is True


def test_moz_legacy_url_metrics_are_not_backlinks(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    result = backlink_indexes.collect(
        "moz",
        "example.test/p",
        confirm_paid=True,
        access_id="synthetic",
        secret_key="secret-canary",
        transport=lambda request: json.dumps(
            {"results": [{"page": "example.test/p", "domain_authority": 30}]}
        ),
    )
    assert result["url_metrics"][0]["domain_authority"] == 30 and result["rows"] is None
    assert result["estimate"]["legacy"] is True
    profile = build_backlink_profile("example.test", evidence=[{"source": "moz", "result": result}])
    assert profile["state"] == "unavailable" and profile["summary"] is None


def test_index_network_failure_never_exposes_key(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))

    def fail(request):
        raise urllib.error.URLError("secret-canary")

    result = backlink_indexes.collect(
        "ahrefs", "example.test", confirm_paid=True, api_key="secret-canary", transport=fail
    )
    assert result["state"] == "failed" and "secret-canary" not in json.dumps(result)
    assert spend.read_all()[0]["extra"]["response_received"] is False
