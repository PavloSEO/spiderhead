"""The shipped extraction preset library: every file validates, loads and evaluates."""

from __future__ import annotations

import pytest

from seohead.checks.extraction_library import available, preset
from seohead.checks.extraction_rules import evaluate

HTML = """
<html><head><title>Shop</title><meta name="description" content="Pumps">
<link rel="canonical" href="https://shop.test/">
</head><body>
<h1>Pumps</h1><h2>Centrifugal</h2><h3>Submersible</h3>
<img src="/pump.png" alt="A pump"><img src="/none.png" alt=""><img src="/bare.png">
<a href="/catalog" rel="nofollow">Catalog</a><a href="https://other.test/">Other</a>
<div style="color:red">inline</div><style>.a{}</style>
<article><h2>Article heading</h2></article><section><h3>Section heading</h3></section>
<script type="application/ld+json">{"@type":"Product"}</script>
</body></html>
"""


def test_every_shipped_preset_validates_and_loads():
    shipped = available()
    assert len(shipped) >= 14
    for item in shipped:
        rules = preset(item["name"])  # case-insensitive
        assert rules and all(rule["id"] for rule in rules)
        assert item["rules"] == len(rules)


def test_unknown_preset_names_every_shipped_one():
    with pytest.raises(ValueError) as exc:
        preset("does-not-exist")
    for item in available():
        assert item["name"] in str(exc.value)


def test_headings_preset_extracts_the_outline():
    out = evaluate(
        html=HTML, parsed={"jsonld": []}, rules=preset("headings"), representation="static"
    )
    by_id = {row["id"]: row for row in out["rules"]}
    assert by_id["h1"]["value"] == ["Pumps"]
    assert by_id["h2"]["value"] == ["Centrifugal", "Article heading"]
    assert by_id["h3"]["value"] == ["Submersible", "Section heading"]
    assert by_id["heading-count"]["value"] == 5  # h1 + two h2 + two h3


def test_images_links_and_inline_css_presets_answer_the_common_questions():
    images = {
        row["id"]: row
        for row in evaluate(
            html=HTML, parsed={"jsonld": []}, rules=preset("images"), representation="static"
        )["rules"]
    }
    assert (
        images["img-alt"]["value"] == ["A pump", "", None][:2]
        or images["img-alt-empty"]["value"] == 1
    )
    assert images["img-count"]["value"] == 3
    links = {
        row["id"]: row
        for row in evaluate(
            html=HTML, parsed={"jsonld": []}, rules=preset("links"), representation="static"
        )["rules"]
    }
    assert links["a-nofollow"]["value"] == 1
    inline = {
        row["id"]: row
        for row in evaluate(
            html=HTML, parsed={"jsonld": []}, rules=preset("inline-css"), representation="static"
        )["rules"]
    }
    assert inline["style-attr"]["value"] == ["color:red"]
    assert inline["style-attr-count"]["value"] == 1


def test_structure_meta_and_jsonld_presets_cover_the_inventory():
    structure = {
        row["id"]: row
        for row in evaluate(
            html=HTML, parsed={"jsonld": []}, rules=preset("structure"), representation="static"
        )["rules"]
    }
    assert structure["article-count"]["value"] == 1
    assert structure["section-count"]["value"] == 1
    meta = {
        row["id"]: row
        for row in evaluate(
            html=HTML, parsed={"jsonld": []}, rules=preset("meta"), representation="static"
        )["rules"]
    }
    assert meta["canonical"]["value"] == ["https://shop.test/"]
    ld = {
        row["id"]: row
        for row in evaluate(
            html=HTML,
            parsed={"jsonld": [{"@type": "Product"}]},
            rules=preset("json-ld"),
            representation="static",
        )["rules"]
    }
    assert ld["jsonld-types"]["value"] == ["Product"]
