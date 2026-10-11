# SEOHEAD Desktop: short user guide

The native guide uses Russian UI labels and opens from the host's Help/F1
integration. It explains the existing workflow and displays a supplied issue;
opening it never starts a scan, reads a project, connects a provider or enables
agent control. Navigation buttons only request an existing view.

## Open, inspect, fix, verify

1. Open an existing SEOHEAD project with the folder button. Check the project and
   saved scan in the top bars before interpreting a table.
2. Select a retained scan to inspect existing evidence. To collect new data,
   choose **Новый скан**, review the URL source, raw/JavaScript mode, request
   rate and concurrency, then explicitly start collection. The URL cap starts
   disabled; enable it to request a bounded scan. HTTP and time budgets are in
   Advanced settings and start at zero (disabled). Full native collection uses
   unlimited depth and a 12 GiB free-disk reserve. Both full and sitemap-only
   modes require the corresponding connected-core capability. An older core
   can still accept a supported bounded plan. The settings are controls, not a
   claim that million-URL performance has been verified.
3. Use **URL** for saved rows and **Аудит** for topical views. Select a row to
   inspect its message, affected URL and evidence. Table filtering applies to
   the loaded page; it is not a global site query.
4. Keep a **before** scan, fix the website outside this application, explicitly
   collect a comparable **after** scan and choose that pair in **Сравнение**.
   Check the verification result and its coverage. A disappeared finding is not
   automatically a verified repair.

## Choose the right surface

| Surface | Use it for | Interpretation |
| --- | --- | --- |
| Работа / Журнал | Plan, run measurements and supplied events | A run's collection rate and an audit plan's task completion are different measurements. No plan denominator means no completion percentage. |
| URL / Аудит | Bounded retained rows, topical evidence and selected details | An empty page is different from unavailable evidence. A visible tab does not promise a connected source. |
| Поиск в содержимом | Literal markers in retained head markup, raw HTML, body text or a CSS-selected element | Choose static HTML or retained rendered DOM deliberately. Unavailable content is not a negative match. Finding a tag marker does not prove it executed. |
| Сканы | Retained history and explicitly owned run controls | Selecting a scan only reads it. Stop targets the selected owned run. Resume requires eligible retained interrupted evidence. Externally started runs remain observations. |
| Входящие | Explicitly save a note or proposed goal | Drafts belong to their project. Saving waits for fresh inbox state and does not start an agent or scan. Unsupported message kinds stay unavailable. |
| Working tabs | Keep a project, scan and view context | Closing a view does not stop collection. Use the View menu to restore panels or adjust density; F6 moves between work areas. |
| Агент | Explicit local CLI/MCP connection to the running Desktop | Share the descriptor file path, never its contents. A restarted instance needs a new connection. Mutating run actions require authorization. See [local control](local-control.md). |

## Read states before counts

- **Завершён** means the run ended. Inspect result completeness and finish reason
  separately.
- **Полные данные** describes the core-confirmed scope and source; it does not
  mean there are no SEO findings.
- **Частичный результат** means only part of the evidence is available. The
  actual reason may be a configured bound, interruption or missing source.
- **Нельзя подтвердить** means the evidence does not support the conclusion.
  Absence is not a verified fix, and missing HTML is not an absent marker.
- **Недоступно**, unknown or skipped checks are not successful checks. Read the
  reported reason and any actual next step before repeating work.

Start with the actual check title, message and recommendation, then inspect the
URL and evidence. Compare source, timestamp, scope and skipped checks. The guide
does not guess a finding's meaning from an unknown machine code. If the source
does not supply a reason or recommendation, none is invented.

Availability follows the selected project, saved evidence and connected core.
The guide does not promise live Screaming Frog launch, cloud export or verified
million-URL GUI performance. These are not unlocked by opening help.

## Host integration

`HelpGuideDialog(parent, available_views=..., issue=...)` is a separate native
dialog. The host owns Help/F1 registration and handles
`navigateRequested(view_id)` through its existing navigation. Pass an explicit
set of available view IDs to disable unavailable links; these links represent
navigation only, not permission to execute a tool. The dialog closes after an
enabled navigation click and does not change the host's splitter state.

`set_issue(payload)` replaces all previous issue fields. Accepted values are
already-redacted strings: `title`, `message`, `reason`, `recommendation`, `code`,
`state`, `url` and `source`. Nested objects and other keys are ignored. Main
fields are bounded to 900 characters, the title to 140, and each optional source
field to 300. Values are plain text, never HTML or executable links. Source
fields start collapsed. The host should pass the actual selected payload or
visible error text, not synthesize a missing reason.

The guide has three scrollable topics and preserves access at 440×420 logical
pixels. It reuses bundled Material Symbols and the current theme; it introduces
no web view, network dependency, persistence or operation dispatch.

## Window sizes

The primary window supports a minimum of 800×720 logical pixels. Use a tall or half-screen window for dense tables. In a short window, search presets and secondary options are in the settings menu, and comparison pair controls can be expanded with Choose pair. The source scan selector and run controls remain reachable. Windows and mixed-monitor scaling require their own acceptance checks.
