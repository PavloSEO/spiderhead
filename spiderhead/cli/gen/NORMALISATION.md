# CLI capture normalisation

Applied by `capture_cli.py` before anything is written. Anything not listed is kept verbatim.

| # | What | Rule | Why |
|---|---|---|---|
| 1 | Toolkit version | the original's `__version__` (e.g. `0.9.0`) -> `{VERSION}` in captured text and in the tree (`python_version`, `--version` text) | the Rust build substitutes its own `TOOLKIT_VERSION`; the tests apply the same rule to Rust output |
| 2 | Home directory | `$HOME` -> `{HOME}` in captured text | absolute paths of the capturing machine |
| 3 | JSON layout | every fixture is written with `indent=1`, `ensure_ascii=False`, UTF-8, trailing newline; records are `sort_keys=True` (the tree keeps argparse registration order) | two runs are byte-identical |

Capture environment (fixed): `COLUMNS=100`, `NO_COLOR=1`, `LC_ALL=C.UTF-8`, `TZ=UTC`, stdin closed.
Help text is *not* rewrapped or trimmed: stdout, stderr and the exit code are compared byte for byte.

Runs use the original's venv interpreter and call `spiderhead.cli.main` in one worker process
(thousands of argparse invocations in seconds). A deterministic sample of ~24 runs is repeated
through the installed `spiderhead` binary on every capture and must agree, otherwise the capture aborts.

Error cases are capped at five per parser, picked in a fixed priority order (unknown flag, unknown
sub-command, missing required, bad choice, bad type, ...); each one is guaranteed to fail during
argument parsing (exit code 2), so no command handler ever runs during capture.
