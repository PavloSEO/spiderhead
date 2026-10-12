"""Extract observable page facts from HTML as the basis for a Schema.org graph.

``schema.check_schema`` validates what a site has already declared in JSON-LD.
Building or extending a graph requires a second source of truth: facts visibly
present on the page, including the title, Open Graph metadata, canonical URL,
publication date, price, rating, ``sameAs`` links, and breadcrumbs. Comparing these
facts with JSON-LD reveals disagreements between what a page presents and what its
structured data claims, which isolated block validation cannot detect.

General SEO facts come from the pure ``parser.parse_html`` function, which already
extracts title, Open Graph and Twitter metadata, canonical, headings, JSON-LD,
links, and text. This module adds only Schema-specific evidence and never performs
network access; it operates on supplied HTML.

Every heuristic is labeled. A microdata price is a directly observed fact, while
a currency-like regex match in visible text is returned with ``heuristic=True``.
This preserves the rule that unavailable or uncertain evidence must not be
reported as measured truth.
"""

from __future__ import annotations

import contextlib
import re
from typing import Any
from urllib.parse import urljoin, urlparse

from bs4 import BeautifulSoup

from seohead.checks.content_area import TEXT_EXCLUDED_TAGS
from seohead.checks.parser import (
    collapse_whitespace,
    document_base_url,
    is_inert_template_content,
    parse_html,
)
from seohead.checks.price import parse_amount, parse_price

# Hosts suitable for an organization's ``sameAs`` references.
_SOCIAL_HOSTS = (
    "facebook.com",
    "twitter.com",
    "x.com",
    "instagram.com",
    "linkedin.com",
    "youtube.com",
    "youtu.be",
    "vk.com",
    "t.me",
    "telegram.me",
    "tiktok.com",
    "github.com",
    "pinterest.com",
    "threads.net",
)

_RATING_RE = re.compile(r"(\d(?:[.,]\d{1,2})?)\s*(?:/|из|из\s*)\s*5", re.IGNORECASE)


def _meta_prop(soup: BeautifulSoup, prop: str) -> str | None:
    """Read either ``<meta property=...>`` or ``<meta name=...>``."""
    for selector in ("property", "name"):
        tag = soup.find("meta", attrs={selector: prop})
        if tag and tag.get("content"):
            return collapse_whitespace(tag.get("content"))
    return None


def _article_time(soup: BeautifulSoup, prop: str = "article:published_time") -> str | None:
    val = _meta_prop(soup, prop)
    if val:
        return val
    # ``<time datetime=...>`` is a fallback source for article dates.
    tag = soup.find("time")
    if tag and tag.get("datetime"):
        return collapse_whitespace(tag.get("datetime"))
    return None


def _rel_author(soup: BeautifulSoup) -> str | None:
    """Read author identity from ``<link rel=author>`` or ``<a rel=author>``."""
    tag = soup.find(attrs={"rel": re.compile(r"\bauthor\b", re.IGNORECASE)})
    if tag and tag.get("href"):
        return collapse_whitespace(tag.get("href"))
    return None


def _breadcrumbs(soup: BeautifulSoup, base_url: str) -> list[dict[str, str]]:
    """Extract breadcrumbs from JSON-LD first, then visible navigation.

    JSON-LD is read here only as a fact source; ``schema.check_schema`` remains
    responsible for validating its structure and vocabulary.
    """
    out: list[dict[str, str]] = []
    # Prefer a JSON-LD BreadcrumbList when available.
    for tag in soup.find_all("script"):
        if str(
            tag.get("type") or ""
        ).strip().lower() != "application/ld+json" or is_inert_template_content(tag):
            continue
        raw = (tag.string or tag.get_text() or "").strip()
        if not raw or "BreadcrumbList" not in raw:
            continue
        try:
            import json

            data = json.loads(raw)
        except ValueError:
            continue
        for node in _walk_jsonld(data):
            if isinstance(node, dict) and node.get("@type") == "BreadcrumbList":
                for item in node.get("itemListElement", []) or []:
                    name = _ld_name(item)
                    url = _ld_url(item)
                    if name:
                        out.append({"name": name, "url": url or ""})
    if out:
        return out
    # Fall back to a breadcrumb ``nav`` or ``ol`` element.
    nav = soup.select_one("nav.breadcrumb, nav[aria-label*=breadcrumb i], ol.breadcrumb")
    if nav:
        for a in nav.find_all("a"):
            text = collapse_whitespace(a.get_text(" "))
            href = a.get("href")
            if text and href:
                with contextlib.suppress(ValueError):
                    out.append({"name": text, "url": urljoin(base_url, href.strip())})
    return out


