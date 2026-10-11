"""Offline Microdata/RDFa extraction and validation regression cases."""

from pathlib import Path

import pytest

from seohead.checks import schema_org as schema


def test_microdata_nested_scope_is_a_property_not_a_leaked_parent_property():
    result = schema.check_schema(
        html="""
        <div itemscope itemtype="https://schema.org/Product">
          <span itemprop="name">Widget</span>
          <div itemprop="offers" itemscope itemtype="https://schema.org/Offer">
            <meta itemprop="price" content="19.99">
            <meta itemprop="priceCurrency" content="USD">
          </div>
        </div>"""
    )
    assert result["blocks"] == 0
    assert result["markup_counts"] == {"microdata": 2, "rdfa": 0}
    assert [(e["types"], e["format"]) for e in result["entities"]] == [
        (["Product"], "microdata"),
        (["Offer"], "microdata"),
    ]
    assert not any(e["errors"] for e in result["entities"])
    assert result["graph"]["top_level"] == 1
    assert result["rich_results"][0]["eligible"] is True
    assert result["rich_results"][0]["rules_version"] == schema.RICH_RESULTS_RULES_VERSION
    assert result["rich_results"][0]["format"] == "microdata"


def test_microdata_itemref_multi_property_and_repeated_values():
    soup = schema.BeautifulSoup(
        """
        <div itemscope itemtype="https://schema.org/Person" itemref="elsewhere elsewhere">
          <span itemprop="name alternateName">Ada</span>
          <span itemprop="alternateName">A</span>
        </div>
        <div id="elsewhere"><meta itemprop="description" content="Engineer"></div>""",
        "lxml",
    )
    nodes, count = schema._extract_microdata(soup, "")
    assert count == 1
    assert nodes[0]["name"] == "Ada"
    assert nodes[0]["alternateName"] == ["Ada", "A"]
    assert nodes[0]["description"] == "Engineer"


def test_microdata_missing_itemref_and_wrong_type_are_actionable():
    result = schema.check_schema(
        html="""
        <div itemscope itemtype="https://schema.org/Article" itemref="missing">
          <span itemprop="servesCuisine">Pizza</span><span itemprop="bogus">X</span>
        </div>"""
    )
    errors = result["entities"][0]["errors"]
    assert any("itemref target missing" in e for e in errors)
    assert any("servesCuisine" in e and "Article" in e for e in errors)
    assert any("Property bogus" in e for e in errors)
    assert any("Vocabulary errors: 3" in f for f in result["findings"])


@pytest.mark.parametrize(
    "markup",
    [
        '<div itemscope itemtype="https://example.org/Product"><span itemprop="name">X</span></div>',
        '<div vocab="https://example.org/" typeof="Product"><span property="name">X</span></div>',
        '<script type="application/ld+json">{"@context":"https://example.org/","@type":"Product","name":"X"}</script>',
    ],
)
def test_foreign_vocabulary_is_not_scored_as_a_schema_rich_result(markup):
    result = schema.check_schema(html=markup)
    assert result["rich_results"] == []
    assert result["entities"][0]["warnings"]
    assert result["entities"][0]["errors"] == []
    assert any(v["supported"] is False for v in result["vocabularies"])


def test_microdata_values_use_html_attributes_and_document_base():
    soup = schema.BeautifulSoup(
        """
        <div itemscope itemtype="https://schema.org/Article" itemid="#article">
          <time itemprop="datePublished" datetime="2026-10-11">Today</time>
          <a itemprop="url" href="story">Read</a><img itemprop="image" src="photo.jpg">
          <data itemprop="wordCount" value="300">Three hundred</data>
        </div>""",
        "lxml",
    )
    nodes, _ = schema._extract_microdata(soup, "https://example.test/news/")
    node = nodes[0]
    assert node["@id"] == "https://example.test/news/#article"
    assert node["datePublished"] == "2026-10-11"
    assert node["url"] == "https://example.test/news/story"
    assert node["image"] == "https://example.test/news/photo.jpg"
    assert node["wordCount"] == "300"


