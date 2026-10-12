"""Validate the existing audit snapshot against its native scan revision."""

import json

from seohead.crawl.settings import manifest

from . import MAX_JSON_BYTES, ScanError, _audit, _dump, _sha


class AuditSizeError(ScanError):
    """The complete audit cannot fit the format reader's finite JSON budget."""


def validate_audit(con, scan, *, required=False):
    rows = con.execute("SELECT * FROM audit WHERE singleton=1").fetchone()
    if rows is None:
        if required:
            raise ScanError(
                "native scan has no current audit; collection evidence is available separately"
            )
        return None
    row = dict(rows)
    raw = row["document_json"]
    if len(raw.encode("utf-8")) > MAX_JSON_BYTES or _sha(raw) != row["sha256"]:
        raise ScanError("native saved audit size or hash is invalid")
    document = _audit(raw)
    if (
        row["schema_version"],
        row["evidence_revision"],
        row["analyzer_version"],
        row["analyzer_revision"],
    ) != (
        document["schema_version"],
        scan["evidence_revision"],
        scan["writer_version"],
        scan["writer_revision"],
    ):
        raise ScanError("native saved audit version, revision or analyzer identity disagrees")
    if document["tool"]["version"] != scan["writer_version"]:
        raise ScanError("native audit tool version differs from the producing build")
    effective_config = json.loads(scan["config_json"])
    has_config_extension = con.execute(
        "SELECT 1 FROM context_items WHERE kind='native_config_extension' AND item_key='run'"
    ).fetchone()
    has_asset_thresholds = con.execute(
        "SELECT 1 FROM context_items WHERE kind='analysis_asset_thresholds' LIMIT 1"
    ).fetchone()
    if has_config_extension:
        from .native_scan import read_effective_config

        effective_config = read_effective_config(con)
    elif has_asset_thresholds:
        # Legacy audits were emitted from the frozen header plus the existing
        # threshold adjunct. Do not inject newer default settings into that
        # historical manifest when validating the saved document.
        from .native_scan import read_effective_config

        effective_config.setdefault("analysis", {})["asset_thresholds"] = read_effective_config(
            con
        )["analysis"]["asset_thresholds"]
    config = manifest(effective_config)
    if _dump(document["run"].get("crawl_config")) != _dump(config):
        raise ScanError("native audit effective configuration disagrees")
    finding_exclusions = effective_config.get("analysis", {}).get("finding_exclusions", [])
    if _dump(document["run"].get("finding_exclusion_policy", [])) != _dump(finding_exclusions):
        raise ScanError("native audit finding-exclusion policy disagrees with saved scan config")
    if bool(document["run"].get("crawl_partial")) != bool(scan["crawl_partial"]):
        raise ScanError("native audit collection completeness disagrees")
    urls = {page["url"] for page in document["pages"]}
    if (
        len(urls) != len(document["pages"])
        or len(urls) != con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
    ):
        raise ScanError("native audit page population disagrees")
    for row in con.execute("SELECT u.url FROM pages p JOIN urls u USING(url_id)"):
        if row[0] not in urls:
            raise ScanError("native audit is missing a collected page")
    return document
