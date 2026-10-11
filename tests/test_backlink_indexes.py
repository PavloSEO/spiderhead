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


def test_moz_backlinks_uses_explicit_json_rpc_operation_and_keeps_unknown_rel(
    tmp_path, monkeypatch
):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    seen = []

    def transport(request):
        seen.append(request)
        sent = json.loads(request.data)
        assert request.full_url == "https://api.moz.com/jsonrpc"
        assert request.headers["X-moz-token"] == "synthetic-moz-token"
        assert sent["jsonrpc"] == "2.0" and sent["method"] == "data.site.link.list"
        assert sent["params"]["data"]["site_query"] == {
            "query": "example.test",
            "scope": "root_domain",
        }
        assert sent["params"]["options"] == {"scope": "page", "filters": ["external"]}
        assert sent["params"]["offset"] == {"limit": 25, "token": None}
        return json.dumps(
            {
                "jsonrpc": "2.0",
                "id": sent["id"],
                "result": {
                    "links": [
                        {
                            "source_url": "https://donor.test/a",
                            "target_url": "https://example.test/p",
                            "anchor_text": "Name",
                            "rel": ["sponsored", "custom-rel"],
                            "is_canonical": True,
                            "is_redirect": False,
                        }
                    ],
                    "offset": {"token": "opaque-next"},
                },
            }
        )

    result = backlink_indexes.collect(
        "moz",
        "example.test",
        operation="backlinks",
        api_key="synthetic-moz-token",
        confirm_paid=True,
        transport=transport,
    )
    assert len(seen) == 1
    assert result["operation"] == "backlinks" and result["estimate"]["legacy"] is False
    assert result["rows"][0]["rel"] == ["sponsored", "custom-rel"]
    assert result["rows"][0]["is_canonical"] is True
    assert result["rows"][0]["is_redirect"] is False
    assert result["next_token"] == "opaque-next" and result["truncated"] is True
    assert result["receipt"]["cost"] is None
    assert result["receipt"]["extra"]["cost_unknown"] is True
    assert "synthetic-moz-token" not in json.dumps(result)


def test_moz_backlinks_rejects_explicit_limit_over_provider_max_and_preserves_default_mode():
    with pytest.raises(ValueError, match=r"1\.\.50"):
        backlink_indexes.estimate("moz", operation="backlinks", limit=51)
    legacy = backlink_indexes.estimate("moz")
    assert legacy["operation"] == "url_metrics_v2" and legacy["legacy"] is True
    modern = backlink_indexes.estimate("moz", operation="backlinks")
    assert modern["operation"] == "backlinks" and modern["max_rows"] == 25


def test_moz_backlinks_rejects_mismatched_json_rpc_id_and_keeps_unknown_receipt(
    tmp_path, monkeypatch
):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))

    def transport(request):
        return json.dumps(
            {
                "jsonrpc": "2.0",
                "id": "wrong-id",
                "result": {"links": [], "offset": {"token": None}},
            }
        )

    result = backlink_indexes.collect(
        "moz",
        "example.test",
        operation="backlinks",
        api_key="synthetic-moz-token",
        confirm_paid=True,
        transport=transport,
    )
    assert result["state"] == "failed"
    assert spend.read_all()[0]["cost"] is None


def test_backlink_response_byte_limit_leaves_unknown_receipt(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    monkeypatch.setattr(backlink_indexes, "MAX_RESPONSE_BYTES", 1)
    result = backlink_indexes.collect(
        "ahrefs",
        "example.test",
        api_key="synthetic-key",
        confirm_paid=True,
        transport=lambda request: ("{}", {}),
    )
    assert result["state"] == "failed"
    assert spend.read_all()[0]["cost"] is None


def test_index_network_failure_never_exposes_key(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))

    def fail(request):
        raise urllib.error.URLError("secret-canary")

    result = backlink_indexes.collect(
        "ahrefs", "example.test", confirm_paid=True, api_key="secret-canary", transport=fail
    )
    assert result["state"] == "failed" and "secret-canary" not in json.dumps(result)
    assert spend.read_all()[0]["extra"]["response_received"] is False
    assert spend.read_all()[0]["cost"] is None