@pytest.mark.parametrize(
    "context,type_name,prop",
    [
        ('vocab="https://schema.org/"', "Article", "headline"),
        ('vocab="https://schema.org"', "Article", "headline"),
        ('prefix="s: https://schema.org/"', "s:Article", "s:headline"),
        ('xmlns:s="https://schema.org/"', "s:Article", "s:headline"),
        ("", "https://schema.org/Article", "https://schema.org/headline"),
    ],
)
def test_rdfa_vocabulary_prefix_and_full_iri(context, type_name, prop):
    result = schema.check_schema(
        html=f'''
        <div {context} typeof="{type_name}" about="#article">
          <span property="{prop}" content="Title">Visible title</span>
        </div>'''
    )
    assert result["entities"][0]["types"] == ["Article"]
    assert result["entities"][0]["format"] == "rdfa"
    assert result["entities"][0]["errors"] == []
    assert result["markup_coverage"][1]["status"] == "checked"
    assert "headline" not in result["rich_results"][0]["missing_recommended"]


def test_rdfa_nested_entity_and_scoped_vocabulary():
    result = schema.check_schema(
        html="""
        <div vocab="https://schema.org/" typeof="Product">
          <span property="name">Widget</span>
          <div property="offers" typeof="Offer">
            <meta property="price" content="19.99"><meta property="priceCurrency" content="USD">
          </div>
        </div>"""
    )
    assert [e["types"] for e in result["entities"]] == [["Product"], ["Offer"]]
    assert not any(e["errors"] or e["warnings"] for e in result["entities"])
    assert result["graph"]["top_level"] == 1
    assert result["rich_results"][0]["eligible"] is True


def test_rdfa_relation_is_a_validated_graph_edge():
    result = schema.check_schema(
        html="""
        <div vocab="https://schema.org/" typeof="Product">
          <span property="name">Widget</span><a rel="offers" href="#offer">Offer</a>
          <div about="#offer" typeof="Offer">
            <meta property="price" content="19.99"><meta property="priceCurrency" content="USD">
          </div>
        </div>"""
    )
    assert result["markup_coverage"][1]["status"] == "checked"
    assert result["entities"][0]["errors"] == []
    assert result["graph"]["linked_by_id"] == 1


def test_rdfa_incomplete_relation_is_explicitly_partial():
    result = schema.check_schema(
        html='<div vocab="https://schema.org/" typeof="Person"><span rel="knows"></span></div>'
    )
    assert result["markup_coverage"][1]["status"] == "partial"
    assert any("incomplete rel/rev" in reason for reason in result["markup_coverage"][1]["reasons"])


def test_rdfa_orphan_schema_property_and_foreign_metadata_are_distinct():
    orphan = schema.check_schema(html='<span property="schema:name">X</span>')
    assert orphan["markup_coverage"][1]["status"] == "checked"
    assert any("Missing typeof" in e for e in orphan["entities"][0]["errors"])
    og = schema.check_schema(html='<meta property="og:title" content="Title">')
    assert og["markup_coverage"][1]["status"] == "absent"


def test_rdfa_subject_graph_lists_typed_literals_and_reverse_edges():
    markup = (Path(__file__).parent / "fixtures" / "rdfa_graph.html").read_text("utf-8")
    base = "https://example.test/article/"
    result = schema.check_schema(html=markup, document_base_url=base)
    assert result["markup_coverage"][1]["status"] == "checked"
    assert result["graph"]["linked_by_id"] >= 2
    person = next(
        e for e in result["entities"] if e["types"] == ["Person"] and e["id"].endswith("#ada")
    )
    assert person["errors"] == []
    assert person["warnings"] == []
    assert result["rules_version"] == schema.RICH_RESULTS_RULES_VERSION
    assert result["markup_counts"]["rdfa"] > 0
    graph, _, reasons = schema._extract_rdfa(schema.BeautifulSoup(markup, "lxml"), base)
    ada = next(node for node in graph if node.get("@id", "").endswith("#ada"))
    grace = next(node for node in graph if node.get("@id", "").endswith("#grace"))
    assert ada["additionalName"] == ["Queen", "of coding"]
    assert ada["birthDate"]["@value"] == "1990-01-01"
    assert ada["birthDate"]["@type"].endswith("#date")
    assert ada["knows"] == {"@id": "https://example.test/article/#grace"}
    assert ada["parent"] == {"@id": "https://example.test/article/#grace"}
    assert reasons == []


def test_schema_validation_marks_oversized_html_as_partial():
    result = schema.check_schema(html=" " * (schema.MAX_HTML_BYTES + 1))
    assert all(item["status"] == "partial" for item in result["markup_coverage"])
    assert "byte schema validation limit" in result["findings"][0]


