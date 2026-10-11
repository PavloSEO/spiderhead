#!/usr/bin/env python3
# ruff: noqa: RUF001  # Russian page content is intentional
"""Deterministic loopback furniture shop with three evolving versions.

The site is a local test bench for the native crawler: ``serve --version v1``
answers a realistic furniture shop (Russian content) with planted SEO defects;
v2 and v3 are later states of the same site where defects get fixed, products
are removed or added, redirects appear, and a few regressions slip in.

Everything is generated from code: no external network, no randomness that is
not seeded from the URL, no wall-clock values in page bodies.

    python shop_site.py serve --version v1 --port 18431 --ready-file ready.json
    python shop_site.py ground-truth --out ground_truth.json
    python shop_site.py urls --version v2
"""

from __future__ import annotations

import argparse
import hashlib
import html
import ipaddress
import json
import os
import random
import re
import signal
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urljoin, urlsplit

FORMAT = "seohead.shop-site.v1"
VERSIONS = ("v1", "v2", "v3")
DEFAULT_BASE = "http://shop.example.test:18431"
SLOW_SECONDS = 2.0  # above the analyzer's 1.5 s response_time_max_s
PAGE_SIZE = 24
# Passive literals for Search in HTML; nothing loads or initializes analytics.
OLD_GTM, NEW_GTM = "GTM-K7OLD12", "GTM-M4NEW58"

# --------------------------------------------------------------------------- content


WORDS = [
    "мебель",
    "массив",
    "дуба",
    "фасад",
    "корпус",
    "фурнитура",
    "петли",
    "доводчик",
    "столешница",
    "кромка",
    "ткань",
    "велюр",
    "рогожка",
    "экокожа",
    "наполнитель",
    "пружинный",
    "блок",
    "ортопедический",
    "каркас",
    "ламинированный",
    "МДФ",
    "шпон",
    "покрытие",
    "лак",
    "масло",
    "оттенок",
    "графит",
    "орех",
    "ясень",
    "бук",
    "сосна",
    "белый",
    "серый",
    "бежевый",
    "размер",
    "ширина",
    "глубина",
    "высота",
    "сборка",
    "доставка",
    "подъём",
    "гарантия",
    "уход",
    "эксплуатация",
    "нагрузка",
    "устойчивость",
    "интерьер",
    "гостиная",
    "спальня",
    "кухня",
    "прихожая",
    "кабинет",
    "детская",
    "стиль",
    "лофт",
    "сканди",
    "классика",
    "модерн",
    "минимализм",
    "пространство",
    "хранение",
    "полки",
    "ящики",
    "направляющие",
    "зеркало",
    "подсветка",
    "комфорт",
    "надёжность",
    "экологичность",
    "материал",
    "производство",
    "фабрика",
    "замер",
    "проект",
    "дизайнер",
]


def _rng(*parts: str) -> random.Random:
    seed = hashlib.sha256("|".join(parts).encode()).hexdigest()
    return random.Random(int(seed[:16], 16))


def text(seed: str, words: int) -> str:
    """Deterministic Russian-looking prose of roughly ``words`` words in paragraphs."""
    rng = _rng("text", seed)
    paras, sentence, para, count = [], [], [], 0
    while count < words:
        sentence.append(rng.choice(WORDS))
        count += 1
        if len(sentence) >= rng.randint(8, 14):
            s = " ".join(sentence)
            para.append(s[0].upper() + s[1:] + ".")
            sentence = []
            if len(para) >= 4:
                paras.append("<p>" + " ".join(para) + "</p>")
                para = []
    if sentence:
        s = " ".join(sentence)
        para.append(s[0].upper() + s[1:] + ".")
    if para:
        paras.append("<p>" + " ".join(para) + "</p>")
    return "".join(paras)


CATEGORIES = (
    # slug, name, en, v1 count
    ("kuhni", "Кухни", "Kitchens", 142),
    ("gostinye", "Гостиные", "Living rooms", 132),
    ("spalni", "Спальни", "Bedrooms", 140),
    ("shkafy", "Шкафы", "Wardrobes", 146),
    ("stoly", "Столы", "Tables", 136),
    ("stulya", "Стулья", "Chairs", 150),
    ("divany", "Диваны", "Sofas", 138),
    ("krovati", "Кровати", "Beds", 126),
)
ADJ = (
    "Альба",
    "Верона",
    "Лофт",
    "Скандинавия",
    "Милан",
    "Прованс",
    "Норд",
    "Модена",
    "Торино",
    "Сиена",
    "Флоренция",
    "Бергамо",
    "Римини",
    "Лугано",
    "Генуя",
    "Палермо",
    "Олимп",
    "Аврора",
    "Сканди",
    "Квадро",
)
SINGULAR = {
    "kuhni": "Кухня",
    "gostinye": "Гостиная",
    "spalni": "Спальня",
    "shkafy": "Шкаф",
    "stoly": "Стол",
    "stulya": "Стул",
    "divany": "Диван",
    "krovati": "Кровать",
}

# Removed in v2 (discontinued): first three go 404, last three 301 to their category.
DISCONTINUED = (
    "stoly/stol-007",
    "stoly/stol-012",
    "stulya/stul-020",
    "divany/divan-003",
    "divany/divan-009",
    "kuhni/kuhnya-030",
)
NEW_PRODUCTS = {"v2": 45, "v3": 14}  # appended to categories round-robin
INFO = (
    ("o-kompanii", "О компании"),
    ("kontakty", "Контакты"),
    ("dostavka", "Доставка"),
    ("oplata", "Оплата"),
    ("garantiya", "Гарантия"),
    ("politika-konfidencialnosti", "Политика конфиденциальности"),
    ("usloviya", "Условия продажи"),
    ("vakansii", "Вакансии"),
    ("showroom", "Шоурумы"),
    ("otzyvy", "Отзывы"),
)
BLOG_V1 = 36
BLOG_NEW_SECTION = 24  # v2 adds /blog/dizayn-interera/ section
BLOG_V3_EXTRA = 10


def _translit_product(cat: str, i: int) -> str:
    stem = {
        "kuhni": "kuhnya",
        "gostinye": "gostinaya",
        "spalni": "spalnya",
        "shkafy": "shkaf",
        "stoly": "stol",
        "stulya": "stul",
        "divany": "divan",
        "krovati": "krovat",
    }[cat]
    return f"{stem}-{i:03d}"


# --------------------------------------------------------------------------- defects


@dataclass
class Defect:
    id: str
    checks: tuple[str, ...]  # analyzer check ids expected to fire
    urls: tuple[str, ...]  # paths where the finding should be reported
    versions: str  # e.g. "12" = present in v1 and v2
    note: str
    detectable: bool = True  # False: plantable but not observable over plain-http loopback

    def active(self, version: str) -> bool:
        return version[1] in self.versions


