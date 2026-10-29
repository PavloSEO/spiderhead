//! Live render over CDP against the oracle fixture server (basic/js.html).
//! Ignored when no Chrome is installed; the loopback fixture server is the
//! only network this test touches.

use spiderhead_oracle_testkit::FixtureServer;

// ignored when no Chrome is installed, per the slice rules)
#[tokio::test]
#[ignore = "requires an installed Chrome"]
async fn renders_the_js_fixture_page() {
    if spiderhead_render::chrome_executable().is_none() {
        panic!("this test requires Chrome; run with --include-ignored on a machine with Chrome");
    }
    let server = FixtureServer::start("basic");
    let url = format!("{}/js.html", server.base);
    let html = spiderhead_render::render_document(&url, 60.0)
        .await
        .expect("render");
    assert!(html.contains("/js-only.html"), "JS-injected link missing");
}