def test_inert_templates_and_text_do_not_generate_markup_entities():
    result = schema.check_schema(
        html="""
        <template><div itemscope itemtype="https://schema.org/Product"></div>
          <div vocab="https://schema.org/" typeof="Product"></div></template>
        <p>itemscope vocab="https://schema.org/" typeof="Product"</p>"""
    )
    assert result["entities"] == []
    assert result["other_markup"] == {"microdata": False, "rdfa": False}


def test_mixed_formats_keep_independent_paths_and_validation_results():
    result = schema.check_schema(
        html="""
        <script type="application/ld+json">{"@type":"Article","headline":"A"}</script>
        <div itemscope itemtype="https://schema.org/Product"><span itemprop="bogus">X</span></div>
        <div vocab="https://schema.org/" typeof="Event"><span property="name">E</span></div>"""
    )
    assert [e["format"] for e in result["entities"]] == ["jsonld", "microdata", "rdfa"]
    assert len({e["path"] for e in result["entities"]}) == 3
    assert result["blocks_found"] == result["blocks"] == 1
    assert result["entities"][0]["errors"] == []
    assert result["entities"][1]["errors"]
    event = next(r for r in result["rich_results"] if r["type"] == "Event")
    assert event["missing_required"] == ["startDate", "location"]


def test_cyclic_microdata_itemref_does_not_crash_or_disappear():
    result = schema.check_schema(
        html="""
        <div id="a" itemscope itemtype="https://schema.org/Person" itemprop="knows" itemref="b"></div>
        <div id="b" itemscope itemtype="https://schema.org/Person" itemprop="knows" itemref="a"></div>"""
    )
    assert result["entities"]
    assert any("Cyclic" in e for n in result["entities"] for e in n["errors"])


def test_reused_rdfa_subject_does_not_create_recursive_python_objects():
    result = schema.check_schema(
        html="""
        <div vocab="https://schema.org/" typeof="Person" about="#person">
          <span property="name">Ada</span>
          <div property="knows" typeof="Person" about="#person"></div>
        </div>"""
    )
    assert result["entities"][0]["types"] == ["Person"]
    assert result["ok"] is True
    assert result["graph"]["linked_by_id"] == 1


def test_rdfa_resource_reference_is_connected_to_its_typed_subject():
    result = schema.check_schema(
        html="""
        <div vocab="https://schema.org/">
          <div typeof="Article" about="#article"><a property="author" resource="#ada">Ada</a></div>
          <div typeof="Person" about="#ada"><span property="name">Ada</span></div>
        </div>"""
    )
    assert not any(e["errors"] or e["warnings"] for e in result["entities"])
    assert result["graph"]["linked_by_id"] == 1
    assert result["graph"]["islands"] == []


@pytest.mark.parametrize("format", ["microdata", "rdfa"])
def test_literal_number_and_date_ranges_are_validated(format):
    scope = (
        'itemscope itemtype="https://schema.org/Article"'
        if format == "microdata"
        else 'vocab="https://schema.org/" typeof="Article"'
    )
    prop = "itemprop" if format == "microdata" else "property"
    result = schema.check_schema(
        html=f"""
        <div {scope}><meta {prop}="wordCount" content="three hundred">
          <meta {prop}="datePublished" content="2026-99-99"></div>"""
    )
    warnings = result["entities"][0]["warnings"]
    assert any("wordCount" in w and "expected ranges" in w for w in warnings)
    assert any("datePublished" in w and "expected ranges" in w for w in warnings)


def test_blank_required_property_is_missing_for_rich_result():
    result = schema.check_schema(
        html="""
        <div itemscope itemtype="https://schema.org/Product">
          <meta itemprop="name" content="  ">
          <div itemprop="offers" itemscope itemtype="https://schema.org/Offer"></div>
        </div>"""
    )
    assert result["rich_results"][0]["eligible"] is False
    assert result["rich_results"][0]["missing_required"] == ["name"]


def test_document_base_resolves_microdata_identity():
    result = schema.check_schema(
        url="https://example.test/page",
        html="""
        <base href="/news/">
        <div itemscope itemtype="https://schema.org/Article" itemid="#article"></div>""",
    )
    assert result["entities"][0]["id"] == "https://example.test/news/#article"


