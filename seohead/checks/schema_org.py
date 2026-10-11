"""Validate structured data against Schema.org and Google rich-result requirements.

The validator keeps two layers deliberately separate:

* **Vocabulary validity** asks whether a type exists, whether a property is
  allowed on that type, and whether its value has an expected range. The bundled
  source is the official Schema.org vocabulary compiled into
  ``seohead/data/schemaorg.json`` with 1,010 types and 1,676 properties.
* **Rich-result eligibility** checks locally observable conditions for a named
  Google search feature: required properties present, and one property from any
  documented "at least one of" group. Vocabulary-valid markup may still fail
  these conditions, and passing them is not a guarantee of actual display —
  Google's ranking systems weigh content quality, policy compliance, and manual
  actions that a static markup check cannot see. Eligibility rules do not
  replace vocabulary validation.

Inheritance makes the bundled vocabulary essential. ``Article`` declares only
eight direct properties but inherits 136 through ``Article -> CreativeWork ->
Thing``. A validator that ignores this chain would falsely reject ``author`` on
an article and flood ordinary sites with false positives.

Markup is evaluated as a graph rather than isolated blocks. Entities connect
through ``@id`` references, and an entity stranded outside that graph is a useful
finding rather than a cosmetic detail.
"""

from __future__ import annotations

import json
import math
import re
from datetime import date, datetime, time
from functools import lru_cache
from typing import Any
from urllib.parse import urljoin

from bs4 import BeautifulSoup, Tag

from seohead.checks.parser import is_inert_template_content
from seohead.recon.net import http_client, normalize_url

# Schema.org types documented by Google as their own rich-result feature.
# ``required`` fields gate eligibility; ``required_any`` is an "at least one of"
# group Google asks for alongside ``required``; ``recommended`` fields improve
# completeness without gating it. ``google_feature`` names the Search feature
# being modeled so a verdict can be traced to what it actually checked rather
# than read as a claim about the type in general.
#
# Sub-entities such as Offer, AggregateRating, and Review are deliberately
# absent from this table: Google does not grant them an independent rich
# result, so scoring one against its own required fields (e.g. Offer without
# a parent) produced a verdict about a feature that does not exist. Their
# presence is instead one of the conditions a parent feature (e.g. Product)
# checks for through ``required_any``.
# The offline rich-result rule set lives in ``seohead/data/rich_results_rules.json``
# so it is versioned as data. Its ``version`` stamps every rich-result verdict,
# letting a stored result say which rule set produced it. Bump it when the rules change.


@lru_cache(maxsize=1)
def _rich_rules_file() -> dict[str, Any]:
    from importlib.resources import files

    return json.loads(files("seohead.data").joinpath("rich_results_rules.json").read_text("utf-8"))


RICH_RESULTS_RULES_VERSION: str = _rich_rules_file()["version"]
RICH_RESULTS: dict[str, dict[str, Any]] = _rich_rules_file()["rules"]

# Hosts recognized as Schema.org vocabulary origins, including the ``www`` alias.
_SCHEMA_ORG_HOSTS = {"schema.org", "www.schema.org"}
# Parse full ``@context`` IRIs as http(s)://host(/...). Only the host determines
# vocabulary support; scheme, case, and path suffix do not.
_SCHEMA_ORG_CTX_RE = re.compile(r"^https?://([^/?#]+)", re.IGNORECASE)

MAX_HTML_BYTES = 5_000_000


@lru_cache(maxsize=1)
def load_vocab() -> dict[str, Any]:
    """Load the compiled Schema.org vocabulary bundled with the package.

    Vendoring the snapshot makes audits reproducible rather than dependent on the
    current network response from Schema.org.
    """
    from importlib.resources import files

    raw = files("seohead.data").joinpath("schemaorg.json").read_text("utf-8")
    return json.loads(raw)


def type_chain(name: str, vocab: dict[str, Any]) -> list[str]:
    """Return a type's inheritance chain to the root for property validation."""
    chain, seen, queue = [], set(), [name]
    while queue:
        cur = queue.pop(0)
        if cur in seen or cur not in vocab["types"]:
            continue
        seen.add(cur)
        chain.append(cur)
        queue += vocab["types"][cur].get("sub", [])
    return chain


def _strip(value: Any) -> str:
    """Return a term name without its prefix or vocabulary URL."""
    if not isinstance(value, str):
        return ""
    return value.rsplit("/", 1)[-1].rsplit(":", 1)[-1].strip()


