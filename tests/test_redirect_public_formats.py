"""Import-compatible redirect output and local chain analysis."""

import csv
import io

from seohead.checks.redirects import analyze_redirect_map, generate_rule, normalize_url


def test_absolute_root_url_keeps_query_and_fragment_when_host_is_removed():
    assert normalize_url("https://example.test?source=old#section") == "/?source=old#section"
    assert normalize_url("https://example.test/path?source=old#section") == "/path?source=old#section"


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
