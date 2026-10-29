# Normalisation rules for captured fixtures

`gen/capture_tools.py` saves the Python oracle's JSON-RPC results **as received**.
Only one value on the wire is unstable, so only one rule exists.

## Rules

1. **`serverInfo.version` → `{VERSION}`.** The Python `mcp` SDK version (the server
   reports the SDK's version, not SpiderHead's). Every string on the wire exactly
   equal to the captured `serverInfo.version` is replaced with the literal
   `{VERSION}`. Consumers that diff against a fixture substitute their own constant
   back before comparing. Nothing else is rewritten: `protocolVersion`, tool names,
   descriptions, schemas and error texts are saved verbatim.

2. **Non-determinism is a generator bug.** Two consecutive runs must be
   byte-identical; `capture_tools.py --check` enforces it against the committed
   files. Key order is preserved as the oracle emits it (`serde_json` is built with
   `preserve_order`, so the Rust side round-trips it faithfully).

## What is intentionally NOT captured

- Per-request `id` numbers inside saved `tools_call.*` fixtures are whatever the
  capture session used; diff tests must compare the `result`/`error` member, not
  the whole envelope.
- `initialize` differs in no way across the five profiles; a single
  `initialize.json` (from the `full` session, asserted identical) is committed.
