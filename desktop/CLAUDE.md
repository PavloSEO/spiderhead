# SEOHEAD Desktop development

Follow the repository's `AGENTS.md`. Desktop is a Python/PyQt5 Qt Widgets presentation adapter in `desktop/`; crawler, analyzer, project and task stores remain in the core package at `seohead/`. Use native widgets and QSS without a browser UI or QtWebEngine.

Use `src/seohead_desktop/theme/tokens.json` as the canonical theme source. Shared native component metrics are documented in `docs/canvas-tokens.md`; bounded table and intent contracts are in `design/tab-contracts.md`. Screens show supplied core data or an explicit empty, partial or unavailable state. Opening a view must not trigger hidden crawl or provider calls. Run blocking core reads from workers.

Preserve existing user files and follow explicit mutation and authorization contracts. Public code and documentation are English; localized UI strings belong to the translation system. Avoid per-row widgets and unbounded data materialization. Record source identities and measured checks; cross-platform, scale and visual acceptance require their own evidence.

From `desktop/`, run the suite with `python scripts/run_tests.py` when verification is authorized. It uses offscreen Qt and exits through `seohead_desktop.qt.exit_now` to avoid PyQt interpreter-finalization crashes. Create QApplication only through `seohead_desktop.qt.app()` and end standalone Qt scripts with `qt.exit_now(code)`.
