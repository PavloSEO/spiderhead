"""Competitor intake and state helpers: parsing, refusals, URL cap and state derived from finished scans only."""

import unittest

from seohead_desktop.competitors import (
    candidate_arguments,
    competitor_rows,
    parse_candidates,
    scan_budget,
    valid_url_cap,
)


class ParseCandidatesTests(unittest.TestCase):
    def test_accepts_clipboard_lines_commas_and_bare_hosts_and_keeps_www(self):
        text = "https://one.example.test/\nwww.two.example.test/shop, three.example.test;\t https://four.example.test"

        parsed = parse_candidates(text)

        self.assertEqual(
            parsed["accepted"],
            [
                "https://one.example.test/",
                "https://www.two.example.test/shop",
                "https://three.example.test",
                "https://four.example.test",
            ],
        )
        self.assertEqual(parsed["refused"], [])

    def test_refuses_unsafe_and_duplicate_entries_with_reasons(self):
        text = "ftp://files.example.test ok.example.test https://user:pw@x.example.test/ localhost https://ok.example.test"

        parsed = parse_candidates(text)

        self.assertEqual(parsed["accepted"], ["https://ok.example.test"])
        reasons = dict(parsed["refused"])
        self.assertEqual(reasons["ftp://files.example.test"], "only http and https URLs can be scanned")
        self.assertEqual(reasons["https://user:pw@x.example.test/"], "credentials and fragments are not accepted")
        self.assertEqual(reasons["localhost"], "the host name is missing or incomplete")

    def test_existing_urls_and_project_limit_are_refused_before_the_core_call(self):
        parsed = parse_candidates(
            "a.example.test b.example.test c.example.test",
            existing=["https://a.example.test"],
            limit=2,
        )

        self.assertEqual(parsed["accepted"], ["https://b.example.test"])
        self.assertEqual(parsed["refused"][0], ("https://a.example.test", "already in the list"))
        self.assertEqual(parsed["refused"][1][1], "the project limit is 2 competitors")

    def test_same_site_with_a_trailing_slash_is_a_duplicate(self):
        parsed = parse_candidates("https://one.example.test", existing=["https://one.example.test/"])

        self.assertEqual(parsed["accepted"], [])
        self.assertEqual(parsed["refused"][0][1], "already in the list")

    def test_www_form_of_a_listed_site_is_the_same_site(self):
        parsed = parse_candidates("www.one.example.test", existing=["https://one.example.test/"])

        self.assertEqual(parsed["accepted"], [])
        self.assertEqual(parsed["refused"][0][1], "already in the list")

    def test_non_text_and_oversized_input_are_refused_whole(self):
        self.assertEqual(parse_candidates(None)["accepted"], [])
        self.assertEqual(parse_candidates("a.example.test " * 20000)["accepted"], [])


class CandidateArgumentsTests(unittest.TestCase):
    def test_arguments_carry_provenance_per_candidate(self):
        args = candidate_arguments(
            "/p",
            ["https://one.example.test"],
            source="desktop clipboard paste",
            observed_at="2026-10-11T00:00:00+00:00",
            consumer="desktop/gui",
        )

        self.assertEqual(
            args,
            {
                "directory": "/p",
                "competitors": [
                    {
                        "url": "https://one.example.test",
                        "source": "desktop clipboard paste",
                        "observed_at": "2026-10-11T00:00:00+00:00",
                    }
                ],
                "consumer": "desktop/gui",
            },
        )


class UrlCapTests(unittest.TestCase):
    def test_cap_must_be_a_bounded_integer(self):
        self.assertEqual(valid_url_cap(1), 1)
        self.assertEqual(valid_url_cap(1000), 1000)
        for value in (0, 1001, -3, 2.5, True, "50", None):
            self.assertIsNone(valid_url_cap(value), value)

    def test_scan_budget_is_explicit_and_inside_the_core_default_thresholds(self):
        # Core defaults: requests 3000, seconds 600. Zero is unbounded and would be refused without approval.
        self.assertEqual(scan_budget(50), {"limits.max_requests": 150, "limits.max_crawl_seconds": 600})
        for cap in (1, 50, 1000):
            budget = scan_budget(cap)
            self.assertGreater(budget["limits.max_requests"], 0)
            self.assertLessEqual(budget["limits.max_requests"], 3000)
            self.assertLessEqual(budget["limits.max_crawl_seconds"], 600)


def _site(url, directory, *scans, state="candidate; audit not run"):
    return {
        "role": "competitor",
        "site": {"target": url},
        "directory": directory,
        "project_uuid": "uuid-" + directory,
        "candidate": {"state": state, "source": "desktop", "observed_at": None},
        "scans": {"items": list(scans)},
    }


class CompetitorRowsTests(unittest.TestCase):
    def test_unscanned_competitor_keeps_the_candidate_note(self):
        rows = competitor_rows([{"role": "primary", "site": {"target": "https://own.example.test/"}}, _site("https://one.example.test/", "competitors/a")])

        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["state"], "candidate; audit not run")
        self.assertEqual(rows[0]["host"], "one.example.test")
        self.assertEqual(rows[0]["scan_count"], 0)

    def test_finished_scan_changes_state_and_partial_is_said(self):
        scan = {"lifecycle": "finished", "crawl_partial": True, "finished_at": "2026-10-11T10:00:00Z"}
        rows = competitor_rows([_site("https://one.example.test/", "competitors/a", scan)])

        self.assertEqual(rows[0]["state"], "scanned, partial")
        self.assertEqual(rows[0]["scan_count"], 1)
        self.assertEqual(rows[0]["latest_finished_at"], "2026-10-11T10:00:00Z")

    def test_running_or_interrupted_scans_are_not_reported_as_audited(self):
        rows = competitor_rows(
            [
                _site("https://one.example.test/", "competitors/a", {"lifecycle": "running"}),
                _site("https://two.example.test/", "competitors/b", {"lifecycle": "interrupted", "finished_at": None}),
            ]
        )

        self.assertEqual(rows[0]["state"], "scan running")
        self.assertEqual(rows[1]["state"], "candidate; audit not run")
        self.assertEqual(rows[1]["scan_count"], 0)

    def test_malformed_snapshot_is_ignored_not_guessed(self):
        self.assertEqual(competitor_rows(None), [])
        self.assertEqual(competitor_rows([None, "x"]), [])


if __name__ == "__main__":
    unittest.main()
