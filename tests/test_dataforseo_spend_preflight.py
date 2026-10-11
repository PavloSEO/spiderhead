"""Offline price quotes and production refusals precede credentials and transport."""

from types import SimpleNamespace

import pytest

from seohead.data_sources import dataforseo, spend
from seohead.mcp import handlers


def prices(operation, request=0.02, row=0.00003):
    node = {
        "priority_normal": [
            {"cost_type": "per_request", "cost": request},
            {"cost_type": "per_result", "cost": row},
        ]
    }
    for part in reversed(dataforseo.ENDPOINTS[operation].split("/")[1:]):
        node = {part: node}
    return node


@pytest.mark.parametrize(
    "operation,args",
    [
        ("search_volume", (["synthetic"],)),
        ("keyword_ideas", ("synthetic",)),
        ("keyword_difficulty", (["synthetic"],)),
        ("serp", ("synthetic",)),
    ],
)
def test_every_production_operation_refuses_before_client(monkeypatch, operation, args):
    monkeypatch.setenv("DATAFORSEO_ENV", "prod")
    monkeypatch.setattr(
        dataforseo, "DataForSEOClient", lambda **kw: pytest.fail("no credential/client lookup")
    )
    result = getattr(dataforseo, operation)(*args)
    assert result["state"] == "confirmation_required"
    assert result["estimate"]["cost_usd"] is None
    assert result["estimate"]["cache_hits"] == 0


def test_quote_uses_provider_snapshot_and_names_declared_cache():
    result = dataforseo.estimate(
        "backlinks",
        query_count=3,
        cache_hits=1,
        row_limit=100,
        prices=prices("backlinks"),
        env="prod",
    )
    assert result["cost_usd"] == 0.046
    assert result["billable_queries"] == 2
    assert result["pricing_source"] == "appendix/user_data"


def test_missing_or_unsupported_price_is_not_zero():
    assert dataforseo.estimate("backlinks", env="prod")["cost_usd"] is None
    assert dataforseo.estimate("backlinks", env="prod", prices={})["state"] == "unavailable"


@pytest.mark.parametrize("ceiling", [float("nan"), float("inf"), -1, True])
def test_invalid_ceiling_rejected_before_network(ceiling):
    with pytest.raises(ValueError):
        dataforseo.search_volume(["synthetic"], spend_ceiling_usd=ceiling)


def test_ceiling_refuses_unknown_or_excess_price_before_client(monkeypatch):
    monkeypatch.setattr(dataforseo, "DataForSEOClient", lambda **kw: pytest.fail("no network"))
    kw = {"env": "prod", "confirm_paid": True, "spend_ceiling_usd": 0.01}
    assert dataforseo.search_volume(["synthetic"], **kw)["state"] == "unavailable"
    assert (
        dataforseo.search_volume(["synthetic"], prices=prices("search_volume"), **kw)["state"]
        == "spend_limit_exceeded"
    )


@pytest.mark.parametrize(
    "operation",
    [
        "backlinks_summary",
        "backlinks",
        "referring_domains",
        "backlinks_history",
        "domain_pages_summary",
    ],
)
def test_all_backlink_endpoints_are_explicit_and_journal_actual_cost(
    monkeypatch, tmp_path, operation
):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    calls = []

    class Client:
        env = "sandbox"

        def post(self, endpoint, payload, **kw):
            calls.append((endpoint, payload))
            return {
                "cost": 0.023,
                "tasks": [
                    {
                        "status_code": 20000,
                        "result": [
                            {
                                "items": [
                                    {
                                        "url_from": "https://donor.example/",
                                        "url_to": "https://example.com/",
                                    }
                                ]
                            }
                        ],
                    }
                ],
            }

    kw = dict(enabled=True, account_eligible=True, cache_key="synthetic", client=Client())
    assert (
        dataforseo.backlink_call(operation, "example.com", **kw)["state"] == "confirmation_required"
    )
    assert calls == []
    result = dataforseo.backlink_call(operation, "example.com", confirm_paid=True, **kw)
    assert result["ok"] is True
    assert calls[0][0] == dataforseo.ENDPOINTS[operation]
    assert spend.read_all()[0]["cost"] == 0.023


