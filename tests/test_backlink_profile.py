"""Offline backlink source provenance, full rel tokens, filters and isolated history."""

import json
import sqlite3
import urllib.parse

import pytest

from seohead.data_sources import backlink_history, bing_webmaster, yandex_webmaster
from seohead.recon.backlink_profile import (
    build_backlink_profile,
    export_disavow,
    fetch_backlink_source,
)

STAMP = "2026-10-11T12:00:00+00:00"


def _build(rows, **kwargs):
    return build_backlink_profile(
        "example.test", observed_at=STAMP, evidence=[{"source": "generic", "rows": rows}], **kwargs
    )


def test_url_pair_dedupe_keeps_all_anchors_rel_and_per_source_dates():
    result = build_backlink_profile(
        "example.test",
        observed_at=STAMP,
        evidence=[
            {
                "source": "ahrefs",
                "rows": [
                    {
                        "url_from": "https://DONOR.test:443/p#frag",
                        "url_to": "https://example.test/",
                        "anchor": "Brand",
                        "is_nofollow": False,
                        "is_sponsored": True,
                        "first_seen_link": "2026-01-01",
                        "last_visited": "2026-10-01",
                    }
                ],
            },
            {
                "source": "generic",
                "rows": [
                    {
                        "source_url": "https://donor.test/p",
                        "target_url": "https://example.test",
                        "anchor": "Name",
                        "rel": "ugc nofollow external",
                        "first_seen": "2026-02-01",
                        "last_seen": "2026-10-02",
                    }
                ],
            },
        ],
    )
    assert result["unfiltered_backlinks"] == 1
    row = result["rows"][0]
    assert row["rel"] == ["external", "nofollow", "sponsored", "ugc"]
    assert row["rel_conflict"] is True and row["follow"] is None
    assert row["anchors"] == ["Brand", "Name"]
    assert row["first_seen"] == "2026-01-01" and row["last_seen"] == "2026-10-02"
    assert {item["source"] for item in row["observations"]} == {"generic", "ahrefs"}
    assert result["whole_web_complete"] is False


@pytest.mark.parametrize(
    "attributes",
    [
        {"is_nofollow": False},
        {"nofollow": False},
        {"ugc": False},
        {"sponsored": False},
        {"follow": False},
        {"is_dofollow": False},
    ],
)
def test_partial_negative_flag_never_invents_normal_or_nofollow(attributes):
    row = _build(
        [
            {
                "source_url": "https://donor.test/a",
                "target_url": "https://example.test/a",
                **attributes,
            }
        ]
    )["rows"][0]
    assert row["rel"] is None and row["link_types"] == ["unknown"]
    assert row["qualification_complete"] is False
    assert row["follow"] is (
        False if "follow" in attributes or "is_dofollow" in attributes else None
    )


@pytest.mark.parametrize(
    "attributes",
    [{"nofollow": False, "sponsored": False, "ugc": False}, {"follow": True}, {"rel": []}],
)
def test_normal_requires_complete_qualifiers_or_explicit_follow_contract(attributes):
    row = _build(
        [
            {
                "source_url": "https://donor.test/a",
                "target_url": "https://example.test/a",
                **attributes,
            }
        ]
    )["rows"][0]
    assert row["rel"] == [] and row["link_types"] == ["normal"]
    assert row["qualification_complete"] is True and row["follow"] is True


def test_positive_qualifier_is_preserved_without_filling_unknown_flags():
    row = _build(
        [
            {
                "source_url": "https://donor.test/a",
                "target_url": "https://example.test/a",
                "ugc": True,
                "nofollow": False,
            }
        ]
    )["rows"][0]
    assert row["rel"] == ["ugc"] and row["link_types"] == ["ugc"]
    assert row["qualification_complete"] is False and row["follow"] is False
    observation = row["observations"][0]
    assert observation["observed_follow"] is None and observation["rel_complete"] is False


@pytest.mark.parametrize(
    "source,headers",
    [
        ("gsc", "Linking page,Target page,Anchor text"),
        ("ahrefs", "Referring page URL,Target URL,Anchor"),
        ("majestic", "SourceURL,TargetURL,AnchorText"),
        ("moz", "Source URL,Target URL,Anchor Text"),
        ("semrush", "Source URL,Target URL,Anchor"),
    ],
)
def test_real_export_header_aliases(tmp_path, source, headers):
    path = tmp_path / "links.csv"
    path.write_text(
        headers + '\nhttps://donor.test/page,https://example.test/page,"Line one\nLine two"\n'
    )
    result = build_backlink_profile(
        "example.test", observed_at=STAMP, imports=[{"source": source, "path": str(path)}]
    )
    assert result["rows"][0]["anchors"] == ["Line one\nLine two"]
    assert result["rows"][0]["rel"] is None


