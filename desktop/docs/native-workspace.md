# Native workspace presentation

The desktop workspace uses the existing SEOHEAD palette, bundled Roboto and Material Symbols. The single token source is `src/seohead_desktop/theme/tokens.json`; QSS renders widget states, Qt layouts and splitters control geometry. No web view, per-row widgets or new data store is introduced. The only optional motion is a finite 120 ms navigation-width transition; the Reduce Motion setting and the supported system preference disable it.

| Surface | Presentation and interaction | Data boundary |
| --- | --- | --- |
| Project and scan context | Compact project toolbar, retained-scan selector, explicit state badge | Existing `project-open`, `project-observe` and `project-scans` responses; selecting a scan never starts collection |
| Work | Readable plan and next actions; bounded run table and selected-run measurements | Up to 50 supplied observations; current rate requires fresh telemetry and keeps its unit separate from the request-rate limit |
| URL | Page-local search/sort, bounded metadata, retained headers, diagnostic payload | Existing bounded page/detail responses; absent, empty, false and zero remain distinct; sensitive header values are masked |
| Audit | Native tab decks, independent detail/summary buttons, translated primary tab labels | Catalogue visibility does not imply available evidence; unsupported sections remain explicitly unavailable |
| Scans | Retained history, owned-run controls and known-frontier progress | Existing ownership controls are reused. Progress uses measured done/queued/inflight counts, never the configured URL limit or an estimated site size |
| Journal | Readable time, run ID, phase and event | Last 20 supplied events per run, at most 200 displayed rows; no arbitrary file reads |
| Comparison | Before/after selectors, bounded differences, coverage details and verified-page summary | Explicit `compare-crawls` creates a local immutable package; `verify-fixes` always receives the new scan. Raw disappearance is never displayed as a confirmed repair |
| Manual scan | Spider / sitemap URL choice and an exact visible scope/limits preview | Sitemap submission is disabled unless the core descriptor advertises `capabilities.sitemap_only_retained`; no automatic collector substitution |
| Reports | Explicit unavailable state | Export is not connected by this presentation change |

Below 1100 logical pixels the navigation becomes a 64-pixel rail and the summary initially collapses. The user can reopen the summary or details, restore panels, or temporarily focus the table. The View menu offers 28/32/40-pixel row density. Open/Find/Copy use Qt platform shortcuts; F6 moves between native work areas. Long context labels elide visually while their full source text stays available to accessibility and tooltips. Splitter grips support arrow keys and double-click restore. Flat scrollbars retain native keyboard and pointer behavior. The project picker contains only explicitly opened recent projects (at most 20), with no folder discovery or automatic opening. Read failures are shown in a dismissible plain-text notice.

Existing scans, process ownership, dispatch, cancellation, resume and MCP transport stay in the existing adapters. App hooks invalidate stale comparison responses when the pair/project changes or reading is cancelled. Locally rotated Material chevron derivatives and their hashes are recorded in the existing asset manifest. The verified application bundle is not replaced by this source change.

## Validation scope

`tests/test_presentation.py` covers missing measurements, zero and empty values, fresh versus retained rates, frontier denominators, scan identity, bounded journal rows, compact layout, density, keyboard focus and supplied headers. Existing shell, component and multi-run lifecycle tests remain applicable.

Native acceptance on 2026-10-07 used Cocoa on macOS ARM64 with Retina device pixel ratio 2. The app read three actual retained local fixture scans with seven URLs each through the packaged core. Recorded interactions cover scan selection, URL filtering and details, headers, clipboard, F6, panel restore, partial-run inspection, unavailable tabs, journal and fullscreen. Retained SQLite hashes were unchanged. This is a desktop presentation check; it is not a scale benchmark, VoiceOver audit, mixed-display test or Windows/Linux acceptance.

Historical acceptance evidence is retained outside the public repository. The packaged core used for saved-data acceptance is `ceb2eab716c0077355d96b3a99f92ea1eb0bc0b6`; source runtime regression tests use the explicitly recorded current core checkout.

## Populated comparison acceptance

The second native pass uses the local QA project's matched 20-URL pair, not a screenshot mockup. The first bounded comparison page has 100 of 145 rows: the core confirms 13 resolved findings, while 20 remain not verifiable. Those are page-scoped verification counts, not a site health score or a statement that all 29 absent observations were repaired. New observations, persistent findings and coverage warnings remain separate.

The `visual-native/polish/` evidence folder records populated wide/compact/fullscreen captures, source SQLite hashes, pointer/keyboard input, finite animation samples and reduced-motion behavior. The comparison creates reports beneath the explicitly opened QA project; it does not fetch the site or alter either source scan. The native lanes outside macOS remain unverified.

## Native workspaces and a second window

The View menu provides four named arrangements: table only, URL with details, comparison, and a monitor in two windows. The monitor is a native `QDockWidget` and can also be docked or detached with Qt's title-bar control. It shares the primary window's exact activity model and selection model. Read-only text is mirrored into separate documents so each window wraps to its own viewport. It creates no gateway, poller, crawler or project store. Its compact table prioritizes run identity, state, received pages, current measured rate and the configured limit; other details stay available below. Closing it hides the monitor. The primary window hides it only after the existing owned-process and worker shutdown gates have completed.

