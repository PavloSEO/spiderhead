"""Fast self-checks for the shop test site: determinism and ground-truth consistency."""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("shop_site", HERE / "shop_site.py")
shop = importlib.util.module_from_spec(_spec)
sys.modules["shop_site"] = shop
_spec.loader.exec_module(shop)


class ShopSiteTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.truth = shop.ground_truth()

    def test_generator_is_deterministic(self):
        for v in shop.VERSIONS:
            a, b = shop.Site(v), shop.Site(v)
            self.assertEqual(
                {k: r.body for k, r in a.routes.items()}, {k: r.body for k, r in b.routes.items()}
            )

    def test_committed_ground_truth_is_current(self):
        committed = json.loads((HERE / "ground_truth.json").read_text())
        self.assertEqual(committed, json.loads(json.dumps(self.truth, ensure_ascii=False)))

    def test_site_grows_between_versions(self):
        sizes = [self.truth["versions"][v]["reachable_urls"] for v in shop.VERSIONS]
        self.assertLess(sizes[0], sizes[1])
        self.assertLess(sizes[1], sizes[2])

    def test_defect_urls_match_planted_state(self):
        for d in shop.DEFECTS:
            for v in shop.VERSIONS:
                urls = self.truth["versions"][v]["urls"]
                for u in d.urls:
                    if d.active(v):
                        self.assertIn(d.id, urls[u]["defects"], (d.id, v, u))
                    elif u in urls:
                        self.assertNotIn(d.id, urls[u]["defects"], (d.id, v, u))

    def test_lifecycle_has_fixes_and_regressions(self):
        states = [s for d in self.truth["defects"].values() for s in d["states"].values()]
        self.assertIn("fixed", states)
        self.assertIn("regressed", states)
        v1 = set(self.truth["versions"]["v1"]["active_defects"])
        fixed_in_v2 = v1 - set(self.truth["versions"]["v2"]["active_defects"])
        self.assertGreaterEqual(len(fixed_in_v2) / len(v1), 0.5)

    def test_expected_statuses(self):
        u1 = self.truth["versions"]["v1"]["urls"]
        self.assertEqual(u1["/aktsii/"]["status"], 500)
        self.assertEqual(u1["/catalog/stoly/stol-loft-staryy/"]["status"], 404)
        self.assertEqual(
            self.truth["versions"]["v2"]["urls"]["/catalog/stoly/stol-007/"]["status"], 404
        )
        self.assertEqual(
            self.truth["versions"]["v3"]["urls"]["/catalog/stoly/stol-007/"]["status"], 301
        )

    def test_additive_image_defects_do_not_change_frontier(self):
        self.assertEqual(
            [self.truth["versions"][v]["expected_urls_crawled"] for v in shop.VERSIONS],
            [1248, 1314, 1330],
        )
        for v in shop.VERSIONS:
            site = shop.Site(v)
            informative = site.get("/catalog/spalni/spalnya-009/").body.decode()
            dimensions = site.get("/catalog/spalni/spalnya-010/").body.decode()
            self.assertEqual("alt='' role='img'" in informative, v in ("v1", "v2"))
            self.assertEqual("width='800' height='600'" in dimensions, v == "v3")
        self.assertIn("IMG_BROKEN", self.truth["defects"]["D47"]["checks"])


if __name__ == "__main__":
    unittest.main()
