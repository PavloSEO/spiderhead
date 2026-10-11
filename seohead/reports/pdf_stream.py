"""Explicit bounded PDF overview with complete, atomic audit.v2 companions."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import Any
from urllib.parse import quote

POLICY = "overview-v1"
MAX_DISPLAY_ROWS = 32
MAX_DISPLAY_BYTES = 2 * 1024 * 1024
MAX_METADATA_BYTES = 4 * 1024 * 1024
MAX_OUTPUT_BYTES = 64 * 1024 * 1024 * 1024
DISK_RESERVE_BYTES = 1024 * 1024 * 1024
ARTIFACT_LINK_PREFIX = "https://report-artifact.invalid/"

_COVERAGE_COLLECTIONS = {
    "/run/checks_skipped",
    "/run/checks_disabled",
    "/summary/check_coverage/checks_silent_ids",
    "/summary/check_coverage/checks_disabled_ids",
    "/summary/evidence_contract/capability_rows",
    "/summary/project_coverage/status/items",
}


def _json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":"))


def build_overview_model(
    reader: Any, project_coverage: dict[str, Any] | None = None
) -> dict[str, Any]:
    """Project ordered prefixes, keeping complete source counts and coverage."""
    from .pdf_model import _collection, _coverage_record, build_pdf_model

    projection = {}
    omissions = []
    header = dict(reader.header)
    # Large nested summary populations (for example one histogram bucket per
    # depth) are complete companion data, not display metadata. Reference them
    # before checking the PDF metadata budget rather than imposing a crawl cap.
    required_summary = {
        "totals",
        "by_severity",
        "by_check",
        "check_coverage",
        "evidence_contract",
        "project_coverage",
    }
    header["summary"] = {
        key: {
            "state": "retained_in_companion",
            "reference": "audit.json#/summary/" + key.replace("~", "~0").replace("/", "~1"),
        }
        if key not in required_summary and isinstance(value, (dict, list))
        else value
        for key, value in (reader.header.get("summary") or {}).items()
    }
    if project_coverage is not None:
        header["summary"]["project_coverage"] = project_coverage
    metadata_bytes = len(_json(header).encode("utf-8"))
    if metadata_bytes > MAX_METADATA_BYTES:
        raise ValueError(f"PDF overview metadata exceeds {MAX_METADATA_BYTES} bytes")

    def restore(value):
        nonlocal metadata_bytes
        if isinstance(value, dict) and "$audit_v2_collection" in value:
            pointer = value["$audit_v2_collection"]
            total = reader.count(pointer)
            if pointer in {"/issues", "/pages"}:
                rows = []
                size = 0
                for row in reader.iter_collection(pointer):
                    row_bytes = len(_json(row).encode("utf-8"))
                    if len(rows) == MAX_DISPLAY_ROWS or size + row_bytes > MAX_DISPLAY_BYTES:
                        break
                    rows.append(row)
                    size += row_bytes
                key = "findings" if pointer == "/issues" else "pages"
                projection[key] = {
                    "source": total,
                    "displayed": len(rows),
                    "omitted": total - len(rows),
                    "first_ordinal": 0,
                    "end_ordinal_exclusive": len(rows),
                    "display_bytes": size,
                    "max_rows": MAX_DISPLAY_ROWS,
                    "max_bytes": MAX_DISPLAY_BYTES,
                }
                if total > len(rows):
                    omissions.append(
                        {
                            "collection": pointer,
                            "count": total - len(rows),
                            "reason": "Outside the ordered PDF display prefix; complete records remain in audit.json and CSV companions.",
                        }
                    )
                return rows
            if pointer in _COVERAGE_COLLECTIONS:
                rows = []
                for row in reader.iter_collection(pointer):
                    metadata_bytes += len(_json(row).encode("utf-8"))
                    if metadata_bytes > MAX_METADATA_BYTES:
                        raise ValueError(
                            f"PDF overview coverage metadata exceeds {MAX_METADATA_BYTES} bytes"
                        )
                    rows.append(row)
                return rows
            if total:
                omissions.append(
                    {
                        "collection": pointer,
                        "count": total,
                        "reason": "This retained collection is available in full in audit.json; it is not expanded in the PDF overview.",
                    }
                )
            if pointer in {"/groups", "/suppressed_issues"}:
                return []
            return {
                "state": "retained_in_companion",
                "source_count": total,
                "reference": "audit.json#" + pointer,
            }
        if isinstance(value, dict):
            return {key: restore(child) for key, child in value.items()}
        if isinstance(value, list):
            return [restore(child) for child in value]
        return value

    document = restore(header)
    document.setdefault("groups", [])
    model = build_pdf_model(document)
    # Native audits retain per-check capability records in source evidence;
    # project each one explicitly so the overview can show every state/reason
    # without repeating the entire capability array in one enormous table cell.
    capabilities = (document.get("summary", {}).get("evidence_contract") or {}).get(
        "capability_rows"
    )
    if isinstance(capabilities, list) and "capabilities" not in model["coverage"]["groups"]:
        group = _collection(
            capabilities, "summary.evidence_contract.capability_rows", record_kind="mapping"
        )
        model["coverage"]["groups"]["capabilities"] = group
        model["coverage"]["checks"].extend(
            _coverage_record(
                "summary.evidence_contract.capability_rows",
                index,
                record,
                record.get("state", "unreported"),
                identifier=record.get("check"),
                reason=record.get("reason"),
            )
            for index, record in enumerate(capabilities)
        )
        model["summary"]["counts"]["checks"]["capabilities"] = {
            key: group[key] for key in ("state", "source_count", "projected_count")
        }
    model["projection"] = {
        "policy": POLICY,
        "selection": "ordered-prefix-not-representative",
        "summary_display": "scalars and complete-artifact references for nested metadata",
        "coverage_display": "all saved check states and reasons; nested descriptors in audit.json",
        "collections": projection,
    }
    model["omissions"] = omissions
    for key, counts in projection.items():
        model["summary"]["counts"][key]["source_count"] = counts["source"]
    return model


class _OutputBudget:
    def __init__(self, directory: Path):
        self.directory = directory
        self.used = 0
        self.next_disk_check = 0
        self.files = {}

    def reserve(self, amount: int):
        if self.used + amount > MAX_OUTPUT_BYTES:
            raise ValueError(
                f"PDF package exceeds {MAX_OUTPUT_BYTES} output bytes; no package published"
            )
        if self.used + amount >= self.next_disk_check:
            if shutil.disk_usage(self.directory).free < DISK_RESERVE_BYTES + amount:
                raise ValueError(
                    "PDF package would consume the 1 GiB disk reserve; no package published"
                )
            self.next_disk_check = self.used + amount + 8 * 1024 * 1024
        self.used += amount

    @contextmanager
    def open_text(self, path: Path, *, csv: bool = False):
        budget = self
        digest = hashlib.sha256()
        size = 0
        writes = 0
        with path.open("xb") as raw:
            os.chmod(path, 0o600)

            class Sink:
                def write(self, value: str):
                    nonlocal size, writes
                    encoded = value.encode("utf-8")
                    budget.reserve(len(encoded))
                    raw.write(encoded)
                    digest.update(encoded)
                    size += len(encoded)
                    writes += 1
                    return len(value)

            sink = Sink()
            if csv:
                sink.write("\ufeff")
                writes = 0
            yield sink
            raw.flush()
            os.fsync(raw.fileno())
        self.files[path.name] = {"bytes": size, "sha256": digest.hexdigest()}
        if csv:
            self.files[path.name]["rows"] = writes - 1

    def open_csv(self, path: Path):
        return self.open_text(path, csv=True)


def _portable_links(path: Path, bundle_name: str, artifacts: list[str]) -> None:
    """Replace staging-independent placeholder URIs with portable relative links."""
    from pypdf import PdfReader, PdfWriter
    from pypdf.generic import NameObject, TextStringObject

    reader = PdfReader(path)
    writer = PdfWriter()
    links = {
        ARTIFACT_LINK_PREFIX + name: quote(bundle_name, safe="") + "/" + quote(name, safe="")
        for name in artifacts
    }
    seen = set()
    for page in reader.pages:
        for annotation in page.get("/Annots", []):
            action = annotation.get_object().get("/A")
            if action and action.get("/URI") in links:
                seen.add(action["/URI"])
                action[NameObject("/URI")] = TextStringObject(links[action["/URI"]])
    if seen != set(links):
        raise ValueError("PDF overview did not retain every complete-companion link")
    writer.clone_document_from_reader(reader)
    replacement = path.with_suffix(".linked.pdf")
    with replacement.open("wb") as output:
        writer.write(output)
    os.replace(replacement, path)


def write_overview(reader: Any, view: Any, target: Path, *, lang: str = "en") -> dict[str, Any]:
    """Publish a new PDF and complete evidence directory, or publish neither."""
    from . import csvfile
    from .pdf_output import write_pdf_report
    from .pdf_validation import DEFAULT_BYTE_LIMIT, DEFAULT_PAGE_LIMIT, validate_pdf_output

    bundle = target.with_name(target.stem + ".files")
    if target.exists() or target.is_symlink() or bundle.exists() or bundle.is_symlink():
        return {
            "ok": False,
            "error": "PDF overview requires a new output path and companion directory",
        }
    model = build_overview_model(reader, (view.get("summary") or {}).get("project_coverage"))
    names = [
        "audit.json",
        "findings.csv",
        "findings.pages.csv",
        "findings.scope.csv",
        "manifest.json",
    ]
    model["artifacts"] = names
    with tempfile.TemporaryDirectory(prefix=".seohead-overview-", dir=target.parent) as temporary:
        stage = Path(temporary) / bundle.name
        stage.mkdir(mode=0o700)
        budget = _OutputBudget(stage)
        budget.reserve(0)
        # Render first: a missing Chromium/PDF dependency must not trigger a
        # potentially large export that cannot produce the requested package.
        pdf = stage / "report.pdf"
        rendered = write_pdf_report(model, pdf, lang=lang)
        if not rendered.get("ok"):
            return rendered
        _portable_links(pdf, bundle.name, names)
        validation = validate_pdf_output(pdf, model)
        if validation["status"] != "ok":
            return {
                "ok": False,
                "error": "PDF overview validation failed",
                "validation": validation,
            }
        budget.reserve(pdf.stat().st_size)
        digest = hashlib.sha256()
        with pdf.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        pdf_hash = digest.hexdigest()
        with budget.open_text(stage / "audit.json") as output:
            for chunk in reader.document_chunks():
                output.write(chunk)
        csvfile.write(view, stage / "findings.csv", open_text=budget.open_csv)
        for name, expected in (
            ("findings.csv", view.finding_count),
            ("findings.pages.csv", view.page_count),
        ):
            if budget.files[name]["rows"] != expected:
                raise ValueError(
                    f"PDF companion row count differs for {name}; no package published"
                )
        manifest = {
            "schema": "seohead.pdf-overview-package/1",
            "pdf_policy": POLICY,
            "source": {
                "format": "audit.v2",
                "sha256": reader.sha256,
                "binding": reader.binding,
                "collections": reader.collections,
            },
            "projection": model["projection"],
            "omissions": model["omissions"],
            "complete_machine_export": True,
            "files": {
                **budget.files,
                "../" + target.name: {"bytes": pdf.stat().st_size, "sha256": pdf_hash},
            },
            "limits": {
                "max_output_bytes": MAX_OUTPUT_BYTES,
                "disk_reserve_bytes": DISK_RESERVE_BYTES,
                "pdf_pages": DEFAULT_PAGE_LIMIT,
                "pdf_bytes": DEFAULT_BYTE_LIMIT,
                "metadata_bytes": MAX_METADATA_BYTES,
            },
        }
        with budget.open_text(stage / "manifest.json") as output:
            output.write(_json(manifest) + "\n")
        published = False
        linked = False
        try:
            if bundle.exists() or bundle.is_symlink():
                raise FileExistsError("PDF companion destination appeared during export")
            os.rename(stage, bundle)
            published = True
            # link() refuses a concurrent destination instead of overwriting it.
            os.link(bundle / "report.pdf", target)
            linked = True
            (bundle / "report.pdf").unlink()
            from seohead.core.filesystem import fsync_directory

            fsync_directory(bundle)
            fsync_directory(target.parent)
        except Exception:
            if linked:
                target.unlink()
            if published:
                shutil.rmtree(bundle)
            raise
    return {
        **rendered,
        "path": str(target),
        "bytes": target.stat().st_size,
        "findings": view.finding_count,
        "pages": view.page_count,
        "pdf_policy": POLICY,
        "projection": model["projection"],
        "manifest": str(bundle / "manifest.json"),
        "manifest_sha256": budget.files["manifest.json"]["sha256"],
        "outputs": [str(target), *(str(bundle / name) for name in names)],
        "validation": validation,
    }
