"""The shop's D41 truth must describe annotations actually served by the fixture."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

from seohead.checks.parser import parse_html


@pytest.mark.parametrize("version,next_page", [("v1", 4), ("v2", 3), ("v3", 3)])
def test_shop_serves_pagination_annotations_and_versioned_sequence_defect(version, next_page):
    path = Path(__file__).parents[1] / "docs/examples/shop-site/shop_site.py"
    spec = importlib.util.spec_from_file_location("shop_pagination_fixture", path)
    assert spec is not None and spec.loader is not None
    shop = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = shop
    spec.loader.exec_module(shop)
    base = "https://example.test"
    site = shop.Site(version, base)
    source = "/catalog/stulya/?page=2"
    parsed = parse_html(site.get(source).body.decode("utf-8"), base + source)
    annotations = {row["rel"]: row["target"] for row in parsed["head_links"]}
    assert annotations["next"] == f"{base}/catalog/stulya/?page={next_page}"
    assert annotations["prev"] == f"{base}/catalog/stulya/"
    target = f"/catalog/stulya/?page={next_page}"
    parsed_target = parse_html(site.get(target).body.decode("utf-8"), base + target)
    annotations_target = {row["rel"]: row["target"] for row in parsed_target["head_links"]}
    assert annotations_target["prev"] == f"{base}/catalog/stulya/?page={next_page - 1}"
