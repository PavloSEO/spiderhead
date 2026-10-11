import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop.scans_urls import default_scan_index


def _scan(path, *, lifecycle="finished", crawl_partial=False, corpus_partial=False, size=100):
    return {
        "path": path,
        "lifecycle": lifecycle,
        "crawl_partial": crawl_partial,
        "corpus_partial": corpus_partial,
        "bytes": size,
    }


class DefaultScanTests(unittest.TestCase):
    def test_largest_complete_scan_is_the_default_over_a_newer_partial_one(self):
        rows = [
            _scan("partial-100-urls", crawl_partial=True, size=900),
            _scan("full-audit", size=500),
            _scan("small-complete", size=100),
        ]
        self.assertEqual(default_scan_index(rows), 1)

    def test_equal_complete_scans_prefer_the_newest_listed(self):
        rows = [_scan("newest", size=300), _scan("older-same-size", size=300)]
        self.assertEqual(default_scan_index(rows), 0)

    def test_interrupted_or_corpus_partial_scans_are_not_complete(self):
        rows = [
            _scan("stopped", lifecycle="interrupted", size=900),
            _scan("corpus", corpus_partial=True, size=800),
            _scan("full", size=10),
        ]
        self.assertEqual(default_scan_index(rows), 2)

    def test_without_a_complete_scan_the_newest_row_is_used(self):
        self.assertEqual(
            default_scan_index([_scan("a", crawl_partial=True), _scan("b", crawl_partial=True)]), 0
        )
        self.assertEqual(default_scan_index([]), 0)


if __name__ == "__main__":
    unittest.main()
