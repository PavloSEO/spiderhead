# MCP profiles and caller-requested progress

`configure_profile(server, profile)` runs after tool registration and removes schemas through
FastMCP's public `remove_tool()` method. Build a fresh server to choose another profile.

| Profile | Advertised surface |
|---|---|
| `full` | Compatibility mode: every registered tool remains available. |
| `audit` | Core doctor, audit workflow, catalog, and bounded SF audit tools. |
| `infra` | Core doctor, inspect/catalog plus domain, CDN, technology, security, headers, robots, sitemap, and regions. |
| `quick-check` | Core doctor, inspect/catalog plus small URL checks. |
| `router` | Core doctor, inspect, audit workflow, and catalog. |

The root bindings provide `seo_inspect_url`, `seo_audit_workflow`, `seo_tool_catalog`, and the
read-only `seo_doctor` diagnostics tool before
selecting a restricted profile. `seo_tool_run` is intentionally excluded from `router`: a generic
action-and-any-payload tool would bypass real tool schemas and their paid, write, and network
guards. The catalog may describe allowed low-level tools without advertising every one as an MCP
schema.

`install_progress(server)` returns a factory for async bindings. Inside a tool request, use
`async with progress.scope(ctx, "crawl-site") as report:` then call either
`await report.known(done, total)` for measured totals or `await report.elapsed()` when no total is
known. Notifications are sent only when the caller supplied an MCP `progressToken`; elapsed
messages label the unknown total, values are monotonic and throttled, and context exit stops
emission on success, cancellation, or failure. Progress does not guarantee client timeout or
completion behavior.

## Scoped project inbox notices

A local stdio host can opt into a stable inbox recipient without making the
recipient a shared default. Set both `SEOHEAD_MCP_CONSUMER_ID` and
`SEOHEAD_MCP_PROJECT_ALLOWLIST` at process startup. The allowlist is a
path-separator-delimited list of existing local project directories. A matching
project call receives only that consumer's bounded unread summary; a call for a
project outside the allowlist receives no inferred consumer identity.

```json
{
  "mcpServers": {
    "seohead-shop": {
      "command": "/absolute/path/to/.venv/bin/seohead",
      "args": ["mcp"],
      "env": {
        "SEOHEAD_MCP_CONSUMER_ID": "agent/shop-audit",
        "SEOHEAD_MCP_PROJECT_ALLOWLIST": "/absolute/path/to/shop-project"
      }
    }
  }
}
```

Run a separate stdio process for another project or agent. Explicit per-call
`consumer` values remain available where a tool exposes them, but a process does
not guess one. A summary is informational: it never reads, acknowledges, accepts,
or completes inbox entries. Those transitions remain explicit project-inbox tools.

When an agent follows the packaged `control` or `full-audit-v1` workflow, it offers
the corresponding human observer once before the first collection or long-running
analysis for a project. The offer is an explicit yes/no choice, tracked only in the
agent's current session/project context. `seo_project_observe` is the MCP-side
read-only snapshot after an acceptance; it does not substitute for consent, launch
a terminal, start a scan, or inject inbox content into a chat.
