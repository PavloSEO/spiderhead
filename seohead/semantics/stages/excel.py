"""excel: free final workbook built from the store.

Sheets: Summary, Core (base/phrase/exact frequency and cluster), Info (blog core), Clusters
with competitors, and the rejection log. Needs openpyxl, a base dependency of the toolkit.
"""

from __future__ import annotations

import collections
from pathlib import Path

from openpyxl import Workbook
from openpyxl.styles import Alignment, Font, PatternFill
from openpyxl.utils import get_column_letter

from seohead.core.tabular import neutralize_formula

HDR = Font(bold=True, color="FFFFFF")
GREY = PatternFill("solid", fgColor="595959")
ZEBRA = PatternFill("solid", fgColor="F2F2F2")
WRAP = Alignment(wrap_text=True, vertical="top")


def _sheet(ws, headers, rows, widths):
    ws.append(headers)
    for c in ws[1]:
        c.font = HDR
        c.fill = GREY
        c.alignment = WRAP
    for i, r in enumerate(rows):
        ws.append([neutralize_formula(value) for value in r])
        if i % 2:
            for c in ws[i + 2]:
                c.fill = ZEBRA
    for j, w in enumerate(widths, 1):
        ws.column_dimensions[get_column_letter(j)].width = w
    ws.freeze_panes = "A2"