def _walk_jsonld(node: Any) -> list[Any]:
    """Flatten an arbitrarily nested JSON-LD structure."""
    out: list[Any] = []
    stack = [node]
    while stack:
        cur = stack.pop()
        if isinstance(cur, dict):
            out.append(cur)
            stack.extend(cur.values())
        elif isinstance(cur, list):
            stack.extend(cur)
    return out


def _ld_name(item: Any) -> str | None:
    if not isinstance(item, dict):
        return None
    name = item.get("name")
    if isinstance(name, str) and name.strip():
        return collapse_whitespace(name)
    inner = item.get("item") if isinstance(item.get("item"), dict) else None
    if isinstance(inner, dict) and isinstance(inner.get("name"), str):
        return collapse_whitespace(inner["name"])
    return None


def _ld_url(item: Any) -> str | None:
    if not isinstance(item, dict):
        return None
    for key in ("url", "item"):
        val = item.get(key)
        if isinstance(val, str):
            return val
        if isinstance(val, dict) and isinstance(val.get("@id"), str):
            return val["@id"]
    return None


def _same_as(soup: BeautifulSoup, base_url: str) -> list[str]:
    """Extract organization ``sameAs`` URLs from microdata and social links."""
    out: list[str] = []
    seen: set[str] = set()
    # Explicit ``itemprop="sameAs"`` is the strongest source.
    for tag in soup.select('[itemprop="sameAs"]'):
        href = (tag.get("href") or "").strip()
        if href and href not in seen:
            seen.add(href)
            out.append(href)
    # Supplement explicit values with recognized social-profile links.
    for a in soup.find_all("a", href=True):
        href = a["href"].strip()
        try:
            host = (urlparse(href).hostname or "").lower()
        except ValueError:
            continue
        if not host:
            continue
        if any(host == s or host.endswith("." + s) for s in _SOCIAL_HOSTS) and href not in seen:
            seen.add(href)
            out.append(href)
    return out


def _microdata_prop(soup: BeautifulSoup, prop: str) -> str | None:
    """Read a microdata ``itemprop`` value as text."""
    tag = soup.select_one(f'[itemprop="{prop}"]')
    if not tag:
        return None
    # Structured ``content`` and ``href`` values take precedence over text.
    for attr in ("content", "href"):
        val = tag.get(attr)
        if val:
            return collapse_whitespace(val)
    text = collapse_whitespace(tag.get_text(" "))
    return text or None


# A sentinel distinct from ``None``: no Product scope can be resolved with
# confidence, so price/rating microdata must not be read at all rather than
# falling back to a page-wide search that could pick up an unrelated item.
_AMBIGUOUS_SCOPE = object()


def _top_level_itemscopes(soup: BeautifulSoup) -> list[Any]:
    """Top-level ``itemscope`` elements: entities, not their nested properties.

    ``aggregateRating`` is itself an ``itemscope`` nested inside a Product; it
    must never be mistaken for a second competing entity.
    """
    out = []
    for tag in soup.select("[itemscope]"):
        if tag.find_parent(attrs={"itemscope": True}) is None:
            out.append(tag)
    return out


def _primary_product_scope(soup: BeautifulSoup) -> Any:
    """Find the single Microdata entity that facts like price/rating belong to.

    Returns the element to search within, ``None`` when there is no competing
    entity structure (legacy unscoped documents), or ``_AMBIGUOUS_SCOPE`` when
    more than one candidate entity exists and none can be singled out — in
    that case the caller must omit the fact rather than guess.
    """
    top_level = _top_level_itemscopes(soup)
    product_scopes = [tag for tag in top_level if "Product" in (tag.get("itemtype") or "")]
    if len(product_scopes) == 1:
        return product_scopes[0]
    if len(product_scopes) > 1:
        h1 = soup.find("h1")
        if h1 is not None:
            containing = [s for s in product_scopes if h1 in s.find_all(True) or h1 is s]
            if len(containing) == 1:
                return containing[0]
        return _AMBIGUOUS_SCOPE
    # No typed Product scope. A single untyped/other-typed itemscope is still
    # an unambiguous single entity and keeps the historical behaviour of
    # markup that never declared an explicit ``itemtype``.
    if len(top_level) == 1:
        return top_level[0]
    return None


def _microdata_in_scope(soup: BeautifulSoup, scope_re: str, prop: str) -> str | None:
    """Read ``itemprop`` only inside an ``itemscope`` matching ``scope_re``.

    Without scope awareness, ``itemprop="name"`` on a product page may capture the
    product name rather than the organization. This helper searches only the
    requested scope; callers may apply their own explicit fallback afterward.
    """
    for scope in soup.select(f'[itemtype*="{scope_re}"]'):
        tag = scope.select_one(f'[itemprop="{prop}"]')
        if tag:
            for attr in ("content", "href"):
                val = tag.get(attr)
                if val:
                    return collapse_whitespace(val)
            text = collapse_whitespace(tag.get_text(" "))
            if text:
                return text
    return None