def test_missing_cost_stays_unknown_in_journal(monkeypatch, tmp_path):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    client = SimpleNamespace(
        env="sandbox", post=lambda *a, **kw: {"tasks": [{"status_code": 20000, "result": []}]}
    )
    _, _, cost, _ = dataforseo._run(client, "backlinks", [{"target": "example.com"}], 1)
    assert cost is None
    assert spend.report()["by_source"] == {}
    assert spend.report()["uncertain"][0]["extra"]["cost_unknown"] is True


def test_handler_propagates_production_confirmation(monkeypatch):
    calls = []
    monkeypatch.setattr(dataforseo, "serp", lambda *a, **kw: calls.append(kw) or {"ok": True})
    handlers.google_serp("synthetic", env="prod", confirm_paid=True, spend_ceiling_usd=0.2)
    assert calls[0]["confirm_paid"] is True
    assert calls[0]["spend_ceiling_usd"] == 0.2


def test_two_step_keyword_ceiling_is_checked_before_either_call(monkeypatch):
    monkeypatch.setattr(dataforseo, "keyword_ideas", lambda *a, **kw: pytest.fail("no paid call"))
    quote = prices("keyword_ideas", request=0.02, row=0)
    quote["dataforseo_labs"]["google"].update(
        prices("keyword_difficulty", request=0.02, row=0)["dataforseo_labs"]["google"]
    )
    result = handlers.google_keywords(
        seed="synthetic",
        difficulty=True,
        env="prod",
        confirm_paid=True,
        spend_ceiling_usd=0.03,
        prices=quote,
    )
    assert result["state"] == "spend_limit_exceeded"


def test_profile_validates_all_sources_before_any_call(monkeypatch):
    monkeypatch.setattr(dataforseo, "backlink_call", lambda *a, **kw: pytest.fail("no paid call"))
    with pytest.raises(ValueError, match="supported source"):
        handlers.backlinks_profile(
            "example.com", sources=[{"source": "dataforseo", "request": {}}, {"source": "unknown"}]
        )


def test_profile_and_disavow_handlers_use_actual_core_offline(tmp_path):
    fixture = tmp_path / "links.csv"
    fixture.write_text(
        "source_url,target_url,anchor,rel\nhttps://donor.example/a,https://example.com/page,synthetic,sponsored ugc\n"
    )
    result = handlers.backlinks_profile(
        "example.com", imports=[{"source": "generic", "path": str(fixture)}]
    )
    assert result["rows"][0]["rel"] == ["sponsored", "ugc"]
    output = tmp_path / "disavow.txt"
    response = handlers.backlinks_disavow(domains=["donor.example"], output=str(output))
    assert response["submitted"] is False
    assert "domain:donor.example" in output.read_text()


def test_history_bound_is_sent_before_provider(monkeypatch):
    calls = []
    monkeypatch.setattr(
        dataforseo,
        "_run",
        lambda client, op, payload, n: calls.append(payload) or ([], [], 0.0, False),
    )
    result = dataforseo.backlink_call(
        "backlinks_history",
        "example.com",
        enabled=True,
        account_eligible=True,
        cache_key="synthetic",
        confirm_paid=True,
        client=SimpleNamespace(env="sandbox"),
        date_from="2026-01-01",
        date_to="2026-02-01",
        limit=2,
    )
    assert result["ok"] is True
    assert calls[0][0]["date_from"] == "2026-01-01"
    with pytest.raises(ValueError, match="row limit"):
        dataforseo.backlink_call(
            "backlinks_history",
            "example.com",
            enabled=True,
            account_eligible=True,
            cache_key="synthetic",
            confirm_paid=True,
            date_from="2026-01-01",
            date_to="2026-02-01",
            limit=1,
        )


