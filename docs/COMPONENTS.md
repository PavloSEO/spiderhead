# Components and install profiles

Binaries are built from the `spiderhead/*` crates. `cargo xtask dist`, the installers and the documentation take their names from here.

## Binaries

| Binary | Crate | What it is |
|---|---|---|
| `spiderhead` | `spiderhead-cli` | CLI: crawl, audit, projects, semantics, log analysis. `spiderhead mcp` runs the MCP server; `spiderhead init` creates a project |
| `spiderhead-mcp` | `spiderhead-mcp` | Alias for `spiderhead mcp`, for clients that want a single command |
| `spiderhead-desktop` | `spiderhead-desktop` | Desktop app (egui): one window, three modes (crawler, log analysis, agent mode) chosen on the launcher. Log analysis is the one enabled in 0.9 |
| `spiderhead-desktop-ctl` | `spiderhead-desktop` | Control-channel client: an agent drives a running window |

## Profiles

| Profile | Installed | On PATH |
|---|---|---|
| **Lite** (`lite`) | `spiderhead-desktop` | no |
| **CLI** (`cli`) | `spiderhead`, `spiderhead-mcp` | yes |
| **Desktop** (`desktop`) | `spiderhead-desktop`, `spiderhead-desktop-ctl`, `spiderhead` | yes |
| **All** (`all`) | all four plus documentation | yes |

## `profile.toml`

`cargo xtask dist` (`xtask/src/dist.rs`) places `profile.toml` next to the binaries (`share/spiderhead/profile.toml` for `install.sh`, `/Library/Application Support/SpiderHead/profile.toml` for the .pkg; Lite has none):

```toml
profile = "desktop"
default_mode = "agent"   # lite/cli: "crawler", desktop/all: "agent"
path_cli = true
```

The installers write `default_mode`; the window does not read it yet.

## Artifact names

`cargo xtask dist` writes `target/dist/<version>/<profile>/{bin/, profile.toml, MANIFEST.json, SHA256SUMS, install.sh}`.
`cargo xtask package <os>` writes `target/dist/<version>/installers/`.
