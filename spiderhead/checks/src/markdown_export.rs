//! Markdown export: write fetched pages as frontmatter'd Markdown files.
//!
//! A site's sitemap, a URL list or one page becomes `<out_dir>/<url-path>.md`
//! (the home page → `index.md`) with a small frontmatter header above the same
//! content-area Markdown the extractor produces. Up to 20 failures are listed
//! with reasons, so a partial export stays visible.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use spiderhead_parse::markdown_extract::extract_markdown;
use spiderhead_parse::parser::{fetch_html, FetchOutcome, HtmlFetcher};

/// Reject anything but exactly one source: Python's `if s` treats an empty
/// string as absent.
pub fn validate_sources(
    url: Option<&str>,
    urls_file: Option<&str>,
    sitemap: Option<&str>,
) -> Result<(), String> {
    let sources = [url, urls_file, sitemap]
        .into_iter()
        .filter(|source| source.is_some_and(|value| !value.is_empty()))
        .count();
    if sources != 1 {
        return Err("exactly one of url, urls_file, sitemap is required".to_owned());
    }
    Ok(())
}

/// URLs from a urls_file: one per line, blank lines and `#` comments skipped.
pub fn read_urls_file(path: &str) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

/// Absolute http(s) URLs in first-appearance order, duplicates dropped.
pub fn dedupe_http(targets: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for target in targets {
        if (target.starts_with("http://") || target.starts_with("https://"))
            && seen.insert(target.clone())
        {
            ordered.push(target.clone());
        }
    }
    ordered
}

/// The page's title and meta description, whitespace-collapsed, quotes straightened.
pub fn meta(html: &str) -> (String, String) {
    let clean = |value: Option<&str>| -> String {
        let value = value.unwrap_or_default().replace('"', "'");
        value.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let title = regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>")
        .ok()
        .and_then(|re| re.captures(html))
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str());
    let description = description_from(html);
    (clean(title), clean(description))
}

/// The meta description's content, in either attribute order.
fn description_from(html: &str) -> Option<&str> {
    let name_first =
        regex::Regex::new(r#"(?i)<meta[^>]+name=["']description["'][^>]+content=["']([^"']*)"#)
            .ok()?;
    if let Some(caps) = name_first.captures(html) {
        return caps.get(1).map(|m| m.as_str());
    }
    let content_first =
        regex::Regex::new(r#"(?i)<meta[^>]+content=["']([^"']*)["'][^>]+name=["']description["']"#)
            .ok()?;
    content_first
        .captures(html)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())
}

/// The output file path for a page URL: sanitized path segments, `.md` suffix.
pub fn relpath(page_url: &str) -> PathBuf {
    let path = spiderhead_parse::py_url::urlsplit(page_url)
        .map(|parts| parts.path)
        .unwrap_or_default();
    let path = path.trim_matches('/').trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let mut rel = PathBuf::new();
    if path.is_empty() {
        rel.push("index");
    } else {
        for segment in path.split('/') {
            let sanitized = sanitize_segment(segment);
            rel.push(&sanitized);
        }
    }
    if rel.extension().is_none_or(|ext| ext != "md") {
        rel.set_extension("md");
    }
    rel
}

/// One path segment: runs of non-URL-safe characters collapse to one `_`
/// (the regex has a `+` quantifier), an emptied segment becomes `_`.
fn sanitize_segment(segment: &str) -> String {
    let mut sanitized = String::with_capacity(segment.len());
    let mut in_run = false;
    for c in segment.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-') {
            sanitized.push(c);
            in_run = false;
        } else if !in_run {
            sanitized.push('_');
            in_run = true;
        }
    }
    if sanitized.is_empty() {
        "_".to_owned()
    } else {
        sanitized
    }
}

