# ruff: noqa: RUF001
"""Offline tests for the spellcheck core (issue #1003, slice 1). Fake backend only."""

from seohead.checks import spellcheck

_HTML = """<html><body>
<nav>Home Products Services About Contact Blog Careers Support Login Sign up</nav>
<main><p>Это тестовый абзац с опечаткой превосходнейшей фразой. Second sentence hass a typo.</p>
<p>Ещё одно предложение про насосы без ошибок.</p></main>
<footer>Copyright policy terms privacy sitemap careers investors press</footer>
</body></html>"""


class FakeBackend:
    name = "fake"

    def __init__(self, table=None, error=None):
        self.table = table or {}
        self.error = error
        self.calls = []

    def check(self, text, lang):
        self.calls.append((lang, text))
        if self.error:
            raise self.error
        out = []
        for word, suggestions in self.table.items():
            idx = text.find(word)
            if idx >= 0:
                out.append({"word": word, "offset": idx, "suggestions": suggestions})
        return out


def test_visible_text_excludes_nav_and_footer():
    text = spellcheck.visible_text(_HTML)
    assert "Products" not in text and "Copyright" not in text
    assert "Ещё одно предложение" in text


def test_split_and_route_ru_en_and_skip():
    assert spellcheck.route("Это достаточно длинный тестовый абзац для проверки.") == "ru"
    assert spellcheck.route("A reasonably long english sentence for routing checks.") == "en"
    assert spellcheck.route("12345 67890 !!!") == "skip"
    assert len(spellcheck.split_blocks("Одно. Два! Три?")) == 3


def test_no_backend_is_skipped_not_zero():
    result = spellcheck.run(_HTML, None)
    assert result["state"] == "skipped"
    assert result["backend"] is None
    assert result["findings_total"] == 0


def test_findings_routed_by_language_with_snippet_and_offset():
    backend = FakeBackend({"hass": ["has"], "превосходнейшей": ["превосходной"]})
    result = spellcheck.run(_HTML, backend)
    assert result["state"] == "complete"
    assert result["languages"]["ru"] >= 1 and result["languages"]["en"] >= 1
    words = {f["word"]: f for f in result["findings"]}
    assert words["hass"]["lang"] == "en" and words["hass"]["suggestions"] == ["has"]
    assert words["превосходнейшей"]["lang"] == "ru"
    assert "hass" in words["hass"]["snippet"] and len(words["hass"]["snippet"]) <= 100


def test_ignore_words_are_dropped_case_insensitively():
    backend = FakeBackend({"hass": ["has"], "превосходнейшей": []})
    result = spellcheck.run(_HTML, backend, ignore_words=["HASS"])
    assert [f["word"] for f in result["findings"]] == ["превосходнейшей"]
    assert result["findings_total"] == 1


def test_max_findings_caps_list_but_counts_all():
    backend = FakeBackend({"hass": [], "превосходнейшей": []})
    result = spellcheck.run(_HTML, backend, max_findings=1)
    assert len(result["findings"]) == 1
    assert result["findings_total"] == 2


def test_backend_error_is_partial_with_reason():
    result = spellcheck.run(_HTML, FakeBackend(error=RuntimeError("boom")))
    assert result["state"] == "partial"
    assert "RuntimeError" in result["reason"]


def test_only_unknown_script_text_is_skipped_not_zero():
    html = "<html><body><main><p>12345 67890 11111 22222 33333</p></main></body></html>"
    result = spellcheck.run(html, FakeBackend())
    assert result["state"] == "skipped"
    assert result["skipped_blocks"] >= 1


def test_visible_text_override_is_used_for_findings_and_snippet_context():
    backend = FakeBackend({"safe": []})
    result = spellcheck.run(
        "<html><body><main>private marker</main></body></html>",
        backend,
        visible_text_override="A safe visible sentence.",
    )
    assert backend.calls == [("en", "A safe visible sentence.")]
    assert result["state"] == "complete"
