---
name: site-to-markdown
description: >-
  Exports a website's pages as Markdown files — one .md per URL with frontmatter (title,
  description, URL, status, date) and clean content-area text (nav/footer stripped). Runs the
  toolkit's markdown-export over a sitemap (site root or sitemap URL; indexes are followed), a
  URL list file, or a single URL, into an output folder the caller names. Use it to hand a whole
  site to a model or a translation batch, to snapshot content before a redesign, or to build the
  Markdown mirrors an AI-ready site serves for Accept: text/markdown. Triggers on: "site to
  markdown", "export pages as markdown", "markdown export", "all pages as .md files", "prepare
  the site content for a model", "markdown mirror of the site", "custom extraction to
  markdown", "slice the site into md files".
---

# Site → Markdown export

One command does the work; this skill is about choosing the input, the bounds, and the output
location — and reporting honestly what landed on disk.

```bash
~/Work/tools/seotools/.venv/bin/seohead markdown-export --input '{"<input>": "<value>", "out_dir": "<dir>", "max_urls": <n>}'
```

Inputs — exactly one:

- `sitemap`: a sitemap or sitemap-index URL; a bare site root works too (robots.txt `Sitemap:`
  directives are read, then the conventional `/sitemap.xml`).
- `urls_file`: path to a TXT with one URL per line (`#` comments allowed).
- `url`: a single page.

## When which bound

- Default `max_urls` is 200 — always raise or lower it deliberately and say so in the report.
  For a full site export, check the sitemap URL count first (`seohead sitemap-crawl` or the
  sitemap itself) and set `max_urls` above it, or the run finishes `truncated: true`.
- `delay` (seconds between requests) defaults to 0.5 — keep it for someone else's site; raise
  it for small hosts. The summary's `sitemap_errors` and `failures` lists are part of the
  result: report them, never hide a partial export behind the total.
- Output path: `Work/outputs/<project>/markdown-YYYY-MM-DD/` for one-off analysis, or the
  site repository's `public/markdown/` when the export feeds real Markdown mirrors.

## Output contract (what you get)

- `<out_dir>/<url-path>.md`; the home page is `index.md`; locale/section prefixes become
  folders (`/it/guide/x` → `it/guide/x.md`).
- Frontmatter: `title`, `description`, `url` (final after redirects), `status`, `exported`.
- Body: the same content-area Markdown `markdown-extract` produces — headings, lists and links
  kept, nav/footer dropped, single lowercase icon-ligature lines removed.

## After the run

1. Read the summary JSON: `written`, `failed`, `bytes`, `truncated`. If `truncated` — rerun
   with a higher `max_urls` or say plainly that the export is partial and by how much.
2. Spot-check two files (one deep page, the home page): frontmatter present, body starts at
   real content.
3. Report: N files, M failures with reasons, total size, output path, and the command used.

## Feeding real Markdown mirrors (AI-ready sites)

When the export becomes the site's own `Accept: text/markdown` mirrors: keep the file set in
git, regenerate per content-wave (not per deploy), and remember the negotiation itself is an
nginx/Astro concern — the files here are just the source.
