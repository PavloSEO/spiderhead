//! checklist-record input shapes. The MCP tool-docstring half and the record handlers are
//! deferred (see the report).

mod common;

// (CLI half only; the MCP `seo_project_checklist_record` docstring assertions are deferred)
#[test]
fn checklist_record_help_documents_the_input_shapes() {
    let argv: Vec<String> = ["project", "checklist-record", "--help"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let captured = common::invoke(&argv);
    assert_eq!(captured.code, 0, "stderr: {}", captured.stderr);
    for fragment in [
        "\"artifact\": \"scans/",
        "\"signoff\": true",
        "\"review\": \"approved\"",
        "\"not_applicable\"",
        "streamed audit.v2",
    ] {
        assert!(
            captured.stdout.contains(fragment),
            "help is missing {fragment:?}"
        );
    }
}
