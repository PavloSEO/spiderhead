# Contributing

Thanks for helping. SpiderHead is a Rust workspace; the CLI is the primary surface. By taking part you
agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately as described
in [SECURITY.md](SECURITY.md).

## Workflow

1. Open an issue (bug report or feature request) or pick an existing one.
2. Branch from `main`: `git switch -c <short-topic>`.
3. Make the change with tests; run `cargo xtask ci` and `cargo xtask layers`.
4. Open a pull request against `main` using the template. Keep it focused; one concern per PR.

## Workspace layout

```
spiderhead/            product crates (crate = spiderhead-<dir>)
  core formats store parse checks render fetch crawl log-analyzer
  reports sf-import catalog projects sources semantics recon
  control mcp cli tui remote desktop
oracle/                frozen reference outputs, synthetic sites, structural differ
xtask/                 repo automation
packaging/ docs/       installers, documentation
```

Crates are layered; a crate may depend only on crates of the same or a lower layer. The table lives in `LAYERS` in `xtask/src/main.rs` and is enforced by `cargo xtask layers`. Modules are named after the domain (`checks::hreflang`, `crawl::frontier`), never `utils` or `helpers`.

## Gates

| Command | What it checks |
|---|---|
| `cargo xtask ci` | `fmt --all --check`, `clippy --workspace --all-targets -- -D warnings`, tests (cargo-nextest when installed, otherwise `cargo test`), doctests; ends with `==> ci OK` |
| `cargo xtask layers` | dependency layering |
| `cargo xtask oracle --slice <id> [--site S]` | structural diff of output against the frozen baselines (`--self-test` checks the differ) |
| `cargo xtask dist [--profile P] [--target T]` | release builds into `target/dist/<version>/<profile>/` with `MANIFEST.json` and `SHA256SUMS` |
| `cargo xtask package macos\|linux\|windows` | installers from the dist folders |

Builds use `sccache` (`.cargo/config.toml`), so worktrees share the compile cache. Run `ci` and `layers` before every commit.

## Fixtures and baselines

- **Synthetic sites** under `oracle/sites/` (only `example.com` and `*.test` hosts) are served by a fixture server (`oracle/testkit`).
- **Baselines** in `oracle/baselines/<site>/` are normalised expected outputs with a sha256 manifest. They change only through a reviewed commit, never by hand.
- **Structural diff** — `oracle/diff` compares JSON after the normalisation described in `oracle/NORMALISATION.md`.
- **Known divergences** — `oracle/known-divergences.toml` lists approved differences. Anything else is a bug.

After merging parallel branches, `cargo build` rejects duplicated dependency lines in `Cargo.toml`; merge those lists by hand (union, alphabetical).

## Code rules

- No network in unit tests; mock the transport.
- Commits and PR text in English, imperative mood ("Add bot verdict filter", not "Added ...").
- Use only synthetic data and reserved domains (`example.com`, `*.test`) in tests and examples; never commit credentials, access logs or client data.
- Only add dependencies you need; shared ones go to `[workspace.dependencies]`, alphabetically.
- No `unwrap`/`expect` outside tests; `thiserror` in libraries, `anyhow` only in binaries.

## Comments

- Comments say **why** and **what the code promises**, never what the code literally does or where it came from.
- `//!` per module: 1–3 lines on responsibility. `///` on every public item: one line, then only the contract (errors, units, invariants) when it is not obvious.
- Surprising behaviour kept for format compatibility is stated as the rule itself ("keys keep insertion order: saved scans compare byte-for-byte").
- No commented-out code; `TODO` only with an issue reference.