/// Drop single lowercase token lines: icon ligature names, not prose.
fn drop_icon_lines(body: &str) -> String {
    let is_icon = |line: &str| {
        let trimmed = line.trim();
        (2..=24).contains(&trimmed.len())
            && !trimmed.is_empty()
            && trimmed.chars().all(|c| c.is_ascii_lowercase() || c == '_')
    };
    body.lines()
        .filter(|line| !is_icon(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Export `targets` into `out_dir`, fetching through `fetcher`.
/// `exported` is the frontmatter date (UTC today on the live surface).
pub fn run(
    targets: &[String],
    fetcher: &dyn HtmlFetcher,
    out_dir: &Path,
    max_urls: usize,
    delay: f64,
    exported: &str,
    sitemap_errors: Vec<Value>,
) -> Result<Value, String> {
    let ordered = dedupe_http(targets);
    let truncated = ordered.len() > max_urls;
    let ordered: Vec<String> = ordered.into_iter().take(max_urls).collect();

    std::fs::create_dir_all(out_dir).map_err(|error| error.to_string())?;
    // Python reports `root.resolve()`: the absolute path after mkdir.
    let resolved = std::fs::canonicalize(out_dir).unwrap_or_else(|_| out_dir.to_path_buf());

    let mut written = 0usize;
    let mut total_bytes = 0u64;
    let mut files: Vec<String> = Vec::new();
    let mut failures: Vec<Value> = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for (index, target) in ordered.iter().enumerate() {
        if index > 0 && delay > 0.0 {
            thread::sleep(Duration::from_secs_f64(delay));
        }
        let fetched = fetch_html(fetcher, target, None);
        let (html, final_url, status_code) = match &fetched {
            FetchOutcome::Completed {
                status_code,
                final_url,
                html,
                ..
            } if (200..300).contains(status_code) => {
                (html.clone(), final_url.clone(), *status_code)
            }
            FetchOutcome::Completed { status_code, .. } => {
                failures.push(json!({
                    "url": target,
                    "error": format!("HTTP {status_code}"),
                }));
                continue;
            }
            FetchOutcome::Failed { error, .. } => {
                failures.push(json!({"url": target, "error": error}));
                continue;
            }
        };
        let (title, description) = meta(&html);
        let body = match extract_markdown(&html, None) {
            Ok(rendered) => rendered.content_markdown,
            Err(error) => {
                failures.push(json!({"url": target, "error": error.to_string()}));
                continue;
            }
        };
        let source = if final_url.is_empty() {
            target
        } else {
            &final_url
        };
        let mut rel = relpath(source);
        let mut n = 2;
        while !used.insert(rel.to_string_lossy().into_owned()) {
            let stem = rel.file_stem().map(|s| s.to_string_lossy().into_owned());
            let stem = stem.unwrap_or_else(|| "page".to_owned());
            rel = rel.with_file_name(format!("{stem}-{n}.md"));
            n += 1;
        }
        let dest = out_dir.join(&rel);
        if let Some(parent) = dest.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                failures.push(json!({"url": target, "error": error.to_string()}));
                continue;
            }
        }
        let frontmatter = format!(
            "---\ntitle: \"{title}\"\ndescription: \"{description}\"\nurl: {source}\nstatus: {status_code}\nexported: {exported}\n---\n\n"
        );
        let document = format!("{}{}\n", frontmatter, drop_icon_lines(&body).trim_end());
        let mut file = match std::fs::File::create(&dest) {
            Ok(file) => file,
            Err(error) => {
                failures.push(json!({"url": target, "error": error.to_string()}));
                continue;
            }
        };
        if let Err(error) = file.write_all(document.as_bytes()) {
            failures.push(json!({"url": target, "error": error.to_string()}));
            continue;
        }
        let bytes = dest.metadata().map(|m| m.len()).unwrap_or(0);
        written += 1;
        total_bytes += bytes;
        files.push(rel.to_string_lossy().into_owned());
    }
    Ok(json!({
        "ok": written > 0,
        "out_dir": resolved.to_string_lossy(),
        "requested": ordered.len(),
        "written": written,
        "failed": failures.len(),
        "bytes": total_bytes,
        "files": files.into_iter().take(50).collect::<Vec<_>>(),
        "failures": failures.into_iter().take(20).collect::<Vec<_>>(),
        "sitemap_errors": sitemap_errors.into_iter().take(10).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

/// The candidate URLs of a sitemap expansion result.
pub fn sitemap_targets(expanded: &Value) -> Vec<String> {
    expanded["urls"]
        .as_array()
        .map(|urls| {
            urls.iter()
                .filter_map(|entry| entry["loc"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The sitemap expansion's error records.
pub fn sitemap_errors(expanded: &Value) -> Vec<Value> {
    expanded["errors"]
        .as_array()
        .cloned()
        .unwrap_or_else(Vec::new)
}

/// The handler's keyword arguments for one command, dropping unset keys.
pub fn expanduser(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MockFetcher {
        /// (request URL, final URL, status, body)
        pages: RefCell<Vec<(String, String, u16, String)>>,
    }

    impl MockFetcher {
        fn new(pages: Vec<(String, String, u16, String)>) -> Self {
            MockFetcher {
                pages: RefCell::new(pages),
            }
        }

        fn simple(pages: Vec<(String, u16, String)>) -> Self {
            Self::new(
                pages
                    .into_iter()
                    .map(|(url, status, text)| (url.clone(), url, status, text))
                    .collect(),
            )
        }
    }

    impl HtmlFetcher for MockFetcher {
        fn get(
            &self,
            url: &str,
            _headers: &[(&str, &str)],
            _timeout: f64,
            _max_redirects: u32,
        ) -> Result<spiderhead_parse::parser::FetchResponse, String> {
            let mut pages = self.pages.borrow_mut();
            let index = pages.iter().position(|(candidate, ..)| candidate == url);
            match index {
                Some(index) => {
                    let (request, final_url, status, text) = pages.remove(index);
                    Ok(spiderhead_parse::parser::FetchResponse {
                        final_url: if final_url.is_empty() {
                            request
                        } else {
                            final_url
                        },
                        status_code: status,
                        text,
                        headers: Vec::new(),
                    })
                }
                None => Err("connection refused".to_owned()),
            }
        }
    }

    fn page(title: &str) -> String {
        format!(
            "<html><head><title>{title}</title><meta name=\"description\" content=\"About us\">\
             </head><body><nav>Menu</nav><main><h1>{title}</h1><p>Hello world content.</p></main>\
             <footer>Footer</footer></body></html>"
        )
    }

    // from tests/test_live_content_interfaces.py::markdown_export writes one file per URL
    #[test]
    fn writes_files_with_frontmatter_and_reports_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fetcher = MockFetcher::simple(vec![("http://x.test/".to_owned(), 200, page("Home"))]);
        let result = run(
            &["http://x.test/".to_owned()],
            &fetcher,
            dir.path(),
            200,
            0.0,
            "2026-10-29",
            Vec::new(),
        )
        .expect("run");
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["written"], json!(1));
        assert_eq!(result["failed"], json!(0));
        assert_eq!(result["files"], json!(["index.md"]));
        let text = std::fs::read_to_string(dir.path().join("index.md")).expect("file");
        assert!(text.starts_with("---\ntitle: \"Home\"\n"));
        assert!(text.contains("description: \"About us\""));
        assert!(text.contains("status: 200"));
        assert!(text.contains("exported: 2026-10-29"));
        assert!(text.contains("# Home"));
        assert!(text.contains("Hello world content."));
        assert!(!text.contains("Menu"));
    }

    // from tests/test_live_content_interfaces.py::markdown_export lists failures without stopping
    #[test]
    fn fetch_failures_are_listed_and_export_continues() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fetcher = MockFetcher::new(vec![
            (
                "http://x.test/gone".to_owned(),
                String::new(),
                404,
                String::new(),
            ),
            (
                "http://x.test/live".to_owned(),
                String::new(),
                200,
                page("Live"),
            ),
        ]);
        let result = run(
            &[
                "http://x.test/gone".to_owned(),
                "http://x.test/live".to_owned(),
            ],
            &fetcher,
            dir.path(),
            200,
            0.0,
            "2026-10-29",
            Vec::new(),
        )
        .expect("run");
        assert_eq!(result["written"], json!(1));
        assert_eq!(result["failed"], json!(1));
        assert_eq!(result["requested"], json!(2));
        let failures = result["failures"].as_array().expect("failures");
        assert_eq!(failures[0]["error"], json!("HTTP 404"));
    }

    #[test]
    fn transport_errors_are_listed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fetcher = MockFetcher::new(Vec::new());
        let result = run(
            &["http://dead.test/".to_owned()],
            &fetcher,
            dir.path(),
            200,
            0.0,
            "2026-10-29",
            Vec::new(),
        )
        .expect("run");
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["failures"][0]["error"], json!("connection refused"));
    }

    // from tests/test_live_content_interfaces.py::markdown_export requires exactly one source
    #[test]
    fn source_selection_gates() {
        assert_eq!(
            validate_sources(None, None, None).unwrap_err(),
            "exactly one of url, urls_file, sitemap is required"
        );
        assert_eq!(
            validate_sources(Some("http://x.test/"), Some("urls.txt"), None).unwrap_err(),
            "exactly one of url, urls_file, sitemap is required"
        );
        // Empty strings are absent (Python truthiness).
        assert!(validate_sources(Some(""), Some(""), Some("http://x.test/sitemap.xml")).is_ok());
        assert!(validate_sources(Some("http://x.test/"), None, None).is_ok());
    }

    #[test]
    fn urls_file_skips_comments_and_blank_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("urls.txt");
        std::fs::write(&path, "# comment\n\nhttp://a.test/\n  \nhttp://b.test/\n").expect("write");
        let urls = read_urls_file(path.to_str().expect("utf8")).expect("read");
        assert_eq!(urls, ["http://a.test/", "http://b.test/"]);
    }

    // from tests/test_markdown_export_handler.py::test_urls_file_drives_the_batch_and_failures_are_kept
    #[test]
    fn urls_file_drives_the_batch_and_failures_are_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let urls_path = dir.path().join("urls.txt");
        std::fs::write(
            &urls_path,
            "https://x.test/\n\
             # a comment line\n\
             https://x.test/blog/canonical-url\n\
             https://x.test/missing\n",
        )
        .expect("write");
        let body = page("Canonical URL");
        let fetcher = MockFetcher::new(vec![
            (
                "https://x.test/".to_owned(),
                String::new(),
                200,
                body.clone(),
            ),
            (
                "https://x.test/blog/canonical-url".to_owned(),
                String::new(),
                200,
                body,
            ),
        ]);
        let result = run(
            &read_urls_file(urls_path.to_str().expect("utf8")).expect("read"),
            &fetcher,
            &dir.path().join("out"),
            200,
            0.0,
            "2026-10-29",
            Vec::new(),
        )
        .expect("run");
        assert_eq!(result["requested"], json!(3));
        assert_eq!(result["written"], json!(2));
        assert_eq!(result["failed"], json!(1));
        assert!(dir.path().join("out").join("index.md").is_file());
        assert!(dir
            .path()
            .join("out")
            .join("blog")
            .join("canonical-url.md")
            .is_file());
        assert_eq!(
            result["failures"][0]["url"],
            json!("https://x.test/missing")
        );
        assert_eq!(result["ok"], json!(true));
    }

    // from tests/test_live_content_interfaces.py::markdown_export keeps only http(s) URLs once
    #[test]
    fn dedupe_keeps_first_http_occurrence() {
        let ordered = dedupe_http(&[
            "http://a/".to_owned(),
            "ftp://b/".to_owned(),
            "http://a/".to_owned(),
            "https://c/".to_owned(),
        ]);
        assert_eq!(ordered, ["http://a/", "https://c/"]);
    }

    #[test]
    fn truncation_flag_and_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fetcher = MockFetcher::new(Vec::new());
        let targets: Vec<String> = (0..5).map(|i| format!("http://x/{i}")).collect();
        let result = run(
            &targets,
            &fetcher,
            dir.path(),
            3,
            0.0,
            "2026-10-29",
            Vec::new(),
        )
        .expect("run");
        assert_eq!(result["truncated"], json!(true));
        assert_eq!(result["requested"], json!(3));
        assert_eq!(result["failed"], json!(3));
    }

    #[test]
    fn meta_extraction_both_attribute_orders_and_quotes() {
        let (title, description) =
            meta("<title>  A  \n  B  </title><meta name=\"description\" content=\"First\">");
        assert_eq!(title, "A B");
        assert_eq!(description, "First");
        let (_, description) = meta("<meta content=\"Second\" name='description'><title>T</title>");
        assert_eq!(description, "Second");
        let (title, description) = meta("<title>He said \"hi\"</title>");
        assert_eq!(title, "He said 'hi'");
        assert_eq!(description, "");
    }

    #[test]
    fn relpath_sanitizes_segments_and_suffixes() {
        assert_eq!(relpath("http://x.test/"), PathBuf::from("index.md"));
        assert_eq!(
            relpath("http://x.test/about.html"),
            PathBuf::from("about.md")
        );
        assert_eq!(
            relpath("http://x.test/blog/2024/My Post!"),
            PathBuf::from("blog/2024/My_Post_.md")
        );
        assert_eq!(
            relpath("http://x.test/docs/guide.md"),
            PathBuf::from("docs/guide.md")
        );
    }

    #[test]
    fn duplicate_relpaths_get_numbered_suffixes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = page("Same");
        // Both targets redirect to the same final URL: the second file gets `-2`.
        let fetcher = MockFetcher::new(vec![
            (
                "http://x.test/one".to_owned(),
                "http://x.test/dup".to_owned(),
                200,
                body.clone(),
            ),
            (
                "http://x.test/two".to_owned(),
                "http://x.test/dup".to_owned(),
                200,
                body,
            ),
        ]);
        let result = run(
            &[
                "http://x.test/one".to_owned(),
                "http://x.test/two".to_owned(),
            ],
            &fetcher,
            dir.path(),
            200,
            0.0,
            "2026-10-29",
            Vec::new(),
        )
        .expect("run");
        assert_eq!(result["written"], json!(2));
        assert_eq!(result["files"], json!(["dup.md", "dup-2.md"]));
    }

    #[test]
    fn icon_ligature_lines_are_dropped() {
        let body = "# Heading\n\nzoom_in\nexpand_more\n\nKeep this line.\nrefresh\n";
        let cleaned = drop_icon_lines(body);
        assert!(!cleaned.contains("zoom_in"));
        assert!(!cleaned.contains("expand_more"));
        assert!(!cleaned.contains("refresh"));
        assert!(cleaned.contains("Keep this line."));
        assert!(cleaned.contains("# Heading"));
    }

    #[test]
    fn sitemap_expansion_supplies_targets_and_errors() {
        let expanded = json!({
            "ok": true,
            "urls": [{"loc": "http://x.test/a"}, {"loc": "http://x.test/b"}],
            "errors": [{"url": "http://x.test/bad.xml", "error": "boom"}],
        });
        assert_eq!(
            sitemap_targets(&expanded),
            ["http://x.test/a", "http://x.test/b"]
        );
        assert_eq!(sitemap_errors(&expanded).len(), 1);
        assert_eq!(sitemap_targets(&json!({})), Vec::<String>::new());
    }

    #[test]
    fn expanduser_expands_home_prefix() {
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            return;
        }
        assert_eq!(expanduser("~/notes"), Path::new(&home).join("notes"));
        assert_eq!(expanduser("/tmp/plain"), PathBuf::from("/tmp/plain"));
    }
}