def _types_of(node: dict[str, Any]) -> list[str]:
    raw = node.get("@type")
    if isinstance(raw, str):
        return [_strip(raw)]
    if isinstance(raw, list):
        return [_strip(t) for t in raw if isinstance(t, str)]
    return []


def _is_schema_org_context(value: Any) -> bool:
    """Return whether an ``@context`` value identifies the Schema.org vocabulary.

    Accepts full IRIs such as ``https://schema.org`` and ``http://schema.org``,
    the ``www`` alias, and the short ``schema.org`` form. A path suffix such as
    ``/docs`` does not change the vocabulary origin; only the case-insensitive host
    is authoritative.
    """
    if not isinstance(value, str):
        return False
    s = value.strip()
    if not s:
        return False
    # Short forms: ``schema.org`` or ``www.schema.org`` with an optional slash.
    if s.lower().rstrip("/") in _SCHEMA_ORG_HOSTS:
        return True
    m = _SCHEMA_ORG_CTX_RE.match(s)
    return bool(m) and m.group(1).lower() in _SCHEMA_ORG_HOSTS


def _block_vocab(block: Any) -> tuple[str, bool]:
    """Resolve a JSON-LD block's vocabulary from its top-level ``@context``.

    Returns ``(context, supported)`` for the ``vocabularies`` report. A block with
    no ``@context`` defaults to Schema.org for backward-compatible validation.
    A context array is supported when any string entry identifies Schema.org.
    Expanded object contexts without a string IRI cannot be judged strictly and
    therefore default to supported rather than generating a false rejection.
    """
    if not isinstance(block, dict):
        return ("https://schema.org", True)
    ctx = block.get("@context")
    if ctx is None:
        return ("https://schema.org", True)
    if isinstance(ctx, str):
        return (ctx, _is_schema_org_context(ctx))
    if isinstance(ctx, list):
        strings = [c for c in ctx if isinstance(c, str)]
        if not strings:
            return ("https://schema.org", True)
        for c in strings:
            if _is_schema_org_context(c):
                return (c, True)
        return (strings[0], False)
    return ("https://schema.org", True)


def _extract_blocks(html: str | BeautifulSoup) -> tuple[list[Any], list[str], int]:
    """Extract page JSON-LD.

    Returns the blocks that parsed, one finding per block that did not, and how
    many blocks the markup carries. The third number is the one that answers
    "does this page ship structured data" — the length of the first is a
    different question, and reporting it as the answer told operators that a
    site whose markup is broken on every page has no markup at all.
    """
    blocks, errors = [], []
    found = 0
    soup = html if isinstance(html, BeautifulSoup) else BeautifulSoup(html, features="lxml")
    scripts = (
        tag
        for tag in soup.find_all("script")
        if str(tag.get("type") or "").strip().lower() == "application/ld+json"
        and not is_inert_template_content(tag)
    )
    for i, tag in enumerate(scripts, 1):
        found += 1
        raw = tag.string or tag.get_text() or ""
        text = raw.strip()
        if not text:
            errors.append(f"JSON-LD block #{i} is empty")
            continue
        try:
            blocks.append(json.loads(text))
        except json.JSONDecodeError as exc:
            errors.append(
                f"JSON-LD block #{i} cannot be parsed: {exc.msg} (line {exc.lineno})"
                + json_syntax_hint(text)
            )
    return blocks, errors, found


def _tokens(tag: Tag, name: str) -> list[str]:
    value = tag.get(name) or ""
    return value if isinstance(value, list) else str(value).split()


def _append_property(node: dict[str, Any], name: str, value: Any) -> None:
    if name not in node:
        node[name] = value
    elif isinstance(node[name], list):
        node[name].append(value)
    else:
        node[name] = [node[name], value]


def _html_value(tag: Tag, base: str, *, rdfa: bool = False) -> str:
    if rdfa and tag.has_attr("content"):
        return str(tag["content"])
    if rdfa:
        for attr in ("resource", "href", "src"):
            if tag.has_attr(attr):
                return urljoin(base, str(tag[attr]))
    attr = {
        "meta": "content",
        "audio": "src",
        "embed": "src",
        "iframe": "src",
        "img": "src",
        "source": "src",
        "track": "src",
        "video": "src",
        "a": "href",
        "area": "href",
        "link": "href",
        "object": "data",
        "data": "value",
        "meter": "value",
        "time": "datetime",
    }.get(tag.name)
    if attr and (tag.name != "time" or tag.has_attr(attr)):
        value = str(tag.get(attr) or "")
        return urljoin(base, value) if attr in {"src", "href", "data"} else value
    return tag.get_text(" ", strip=True)


