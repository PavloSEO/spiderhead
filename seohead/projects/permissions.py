"""Fixed interface policy for project actions without a trusted MCP principal.

This is an application workflow boundary, not an operating-system sandbox. A
local process with filesystem or CLI access can still modify project files.
"""

from __future__ import annotations

OWNER_ONLY_ACTIONS = frozenset(
    {
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
    }
)


def require_mcp_allowed(action: str) -> None:
    """Fail closed for owner-only actions on local MCP, which has no trusted principal."""
    if type(action) is not str or not action or len(action) > 128:
        raise ValueError("action must be a bounded server-defined identifier")
    if action in OWNER_ONLY_ACTIONS:
        raise ValueError(
            f"{action} is owner-only and unavailable over MCP because this server has no "
            "trusted caller identity; use the explicit local CLI workflow"
        )