def _organization(soup: BeautifulSoup, og: dict[str, str]) -> dict[str, Any]:
    """Extract organization name, logo, phone, and address from microdata and OG.

    Organization and LocalBusiness scopes are checked first to avoid capturing a
    product name; ``og:site_name`` and ``og:logo`` provide narrow fallbacks.
    """
    name = (
        _microdata_in_scope(soup, "Organization", "name")
        or _microdata_in_scope(soup, "LocalBusiness", "name")
        or collapse_whitespace(og.get("og:site_name"))
        or None
    )
    logo = None
    logo_meta = soup.find("meta", attrs={"property": "og:logo"}) or soup.find(
        "meta", attrs={"name": "og:logo"}
    )
    if logo_meta and logo_meta.get("content"):
        logo = collapse_whitespace(logo_meta["content"])
    if not logo:
        logo = _microdata_in_scope(soup, "Organization", "logo") or _microdata_in_scope(
            soup, "LocalBusiness", "logo"
        )
    return {
        "name": name,
        "logo": logo,
        "telephone": (
            _microdata_in_scope(soup, "Organization", "telephone")
            or _microdata_in_scope(soup, "LocalBusiness", "telephone")
        ),
        "address": (
            _microdata_in_scope(soup, "Organization", "address")
            or _microdata_in_scope(soup, "LocalBusiness", "address")
        ),
    }


def _is_markup_text(node: Any) -> bool:
    """True when ``node`` is text an element carries as markup, not as prose.

    ``content_area.TEXT_EXCLUDED_TAGS`` is the one place that answer lives, and
    this reader takes it whole rather than keeping a copy that falls behind it.
    The whole ancestor chain is tested, not just the immediate parent: an SVG
    label is a text node under ``<text>`` under ``<svg>``, so a parent-only test
    let a chart axis reading "19 900 rub." be returned as the page's price
    (issue #544). Unlike the stripped-tree readers, the price scan below walks
    the live document's text nodes, so it filters instead of decomposing.
    """
    return any(parent.name in TEXT_EXCLUDED_TAGS for parent in node.parents)


def _price(soup: BeautifulSoup, text: str, scope: Any) -> dict[str, Any] | None:
    """Extract a price from microdata, then heuristically from visible text.

    ``scope`` restricts the microdata read to the primary Product entity so a
    related-item card elsewhere on the page is never mistaken for the target's
    own price. See ``_primary_product_scope``.
    """
    if scope is _AMBIGUOUS_SCOPE:
        # The visible-text fallback is page-wide too. With competing Product
        # scopes, it has no safer ownership signal than the Microdata lookup,
        # so returning a related card's price as a target fact would only turn
        # an honest unknown into a different kind of guess.
        return None
    declared = currency = None
    source = scope if scope is not None else soup
    declared = _microdata_prop(source, "price")
    currency = _microdata_prop(source, "priceCurrency")
    if declared:
        # Microdata states the currency in its own property, so the value is
        # usually a bare number: parse the amount when no marker accompanies it.
        parsed = parse_price(declared) or {}
        value = parsed.get("value")
        if value is None:
            value = parse_amount(declared.strip())
        return {
            # A declared price is a fact, so the string stands even when it
            # does not parse; the number is offered alongside it when it does.
            "value": declared if value is None else value,
            "raw": declared,
            "currency": currency or parsed.get("currency"),
            "heuristic": False,
            "source": "microdata",
        }
    # One text node at a time. Searching the page's joined text let a match
    # begin in one price and end in the next, which on a listing of 19 900 and
    # 23 000 returned a number that was on the page nowhere.
    for node in soup.find_all(string=True):
        if _is_markup_text(node):
            continue
        found = parse_price(collapse_whitespace(node))
        if found:
            return {**found, "heuristic": True, "source": "text"}
    return None


# Words that mean "you can buy this here". The English and Russian sets both
# matter: a catalogue with no structured data and no Latin slugs is exactly the
# page this classifier used to give up on.
_COMMERCE_ACTIONS = (
    "add to cart",
    "add to basket",
    "buy now",
    "order now",
    "в корзину",
    "в козину",
    "купить",
    "заказать",
    "под заказ",
    "добавить в корзину",
    "оформить заказ",
)

# How far above a price to look for the link that makes it an item in a list
# rather than the price of this page's own subject.
_CARD_DEPTH = 5


