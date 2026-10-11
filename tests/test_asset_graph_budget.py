"""Resource-budget bookkeeping must not repeatedly scan completed declarations."""

from __future__ import annotations

from seohead.storage.native_scan import NativeScan
from tests.test_batch_resource_graph import _store_html, _v2_metadata


def test_pending_resource_selection_uses_the_sparse_budget_index(tmp_path):
    with NativeScan.create(
        tmp_path / "scan.sqlite", format_version="scan.v2", **_v2_metadata()
    ) as scan:
        _store_html(scan, '<script src="/first.js"></script><script src="/second.js"></script>')
        plan = scan.con.execute(
            "EXPLAIN QUERY PLAN SELECT * FROM resource_graph_occurrences WHERE state='disabled' ORDER BY occurrence_id LIMIT 1"
        ).fetchall()
        assert any("resource_graph_pending" in row[3] for row in plan)
        scan.con.execute(
            "UPDATE resource_graph_occurrences SET state='budget' WHERE occurrence_id=1"
        )
        assert (
            scan.con.execute(
                "SELECT occurrence_id FROM resource_graph_occurrences WHERE state='disabled' ORDER BY occurrence_id LIMIT 1"
            ).fetchone()[0]
            == 2
        )
