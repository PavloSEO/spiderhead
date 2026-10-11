"""Import-compatible redirect output and local chain analysis."""

import csv
import io

from seohead.checks.redirects import analyze_redirect_map, generate_rule


def test_wordpress_redirection_csv_row_has_import_fields():
    row = generate_rule("https://example.test/old", "/new", "wordpress-redirection")
    assert next(csv.reader(io.StringIO(row))) == ["/old", "/new", "0", "301", "url"]


def test_redirect_map_reports_chains_and_loops_without_fetching():
    analysis = analyze_redirect_map(
        [
            {"from": "/old", "to": "/middle"},
            {"from": "/middle", "to": "/new"},
            {"from": "/loop-a", "to": "/loop-b"},
            {"from": "/loop-b", "to": "/loop-a"},
        ]
    )
    assert {tuple(item["path"]) for item in analysis["chains"]} == {
        ("/old", "/middle", "/new")
    }
    assert analysis["loops"] == [
        {"source": "/loop-a", "path": ["/loop-a", "/loop-b", "/loop-a"]},
        {"source": "/loop-b", "path": ["/loop-b", "/loop-a", "/loop-b"]},
    ]
