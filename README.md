# SpiderHead

SpiderHead is an SEO toolkit built on one Rust core with three faces: a command-line tool, an MCP server for AI agents, and a desktop app.

> **Status: 0.9, first public snapshot.** The command line is the primary, working surface. The MCP server works through the same binary. The desktop app is early and its visuals are weak. See [ROADMAP.md](ROADMAP.md).

## What works today

| Surface | State |
|---|---|
| `spiderhead` CLI | Primary and working. 250+ commands: **log analysis** (`logs-import`, `logs-summary`, `logs-bots` with IP-range, reverse-DNS and ASN verification of search and AI bots), **crawling** (`crawl-site`), **page parsing** (`parse`), **projects**, **data sources**, semantics, audits and reports |
| MCP server | `spiderhead mcp` serves the same tools over stdio (`spiderhead-mcp` is kept as an alias) |
| Desktop app | Early. Log analysis opens from the launcher; the crawler UI and the agent mode are marked "coming soon". Expect rough visuals |

Not finished yet (see the roadmap): some comparison commands, content and image tools, the Cloudflare Logpush / Instant Logs importers, and a Windows build of every component.

## Install

There are no signed releases yet. Build from source:

```sh
cargo build --release
# binaries: target/release/spiderhead  spiderhead-mcp  spiderhead-desktop  spiderhead-desktop-ctl
```

`cargo xtask dist` and `cargo xtask package macos|linux|windows` produce profile archives and installers (see [docs/COMPONENTS.md](docs/COMPONENTS.md)). The desktop app has menu actions *Install command in terminal* (links the bundled `spiderhead` into a user-writable `PATH` directory) and *Connect to Claude* (copies the MCP config snippet; it never edits client configuration files).

## Quick start

```sh
spiderhead --version
spiderhead --help

# analyse server logs
spiderhead logs-import --help
spiderhead logs-summary --help
spiderhead logs-bots --help

# use it from an AI client: add to the client's MCP settings
#   { "mcpServers": { "spiderhead": { "command": "spiderhead", "args": ["mcp"] } } }
spiderhead mcp
```

## Project model

A project is a `seo/` folder inside your site's working tree:

```
my-site/
  seo/
    project.json     manifest (schema_version, site, facts); relative paths only
    log.md           project journal
    scans/ reports/  heavy data
    .gitignore       keeps scans/, logs/ and *.db out of git
```

- `spiderhead init [--site URL]` creates `seo/` in the current directory. Without `--site` the directory name is used if it looks like a host name.
- **Autodiscovery.** Like git finds `.git`, the CLI, the MCP server and the desktop app walk up from the current directory to the nearest `seo/project.json` (a `project.json` in the current directory also counts). An explicit `--project` or `--directory` always wins.
- `project.json` carries a `schema_version`; manifests written before it existed load as version 1 and are migrated in memory.

## Build and test

Requires the Rust toolchain pinned in `rust-toolchain.toml`.

```sh
cargo build --release
cargo xtask ci        # fmt --check, clippy -D warnings, tests, doctests
cargo xtask layers    # crate layering check
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the workspace layout and quality gates.

## License

MIT. See [LICENSE](LICENSE).
