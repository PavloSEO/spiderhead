# Parser stack notes (crate `spiderhead-parse`)

## Decision (W2 fill, measured against the oracle)

* Tree: **html5ever tokenizer + a libxml2-style tree builder** (`src/dom/build.rs`), producing a
  `scraper::Html`. libxml2 2.14 (the oracle's lxml) tokenises like HTML5 but builds the tree with
  its legacy rules, so html5ever's own tree builder is not used. The start-tag auto-close pairs and
  the empty-element set are measured from the oracle (the reference suite -> `src/dom/tables.rs`);
  the end-tag priorities, implied head/body, discarded `html`/`head`/`body`, `/>` closing any element
  and BeautifulSoup's text rules (whitespace-only strings collapse, container string classes) are
  replayed in `build.rs`. `tests/dom_tree.rs` compares the whole tree with lxml/bs4
  (the reference suite); 12000+3000 fuzzed documents agree except the known gap below.
* Known gap: in a frameset-only document (no body) libxml2 implies `<body>` before a text chunk that
  starts with a literal stray `<`; the tokenizer cannot tell it from `&lt;`, so this is not modelled.
* Raw scanners (`parser/pyscan.rs`): port of Creference 3.14 `html.parser` (the oracle subclasses it).
* JSON-LD: `parser/pyjson.rs` reproduces `json.loads` errors (positions and texts). `NaN`/`Infinity`
  and floats overflowing to infinity are strings, integers beyond 64 bits are floats, lone surrogate
  escapes become U+FFFD (a Rust `Value` cannot hold them).

## Original notes (kept for reference)

* HTML tree: `scraper` 0.29 (html5ever 0.40, WHATWG HTML5 tree builder + `selectors` CSS engine).
  All modules go through `src/dom.rs` (`Soup = scraper::Html`, `ElementRef`), so the backend can be
  swapped in one file.
* Regex: `fancy-regex` (workspace dep). reference `re` patterns here use lookahead (`_SRCSET_SPLIT`) and
  user-supplied patterns (`custom_extract`, `custom_search`) may use backreferences/lookaround.
* URL: `url` crate wrapped in `urlutil.rs` to match `urllib.parse.urljoin/urlparse` quirks.
* Hashes: `sha1` (boilerplate hash, duplicate content hash), `sha2` (normaliser hashes).
* Unicode: `icu_normalizer` (NFC), `caseless` (`str.casefold`), `unicode-general-category`
  (Cc/Cf/Cs removal, `str.isalpha`-like letter test needs `char::is_alphabetic`).

## Known lxml (libxml2 HTML4 parser) vs html5ever differences to check against the oracle

The oracle builds trees with `BeautifulSoup(html, "lxml")` = libxml2's HTML parser, which is NOT
the HTML5 algorithm. Differences that change parser output:

| Area | lxml / libxml2 | html5ever |
|---|---|---|
| Missing `<html>/<head>/<body>` | wraps, but a stray element before `<head>` can land outside | always builds full skeleton per spec |
| Misplaced head elements (`<title>`/`<meta>`/`<link>` after body content) | stays where written, parent is whatever is open | moved/foster-parented per spec; `document_position` (`*_outside_head`) and `invalid_head_elements` will differ unless they are read from tokens (they are: `_HeadElementScanner` reads the raw tokenizer, port it as a tokenizer pass, not from the tree) |
| `<template>` | no special handling: contents are ordinary children | contents live in a separate fragment, not reachable by child traversal (`scraper` exposes them as children of the template node in 0.29? verify) — `is_inert_template_content` must work on both |
| Nested `<a>`, `<form>`, `<p>` auto-closing | HTML4 rules, differs for `<p>` inside `<table>`, nested forms | spec rules (adoption agency) |
| `<noscript>` in head/body | parsed as elements (scripting off in lxml) | `scraper` parses with scripting disabled: children are elements — matches; verify for `<head><noscript>` |
| `<svg>`/`<math>` | no foreign-content namespace; `<title>` inside svg is just a title tag | namespaced; `FOREIGN_CONTENT` check must look at the ancestor chain, names are the same |
| Character references without `;`, invalid numeric refs | libxml2 table | spec table (windows-1252 remap for 0x80-0x9F) — affects `test_double_escaped_entities` |
| Attribute value whitespace, duplicate attributes | last wins in lxml? | first wins per spec — check `test_*_attribute*` |
| `<br/>` end tags, `</p>` stray | stray `</p>` ignored | stray `</p>` creates empty `<p>` |
| Comments / `<![CDATA[` in HTML | CDATA kept as text | bogus comment |
| Encoding | input is already a decoded `str`; no sniffing | same (`parse_document(&str)`) |
| Entity in `<script>`/`<style>` | raw text | raw text (same) |
| `lxml` `.string` / bs4 `get_text` | joins text nodes | same, but compare whitespace around inline elements |

The reference code also sets `multi_valued_attributes` so `link rel` is NOT tokenised (rel read as one
string); in `scraper` all attributes are plain strings — nothing to do, but `class`-based selectors
should use CSS (`.x`), not split manually.

## Where `lol_html` may be needed

`lol_html` (streaming rewriter, no tree) is only worth pulling in if raw-source scans must see
tokens exactly as a tokenizer emits them without tree repair:
* `invalid_head_elements`, `document_base_url(Raw)`, `markup::_DocumentMarkup` (AMP raw tags),
  `document_charset`/`document_doctype` (regex over a window of raw text — no parser needed).
Start with a tiny hand-written tolerant tag scanner (RCDATA/rawtext aware, like the reference
`_OpaqueTextHTMLParser`); reach for `lol_html` or `html5ever::tokenizer` directly only if the
scanner fails the fixtures. `html5ever`'s tokenizer API (via `scraper`'s dependency) is the first
alternative: no extra crate.

