"""Desktop log commands preserve bounded and explicit core boundaries."""

import unittest

from seohead_desktop.screens.logs import (
    PAGE_SIZE,
    bot_arguments,
    import_arguments,
    join_arguments,
    summary_arguments,
)


class LogArgumentTests(unittest.TestCase):
    def test_import_uses_one_chosen_path_and_explicit_budgets(self):
        self.assertEqual(
            import_arguments("/tmp/project with spaces", "/tmp/access log.gz"),
            [
                "logs-import",
                "--project",
                "/tmp/project with spaces",
                "--path",
                "/tmp/access log.gz",
                "--max-lines",
                "10000000",
                "--max-files",
                "1",
            ],
        )

    def test_read_pages_are_bounded_and_dns_requires_an_explicit_flag(self):
        self.assertEqual(
            summary_arguments("/tmp/project/logs/a.sqlite", "bot_ips", PAGE_SIZE),
            [
                "logs-summary",
                "--source",
                "/tmp/project/logs/a.sqlite",
                "--view",
                "bot_ips",
                "--limit",
                "100",
                "--offset",
                "100",
            ],
        )
        self.assertNotIn("--verify-bots", bot_arguments("artifact"))
        self.assertIn("--verify-bots", bot_arguments("artifact", verify=True))

    def test_scan_join_requires_a_scan_and_known_view(self):
        self.assertEqual(
            join_arguments("logs.sqlite", "scan.sqlite", "status_waste"),
            [
                "logs-join",
                "--source",
                "logs.sqlite",
                "--scan",
                "scan.sqlite",
                "--view",
                "status_waste",
                "--limit",
                "100",
                "--offset",
                "0",
            ],
        )
        with self.assertRaises(ValueError):
            join_arguments("logs.sqlite", "", "status_waste")


if __name__ == "__main__":
    unittest.main()
