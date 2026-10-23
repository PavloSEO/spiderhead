"""Network-independent tests for the Schema.org validator.

The bundled 172 KB vocabulary keeps validation fully offline. Fixtures use
``check_schema(html=...)`` directly, so these tests never make HTTP requests.
"""

from seohead.checks import schema_org as schema

# ── JSON-LD block parsing ───────────────────────────────────────────────────


def _wrap(body: str) -> str:
    return f"<html><head>{body}</head><body></body></html>"


def _ld(json_str: str) -> str:
    return _wrap(f'<script type="application/ld+json">{json_str}</script>')


def test_no_jsonld_reports_finding_and_detects_microdata():
    html = _wrap('<div itemscope><span itemprop="x">y</span></div>')
    r = schema.check_schema(html=html)
    assert r["ok"] is True
    assert r["blocks"] == 0
    assert any("JSON-LD" in f for f in r["findings"])
    assert r["other_markup"]["microdata"] is True
    assert r["other_markup"]["rdfa"] is False


def test_broken_jsonld_goes_to_parse_errors_not_crash():
    html = _ld('{"@type": "Article", "headline": }')  # Malformed JSON is intentional.
    r = schema.check_schema(html=html)
    assert r["parse_errors"], "The malformed block must be reported in parse_errors"
    assert len(r["parse_errors"]) == 1
    assert "json-ld block #1" in r["parse_errors"][0].lower()


# ── Layer 1: vocabulary and inheritance ─────────────────────────────────────


def test_inheritance_author_on_article_is_not_error():
    # author is declared on CreativeWork; Article -> CreativeWork -> Thing.
    # A validator that ignores inheritance would reject author as unrelated.
    html = _ld(
        '{"@context":"https://schema.org","@type":"Article",'
        '"headline":"T","author":{"@type":"Person","name":"Joe"}}'
    )
    r = schema.check_schema(html=html)
    assert r["entities"], "The entity must be recognized"
    errs = [e for e in r["entities"][0]["errors"] if "author" in e]
    assert not errs, f"author must be valid on Article: {errs}"


def test_unknown_type_is_error():
    html = _ld('{"@type":"BogusType","name":"x"}')
    r = schema.check_schema(html=html)
    assert r["entities"][0]["errors"] == [
        "Type BogusType is not present in the Schema.org vocabulary"
    ]


def test_unknown_property_is_error():
    html = _ld('{"@type":"Article","totallyMadeUp":"x","headline":"t"}')
    r = schema.check_schema(html=html)
    assert r["entities"][0]["errors"] == [
        "Property totallyMadeUp is not present in the Schema.org vocabulary"
    ]


def test_deprecated_type_is_warning_not_silence():
    # Code is superseded by SoftwareSourceCode in the bundled 2026-08-12 vocabulary.
    html = _ld('{"@type":"Code","name":"x"}')
    r = schema.check_schema(html=html)
    warns = r["entities"][0]["warnings"]
    assert any("Code" in w and "SoftwareSourceCode" in w for w in warns)


def test_pending_layer_is_warning():
    html = _ld('{"@type":"3DModel","name":"x"}')
    r = schema.check_schema(html=html)
    assert any("pending" in w for w in r["entities"][0]["warnings"])


def test_property_not_allowed_on_type():
    # servesCuisine is declared on FoodEstablishment, outside Article's hierarchy.
    html = _ld('{"@type":"Article","headline":"t","servesCuisine":"pizza"}')
    r = schema.check_schema(html=html)
    assert any("servesCuisine" in e and "Article" in e for e in r["entities"][0]["errors"])


# ── Graph integrity: dangling @id references and islands ────────────────────


def test_dangling_id_reference_is_error():
    html = _ld('{"@type":"Article","headline":"t","author":{"@id":"#joe"}}')
    r = schema.check_schema(html=html)
    flat = [e for ent in r["entities"] for e in ent["errors"]]
    assert any("#joe" in e and "author" in e for e in flat)