## XPath (custom_extract mode `xpath`, custom_search scope `xpath`)

lxml supports XPath 1.0 over its HTML tree; `scraper` has none. Options, in order of preference:
1. `skyscraper` (HTML parser + XPath 1.0) — try first; only used by those two scopes.
2. `sxd-xpath` over a well-formed re-serialisation — lossy.
3. libxml2 via the `libxml` crate — exact lxml parity incl. HTML4 tree, needs the system library;
   if the oracle diff shows tree differences in CSS paths too, this is the fallback for the whole
   crate (swap `dom.rs`).
Neither is added to `Cargo.toml` yet: the filler of `checks-pack-01-parse` decides and adds the
single workspace dep line. Document unsupported XPath constructs as `Invalid` errors, never panics.

## Budgets without SIGALRM

reference aborts a runaway regex per (document, extractor) with `SIGALRM`. Rust: use
`fancy_regex::RegexBuilder::backtrack_limit` + an `Instant` deadline checked between matches /
nodes; a hit sets `budget_exceeded` exactly as reference does. The wall-clock tests of the reference suite
become limit-based tests.

## Other reference-vs-Rust semantics to keep

* `str.split()` (Unicode whitespace) vs `split_whitespace` — same set; `collapse_whitespace` is in core.
* `round()` is banker's rounding on the binary float (readability, duplicate similarity, fractions).
* `casefold` != `to_lowercase` (ß, final sigma); `isalpha`, `\w` Unicode classes (use `char::is_alphabetic`/`fancy_regex` with Unicode on).
* `json.loads` error messages (`JsonldInvalid.error`) are the reference wording ("Expecting value: line 1 column 1 (char 0)"); needs a hand-made message mapper over `serde_json::Error` line/column plus byte-to-char offsets.
* `urljoin` / `urlparse(...).hostname` differ from WHATWG on malformed hosts, empty schemes, `//`-forms, IPv6 brackets.