def _extract_microdata(soup: BeautifulSoup, base: str) -> tuple[list[dict[str, Any]], int]:
    """Read HTML item scopes, respecting nested ownership and itemref boundaries."""
    scopes = [
        t for t in soup.find_all(attrs={"itemscope": True}) if not is_inert_template_content(t)
    ]
    positions = {id(t): i for i, t in enumerate(scopes, 1)}
    cache: dict[int, dict[str, Any]] = {}
    contained: set[int] = set()

    def item(scope: Tag, ancestors: set[int]) -> dict[str, Any]:
        key = id(scope)
        if key in ancestors:
            return {
                "@id": urljoin(base, str(scope.get("itemid") or f"_:microdata{positions[key]}"))
            }
        if key in cache:
            return cache[key]
        types = _tokens(scope, "itemtype")
        context = types[0] if types else "https://schema.org"
        node: dict[str, Any] = {
            "@type": types,
            "_format": "microdata",
            "_path": f"microdata[{positions[key]}]",
            "_vocab_context": context,
            "_vocab_supported": all(_is_schema_org_context(t) for t in types),
        }
        cache[key] = node
        if len(ancestors) >= 64:
            node["_markup_errors"] = ["Microdata nesting exceeds the validation depth limit (64)"]
            return node
        if scope.has_attr("itemid"):
            node["@id"] = urljoin(base, str(scope["itemid"]))
        pending = list(reversed(scope.find_all(recursive=False)))
        for ref in reversed(_tokens(scope, "itemref")):
            target = soup.find(id=ref)
            if target is None:
                node.setdefault("_markup_errors", []).append(f"itemref target {ref} is missing")
            else:
                pending.append(target)
        seen = {key}
        while pending:
            tag = pending.pop()
            if id(tag) in seen or is_inert_template_content(tag):
                continue
            seen.add(id(tag))
            props = _tokens(tag, "itemprop")
            if props:
                if tag.has_attr("itemscope"):
                    contained.add(id(tag))
                    value: Any = item(tag, ancestors | {key})
                    if id(tag) in ancestors | {key}:
                        node.setdefault("_markup_errors", []).append("Cyclic itemref relationship")
                else:
                    value = _html_value(tag, base)
                for prop in props:
                    _append_property(
                        node, _strip(prop) if _is_schema_org_context(prop) else prop, value
                    )
            if not tag.has_attr("itemscope"):
                pending.extend(reversed(tag.find_all(recursive=False)))
        return node

    for scope in scopes:
        item(scope, set())
    roots = [cache[id(t)] for t in scopes if id(t) not in contained]
    return roots or ([cache[id(scopes[0])]] if scopes else []), len(scopes)


def _rdfa_term(value: str, vocab: str, prefixes: dict[str, str]) -> tuple[str, bool]:
    value = value.strip("[]")
    if ":" in value and not value.startswith(("http://", "https://")):
        prefix, local = value.split(":", 1)
        value = prefixes.get(prefix, prefix + ":") + local
    elif ":" not in value:
        value = vocab.rstrip("/") + "/" + value if vocab else value
    return (_strip(value), True) if _is_schema_org_context(value) else (value, False)