def test_two_unlinked_nodes_reported_as_islands():
    # Neither node references the other through a pure {@id}, so no graph exists.
    html = _ld(
        '[{"@type":"Organization","@id":"#org","name":"A"},'
        '{"@type":"WebPage","@id":"#page","name":"P"}]'
    )
    r = schema.check_schema(html=html)
    g = r["graph"]
    assert g["nodes"] == 2
    assert not g["is_graph"], "Cross-node @id references are required for a linked graph"
    assert any("2" in f and "@id" in f for f in r["findings"])


def test_linked_nodes_form_graph():
    html = _ld(
        '[{"@type":"Organization","@id":"#org","name":"A"},'
        '{"@type":"WebPage","@id":"#page","name":"P",'
        '"publisher":{"@id":"#org"}}]'
    )
    r = schema.check_schema(html=html)
    g = r["graph"]
    assert g["is_graph"] is True
    assert g["linked_by_id"] >= 1


# ── Layer 2: rich-result eligibility ────────────────────────────────────────


def test_product_missing_required_name_not_eligible():
    html = _ld('{"@type":"Product","description":"x"}')  # name is intentionally absent.
    r = schema.check_schema(html=html)
    rr = next(x for x in r["rich_results"] if x["type"] == "Product")
    assert rr["eligible"] is False
    assert "name" in rr["missing_required"]


def test_rich_result_verdicts_carry_rules_version():
    html = _ld('{"@type":"Product","description":"x"}')
    r = schema.check_schema(html=html)
    assert r["rules_version"] == schema.RICH_RESULTS_RULES_VERSION
    rr = next(x for x in r["rich_results"] if x["type"] == "Product")
    assert rr["rules_version"] == schema.RICH_RESULTS_RULES_VERSION


def test_faqpage_marked_deprecated_for_rich():
    html = _ld(
        '{"@type":"FAQPage","mainEntity":[{"@type":"Question","name":"q",'
        '"acceptedAnswer":{"@type":"Answer","text":"a"}}]}'
    )
    r = schema.check_schema(html=html)
    rr = next(x for x in r["rich_results"] if x["type"] == "FAQPage")
    assert rr.get("note"), "FAQPage must include its deprecated_for_rich note"


# Issue #253: the flat table required Article headline (Google has no required
# Article property, only recommended ones) and scored Offer's own required
# fields as if Offer were an independent Google feature.


def test_article_without_headline_is_eligible():
    html = _ld('{"@type":"Article","author":{"@type":"Person","name":"Ada"}}')
    r = schema.check_schema(html=html)
    rr = next(x for x in r["rich_results"] if x["type"] == "Article")
    assert rr["eligible"] is True, "Google documents no required Article property"
    assert "headline" in rr["missing_recommended"]


def test_offer_is_not_scored_as_its_own_rich_result():
    html = _ld('{"@type":"Product","name":"Widget","offers":{"@type":"Offer","price":19.99}}')
    r = schema.check_schema(html=html)
    assert not any(x["type"] == "Offer" for x in r["rich_results"]), (
        "Offer has no independent Google rich result; it is a condition Product checks for"
    )


def test_product_name_only_is_not_eligible_without_review_rating_or_offers():
    # Product snippets need name plus at least one of review, aggregateRating, or offers.
    html = _ld('{"@type":"Product","name":"Widget"}')
    r = schema.check_schema(html=html)
    rr = next(x for x in r["rich_results"] if x["type"] == "Product")
    assert rr["eligible"] is False
    assert rr["missing_required_any_of"] == ["review", "aggregateRating", "offers"]


def test_product_with_name_and_offers_is_eligible():
    html = _ld(
        '{"@type":"Product","name":"Widget",'
        '"offers":{"@type":"Offer","price":19.99,"priceCurrency":"USD"}}'
    )
    r = schema.check_schema(html=html)
    rr = next(x for x in r["rich_results"] if x["type"] == "Product")
    assert rr["eligible"] is True
    assert rr["missing_required_any_of"] == []