Layouts use versioned `QSettings` entries with stable view/panel identifiers, density, splitter positions and native dock state. Restoring a window clamps its geometry to currently available screens. Opening another project clears both windows' old observations before loading the new project. This monitor uses the primary view's existing refresh cadence; it does not create an independent live observer.

Cmd/Ctrl+K searches a bounded registry of declared UI actions. Enter opens a view or the scan plan; it never interprets typed text as a command or automatically dispatches a scan. Unsupported retained-HTML presets are displayed with an explicit disabled reason until the corresponding core adapter is connected. The global header uses native Qt toolbars, so docking a panel does not narrow the project controls. URL details also have visible hide/reopen controls.

On macOS, the optional 120 ms navigation transition reads the actual `com.apple.universalaccess.plist` preference through bounded, read-only `plistlib` parsing at window startup. A denied or invalid preference read disables optional motion. Missing preferences use the OS default. The application Reduce Motion control remains available where the system does not require it. No system setting is modified.

The third-pass native evidence is in `visual-native/workspace/`: distinct native window IDs, shared-model identities, synchronized selection, native detach, close/reopen, action finder from either window, layout restore, 1024/1440/fullscreen views and unchanged source SQLite hashes. The machine has one attached Retina display; mixed-monitor DPI, VoiceOver and other operating systems are not claimed as tested. `tests/test_workspace.py` adds focused regressions for those data/ownership boundaries and the isolated preference parser.

## Selection and draft safety

The workspace clears URL detail and pending detail intents when a page or local filter invalidates selection. Observer rows map Stop to the exact owned run identity; external rows cannot target an owned process. Project note drafts use both project UUID and resolved directory, and writes require a fresh inbox revision. The native plan keeps invalid input open and updates when the core capability descriptor arrives. User panel and navigation choices survive responsive breakpoints.

The safety integration was checked with actual Cocoa interactions in `functional-resume-20261007/ux-five/`: scan-plan validation, URL/detail clearing, project note isolation, two-window wrapping/geometry, and retained-data immutability. Owned Stop/Resume was additionally exercised against a fresh loopback fixture.

## Retained content search

The HTML search workspace uses the existing core CLI over the selected retained scan. GTM, Google tag and Metrika presets only fill the form; Search is explicit. Static HTML and retained rendered DOM remain separate, and missing bodies are reported as unavailable, never as absence. Corpus coverage is kept beside bounded pages of at most 100 rows. A marker does not prove that analytics executes. Search owns one separate CLI child and does not block the project MCP observer; project/scan changes invalidate its result, and window shutdown drains only its own child. Older core packages disable the feature using advertised optional capabilities.

## External agent observation

An open project keeps the existing single observer timer running without an owned scan. It reads every two seconds while idle and every 500 ms when an observed or owned run is active. An in-flight observer, project transition or pending note write suppresses another read. Changed plan revisions refresh the bounded checklist. Externally launched CLI/MCP runs remain observations and never become GUI-owned Stop targets.

## Browser-style workspace contexts and agent control

A native tab strip holds at most 12 immutable view contexts over the same MainWindow, gateway and local scan manager. Tabs remember project/scan identity, view, URL page/filter/selection, per-panel searches and splitters. Switching clears old evidence and reloads bounded core projections with the existing generation guard. Note drafts remain scoped to project UUID and resolved path; an unconfirmed write blocks tab changes. Closing a tab removes only its descriptor, and an owned scan continues. Cmd/Ctrl T/W and native next/previous-tab shortcuts are available; duplicate and overflow actions live beside the tab strip.

The Agent menu and optional `--agent-control EXISTING_DIRECTORY` publish one protected descriptor path for the local CLI/MCP bridge. No automatic endpoint discovery or arbitrary IPC project paths are admitted. Run actions use existing typed core settings, explicit approval, loaded IDs and the same scan manager. `--project EXISTING_PROJECT` is a separate explicit startup argument that opens retained data without launching a scan. The main URL view exposes bounded paging and separate Copy URL / Copy row (TSV) actions.

## Practical minimum and live monitor

The primary-window minimum is now 640×480 logical pixels to support the owner review size. Visual acceptance at 640×480 and 800×800 remains pending. Navigation and evidence screens still need the integrated two-size review before this geometry is a support claim. Startup geometry is clamped to the available screen including the native frame. Wide URL tables distribute spare width to URL and Title while retaining user column-width intent.

The Work overview and detached monitor consume the same observer snapshot and exact run selection. Real counters, current measured speed, queue semantics, stage and recent events stay distinct from the agreed audit-plan percentage. No current speed is shown as a dash with a reason; exact retained telemetry remains in the tooltip and detail view. Run IDs distinguish simultaneous runs. A bounded 120 ms transition reacts to changed measured values and obeys reduced motion. Material symbols are painted from the licensed SVG paths at the actual device scale; fixed low-resolution label bitmaps are not used. F1 opens the native guide, with a current error only when one is actually present.

Final native evidence lives in `visual-native/final-matrix/`, `final-minimum/` and `final-supported/` under the functional-resume receipt tree. It includes real retained comparison/search data, workspace switching, the 800-pixel width, a half-screen window, fullscreen, hover affordances, the plan dialog cancelled without a launch, separate native windows and unchanged source scan hashes. Only one Retina macOS display was exercised.