def _extract_rdfa(soup: BeautifulSoup, base: str) -> tuple[list[dict[str, Any]], int, list[str]]:
    """Extract common RDFa subjects; report advanced constructs as partial coverage.

    This is a Schema.org HTML reader, not a general RDFa triple processor. rel,
    rev and RDF collections are left explicitly unavailable instead of guessing
    their graph semantics. No remote vocabularies or contexts are fetched.
    """
    nodes: list[dict[str, Any]] = []
    by_subject: dict[str, dict[str, Any]] = {}
    reasons: set[str] = set()
    count = 0
    pending = [(soup, "", {"schema": "https://schema.org/"}, None)]
    while pending:
        tag, vocab, prefixes, parent = pending.pop()
        if is_inert_template_content(tag):
            continue
        vocab = str(tag.get("vocab", vocab))
        prefixes = dict(prefixes)
        for prefix, iri in re.findall(r"(\w[\w-]*):\s+(\S+)", str(tag.get("prefix") or "")):
            prefixes[prefix] = iri
        for attr, value in tag.attrs.items():
            if attr.startswith("xmlns:"):
                prefixes[attr[6:]] = str(value)
        types = [_rdfa_term(t, vocab, prefixes) for t in _tokens(tag, "typeof")]
        props = [_rdfa_term(p, vocab, prefixes) for p in _tokens(tag, "property")]
        if not types and parent is None and not any(supported for _, supported in props):
            props = []  # Foreign metadata (for example Open Graph) is not Schema.org evidence.
        current = parent
        if any(tag.has_attr(a) for a in ("rel", "rev", "inlist")) and (types or props or parent):
            reasons.add("RDFa rel/rev/inlist graph semantics are not supported")
        reused = False
        if types:
            count += 1
            subject = tag.get("about")
            if subject is None and (props or parent is None):
                subject = tag.get("resource") or tag.get("href") or tag.get("src")
            subject = urljoin(base, str(subject)) if subject is not None else None
            current = by_subject.get(subject) if subject else None
            reused = current is not None
            if current is None:
                current = {
                    "@type": [],
                    "_format": "rdfa",
                    "_path": f"rdfa[{count}]",
                    "_vocab_context": vocab or "/".join(t for t, _ in types),
                    "_vocab_supported": all(supported for _, supported in types),
                }
                if subject:
                    current["@id"] = subject
                    by_subject[subject] = current
                nodes.append(current)
            current["@type"] = list(dict.fromkeys(current["@type"] + [t for t, _ in types]))
            current["_vocab_supported"] = current["_vocab_supported"] and all(
                supported for _, supported in types
            )
        elif tag.has_attr("about"):
            subject = urljoin(base, str(tag["about"]))
            current = by_subject.get(subject)
            if current is None and props:
                reasons.add("RDFa properties on an untyped subject cannot be validated")
            # about changes the subject even when it has no type declaration.
        elif tag.has_attr("resource") and not props and parent is not None:
            current = by_subject.get(urljoin(base, str(tag["resource"])))
            if current is None:
                reasons.add("RDFa resource subject changes without typeof are not normalized")
        if props:
            owner = parent if types else current
            if owner is None:
                reasons.add("RDFa property has no typed subject")
            else:
                for prop, supported in props:
                    if not supported:
                        owner.setdefault("_markup_warnings", []).append(
                            f"Property {prop} uses an unsupported vocabulary; validation was skipped"
                        )
                        reasons.add("RDFa contains properties from an unsupported vocabulary")
                        continue
                    if types:
                        value = {"@id": current["@id"]} if reused else current
                    else:
                        value = _html_value(tag, base, rdfa=True)
                        if not tag.has_attr("content") and any(
                            tag.has_attr(attr) for attr in ("resource", "href", "src")
                        ):
                            ranges = load_vocab()["properties"].get(prop, {}).get("r", [])
                            if ranges and not _literal_ok(value, ranges, load_vocab()):
                                value = {"@id": value}
                    if tag.has_attr("datatype") and str(tag["datatype"]):
                        reasons.add("RDFa explicit datatypes are not normalized")
                    _append_property(owner, prop, value)
        pending.extend(
            (child, vocab, prefixes, current) for child in reversed(tag.find_all(recursive=False))
        )
    # Nodes linked by property containment must not also be emitted as roots.
    contained = {
        id(v)
        for n in nodes
        for k, value in n.items()
        if not k.startswith(("@", "_"))
        for v in (value if isinstance(value, list) else [value])
        if isinstance(v, dict)
    }
    return [n for n in nodes if id(n) not in contained], count, sorted(reasons)


# JSON has no comments and permits no trailing comma. Both appear constantly in
# templates filled in by hand, and both void the whole block, so naming the
# cause turns a parse error into a one-line fix.
_JSON_COMMENT_RE = re.compile(r"/\*.*?\*/|(?<![:\w])//[^\n]*", re.DOTALL)
_TRAILING_COMMA_RE = re.compile(r",\s*[}\]]")


def json_syntax_hint(text: str) -> str:
    """Name the two syntax mistakes that most often void a JSON-LD block."""
    causes = []
    if _JSON_COMMENT_RE.search(text):
        causes.append("JSON permits no comments")
    if _TRAILING_COMMA_RE.search(text):
        causes.append("JSON permits no trailing comma")
    return f" — {'; '.join(causes)}" if causes else ""