# Issue #1413: the software-app feature requires offers (with price) plus a
# rating or a review, so offers alone must not look eligible.


def test_software_application_with_offers_but_no_rating_or_review_not_eligible():
    html = _ld(
        '{"@type":"SoftwareApplication","name":"App",'
        '"offers":{"@type":"Offer","price":0,"priceCurrency":"USD"}}'
    )
    r = schema.check_schema(html=html)
    rr = next(x for x in r["rich_results"] if x["type"] == "SoftwareApplication")
    assert rr["eligible"] is False
    assert rr["missing_required_any_of"] == ["aggregateRating", "review"]


def test_software_application_with_offers_and_rating_is_eligible():
    html = _ld(
        '{"@type":"SoftwareApplication","name":"App",'
        '"offers":{"@type":"Offer","price":0,"priceCurrency":"USD"},'
        '"aggregateRating":{"@type":"AggregateRating","ratingValue":4.6,'
        '"ratingCount":9}}'
    )
    r = schema.check_schema(html=html)
    rr = next(x for x in r["rich_results"] if x["type"] == "SoftwareApplication")
    assert rr["eligible"] is True
    assert rr["missing_required_any_of"] == []


def test_software_application_without_offers_reports_missing_required():
    html = _ld(
        '{"@type":"SoftwareApplication","name":"App",'
        '"review":{"@type":"Review","author":{"@type":"Person","name":"Ada"}}}'
    )
    r = schema.check_schema(html=html)
    rr = next(x for x in r["rich_results"] if x["type"] == "SoftwareApplication")
    assert rr["eligible"] is False
    assert rr["missing_required"] == ["offers"]


def test_eligible_rich_result_carries_a_no_guarantee_disclaimer():
    # Issue #253/#255: a structural "eligible" verdict must never read as a promise
    # that Google will actually display the rich result.
    html = _ld(
        '{"@type":"Product","name":"Widget",'
        '"offers":{"@type":"Offer","price":19.99,"priceCurrency":"USD"}}'
    )
    r = schema.check_schema(html=html)
    assert any("not a guarantee" in f for f in r["findings"])


# Issue #254: a nested Offer inside a Product is connected by containment, so it
# must not be reported as a disconnected graph island just for lacking its own @id.


def test_nested_anonymous_offer_is_not_a_graph_island():
    html = _ld(
        '{"@context":"https://schema.org","@type":"Product","name":"Widget",'
        '"offers":{"@type":"Offer","price":19.99,"priceCurrency":"USD"}}'
    )
    r = schema.check_schema(html=html)
    g = r["graph"]
    assert g["nodes"] == 2
    assert g["islands"] == [], f"A nested Offer is not a disconnected island: {g['islands']}"
    assert not any("isolated blocks" in f or "island" in f.lower() for f in r["findings"])


# Issue #326: a typed inline node-object that reuses a top-level @id is
# connected by identity exactly like a pure {"@id": ...} reference, since
# JSON-LD merges both forms by identifier. The prior direction-only check
# (only a bare {"@id"} counted as an edge, and only inbound references were
# tested) reported both this valid relationship and the ordinary pure-@id
# case as islands.


def test_typed_inline_node_reusing_a_top_level_id_is_not_an_island():
    html = _ld(
        '[{"@type":"WebPage","@id":"#page",'
        '"publisher":{"@id":"#org","@type":"Organization","name":"Acme"}},'
        '{"@type":"Organization","@id":"#org","name":"Acme"}]'
    )
    r = schema.check_schema(html=html)
    g = r["graph"]
    assert g["is_graph"] is True
    assert g["islands"] == []
    assert not any("isolated blocks" in f or "island" in f.lower() for f in r["findings"])


def test_pure_id_reference_root_is_not_an_island():
    # The referencing root (#page) must not be penalized for lacking an
    # *inbound* reference: it is connected because it references #org.
    html = _ld(
        '[{"@type":"WebPage","@id":"#page","publisher":{"@id":"#org"}},'
        '{"@type":"Organization","@id":"#org","name":"Acme"}]'
    )
    r = schema.check_schema(html=html)
    g = r["graph"]
    assert g["is_graph"] is True
    assert g["islands"] == []