def test_refresh_prices_is_an_explicit_free_read(monkeypatch):
    calls = []

    class Client:
        def __init__(self, **kw):
            calls.append(kw)

        def balance(self):
            return {"prices": prices("backlinks")}

    monkeypatch.setattr(dataforseo, "DataForSEOClient", Client)
    assert handlers.dataforseo_estimate("backlinks", env="prod")["state"] == "unavailable"
    assert calls == []
    result = handlers.dataforseo_estimate(
        "backlinks", env="prod", row_limit=100, refresh_prices=True
    )
    assert result["cost_usd"] == 0.023
    assert calls == [{"env": "prod"}]


def test_profile_retained_history_handler_never_collects(monkeypatch, tmp_path):
    database = tmp_path / "backlinks.sqlite"
    evidence = [
        {
            "source": "generic",
            "rows": [
                {
                    "source_url": "https://donor.example/a",
                    "target_url": "https://example.com/",
                    "rel": "ugc",
                }
            ],
        }
    ]
    handlers.backlinks_profile("example.com", evidence=evidence, db=str(database))
    monkeypatch.setattr(
        dataforseo, "backlink_call", lambda *a, **kw: pytest.fail("offline history")
    )
    history = handlers.backlinks_profile("example.com", db=str(database), history_only=True)
    assert history["total"] == 1
    assert (
        handlers.backlinks_profile("https://example.com/", db=str(database), history_only=True)[
            "total"
        ]
        == 1
    )


@pytest.mark.parametrize("body", [[], None, "not an object"])
def test_non_object_paid_response_still_has_an_unknown_receipt(monkeypatch, tmp_path, body):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))

    class Client:
        env = "sandbox"

        def __init__(self, **kwargs):
            pass

        def post(self, *args, **kwargs):
            return body

    monkeypatch.setattr(dataforseo, "DataForSEOClient", Client)
    result = dataforseo.search_volume(["synthetic"])
    assert result["ok"] is False and result["state"] == "failed"
    assert spend.report()["by_source"] == {}
    receipt = spend.report()["uncertain"][0]
    assert receipt["extra"]["response_received"] is True
    assert receipt["extra"]["cost_unknown"] is True


def test_serp_quote_includes_depth_and_search_operator_surcharges(monkeypatch):
    quote = prices("serp", request=0.002, row=0)
    assert (
        dataforseo.estimate("serp", env="prod", prices=quote, row_limit=30, query="synthetic")[
            "cost_usd"
        ]
        == 0.006
    )
    result = dataforseo.estimate(
        "serp", env="prod", prices=quote, row_limit=30, query="site:example.test synthetic"
    )
    assert result["cost_usd"] == 0.03
    assert result["billable_serp_pages_per_query"] == 3
    assert result["search_operator_multiplier"] == 5
    assert (
        dataforseo.estimate(
            "serp", env="prod", prices=quote, row_limit=30, query="site%3Aexample.test"
        )["cost_usd"]
        == 0.03
    )
    assert dataforseo.estimate("serp", env="prod", prices=quote)["state"] == "unavailable"
    monkeypatch.setattr(
        dataforseo,
        "DataForSEOClient",
        lambda **kw: pytest.fail("ceiling must refuse before client"),
    )
    result = dataforseo.serp(
        "site:example.test synthetic",
        env="prod",
        confirm_paid=True,
        depth=30,
        prices=quote,
        spend_ceiling_usd=0.02,
    )
    assert result["state"] == "spend_limit_exceeded"


@pytest.mark.parametrize("depth", [0, 201, True])
def test_serp_depth_is_bounded_before_transport(depth):
    with pytest.raises(ValueError, match="depth"):
        dataforseo.serp("synthetic", depth=depth)


def test_url_shaped_profile_domain_is_normalized_before_paid_source(monkeypatch):
    calls = []
    monkeypatch.setattr(
        dataforseo,
        "backlink_call",
        lambda operation, target, **kw: (
            calls.append(target) or {"ok": False, "state": "confirmation_required"}
        ),
    )
    handlers.backlinks_profile("https://example.com/", sources=[{"source": "dataforseo"}])
    assert calls == ["example.com"]
    with pytest.raises(ValueError, match="domain"):
        handlers.backlinks_profile("", history_only=True, db="unread.sqlite")
