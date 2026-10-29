//! Fixed interface policy for project actions without a trusted MCP principal
//!
//! This is an application workflow boundary, not an operating-system sandbox. A local
//! process with filesystem or CLI access can still modify project files.

use crate::error::ProjectsResult;

/// Actions that require a trusted caller identity the local MCP server does not have.
pub const OWNER_ONLY_ACTIONS: &[&str] = &[
    "project.delete",
    "scan.delete",
    "project.settings.update",
    "project.settings.review",
    "project.policy.apply",
    "project.goal.accept",
    "project.checklist.review",
    "project.checklist.signoff",
    "provider.credential.revoke",
    "provider.auth.start",
    "provider.auth.cancel",
    "provider.auth.connect",
    "provider.auth.refresh",
    "provider.auth.disconnect",
    "provider.auth.revoke",
];

/// `require_mcp_allowed`: fail closed for owner-only actions on local MCP.
pub fn require_mcp_allowed(action: &str) -> ProjectsResult<()> {
    if action.is_empty() || action.len() > 128 {
        return Err(crate::error::value(
            "action must be a bounded server-defined identifier",
        ));
    }
    if OWNER_ONLY_ACTIONS.contains(&action) {
        return Err(crate::error::value(format!(
            "{action} is owner-only and unavailable over MCP because this server has no \
             trusted caller identity; use the explicit local CLI workflow"
        )));
    }
    Ok(())
}
