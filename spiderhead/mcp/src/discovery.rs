//! Filling the project argument from the working directory when a call leaves it out.

use std::path::Path;

use serde_json::Value;

/// Tools whose `directory` names a path to create, never an existing project.
const CREATES: &[&str] = &["seo_project_new"];

/// If the tool takes exactly one project argument (`directory` or `project`) and the call
/// omits it, set it to the nearest project found from `start`.
pub fn fill_project(tool: &str, schema: &Value, args: &mut Value, start: &Path) {
    if CREATES.contains(&tool) {
        return;
    }
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let key = match (
        props.contains_key("directory"),
        props.contains_key("project"),
    ) {
        (true, false) => "directory",
        (false, true) => "project",
        _ => return,
    };
    let Some(map) = args.as_object_mut() else {
        return;
    };
    if map.get(key).is_some_and(|v| !v.is_null()) {
        return;
    }
    if let Some(found) = spiderhead_core::project_root::discover(start) {
        map.insert(
            key.into(),
            Value::String(found.to_string_lossy().into_owned()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sh-mcp-disc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("seo")).unwrap();
        std::fs::write(dir.join("seo/project.json"), "{}").unwrap();
        dir
    }

    #[test]
    fn an_omitted_project_is_filled_and_an_explicit_one_wins() {
        let dir = project();
        let schema = json!({"properties": {"project": {}}, "required": ["project"]});
        let mut args = json!({});
        fill_project("seo_logs_summary", &schema, &mut args, &dir);
        assert_eq!(args["project"], json!(dir.join("seo").to_string_lossy()));
        let mut args = json!({"project": "/elsewhere"});
        fill_project("seo_logs_summary", &schema, &mut args, &dir);
        assert_eq!(args["project"], json!("/elsewhere"));
    }

    #[test]
    fn creating_tools_are_left_alone() {
        let dir = project();
        let schema = json!({"properties": {"directory": {}}});
        let mut args = json!({});
        fill_project("seo_project_new", &schema, &mut args, &dir);
        assert_eq!(args, json!({}));
    }
}