def _structure(soup: BeautifulSoup, breadcrumbs: list[dict[str, Any]] | None) -> dict[str, Any]:
    """Shape signals: what the page is built like, independent of its language.

    Classification used to rest on the URL slug and a single price, so a
    catalogue with Cyrillic slugs and no markup had nothing to offer and came
    back as a bare WebPage. What a page is built out of — a grid of linked,
    priced cards, a gallery beside a spec table, a buy button — says what it is
    without depending on anyone writing the words in English.
    """
    priced_items = 0
    for node in soup.find_all(string=True):
        if _is_markup_text(node):
            continue
        if not parse_price(collapse_whitespace(node)):
            continue
        ancestor = node.parent
        for _ in range(_CARD_DEPTH):
            if ancestor is None:
                break
            if ancestor.name == "a" and ancestor.get("href"):
                priced_items += 1
                break
            if ancestor.find("a", href=True) is not None:
                priced_items += 1
                break
            ancestor = ancestor.parent

    actions = set()
    for tag in soup.find_all(["a", "button", "input"]):
        label = collapse_whitespace(tag.get_text(" ") or tag.get("value") or "").lower()
        actions.update(phrase for phrase in _COMMERCE_ACTIONS if phrase in label)

    return {
        "priced_items": priced_items,
        "commerce_actions": sorted(actions),
        "images": len(soup.find_all("img")),
        "spec_rows": len(soup.find_all("tr")) + len(soup.find_all("dt")),
        "breadcrumb_depth": len(breadcrumbs or []),
    }


def _rating(soup: BeautifulSoup, text: str, scope: Any) -> dict[str, Any] | None:
    """Extract a factual microdata rating or a heuristic ``4.5 out of 5`` match.

    ``scope`` restricts the microdata read to the primary Product entity, for
    the same reason as ``_price``.
    """
    if scope is _AMBIGUOUS_SCOPE:
        return None
    source = scope if scope is not None else soup
    val = _microdata_prop(source, "ratingValue")
    if val:
        return {
            "value": val,
            "count": _microdata_prop(source, "reviewCount")
            or _microdata_prop(source, "ratingCount"),
            "heuristic": False,
            "source": "microdata",
        }
    match = _RATING_RE.search(text or "")
    if match:
        return {"value": match.group(1), "count": None, "heuristic": True, "source": "text"}
    return None


def _types_from_jsonld(blocks: list[Any]) -> list[str]:
    """Extract existing JSON-LD ``@type`` values, the classifier's strongest signal."""
    types: list[str] = []
    for node in _walk_jsonld(blocks):
        if not isinstance(node, dict):
            continue
        raw = node.get("@type")
        if isinstance(raw, str):
            types.append(raw.rsplit("/", 1)[-1].rsplit(":", 1)[-1])
        elif isinstance(raw, list):
            types.extend(t.rsplit("/", 1)[-1].rsplit(":", 1)[-1] for t in raw if isinstance(t, str))
    # Deduplicate while preserving source order.
    seen: set[str] = set()
    uniq: list[str] = []
    for t in types:
        if t not in seen:
            seen.add(t)
            uniq.append(t)
    return uniq


def extract(html: str, url: str) -> dict[str, Any]:
    """Extract page facts from supplied HTML without network access.

    The flat result feeds both the page-type classifier and ``@graph`` generator.
    Fields remain ``None`` or empty when evidence is absent; explicit absence is
    preferable to an invented value.
    """
    base = parse_html(html, url)
    soup = BeautifulSoup(html, features="lxml")
    doc_base = document_base_url(soup, url)
    h1_list = base["headings"].get("h1") or []
    text = base["text"] or ""
    crumbs = _breadcrumbs(soup, doc_base)
    product_scope = _primary_product_scope(soup)

    return {
        "url": url,
        "title": base["title"],
        "description": base["meta_description"],
        "meta_robots": ", ".join(base.get("robots_meta_scoped") or []),
        "canonical": base["canonical"],
        "og": base["og"],
        "twitter": base["twitter"],
        "h1": h1_list[0] if h1_list else None,
        "word_count": base["word_count"],
        "published_time": _article_time(soup),
        "modified_time": _article_time(soup, "article:modified_time"),
        "author_rel": _rel_author(soup),
        "breadcrumbs": crumbs,
        "same_as": _same_as(soup, doc_base),
        "organization": _organization(soup, base["og"]),
        "price": _price(soup, text, product_scope),
        "structure": _structure(soup, crumbs),
        "rating": _rating(soup, text, product_scope),
        "existing_jsonld": base["jsonld"],
        "existing_types": _types_from_jsonld(base["jsonld"]),
    }
