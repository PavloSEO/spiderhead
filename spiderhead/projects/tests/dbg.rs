#[test]
fn dbg_save_revision() {
    let dir = std::env::temp_dir().join(format!("dbg-sr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let proj = dir.join("proj");
    let v = spiderhead_projects::workspace::create_project(
        &proj,
        "https://example.com",
        Some("Example"),
        None,
        None,
        None,
    );
    println!(
        "create => {}",
        serde_json::to_string_pretty(&v.unwrap_or(serde_json::json!(null))).unwrap_or_default()
    );
    let mut doc = serde_json::json!({"format": "f", "project_uuid": "PLACEHOLDER", "revision": 0});
    let r = spiderhead_projects::io::save_revision(&proj, "doc", &mut doc);
    println!("save => {r:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
