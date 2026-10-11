# Control a running Desktop from CLI or an agent

The controller addresses one running native Desktop. It uses that window's
existing project bindings and scan manager. It does not start another core MCP
connection, crawler, HTTP listener or browser. Install the Desktop package's
existing dependencies with the normal source installation; no extra package is
required. The packaged helper is named `seohead-desktop-agent` when supplied by
the application bundle.

Desktop explicitly enables agent control and shows a **descriptor file path**.
Pass that path as `--endpoint`. It is not a website or a socket name. Keep the
file private: its contents include an authentication token. Only the path belongs
in agent configuration, screenshots or logs. Closing Desktop invalidates its
descriptor; reconnect using the new path after another instance starts.

```bash
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json status
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json tabs
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json project-scans --project-uuid PROJECT_ID
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json select-scan --project-uuid PROJECT_ID --scan-uuid SCAN_ID
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json select-view --view-id url --tab-id TAB_ID
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json new-tab --project-uuid PROJECT_ID --scan-uuid SCAN_ID --view-id url
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json select-tab --tab-id TAB_ID
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json close-tab --tab-id TAB_ID
```

Use IDs returned by this Desktop. There is no command to discover projects,
open an arbitrary path, execute code or stop an external process. Closing a tab
only closes that view; it does not stop a scan. A view or scan selector without
`--tab-id` targets the active tab. Status reports the current project, scan, view,
tab, available capabilities/errors and bounded run measurements supplied by the
owner callback.

Run mutations require explicit user authorization and `--approve`. A new scan
requires explicit URL, HTTP-request and time budgets. Additional
`configuration_overrides` are validated by the same current core descriptor as
the native scan plan. The Desktop callback also verifies project ownership and
sitemap capability before admitting a run.

```bash
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json new-scan --project-uuid PROJECT_ID --config-json '{"max_urls":20,"rendering_mode":"raw","max_requests":40,"max_seconds":60}' --approve
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json stop-run --run-id OWNED_RUN_ID --approve
python -m seohead_desktop.control_cli --endpoint /absolute/private-instance/control.json resume-run --run-id OWNED_RUN_ID --approve
```

Admission returns promptly with the queued/rejected state; it never waits for a
crawl to finish. Resume resolves the owned run's immutable saved artifact through
the existing core policy; a caller cannot supply an artifact path. A transport
timeout may leave a mutation's outcome unknown: inspect status before retrying.
There is no automatic retry. Optional global `--request-id` identifies one
attempt; the running server remembers its last 16 mutation receipts to avoid
duplicate admission. These receipts are bounded memory, not durable history.

For Claude or another stdio MCP client, use the installed Python interpreter or
replace `command` with the packaged helper and omit `-m ...` from `args`:

```json
{
  "mcpServers": {
    "seohead-desktop": {
      "command": "/absolute/desktop/.venv/bin/python",
      "args": ["-m", "seohead_desktop.control_cli", "--endpoint", "/absolute/private-instance/control.json", "mcp"]
    }
  }
}
```

The eleven `desktop_*` tools correspond to the CLI commands above. Read tools
carry `readOnlyHint`; selection/tab actions are declared UI mutations; scan and
resume declare external effects; stopping is marked destructive. Run tools
default to `approved=false`. An agent must obtain the user's authorization before
setting it to true. The MCP entrypoint uses stdio only.

## Native integration contract

```python
endpoint = prepare_endpoint(existing_runtime_directory)
control = DesktopControlServer(endpoint, window.dispatch_control, parent=window)
control.start()
# Display endpoint.descriptor_path only. Never serialize endpoint.
# On accepted application shutdown, call control.close().
```

The existing runtime parent must be an absolute, owned real directory. No
existing directory is chmodded. Preparation creates a fresh private `0700`
instance directory and an exclusive `0600` `control.json`. Unix sockets bind
directly inside that directory with no Qt permission-staging flags; the final
path is limited to 100 encoded bytes. Qt 5's permission-staging path adds an
extra temporary directory in its [Unix implementation](https://github.com/qt/qtbase/blob/5.15/src/network/socket/qlocalserver_unix.cpp), which can exceed the native socket limit even when
the final path fits. The private directory and token enforce Unix access.
Windows uses a random per-instance pipe and Qt `UserAccessOption`.
Authentication verifies a 256-bit token plus instance identity on every request.
This directory/token boundary is necessary because macOS ignores Qt socket
permission flags, as documented by [Qt](https://doc.qt.io/archives/qt-5.15/qlocalserver.html#socketOptions-prop). Clients verify owner, mode, real path and descriptor size
before reading it. Same-user processes can read the token; this is local user
access control, not isolation from the user's other applications.

`dispatch_control(operation, arguments)` runs in the Qt application thread and
returns a JSON object immediately. It resolves project/scan/tab IDs from already
opened contexts, validates configuration with `preview_configuration`, and
checks stop/resume IDs against the shared manager's owned runs. Domain refusals
raise `ControlError(code, message)` or `ValueError`. No callback may wait for
core/refresh completion or open a modal dialog. Long work returns an accepted or
queued result and exposes progress through status.

Requests use `seohead.desktop.control.v1` with one newline-terminated JSON frame
per connection. Limits: 64 KiB request, 1 MiB response, 5 seconds, eight client
connections. Unknown operations/fields, missing authorization, malformed JSON,
duplicate keys and extra frames are rejected. Server start never removes a
colliding endpoint. Shutdown closes only its own server/connections and removes
its original descriptor inode; unrelated files in its private directory remain.

Verification uses real local sockets, a real CLI subprocess, and an in-memory
MCP client calling those same sockets. It covers invalid tokens, collisions,
connection/size/deadline limits, permissions, Qt-thread dispatch and refusal of
external IDs by the owner callback. Shared-manager/UI integration is a separate
gate owned by the native application integration. macOS is exercised; Windows
and Linux runtime acceptance are not claimed.