def _flatten(
    payload: Any,
    out: list[dict[str, Any]],
    path: str = "",
    contained: bool = False,
    parent_key: str | None = None,
    depth: int = 0,
) -> None:
    """Flatten any JSON-LD payload while preserving each node's source path.

    ``contained`` marks a node reached by descending into a parent's property
    value (an inline Offer inside a Product, say) rather than sitting beside
    other entities at the top of a block or an ``@graph`` array. A contained
    node is already connected to its parent by nesting; graph analysis must
    not also demand it carry an ``@id`` link, or ordinary anonymous markup
    reads as a disconnected island.

    ``parent_key`` carries the enclosing node's identity (its ``@id``, or a
    synthetic path-based key when it has none) so ``_graph_shape`` can union a
    contained node with its parent even when several levels of anonymous
    nesting sit between them and an ``@id`` further down.
    """
    if isinstance(payload, list):
        for i, item in enumerate(payload):
            _flatten(item, out, f"{path}[{i}]", contained, parent_key, depth + 1)
        return
    if not isinstance(payload, dict):
        return
    if "@graph" in payload:
        _flatten(payload["@graph"], out, f"{path}@graph", contained, parent_key, depth + 1)
        # An @graph wrapper may have properties, but it is not treated as an entity.
        return
    # A pure {"@id": ...} object is a reference, not an entity definition. Treating
    # it as a node would suppress dangling-reference detection by self-definition.
    if set(payload) <= {"@id"}:
        return
    node = dict(payload)
    node_path = node.get("_path") or path or "@root"
    node["_path"] = node_path
    node["_contained"] = contained
    node_key = node.get("@id") or f"_path:{node_path}"
    node["_key"] = node_key
    node["_parent_key"] = parent_key if contained else None
    out.append(node)
    if depth >= 64:
        node["_markup_errors"] = [
            *(node.get("_markup_errors") or []),
            "Structured-data nesting exceeds the validation depth limit (64)",
        ]
        return
    for key, value in payload.items():
        if key.startswith("@"):
            continue
        _flatten(value, out, f"{path}.{key}" if path else key, True, node_key, depth + 1)


def _literal_ok(value: Any, ranges: list[str], vocab: dict[str, Any]) -> bool:
    """Return whether a literal is valid for the vocabulary's expected ranges."""
    if not ranges:
        return True  # A property without rangeIncludes provides no range to enforce.
    for r in ranges:
        if any(vocab["types"][t].get("datatype") for t in type_chain(r, vocab)):
            try:
                if r in {"Number", "Float", "Integer"}:
                    number = float(value)
                    if (
                        not isinstance(value, bool)
                        and math.isfinite(number)
                        and (r != "Integer" or number.is_integer())
                    ):
                        return True
                elif r == "Boolean":
                    if isinstance(value, bool) or value in ("true", "false", "True", "False"):
                        return True
                elif r == "Date":
                    date.fromisoformat(str(value))
                    return True
                elif r == "DateTime":
                    if "T" in str(value) or " " in str(value):
                        datetime.fromisoformat(str(value).replace("Z", "+00:00"))
                        return True
                elif r == "Time":
                    time.fromisoformat(str(value))
                    return True
                elif value is not None:
                    return True
            except (ValueError, TypeError, OverflowError):
                pass
        # Enumeration values are commonly expressed as URL strings and are valid.
        if "Enumeration" in type_chain(r, vocab):
            return True
    return False


