//! Self-description of commands: the same list serves the UI, the control channel
//! (`commands.list`), the CLI and — later — the MCP server. One registry, four faces.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One command as it is advertised to agents and menus. `enabled`/`disabled_reason` are
/// evaluated against the live session, everything else is static.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInfo {
    pub id: String,
    pub label: String,
    /// Human-readable parameter documentation; `{}` when the command takes no parameters.
    pub params: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}

/// Keep the commands whose id, label or params mention `search` (case-insensitive).
pub fn search_commands(commands: &[CommandInfo], search: &str) -> Vec<CommandInfo> {
    if search.is_empty() {
        return commands.to_vec();
    }
    let needle = search.to_lowercase();
    commands
        .iter()
        .filter(|c| {
            c.id.to_lowercase().contains(&needle)
                || c.label.to_lowercase().contains(&needle)
                || c.params.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

/// The reply payload of `commands.list`.
pub fn commands_value(commands: &[CommandInfo]) -> Value {
    json!({ "commands": commands })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<CommandInfo> {
        vec![
            CommandInfo {
                id: "app.quit".into(),
                label: "Quit".into(),
                params: "{}".into(),
                enabled: true,
                disabled_reason: None,
            },
            CommandInfo {
                id: "scan.open".into(),
                label: "Open scan".into(),
                params: r#"{"path": "file"}"#.into(),
                enabled: false,
                disabled_reason: Some("no scan loaded".into()),
            },
        ]
    }

    #[test]
    fn search_filters_by_id_label_and_params() {
        let commands = sample();
        assert_eq!(search_commands(&commands, "quit").len(), 1);
        assert_eq!(search_commands(&commands, "scan").len(), 1);
        assert_eq!(search_commands(&commands, "APP").len(), 1);
        assert_eq!(search_commands(&commands, "").len(), 2);
        assert_eq!(search_commands(&commands, "nothing-matches").len(), 0);
    }

    #[test]
    fn serialization_skips_absent_reason() {
        let v = serde_json::to_value(&sample()[0]).unwrap();
        assert!(v.get("disabled_reason").is_none());
        assert_eq!(v["enabled"], json!(true));
    }
}
