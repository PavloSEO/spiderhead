"""Common progress values stay typed while each resumable job owns its checkpoint."""

import pytest

from seohead.core.job_control import JobProgressUpdate


def test_job_progress_update_accepts_unknown_total_and_missing_count():
    update = JobProgressUpdate(
        job_id="compare-1",
        state="running",
        stage="compare-pages",
        completed=12,
        total=None,
    )
    assert update.total is None
    assert update.completed == 12


@pytest.mark.parametrize(
    "values",
    [
        {"job_id": "", "state": "running", "stage": "crawl", "completed": 0, "total": 0},
        {"job_id": "x", "state": "waiting", "stage": "crawl", "completed": 0, "total": 0},
        {"job_id": "x", "state": "running", "stage": "", "completed": 0, "total": 0},
        {"job_id": "x", "state": "running", "stage": "crawl", "completed": -1, "total": 0},
        {"job_id": "x", "state": "running", "stage": "crawl", "completed": 0, "total": True},
    ],
)
def test_job_progress_rejects_ambiguous_or_unbounded_fields(values):
    with pytest.raises(ValueError):
        JobProgressUpdate(**values)