def _check_node(node: dict[str, Any], vocab: dict[str, Any], known_ids: set[str]) -> dict[str, Any]:
    """Validate one graph node against the bundled vocabulary."""
    errors: list[str] = list(node.get("_markup_errors") or [])
    warnings: list[str] = list(node.get("_markup_warnings") or [])
    types = _types_of(node)
    path = node.get("_path", "?")

    # Do not validate nodes from unsupported vocabularies as Schema.org. A foreign
    # vocabulary with a coincidentally identical type name could otherwise produce
    # either a false missing-type error or false approval.
    if not node.get("_vocab_supported", True):
        label = node.get("_vocab_context") or "unknown vocabulary"
        warnings.append(
            f"Node uses unsupported vocabulary {label}; Schema.org validation was skipped"
        )
        return {
            "path": path,
            "types": types,
            "id": node.get("@id"),
            "format": node.get("_format", "jsonld"),
            "errors": errors,
            "warnings": warnings,
        }

    if not types:
        attribute = {"microdata": "itemtype", "rdfa": "typeof"}.get(node.get("_format"), "@type")
        errors.append(
            f"Missing {attribute}; the entity cannot be validated and may be ignored by search engines"
        )
    chain: list[str] = []
    for t in types:
        if t not in vocab["types"]:
            errors.append(f"Type {t} is not present in the Schema.org vocabulary")
            continue
        entry = vocab["types"][t]
        if entry.get("dead"):
            warnings.append(f"Type {t} is superseded by {', '.join(entry['dead'])}")
        if entry.get("layer") == "pending":
            warnings.append(f"Type {t} is in Schema.org's pending layer and is not yet stable")
        chain += type_chain(t, vocab)
    chain_set = set(chain)

    for key, value in node.items():
        if key.startswith("@") or key.startswith("_"):
            continue
        prop = vocab["properties"].get(key)
        if prop is None:
            errors.append(f"Property {key} is not present in the Schema.org vocabulary")
            continue
        if prop.get("dead"):
            warnings.append(f"Property {key} is superseded by {', '.join(prop['dead'])}")
        if prop.get("layer") == "pending":
            warnings.append(f"Property {key} is in Schema.org's pending layer")
        # A property is valid when declared on any type in the inheritance chain.
        if chain_set and prop["d"] and not (set(prop["d"]) & chain_set):
            errors.append(
                f"Property {key} is not declared for {'/'.join(types)} "
                f"(allowed domains include {', '.join(prop['d'][:4])})"
            )
            continue
        # Validate property value types.
        values = value if isinstance(value, list) else [value]
        for v in values:
            if isinstance(v, dict):
                sub = _types_of(v)
                if sub and prop["r"]:
                    ok = any(set(type_chain(s, vocab)) & set(prop["r"]) for s in sub)
                    if not ok:
                        warnings.append(
                            f"{key}: nested type {'/'.join(sub)} does not match expected "
                            f"ranges {', '.join(prop['r'][:4])}"
                        )
                if not sub and "@id" in v and _strip(v["@id"]) and v["@id"] not in known_ids:
                    errors.append(f"{key}: @id reference {v['@id']} has no entity in the graph")
            elif not _literal_ok(v, prop["r"], vocab):
                if any(
                    vocab["types"][t].get("datatype")
                    for r in prop["r"]
                    for t in type_chain(r, vocab)
                ):
                    warnings.append(
                        f"{key}: literal value does not match expected ranges "
                        f"({', '.join(prop['r'][:3])})"
                    )
                else:
                    warnings.append(
                        f"{key}: literal value supplied where an object is expected "
                        f"({', '.join(prop['r'][:3])}); Google may accept it, but the "
                        "entity relationship is lost"
                    )

    return {
        "path": path,
        "types": types,
        "id": node.get("@id"),
        "format": node.get("_format", "jsonld"),
        "errors": errors,
        "warnings": warnings,
    }


