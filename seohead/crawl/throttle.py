"""Politeness that reacts to the origin, not just to a configured ceiling.

Measured on a real shared-hosting catalogue: under a polite 1.5 URL/s the origin
degraded from 1 196 ms to 16 455 ms TTFB and then began refusing TLS handshakes,
without ever returning an error status. A throttle that only widens on non-200
would have kept pushing, so latency widens the delay and a timeout widens it
hard — a timeout is the strongest signal available, never a reason to retry
immediately.

The same object also tracks how many requests may be in flight to this origin
at once. That number starts conservative and only widens on sustained success;
a timeout or a server refusal collapses it back to one immediately — the same
asymmetry the delay itself uses, and for the same reason: fetching more from an
origin that just showed it is struggling turns an audit into a load test.

Both the delay and the concurrency level are read and mutated from whichever
thread is fetching the corresponding URL when the crawler runs several requests
at once, so every mutating method is serialized behind one lock.

The two adapt together, not independently: the delay is nudged toward
``latency / concurrency``, not toward raw latency. At concurrency 1 that is
just latency, unchanged from a strictly sequential crawler. At a higher earned
concurrency it is the spacing that keeps that many requests landing back to
back — the pacing a level-batched crawler needs to turn overlapped wait time
into real throughput instead of quietly re-serializing dispatch to the same
one-at-a-time rate.
"""

from __future__ import annotations

import math
import threading
import time
from collections.abc import Callable
from typing import Any

TIMEOUT_PENALTY = 4.0
MAX_DELAY_S = 60.0

# How many consecutive good responses it takes to trust the origin with one
# more concurrent request. Slow to grow, fast to collapse.
WIDEN_AFTER_CONSECUTIVE_OK = 3


class RequestBudgetExhausted(RuntimeError):
    """The configured total HTTP-attempt budget was consumed before dispatch."""


class DispatchGate:
    """Thread-safe shared dispatch clock for every request to one origin."""

    def __init__(
        self,
        throttle: Throttle,
        sleeper: Callable[[float], None],
        clock: Callable[[], float] = time.monotonic,
        max_requests: int = 0,
        requests_used: int = 0,
        event_callback: Callable[[str, dict[str, Any]], None] | None = None,
    ) -> None:
        self._throttle = throttle
        self._sleeper = sleeper
        self._clock = clock
        self._lock = threading.Lock()
        self._last_at: float | None = None
        if type(max_requests) is not int or max_requests < 0:
            raise ValueError("request budget must be a nonnegative integer")
        if type(requests_used) is not int or requests_used < 0:
            raise ValueError("request count must be a nonnegative integer")
        if max_requests and requests_used > max_requests:
            raise ValueError("request count exceeds configured budget")
        self._max_requests = max_requests
        self._requests_used = requests_used
        self._event_callback = event_callback

    @property
    def throttle(self) -> Throttle:
        """The live throttle whose delay this gate reserves against."""
        return self._throttle

    @property
    def requests_used(self) -> int:
        with self._lock:
            return self._requests_used

    @property
    def max_requests(self) -> int:
        """The configured total-attempt budget; 0 means uncapped."""
        return self._max_requests

    def restore_requests_used(self, value: int) -> None:
        if (
            type(value) is not int
            or value < 0
            or (self._max_requests and value > self._max_requests)
        ):
            raise ValueError("request count is outside configured budget")
        with self._lock:
            self._requests_used = value

    def wait_turn(self) -> None:
        with self._lock:
            if self._max_requests and self._requests_used >= self._max_requests:
                raise RequestBudgetExhausted("total HTTP request budget exhausted")
            now = self._clock()
            # Read the current delay when reserving a turn: a timeout or a newly
            # learned robots delay must apply to the next request, not one later.
            start_at = (
                now if self._last_at is None else max(now, self._last_at + self._throttle.delay)
            )
            self._last_at = start_at
            self._requests_used += 1
            wait = start_at - now
            if self._event_callback is not None:
                self._event_callback(
                    "throttle",
                    {
                        "delay_ms": round(self._throttle.delay * 1000),
                        "concurrency": self._throttle.concurrency,
                        "state": "dispatch",
                    },
                )
        if wait > 0:
            self._sleeper(wait)


