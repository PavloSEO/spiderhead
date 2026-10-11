"""Live crawl progress: measured rate and an estimate over the current frontier.

A native crawl of a large site used to print one rate line and then nothing for
an hour (issue #619). Everything an operator needs was already in the spider --
the frontier, the fetched count, the URL budget -- and none of it reached them.

The one thing this module refuses to do is imply a total it does not have. A
crawler discovers its own workload: the frontier grows as links are found, so
"25%" and the ETA describe only what is known *now*, and the set they describe
will very likely be larger a minute from now. That is said in a header line
printed once, and the population is labelled "known", never "total".

The denominator is ``fetched + queued``, capped at the URL budget:

* excluded URLs (out of scope, robots-blocked, over a limit) are not remaining
  work, so counting them would leave the percentage permanently short of 100
  and make a finished crawl read as a stalled one;
* the budget is a real ceiling -- with ``max_urls=200`` a crawl that has found
  34 000 URLs will still fetch 200 -- so an uncapped denominator would report
  0% on a crawl that is one page from stopping.

Either can move the denominator down (an exclusion drops a queued URL), so
``known`` is not monotonic. That is the truth about the crawl rather than a
smoothed number, and every value printed describes the state that produced it.

Output goes to a caller-supplied stream (stderr for the CLI, so the JSON result
on stdout stays clean). On a TTY the line is redrawn in place with a carriage
return; on anything else -- a pipe, a log file, CI -- plain lines are printed on
a much longer interval instead, because a log full of carriage returns is worse
than no progress at all.
"""

from __future__ import annotations

import os
import time
from collections import deque
from collections.abc import Callable
from math import ceil
from typing import TextIO

# How often the line is refreshed. A redraw is cheap and the eye wants motion;
# a log file wants one line every so often, not one per page.
TTY_INTERVAL_SECONDS = 0.5
STREAM_INTERVAL_SECONDS = 30.0

# The rate is measured over a trailing window rather than the whole run: an
# operator watching a crawl slow down needs the rate now, not the average since
# a fast start an hour ago.
RATE_WINDOW_SECONDS = 15.0


def _format_elapsed(seconds: float) -> str:
    whole = int(max(0.0, seconds))
    if whole < 60:
        return f"{whole}s"
    if whole < 3600:
        return f"{whole // 60}m{whole % 60:02d}s"
    return f"{whole // 3600}h{(whole % 3600) // 60:02d}m"


def _format_bytes(size: int) -> str:
    if size < 1024:
        return f"{size} B"
    value = size / 1024
    for unit in ("KB", "MB", "GB"):
        if value < 1024 or unit == "GB":
            return f"{value:.1f} {unit}"
        value /= 1024
    raise AssertionError("unreachable")  # pragma: no cover - the loop always returns