def _rich_results(nodes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Report rich-result candidates against locally verifiable requirements.

    ``eligible`` reflects only the structural conditions this validator can
    check for the named ``google_feature``: every ``required`` property present,
    plus one property from ``required_any`` when that group exists. It is not a
    guarantee that Google will display the rich result — ranking systems apply
    content-quality, policy, and manual-action signals a static markup check
    cannot observe. See ``_findings`` for the disclaimer surfaced with a true
    verdict.
    """
    out = []
    for node in nodes:
        if not node.get("_vocab_supported", True):
            continue
        for t in _types_of(node):
            spec = RICH_RESULTS.get(t)
            if not spec:
                continue
            present = {
                k
                for k, value in node.items()
                if not k.startswith(("@", "_"))
                and value is not None
                and value != []
                and value != {}
                and (not isinstance(value, str) or value.strip())
            }
            missing_req = [f for f in spec["required"] if f not in present]
            required_any = spec.get("required_any", [])
            missing_any = required_any if required_any and not (present & set(required_any)) else []
            entry = {
                "type": t,
                "google_feature": spec.get("google_feature", t),
                "eligible": not missing_req and not missing_any,
                "missing_required": missing_req,
                "missing_required_any_of": missing_any,
                "missing_recommended": [f for f in spec["recommended"] if f not in present],
                "rules_version": RICH_RESULTS_RULES_VERSION,
                "format": node.get("_format", "jsonld"),
                "path": node.get("_path", "@root"),
            }
            if spec.get("deprecated_for_rich"):
                entry["note"] = (
                    "Google no longer shows a rich result for this type, "
                    "but the markup remains useful for AI content extraction"
                )
            out.append(entry)
    return out


class _UnionFind:
    """Minimal union-find over string keys, used to find connected components."""

    def __init__(self) -> None:
        self._parent: dict[str, str] = {}

    def add(self, x: str) -> None:
        self._parent.setdefault(x, x)

    def find(self, x: str) -> str:
        self.add(x)
        root = x
        while self._parent[root] != root:
            root = self._parent[root]
        while self._parent[x] != root:
            self._parent[x], x = root, self._parent[x]
        return root

    def union(self, a: str, b: str) -> None:
        ra, rb = self.find(a), self.find(b)
        if ra != rb:
            self._parent[ra] = rb


def _graph_shape(nodes: list[dict[str, Any]]) -> dict[str, Any]:
    """Measure graph connectivity and identify entities disconnected from the rest.

    An identity edge is drawn from a node to *any* property value that is a
    node-object carrying an ``@id`` — a typed inline entity that reuses a
    top-level identifier is linked exactly like a pure ``{"@id": ...}``
    reference, since JSON-LD merges both forms by identity. A containment edge
    also unions every node with its enclosing parent, so a node reached only
    by descending through anonymous nesting (``_contained``) still carries the
    identity edges found deeper inside it up to its top-level ancestor.

    Islands are then top-level entities whose connected component contains no
    other top-level entity — connectivity, not a redundant ``@id`` pointing
    back at each of them individually. A lone entity, or a Product with a
    nested Offer, has nothing to be disconnected from in the first place.
    """
    with_id = [n for n in nodes if n.get("@id")]
    uf = _UnionFind()
    referenced: set[str] = set()
    for n in nodes:
        key = n["_key"]
        uf.add(key)
        parent_key = n.get("_parent_key")
        if parent_key:
            uf.union(key, parent_key)
        for prop, value in n.items():
            if prop.startswith("@") or prop.startswith("_"):
                continue
            for v in value if isinstance(value, list) else [value]:
                if isinstance(v, dict) and v.get("@id"):
                    referenced.add(v["@id"])
                    uf.union(key, v["@id"])

    candidates = [n for n in nodes if not n.get("_contained")]
    # Island status only means something when two or more top-level entities
    # exist to be connected in the first place.
    islands: list[str] = []
    if len(candidates) > 1:
        components: dict[str, list[dict[str, Any]]] = {}
        for n in candidates:
            components.setdefault(uf.find(n["_key"]), []).append(n)
        for members in components.values():
            if len(members) == 1:
                n = members[0]
                islands.append(n.get("@id") or "/".join(_types_of(n)) or "?")

    return {
        "nodes": len(nodes),
        "with_id": len(with_id),
        "linked_by_id": len(referenced),
        "top_level": len(candidates),
        "islands": islands[:20],
        "is_graph": len(referenced) > 0,
    }


def check_schema(
    url: str | None = None, html: str | None = None, timeout: float = 25.0
) -> dict[str, Any]:
    """Validate one page's vocabulary, entity graph, and rich-result readiness."""
    result: dict[str, Any] = {"ok": True}
    if html is None:
        target = normalize_url(url or "")
        if not target:
            return {"ok": False, "error": f"Not a valid HTTP(S) URL: {url!r}"}
        try:
            client, _ = http_client(timeout)
        except ImportError:
            return {"ok": False, "error": "httpx is required"}
        try:
            with client:
                resp = client.get(target)
                html = resp.text[:MAX_HTML_BYTES]
        except Exception as exc:
            return {"ok": False, "url": target, "error": str(exc)}
        result.update(url=target, final_url=str(resp.url), status_code=resp.status_code)

    vocab = load_vocab()
    soup = BeautifulSoup(html, features="lxml")
    blocks, parse_errors, blocks_found = _extract_blocks(soup)

    nodes: list[dict[str, Any]] = []
    # Record each JSON-LD block's declared vocabulary. Resolve context once per
    # block and attach it to every flattened node, including nested and @graph
    # entities. Validation is deliberately block-scoped rather than propagating
    # nested contexts through the flattened representation.
    vocabularies: list[dict[str, Any]] = []
    _vocab_pos: dict[tuple[str, bool], int] = {}
    for block_index, block in enumerate(blocks, 1):
        context, supported = _block_vocab(block)
        before = len(nodes)
        _flatten(block, nodes, f"jsonld[{block_index}]")
        for n in nodes[before:]:
            n["_vocab_context"] = context
            n["_vocab_supported"] = supported
            n["_format"] = "jsonld"
        key = (context, supported)
        pos = _vocab_pos.get(key)
        if pos is None:
            _vocab_pos[key] = len(vocabularies)
            vocabularies.append({"context": context, "supported": supported, "blocks": 1})
        else:
            vocabularies[pos]["blocks"] += 1
    base = result.get("final_url") or url or ""
    base_tag = soup.find("base", href=True)
    if base_tag:
        base = urljoin(base, str(base_tag["href"]))
    microdata, microdata_count = _extract_microdata(soup, base)
    rdfa, rdfa_count, rdfa_reasons = _extract_rdfa(soup, base)
    for entity in microdata + rdfa:
        _flatten(entity, nodes, entity["_path"])
    for node in nodes:
        if node.get("_format") == "jsonld":
            continue
        key = (node.get("_vocab_context", ""), node.get("_vocab_supported", True))
        if key not in _vocab_pos:
            _vocab_pos[key] = len(vocabularies)
            vocabularies.append({"context": key[0], "supported": key[1], "blocks": 0})
    known_ids = {n["@id"] for n in nodes if isinstance(n.get("@id"), str)}

    entities = [_check_node(n, vocab, known_ids) for n in nodes]
    result.update(
        vocabulary={
            "fetched": vocab["fetched"],
            "types": len(vocab["types"]),
            "properties": len(vocab["properties"]),
        },
        blocks=len(blocks),
        blocks_found=blocks_found,
        blocks_invalid=blocks_found - len(blocks),
        vocabularies=vocabularies,
        parse_errors=parse_errors,
        graph=_graph_shape(nodes),
        entities=entities,
        rich_results=_rich_results(nodes),
        rules_version=RICH_RESULTS_RULES_VERSION,
        other_markup={
            "microdata": bool(microdata_count),
            "rdfa": bool(rdfa_count or rdfa_reasons),
        },
        markup_counts={"microdata": microdata_count, "rdfa": rdfa_count},
        markup_coverage=[
            {"format": "microdata", "status": "checked" if microdata_count else "absent"},
            {
                "format": "rdfa",
                "status": "partial" if rdfa_reasons else "checked" if rdfa_count else "absent",
                "reasons": rdfa_reasons,
            },
        ],
    )
    result["findings"] = _findings(result)
    return result


def _findings(r: dict[str, Any]) -> list[str]:
    out: list[str] = []
    status = r.get("status_code")
    if status is not None and not 200 <= status < 300:
        # Everything below describes the body that was actually served. Saying
        # so first stops the report being read as a verdict on the requested URL.
        out.append(
            f"The page returned HTTP {status}: what follows describes the error page, "
            "not the requested URL"
        )
    out += r["parse_errors"]

    for coverage in r.get("markup_coverage", []):
        for reason in coverage.get("reasons", []):
            out.append(f"{coverage['format']} validation coverage is partial: {reason}")

    if not r["blocks"]:
        # "No blocks" and "block #1 cannot be parsed" cannot both be true, and
        # the second is the fact: the page does ship structured data, and search
        # engines discard all of it.
        if r["blocks_found"]:
            out.append(
                f"All {r['blocks_found']} JSON-LD block(s) on the page are invalid, so every "
                "search engine discards the structured data this page believes it publishes"
            )
        else:
            out.append("The page contains no JSON-LD blocks")
        if not r["entities"]:
            return out

    errors = sum(len(e["errors"]) for e in r["entities"])
    warnings = sum(len(e["warnings"]) for e in r["entities"])
    if errors:
        out.append(f"Vocabulary errors: {errors}")
    if warnings:
        out.append(f"Vocabulary warnings: {warnings}")

    g = r["graph"]
    # Connectivity only becomes a question once two or more top-level entities
    # compete for it; a contained node (an inline Offer inside a Product) is
    # already connected by nesting, and a lone top-level entity has no peer to
    # be disconnected from, so neither is judged here.
    if g["top_level"] > 1 and not g["is_graph"]:
        out.append(
            f"{g['top_level']} top-level entities are marked up, but none are linked "
            "through @id; this is a set of isolated blocks rather than a graph"
        )
    elif g["islands"]:
        out.append(
            f"Entities outside the connected graph: {len(g['islands'])} "
            f"({', '.join(str(i) for i in g['islands'][:3])})"
        )

    for rr in r["rich_results"]:
        if rr["missing_required"]:
            out.append(
                f"{rr['type']}: missing required rich-result fields: "
                f"{', '.join(rr['missing_required'])}"
            )
        if rr.get("missing_required_any_of"):
            out.append(
                f"{rr['type']}: {rr['google_feature']} needs at least one of "
                f"{', '.join(rr['missing_required_any_of'])}"
            )
        if rr.get("note"):
            out.append(f"{rr['type']}: {rr['note']}")

    # A structural pass cannot see what Google's ranking systems weigh — content
    # quality, policy compliance, or a manual action — so an "eligible" verdict
    # here is never a promise that the rich result will actually appear.
    if any(rr["eligible"] for rr in r["rich_results"]):
        out.append(
            "Rich-result checks above verify only locally observable structure "
            "(required properties present for a type Google documents); they are not "
            "a guarantee that Google will display a rich result for this page"
        )
    return out