class Throttle:
    """Adaptive delay and concurrency for one origin."""

    def __init__(
        self,
        start_delay: float = 0.0,
        min_delay: float = 0.0,
        max_delay: float = MAX_DELAY_S,
        max_concurrency: int = 1,
        adaptive: bool = True,
    ) -> None:
        self._min_delay = max(0.0, float(min_delay))
        self._max_delay = max(self._min_delay, float(max_delay))
        self.delay = max(self._min_delay, float(start_delay))
        self.timeouts = 0
        self.server_errors = 0
        # The ceiling is a configured, bounded fact; ``concurrency`` is what the
        # origin has earned so far, never more than the ceiling allows.
        self.max_concurrency = max(1, int(max_concurrency))
        self.concurrency = min(2, self.max_concurrency)
        self._consecutive_ok = 0
        # speed.adaptive. When False the delay stays exactly where it was configured and the
        # concurrency stays at the earned starting level: the crawl is paced by the operator,
        # not by the origin. The timeout and server-error counters keep running either way —
        # they are a separate "give up" mechanism, and a non-adaptive crawl still has to stop
        # when the origin stops answering rather than keep hammering it.
        self.adaptive = bool(adaptive)
        if not self.adaptive:
            self.concurrency = self.max_concurrency
        self._lock = threading.Lock()

    @property
    def min_delay(self) -> float:
        return self._min_delay

    @min_delay.setter
    def min_delay(self, value: float) -> None:
        """Raising the floor above the current ceiling raises the ceiling too.

        A robots.txt ``Crawl-delay`` above ``max_delay_seconds`` (issue #150) is the
        motivating case: every adaptive clamp below is ``min(max_delay, max(min_delay,
        x))``, which silently collapses to ``max_delay`` forever once the floor
        exceeds it. Keeping the invariant here, on assignment, means no call site —
        today's or a future one — has to remember to touch both fields by hand.
        """
        self._min_delay = max(0.0, float(value))
        if self._max_delay < self._min_delay:
            self._max_delay = self._min_delay

    @property
    def max_delay(self) -> float:
        return self._max_delay

    @max_delay.setter
    def max_delay(self, value: float) -> None:
        self._max_delay = max(0.0, float(value))
        if self._min_delay > self._max_delay:
            self._min_delay = self._max_delay

    def record_response(self, latency_s: float, ok: bool) -> None:
        """Fold one completed response into the delay and the concurrency level.

        A non-2xx response may raise the delay but never lower it: a fast 500 is
        not evidence that the origin is healthy.
        """
        with self._lock:
            if not self.adaptive:
                return
            target = max(latency_s, 0.0) / self.concurrency
            new_delay = (self.delay + target) / 2
            if not ok:
                new_delay = max(new_delay, self.delay)
            self.delay = min(self.max_delay, max(self.min_delay, new_delay))
            if ok:
                self._consecutive_ok += 1
                if (
                    self._consecutive_ok >= WIDEN_AFTER_CONSECUTIVE_OK
                    and self.concurrency < self.max_concurrency
                ):
                    self.concurrency += 1
                    self._consecutive_ok = 0
            else:
                self._consecutive_ok = 0

    def record_timeout(self) -> None:
        """A connection, TLS or read timeout: the origin is failing, back off hard."""
        with self._lock:
            self.timeouts += 1
            if not self.adaptive:
                return
            base = max(self.delay, self.min_delay, 0.5)
            self.delay = min(self.max_delay, base * TIMEOUT_PENALTY)
            self._consecutive_ok = 0
            self.concurrency = 1

    def should_stop(self, limit: int = 5) -> bool:
        """Consecutive timeouts mean the origin is down; stop rather than hammer it.

        The count is shared across every concurrent worker: three of four
        workers seeing a timeout is the same "origin is failing" signal as one
        worker seeing three in a row.
        """
        with self._lock:
            return self.timeouts >= limit

    def record_success(self) -> None:
        with self._lock:
            self.timeouts = 0
            self.server_errors = 0

    def record_server_error(self, status_code: int, retry_after: float | None = None) -> None:
        """A host answering 429 or 5xx is already struggling.

        Treat a single 429 as an overload signal rather than a retryable blip:
        it is the server explicitly asking for less, and continuing at the same
        rate turns an audit into a load test.
        """
        with self._lock:
            self.server_errors += 1
            if not self.adaptive:
                return
            base = max(self.delay, self.min_delay, 0.5)
            widened = base * TIMEOUT_PENALTY if status_code == 429 else base * 2
            if retry_after is not None:
                widened = max(widened, retry_after)
            self.delay = min(self.max_delay, widened)
            self._consecutive_ok = 0
            self.concurrency = 1

    def host_is_failing(self, limit: int = 5) -> bool:
        """Consecutive server refusals mean stop, not retry harder.

        Shared across workers for the same reason as ``should_stop``: the
        signal is about the origin, not about which worker happened to see it.
        """
        with self._lock:
            return self.server_errors >= limit

    def set_rate(self, min_delay: float, concurrency: int | None = None) -> None:
        """Apply an operator's live politeness change to the next dispatched request.

        The delay starts at the new floor: the operator, not earlier adaptive
        back-off, decides the pace from here. Adaptive back-off still applies
        afterwards when the origin starts to struggle.
        """
        with self._lock:
            self.min_delay = min_delay
            self.delay = self.min_delay
            if concurrency is not None:
                self.max_concurrency = max(1, int(concurrency))
                self.concurrency = (
                    min(self.concurrency, self.max_concurrency)
                    if self.adaptive
                    else self.max_concurrency
                )

    def snapshot_state(self) -> dict[str, float | int]:
        """Return the adaptive state persisted independently of circuit streaks.

        Timeout and server-refusal streaks belong to the crawler's deterministic
        queue-order fold, not this completion-order adaptive throttle state.
        Once concurrency has reached its configured ceiling, additional successes
        cannot widen it further; cap their persisted streak at the largest value
        that can still affect a future response. A timeout, refusal or failed
        response resets the live streak either way.
        """
        with self._lock:
            return {
                "delay_seconds": self.delay,
                "concurrency": self.concurrency,
                "consecutive_ok": min(self._consecutive_ok, WIDEN_AFTER_CONSECUTIVE_OK - 1),
            }

    def restore_state(self, state: dict[str, Any]) -> None:
        """Restore a validated :meth:`snapshot_state` payload.

        The payload is deliberately closed: accepting a missing/defaulted field
        would turn a resumed crawl into a different adaptive policy.
        """
        if not isinstance(state, dict):
            raise ValueError("invalid throttle state keys")
        if state.get("schema_version") in {"scan_throttle.v1", "scan_throttle.v2"}:
            allowed = {
                "schema_version",
                "delay_seconds",
                "concurrency",
                "consecutive_ok",
            }
            if state["schema_version"] == "scan_throttle.v2":
                allowed.add("requests_used")
            if set(state) != allowed:
                raise ValueError("invalid throttle state keys")
            state = {
                key: value
                for key, value in state.items()
                if key in {"delay_seconds", "concurrency", "consecutive_ok"}
            }
        if set(state) != {
            "delay_seconds",
            "concurrency",
            "consecutive_ok",
        }:
            raise ValueError("invalid throttle state keys")
        delay = state["delay_seconds"]
        concurrency = state["concurrency"]
        consecutive_ok = state["consecutive_ok"]
        if type(delay) not in (int, float) or not math.isfinite(float(delay)):
            raise ValueError("throttle delay_seconds must be finite")
        if type(concurrency) is not int or concurrency < 1:
            raise ValueError("throttle concurrency must be a positive integer")
        if type(consecutive_ok) is not int or not 0 <= consecutive_ok < WIDEN_AFTER_CONSECUTIVE_OK:
            raise ValueError("throttle consecutive_ok is outside supported bounds")
        if float(delay) < 0:
            raise ValueError("throttle delay_seconds cannot be negative")
        # A live operator rate change (``set_rate``) may have left the persisted
        # state outside the configured bounds; a resume returns to the configured
        # politeness rather than refusing the scan.
        with self._lock:
            self.delay = min(self.max_delay, max(self.min_delay, float(delay)))
            self.concurrency = (
                min(concurrency, self.max_concurrency) if self.adaptive else self.max_concurrency
            )
            self._consecutive_ok = consecutive_ok