def run(store, cfg, out_dir, *, scope="all"):
    db = store.db
    scope_where, scope_params = store.scope_clause(scope, "p")

    proj = cfg["_project"]
    vendor_path = Path(cfg["_dir"]) / cfg.get("vendor_file", "vendors.txt")
    vendors = (
        {
            line.strip().lower().removeprefix("www.")
            for line in vendor_path.read_text(encoding="utf-8").splitlines()
            if line.strip()
        }
        if vendor_path.exists()
        else set()
    )

    total = db.execute("SELECT COUNT(*) FROM phrases p " + scope_where, scope_params).fetchone()[0]
    uniq = db.execute(
        "SELECT COUNT(DISTINCT p.norm) FROM phrases p " + scope_where, scope_params
    ).fetchone()[0]
    dupes = total - uniq
    where = (scope_where + " AND " if scope_where else " WHERE ") + "p.status='kept'"
    core = db.execute("SELECT COUNT(*) FROM phrases p" + where, scope_params).fetchone()[0]
    core_exact10 = db.execute(
        "SELECT COUNT(*) FROM phrases p" + where + " AND p.exact>=10", scope_params
    ).fetchone()[0]
    core_noexact = db.execute(
        "SELECT COUNT(*) FROM phrases p" + where + " AND p.exact IS NULL", scope_params
    ).fetchone()[0]
    n_clusters = db.execute(
        "SELECT COUNT(DISTINCT p.cluster_id) FROM phrases p"
        + where
        + " AND p.cluster_id IS NOT NULL",
        scope_params,
    ).fetchone()[0]
    drop_n = db.execute(
        "SELECT COUNT(*) FROM phrases p "
        + scope_where
        + (" AND " if scope_where else " WHERE ")
        + "p.status='dropped'",
        scope_params,
    ).fetchone()[0]
    rev_n = db.execute(
        "SELECT COUNT(*) FROM phrases p "
        + scope_where
        + (" AND " if scope_where else " WHERE ")
        + "p.status='review'",
        scope_params,
    ).fetchone()[0]
    # Informational phrases are the blog core, not junk: count them separately.
    info_n = db.execute(
        "SELECT COUNT(*) FROM phrases p" + where + " AND p.route='info'", scope_params
    ).fetchone()[0]
    sp = store.spend()

    wb = Workbook()

    ws = wb.active
    ws.title = "Summary"
    _sheet(
        ws,
        ["Metric", "Value"],
        [
            ["Project", proj],
            ["Core (on topic, kept)", core],
            ["  with exact !W >= 10", core_exact10],
            ["  without exact frequency", core_noexact],
            ["Clusters in the core", n_clusters],
            ["Dropped (retained)", drop_n],
            ["In review", rev_n],
            ["Total phrases in the store", total],
            ["Duplicate check (unique norm)", f"{uniq} unique, duplicates: {dupes}"],
            [
                "Provider units",
                f"{sp['yandex_requests']} Yandex requests + {sp['arsenkin']} Arsenkin limits",
            ],
        ],
        [42, 44],
    )

    rows = db.execute(
        "SELECT p.norm, p.base, p.quoted, p.exact, c.name FROM phrases p LEFT JOIN clusters c ON p.cluster_id=c.id "
        + (scope_where + " AND " if scope_where else " WHERE ")
        + "p.status='kept' AND p.route!='info' ORDER BY COALESCE(p.exact,0) DESC, p.base DESC",
        scope_params,
    ).fetchall()
    _sheet(
        wb.create_sheet("Core"),
        ["Phrase", "base W", 'phrase "W"', "exact !W", "cluster"],
        [[r["norm"], r["base"], r["quoted"], r["exact"], r["name"] or ""] for r in rows],
        [50, 12, 14, 12, 34],
    )

    info_rows = db.execute(
        "SELECT p.norm,p.base,p.quoted,p.exact,c.name FROM phrases p LEFT JOIN clusters c ON p.cluster_id=c.id "
        + (scope_where + " AND " if scope_where else " WHERE ")
        + "p.status='kept' AND p.route='info' ORDER BY p.base DESC",
        scope_params,
    ).fetchall()
    _sheet(
        wb.create_sheet("Info"),
        ["Phrase", "base W", 'phrase "W"', "exact !W", "cluster"],
        [[r["norm"], r["base"], r["quoted"], r["exact"], r["name"] or ""] for r in info_rows],
        [50, 12, 14, 12, 34],
    )

    crows = []
    for cl in db.execute(
        "SELECT c.id, c.name, c.coherence, COUNT(p.id) n, SUM(p.exact) se "
        "FROM clusters c JOIN phrases p ON p.cluster_id=c.id "
        + (scope_where + " AND " if scope_where else "WHERE ")
        + "p.status='kept' GROUP BY c.id HAVING n>=2 ORDER BY se DESC, n DESC",
        scope_params,
    ).fetchall():
        doms = collections.Counter(
            x["domain"]
            for x in db.execute(
                "SELECT s.domain FROM serp s JOIN phrases p ON p.norm=s.phrase_norm "
                "WHERE p.cluster_id=? AND s.domain IS NOT NULL",
                (cl["id"],),
            )
        )
        vend = ", ".join(d for d, _ in doms.most_common(20) if not vendors or d in vendors)[:60]
        crows.append([cl["name"], cl["n"], cl["se"], cl["coherence"], vend])
    _sheet(
        wb.create_sheet("Clusters"),
        ["Cluster marker", "phrases", "sum !W", "coherence", "competitors"],
        crows,
        [40, 8, 10, 11, 46],
    )

    rows = db.execute(
        "SELECT norm,base,status,reason,reason_stage FROM phrases WHERE status IN ('dropped','dead','review') ORDER BY base DESC"
        if scope == "all"
        else "SELECT p.norm,p.base,p.status,p.reason,p.reason_stage FROM phrases p "
        + scope_where
        + " AND p.status IN ('dropped','dead','review') ORDER BY p.base DESC",
        scope_params if scope != "all" else (),
    ).fetchall()
    _sheet(
        wb.create_sheet("Rejected"),
        ["Phrase", "base W", "Status", "Reason", "Stage"],
        [[r["norm"], r["base"], r["status"], r["reason"], r["reason_stage"]] for r in rows],
        [50, 12, 14, 40, 16],
    )

    out = Path(out_dir) / f"{proj}_semantic_core.xlsx"
    out.parent.mkdir(parents=True, exist_ok=True)
    wb.save(out)
    store.add_run("excel", note=f"core={core}, sheets={len(wb.worksheets)}")
    print(
        f"excel: {out} | core={core} (exact>=10: {core_exact10}, no exact: {core_noexact}) "
        f"info={info_n} review={rev_n} dropped={drop_n} duplicates={dupes} clusters={n_clusters}"
    )
    return {"core": core, "file": str(out), "scope": scope}
