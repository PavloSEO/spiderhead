//!
//! Only the tests whose behaviour the Rust `AuditContext` already exposes are ported here. The
//! reference originals load their rows through the SF CSV loader, which has no Rust counterpart
//! yet; the same rows are built in memory instead, with the same URLs, status codes and
//! content types.

use spiderhead_checks::audit::{AuditContext, Page};

fn page(url: &str, status: i64, content_type: &str, indexability: &str) -> Page {
    Page {
        url: url.to_owned(),
        status_code: Some(status),
        content_type: content_type.to_owned(),
        indexability: indexability.to_owned(),
        ..Page::default()
    }
}

#[test]
fn html_pages_excludes_redirect_and_error_stubs() {
    // Issue #133: a 301 or 404 stub served as text/html must not enter the population.
    let ctx = AuditContext::new(vec![
        page("https://example.com/live", 200, "text/html", "Indexable"),
        page(
            "https://example.com/old-page",
            301,
            "text/html",
            "Non-Indexable",
        ),
        page(
            "https://example.com/gone",
            404,
            "text/html; charset=UTF-8",
            "Non-Indexable",
        ),
    ]);
    let urls: Vec<&str> = ctx
        .html_pages()
        .into_iter()
        .map(|index| ctx.pages[index].url.as_str())
        .collect();
    assert_eq!(urls, vec!["https://example.com/live"]);
}