DEFECTS: tuple[Defect, ...] = (
    # 7.A statuses / robots
    Defect(
        "D01",
        ("BROKEN_PAGE_4XX", "BROKEN_INTERNAL_LINK"),
        ("/catalog/stoly/stol-loft-staryy/",),
        "1",
        "category links a product URL that never existed (404)",
    ),
    Defect(
        "D02",
        ("BROKEN_PAGE_4XX", "BROKEN_INTERNAL_LINK"),
        ("/catalog/stoly/stol-007/", "/catalog/stoly/stol-012/", "/catalog/stulya/stul-020/"),
        "2",
        "discontinued products return 404 but an old blog post still links them; v3 adds 301s",
    ),
    Defect(
        "D03",
        ("INTERNAL_LINK_TO_REDIRECT",),
        ("/catalog/stoly/stol-007/", "/catalog/stoly/stol-012/", "/catalog/stulya/stul-020/"),
        "3",
        "after v3 redirects the old blog post still links the redirected URLs",
    ),
    Defect(
        "D04",
        ("SITEMAP_URL_3XX",),
        ("/catalog/divany/divan-003/", "/catalog/divany/divan-009/", "/catalog/kuhni/kuhnya-030/"),
        "2",
        "discontinued products 301 to category but stay in sitemap-products.xml",
    ),
    Defect(
        "D05",
        ("SITEMAP_URL_4XX_5XX",),
        ("/catalog/stoly/stol-007/", "/catalog/stoly/stol-012/", "/catalog/stulya/stul-020/"),
        "2",
        "404 discontinued products stay in the sitemap",
    ),
    Defect(
        "D06",
        ("SERVER_ERROR_5XX", "LINK_TO_5XX"),
        ("/aktsii/",),
        "12",
        "promotions page answers 500",
    ),
    Defect(
        "D07",
        ("REDIRECT_CHAIN",),
        ("/skidki/",),
        "1",
        "/skidki/ -> /rasprodazha/ -> /sale/ -> /aktsii/",
    ),
    Defect(
        "D08",
        ("REDIRECT_LOOP",),
        ("/staryy-katalog/",),
        "1",
        "footer link into a two-URL redirect loop",
    ),
    Defect(
        "D09",
        ("BAD_REDIRECT_TYPE",),
        ("/dostavka-i-oplata/",),
        "12",
        "permanent move served as 302",
    ),
    Defect(
        "D10", ("META_REFRESH_REDIRECT",), ("/akcii-arhiv/",), "1", "meta refresh instead of 301"
    ),
    Defect(
        "D11",
        ("IMPORTANT_URL_BLOCKED_BY_ROBOTS", "BLOCKED_BY_ROBOTS"),
        ("/blog/tag/kuhni/", "/blog/tag/spalni/", "/blog/tag/divany/"),
        "12",
        "linked blog tag pages disallowed in robots.txt",
    ),
    Defect(
        "D12",
        ("ROBOTS_BLOCKS_RESOURCES",),
        ("/",),
        "1",
        "robots.txt Disallow: /static/js/ (site-level, reported on the start URL)",
    ),
    # canonical / indexability
    Defect(
        "D13",
        ("CANONICAL_MISSING",),
        tuple(f"/{s}/" for s, _ in INFO[:5]),
        "1",
        "info pages without canonical",
    ),
    Defect(
        "D14",
        ("CANONICAL_TARGET_ERROR",),
        ("/catalog/shkafy/shkaf-011/",),
        "12",
        "canonical points to a 404",
    ),
    Defect(
        "D15",
        ("CANONICAL_CHAIN",),
        ("/catalog/krovati/krovat-001/",),
        "1",
        "canonical -> krovat-002 whose canonical -> krovat-003",
    ),
    Defect(
        "D16",
        ("NOINDEX", "SITEMAP_URL_NON_INDEXABLE"),
        ("/garantiya/",),
        "13",
        "meta noindex on the warranty page (fixed v2, regressed v3)",
    ),
    Defect("D17", ("NOINDEX",), ("/oplata/",), "12", "X-Robots-Tag: noindex header"),
    Defect(
        "D18", ("NOFOLLOW_PAGE",), ("/blog/?page=2",), "1", "meta robots nofollow on blog page 2"
    ),
    Defect(
        "D19",
        ("CANONICALISED",),
        tuple(f"/catalog/stulya/stul-{i:03d}/" for i in range(1, 7)),
        "3",
        "regression: chair products canonicalised to the category",
    ),
    # titles / meta / headings
    Defect(
        "D20",
        ("TITLE_DUPLICATE",),
        tuple(f"/catalog/stulya/stul-{i:03d}/" for i in range(30, 34)),
        "1",
        "four chairs share one title",
    ),
    Defect(
        "D21",
        ("DESC_DUPLICATE",),
        tuple(f"/catalog/kuhni/kuhnya-{i:03d}/" for i in range(40, 46)),
        "12",
        "six kitchens share one description",
    ),
    Defect(
        "D22",
        ("TITLE_MISSING",),
        tuple(f"/catalog/gostinye/gostinaya-{i:03d}/" for i in (5, 6, 7)),
        "1",
        "no <title>",
    ),
    Defect(
        "D23",
        ("TITLE_TOO_LONG",),
        ("/catalog/divany/", "/catalog/krovati/"),
        "123",
        "category titles > 60 chars",
    ),
    Defect(
        "D24",
        ("DESC_MISSING",),
        ("/vakansii/", "/showroom/", "/otzyvy/"),
        "12",
        "no meta description",
    ),
    Defect(
        "D25", ("H1_MISSING",), ("/catalog/shkafy/", "/catalog/stoly/"), "1", "category without H1"
    ),
    Defect("D26", ("H1_MULTIPLE",), ("/",), "12", "home has two H1"),
    Defect(
        "D27",
        ("H1_DUPLICATE",),
        ("/blog/kak-vybrat-matras/", "/blog/kak-vybrat-matras-2/"),
        "1",
        "two articles share H1",
    ),
    Defect(
        "D28", ("DESC_TOO_LONG",), (), "2", "new design-blog section ships descriptions > 160 chars"
    ),
    Defect(
        "D29", ("HEADING_SKIP",), ("/blog/uhod-za-mebelyu-iz-massiva/",), "1", "h1 followed by h4"
    ),
    Defect("D30", ("VIEWPORT_MISSING",), ("/blog/",), "1", "blog index lacks viewport meta"),
    # content
    Defect(
        "D31", ("THIN_CONTENT",), (), "1", "v1 blog articles are short (< 200 words); grow in v2/v3"
    ),
    Defect(
        "D32",
        ("THIN_CONTENT",),
        ("/blog/dizayn-interera/trendy-2027/", "/blog/dizayn-interera/cveta-sezona/"),
        "2",
        "two new-section drafts published thin",
    ),
    Defect(
        "D33",
        ("LOREM_IPSUM_PLACEHOLDER",),
        ("/catalog/krovati/krovat-127/",),
        "2",
        "new product with lorem ipsum",
    ),
    Defect(
        "D34",
        ("TITLE_DUPLICATE", "CANONICAL_MISSING", "NEAR_DUPLICATE"),
        tuple(f"/catalog/{c}/?sort=price" for c, *_ in CATEGORIES[:4]),
        "1",
        "sort parameter pages duplicate the category without canonical",
    ),
    Defect("D35", ("NO_INTERNAL_OUTLINKS",), ("/spasibo/",), "12", "thank-you page with no links"),
    Defect("D36", ("GENERIC_ANCHOR_TEXT",), (), "12", "'подробнее' anchors in blog listing"),
    # links / structure
    Defect(
        "D37",
        ("SITEMAP_ORPHAN",),
        ("/landing/kuhni-na-zakaz/", "/landing/shkafy-kupe/"),
        "12",
        "landings only in sitemap, no inlinks",
    ),
    Defect(
        "D38",
        ("DEEP_CRAWL_DEPTH",),
        tuple(f"/blog/arhiv/{y}/" for y in (2019, 2018)),
        "12",
        "archive reachable only through a year-by-year chain",
    ),
    Defect(
        "D39",
        ("URL_UPPERCASE",),
        ("/Catalog/Shkafy/Rasprodazha/",),
        "1",
        "uppercase URL linked from menu",
    ),
    Defect(
        "D40",
        ("URL_UNDERSCORES",),
        ("/blog/kak_vybrat_divan/",),
        "123",
        "legacy underscore slug kept",
    ),
    Defect(
        "D41",
        ("PAGINATION_SEQUENCE_ERROR",),
        ("/catalog/stulya/?page=2",),
        "1",
        "rel=next skips page 3",
    ),
    # hreflang
    Defect(
        "D42",
        ("HREFLANG_MISSING_RETURN_LINK",),
        ("/en/catalog/kuhni/",),
        "1",
        "en kitchens lacks ru return",
    ),
    Defect("D43", ("HREFLANG_INVALID_CODE",), ("/catalog/spalni/",), "12", "hreflang='en-UK'"),
    Defect(
        "D44", ("HREFLANG_BROKEN_TARGET",), ("/catalog/divany/",), "1", "hreflang en points to 404"
    ),
    # images
    Defect(
        "D45",
        ("IMG_MISSING_ALT_ATTRIBUTE",),
        tuple(f"/catalog/spalni/spalnya-{i:03d}/" for i in range(1, 9)),
        "12",
        "bedroom product images without alt",
    ),
    Defect("D46", ("IMG_OVER_KB",), ("/img/banner-glavnaya.jpg",), "12", "home banner ~320 KB"),
    Defect(
        "D47",
        ("BROKEN_INTERNAL_LINK", "BROKEN_PAGE_4XX"),
        ("/img/catalog/stoly-hero.jpg",),
        "1",
        "category hero image 404",
    ),
    # structured data
    Defect(
        "D48",
        ("STRUCTURED_DATA_PARSE_ERROR",),
        tuple(f"/catalog/divany/divan-{i:03d}/" for i in (20, 21, 22)),
        "1",
        "truncated JSON-LD",
    ),
    Defect(
        "D49",
        ("STRUCTURED_DATA_PARSE_ERROR",),
        ("/catalog/kuhni/kuhnya-001/",),
        "3",
        "regression: trailing comma in Product JSON-LD",
    ),
    # performance
    Defect(
        "D50",
        ("SLOW_RESPONSE",),
        ("/catalog/shkafy/?page=2",),
        "1",
        f"answers after {SLOW_SECONDS}s",
    ),
    Defect(
        "D51",
        ("SLOW_RESPONSE",),
        ("/blog/",),
        "3",
        f"regression: blog index answers after {SLOW_SECONDS}s",
    ),
    # soft 404 / mixed content (not directly mapped to native-crawl checks)
    Defect(
        "D52",
        ("SOFT_404",),
        ("/catalog/stoly/ne-naydeno/",),
        "12",
        "200 page saying 'Товар не найден'",
    ),
    Defect(
        "D54",
        ("HTML_SEARCH:GTM-K7OLD12",),
        ("/kontakty/",),
        "12",
        "Search-in-HTML marker: retired GTM container id; site-wide in v1, left on /kontakty/ in v2",
        detectable=False,
    ),
    Defect(
        "D55",
        ("REDIRECT_CHAIN",),
        ("/catalog/stoly/stol-007/",),
        "3",
        "regression: 301 to a temporary /tovar/ URL that 301s again to the category",
    ),
    Defect(
        "D53",
        ("MIXED_CONTENT", "INSECURE_SUBRESOURCE"),
        ("/kontakty/",),
        "12",
        "http:// map script — only meaningful on https, so not observable on loopback http",
        detectable=False,
    ),
)


