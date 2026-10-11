"""Bounded Chromium lab coverage through Playwright's CDP session.

Only the current page's navigation window is observed. No source is retained;
the returned summaries use UTF-16 source offsets, never transferred bytes.
CDP protocol replies materialize before these local acceptance limits apply;
these limits do not bound the browser's native memory or protocol reply size.
"""

from __future__ import annotations

from contextlib import suppress
from typing import Any

from seohead.checks.lab_coverage import analyze_css_coverage, analyze_js_coverage

MAX_SOURCES = 60
MAX_SOURCE_CHARS = 2_000_000
MAX_TOTAL_CHARS = 8_000_000
MAX_METADATA_ENTRIES = 2_000
MAX_RANGES_PER_SOURCE = 10_000
MAX_TOTAL_RANGES = 50_000
MAX_URL_CHARS = 2_048
MAX_ID_CHARS = 256


def _bounded_name(value: Any, limit: int) -> bool:
    return isinstance(value, str) and 0 < len(value) <= limit


def _offset(value: Any) -> bool:
    return type(value) is int and value >= 0


class BrowserCoverage:
    def __init__(self, page: Any, engine: str) -> None:
        self.session = None
        self.styles: dict[str, dict[str, Any]] = {}
        self.reason = "Chromium CDP is required for native rendered-page coverage"
        self.omitted = 0
        self.metadata_omitted = 0
        self.ranges_omitted = 0
        if engine != "chromium":
            return
        try:
            self.session = page.context.new_cdp_session(page)
            self.session.send("DOM.enable")
            self.session.send("CSS.enable")
            self.session.on("CSS.styleSheetAdded", self._style_added)
            self.session.send("CSS.startRuleUsageTracking")
            self.session.send("Profiler.enable")
            self.session.send("Debugger.enable")
            self.session.send(
                "Profiler.startPreciseCoverage", {"callCount": True, "detailed": True}
            )
            self.reason = ""
        except Exception:
            self.reason = "native render could not start Chromium JS/CSS coverage"
            self.close()

    def _style_added(self, event: dict[str, Any]) -> None:
        header = event.get("header") or {}
        ident = header.get("styleSheetId")
        url = header.get("sourceURL") or "(inline)"
        size = header.get("length")
        if (
            len(self.styles) >= MAX_SOURCES
            or not _bounded_name(ident, MAX_ID_CHARS)
            or not _bounded_name(url, MAX_URL_CHARS)
            or not _offset(size)
        ):
            self.omitted += 1
            self.metadata_omitted += 1
        else:
            # Retain only the three fields needed below, not arbitrary CDP metadata.
            self.styles[ident] = {"sourceURL": url, "length": size}

    def close(self) -> None:
        if self.session is not None:
            with suppress(Exception):
                self.session.detach()
            self.session = None

    def finish(self) -> dict[str, Any]:
        if self.reason or self.session is None:
            return {"state": "unavailable", "reason": self.reason}
        try:
            scripts = self.session.send("Profiler.takePreciseCoverage")["result"]
            rules = self.session.send("CSS.stopRuleUsageTracking")["ruleUsage"]
            js, css = [], []
            total = range_count = 0
            if len(scripts) > MAX_METADATA_ENTRIES:
                self.metadata_omitted += len(scripts) - MAX_METADATA_ENTRIES
            for index in range(min(len(scripts), MAX_METADATA_ENTRIES)):
                script = scripts[index]
                url = script.get("url")
                if not url or url.startswith("__playwright"):
                    continue
                functions = script.get("functions") or []
                if (
                    len(js) >= MAX_SOURCES
                    or not _bounded_name(url, MAX_URL_CHARS)
                    or not _bounded_name(script.get("scriptId"), MAX_ID_CHARS)
                    or len(functions) > MAX_RANGES_PER_SOURCE
                ):
                    self.omitted += 1
                    self.metadata_omitted += 1
                    continue
                ranges = []
                invalid = False
                for function in functions:
                    items = function.get("ranges") or []
                    if (
                        len(ranges) + len(items) > MAX_RANGES_PER_SOURCE
                        or range_count + len(ranges) + len(items) > MAX_TOTAL_RANGES
                    ):
                        self.ranges_omitted += len(ranges) + len(items)
                        invalid = True
                        break
                    for item in items:
                        start, end, count = (
                            item.get("startOffset"),
                            item.get("endOffset"),
                            item.get("count", 0),
                        )
                        if not all(_offset(value) for value in (start, end, count)) or end < start:
                            self.metadata_omitted += 1
                            invalid = True
                            break
                        ranges.append({"startOffset": start, "endOffset": end, "count": count})
                    if invalid:
                        break
                if invalid:
                    self.omitted += 1
                    continue
                size = max((r["endOffset"] for r in ranges), default=0)
                if size > MAX_SOURCE_CHARS or total + size > MAX_TOTAL_CHARS:
                    self.omitted += 1
                    continue
                source = self.session.send(
                    "Debugger.getScriptSource", {"scriptId": script["scriptId"]}
                )["scriptSource"]
                actual = len(source.encode("utf-16-le", "surrogatepass")) // 2
                if actual > MAX_SOURCE_CHARS or total + actual > MAX_TOTAL_CHARS:
                    self.omitted += 1
                    continue
                total += actual
                range_count += len(ranges)
                js.append({"url": url, "functions": [{"ranges": ranges}], "source": source})
            # Dropping CSS ranges could mislabel used CSS as unused. If the global
            # range payload exceeds the remaining budget, omit CSS as a whole.
            if len(rules) > MAX_TOTAL_RANGES - range_count:
                self.ranges_omitted += len(rules)
                self.omitted += len(self.styles)
                styles = {}
            else:
                styles = self.styles
            ranges_by_style: dict[str, list[dict[str, int]]] = {}
            invalid_styles: set[str] = set()
            for rule in rules if styles else []:
                ident = rule.get("styleSheetId")
                if not _bounded_name(ident, MAX_ID_CHARS):
                    self.metadata_omitted += 1
                    continue
                if ident not in styles:
                    continue
                start, end = rule.get("startOffset"), rule.get("endOffset")
                if (
                    not _offset(start)
                    or not _offset(end)
                    or end < start
                    or type(rule.get("used")) is not bool
                ):
                    invalid_styles.add(ident)
                    self.metadata_omitted += 1
                    continue
                items = ranges_by_style.setdefault(ident, [])
                if len(items) >= MAX_RANGES_PER_SOURCE:
                    invalid_styles.add(ident)
                    self.ranges_omitted += 1
                else:
                    items.append({"start": start, "end": end, "used": rule["used"]})
            for ident, header in styles.items():
                size = header["length"]
                if (
                    ident in invalid_styles
                    or size > MAX_SOURCE_CHARS
                    or total + size > MAX_TOTAL_CHARS
                ):
                    self.omitted += 1
                    continue
                text = self.session.send("CSS.getStyleSheetText", {"styleSheetId": ident})["text"]
                actual = len(text.encode("utf-16-le", "surrogatepass")) // 2
                if actual > MAX_SOURCE_CHARS or total + actual > MAX_TOTAL_CHARS:
                    self.omitted += 1
                    continue
                total += actual
                css.append(
                    {
                        "url": header["sourceURL"],
                        "text": text,
                        "ranges": [
                            {"start": r["start"], "end": r["end"]}
                            for r in ranges_by_style.get(ident, [])
                            if r["used"]
                        ],
                    }
                )
            partial = bool(self.omitted or self.metadata_omitted or self.ranges_omitted)
            return {
                "state": "partial" if partial else "complete",
                "reason": "native render coverage source, metadata or range budget exceeded or invalid"
                if partial
                else "",
                "sources_omitted": self.omitted,
                "metadata_omitted": self.metadata_omitted,
                "ranges_omitted": self.ranges_omitted,
                "unit": "UTF-16 code units",
                "scope": "named scripts and captured stylesheets in the current page CDP session; workers and out-of-process frames are not measured",
                "limit_scope": "local acceptance and analysis only; CDP replies materialize before limits, and browser native memory is not capped",
                "js": analyze_js_coverage(js),
                "css": analyze_css_coverage(css),
            }
        except Exception:
            return {
                "state": "unavailable",
                "reason": "native render could not read Chromium JS/CSS coverage",
            }
        finally:
            self.close()
