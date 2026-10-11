"""Offline Microdata/RDFa extraction and validation regression cases."""

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


def test_rdfa_unsupported_features_are_partial_not_silently_clean():
    result = schema.check_schema(
        html="""
        <div vocab="https://schema.org/" typeof="Product">
          <span property="name">Widget</span><a rel="offers" href="#offer">Offer</a>
        </div>"""
    )
    assert result["markup_coverage"][1]["status"] == "partial"
    assert any("rel/rev/inlist" in f for f in result["findings"])


def test_rdfa_orphan_schema_property_and_foreign_metadata_are_distinct():
    orphan = schema.check_schema(html='<span property="schema:name">X</span>')
    assert orphan["markup_coverage"][1]["status"] == "partial"
    assert any("no typed subject" in f for f in orphan["findings"])
    og = schema.check_schema(html='<meta property="og:title" content="Title">')
    assert og["markup_coverage"][1]["status"] == "absent"


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