def test_utf16_tsv_custom_column_mapping_and_query_variants(tmp_path):
    path = tmp_path / "links.csv"
    path.write_text(
        "Donor\tDestination\tAttributes\nhttps://donor.test/a?q=1\thttps://example.test/p\tugc sponsored\nhttps://donor.test/a?q=2\thttps://example.test/p\t\n",
        encoding="utf-16",
    )
    result = build_backlink_profile(
        "example.test",
        imports=[
            {
                "source": "generic",
                "path": str(path),
                "columns": {
                    "source_url": "Donor",
                    "target_url": "Destination",
                    "rel": "Attributes",
                },
            }
        ],
    )
    assert result["summary"]["backlinks"] == 2
    assert result["rows"][1]["link_types"] == ["normal"]


def test_gsc_site_and_top_page_aggregates_do_not_invent_backlink_pairs(tmp_path):
    sites, pages = tmp_path / "sites.csv", tmp_path / "pages.csv"
    sites.write_text("Site,External links,Linked pages\ndonor.test,12,2\n")
    pages.write_text("Target page,External links,Linking sites\nhttps://example.test/p,12,3\n")
    result = build_backlink_profile(
        "example.test",
        imports=[{"source": "gsc", "path": str(sites)}, {"source": "gsc", "path": str(pages)}],
    )
    assert result["rows"] == [] and result["summary"]["referring_domains"] == 1
    assert result["domain_edges"][0]["granularity"] == "domain"
    assert result["aggregate_rows"][0]["target_url"] == "https://example.test/p"


def test_common_crawl_domain_edges_and_wat_urls_are_distinct(tmp_path):
    graph = tmp_path / "graph.csv"
    graph.write_text(
        "source_domain,target_domain\ndonor.test,example.test\nother.test,elsewhere.test\n"
    )
    wat = tmp_path / "sample.jsonl"
    wat.write_text(
        json.dumps(
            {
                "Envelope": {
                    "WARC-Header-Metadata": {"WARC-Target-URI": "https://donor.test/a"},
                    "Payload-Metadata": {
                        "HTTP-Response-Metadata": {
                            "HTML-Metadata": {
                                "Links": [
                                    {
                                        "path": "A@/href",
                                        "url": "https://example.test/p",
                                        "text": "Brand",
                                    },
                                    {"path": "IMG@/src", "url": "https://example.test/image.png"},
                                ]
                            }
                        }
                    },
                }
            }
        )
        + "\n"
    )
    result = build_backlink_profile(
        "example.test",
        imports=[
            {"source": "common_crawl", "path": str(graph), "format": "domain_edges"},
            {"source": "common_crawl", "path": str(wat), "format": "wat"},
        ],
    )
    assert result["summary"]["backlinks"] == 1
    assert (
        len(result["domain_edges"]) == 1
        and result["domain_edges"][0]["source_domain"] == "donor.test"
    )
    assert result["rows"][0]["rel"] is None


def test_invalid_urls_unknown_targets_and_failed_provider_are_not_clean_zero(tmp_path):
    result = _build(
        [
            {"source_url": "javascript:alert(1)", "target_url": "https://example.test/"},
            {
                "source_url": "https://user:secret@donor.test/",
                "target_url": "https://example.test/",
            },
            {"source_url": "https://donor.test/good", "target_url": "https://other.test/"},
            {"source_url": "https://donor.test/missing-target"},
        ]
    )
    assert result["coverage"][0]["rejected_rows"] == 2
    assert result["coverage"][0]["out_of_scope_rows"] == 1
    assert result["coverage"][0]["unknown_targets"] == 1
    assert result["rows"][0]["target_url"] is None
    failed = build_backlink_profile(
        "example.test",
        evidence=[{"source": "ahrefs", "result": {"ok": False, "state": "confirmation_required"}}],
        db=str(tmp_path / "unused.sqlite"),
    )
    assert failed["ok"] is False and failed["state"] == "unavailable" and failed["summary"] is None
    assert not (tmp_path / "unused.sqlite").exists()
    empty = _build([])
    assert empty["ok"] is True and empty["summary"]["backlinks"] == 0


def test_local_filters_spam_unknown_and_404_mapping():
    rows = [
        {
            "source_url": "https://d.test/a",
            "target_url": "https://example.test/shop/a",
            "rel": "sponsored ugc",
            "state": "live",
            "spam_score": 80,
            "first_seen": "2026-10-01",
        },
        {
            "source_url": "https://d.test/b",
            "target_url": "https://example.test/shop/b",
            "rel": "",
            "state": "lost",
            "spam_score": 5,
            "first_seen": "2026-01-01",
        },
        {"source_url": "https://unknown.test/c", "target_url": "https://example.test/"},
    ]
    result = _build(
        rows,
        follow="ugc",
        state="live",
        target_prefix="https://example.test/shop/",
        min_spam_score=70,
        first_seen_from="2026-09-01",
        target_status={"https://example.test/shop/a": 404},
    )
    assert result["summary"]["backlinks"] == result["summary"]["target_404"] == 1
    assert result["rows"][0]["spam"]["penalty_evidence"] is False
    assert _build(rows, max_spam_score=100)["summary"]["backlinks"] == 2
    assert _build(rows, one_per_domain=True)["summary"]["backlinks"] == 2


