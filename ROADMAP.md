# Roadmap

Milestones and epics. Each epic has a goal and a checklist; checked items are done in the current snapshot.

## 0.9 — CLI core and log analysis

First public snapshot: the CLI is the primary surface; log analysis with bot verification; MCP via `spiderhead mcp`; project model with autodiscovery and `init`.

### CLI core

**Goal:** Working command tree, output formats, project model

- [x] ~250 commands registered
- [x] `spiderhead init`, project autodiscovery, `schema_version` migration hook
- [x] `spiderhead mcp` subcommand (alias binary kept)
- [ ] Replace the scheduler's interpreter-based command line with the native binary

### Log analysis

**Goal:** Import, summary and bots with IP-range, rDNS and ASN verification

- [x] `logs-import`, `logs-summary`, `logs-bots`
- [x] Bot verification: IP ranges, reverse DNS, ASN
- [ ] Large-log performance pass

### Public snapshot

**Goal:** Clean first public history

- [x] Reference tooling removed from the repo
- [x] README, ROADMAP, CHANGELOG
- [ ] First tagged release and installers

## 0.10 — Remaining commands and Windows

Close the command gaps and ship a Windows build.

### Comparison commands

**Goal:** `compare-*` family

- [ ] compare-crawls
- [ ] compare-findings
- [ ] compare-url / compare-report

### Content and images

**Goal:** Content extraction and image tools

- [ ] markdown export / extract
- [ ] images download / optimize
- [ ] boilerplate and duplicate reports

### Checks list

**Goal:** Complete checks catalogue

- [ ] every documented check reachable from the CLI
- [ ] per-check help text

### Cloudflare logs

**Goal:** Logpush and Instant Logs

- [ ] logpush import
- [ ] instant logs stream
- [ ] traffic and history reports

### Windows build

**Goal:** Every component builds and runs on Windows

- [ ] CLI and MCP
- [ ] desktop app
- [ ] MSI installer

## 0.11 — Desktop shell

A usable desktop shell: launcher, project mode, log analysis UI.

### Launcher

**Goal:** Mode chooser and recent projects

- [x] Launcher with log analysis enabled
- [ ] Recent projects list
- [ ] Open project from folder (autodiscovery)

### Project mode

**Goal:** Open, switch and inspect a project

- [ ] Project overview
- [ ] Scans and reports browser
- [ ] Task and checklist panels

### Log analysis UI

**Goal:** Screens for the log workflow

- [ ] Import dialog
- [ ] Summary and trends
- [ ] Verified bots view
- [ ] Visual polish pass

## 0.12 — Crawler UI parity

The desktop crawler reaches parity with the CLI crawl.

### Crawler UI

**Goal:** Run, monitor and review crawls

- [ ] Crawl setup and presets
- [ ] Live progress and controls
- [ ] URL table, details and issues
- [ ] Compare scans

## 0.13 — Agent mode and report v2

Agent mode in the desktop app and a second-generation report.

### Agent mode

**Goal:** Let an agent work a project inside the app

- [ ] Permissions and visible action log
- [ ] Task inbox
- [ ] Control channel hardening

### Report v2

**Goal:** New report format

- [ ] Report model
- [ ] HTML / PDF / XLSX outputs
- [ ] Client-ready templates

## 1.0 — Stable

Stable CLI, MCP and desktop with a compatibility promise.

### Stabilisation

**Goal:** Freeze the public surface

- [ ] Command and tool names frozen
- [ ] `project.json` and scan formats versioned with migrations
- [ ] Signed installers for macOS, Linux and Windows
- [ ] Documentation site

## post-1.0 — AI visibility

Measure and improve visibility in AI search.

### ai-visibility module

**Goal:** Track how AI answers cite a site

- [ ] Prompt sets and sampling
- [ ] Citation tracking over time
- [ ] Report section
