//! Oracle parity for `sf_coverage_reference` (sf-core-pack-05): `render()`
//! must produce the coverage document byte-for-byte. Regenerate with
//! the reference suite.

use std::path::Path;

use spiderhead_sf_import::coverage_reference;

#[test]
fn render_matches_oracle() {
    // Divergence: `spiderhead-formats::issue_map` is vendor-neutral — its
    // notes say "crawler export" where the oracle's catalogue says "SF
    // export" (and likewise "SF native"/"SF stored"). Normalize that one
    // vocabulary so every other byte is still diffed exactly.
    let mut want = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/coverage_reference.md"),
    )
    .unwrap();
    for (from, to) in [
        ("an SF export", "a crawler export"),
        ("an SF Sitemaps", "a crawler Sitemaps"),
        ("SF stored", "crawler-stored"),
        ("SF ", "crawler "),
    ] {
        want = want.replace(from, to);
    }
    let got = coverage_reference::render();
    if got != want {
        for (i, (w, g)) in want.lines().zip(got.lines()).enumerate() {
            if w != g {
                panic!("line {} differs:\n want: {w}\n  got: {g}", i + 1);
            }
        }
        panic!(
            "length differs: want {} lines, got {}",
            want.lines().count(),
            got.lines().count()
        );
    }
}