def test_deep_rdfa_reports_validation_limit():
    html = '<div vocab="https://schema.org/" typeof="Person">'
    html += '<div property="knows" typeof="Person">' * 70
    result = schema.check_schema(html=html + "</div>" * 71)
    assert any("depth limit" in e for node in result["entities"] for e in node["errors"])


def test_untyped_microdata_names_the_html_attribute_to_fix():
    result = schema.check_schema(html='<div itemscope><span itemprop="name">X</span></div>')
    assert result["entities"][0]["errors"][0].startswith("Missing itemtype;")


def test_foreign_redeclaration_of_rdfa_subject_remains_unsupported():
    result = schema.check_schema(
        html="""
        <div vocab="https://schema.org/" typeof="Product" about="#item"><span property="name">X</span></div>
        <div vocab="https://example.org/" typeof="Product" about="#item"></div>"""
    )
    assert result["rich_results"] == []
    assert result["entities"][0]["warnings"]


@pytest.mark.parametrize("format", ["microdata", "rdfa"])
@pytest.mark.parametrize(
    "base_tag,expected_base",
    [
        ("", "https://example.test/page"),
        ('<base href="news/">', "https://example.test/news/"),
    ],
)
def test_relative_identity_links_jsonld_and_html_without_false_dangling_reference(
    format, base_tag, expected_base
):
    declaration = (
        'itemscope itemtype="https://schema.org/Person" itemid="#ada"'
        if format == "microdata"
        else 'vocab="https://schema.org/" typeof="Person" about="#ada"'
    )
    prop = "itemprop" if format == "microdata" else "property"
    html = (
        base_tag
        + """
        <script type="application/ld+json">
          {"@context":"https://schema.org","@type":"Article","@id":"#article",
           "author":{"@id":"#ada"}}
        </script>"""
    )
    html += f'<div {declaration}><span {prop}="name">Ada</span></div>'
    result = schema.check_schema(url="https://example.test/page", html=html)
    assert {e["id"] for e in result["entities"]} == {
        expected_base + "#article",
        expected_base + "#ada",
    }
    assert not any(e["errors"] for e in result["entities"])
    assert result["graph"]["islands"] == []
    assert result["graph"]["is_graph"] is True
    assert result["graph"]["linked_by_id"] == 1


def test_resolved_document_base_is_not_applied_twice():
    html = """<base href="news/">
        <script type="application/ld+json">{"@type":"Article","@id":"#article","author":{"@id":"#ada"}}</script>
        <div itemscope itemtype="https://schema.org/Person" itemid="#ada"></div>"""
    result = schema.check_schema(html=html, document_base_url="https://example.test/news/")
    assert {e["id"] for e in result["entities"]} == {
        "https://example.test/news/#article",
        "https://example.test/news/#ada",
    }
    assert not any(e["errors"] for e in result["entities"])


def test_identity_normalization_copies_source_and_preserves_foreign_context_and_blank_ids():
    payload = {
        "@context": "https://schema.org",
        "@type": "Article",
        "@id": "#article",
        "author": {"@id": "#ada"},
        "publisher": {
            "@context": "https://example.org/",
            "@type": "Organization",
            "@id": "#foreign",
        },
        "about": {"@id": "_:blank"},
    }
    copied = schema.normalize_jsonld_ids(payload, "https://example.test/page")
    assert payload["@id"] == "#article"
    assert payload["author"]["@id"] == "#ada"
    assert copied["@id"] == "https://example.test/page#article"
    assert copied["author"]["@id"] == "https://example.test/page#ada"
    assert copied["publisher"]["@id"] == "#foreign"
    assert copied["about"]["@id"] == "_:blank"
    assert (
        schema.normalize_jsonld_ids(payload["publisher"], "https://example.test/")["@id"]
        == "#foreign"
    )


def test_jsonld_local_base_and_foreign_vocab_context_are_respected():
    payload = {
        "@context": {"@vocab": "https://schema.org/", "@base": "people/"},
        "@type": "Person",
        "@id": "ada",
    }
    assert (
        schema.normalize_jsonld_ids(payload, "https://example.test/")["@id"]
        == "https://example.test/people/ada"
    )
    payload["@context"]["@base"] = None
    assert schema.normalize_jsonld_ids(payload, "https://example.test/")["@id"] == "ada"
    payload["@context"]["@vocab"] = "https://example.org/"
    assert schema.normalize_jsonld_ids(payload, "https://example.test/")["@id"] == "ada"