def test_disconnected_typed_nodes_are_still_reported_as_islands():
    # Control: two typed top-level entities with no @id relationship at all
    # must still be flagged, so the fix must not blanket-suppress islands.
    html = _ld(
        '[{"@type":"WebPage","@id":"#page","name":"P"},'
        '{"@type":"Organization","@id":"#org","name":"Acme"}]'
    )
    r = schema.check_schema(html=html)
    g = r["graph"]
    assert g["is_graph"] is False
    assert set(g["islands"]) == {"#page", "#org"}
    assert any("isolated blocks" in f for f in r["findings"])


# ── Findings summary ────────────────────────────────────────────────────────


def test_findings_count_errors_and_warnings():
    html = _ld('{"@type":"Article","headline":"t","bogusProp":"x"}')
    r = schema.check_schema(html=html)
    error_count = sum(len(entity["errors"]) for entity in r["entities"])
    assert error_count == 1
    assert any(str(error_count) in f for f in r["findings"])


# ── @context and vocabulary resolution (section 4) ──────────────────────────


def test_schema_org_context_validates_normally():
    # A https://schema.org context follows normal validation: author remains
    # valid on Article because it is inherited from CreativeWork.
    html = _ld(
        '{"@context":"https://schema.org","@type":"Article",'
        '"headline":"T","author":{"@type":"Person","name":"Joe"}}'
    )
    r = schema.check_schema(html=html)
    assert r["entities"], "The entity must be recognized"
    errs = [e for e in r["entities"][0]["errors"] if "author" in e]
    assert not errs, f"author must be valid on Article: {errs}"
    # vocabularies must report one supported Schema.org block.
    vocabs = r["vocabularies"]
    assert any(
        v["context"] == "https://schema.org" and v["supported"] and v["blocks"] == 1 for v in vocabs
    ), f"Expected a supported Schema.org vocabulary: {vocabs}"
    assert all(v["supported"] for v in vocabs), (
        f"Every vocabulary in this fixture must be supported: {vocabs}"
    )


def test_unsupported_vocab_skips_schema_validation():
    # Two blocks declare different vocabularies: a valid Schema.org Article and
    # an external example.org/x Foo. Foo must be marked as unsupported rather
    # than producing a false Schema.org unknown-type error.
    body = (
        '<script type="application/ld+json">'
        '{"@context":"https://schema.org","@type":"Article","headline":"T"}'
        "</script>"
        '<script type="application/ld+json">'
        '{"@context":"https://example.org/x","@type":"Foo","name":"bar"}'
        "</script>"
    )
    r = schema.check_schema(html=_wrap(body))
    assert r["blocks"] == 2

    foo = next(e for e in r["entities"] if "Foo" in e["types"])
    # Schema.org validation is skipped, so Foo must not produce an unknown-type error.
    assert foo["errors"] == [], f"No errors are expected for the skipped node: {foo['errors']}"
    # The node must instead carry an explicit unsupported-vocabulary warning.
    assert foo["warnings"] == [
        "Node uses unsupported vocabulary https://example.org/x; Schema.org validation was skipped"
    ], f"Expected an unsupported-vocabulary warning: {foo['warnings']}"

    # The Schema.org Article must still validate independently.
    article = next(e for e in r["entities"] if "Article" in e["types"])
    assert article["errors"] == [], (
        f"The Schema.org block must remain unaffected: {article['errors']}"
    )

    # vocabularies must expose both contexts with the correct support flags.
    by_ctx = {v["context"]: v for v in r["vocabularies"]}
    # Indexed rather than tested with "in": a missing key raises here, which
    # says which context is absent instead of only that one of them is.
    assert by_ctx["https://schema.org"]["supported"] is True
    assert by_ctx["https://example.org/x"]["supported"] is False
    assert by_ctx["https://example.org/x"]["blocks"] == 1
