"""Small in-process progress contract shared by resumable local jobs."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Literal

JobState = Literal[
    "running",
    "pause_requested",
    "paused",
    "stop_requested",
    "cancelled",
    "finished",
    "failed",
]


@dataclass(frozen=True)
class JobProgressUpdate:
    """One bounded progress event; persistence and resume belong to each job owner."""

    job_id: str
    state: JobState
    stage: str
    completed: int | None
    total: int | None
    reason: str | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.job_id, str) or not self.job_id or len(self.job_id) > 128:
            raise ValueError("job_id must be non-empty text no longer than 128 characters")
        if not isinstance(self.state, str) or self.state not in {
            "running",
            "pause_requested",
            "paused",
            "stop_requested",
            "cancelled",
            "finished",
            "failed",
        }:
            raise ValueError("job state is unsupported")
        if not isinstance(self.stage, str) or not self.stage or len(self.stage) > 64:
            raise ValueError("stage must be non-empty text no longer than 64 characters")
        for name, value in (("completed", self.completed), ("total", self.total)):
            if value is not None and (type(value) is not int or value < 0):
                raise ValueError(f"{name} must be a nonnegative integer or None")
        if self.reason is not None and (
            not isinstance(self.reason, str) or len(self.reason) > 500
        ):
            raise ValueError("reason must be text no longer than 500 characters or None")


JobProgressCallback = Callable[[JobProgressUpdate], None]
