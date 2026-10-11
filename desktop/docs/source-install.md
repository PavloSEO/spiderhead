# Install and launch from source

Use Python 3.11 or newer (3.12 is exercised) and
[uv](https://docs.astral.sh/uv/getting-started/installation/). The bootstrap
creates one project virtual environment containing Desktop, its agent CLI and
the compatible SEOHEAD core. It never installs into global Python, registers a
service, downloads browser binaries, configures credentials or starts a scan.

Desktop currently has no published Git remote. These commands start from an
existing Desktop source checkout; no public clone URL is implied. The single
tested core pin is in `packaging/source-install.json`. An existing core checkout
must be clean and match that revision. Keep a working core checkout on its own
branch; clone a separate source directory for this install when needed:

```bash
git clone https://github.com/PavloSEO/seohead-tools.git /path/to/desktop-core
git -C /path/to/desktop-core checkout "$(python3.12 -c 'import json; print(json.load(open("packaging/source-install.json"))["core_commit"])')"
python3.12 scripts/source.py install --core-source /path/to/desktop-core --dry-run
python3.12 scripts/source.py install --core-source /path/to/desktop-core
python3.12 scripts/source.py run
```

Run these from the Desktop checkout. On Windows use `py -3.12` for Python and
read the JSON pin before `git checkout`; the same Python script chooses
`Scripts/*.exe`. Linux and Windows source installation still require native
runtime acceptance. PyQt may require OS display libraries on Linux.

`install` refuses an existing environment. Use `--venv /new/path` if `.venv`
already belongs to another workflow. Re-running with `--update` is permitted
only for an environment this script created for this checkout; this also
resumes a failed dependency download. Editable source changes apply on the
next launch. Upgrading the supported core requires updating the one JSON pin
after compatibility tests, rather than bypassing its check.

The optional core dependencies selected by `core_extras` are installed as
Python packages. JavaScript browser binaries and external provider credentials
remain separate setup. Neither is fetched or configured by this script.

## Open a project, capture a log, connect an agent

```bash
python3.12 scripts/source.py run --project /path/to/existing/project
python3.12 scripts/source.py run --project /path/to/existing/project \
  --agent-control /path/to/existing/runtime --log /path/to/new-desktop.log
```

`--project .` selects the current directory explicitly. The project is opened
read-only; scan admission still uses the application's confirmation. Every
launch writes a new log under `.build/logs/` unless `--log` selects a new file.
An existing log is never overwritten. Logs are mode `0600` on Unix and contain
startup output, application stderr and the exit code. Keep them private when
working with client projects. The command stays in the foreground and returns
the application's exit code.

Agent control is opt-in. Its runtime parent must already exist and belong to
the current user; use a short path on Unix because local sockets have a path
length limit. Desktop reports its newly created `control.json` path in the
connection dialog. Pass only that path to the agent CLI; never copy its token:

```bash
.venv/bin/seohead-desktop-agent --endpoint /path/to/private-instance/control.json status
.venv/bin/seohead-desktop-agent --endpoint /path/to/private-instance/control.json mcp
.venv/bin/seohead --version
.venv/bin/seohead mcp
```

On Windows these are `.venv\Scripts\seohead-desktop-agent.exe` and
`.venv\Scripts\seohead.exe`. The Desktop agent controls one running window;
the core MCP is the separate CLI/MCP interface to the existing toolkit.
See [local-control.md](local-control.md) for the bounded commands and explicit
authorization required for scan mutations.

## Fast development and release builds

`source.py run` starts the current editable code without freezing a bundle.
For a macOS bundle preview, use `build_macos.sh --incremental` as documented in
[packaging.md](packaging.md). A release uses a clean source checkout and the
default clean build. Preview bundles retain a source inventory and are labeled
`developer-preview`; they are not release artifacts.

## Source publication boundary

Before creating a Desktop remote or publishing a source archive, review the
tracked source inventory and imported design inputs, remove private machine
paths and internal instructions, verify asset redistribution rights, and
approve the public repository name. The current local Git checkout and tests
do not constitute that publication approval. No script creates a remote,
pushes commits, uploads artifacts or replaces an installed application.
