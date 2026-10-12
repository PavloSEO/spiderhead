"""Local Cloudflare aggregate history is append-only and contains no request IPs."""

from seohead.data_sources.cloudflare_history import list_snapshots, list_zones, save_snapshot


def _result(**overrides):
    return {
        "ok": True,
        "zone_id": "synthetic-zone-id",
        "zone": "example.test",
        "period": {"from": "2026-10-01", "to": "2026-10-01"},
        "url_facts": [
            {
                "host": "example.test",
                "path": "/page",
                "date": "2026-10-01",
                "bot_family": "ai",
                "bot_name": "ExampleBot",
                "verified_bot_category": "AI Crawler",
                "edge_status": 200,
                "origin_status": 200,
                "cache_status": "hit",
                "edge_redirect": False,
                "requests": 3,
            }
        ],
        "url_facts_truncated": False,
        "url_facts_omitted": 0,
        "requests": 3,
        "sampled": False,
        "truncated_days": [],
        "verification": {"checked": False},
        "note": "aggregate test fixture",
        **overrides,
    }


def test_cloudflare_snapshot_history_is_bounded_local_and_url_keyed(tmp_path):
    (tmp_path / "project.json").write_text("{}", encoding="utf-8")
    saved = save_snapshot(_result(), project=str(tmp_path), observed_at="2026-10-02T00:00:00Z")

    metadata = list_snapshots(project=str(tmp_path), zone_id="synthetic-zone-id")
    page = list_snapshots(project=str(tmp_path), zone_id="synthetic-zone-id", include_facts=True)
    zones = list_zones(project=str(tmp_path))

    assert saved["state"] == "stored" and saved["snapshot_state"] == "complete"
    assert page["total"] == 1 and page["rows"][0]["url_facts"][0]["path"] == "/page"
    assert metadata["rows"][0]["url_facts_available"] is True
    assert "url_facts" not in metadata["rows"][0]
    assert zones["zones"][0]["zone_id"] == "synthetic-zone-id"
    assert zones["zones"][0]["sampled"] is False
    assert "ClientIP" not in str(page)


def test_cloudflare_partial_sample_stays_partial_in_retained_history(tmp_path):
    (tmp_path / "project.json").write_text("{}", encoding="utf-8")
    result = _result(sampled=True, truncated_days=["2026-10-01"], url_facts_truncated=True)
    save_snapshot(result, project=str(tmp_path), observed_at="2026-10-02T00:00:00Z")

    stored = list_snapshots(project=str(tmp_path), include_facts=True)["rows"][0]

    assert stored["state"] == "partial"
    assert stored["sampled"] is True
    assert stored["truncated_days"] == ["2026-10-01"]
    assert stored["url_facts_truncated"] is True


def test_facts_in_history_are_explicitly_paged_and_metadata_stays_bounded(tmp_path):
    (tmp_path / "project.json").write_text("{}", encoding="utf-8")
    for day in range(1, 11):
        save_snapshot(
            _result(period={"from": f"2026-10-{day:02d}", "to": f"2026-10-{day:02d}"}),
            project=str(tmp_path),
            observed_at=f"2026-10-{day:02d}T00:00:00Z",
        )

    metadata = list_snapshots(project=str(tmp_path), limit=100)
    facts = list_snapshots(project=str(tmp_path), limit=100, include_facts=True)

    assert len(metadata["rows"]) == 10
    assert all("url_facts" not in row for row in metadata["rows"])
    assert len(facts["rows"]) == 8
    assert facts["has_more"] is True
    assert facts["next_offset"] == 8