class CrawlProgress:
    """A progress line for one crawl. Call it with ``(fetched, queued)``; close it at the end.

    It *is* the progress callback rather than something a callback wraps, so the
    crawlers take one optional argument and know nothing about terminals.

    ``artifact_path`` is the SQLite scan being written, when there is one. Its
    ``-wal`` sidecar is counted alongside it: under WAL journalling the pages a
    running crawl has committed sit in that sidecar, and reporting the main file
    alone would show a size frozen for minutes at a time on a crawl that is
    working perfectly.
    """

    def __init__(
        self,
        stream: TextIO,
        *,
        budget: int,
        tty: bool,
        artifact_path: str | None = None,
        interval: float | None = None,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.stream = stream
        self.budget = budget
        self.tty = tty
        self.artifact_path = artifact_path
        default_interval = TTY_INTERVAL_SECONDS if tty else STREAM_INTERVAL_SECONDS
        self.interval = default_interval if interval is None else interval
        self.clock = clock
        self._started = clock()
        self._samples: deque[tuple[float, int]] = deque()
        self._last_report: tuple[int, int] | None = None
        self._last_emit: float | None = None
        self._painted = 0  # width of the line currently on screen, for TTY erasure
        self._header_printed = False
        self._live = True  # cleared when the stream stops accepting writes

    # -- public API ---------------------------------------------------------

    def __call__(self, fetched: int, queued: int) -> None:
        """Report crawl state: ``fetched`` pages stored, ``queued`` URLs known and unfetched."""
        self._render(fetched, queued, force=False)

    def close(self) -> None:
        """Repaint the last reported state unthrottled, then leave a fresh row.

        Nothing is printed when the crawl never reported anything: a run that
        failed before its first fetch must not leave a "0 fetched" line implying
        it got as far as measuring. The numbers repainted here are the last ones
        a crawler actually reported -- none are invented at the end of a run.
        """
        if self._last_report is None:
            return
        self._render(*self._last_report, force=True)
        if self.tty:
            self._write("\n")
            self._painted = 0

    # -- rendering ----------------------------------------------------------

    def _render(self, fetched: int, queued: int, *, force: bool) -> None:
        if not self._live:
            return
        now = self.clock()
        self._last_report = (fetched, queued)
        self._samples.append((now, fetched))
        while len(self._samples) > 2 and now - self._samples[0][0] > RATE_WINDOW_SECONDS:
            self._samples.popleft()
        if not force and self._last_emit is not None and now - self._last_emit < self.interval:
            return
        self._last_emit = now
        if not self._header_printed:
            self._header_printed = True
            self._write(self._header() + "\n")
        line = self.line(fetched, queued, elapsed=now - self._started, rate=self._rate())
        if self.tty:
            self._write("\r" + line + " " * max(0, self._painted - len(line)))
            self._painted = len(line)
        else:
            self._write(line + "\n")

    def _header(self) -> str:
        population = (
            f"capped at the {self.budget}-URL budget" if self.budget else "without a URL limit"
        )
        return (
            "crawl-site: progress below is pages fetched of the URLs known so far "
            f"(fetched + queued, {population}). The frontier grows "
            "as links are found, so the percentage and ETA cover only the current known frontier, "
            "not the whole site or total crawl duration."
        )

    def line(self, fetched: int, queued: int, *, elapsed: float, rate: float | None) -> str:
        """The progress line itself, kept separate from writing it so a test can read it."""
        known = min(fetched + queued, self.budget) if self.budget else fetched + queued
        percent = f"{100 * fetched // known}%" if known > 0 else "n/a"
        fields = [
            f"{fetched} fetched",
            f"{known} known ({percent})",
            "rate n/a" if rate is None else f"{rate:.1f} req/s",
            (
                f"known ETA ~{_format_elapsed(ceil((known - fetched) / rate))}"
                if rate is not None and rate > 0
                else "known ETA n/a"
            ),
            _format_elapsed(elapsed),
        ]
        artifact = self._artifact_bytes()
        if artifact is not None:
            fields.append(f"scan {_format_bytes(artifact)}")
        return "crawl-site: " + ", ".join(fields)

    def _rate(self) -> float | None:
        """Requests per second over the trailing window, or None while unmeasurable.

        A rate needs two samples and a non-zero span between them. Until there is
        one the field says so, rather than printing 0.0 req/s -- which would read
        as a stalled crawl at the exact moment nothing is yet known about it.
        """
        if len(self._samples) < 2:
            return None
        (first_at, first_fetched), (last_at, last_fetched) = self._samples[0], self._samples[-1]
        span = last_at - first_at
        return (last_fetched - first_fetched) / span if span > 0 else None

    def _artifact_bytes(self) -> int | None:
        """Bytes the scan artifact occupies, or None when no artifact is being written.

        The figure can fall at the end of a run: SQLite checkpoints the WAL back
        into the file and the sidecar goes away, so a crawl that showed 960 KB
        mid-run finishes at 240 KB. That is the artifact getting smaller, not
        evidence being lost.
        """
        if not self.artifact_path:
            return None
        total = 0
        for path in (self.artifact_path, self.artifact_path + "-wal"):
            try:
                total += os.stat(path).st_size
            except OSError:
                continue  # not created yet, or already checkpointed away
        return total

    def _write(self, text: str) -> None:
        try:
            self.stream.write(text)
            self.stream.flush()
        except (OSError, ValueError):
            # A closed or broken stderr (a killed `head`, a detached terminal)
            # must not take the crawl down with it: the crawl is the point, the
            # progress line is not.
            self._live = False
