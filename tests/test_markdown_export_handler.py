"""Tests for the markdown-export handler: URL collection, file naming and frontmatter.

The content extraction itself belongs to ``seohead.checks.markdown_extract`` and is
tested there; what these tests pin down is the export's own contract — one input
source at a time, path-derived file names (home -> ``index.md``), a frontmatter
header above the body, and failures recorded instead of aborting the run.
"""

from __future__ import annotations

import re

import pytest

from seohead.mcp import handlers


def _fetched(html: str, url: str, status: int = 200) -> dict:
    return {"ok": True, "html": html, "final_url": url, "status_code": status}


def _serve(monkeypatch, pages: dict[str, dict]) -> None:
    def fake_fetch(url: str) -> dict:
        return pages.get(url, {"ok": False, "url": url, "error": "not found"})

    monkeypatch.setattr(handlers, "_require_fetched_html", fake_fetch)


PAGE = (
    "<html><head><title>Canonical URL</title>"
    '<meta name="description" content="What rel canonical is"></head>'
    "<body><main><h1>Canonical URL</h1><p>What it decides.</p></main></body></html>"
)


def test_exactly_one_input_source_is_required():
    with pytest.raises(ValueError, match="exactly one"):
        handlers.markdown_export()
    with pytest.raises(ValueError, match="exactly one"):
        handlers.markdown_export(url="https://x.test/", sitemap="https://x.test/sitemap.xml")


def test_url_paths_become_files_with_frontmatter(monkeypatch, tmp_path):
    _serve(
        monkeypatch,
        {
            "https://x.test/": _fetched(PAGE, "https://x.test/"),
        },
    )
    result = handlers.markdown_export(url="https://x.test/", out_dir=str(tmp_path), delay=0)
    assert result["written"] == 1
    assert result["failed"] == 0
    body = (tmp_path / "index.md").read_text(encoding="utf-8")
    assert body.startswith("---\n")
    assert 'title: "Canonical URL"' in body
    assert "url: https://x.test/" in body
    assert "Canonical URL" in body


def test_urls_file_drives_the_batch_and_failures_are_kept(monkeypatch, tmp_path):
    _serve(
        monkeypatch,
        {
            "https://x.test/": _fetched(PAGE, "https://x.test/"),
            "https://x.test/blog/canonical-url": _fetched(
                PAGE, "https://x.test/blog/canonical-url"
            ),
            "https://x.test/missing": {"ok": False, "error": "boom"},
        },
    )
    urls = tmp_path / "urls.txt"
    urls.write_text(
        "https://x.test/\n"
        "# a comment line\n"
        "https://x.test/blog/canonical-url\n"
        "https://x.test/missing\n",
        encoding="utf-8",
    )
    result = handlers.markdown_export(urls_file=str(urls), out_dir=str(tmp_path / "out"), delay=0)
    assert (result["requested"], result["written"], result["failed"]) == (3, 2, 1)
    assert (tmp_path / "out" / "index.md").is_file()
    assert (tmp_path / "out" / "blog" / "canonical-url.md").is_file()
    assert result["failures"][0]["url"] == "https://x.test/missing"
    assert result["ok"] is True


def test_icon_ligature_lines_are_dropped(monkeypatch, tmp_path):
    page = PAGE.replace("<p>What it decides.</p>", "<p>zoom_in</p><p>What it decides.</p>")
    _serve(monkeypatch, {"https://x.test/a": _fetched(page, "https://x.test/a")})
    result = handlers.markdown_export(url="https://x.test/a", out_dir=str(tmp_path), delay=0)
    body = (tmp_path / "a.md").read_text(encoding="utf-8")
    assert result["written"] == 1
    assert not re.search(r"^zoom_in$", body, re.M)
    assert "What it decides." in body