def test_history_snapshot_retry_absence_and_foreign_db_preservation(tmp_path):
    db = tmp_path / "backlinks.sqlite"
    rows = [{"source_url": "https://donor.test/a", "target_url": "https://example.test/a"}]
    initial = _build(rows, db=str(db))
    repeated = _build(rows, db=str(db))
    assert initial["history"]["snapshot_id"] == repeated["history"]["snapshot_id"]
    assert backlink_history.snapshot_history(db=str(db))["total"] == 1
    empty = build_backlink_profile(
        "example.test",
        observed_at="2026-10-12T12:00:00Z",
        evidence=[{"source": "generic", "rows": []}],
        db=str(db),
    )
    assert empty["history"]["unobserved_since_previous"] == 1
    assert empty["history"]["confirmed_lost"] == 0
    assert (
        backlink_history.read_snapshot(1, db=str(db))["rows"][0]["target_url"]
        == "https://example.test/a"
    )
    foreign = tmp_path / "scan.sqlite"
    with sqlite3.connect(foreign) as con:
        con.execute("CREATE TABLE scan(value TEXT)")
    before = foreign.read_bytes()
    with pytest.raises(ValueError, match="not a backlink"):
        _build(rows, db=str(foreign))
    assert foreign.read_bytes() == before


def test_project_history_path_is_separate_from_sources_and_scan(tmp_path):
    (tmp_path / "project.json").write_text("{}")
    result = _build([], project=str(tmp_path))
    assert result["history"]["db"].endswith("backlinks.sqlite")
    assert not (tmp_path / "sources.sqlite").exists()


def test_bing_link_counts_never_become_donor_urls():
    result = build_backlink_profile(
        "example.test",
        evidence=[
            {
                "source": "bing_webmaster",
                "result": {
                    "ok": True,
                    "operation": "links",
                    "state": "complete",
                    "rows": [{"Url": "https://example.test/page", "Count": 12}],
                },
            }
        ],
    )
    assert result["ok"] is False and result["rows"] == []
    assert result["coverage"][0]["aggregate"][0]["Count"] == 12


def test_disavow_is_explicit_sorted_and_never_overwrites(tmp_path):
    path = tmp_path / "disavow.txt"
    result = export_disavow(
        domains=["DONOR.test", "donor.test"], urls=["https://other.test/p#frag"], output=str(path)
    )
    assert "domain:donor.test\nhttps://other.test/p\n" in result["text"]
    assert result["submitted"] is False
    with pytest.raises(FileExistsError):
        export_disavow(domains=["another.test"], output=str(path))
    assert path.read_text() == result["text"]


def test_free_adapters_actual_documented_responses_and_bounded_paging():
    urls = []

    def bing_transport(url):
        urls.append(url)
        return json.dumps(
            {
                "d": {
                    "Details": [{"Url": "https://donor.test/a", "AnchorText": "Brand"}],
                    "TotalPages": 2,
                }
            }
        )

    collected = fetch_backlink_source(
        "bing_webmaster",
        "example.test",
        {
            "site_url": "https://example.test/",
            "link": "https://example.test/p",
            "api_key": "synthetic",
        },
        transport=bing_transport,
    )
    assert "/GetUrlLinks?" in urls[0]
    assert (
        json.loads(urllib.parse.parse_qs(urllib.parse.urlsplit(urls[0]).query)["link"][0])
        == "https://example.test/p"
    )
    profile = build_backlink_profile("example.test", evidence=[collected])
    assert profile["rows"][0]["anchors"] == ["Brand"] and profile["state"] == "partial"
    wm_urls = []

    def wm_transport(method, url, payload, token):
        wm_urls.append(url)
        return json.dumps(
            {
                "count": 200,
                "links": [
                    {
                        "source_url": "https://donor.test/a",
                        "destination_url": "https://example.test/p",
                        "discovery_date": "2026-01-01",
                        "source_last_access_date": "2026-10-01",
                    }
                ],
            }
        )

    result = yandex_webmaster.collect(
        "external_links_samples",
        user_id="1",
        host_id="h",
        paginate=True,
        max_rows=1,
        token="synthetic",
        transport=wm_transport,
    )
    assert result["truncated"] is True and len(wm_urls) == 1
    assert "/links/external/samples?offset=0&limit=1" in wm_urls[0]
    assert bing_webmaster.collect(
        "url_links",
        site_url="x",
        link="x",
        api_key="synthetic",
        transport=lambda url: json.dumps({"d": {"Details": [], "TotalPages": 0}}),
    )["ok"]