# --------------------------------------------------------------------------- site model


@dataclass
class Resp:
    status: int
    body: bytes = b""
    ctype: str = "text/html; charset=utf-8"
    headers: dict[str, str] = field(default_factory=dict)
    delay: float = 0.0


class Site:
    def __init__(self, version: str, base: str = DEFAULT_BASE):
        if version not in VERSIONS:
            raise ValueError(version)
        self.v = version
        self.n = int(version[1])
        self.base = base.rstrip("/")
        self.on = {d.id: d.active(version) for d in DEFECTS}
        self.routes: dict[str, Resp] = {}
        self.products = self._products()
        self.blog = self._blog()
        self._build()

    # ---- inventory

    def _products(self) -> dict[str, list[dict]]:
        out: dict[str, list[dict]] = {}
        for cat, _name, _en, count in CATEGORIES:
            items = []
            for i in range(1, count + 1):
                slug = _translit_product(cat, i)
                if self.n >= 2 and f"{cat}/{slug}" in DISCONTINUED:
                    continue
                items.append({"cat": cat, "slug": slug, "i": i})
            out[cat] = items
        extra = sum(NEW_PRODUCTS.get(f"v{k}", 0) for k in range(2, self.n + 1))
        for j in range(extra):
            cat = CATEGORIES[j % len(CATEGORIES)][0]
            i = dict((c, cnt) for c, _, _, cnt in CATEGORIES)[cat] + 1 + j // len(CATEGORIES)
            out[cat].append({"cat": cat, "slug": _translit_product(cat, i), "i": i})
        return out

    def _blog(self) -> list[dict]:
        topics = (
            "kak-vybrat-divan-dlya-gostinoy",
            "kak-vybrat-matras",
            "kak-vybrat-matras-2",
            "uhod-za-mebelyu-iz-massiva",
            "kuhnya-v-stile-loft",
            "shkaf-kupe-ili-raspashnoy",
            "obedennyy-stol-razmery",
            "stulya-dlya-kuhni",
            "spalnya-v-skandinavskom-stile",
            "kak_vybrat_divan",
        )
        posts = [{"slug": t, "section": ""} for t in topics]
        posts += [
            {"slug": f"statya-{k:02d}", "section": ""} for k in range(len(topics) + 1, BLOG_V1 + 1)
        ]
        if self.n >= 2:
            sec = ["trendy-2027", "cveta-sezona"] + [
                f"ideya-{k:02d}" for k in range(3, BLOG_NEW_SECTION + 1)
            ]
            posts += [{"slug": s, "section": "dizayn-interera"} for s in sec]
        if self.n >= 3:
            posts += [
                {"slug": f"obzor-{k:02d}", "section": ""} for k in range(1, BLOG_V3_EXTRA + 1)
            ]
        for p in posts:
            p["path"] = f"/blog/{p['section'] + '/' if p['section'] else ''}{p['slug']}/"
        return posts

    # ---- helpers

    def abs(self, path: str) -> str:
        return self.base + path

    def add(self, path: str, resp: Resp) -> None:
        self.routes[path] = resp

    def page(
        self,
        path: str,
        *,
        title: str | None,
        h1: str | list[str] | None,
        body: str,
        desc: str | None = "",
        canonical: str | None = "self",
        robots: str = "",
        hreflang: str = "",
        jsonld: str = "",
        viewport: bool = True,
        extra_head: str = "",
        headers: dict | None = None,
        delay: float = 0.0,
        lang: str = "ru",
    ) -> None:
        head = ["<meta charset='utf-8'>"]
        if viewport:
            head.append("<meta name='viewport' content='width=device-width, initial-scale=1'>")
        if title is not None:
            head.append(f"<title>{html.escape(title)}</title>")
        if desc == "":
            desc = f"{title or 'Мебельный магазин'} — доставка по Минску и Беларуси, гарантия, сборка и подъём."
            desc = desc[:158]
        if desc:
            head.append(f"<meta name='description' content='{html.escape(desc)}'>")
        if canonical:
            head.append(
                f"<link rel='canonical' href='{self.abs(path if canonical == 'self' else canonical)}'>"
            )
        if robots:
            head.append(f"<meta name='robots' content='{robots}'>")
        head.append("<link rel='stylesheet' href='/static/css/main.css'>")
        gtm = OLD_GTM if (self.n == 1 or (path == "/kontakty/" and self.on["D54"])) else NEW_GTM
        head.append(
            f"<script type='application/json' data-tag-manager='{gtm}'>{{\"container\":\"{gtm}\"}}</script>"
        )
        head.append(f"<meta property='og:title' content='{html.escape(title or '')}'>")
        head += [hreflang, jsonld, extra_head]
        h1s = [h1] if isinstance(h1, str) else (h1 or [])
        h1html = "".join(f"<h1>{html.escape(h)}</h1>" for h in h1s)
        doc = (
            f"<!doctype html><html lang='{lang}'><head>{''.join(head)}</head><body>"
            f"{self.header()}<main>{h1html}{body}</main>{self.footer()}"
            "<script src='/static/js/app.js' defer></script></body></html>"
        )
        self.add(path, Resp(200, doc.encode(), headers=headers or {}, delay=delay))

    def header(self) -> str:
        cats = "".join(f"<li><a href='/catalog/{c}/'>{n}</a></li>" for c, n, *_ in CATEGORIES)
        sale = (
            "<li><a href='/Catalog/Shkafy/Rasprodazha/'>Распродажа шкафов</a></li>"
            if self.on["D39"]
            else ""
        )
        return (
            f"<header><a href='/'><img src='/img/logo.png' alt='Мебельный магазин' width='160' height='40'></a>"
            f"<nav><ul><li><a href='/catalog/'>Каталог</a></li>{cats}{sale}"
            "<li><a href='/blog/'>Блог</a></li><li><a href='/aktsii/'>Акции</a></li></ul></nav></header>"
        )

    def footer(self) -> str:
        info = "".join(f"<li><a href='/{s}/'>{n}</a></li>" for s, n in INFO)
        legacy = ""
        if self.on["D07"]:
            legacy += "<li><a href='/skidki/'>Скидки</a></li>"
        if self.on["D08"]:
            legacy += "<li><a href='/staryy-katalog/'>Старый каталог</a></li>"
        if self.on["D09"]:
            legacy += "<li><a href='/dostavka-i-oplata/'>Доставка и оплата</a></li>"
        if self.on["D10"]:
            legacy += "<li><a href='/akcii-arhiv/'>Архив акций</a></li>"
        return (
            f"<footer><ul>{info}{legacy}<li><a href='/en/'>English</a></li></ul>"
            "<p>© Мебельный магазин. Тестовый сайт, вымышленные товары.</p></footer>"
        )

    def org_ld(self) -> str:
        return (
            "<script type='application/ld+json'>"
            + json.dumps(
                {
                    "@context": "https://schema.org",
                    "@type": "Organization",
                    "name": "Мебельный магазин",
                    "url": self.abs("/"),
                    "logo": self.abs("/img/logo.png"),
                },
                ensure_ascii=False,
            )
            + "</script>"
        )

    def crumbs_ld(self, items: list[tuple[str, str]]) -> str:
        return (
            "<script type='application/ld+json'>"
            + json.dumps(
                {
                    "@context": "https://schema.org",
                    "@type": "BreadcrumbList",
                    "itemListElement": [
                        {"@type": "ListItem", "position": k + 1, "name": n, "item": self.abs(p)}
                        for k, (n, p) in enumerate(items)
                    ],
                },
                ensure_ascii=False,
            )
            + "</script>"
        )

    # ---- build

    def _build(self) -> None:
        self._assets()
        self._home()
        self._catalog()
        self._blog_pages()
        self._info()
        self._legacy()
        self._en()
        self._robots_sitemaps()

    def _assets(self) -> None:
        self.add(
            "/static/css/main.css",
            Resp(200, b"body{font-family:sans-serif}main{max-width:1100px}", "text/css"),
        )
        self.add(
            "/static/js/app.js",
            Resp(200, b"document.documentElement.classList.add('js');", "application/javascript"),
        )
        self.add("/img/logo.png", Resp(200, _image("logo", 2), "image/png"))
        banner_kb = 320 if (self.on["D46"]) else 90
        self.add("/img/banner-glavnaya.jpg", Resp(200, _image("banner", banner_kb), "image/jpeg"))
        for c, *_ in CATEGORIES:
            if c == "stoly" and self.on["D47"]:
                continue  # hero image missing -> 404
            self.add(f"/img/catalog/{c}-hero.jpg", Resp(200, _image(c, 40), "image/jpeg"))

    def _home(self) -> None:
        cats = "".join(
            f"<li><a href='/catalog/{c}/'>{n} — каталог</a></li>" for c, n, *_ in CATEGORIES
        )
        news = "".join(
            f"<li><a href='{p['path']}'>{_post_title(p)}</a></li>" for p in self.blog[-6:]
        )
        h1 = (
            ["Мебельный магазин", "Мебель для дома"]
            if self.on["D26"]
            else "Мебельный магазин — мебель для дома"
        )
        body = (
            f"<img src='/img/banner-glavnaya.jpg' alt='Новая коллекция мебели' width='1200' height='400'>"
            f"<h2>Каталог</h2><ul>{cats}</ul><h2>Новое в блоге</h2><ul>{news}</ul>"
            f"<h2>О магазине</h2>{text('home', 260)}"
        )
        hl = (
            f"<link rel='alternate' hreflang='ru' href='{self.abs('/')}'>"
            f"<link rel='alternate' hreflang='en' href='{self.abs('/en/')}'>"
            f"<link rel='alternate' hreflang='x-default' href='{self.abs('/')}'>"
        )
        self.page(
            "/",
            title="Мебельный магазин — кухни, шкафы и диваны с доставкой",
            h1=h1,
            body=body,
            hreflang=hl,
            jsonld=self.org_ld(),
        )

    def _catalog(self) -> None:
        all_links = "".join(f"<li><a href='/catalog/{c}/'>{n}</a></li>" for c, n, *_ in CATEGORIES)
        self.page(
            "/catalog/",
            title="Каталог мебели — все категории магазина",
            h1="Каталог мебели",
            body=f"<ul>{all_links}</ul>{text('catalog', 240)}",
        )
        for cat, name, _en, _ in CATEGORIES:
            items = self.products[cat]
            pages = max(1, -(-len(items) // PAGE_SIZE))
            for pg in range(1, pages + 1):
                path = f"/catalog/{cat}/" + (f"?page={pg}" if pg > 1 else "")
                chunk = items[(pg - 1) * PAGE_SIZE : pg * PAGE_SIZE]
                cards = "".join(
                    f"<li><a href='/catalog/{cat}/{p['slug']}/'>{_product_name(cat, p['i'])}</a></li>"
                    for p in chunk
                )
                nums = "".join(
                    f"<a href='/catalog/{cat}/" + (f"?page={k}" if k > 1 else "") + f"'>{k}</a> "
                    for k in range(1, pages + 1)
                )
                rel = ""
                if pg < pages:
                    nxt = pg + 1
                    if cat == "stulya" and pg == 2 and self.on["D41"] and pages > 3:
                        nxt = 4
                    rel += f"<link rel='next' href='{self.abs(f'/catalog/{cat}/?page={nxt}')}'>"
                if pg > 1:
                    prev = f"/catalog/{cat}/" + (f"?page={pg - 1}" if pg > 2 else "")
                    rel += f"<link rel='prev' href='{self.abs(prev)}'>"
                body = (
                    f"<p><a href='/catalog/{cat}/?sort=price'>Сортировать по цене</a></p><ul>{cards}</ul>"
                    f"<nav class='pages'>{nums}</nav>"
                )
                if pg == 1:
                    hero = f"<img src='/img/catalog/{cat}-hero.jpg' alt='{name}' width='1200' height='300'>"
                    body = hero + body + f"<h2>{name} на заказ</h2>{text('cat' + cat, 230)}"
                    if cat == "stoly" and self.on["D01"]:
                        body += "<p><a href='/catalog/stoly/stol-loft-staryy/'>Стол Лофт (архив)</a></p>"
                    if cat == "stoly" and self.on["D52"]:
                        body += "<p><a href='/catalog/stoly/ne-naydeno/'>Стол Нордик</a></p>"
                title = f"{name} — купить в Минске недорого, каталог {name.lower()}"
                if cat in ("divany", "krovati") and self.on["D23"] and pg == 1:
                    title = (
                        f"{name} купить в Минске недорого с доставкой и сборкой — каталог "
                        f"{name.lower()} от производителя, цены, фото"
                    )
                elif pg > 1:
                    title = f"{name} — страница {pg} каталога мебельного магазина"
                h1 = (
                    None
                    if (cat in ("shkafy", "stoly") and self.on["D25"] and pg == 1)
                    else (name if pg == 1 else f"{name} — страница {pg}")
                )
                hl = ""
                if pg == 1:
                    en_target = f"/en/catalog/{cat}/"
                    if cat == "divany" and self.on["D44"]:
                        en_target = "/en/catalog/sofas-old/"
                    code = "en-UK" if (cat == "spalni" and self.on["D43"]) else "en"
                    hl = (
                        f"<link rel='alternate' hreflang='ru' href='{self.abs(path)}'>"
                        f"<link rel='alternate' hreflang='{code}' href='{self.abs(en_target)}'>"
                    )
                delay = SLOW_SECONDS if (cat == "shkafy" and pg == 2 and self.on["D50"]) else 0.0
                self.page(
                    path,
                    title=title,
                    h1=h1,
                    body=body,
                    hreflang=hl,
                    extra_head=rel,
                    delay=delay,
                    jsonld=self.crumbs_ld(
                        [("Главная", "/"), ("Каталог", "/catalog/"), (name, f"/catalog/{cat}/")]
                    ),
                )
            # sort parameter page
            sp = f"/catalog/{cat}/?sort=price"
            first = items[:PAGE_SIZE]
            cards = "".join(
                f"<li><a href='/catalog/{cat}/{p['slug']}/'>{_product_name(cat, p['i'])}</a></li>"
                for p in sorted(first, key=lambda p: p["slug"])
            )
            dup = self.on["D34"] and cat in [c for c, *_ in CATEGORIES[:4]]
            self.page(
                sp,
                title=f"{name} — купить в Минске недорого, каталог {name.lower()}",
                h1=name,
                body=f"<ul>{cards}</ul>{text('cat' + cat, 230)}",
                canonical=None if dup else f"/catalog/{cat}/",
            )
            for p in items:
                self._product(cat, name, p)
        if self.on["D52"]:
            self.page(
                "/catalog/stoly/ne-naydeno/",
                title="Товар не найден — мебельный магазин онлайн",
                h1="Товар не найден",
                body="<p>К сожалению, товар не найден. Вернитесь в <a href='/catalog/'>каталог</a>.</p>",
            )
        for land in ("kuhni-na-zakaz", "shkafy-kupe"):
            if self.on["D37"]:
                self.page(
                    f"/landing/{land}/",
                    title=f"{land.replace('-', ' ').capitalize()} — спецпредложение магазина",
                    h1=land.replace("-", " ").capitalize(),
                    body=text("land" + land, 260),
                )
        if self.n >= 2:  # D02/D03/D04: discontinued products
            for key in DISCONTINUED:
                cat, slug = key.split("/")
                path = f"/catalog/{cat}/{slug}/"
                if key in DISCONTINUED[:3] and self.n == 2:
                    continue  # plain 404
                if key == DISCONTINUED[0] and self.on["D55"]:
                    self.add(path, Resp(301, headers={"Location": self.abs(f"/tovar/{slug}/")}))
                    self.add(
                        f"/tovar/{slug}/",
                        Resp(301, headers={"Location": self.abs(f"/catalog/{cat}/")}),
                    )
                    continue
                self.add(path, Resp(301, headers={"Location": self.abs(f"/catalog/{cat}/")}))

    def _product(self, cat: str, name: str, p: dict) -> None:
        path = f"/catalog/{cat}/{p['slug']}/"
        pname = _product_name(cat, p["i"])
        title = f"{pname} — купить, цена {_price(cat, p['i'])} руб."
        desc = f"{pname}: материалы, размеры, фото. Доставка и сборка, гарантия 24 месяца. Мебельный магазин."
        canonical = "self"
        idx = p["i"]
        if cat == "stulya" and 30 <= idx <= 33 and self.on["D20"]:
            title = "Стул для кухни — купить в мебельном магазине"
        if cat == "kuhni" and 40 <= idx <= 45 and self.on["D21"]:
            desc = "Кухни на заказ по индивидуальным размерам. Доставка, сборка и гарантия от мебельного магазина."
        if cat == "gostinye" and idx in (5, 6, 7) and self.on["D22"]:
            title = None
        if cat == "shkafy" and idx == 11 and self.on["D14"]:
            canonical = "/catalog/shkafy/shkaf-011-old/"
        if cat == "krovati" and self.on["D15"]:
            if idx == 1:
                canonical = "/catalog/krovati/krovat-002/"
            elif idx == 2:
                canonical = "/catalog/krovati/krovat-003/"
        if cat == "stulya" and idx <= 6 and self.on["D19"]:
            canonical = "/catalog/stulya/"
        alt = (
            ""
            if (cat == "spalni" and idx <= 8 and self.on["D45"])
            else f" alt='{html.escape(pname)}'"
        )
        img = f"/img/products/{cat}/{p['slug']}.jpg"
        self.add(img, Resp(200, _image(img, 18), "image/jpeg"))
        ld = {
            "@context": "https://schema.org",
            "@type": "Product",
            "name": pname,
            "sku": p["slug"],
            "image": self.abs(img),
            "offers": {
                "@type": "Offer",
                "priceCurrency": "BYN",
                "price": str(_price(cat, p["i"])),
                "availability": "https://schema.org/InStock",
            },
        }
        ld_s = json.dumps(ld, ensure_ascii=False)
        if cat == "divany" and idx in (20, 21, 22) and self.on["D48"]:
            ld_s = ld_s[: len(ld_s) // 2]
        if cat == "kuhni" and idx == 1 and self.on["D49"]:
            ld_s = ld_s[:-1] + ",}"
        words = 240 + (40 if self.n >= 2 else 0)
        body = text(path, words)
        if cat == "krovati" and self.on["D33"] and idx == 127:
            body = (
                "<p>" + ("Lorem ipsum dolor sit amet, consectetur adipiscing elit. " * 40) + "</p>"
            )
        siblings = [q for q in self.products[cat] if q is not p][: 4 if self.n >= 2 else 2]
        rel = "".join(
            f"<li><a href='/catalog/{cat}/{q['slug']}/'>{_product_name(cat, q['i'])}</a></li>"
            for q in siblings
        )
        self.page(
            path,
            title=title,
            h1=pname,
            desc=desc,
            canonical=canonical,
            body=f"<img src='{img}'{alt} width='800' height='600'>{body}<h2>Похожие товары</h2><ul>{rel}</ul>",
            jsonld=f"<script type='application/ld+json'>{ld_s}</script>"
            + self.crumbs_ld([("Главная", "/"), (name, f"/catalog/{cat}/"), (pname, path)]),
        )

    def _blog_pages(self) -> None:
        words = {1: 150, 2: 420, 3: 650}[self.n]
        per = 12
        listing = self.blog
        pages = -(-len(listing) // per)
        for pg in range(1, pages + 1):
            path = "/blog/" + (f"?page={pg}" if pg > 1 else "")
            chunk = listing[(pg - 1) * per : pg * per]
            if self.on["D36"]:
                cards = "".join(
                    f"<li>{_post_title(p)} <a href='{p['path']}'>подробнее</a></li>" for p in chunk
                )
            else:
                cards = "".join(
                    f"<li><a href='{p['path']}'>{_post_title(p)}</a></li>" for p in chunk
                )
            nums = "".join(
                "<a href='/blog/" + (f"?page={k}" if k > 1 else "") + f"'>{k}</a> "
                for k in range(1, pages + 1)
            )
            tags = "".join(
                f"<a href='/blog/tag/{t}/'>#{t}</a> " for t in ("kuhni", "spalni", "divany")
            )
            extra = ""
            if pg == 1 and self.on["D38"]:
                extra = "<p><a href='/blog/arhiv/2023/'>Архив публикаций</a></p>"
            if pg == 1 and self.n >= 2:
                extra += "<p><a href='/blog/dizayn-interera/'>Дизайн интерьера</a></p>"
            self.page(
                path,
                title="Блог мебельного магазина — советы по выбору мебели"
                if pg == 1
                else f"Блог мебельного магазина — страница {pg} статей и советов",
                h1="Блог" if pg == 1 else f"Блог — страница {pg}",
                body=f"<p>{tags}</p><ul>{cards}</ul><nav>{nums}</nav>{extra}"
                + (text("blogidx", 220) if pg == 1 else text(f"blog{pg}", 220)),
                robots="index, nofollow" if (pg == 2 and self.on["D18"]) else "",
                viewport=not (pg == 1 and self.on["D30"]),
                delay=SLOW_SECONDS if (pg == 1 and self.on["D51"]) else 0.0,
            )
        for t in ("kuhni", "spalni", "divany"):
            self.page(
                f"/blog/tag/{t}/",
                title=f"Статьи с тегом {t} — блог мебельного магазина",
                h1=f"Тег {t}",
                body="".join(
                    f"<p><a href='{p['path']}'>{_post_title(p)}</a></p>" for p in listing[:5]
                )
                + text("tag" + t, 220),
            )
        if self.n >= 2:
            sec = [p for p in listing if p["section"]]
            self.page(
                "/blog/dizayn-interera/",
                title="Дизайн интерьера — идеи и тренды в блоге магазина",
                h1="Дизайн интерьера",
                body="<ul>"
                + "".join(f"<li><a href='{p['path']}'>{_post_title(p)}</a></li>" for p in sec)
                + "</ul>"
                + text("sec", 230),
            )
        if self.on["D38"]:
            years = list(range(2023, 2017, -1))
            for k, y in enumerate(years):
                nxt = (
                    f"<a href='/blog/arhiv/{years[k + 1]}/'>Более ранние публикации</a>"
                    if k + 1 < len(years)
                    else ""
                )
                self.page(
                    f"/blog/arhiv/{y}/",
                    title=f"Архив блога за {y} год — мебельный магазин",
                    h1=f"Архив {y}",
                    body=text(f"arh{y}", 220) + nxt,
                )
        for k, p in enumerate(listing):
            path = p["path"]
            ptitle = _post_title(p)
            w = words
            if p["section"]:
                w = (
                    120
                    if (p["slug"] in ("trendy-2027", "cveta-sezona") and self.on["D32"])
                    else max(words, 320)
                )
            elif self.n >= 3 and p["slug"].startswith("obzor"):
                w = 700
            h1 = ptitle
            if p["slug"] == "kak-vybrat-matras-2" and self.on["D27"]:
                h1 = _post_title({"slug": "kak-vybrat-matras", "section": ""})
            body = text(path, w)
            if p["slug"] == "uhod-za-mebelyu-iz-massiva" and self.on["D29"]:
                body = "<h4>Коротко</h4>" + body
            else:
                body = "<h2>Главное</h2>" + body
            links = ""
            if k == 0 and self.n >= 2:  # old post that linked discontinued products
                links = "".join(f"<a href='/catalog/{d}/'>{d}</a> " for d in DISCONTINUED[:3])
            if self.n >= 2:  # better internal linking: product links from posts
                cat = CATEGORIES[k % len(CATEGORIES)][0]
                links += "".join(
                    f"<a href='/catalog/{cat}/{q['slug']}/'>{_product_name(cat, q['i'])}</a> "
                    for q in self.products[cat][k % 5 : k % 5 + 3]
                )
            if self.n >= 3:
                links += "".join(
                    f"<a href='{q['path']}'>{_post_title(q)}</a> " for q in listing[k + 1 : k + 4]
                )
            desc = ""
            if p["section"] and self.on["D28"]:
                desc = (
                    f"{ptitle}: подробный разбор от дизайнеров мебельного магазина — цвета, материалы, "
                    "сочетания фактур, планировка, освещение и хранение, примеры интерьеров и советы по выбору мебели."
                )
            self.page(
                path,
                title=f"{ptitle} — блог мебельного магазина",
                h1=h1,
                desc=desc or "",
                body=body + (f"<h2>Читайте также</h2><p>{links}</p>" if links else ""),
                jsonld=self.crumbs_ld([("Главная", "/"), ("Блог", "/blog/"), (ptitle, path)]),
            )

    def _info(self) -> None:
        for slug, name in INFO:
            path = f"/{slug}/"
            canonical = None if (slug in [s for s, _ in INFO[:5]] and self.on["D13"]) else "self"
            desc = None if (slug in ("vakansii", "showroom", "otzyvy") and self.on["D24"]) else ""
            robots = "noindex, follow" if (slug == "garantiya" and self.on["D16"]) else ""
            headers = {"X-Robots-Tag": "noindex"} if (slug == "oplata" and self.on["D17"]) else {}
            extra = ""
            if slug == "kontakty" and self.on["D53"]:
                extra = "<script src='http://maps.example.test/api.js'></script>"
            body = text(path, 260)
            if slug == "kontakty":
                body += "<p><a href='/spasibo/'>Оставить заявку</a></p>"
            self.page(
                path,
                title=f"{name} — мебельный магазин в Минске и онлайн",
                h1=name,
                body=body,
                desc=desc,
                canonical=canonical,
                robots=robots,
                headers=headers,
                extra_head=extra,
            )
        spasibo = text("spasibo", 210)
        if not self.on["D35"]:
            spasibo += "<p><a href='/catalog/'>Вернуться в каталог</a></p>"
        doc = (
            f"<!doctype html><html lang='ru'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width'>"
            f"<title>Спасибо за заявку — мебельный магазин свяжется с вами</title>"
            f"<meta name='description' content='Заявка принята. Менеджер мебельного магазина перезвонит в течение рабочего дня и уточнит детали.'>"
            f"<link rel='canonical' href='{self.abs('/spasibo/')}'></head><body><main><h1>Спасибо!</h1>{spasibo}</main></body></html>"
        )
        self.add("/spasibo/", Resp(200, doc.encode()))
        self.page(
            "/aktsii/",
            title="Акции и скидки на мебель — мебельный магазин",
            h1="Акции",
            body=text("aktsii", 240),
        )
        if self.on["D06"]:
            self.add(
                "/aktsii/",
                Resp(500, b"<!doctype html><title>500</title><h1>Internal Server Error</h1>"),
            )

    def _legacy(self) -> None:
        def redir(src: str, dst: str, code: int = 301) -> None:
            self.add(src, Resp(code, headers={"Location": self.abs(dst)}))

        if self.on["D07"]:
            redir("/skidki/", "/rasprodazha/")
            redir("/rasprodazha/", "/sale/")
            redir("/sale/", "/aktsii/")
        if self.on["D08"]:
            redir("/staryy-katalog/", "/staryy-katalog-2/")
            redir("/staryy-katalog-2/", "/staryy-katalog/")
        if self.on["D09"]:
            redir("/dostavka-i-oplata/", "/dostavka/", 302)
        if self.on["D10"]:
            doc = (
                "<!doctype html><html lang='ru'><head><meta charset='utf-8'><title>Архив акций — перенаправление</title>"
                f"<meta http-equiv='refresh' content='0; url={self.abs('/aktsii/')}'></head><body></body></html>"
            )
            self.add("/akcii-arhiv/", Resp(200, doc.encode()))
        if self.on["D39"]:
            self.page(
                "/Catalog/Shkafy/Rasprodazha/",
                title="Распродажа шкафов — скидки до 30% в магазине",
                h1="Распродажа шкафов",
                body=text("rasp", 230),
            )

    def _en(self) -> None:
        self.page(
            "/en/",
            title="Furniture store — kitchens, wardrobes and sofas",
            h1="Furniture store",
            lang="en",
            body="<ul>"
            + "".join(f"<li><a href='/en/catalog/{c}/'>{e}</a></li>" for c, _, e, _ in CATEGORIES)
            + "</ul>"
            + text("en", 230),
            hreflang=(
                f"<link rel='alternate' hreflang='en' href='{self.abs('/en/')}'>"
                f"<link rel='alternate' hreflang='ru' href='{self.abs('/')}'>"
            ),
        )
        for cat, _name, en, _ in CATEGORIES:
            path = f"/en/catalog/{cat}/"
            hl = f"<link rel='alternate' hreflang='en' href='{self.abs(path)}'>"
            if not (cat == "kuhni" and self.on["D42"]):
                hl += f"<link rel='alternate' hreflang='ru' href='{self.abs(f'/catalog/{cat}/')}'>"
            self.page(
                path,
                title=f"{en} — furniture store catalogue online",
                h1=en,
                lang="en",
                body=text("en" + cat, 230),
                hreflang=hl,
                desc=f"{en} from the furniture store: materials, sizes, delivery and assembly across the country.",
            )

    def _robots_sitemaps(self) -> None:
        lines = ["User-agent: *", "Disallow: /cart/", "Disallow: /search"]
        if self.on["D11"]:
            lines.append("Disallow: /blog/tag/")
        if self.on["D12"]:
            lines.append("Disallow: /static/js/")
        lines.append(f"Sitemap: {self.abs('/sitemap.xml')}")
        self.add(
            "/robots.txt",
            Resp(200, ("\n".join(lines) + "\n").encode(), "text/plain; charset=utf-8"),
        )
        lastmod = {1: "2026-06-01", 2: "2026-08-01", 3: "2026-10-01"}[self.n]
        products = [f"/catalog/{c}/{p['slug']}/" for c, *_ in CATEGORIES for p in self.products[c]]
        if self.n == 2:  # D04/D05: deleted products left in the sitemap
            products += [f"/catalog/{d}/" for d in DISCONTINUED]
        pages = (
            ["/", "/catalog/"]
            + [f"/catalog/{c}/" for c, *_ in CATEGORIES]
            + [f"/{s}/" for s, _ in INFO]
        )
        if self.on["D37"]:
            pages += ["/landing/kuhni-na-zakaz/", "/landing/shkafy-kupe/"]
        blog = ["/blog/"] + [p["path"] for p in self.blog]
        maps = {
            "/sitemap-products.xml": products,
            "/sitemap-pages.xml": pages,
            "/sitemap-blog.xml": blog,
        }
        for path, urls in maps.items():
            body = "".join(
                f"<url><loc>{self.abs(u)}</loc><lastmod>{lastmod}</lastmod></url>" for u in urls
            )
            self.add(
                path,
                Resp(
                    200,
                    (
                        "<?xml version='1.0' encoding='UTF-8'?><urlset xmlns="
                        f"'http://www.sitemaps.org/schemas/sitemap/0.9'>{body}</urlset>"
                    ).encode(),
                    "application/xml",
                ),
            )
        idx = "".join(
            f"<sitemap><loc>{self.abs(p)}</loc><lastmod>{lastmod}</lastmod></sitemap>" for p in maps
        )
        self.add(
            "/sitemap.xml",
            Resp(
                200,
                (
                    "<?xml version='1.0' encoding='UTF-8'?><sitemapindex xmlns="
                    f"'http://www.sitemaps.org/schemas/sitemap/0.9'>{idx}</sitemapindex>"
                ).encode(),
                "application/xml",
            ),
        )

    # ---- serving / analysis

    def get(self, path_qs: str) -> Resp:
        resp = self.routes.get(path_qs)
        if resp is not None:
            return resp
        doc = (
            "<!doctype html><html lang='ru'><head><meta charset='utf-8'><title>Страница не найдена — 404</title>"
            "</head><body><h1>404</h1><p>Страница не найдена.</p><a href='/'>На главную</a></body></html>"
        )
        return Resp(404, doc.encode())

    def reachable(self) -> dict[str, int]:
        """BFS from / plus sitemap members: path -> status.

        Mirrors the native crawler with ``resources.fetch`` on: anchors, stylesheets,
        scripts, redirects and sitemap members; images, canonical and hreflang
        targets are not fetched as URLs.
        """
        seen: dict[str, int] = {}
        queue = deque(["/", *self.sitemap_urls()])
        while queue:
            path = queue.popleft()
            if path in seen:
                continue
            resp = self.get(path)
            seen[path] = resp.status
            if 300 <= resp.status < 400:
                queue.append(self._local(resp.headers["Location"]))
                continue
            if not resp.ctype.startswith("text/html") or resp.status != 200:
                continue
            body = resp.body.decode()
            refs = re.findall(r"<a [^>]*href='([^']+)'", body)
            refs += re.findall(r"<link rel='stylesheet' href='([^']+)'", body)
            refs += re.findall(r"<script src='([^']+)'", body)
            for ref in refs:
                local = self._local(urljoin(self.abs(path), ref))
                if local:
                    queue.append(local)
        return dict(sorted(seen.items()))

    def robots_disallowed(self) -> list[str]:
        body = self.routes["/robots.txt"].body.decode()
        return [
            ln.split(":", 1)[1].strip() for ln in body.splitlines() if ln.startswith("Disallow:")
        ]

    def sitemap_urls(self) -> list[str]:
        out = []
        for path in ("/sitemap-products.xml", "/sitemap-pages.xml", "/sitemap-blog.xml"):
            out += [
                self._local(u)
                for u in re.findall(r"<loc>([^<]+)</loc>", self.routes[path].body.decode())
            ]
        return out

    def _local(self, url: str) -> str:
        parts = urlsplit(url)
        if parts.netloc and parts.netloc != urlsplit(self.base).netloc:
            return ""
        return (parts.path or "/") + (f"?{parts.query}" if parts.query else "")


def _post_title(p: dict) -> str:
    base = p["slug"].replace("_", " ").replace("-", " ")
    return base[0].upper() + base[1:]


def _product_name(cat: str, i: int) -> str:
    return f"{SINGULAR[cat]} {ADJ[i % len(ADJ)]} {i:03d}"


def _price(cat: str, i: int) -> int:
    return 300 + _rng("price", cat, str(i)).randint(0, 4000)


def _image(seed: str, kb: int) -> bytes:
    """Deterministic opaque bytes with a JPEG/PNG-looking header; size is what matters."""
    head = b"\xff\xd8\xff\xe0" if kb != 2 else b"\x89PNG\r\n\x1a\n"
    chunk = hashlib.sha256(seed.encode()).digest()
    return head + (chunk * (kb * 1024 // len(chunk) + 1))[: kb * 1024]


# --------------------------------------------------------------------------- ground truth


def ground_truth(base: str = DEFAULT_BASE) -> dict:
    versions = {}
    for v in VERSIONS:
        site = Site(v, base)
        reach = site.reachable()
        per_url: dict[str, dict] = {p: {"status": s, "defects": []} for p, s in reach.items()}
        for d in DEFECTS:
            if not d.active(v):
                continue
            for u in d.urls:
                per_url.setdefault(u, {"status": site.get(u).status, "defects": []})[
                    "defects"
                ].append(d.id)
        blocked = site.robots_disallowed()
        crawled = [
            p
            for p in reach
            if not p.startswith("/static/") and not any(p.startswith(b) for b in blocked)
        ]
        versions[v] = {
            "reachable_urls": len(reach),
            "expected_urls_crawled": len(crawled),
            "html_200": sum(
                1
                for p, s in reach.items()
                if s == 200 and site.get(p).ctype.startswith("text/html")
            ),
            "status_counts": _counts(reach.values()),
            "products": sum(len(x) for x in site.products.values()),
            "blog_posts": len(site.blog),
            "active_defects": [d.id for d in DEFECTS if d.active(v)],
            "urls": per_url,
        }
    lifecycle = {}
    for d in DEFECTS:
        states, prev = [], False
        for v in VERSIONS:
            cur = d.active(v)
            if cur and not prev:
                states.append("introduced" if v == "v1" or "1" not in d.versions else "regressed")
                if (
                    v != "v1"
                    and "1" not in d.versions
                    and not any(d.active(w) for w in VERSIONS[: VERSIONS.index(v)])
                ):
                    states[-1] = "introduced"
            elif cur and prev:
                states.append("persists")
            elif prev and not cur:
                states.append("fixed")
            else:
                states.append("absent")
            prev = cur
        lifecycle[d.id] = {
            "checks": list(d.checks),
            "urls": list(d.urls),
            "note": d.note,
            "detectable_over_http": d.detectable,
            "states": dict(zip(VERSIONS, states, strict=True)),
        }
    return {"format": FORMAT, "base": base, "versions": versions, "defects": lifecycle}


def _counts(values) -> dict[str, int]:
    out: dict[str, int] = {}
    for s in values:
        out[str(s)] = out.get(str(s), 0) + 1
    return dict(sorted(out.items()))


# --------------------------------------------------------------------------- server


def _handler(site: Site):
    class Handler(BaseHTTPRequestHandler):
        server_version = "shop-site/1"
        sys_version = ""

        def do_GET(self):
            self._send(head=False)

        def do_HEAD(self):
            self._send(head=True)

        def _send(self, head: bool) -> None:
            resp = site.get(self.path)
            if resp.delay:
                time.sleep(resp.delay)
            self.send_response(resp.status)
            self.send_header("Content-Type", resp.ctype)
            self.send_header("Content-Length", str(len(resp.body)))
            for k, v in resp.headers.items():
                self.send_header(k, v)
            self.end_headers()
            if not head:
                self.wfile.write(resp.body)

        def log_message(self, *args):  # quiet
            pass

    return Handler


def serve(
    version: str, host: str, port: int, base: str | None, ready_file: str | None, lifetime: float
) -> None:
    if not ipaddress.ip_address(host).is_loopback:
        raise SystemExit("refusing to bind a non-loopback address")
    httpd = ThreadingHTTPServer((host, port), _handler(Site(version, base or DEFAULT_BASE)))
    httpd.daemon_threads = True
    real_port = httpd.server_address[1]
    if ready_file:
        Path(ready_file).write_text(
            json.dumps(
                {
                    "format": FORMAT,
                    "version": version,
                    "pid": os.getpid(),
                    "port": real_port,
                    "base": base or DEFAULT_BASE,
                }
            )
        )
    signal.signal(signal.SIGTERM, lambda *_: threading.Thread(target=httpd.shutdown).start())
    if lifetime > 0:
        timer = threading.Timer(lifetime, httpd.shutdown)
        timer.daemon = True
        timer.start()
    try:
        httpd.serve_forever()
    finally:
        httpd.server_close()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("serve")
    s.add_argument("--version", choices=VERSIONS, required=True)
    s.add_argument("--host", default="127.0.0.1")
    s.add_argument("--port", type=int, default=18431, help="0 = any free port")
    s.add_argument(
        "--base",
        default=None,
        help=f"absolute origin used in canonicals/sitemaps (default {DEFAULT_BASE})",
    )
    s.add_argument("--ready-file")
    s.add_argument(
        "--lifetime", type=float, default=3600.0, help="seconds before auto-shutdown; 0 = forever"
    )
    g = sub.add_parser("ground-truth")
    g.add_argument("--out", default=str(Path(__file__).with_name("ground_truth.json")))
    g.add_argument("--base", default=DEFAULT_BASE)
    u = sub.add_parser("urls")
    u.add_argument("--version", choices=VERSIONS, required=True)
    args = ap.parse_args(argv)
    if args.cmd == "serve":
        serve(args.version, args.host, args.port, args.base, args.ready_file, args.lifetime)
    elif args.cmd == "ground-truth":
        Path(args.out).write_text(
            json.dumps(ground_truth(args.base), ensure_ascii=False, indent=1) + "\n"
        )
    else:
        site = Site(args.version)
        blocked = site.robots_disallowed()
        for path, status in site.reachable().items():
            kind = (
                "resource"
                if path.startswith("/static/")
                else ("robots-blocked" if any(path.startswith(b) for b in blocked) else "page")
            )
            print(status, kind, path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
