"""Versioned remediation ledger: stable finding/occurrence identity over saved scans.

The ledger is a separate local SQLite artifact (``ledger.v1``) bound to a
``seohead.project.v1`` project/site identity.  It ingests validated
``scan.v1``/``scan.v2`` artifacts read-only -- never appending to, upgrading, or
mutating the source scan -- and records which findings were observed, where each
occurrence lives, and every later observation of the same case, so a recheck or
remediation workflow can resume without rerunning the analyzer.

Identity rules (docs/LEDGER.md):

* a *finding* is the logical problem at project/site scope: one check at one
  subject (a canonical URL, or the explicit ``scope:site`` subject for an
  audit-wide finding);
* an *occurrence* is where that problem manifests: one check at one affected
  URL or non-URL subject, under one representation, with a typed discriminator
  separating firings on the same URL;
* *affected URLs* are finding membership, counted separately so findings,
  occurrences and distinct affected URLs never collapse into one number;
* an *observation* is one immutable sighting of an occurrence bound to an exact
  source scan revision (scan UUID + evidence revision + audit SHA-256 +
  configuration + producer identity).  Re-ingesting the same source revision is
  idempotent; a new revision appends and never overwrites the baseline.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import re
import sqlite3
import tempfile
import uuid
from collections.abc import Sequence
from datetime import datetime, timezone
from functools import lru_cache
from importlib.resources import files
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from seohead.core.sqlite import open_readonly

from ..core.filesystem import file_sha256
from . import ScanError, _dump, _loads, open_scan
from .native_scan import _utc

APPLICATION_ID = 1397051212  # ASCII SEOL; scan artifacts use SEOH (1397051208).
USER_VERSION = 5
FORMAT_VERSION = "ledger.v1"
READ_TIMEOUT_SECONDS = 30
MAX_PAYLOAD_BYTES = 8 * 1024 * 1024
DEFAULT_CASE_LIMIT = 100
MAX_CASE_LIMIT = 1_000
_REPRESENTATIONS = frozenset(
    {"static", "rendered", "legacy_fragment", "legacy_unknown", "unknown", "scope"}
)
_SCOPE_SUBJECT = "site"
_KEY_FINDING = "seohead.ledger-finding.v1"
_KEY_OCCURRENCE = "seohead.ledger-occurrence.v1"
_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_FINGERPRINT = re.compile(r"[0-9a-f]{16}\Z")
_REVISION = re.compile(r"[0-9a-f]{40}\Z")
_UTC = timezone.utc

# A lifecycle is deliberately a decision layer over retained observations.  It
# never changes an observation or turns an absent/failed fetch into a fix.
_LIFECYCLE_STATES = frozenset(
    {
        "detected",
        "verified",
        "fix_reported",
        "recheck_pending",
        "resolved",
        "persisting",
        "false_positive_reviewed",
        "unverifiable",
        "regressed",
    }
)
_TRANSITIONS = {
    "detected": frozenset({"verified", "fix_reported", "false_positive_reviewed", "unverifiable"}),
    "verified": frozenset(
        {"fix_reported", "recheck_pending", "false_positive_reviewed", "unverifiable"}
    ),
    "fix_reported": frozenset({"verified", "recheck_pending", "unverifiable"}),
    "recheck_pending": frozenset({"resolved", "persisting", "regressed", "unverifiable"}),
    "resolved": frozenset({"recheck_pending", "regressed"}),
    "persisting": frozenset({"fix_reported", "recheck_pending", "regressed", "unverifiable"}),
    "false_positive_reviewed": frozenset({"verified", "recheck_pending"}),
    "unverifiable": frozenset({"verified", "fix_reported", "recheck_pending"}),
    "regressed": frozenset({"fix_reported", "recheck_pending", "unverifiable"}),
}
_MEASURED_OUTCOMES = frozenset({"resolved", "persisting", "regressed"})


class LedgerError(ScanError):
    """The input cannot be used as a supported, consistent ledger artifact."""


def _runtime() -> None:
    if sqlite3.sqlite_version_info < (3, 31, 0):
        raise LedgerError(
            f"ledger.v1 requires SQLite >= 3.31; this Python uses {sqlite3.sqlite_version}"
        )


def _schema() -> str:
    return files(__package__).joinpath("ledger_v1.sql").read_text(encoding="utf-8")


def _v2_schema() -> str:
    """The additive v2 evidence binding, kept separate from the v1 bootstrap.

    Keeping this delta separate lets an existing ledger.v1 migrate in one
    transaction while a fresh ledger is built from the identical sequence.
    ``sqlite_master`` is deliberately compared after applying this delta, so a
    hand-added evidence table is never silently accepted.
    """
    return files(__package__).joinpath("ledger_v2.sql").read_text(encoding="utf-8")


def _v3_schema() -> str:
    """The additive source-scope state used by remediation coverage."""
    return files(__package__).joinpath("ledger_v3.sql").read_text(encoding="utf-8")


def _v4_schema() -> str:
    """Add canonical audit identity for safely bridged legacy audit documents."""
    return files(__package__).joinpath("ledger_v4.sql").read_text(encoding="utf-8")


def _apply_v2_schema(con: sqlite3.Connection) -> None:
    """Apply the v1 -> v2 DDL one statement at a time inside a transaction."""
    for piece in _v2_schema().split(";"):
        statement = piece.strip()
        if statement and not statement.startswith("--"):
            con.execute(statement)


def _apply_v3_schema(con: sqlite3.Connection) -> None:
    for piece in _v3_schema().split(";"):
        statement = piece.strip()
        if statement and not statement.startswith("--"):
            con.execute(statement)


def _apply_v4_schema(con: sqlite3.Connection) -> None:
    for piece in _v4_schema().split(";"):
        statement = piece.strip()
        if statement and not statement.startswith("--"):
            con.execute(statement)


def _apply_v5_schema(con: sqlite3.Connection) -> None:
    for piece in files(__package__).joinpath("ledger_v5.sql").read_text("utf-8").split(";"):
        if piece.strip():
            con.execute(piece.strip())


def _ddl_statements() -> list[str]:
    """Schema statements minus PRAGMAs, for transactional migration.

    ``executescript`` commits any pending transaction first, so a migration that
    must roll back on failure applies the DDL one statement at a time instead.
    """
    statements = []
    for piece in _schema().split(";"):
        # Keep the piece verbatim: sqlite_master.sql stores embedded comments,
        # so the migrated schema must textually match a direct executescript.
        # Comments in ledger_v1.sql never contain ';' for exactly this reason.
        meaningful = [
            line
            for line in piece.splitlines()
            if line.strip() and not line.strip().startswith("--")
        ]
        if meaningful and not meaningful[0].strip().upper().startswith("PRAGMA"):
            statements.append(piece)
    return statements


def _objects(con) -> list[tuple]:
    return [
        tuple(row)
        for row in con.execute(
            "SELECT type, name, tbl_name, sql FROM sqlite_master "
            "WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name"
        )
    ]


@lru_cache(maxsize=1)
def _expected() -> list[tuple]:
    _runtime()
    con = sqlite3.connect(":memory:")
    try:
        con.executescript(_schema())
        _apply_v2_schema(con)
        _apply_v3_schema(con)
        _apply_v4_schema(con)
        _apply_v5_schema(con)
        return _objects(con)
    finally:
        con.close()


def _json_sha256(value: Any) -> str:
    """Hash one retained JSON value without depending on its file formatting."""
    raw = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def _key(*parts: str) -> str:
    encoded = json.dumps(list(parts), ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def canonical_url(value: str) -> str:
    """Canonicalize an affected URL with the audit's own equality policy.

    This delegates to :func:`seohead.sf.core.normalize.norm_url` -- the same
    strip/lowercase-scheme-and-host/drop-trailing-slash rule the analyzer uses
    for URL identity -- so a recheck reproduces the same key without a second
    normalization contract to drift.  Query and fragment are preserved.
    """
    from seohead.sf.core.normalize import norm_url

    if not isinstance(value, str) or not value.strip():
        raise LedgerError("affected URL must be a nonempty string")
    normalized = norm_url(value)
    if not normalized:
        raise LedgerError("affected URL must be a nonempty string")
    if len(normalized) > 2048:
        raise LedgerError("affected URL exceeds the 2048-character bound")
    return normalized


def finding_key(
    *,
    project_uuid: str,
    site_host: str,
    check: str,
    subject_type: str,
    subject_value: str,
) -> str:
    """Stable logical identity of a check/problem at project/site scope."""
    return _key(_KEY_FINDING, project_uuid, site_host, check, subject_type, subject_value)


def occurrence_key(
    *,
    project_uuid: str,
    site_host: str,
    check: str,
    subject_type: str,
    subject_value: str,
    representation: str,
    discriminator_type: str,
    discriminator_value: str,
) -> str:
    """Stable, versioned identity of one affected case.

    Deliberately excludes severity, message, HTTP status, evidence value or
    hash, configuration, producer build, timestamps, the run-local audit
    ordinal and the scan UUID: those describe an *observation* of the case,
    never the case itself.
    """
    return _key(
        _KEY_OCCURRENCE,
        project_uuid,
        site_host,
        check,
        subject_type,
        subject_value,
        representation,
        discriminator_type,
        discriminator_value,
    )


def _validated_time(value: Any) -> str | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None or parsed.utcoffset() != _UTC.utcoffset(parsed):
        return None
    return value


def _site_row(con, netloc: str):
    return con.execute(
        "SELECT * FROM site WHERE netloc=? ORDER BY (role='primary') DESC, site_id LIMIT 1",
        (netloc,),
    ).fetchone()


def _validate(con) -> None:
    if _objects(con) != _expected():
        raise LedgerError(
            "ledger schema differs: missing/changed tables, indexes or unexpected schema objects"
        )
    if con.execute("PRAGMA quick_check").fetchone()[0] != "ok":
        raise LedgerError("ledger database failed quick_check")
    if con.execute("PRAGMA foreign_key_check").fetchone():
        raise LedgerError("ledger database has inconsistent foreign-key references")
    for group in con.execute("SELECT * FROM source_group"):
        digest, count = hashlib.sha256(), 0
        for member in con.execute(
            "SELECT ordinal,value_json FROM source_group_member WHERE source_scan_id=? AND group_ref=? ORDER BY ordinal",
            (group["source_scan_id"], group["group_ref"]),
        ):
            if member["ordinal"] != count:
                raise LedgerError("source group members are incomplete or unordered")
            encoded = member["value_json"].encode("utf-8")
            if len(encoded) > MAX_PAYLOAD_BYTES:
                raise LedgerError("source group member exceeds the ledger payload bound")
            _loads(member["value_json"], "source group member")
            digest.update(len(encoded).to_bytes(8, "big"))
            digest.update(encoded)
            count += 1
        if (count, digest.hexdigest()) != (group["member_count"], group["members_sha256"]):
            raise LedgerError("source group member count or digest differs")
    rows = con.execute("SELECT * FROM ledger").fetchall()
    if len(rows) != 1:
        raise LedgerError("ledger.v1 requires exactly one header")
    header = dict(rows[0])
    if header["format_version"] != FORMAT_VERSION:
        raise LedgerError("unsupported ledger format_version")
    try:
        uuid.UUID(header["ledger_uuid"])
    except (TypeError, ValueError) as exc:
        raise LedgerError("invalid ledger UUID") from exc
    if not _REVISION.fullmatch(header["writer_revision"]):
        raise LedgerError("ledger writer build must be a full lowercase Git commit SHA")
    if type(header["ledger_revision"]) is not int or header["ledger_revision"] < 0:
        raise LedgerError("invalid ledger revision")
    if (
        con.execute(
            "SELECT 1 FROM finding WHERE current_state NOT IN ("
            + ",".join("?" for _ in _LIFECYCLE_STATES)
            + ") LIMIT 1",
            tuple(sorted(_LIFECYCLE_STATES)),
        ).fetchone()
        or con.execute(
            "SELECT 1 FROM occurrence WHERE current_state NOT IN ("
            + ",".join("?" for _ in _LIFECYCLE_STATES)
            + ") LIMIT 1",
            tuple(sorted(_LIFECYCLE_STATES)),
        ).fetchone()
    ):
        raise LedgerError("ledger contains an unsupported lifecycle state")
    if con.execute(
        "SELECT 1 FROM decision WHERE state NOT IN ("
        + ",".join("?" for _ in _LIFECYCLE_STATES)
        + ") OR actor='' OR reason='' OR decided_at_state!='known' OR decided_at IS NULL "
        "LIMIT 1",
        tuple(sorted(_LIFECYCLE_STATES)),
    ).fetchone():
        raise LedgerError("ledger contains an invalid lifecycle decision")
    if con.execute(
        "SELECT 1 FROM decision d LEFT JOIN verification_result vr "
        "ON vr.verification_result_id=d.verification_result_id "
        "WHERE d.verification_result_id IS NOT NULL AND (vr.verification_result_id IS NULL "
        "OR vr.outcome!=d.state OR (d.state IN ('resolved','persisting','regressed') "
        "AND vr.measured!=1)) LIMIT 1"
    ).fetchone():
        raise LedgerError("ledger verification decision is missing measured result evidence")
    for site in con.execute("SELECT * FROM site"):
        try:
            uuid.UUID(site["project_uuid"])
        except (TypeError, ValueError) as exc:
            raise LedgerError("site row has an invalid project UUID") from exc
        if site["netloc"] != urlsplit(site["target"]).netloc.lower():
            raise LedgerError("site row identity is inconsistent")
    for row in con.execute(
        "SELECT f.finding_key,f.subject_type,f.subject_value,c.check_key,s.project_uuid,s.host "
        "FROM finding f JOIN check_def c ON c.check_id=f.check_id JOIN site s ON s.site_id=f.site_id"
    ):
        expected = finding_key(
            project_uuid=row["project_uuid"],
            site_host=row["host"],
            check=row["check_key"],
            subject_type=row["subject_type"],
            subject_value=row["subject_value"],
        )
        if row["finding_key"] != expected:
            raise LedgerError("finding identity does not match its stored key")
    for row in con.execute(
        "SELECT o.occurrence_key,o.subject_type,o.subject_value,o.representation,"
        "o.discriminator_type,o.discriminator_value,c.check_key,s.project_uuid,s.host "
        "FROM occurrence o JOIN check_def c ON c.check_id=o.check_id "
        "JOIN finding f ON f.finding_id=o.finding_id JOIN site s ON s.site_id=f.site_id"
    ):
        expected = occurrence_key(
            project_uuid=row["project_uuid"],
            site_host=row["host"],
            check=row["check_key"],
            subject_type=row["subject_type"],
            subject_value=row["subject_value"],
            representation=row["representation"],
            discriminator_type=row["discriminator_type"],
            discriminator_value=row["discriminator_value"],
        )
        if row["occurrence_key"] != expected:
            raise LedgerError("occurrence identity does not match its stored key")
    if con.execute(
        "SELECT 1 FROM source_scan WHERE NOT (audit_sha256 GLOB '[0-9a-f][0-9a-f]*' "
        "AND length(audit_sha256)=64) OR NOT (scan_sha256 GLOB '[0-9a-f][0-9a-f]*' "
        "AND length(scan_sha256)=64) LIMIT 1"
    ).fetchone():
        raise LedgerError("source scan binding has an invalid digest")
    if con.execute(
        "SELECT 1 FROM source_scan WHERE canonical_audit_sha256 IS NOT NULL AND "
        "(canonical_audit_sha256 GLOB '*[^0-9a-f]*' OR "
        "length(canonical_audit_sha256)!=64) LIMIT 1"
    ).fetchone():
        raise LedgerError("source scan has an invalid canonical audit digest")
    if con.execute(
        "SELECT 1 FROM source_scan WHERE group_memberships_state "
        "NOT IN ('complete','partial','unavailable') LIMIT 1"
    ).fetchone():
        raise LedgerError("source scan has an invalid group-membership scope state")
    if con.execute(
        "SELECT 1 FROM observation WHERE (observed_at IS NULL) != (observed_at_state='unknown') LIMIT 1"
    ).fetchone():
        raise LedgerError("observation time state disagrees with its stored value")


def _reader(path: Path) -> sqlite3.Connection:
    return open_readonly(
        path.resolve(),
        row_factory=sqlite3.Row,
        pragmas=(
            "PRAGMA trusted_schema=OFF",
            "PRAGMA query_only=ON",
            "PRAGMA foreign_keys=ON",
        ),
    )


def _writer(path: Path) -> sqlite3.Connection:
    con = sqlite3.connect(path.absolute().as_uri() + "?mode=rw", uri=True, timeout=5)
    con.row_factory = sqlite3.Row
    con.execute("PRAGMA trusted_schema=OFF")
    con.execute("PRAGMA foreign_keys=ON")
    con.execute("PRAGMA synchronous=FULL")
    return con


def _migrate_0_to_1(con: sqlite3.Connection) -> None:
    """Complete a staged ``ledger.v0`` stub into the full v1 schema.

    ``ledger.v0`` is a bootstrap stub format an interrupted or hand-staged
    creation may leave: the ``SEOL`` application id, a ``user_version`` of 0
    and a one-row ``ledger_meta`` marker naming the intended ledger UUID, its
    project/site binding and the writer that staged it.  A stub never held
    findings, and ``create_ledger`` itself always writes a complete validated
    ``ledger.v1`` -- only an explicit write open finishes a stub.
    """
    try:
        row = con.execute("SELECT * FROM ledger_meta WHERE singleton=1").fetchone()
    except sqlite3.Error as exc:
        raise LedgerError("ledger.v0 stub is missing its ledger_meta marker") from exc
    if row is None or row["format_version"] != "ledger.v0":
        raise LedgerError("ledger.v0 marker does not name ledger.v0")
    meta = dict(row)
    con.execute("DROP TABLE ledger_meta")
    for statement in _ddl_statements():
        con.execute(statement)
    con.execute("PRAGMA user_version=1")
    con.execute(
        "INSERT INTO ledger VALUES(1,?,?,?,?,?,?)",
        (
            meta["ledger_uuid"],
            "ledger.v1",
            meta["created_at"],
            meta["writer_version"],
            meta["writer_revision"],
            0,
        ),
    )
    register_site(
        con,
        project_uuid=meta["project_uuid"],
        target=meta["site_target"],
        role="primary",
        created_at=meta["created_at"],
    )


def _migrate_1_to_2(con: sqlite3.Connection) -> None:
    """Add immutable verification artifact/result bindings to a v1 ledger."""
    _apply_v2_schema(con)
    con.execute("UPDATE ledger SET format_version=? WHERE singleton=1", (FORMAT_VERSION,))
    con.execute("PRAGMA user_version=2")


def _migrate_2_to_3(con: sqlite3.Connection) -> None:
    """Persist whether group members were fully retained by the source audit."""
    _apply_v3_schema(con)
    # v2 did not retain this state for streaming scan.v2 audit companions. A
    # migration cannot reconstruct missing group members from their old ledger
    # rows, so mark their full scope unavailable rather than backfilling a
    # deceptively complete denominator. New v3 ingestion records it exactly.
    con.execute(
        "UPDATE source_scan SET group_memberships_state='unavailable' "
        "WHERE format_version='scan.v2'"
    )
    con.execute("PRAGMA user_version=3")


def _migrate_3_to_4(con: sqlite3.Connection) -> None:
    """Reserve canonical audit identities without inventing them for old rows."""
    _apply_v4_schema(con)
    con.execute("PRAGMA user_version=4")


def _migrate_4_to_5(con: sqlite3.Connection) -> None:
    """Retain each ordered source group once; reject inconsistent old copies."""
    _apply_v5_schema(con)
    for row in con.execute("SELECT * FROM finding_group ORDER BY source_scan_id,group_ref"):
        _created, complete = _store_source_group(
            con,
            source_scan_id=row["source_scan_id"],
            group={
                "group_id": row["group_ref"],
                "check": row["group_check"],
                "value": row["group_value"],
                "count": row["group_count"],
                "urls": _loads(row["group_urls_json"], "group members"),
            },
            refuse_new=False,
        )
        if not complete:
            con.execute(
                "UPDATE source_scan SET group_memberships_state='partial' WHERE source_scan_id=? AND group_memberships_state='complete'",
                (row["source_scan_id"],),
            )
    con.execute("UPDATE finding_group SET group_urls_json='[]'")
    con.execute("PRAGMA user_version=5")


_MIGRATIONS = {
    0: _migrate_0_to_1,
    1: _migrate_1_to_2,
    2: _migrate_2_to_3,
    3: _migrate_3_to_4,
    4: _migrate_4_to_5,
}


def _migrate(con: sqlite3.Connection) -> None:
    """Apply known migrations inside the caller's write transaction.

    Only a write-mode open reaches here: reading never mutates the file, an
    unknown/newer version was refused before this point, and a raised migration
    error leaves the caller's transaction to roll the file back unchanged.
    """
    version = con.execute("PRAGMA user_version").fetchone()[0]
    if version < 0 or version > USER_VERSION:
        raise LedgerError(f"unsupported ledger user_version {version}")
    while version < USER_VERSION:
        migration = _MIGRATIONS.get(version)
        if migration is None:
            raise LedgerError(f"no ledger migration path from version {version}")
        migration(con)
        new_version = con.execute("PRAGMA user_version").fetchone()[0]
        if new_version != version + 1:
            raise LedgerError(f"ledger migration {version} did not advance the version")
        version = new_version


def open_ledger(path: str | Path, *, write: bool = False) -> sqlite3.Connection:
    """Return a validated ledger connection; the caller must close it.

    A read open never mutates the artifact.  A write open is the explicit
    operation allowed to migrate a known older version, inside one transaction;
    a newer/unknown version refuses without touching the file.
    """
    _runtime()
    path = Path(path)
    con = None
    try:
        con = _writer(path) if write else _reader(path)
        app_id = con.execute("PRAGMA application_id").fetchone()[0]
        if app_id != APPLICATION_ID:
            raise LedgerError(f"foreign application_id {app_id}; expected {APPLICATION_ID} (SEOL)")
        version = con.execute("PRAGMA user_version").fetchone()[0]
        if write:
            if version > USER_VERSION or version < 0:
                raise LedgerError(
                    f"unsupported ledger user_version {version}; this build knows {USER_VERSION}"
                )
            if version != USER_VERSION:
                con.execute("BEGIN IMMEDIATE")
                try:
                    _migrate(con)
                    # Validation stays inside the transaction: a migration that
                    # produced an inconsistent ledger rolls back untouched.
                    _validate(con)
                    con.commit()
                except BaseException:
                    con.rollback()
                    raise
                return con
        elif version != USER_VERSION:
            raise LedgerError(
                f"unsupported ledger user_version {version}; this build knows {USER_VERSION}"
            )
        _validate(con)
        return con
    except (OSError, sqlite3.Error, ValueError) as exc:
        if con is not None:
            con.close()
        if isinstance(exc, LedgerError):
            raise
        raise LedgerError(f"cannot read ledger: {exc}") from exc


def create_ledger(path: str | Path, *, project_dir: str | Path, producer_build: str) -> Path:
    """Create one new empty ledger bound to a validated project; never overwrites."""
    _runtime()
    if not isinstance(producer_build, str) or not _REVISION.fullmatch(producer_build):
        raise LedgerError(
            "producer_build must identify the ledger-writing build with a full lowercase Git SHA"
        )
    from seohead import __version__
    from seohead.projects.workspace import open_project

    project = open_project(project_dir)["project"]
    out = Path(path).absolute()
    if os.path.lexists(out):
        raise LedgerError(f"ledger already exists: {out}; ledgers never overwrite")
    if not out.parent.is_dir():
        raise LedgerError(f"ledger parent directory does not exist: {out.parent}")
    temporary = None
    con = None
    try:
        fd, name = tempfile.mkstemp(prefix=".ledger-", suffix=".sqlite", dir=out.parent)
        os.close(fd)
        temporary = Path(name)
        con = sqlite3.connect(temporary)
        con.row_factory = sqlite3.Row
        con.executescript(_schema())
        _apply_v2_schema(con)
        _apply_v3_schema(con)
        _apply_v4_schema(con)
        _apply_v5_schema(con)
        con.execute(f"PRAGMA user_version={USER_VERSION}")
        con.execute("PRAGMA trusted_schema=OFF")
        con.execute("PRAGMA foreign_keys=ON")
        con.execute("PRAGMA synchronous=FULL")
        con.execute("BEGIN")
        now = _utc()
        con.execute(
            "INSERT INTO ledger VALUES(1,?,?,?,?,?,?)",
            (
                str(uuid.uuid4()),
                FORMAT_VERSION,
                now,
                __version__,
                producer_build,
                0,
            ),
        )
        register_site(
            con,
            project_uuid=project["project_uuid"],
            target=project["site"]["target"],
            role="primary",
            created_at=now,
        )
        con.commit()
        con.close()
        con = None
        check = open_ledger(temporary)
        check.close()
        with temporary.open("r+b") as stream:
            os.fsync(stream.fileno())
        os.link(temporary, out)
        from seohead.core.filesystem import fsync_directory

        fsync_directory(out.parent)
        return out
    except FileExistsError as exc:
        raise LedgerError(f"ledger already exists: {out}; ledgers never overwrite") from exc
    except (OSError, sqlite3.Error, ValueError) as exc:
        if isinstance(exc, LedgerError):
            raise
        raise LedgerError(f"cannot create ledger: {exc}") from exc
    finally:
        if con is not None:
            con.close()
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def register_site(
    con: sqlite3.Connection,
    *,
    project_uuid: str,
    target: str,
    role: str = "secondary",
    created_at: str | None = None,
) -> int:
    """Register one site identity on an open write connection.

    ``target`` is normalized with the same ``seohead.project.v1`` site-target
    rules the project workspace applies; scans bind by exact ``netloc`` match,
    the convention saved-evidence validation already uses.  A repeat call with
    an identical (project_uuid, netloc) pair returns the existing row.
    """
    from seohead.projects.workspace import _target as _normalize_target
    from seohead.recon.net import normalize_domain

    try:
        normalized_uuid = str(uuid.UUID(project_uuid))
    except (TypeError, ValueError, AttributeError) as exc:
        raise LedgerError("site registration requires a project UUID") from exc
    if role not in {"primary", "secondary"}:
        raise LedgerError("site role must be 'primary' or 'secondary'")
    normalized = _normalize_target(target)
    parts = urlsplit(normalized)
    host = normalize_domain(parts.hostname or "")
    netloc = parts.netloc.lower()
    if not host or not netloc:
        raise LedgerError("site registration requires a public hostname")
    row = con.execute(
        "SELECT site_id,target,host,role FROM site WHERE project_uuid=? AND netloc=?",
        (normalized_uuid, netloc),
    ).fetchone()
    if row is not None:
        if row["target"] != normalized or row["host"] != host:
            raise LedgerError("site identity conflicts with a registered row")
        return int(row["site_id"])
    con.execute(
        "INSERT INTO site(project_uuid,target,host,netloc,role,created_at) VALUES(?,?,?,?,?,?)",
        (normalized_uuid, normalized, host, netloc, role, created_at or _utc()),
    )
    return int(con.execute("SELECT last_insert_rowid()").fetchone()[0])


def _representation_map(scan_con, document: dict[str, Any]) -> dict[str, str]:
    """URL -> recorded representation, the audit document's claim first."""
    result: dict[str, str] = {}
    try:
        for row in scan_con.execute(
            "SELECT u.url,p.representation FROM pages p JOIN urls u USING(url_id)"
        ):
            if isinstance(row[1], str) and row[1]:
                result[canonical_url(row[0])] = row[1]
    except (sqlite3.Error, LedgerError):
        pass
    for page in document.get("pages") or []:
        if not isinstance(page, dict):
            continue
        metrics = page.get("metrics") if isinstance(page.get("metrics"), dict) else {}
        value = metrics.get("representation")
        url = page.get("url")
        if isinstance(url, str) and isinstance(value, str) and value:
            try:
                result[canonical_url(url)] = value
            except LedgerError:
                continue
    return result


def _observation_index(scan_con) -> tuple[dict[str, set[str]], dict[int, str]]:
    """Map each saved page URL to the typed observation ids it owns.

    ``observation_id`` values in the evidence contract are ``url_id:N``,
    ``document_id:N``, ``response_id:N`` or ``language_evidence:...:document:N``.
    Resolving them to URLs lets one occurrence name the retained evidence for
    its own affected page instead of borrowing another page's.
    """
    by_url: dict[str, set[str]] = {}
    doc_url: dict[int, str] = {}
    try:
        rows = scan_con.execute(
            "SELECT u.url,p.url_id,p.document_id,d.source_response_id "
            "FROM pages p JOIN urls u USING(url_id) "
            "LEFT JOIN documents d ON d.document_id=p.document_id"
        )
    except sqlite3.Error:
        return by_url, doc_url
    for url, url_id, document_id, source_response_id in rows:
        try:
            canon = canonical_url(url)
        except LedgerError:
            continue
        owned = by_url.setdefault(canon, set())
        owned.add(f"url_id:{url_id}")
        if type(document_id) is int and document_id > 0:
            owned.add(f"document_id:{document_id}")
            doc_url[document_id] = canon
        if type(source_response_id) is int and source_response_id > 0:
            owned.add(f"response_id:{source_response_id}")
    return by_url, doc_url


_DOC_MARKER = re.compile(r":document:(\d+):representation:")


def _occurrence_evidence(
    contract: dict[str, Any],
    *,
    role: str,
    url: str | None,
    by_url: dict[str, set[str]],
    doc_url: dict[int, str],
) -> dict[str, Any]:
    """Slice the issue-level contract refs down to this occurrence's own page."""
    observations = contract.get("observations") if isinstance(contract, dict) else None
    if not isinstance(observations, list) or not observations:
        reason = (
            contract.get("reason")
            if isinstance(contract, dict) and isinstance(contract.get("reason"), str)
            else "no saved evidence contract is present"
        )
        return {
            "state": "unavailable",
            "reason": reason or "no retained observation for this affected URL",
            "references": [],
        }
    refs: list[dict[str, Any]] = []
    for item in observations:
        if not isinstance(item, dict):
            continue
        observation_id = item.get("observation_id")
        if not isinstance(observation_id, str):
            continue
        if role == "target":
            if item.get("role") == "target":
                refs.append(item)
                continue
            match = _DOC_MARKER.search(observation_id)
            if match and doc_url.get(int(match.group(1))) == url:
                refs.append(item)
        elif url is not None and observation_id in by_url.get(url, set()):
            refs.append(item)
    if not refs:
        return {
            "state": "unavailable",
            "reason": "no retained observation for this affected URL",
            "references": [],
        }
    state = refs[0].get("state")
    return {
        "state": state if state in {"measured", "imported_projection"} else "unavailable",
        "reason": ""
        if state in {"measured", "imported_projection"}
        else "retained observation state is unknown",
        "references": [
            {
                key: item[key]
                for key in ("id", "source_table", "observation_id", "role", "representation")
                if key in item
            }
            for item in refs
        ],
    }


def _coverage_state(issue: dict[str, Any], locations: list) -> tuple[str, int]:
    """Classify how much of a reported total the audit enumerated.

    ``enumerated`` means the saved document names the whole reported total;
    ``capped`` means the audit kept only a bounded location list;
    ``aggregate_only`` means repeated locations lack enough stable locator
    data to distinguish their events. ``unknown`` means the document does not
    state a usable total. The enumerated count is stable location events, not
    distinct affected URLs; those memberships are stored separately.
    """
    reported = issue.get("occurrences_count")
    entries = _location_entries(locations)
    by_url: dict[str, list[dict[str, Any]]] = {}
    for entry in entries:
        by_url.setdefault(entry["url"], []).append(entry)
    locator_groups = _locator_groups(issue, entries)
    enumerated = sum(len(locator_groups[url]) if url in locator_groups else 1 for url in by_url)
    if not locations and isinstance(issue.get("target_url"), str):
        enumerated = 1
    if type(reported) is not int or reported < 0:
        return "unknown", enumerated
    if not locations:
        return ("enumerated" if reported <= enumerated else "aggregate_only"), enumerated
    if reported > len(locations):
        return "capped", min(reported, enumerated)
    if reported > enumerated:
        return "aggregate_only", enumerated
    return "enumerated", min(reported, enumerated)


def _check_id(con, check_key: str, refuse_new: bool) -> int:
    if not isinstance(check_key, str) or not check_key or len(check_key) > 128:
        raise LedgerError("an audit issue requires a bounded check identifier")
    row = con.execute("SELECT check_id FROM check_def WHERE check_key=?", (check_key,)).fetchone()
    if row is not None:
        return int(row[0])
    if refuse_new:
        raise LedgerError("conflicting evidence under a previously bound source revision")
    con.execute("INSERT INTO check_def(check_key) VALUES(?)", (check_key,))
    return int(con.execute("SELECT last_insert_rowid()").fetchone()[0])


def _insert_finding(
    con,
    *,
    site_id: int,
    check_id: int,
    check_key: str,
    project_uuid: str,
    site_host: str,
    subject_type: str,
    subject_value: str,
    now: str,
    refuse_new: bool,
) -> tuple[int, bool, str]:
    key = finding_key(
        project_uuid=project_uuid,
        site_host=site_host,
        check=check_key,
        subject_type=subject_type,
        subject_value=subject_value,
    )
    row = con.execute("SELECT finding_id FROM finding WHERE finding_key=?", (key,)).fetchone()
    if row is not None:
        return int(row[0]), False, key
    if refuse_new:
        raise LedgerError("conflicting evidence under a previously bound source revision")
    con.execute(
        "INSERT INTO finding(site_id,check_id,subject_type,subject_value,finding_key,first_seen_at)"
        " VALUES(?,?,?,?,?,?)",
        (site_id, check_id, subject_type, subject_value, key, now),
    )
    return int(con.execute("SELECT last_insert_rowid()").fetchone()[0]), True, key


def _insert_occurrence(
    con,
    *,
    finding_id: int,
    check_id: int,
    check_key: str,
    project_uuid: str,
    site_host: str,
    subject_type: str,
    subject_value: str,
    representation: str,
    discriminator_type: str,
    discriminator_value: str,
    now: str,
    refuse_new: bool,
) -> tuple[int, bool, str]:
    if representation not in _REPRESENTATIONS:
        raise LedgerError(f"unsupported occurrence representation {representation!r}")
    key = occurrence_key(
        project_uuid=project_uuid,
        site_host=site_host,
        check=check_key,
        subject_type=subject_type,
        subject_value=subject_value,
        representation=representation,
        discriminator_type=discriminator_type,
        discriminator_value=discriminator_value,
    )
    row = con.execute(
        "SELECT occurrence_id FROM occurrence WHERE occurrence_key=?", (key,)
    ).fetchone()
    if row is not None:
        return int(row[0]), False, key
    if refuse_new:
        raise LedgerError("conflicting evidence under a previously bound source revision")
    con.execute(
        "INSERT INTO occurrence(finding_id,check_id,subject_type,subject_value,representation,"
        "discriminator_type,discriminator_value,occurrence_key,first_seen_at)"
        " VALUES(?,?,?,?,?,?,?,?,?)",
        (
            finding_id,
            check_id,
            subject_type,
            subject_value,
            representation,
            discriminator_type,
            discriminator_value,
            key,
            now,
        ),
    )
    return int(con.execute("SELECT last_insert_rowid()").fetchone()[0]), True, key


def _insert_member(
    con, *, finding_id: int, url: str, role: str, now: str, refuse_new: bool
) -> bool:
    cursor = con.execute(
        "INSERT OR IGNORE INTO affected_url(finding_id,url,role,first_seen_at) VALUES(?,?,?,?)",
        (finding_id, url, role, now),
    )
    created = cursor.rowcount > 0
    if created and refuse_new:
        raise LedgerError("conflicting evidence under a previously bound source revision")
    return created


def _insert_projection(
    con, *, finding_id: int, source_scan_id: int, row: dict[str, Any], refuse_new: bool
) -> bool:
    existing = con.execute(
        "SELECT payload_sha256 FROM finding_observation "
        "WHERE finding_id=? AND source_scan_id=? AND issue_ordinal=?",
        (finding_id, source_scan_id, row["issue_ordinal"]),
    ).fetchone()
    if existing is not None:
        if existing[0] != row["payload_sha256"]:
            raise LedgerError("conflicting finding projection under an identical source revision")
        return False
    if refuse_new:
        raise LedgerError("conflicting evidence under a previously bound source revision")
    con.execute(
        "INSERT INTO finding_observation(finding_id,source_scan_id,issue_ordinal,"
        "audit_fingerprint,severity,status_code,message,occurrences_count,enumerated_count,"
        "coverage_state,payload_json,payload_sha256) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            finding_id,
            source_scan_id,
            row["issue_ordinal"],
            row["audit_fingerprint"],
            row["severity"],
            row["status_code"],
            row["message"],
            row["occurrences_count"],
            row["enumerated_count"],
            row["coverage_state"],
            row["payload_json"],
            row["payload_sha256"],
        ),
    )
    return True


def _store_source_group(con, *, source_scan_id: int, group, refuse_new: bool) -> tuple[bool, bool]:
    """Store one source group and stream each member once, preserving its ordinal."""
    ref = group["group_id"]
    check = str(group.get("check") or "")
    value = group.get("value") if isinstance(group.get("value"), str) else None
    count = group.get("count") if type(group.get("count")) is int else None
    urls = group.get("urls")
    array = isinstance(urls, Sequence) and not isinstance(urls, (str, bytes))
    existing = con.execute(
        "SELECT * FROM source_group WHERE source_scan_id=? AND group_ref=?", (source_scan_id, ref)
    ).fetchone()
    if existing is not None and (
        existing["group_check"],
        existing["group_value"],
        existing["group_count"],
    ) != (check, value, count):
        raise LedgerError("conflicting group membership under an identical source revision")
    if existing is None:
        if refuse_new:
            raise LedgerError("conflicting evidence under a previously bound source revision")
        con.execute(
            "INSERT INTO source_group VALUES(?,?,?,?,?,0,'')",
            (source_scan_id, ref, check, value, count),
        )
    digest, member_count, valid = hashlib.sha256(), 0, array
    for ordinal, member in enumerate(urls if array else ()):
        raw = _dump(member)
        encoded = raw.encode("utf-8")
        if len(encoded) > MAX_PAYLOAD_BYTES:
            raise LedgerError("group member exceeds the ledger payload bound")
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
        valid = valid and isinstance(member, str) and bool(member.strip())
        if existing is None:
            con.execute(
                "INSERT INTO source_group_member VALUES(?,?,?,?)",
                (source_scan_id, ref, ordinal, raw),
            )
        member_count += 1
    if existing is None:
        con.execute(
            "UPDATE source_group SET member_count=?,members_sha256=? WHERE source_scan_id=? AND group_ref=?",
            (member_count, digest.hexdigest(), source_scan_id, ref),
        )
    elif (existing["member_count"], existing["members_sha256"]) != (
        member_count,
        digest.hexdigest(),
    ):
        raise LedgerError("conflicting group membership under an identical source revision")
    unique = con.execute(
        "SELECT COUNT(DISTINCT value_json) FROM source_group_member WHERE source_scan_id=? AND group_ref=?",
        (source_scan_id, ref),
    ).fetchone()[0]
    return existing is None, bool(valid and count == member_count == unique)


def _insert_group(
    con, *, finding_id: int, source_scan_id: int, row: dict[str, Any], refuse_new: bool
) -> bool:
    existing = con.execute(
        "SELECT group_check,group_value,group_count,group_urls_json FROM finding_group "
        "WHERE finding_id=? AND source_scan_id=? AND group_ref=?",
        (finding_id, source_scan_id, row["group_ref"]),
    ).fetchone()
    if existing is not None:
        stored = {
            "group_check": existing["group_check"],
            "group_value": existing["group_value"],
            "group_count": existing["group_count"],
            "group_urls_json": existing["group_urls_json"],
        }
        presented = {name: row[name] for name in stored}
        if stored != presented:
            raise LedgerError("conflicting group membership under an identical source revision")
        return False
    if refuse_new:
        raise LedgerError("conflicting evidence under a previously bound source revision")
    con.execute(
        "INSERT INTO finding_group(finding_id,source_scan_id,group_ref,group_check,group_value,"
        "group_count,group_urls_json) VALUES(?,?,?,?,?,?,?)",
        (
            finding_id,
            source_scan_id,
            row["group_ref"],
            row["group_check"],
            row["group_value"],
            row["group_count"],
            row["group_urls_json"],
        ),
    )
    return True


def _insert_observation(
    con, *, occurrence_id: int, source_scan_id: int, row: dict[str, Any], refuse_new: bool
) -> bool:
    existing = con.execute(
        "SELECT payload_sha256 FROM observation "
        "WHERE occurrence_id=? AND source_scan_id=? AND issue_ordinal=?",
        (occurrence_id, source_scan_id, row["issue_ordinal"]),
    ).fetchone()
    if existing is not None:
        if existing[0] != row["payload_sha256"]:
            raise LedgerError("conflicting observation bytes under an identical source revision")
        return False
    if refuse_new:
        raise LedgerError("conflicting evidence under a previously bound source revision")
    revision = con.execute(
        "SELECT COALESCE(MAX(observation_revision),0)+1 FROM observation WHERE occurrence_id=?",
        (occurrence_id,),
    ).fetchone()[0]
    con.execute(
        "INSERT INTO observation(occurrence_id,source_scan_id,observation_revision,role,"
        "issue_ordinal,evidence_state,evidence_reason,evidence_json,payload_sha256,"
        "config_fingerprint,producer_version,producer_revision,observed_at,observed_at_state,"
        "ingested_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            occurrence_id,
            source_scan_id,
            revision,
            row["role"],
            row["issue_ordinal"],
            row["evidence_state"],
            row["evidence_reason"],
            row["evidence_json"],
            row["payload_sha256"],
            row["config_fingerprint"],
            row["producer_version"],
            row["producer_revision"],
            row["observed_at"],
            row["observed_at_state"],
            row["ingested_at"],
        ),
    )
    return True


def _affected_urls(issue: dict[str, Any]) -> list[tuple[str, str]]:
    """Ordered (canonical URL, role) members; duplicates collapse, roles may both persist."""
    members: list[tuple[str, str]] = []
    seen: set[tuple[str, str]] = set()

    def add(url: Any, role: str) -> None:
        if not isinstance(url, str) or not url.strip():
            return
        canon = canonical_url(url)
        if (canon, role) not in seen:
            seen.add((canon, role))
            members.append((canon, role))

    add(issue.get("target_url"), "subject")
    locations = issue.get("locations")
    if isinstance(locations, list):
        for location in locations:
            if not isinstance(location, dict):
                continue
            add(location.get("source_url"), "location")
            add(location.get("url"), "location")
    return members


def _location_entries(locations: list[Any]) -> list[dict[str, Any]]:
    """Return the stable source/page locator data carried by audit locations."""
    entries = []
    for location in locations:
        if not isinstance(location, dict):
            continue
        raw_url = location.get("source_url") or location.get("url")
        if not isinstance(raw_url, str) or not raw_url.strip():
            continue
        try:
            url = canonical_url(raw_url)
        except LedgerError:
            continue
        path = location.get("link_path")
        entries.append(
            {
                "url": url,
                "link_path": path.strip()
                if isinstance(path, str) and path.strip() and len(path) <= 2048
                else None,
                "representation": location.get("representation"),
            }
        )
    return entries


def _locator_groups(
    issue: dict[str, Any], entries: list[dict[str, Any]]
) -> dict[str, list[dict[str, Any]]]:
    """Return repeated URL locations whose link paths make each occurrence stable."""
    reported = issue.get("occurrences_count")
    by_url: dict[str, list[dict[str, Any]]] = {}
    for entry in entries:
        by_url.setdefault(entry["url"], []).append(entry)
    if type(reported) is not int or reported <= len(by_url):
        return {}
    return {
        url: rows
        for url, rows in by_url.items()
        if len(rows) > 1
        and all(row["link_path"] for row in rows)
        and len({row["link_path"] for row in rows}) == len(rows)
    }


def _locator_discriminator(subject_value: str, url: str, link_path: str) -> str:
    return "link_path:" + _key("seohead.ledger-link-path.v1", subject_value, url, link_path)


def _occurrence_specs(
    issue: dict[str, Any],
    *,
    subject_type: str,
    subject_value: str,
    members: list[tuple[str, str]],
    representations: dict[str, str],
) -> list[dict[str, Any]]:
    """Derive the occurrence set for one saved issue.

    The primary occurrence sits at the finding's own subject; every other
    distinct affected URL gets an occurrence discriminated by that subject, so
    two findings of one check landing on the same page stay two cases.  A
    location naming the subject itself merges into the primary occurrence.
    """

    def repr_for(url: str) -> str:
        value = issue.get("representation") or representations.get(url)
        if not isinstance(value, str) or not value:
            return "unknown"
        return value

    locations = issue.get("locations") if isinstance(issue.get("locations"), list) else []
    entries = _location_entries(locations)
    locator_groups = _locator_groups(issue, entries)
    specs = []
    if subject_type == "scope":
        specs.append(
            {
                "subject_type": "scope",
                "subject_value": _SCOPE_SUBJECT,
                "representation": "scope",
                "discriminator_type": "primary",
                "discriminator_value": "",
                "role": "target",
            }
        )
    elif subject_value in locator_groups:
        for entry in locator_groups[subject_value]:
            representation = entry["representation"]
            if representation not in _REPRESENTATIONS:
                representation = repr_for(subject_value)
            specs.append(
                {
                    "subject_type": "url",
                    "subject_value": subject_value,
                    "representation": representation,
                    "discriminator_type": "locator",
                    "discriminator_value": _locator_discriminator(
                        subject_value, subject_value, entry["link_path"]
                    ),
                    "role": "target",
                }
            )
    else:
        specs.append(
            {
                "subject_type": "url",
                "subject_value": subject_value,
                "representation": repr_for(subject_value),
                "discriminator_type": "primary",
                "discriminator_value": "",
                "role": "target",
            }
        )
    for url, _member_role in members:
        if subject_type == "url" and url == subject_value:
            continue
        if url in locator_groups:
            for entry in locator_groups[url]:
                representation = entry["representation"]
                if representation not in _REPRESENTATIONS:
                    representation = repr_for(url)
                specs.append(
                    {
                        "subject_type": "url",
                        "subject_value": url,
                        "representation": representation,
                        "discriminator_type": "locator",
                        "discriminator_value": _locator_discriminator(
                            subject_value, url, entry["link_path"]
                        ),
                        "role": "source",
                    }
                )
        else:
            specs.append(
                {
                    "subject_type": "url",
                    "subject_value": url,
                    "representation": repr_for(url),
                    "discriminator_type": "subject",
                    "discriminator_value": subject_value,
                    "role": "source",
                }
            )
    return specs


def _bind_source(
    con,
    *,
    site_id: int,
    scan: dict[str, Any],
    audit_row: dict[str, Any],
    canonical_audit_sha256: str | None,
    file_sha256: str,
    group_memberships_state: str,
    now: str,
) -> tuple[int, bool, bool, bool]:
    """Bind one validated source revision; refuse an inconsistent reused identity.

    Returns ``(source_scan_id, created, bytes_differ, restored)``.
    ``bytes_differ`` marks a same-revision binding whose container bytes differ
    from the first seen digest -- legitimate after pin/copy -- where the ingest
    must reproduce the recorded content exactly or fail rather than mint
    alternate truth.  ``restored`` records a bound artifact marked ``missing``
    that a successful re-ingest proved present again.
    """
    row = con.execute(
        "SELECT * FROM source_scan WHERE scan_uuid=? AND evidence_revision=?",
        (scan["scan_uuid"], scan["evidence_revision"]),
    ).fetchone()
    fields = {
        "site_id": site_id,
        "scan_uuid": scan["scan_uuid"],
        "format_version": scan["format_version"],
        "source_kind": scan["source_kind"],
        "lifecycle": scan["lifecycle"],
        "evidence_revision": scan["evidence_revision"],
        "audit_sha256": audit_row["sha256"],
        "canonical_audit_sha256": canonical_audit_sha256,
        "audit_schema_version": audit_row["schema_version"],
        "audit_created_at": audit_row["created_at"],
        "config_fingerprint": scan["config_fingerprint"],
        "writer_version": scan["writer_version"],
        "writer_revision": scan["writer_revision"],
        "analyzer_version": audit_row["analyzer_version"],
        "analyzer_revision": audit_row["analyzer_revision"],
        "scan_sha256": file_sha256,
        "crawl_partial": int(bool(scan["crawl_partial"])),
        "corpus_partial": int(bool(scan["corpus_partial"])),
        "artifact_state": "present",
        "missing_reason": "",
        "group_memberships_state": group_memberships_state,
        "ingested_at": now,
    }
    if row is None:
        con.execute(
            "INSERT INTO source_scan(site_id,scan_uuid,format_version,source_kind,lifecycle,"
            "evidence_revision,audit_sha256,canonical_audit_sha256,audit_schema_version,audit_created_at,"
            "config_fingerprint,writer_version,writer_revision,analyzer_version,"
            "analyzer_revision,scan_sha256,crawl_partial,corpus_partial,artifact_state,"
            "missing_reason,group_memberships_state,ingested_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                fields["site_id"],
                fields["scan_uuid"],
                fields["format_version"],
                fields["source_kind"],
                fields["lifecycle"],
                fields["evidence_revision"],
                fields["audit_sha256"],
                fields["canonical_audit_sha256"],
                fields["audit_schema_version"],
                fields["audit_created_at"],
                fields["config_fingerprint"],
                fields["writer_version"],
                fields["writer_revision"],
                fields["analyzer_version"],
                fields["analyzer_revision"],
                fields["scan_sha256"],
                fields["crawl_partial"],
                fields["corpus_partial"],
                fields["artifact_state"],
                fields["missing_reason"],
                fields["group_memberships_state"],
                fields["ingested_at"],
            ),
        )
        return int(con.execute("SELECT last_insert_rowid()").fetchone()[0]), True, False, False
    stored = dict(row)
    for name in (
        "site_id",
        "format_version",
        "source_kind",
        "lifecycle",
        "audit_sha256",
        "audit_schema_version",
        "config_fingerprint",
        "writer_version",
        "writer_revision",
        "analyzer_version",
        "analyzer_revision",
        "crawl_partial",
        "corpus_partial",
    ):
        if stored[name] != fields[name]:
            raise LedgerError(
                f"source scan {scan['scan_uuid']} revision {scan['evidence_revision']} "
                f"was already bound with different {name}; refusing alternate truth"
            )
    stored_canonical = stored["canonical_audit_sha256"]
    if stored_canonical is None and canonical_audit_sha256 is not None:
        con.execute(
            "UPDATE source_scan SET canonical_audit_sha256=? WHERE source_scan_id=?",
            (canonical_audit_sha256, stored["source_scan_id"]),
        )
    elif stored_canonical != canonical_audit_sha256:
        raise LedgerError(
            f"source scan {scan['scan_uuid']} revision {scan['evidence_revision']} "
            "was already bound with a different canonical audit digest; refusing alternate truth"
        )
    restored = stored["artifact_state"] == "missing"
    if restored:
        con.execute(
            "UPDATE source_scan SET artifact_state='present',missing_reason='' WHERE source_scan_id=?",
            (stored["source_scan_id"],),
        )
    return (
        int(stored["source_scan_id"]),
        False,
        stored["scan_sha256"] != file_sha256,
        restored,
    )


def ingest_scan(ledger: str | Path | sqlite3.Connection, scan_path: str | Path) -> dict[str, Any]:
    """Ingest one validated saved audit as baseline/history without rerunning it.

    The source artifact is opened strictly read-only through :func:`open_scan`:
    ingest never writes to the scan, never upgrades ``scan.v1`` to ``scan.v2``,
    and leaves its ``user_version`` and bytes untouched.  Re-ingesting the same
    source revision is idempotent; a new revision appends observation history.
    ``ledger`` may be a path (opened write-mode for this call) or an already
    open write connection, which receives the whole ingest in one transaction.
    """
    with contextlib.ExitStack() as resources:
        return _ingest_scan(ledger, scan_path, resources)


def _ingest_scan(ledger, scan_path, resources: contextlib.ExitStack) -> dict[str, Any]:
    path = Path(scan_path)
    digest = file_sha256(path)
    scan_con = open_scan(path)
    audit_reader = None
    streamed_issues = None
    group_memberships_state = "complete"
    try:
        scan_row = scan_con.execute("SELECT * FROM scan WHERE singleton=1").fetchone()
        audit = scan_con.execute("SELECT * FROM audit WHERE singleton=1").fetchone()
        if scan_row is None:
            raise LedgerError("source scan has no header to ingest")
        scan = dict(scan_row)
        has_config_extension = scan_con.execute(
            "SELECT 1 FROM context_items WHERE kind='native_config_extension' AND item_key='run'"
        ).fetchone()
        has_thresholds = scan_con.execute(
            "SELECT 1 FROM context_items WHERE kind='analysis_asset_thresholds' AND item_key='run'"
        ).fetchone()
        if scan["source_kind"] in {"native", "reanalysis"} and (
            has_config_extension or has_thresholds
        ):
            from seohead.crawl.settings import fingerprint
            from seohead.storage.native_scan import read_effective_config

            # Bind the measured results policy, including the validated adjunct,
            # while the source artifact keeps its frozen header and bytes.
            if has_config_extension:
                scan["config_fingerprint"] = fingerprint(read_effective_config(scan_con))
            else:
                historical = json.loads(scan["config_json"])
                historical.setdefault("analysis", {})["asset_thresholds"] = read_effective_config(
                    scan_con
                )["analysis"]["asset_thresholds"]
                scan["config_fingerprint"] = fingerprint(historical)
        from seohead.sf.core.evidence_contract import attach_contract

        if audit is not None:
            audit_row = dict(audit)
            document = _loads(audit_row["document_json"], "audit")
            canonical_audit_sha256 = _json_sha256(document)
            projected = attach_contract(document, scan_uuid=scan["scan_uuid"], con=scan_con)
            representations = _representation_map(scan_con, document)
            by_url, doc_url = _observation_index(scan_con)
            raw_issues = document.get("issues") if isinstance(document.get("issues"), list) else []
            groups = document.get("groups") or []
        else:
            from seohead.sf.core.evidence_contract import attach_contract_parts
            from seohead.sf.core.models import _Rows
            from seohead.storage.audit_v2 import AuditV2Reader

            audit_reader = resources.enter_context(AuditV2Reader(path))
            document = dict(audit_reader.header)
            if document.get("schema_version") != "2.0":
                raise LedgerError("audit.v2 header has an unsupported schema version")
            if "/issues" not in audit_reader.collections:
                raise LedgerError("audit.v2 source has no retained issue collection")
            rows = _Rows(lambda: audit_reader.iter_collection("/issues"))
            projected, streamed_issues = attach_contract_parts(
                document, rows, scan_uuid=scan["scan_uuid"], con=None
            )
            document = projected
            audit_row = {
                "sha256": audit_reader.sha256,
                "schema_version": document["schema_version"],
                "created_at": (document.get("run") or {}).get("generated_at"),
                "analyzer_version": audit_reader.binding["analyzer_version"],
                "analyzer_revision": audit_reader.binding["analyzer_revision"],
            }
            canonical_audit_sha256 = None
            representations = {}
            by_url, doc_url = {}, {}
            raw_issues = None
            groups = None
            if "/groups" in audit_reader.collections:
                groups = audit_reader.iter_collection("/groups")
            else:
                group_memberships_state = "unavailable"
    finally:
        scan_con.close()
    run = document.get("run") if isinstance(document.get("run"), dict) else {}
    document_uuid = run.get("scan_uuid")
    # A native/reanalysis audit's own scan_uuid must agree with its container;
    # a legacy import carries the *original* run's uuid as provenance while the
    # binding uuid was minted at import time, so it is compared as data instead.
    if (
        scan["source_kind"] != "legacy_import"
        and isinstance(document_uuid, str)
        and document_uuid
        and document_uuid != scan["scan_uuid"]
    ):
        raise LedgerError("saved audit scan UUID disagrees with its scan header")
    start = scan.get("start_url") or run.get("start_url") or run.get("source")
    netloc = urlsplit(start).netloc.lower() if isinstance(start, str) and start else ""
    if not netloc:
        raise LedgerError("source scan does not record the site it belongs to")
    if streamed_issues is None:
        issues = projected.get("issues") if isinstance(projected.get("issues"), list) else []
    else:
        # ``attach_contract_parts`` returns a re-iterable disk-backed stream.
        # It must not be wrapped in list() here: a million-row audit.v2 is the
        # exact case this ledger ingestion path exists to retain.
        issues = streamed_issues
    observed_at = _validated_time(run.get("generated_at"))

    own = not isinstance(ledger, sqlite3.Connection)
    con = open_ledger(ledger, write=True) if own else ledger
    group_ids_created = False
    try:
        site = _site_row(con, netloc)
        if site is None:
            raise LedgerError(
                f"scan site {netloc!r} does not match any registered ledger site identity"
            )
        site_uuid, site_host = site["project_uuid"], site["host"]
        now = _utc()
        con.execute("BEGIN IMMEDIATE")
        counts = {
            "findings": 0,
            "occurrences": 0,
            "affected_urls": 0,
            "finding_observations": 0,
            "group_memberships": 0,
            "observations": 0,
        }
        source_scan_id, new_source, bytes_differ, restored = _bind_source(
            con,
            site_id=int(site["site_id"]),
            scan=scan,
            audit_row=audit_row,
            canonical_audit_sha256=canonical_audit_sha256,
            file_sha256=digest,
            group_memberships_state=group_memberships_state,
            now=now,
        )
        con.execute("CREATE TEMP TABLE ingest_group_ids (group_ref TEXT PRIMARY KEY)")
        group_ids_created = True
        for group in groups if groups is not None else ():
            if (
                not isinstance(group, dict)
                or not isinstance(group.get("group_id"), str)
                or not group["group_id"]
            ):
                group_memberships_state = "partial"
                continue
            try:
                con.execute("INSERT INTO ingest_group_ids VALUES(?)", (group["group_id"],))
            except sqlite3.IntegrityError as exc:
                raise LedgerError("saved audit contains duplicate group identities") from exc
            _created, complete = _store_source_group(
                con, source_scan_id=source_scan_id, group=group, refuse_new=not new_source
            )
            if not complete:
                group_memberships_state = "partial"
        # Once a source revision is bound, a replay must reproduce its recorded
        # rows exactly: a new row under a bound revision means the same claimed
        # identity now presents different content, which is refused, not merged.
        refuse_new = not new_source
        for ordinal, projected_issue in enumerate(issues):
            if not isinstance(projected_issue, dict):
                raise LedgerError("saved audit issue is not an object")
            issue = (
                raw_issues[ordinal]
                if isinstance(raw_issues, list) and ordinal < len(raw_issues)
                else projected_issue
            )
            check_key = projected_issue.get("check")
            check_id = _check_id(con, check_key, refuse_new)
            target = projected_issue.get("target_url")
            if isinstance(target, str) and target.strip():
                subject_type, subject_value = "url", canonical_url(target)
            else:
                subject_type, subject_value = "scope", _SCOPE_SUBJECT
            finding_id, created, _ = _insert_finding(
                con,
                site_id=int(site["site_id"]),
                check_id=check_id,
                check_key=check_key,
                project_uuid=site_uuid,
                site_host=site_host,
                subject_type=subject_type,
                subject_value=subject_value,
                now=now,
                refuse_new=refuse_new,
            )
            counts["findings"] += int(created)
            members = _affected_urls(projected_issue)
            for url, role in members:
                counts["affected_urls"] += _insert_member(
                    con,
                    finding_id=finding_id,
                    url=url,
                    role=role,
                    now=now,
                    refuse_new=refuse_new,
                )
            contract = (
                projected_issue.get("evidence", {}).get("contract", {})
                if isinstance(projected_issue.get("evidence"), dict)
                else {}
            )
            specs = _occurrence_specs(
                projected_issue,
                subject_type=subject_type,
                subject_value=subject_value,
                members=members,
                representations=representations,
            )
            locations = (
                projected_issue.get("locations")
                if isinstance(projected_issue.get("locations"), list)
                else []
            )
            coverage_state, enumerated = _coverage_state(projected_issue, locations)
            payload = _dump(issue)
            if len(payload.encode("utf-8")) > MAX_PAYLOAD_BYTES:
                raise LedgerError("saved issue projection exceeds the ledger payload bound")
            counts["finding_observations"] += _insert_projection(
                con,
                finding_id=finding_id,
                source_scan_id=source_scan_id,
                refuse_new=refuse_new,
                row={
                    "issue_ordinal": str(projected_issue.get("id") or ""),
                    "audit_fingerprint": projected_issue.get("fingerprint")
                    if isinstance(projected_issue.get("fingerprint"), str)
                    else None,
                    "severity": str(projected_issue.get("severity") or ""),
                    "status_code": projected_issue.get("status_code")
                    if type(projected_issue.get("status_code")) is int
                    else None,
                    "message": str(projected_issue.get("message") or ""),
                    "occurrences_count": projected_issue.get("occurrences_count")
                    if type(projected_issue.get("occurrences_count")) is int
                    else 0,
                    "enumerated_count": enumerated,
                    "coverage_state": coverage_state,
                    "payload_json": payload,
                    "payload_sha256": hashlib.sha256(payload.encode("utf-8")).hexdigest(),
                },
            )
            group_ref = projected_issue.get("group_id")
            if isinstance(group_ref, str) and group_ref and groups is not None:
                row = con.execute(
                    "SELECT * FROM source_group WHERE source_scan_id=? AND group_ref=?",
                    (source_scan_id, group_ref),
                ).fetchone()
                group = dict(row) if row is not None else None
                if group is None:
                    group = {}
                    group_memberships_state = "partial"
                counts["group_memberships"] += _insert_group(
                    con,
                    finding_id=finding_id,
                    source_scan_id=source_scan_id,
                    refuse_new=refuse_new,
                    row={
                        "group_ref": group_ref,
                        "group_check": str(group.get("group_check") or check_key),
                        "group_value": group.get("group_value"),
                        "group_count": group.get("group_count"),
                        "group_urls_json": "[]",
                    },
                )
            elif isinstance(group_ref, str) and group_ref:
                # The audit.v2 stream retained the issue's group reference but
                # this ledger reader has not materialized a group lookup table.
                # Keep that loss explicit in the ingest result rather than
                # inventing an empty group membership.
                if group_memberships_state != "partial":
                    group_memberships_state = "unavailable"
            for spec in specs:
                occurrence_id, created, _ = _insert_occurrence(
                    con,
                    finding_id=finding_id,
                    check_id=check_id,
                    check_key=check_key,
                    project_uuid=site_uuid,
                    site_host=site_host,
                    now=now,
                    refuse_new=refuse_new,
                    **{name: value for name, value in spec.items() if name != "role"},
                )
                counts["occurrences"] += int(created)
                evidence = _occurrence_evidence(
                    contract,
                    role=spec["role"],
                    url=spec["subject_value"] if spec["subject_type"] == "url" else None,
                    by_url=by_url,
                    doc_url=doc_url,
                )
                evidence_json = _dump(evidence)
                occurrence_payload = _dump(
                    {
                        "occurrence": spec,
                        "issue_ordinal": str(projected_issue.get("id") or ""),
                        "severity": projected_issue.get("severity"),
                        "status_code": projected_issue.get("status_code"),
                        "message": projected_issue.get("message"),
                        "occurrences_count": projected_issue.get("occurrences_count"),
                        "coverage_state": coverage_state,
                        "group_ref": group_ref if isinstance(group_ref, str) else None,
                        "evidence": evidence,
                    }
                )
                counts["observations"] += _insert_observation(
                    con,
                    occurrence_id=occurrence_id,
                    source_scan_id=source_scan_id,
                    refuse_new=refuse_new,
                    row={
                        "role": spec["role"],
                        "issue_ordinal": str(projected_issue.get("id") or ""),
                        "evidence_state": evidence["state"],
                        "evidence_reason": evidence["reason"],
                        "evidence_json": evidence_json,
                        "payload_sha256": hashlib.sha256(
                            occurrence_payload.encode("utf-8")
                        ).hexdigest(),
                        "config_fingerprint": scan["config_fingerprint"],
                        "producer_version": audit_row["analyzer_version"],
                        "producer_revision": audit_row["analyzer_revision"],
                        "observed_at": observed_at,
                        "observed_at_state": "known" if observed_at else "unknown",
                        "ingested_at": now,
                    },
                )
        # Membership completeness is a derived reader result, not source identity.
        # Persist the final state after every issue reference has been checked,
        # including a correction to a previously ingested immutable source.
        scope_changed = con.execute(
            "UPDATE source_scan SET group_memberships_state=? "
            "WHERE source_scan_id=? AND group_memberships_state!=?",
            (group_memberships_state, source_scan_id, group_memberships_state),
        ).rowcount
        changed = new_source or restored or bool(scope_changed) or any(counts.values())
        if changed:
            con.execute("UPDATE ledger SET ledger_revision=ledger_revision+1 WHERE singleton=1")
        revision = con.execute("SELECT ledger_revision FROM ledger WHERE singleton=1").fetchone()[0]
        con.commit()
    except BaseException:
        con.rollback()
        raise
    finally:
        if group_ids_created:
            con.execute("DROP TABLE IF EXISTS temp.ingest_group_ids")
        if own:
            con.close()
    return {
        "ok": True,
        "ledger_revision": int(revision),
        "site_id": int(site["site_id"]),
        "site_netloc": netloc,
        "source_scan_id": source_scan_id,
        "scan_uuid": scan["scan_uuid"],
        "format_version": scan["format_version"],
        "evidence_revision": scan["evidence_revision"],
        "audit_sha256": audit_row["sha256"],
        "new_source": new_source,
        "already_recorded": not changed,
        "scan_bytes_differ": bytes_differ,
        "recorded": {**counts, "group_memberships_state": group_memberships_state},
    }


def note_source_missing(
    ledger: str | Path | sqlite3.Connection, source_scan_id: int, *, reason: str
) -> dict[str, Any]:
    """Record that a bound source artifact is gone (pruned or moved away).

    Only the availability flag changes: the binding, digests and observation
    history stay so every recorded case still cites its original evidence.
    """
    if not isinstance(reason, str) or not reason or len(reason) > 512:
        raise LedgerError("a missing-source note requires a bounded reason")
    own = not isinstance(ledger, sqlite3.Connection)
    con = open_ledger(ledger, write=True) if own else ledger
    try:
        row = con.execute(
            "SELECT source_scan_id,artifact_state FROM source_scan WHERE source_scan_id=?",
            (source_scan_id,),
        ).fetchone()
        if row is None:
            raise LedgerError("unknown source scan binding")
        con.execute("BEGIN IMMEDIATE")
        try:
            if row["artifact_state"] != "missing":
                con.execute(
                    "UPDATE source_scan SET artifact_state='missing',missing_reason=? "
                    "WHERE source_scan_id=?",
                    (reason, source_scan_id),
                )
                con.execute("UPDATE ledger SET ledger_revision=ledger_revision+1 WHERE singleton=1")
            revision = con.execute(
                "SELECT ledger_revision FROM ledger WHERE singleton=1"
            ).fetchone()[0]
            con.commit()
        except BaseException:
            con.rollback()
            raise
    finally:
        if own:
            con.close()
    return {"ok": True, "ledger_revision": int(revision)}


def _lifecycle_text(value: Any, name: str, maximum: int) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > maximum:
        raise LedgerError(f"{name} must be nonempty text of at most {maximum} characters")
    return value.strip()


def occurrence_matches_baseline(
    con: sqlite3.Connection,
    occurrence_id: int,
    *,
    audit_sha256: str,
    results_policy_fingerprint: str,
) -> bool:
    """Return whether one occurrence belongs to an equivalent retained baseline.

    A scan UUID remains provenance only: it is reusable in copied or altered
    documents.  Legacy audit JSON can use the canonical digest retained at
    ingestion, while audit.v2 uses its independently validated exact digest.
    Both routes require the originally recorded results-affecting policy.
    """
    if type(occurrence_id) is not int or occurrence_id < 1:
        raise LedgerError("occurrence_id must be a positive integer")
    if not isinstance(audit_sha256, str) or not _SHA256.fullmatch(audit_sha256):
        raise LedgerError("baseline audit_sha256 is invalid")
    if not isinstance(results_policy_fingerprint, str) or not _FINGERPRINT.fullmatch(
        results_policy_fingerprint
    ):
        raise LedgerError("baseline results_policy_fingerprint is invalid")
    row = con.execute(
        "SELECT 1 FROM observation ob JOIN source_scan s ON s.source_scan_id=ob.source_scan_id "
        "WHERE ob.occurrence_id=? AND ob.role='target' "
        "AND (s.audit_sha256=? OR s.canonical_audit_sha256=?) "
        "AND s.config_fingerprint=? LIMIT 1",
        (occurrence_id, audit_sha256, audit_sha256, results_policy_fingerprint),
    ).fetchone()
    return row is not None


def _finding_state(con: sqlite3.Connection, finding_id: int) -> str:
    """Project exact occurrence states to one conservative finding state.

    A finding is resolved only when every retained occurrence is resolved.
    Mixed evidence is never flattened to resolved; the most actionable retained
    state remains visible to a page that lists findings.
    """
    states = {
        row[0]
        for row in con.execute(
            "SELECT current_state FROM occurrence WHERE finding_id=?", (finding_id,)
        )
    }
    if not states:
        return "detected"
    if states == {"resolved"}:
        return "resolved"
    if "regressed" in states:
        return "regressed"
    if "persisting" in states:
        return "persisting"
    if "recheck_pending" in states:
        return "recheck_pending"
    if states == {"false_positive_reviewed"}:
        return "false_positive_reviewed"
    if "unverifiable" in states:
        return "unverifiable"
    if "fix_reported" in states:
        return "fix_reported"
    if "verified" in states:
        return "verified"
    return "detected"


def _outcome_observation(
    con: sqlite3.Connection, occurrence_id: int, observation_id: int | None
) -> int | None:
    """Validate an evidence reference used for a measured lifecycle outcome."""
    if observation_id is None:
        raise LedgerError("a measured lifecycle outcome requires an observation_id")
    if type(observation_id) is not int or observation_id < 1:
        raise LedgerError("observation_id must be a positive integer")
    row = con.execute(
        "SELECT o.observation_id,o.observation_revision,o.evidence_state,s.artifact_state "
        "FROM observation o JOIN source_scan s ON s.source_scan_id=o.source_scan_id "
        "WHERE o.observation_id=? AND o.occurrence_id=?",
        (observation_id, occurrence_id),
    ).fetchone()
    if row is None:
        raise LedgerError("observation_id does not belong to this occurrence")
    if row["evidence_state"] != "measured" or row["artifact_state"] != "present":
        raise LedgerError("measured lifecycle outcomes require retained measured evidence")
    if row["observation_revision"] <= 1:
        raise LedgerError(
            "measured lifecycle outcomes require a later observation, not baseline evidence"
        )
    return int(row["observation_id"])


def transition_occurrence(
    ledger: str | Path | sqlite3.Connection,
    *,
    occurrence_key: str,
    state: str,
    actor: str,
    reason: str,
    expected_revision: int,
    observation_id: int | None = None,
    decided_at: str | None = None,
) -> dict[str, Any]:
    """Append one validated, revision-safe lifecycle decision for a case.

    The caller supplies the ledger revision read from ``ledger_summary`` or
    ``read_cases``.  A stale writer is refused before mutation.  In particular,
    a user assertion cannot resolve a finding: ``resolved``, ``persisting`` and
    ``regressed`` require a retained measured observation after the baseline.
    """
    if not isinstance(occurrence_key, str) or not _SHA256.fullmatch(occurrence_key):
        raise LedgerError("occurrence_key must be a lowercase SHA-256 key")
    if state not in _LIFECYCLE_STATES:
        raise LedgerError("unsupported lifecycle state")
    actor = _lifecycle_text(actor, "actor", 128)
    reason = _lifecycle_text(reason, "reason", 2048)
    if type(expected_revision) is not int or expected_revision < 0:
        raise LedgerError("expected_revision must be a nonnegative integer")
    if decided_at is not None and _validated_time(decided_at) is None:
        raise LedgerError("decided_at must be a UTC ISO-8601 timestamp")

    own = not isinstance(ledger, sqlite3.Connection)
    con = open_ledger(ledger, write=True) if own else ledger
    try:
        con.execute("BEGIN IMMEDIATE")
        header = con.execute("SELECT ledger_revision FROM ledger WHERE singleton=1").fetchone()
        if header is None or header["ledger_revision"] != expected_revision:
            raise LedgerError("ledger revision changed; reread cases before recording a decision")
        occurrence = con.execute(
            "SELECT occurrence_id,finding_id,current_state FROM occurrence WHERE occurrence_key=?",
            (occurrence_key,),
        ).fetchone()
        if occurrence is None:
            raise LedgerError("unknown occurrence_key")
        previous = occurrence["current_state"]
        if state == previous:
            raise LedgerError("lifecycle transition must change the current state")
        if state not in _TRANSITIONS.get(previous, frozenset()):
            raise LedgerError(f"invalid lifecycle transition from {previous!r} to {state!r}")
        linked_observation = (
            _outcome_observation(con, int(occurrence["occurrence_id"]), observation_id)
            if state in _MEASURED_OUTCOMES
            else None
        )
        if observation_id is not None and state not in _MEASURED_OUTCOMES:
            linked_observation = _outcome_observation(
                con, int(occurrence["occurrence_id"]), observation_id
            )
        next_revision = expected_revision + 1
        recorded_at = decided_at or _utc()
        con.execute(
            "INSERT INTO decision(occurrence_id,state,actor,reason,decided_at,decided_at_state,"
            "observation_id,ledger_revision) VALUES(?,?,?,?,?,?,?,?)",
            (
                occurrence["occurrence_id"],
                state,
                actor,
                reason,
                recorded_at,
                "known",
                linked_observation,
                next_revision,
            ),
        )
        con.execute(
            "UPDATE occurrence SET current_state=? WHERE occurrence_id=?",
            (state, occurrence["occurrence_id"]),
        )
        finding_state = _finding_state(con, int(occurrence["finding_id"]))
        con.execute(
            "UPDATE finding SET current_state=? WHERE finding_id=?",
            (finding_state, occurrence["finding_id"]),
        )
        con.execute("UPDATE ledger SET ledger_revision=? WHERE singleton=1", (next_revision,))
        con.commit()
    except BaseException:
        con.rollback()
        raise
    finally:
        if own:
            con.close()
    return {
        "ok": True,
        "occurrence_key": occurrence_key,
        "previous_state": previous,
        "state": state,
        "finding_state": finding_state,
        "observation_id": linked_observation,
        "ledger_revision": next_revision,
    }


def record_verification(
    ledger: str | Path | sqlite3.Connection,
    verification_path: str | Path,
    *,
    actor: str,
    expected_revision: int,
    occurrence_keys: list[str] | None = None,
    task_id: str = "unassigned",
) -> dict[str, Any]:
    """Apply one retained ``verification.v1`` artifact to pending ledger cases.

    The artifact remains a standalone immutable file, while the ledger stores
    its byte digest, exact selected scope and one typed result per occurrence.
    Measured outcomes are refused unless the guarded verifier retained the
    before/after page evidence that supports them.  ``occurrence_keys`` is the
    preferred exact binding used by the ledger-aware recheck command; the
    legacy check/URL lookup remains only for one unambiguous local occurrence.
    """
    path = Path(verification_path)
    if not path.is_file() or path.is_symlink() or path.stat().st_size > MAX_PAYLOAD_BYTES:
        raise LedgerError("verification artifact must be a bounded regular file")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise LedgerError("verification artifact is not valid JSON") from exc
    if document.get("schema_version") != "verification.v1":
        raise LedgerError("verification artifact must use schema_version verification.v1")
    findings = document.get("findings")
    if not isinstance(findings, list) or not findings:
        raise LedgerError("verification artifact has no finding results")
    if type(expected_revision) is not int or expected_revision < 0:
        raise LedgerError("expected_revision must be a nonnegative integer")
    actor = _lifecycle_text(actor, "actor", 128)
    task_id = _lifecycle_text(task_id, "task_id", 128)
    if occurrence_keys is not None:
        if not isinstance(occurrence_keys, list) or len(occurrence_keys) != len(findings):
            raise LedgerError(
                "occurrence_keys must name exactly one key for every verification row"
            )
        if any(not isinstance(key, str) or not _SHA256.fullmatch(key) for key in occurrence_keys):
            raise LedgerError("occurrence_keys must contain lowercase SHA-256 keys")
        if len(set(occurrence_keys)) != len(occurrence_keys):
            raise LedgerError("occurrence_keys must not repeat a ledger case")
    baseline = document.get("baseline")
    selection = document.get("selection")
    collection = document.get("collection")
    if (
        not isinstance(baseline, dict)
        or not isinstance(selection, dict)
        or not isinstance(collection, dict)
    ):
        raise LedgerError("verification artifact lacks baseline, selection or collection evidence")
    baseline_sha = baseline.get("audit_sha256")
    if not isinstance(baseline_sha, str) or not _SHA256.fullmatch(baseline_sha):
        raise LedgerError("verification artifact baseline audit_sha256 is invalid")
    baseline_policy = baseline.get("results_policy_fingerprint")
    if not isinstance(baseline_policy, str) or not _FINGERPRINT.fullmatch(baseline_policy):
        raise LedgerError("verification artifact baseline results_policy_fingerprint is invalid")
    baseline_scan_uuid = baseline.get("scan_uuid")
    if baseline_scan_uuid is not None and (
        not isinstance(baseline_scan_uuid, str) or not baseline_scan_uuid
    ):
        raise LedgerError("verification artifact baseline scan_uuid is invalid")
    collection_state = collection.get("state")
    if collection_state not in {"measured", "offline", "partial", "not_run", "not_verifiable"}:
        raise LedgerError("verification artifact collection state is invalid")
    artifact_sha256 = file_sha256(path)
    outcomes = {
        "resolved": "resolved",
        "persisting": "persisting",
        "changed": "regressed",
        "not_verifiable": "unverifiable",
    }

    own = not isinstance(ledger, sqlite3.Connection)
    con = open_ledger(ledger, write=True) if own else ledger
    try:
        con.execute("BEGIN IMMEDIATE")
        header = con.execute("SELECT ledger_revision FROM ledger WHERE singleton=1").fetchone()
        if header is None or header["ledger_revision"] != expected_revision:
            raise LedgerError("ledger revision changed; reread cases before recording verification")
        existing = con.execute(
            "SELECT verification_id FROM verification_artifact WHERE artifact_sha256=?",
            (artifact_sha256,),
        ).fetchone()
        if existing is not None:
            raise LedgerError("verification artifact was already recorded")
        con.execute(
            "INSERT INTO verification_artifact(artifact_sha256,schema_version,artifact_path,task_id,"
            "baseline_audit_sha256,selection_sha256,collection_sha256,collection_state,recorded_at) "
            "VALUES(?,?,?,?,?,?,?,?,?)",
            (
                artifact_sha256,
                "verification.v1",
                str(path.absolute()),
                task_id,
                baseline_sha,
                _json_sha256(selection),
                _json_sha256(collection),
                collection_state,
                _utc(),
            ),
        )
        verification_id = int(con.execute("SELECT last_insert_rowid()").fetchone()[0])
        prepared: list[tuple[sqlite3.Row, str, str, dict[str, Any]]] = []
        seen: set[int] = set()
        for index, item in enumerate(findings):
            if not isinstance(item, dict):
                raise LedgerError("verification finding result must be an object")
            check, url, status = item.get("check"), item.get("url"), item.get("status")
            if not isinstance(check, str) or not isinstance(url, str) or status not in outcomes:
                raise LedgerError("verification finding has unsupported check, URL or status")
            canonical = canonical_url(url)
            if occurrence_keys is None:
                rows = con.execute(
                    "SELECT o.occurrence_id,o.finding_id,o.current_state,o.occurrence_key FROM occurrence o "
                    "JOIN check_def c ON c.check_id=o.check_id "
                    "WHERE c.check_key=? AND o.subject_type='url' AND o.subject_value=?",
                    (check, canonical),
                ).fetchall()
            else:
                rows = con.execute(
                    "SELECT o.occurrence_id,o.finding_id,o.current_state,o.occurrence_key,c.check_key "
                    "FROM occurrence o JOIN check_def c ON c.check_id=o.check_id "
                    "WHERE o.occurrence_key=? AND o.subject_type='url' AND o.subject_value=? "
                    "AND c.check_key=?",
                    (occurrence_keys[index], canonical, check),
                ).fetchall()
            if len(rows) != 1:
                raise LedgerError(
                    "verification finding does not map to exactly one ledger occurrence"
                )
            occurrence = rows[0]
            if occurrence["occurrence_id"] in seen:
                raise LedgerError(
                    "verification artifact maps multiple rows to one ledger occurrence"
                )
            if occurrence["current_state"] != "recheck_pending":
                raise LedgerError("verification result requires a recheck_pending occurrence")
            if not occurrence_matches_baseline(
                con,
                int(occurrence["occurrence_id"]),
                audit_sha256=baseline_sha,
                results_policy_fingerprint=baseline_policy,
            ):
                raise LedgerError(
                    "verification artifact baseline does not bind the selected ledger case"
                )
            before = item.get("before")
            before_page = item.get("before_page")
            after = item.get("after")
            after_page = item.get("after_page")
            if not isinstance(before, dict) or not isinstance(before_page, dict):
                raise LedgerError("verification result lacks exact baseline finding/page evidence")
            state = outcomes[status]
            measured = state in _MEASURED_OUTCOMES
            if measured and collection_state not in {"measured", "offline", "partial"}:
                raise LedgerError("measured verification outcome lacks a completed collection")
            if measured and not isinstance(after_page, dict):
                raise LedgerError("measured verification outcome lacks an after page observation")
            detail = item.get("reason")
            if not isinstance(detail, str):
                detail = ""
            reason = _lifecycle_text(
                f"verification.v1 sha256={artifact_sha256} row={index}; {detail or 'bounded recheck result'}",
                "reason",
                2048,
            )
            evidence = {
                "row_index": index,
                "before_sha256": _json_sha256({"finding": before, "page": before_page}),
                "after_sha256": (
                    _json_sha256({"finding": after, "page": after_page})
                    if isinstance(after_page, dict)
                    else None
                ),
                "scope_sha256": _json_sha256(
                    {
                        "check": check,
                        "url": canonical,
                        "before": {"finding": before, "page": before_page},
                        "after": {"finding": after, "page": after_page},
                    }
                ),
                "measured": measured,
                "reason": detail,
            }
            prepared.append((occurrence, state, reason, evidence))
            seen.add(int(occurrence["occurrence_id"]))
        next_revision = expected_revision + 1
        for occurrence, state, reason, evidence in prepared:
            con.execute(
                "INSERT INTO decision(occurrence_id,state,actor,reason,decided_at,decided_at_state,"
                "observation_id,ledger_revision,verification_result_id) VALUES(?,?,?,?,?,?,?,?,?)",
                (
                    occurrence["occurrence_id"],
                    state,
                    actor,
                    reason,
                    _utc(),
                    "known",
                    None,
                    next_revision,
                    None,
                ),
            )
            decision_id = int(con.execute("SELECT last_insert_rowid()").fetchone()[0])
            con.execute(
                "INSERT INTO verification_result(verification_id,occurrence_id,row_index,outcome,measured,"
                "before_sha256,after_sha256,scope_sha256,reason) VALUES(?,?,?,?,?,?,?,?,?)",
                (
                    verification_id,
                    occurrence["occurrence_id"],
                    evidence["row_index"],
                    state,
                    int(evidence["measured"]),
                    evidence["before_sha256"],
                    evidence["after_sha256"],
                    evidence["scope_sha256"],
                    evidence["reason"],
                ),
            )
            verification_result_id = int(con.execute("SELECT last_insert_rowid()").fetchone()[0])
            con.execute(
                "UPDATE decision SET verification_result_id=? WHERE decision_id=?",
                (verification_result_id, decision_id),
            )
            con.execute(
                "UPDATE occurrence SET current_state=? WHERE occurrence_id=?",
                (state, occurrence["occurrence_id"]),
            )
            finding_state = _finding_state(con, int(occurrence["finding_id"]))
            con.execute(
                "UPDATE finding SET current_state=? WHERE finding_id=?",
                (finding_state, occurrence["finding_id"]),
            )
        con.execute("UPDATE ledger SET ledger_revision=? WHERE singleton=1", (next_revision,))
        con.commit()
    except BaseException:
        con.rollback()
        raise
    finally:
        if own:
            con.close()
    return {
        "ok": True,
        "ledger_revision": next_revision,
        "verification_sha256": artifact_sha256,
        "verification_id": verification_id,
        "task_id": task_id,
        "recorded": len(prepared),
    }


def _membership_scope(con: sqlite3.Connection, represented_occurrences: int) -> dict[str, Any]:
    """Describe the boundary between represented cases and source group scope.

    A group count can name a much larger population than the URLs retained in
    its issue rows. Those missing members have no occurrence keys and cannot
    be assigned an outcome. The scope is therefore evidence, not a guessed
    denominator: any truncated or unavailable group withholds full-scope
    percentages while preserving exact metrics for represented occurrences.
    """
    known_unrepresented = 0
    unknown_groups = 0
    incomplete_groups = 0
    samples: list[dict[str, Any]] = []
    for row in con.execute(
        "SELECT fg.source_scan_id,fg.group_ref,MAX(fg.group_count) AS group_count,"
        "COUNT(DISTINCT o.occurrence_id) AS represented "
        "FROM finding_group fg LEFT JOIN occurrence o ON o.finding_id=fg.finding_id "
        "GROUP BY fg.source_scan_id,fg.group_ref ORDER BY fg.source_scan_id,fg.group_ref"
    ):
        count = row["group_count"]
        represented = int(row["represented"])
        if type(count) is not int or count < represented:
            unknown_groups += 1
            incomplete_groups += 1
            kind = "unknown"
            missing = None
        elif count > represented:
            missing = count - represented
            known_unrepresented += missing
            incomplete_groups += 1
            kind = "partial"
        else:
            continue
        if len(samples) < 20:
            samples.append(
                {
                    "source_scan_id": int(row["source_scan_id"]),
                    "group_ref": row["group_ref"],
                    "state": kind,
                    "represented_members": represented,
                    "known_unrepresented_members": missing,
                }
            )
    source_states = [
        row["group_memberships_state"]
        for row in con.execute(
            "SELECT group_memberships_state FROM source_scan "
            "WHERE group_memberships_state!='complete'"
        )
    ]
    incomplete_groups += len(source_states)
    if unknown_groups or "unavailable" in source_states:
        state = "unknown"
    elif incomplete_groups:
        state = "partial"
    else:
        state = "complete"
    return {
        "state": state,
        "represented_occurrences": represented_occurrences,
        "known_unrepresented_members": known_unrepresented,
        "unknown_group_memberships": unknown_groups + len(source_states),
        "incomplete_group_memberships": incomplete_groups,
        "samples": samples,
    }


def remediation_summary(ledger: str | Path | sqlite3.Connection) -> dict[str, Any]:
    """Return case counts and explicitly named remediation/recheck denominators.

    ``resolved_percent`` includes unresolved evidence states in its denominator;
    ``rechecked_percent`` deliberately excludes ``unverifiable`` from its
    numerator.  This makes a partial or failed recheck visible instead of
    shrinking the original case population.
    """
    own = not isinstance(ledger, sqlite3.Connection)
    con = open_ledger(ledger) if own else ledger
    try:
        counts = {state: 0 for state in _LIFECYCLE_STATES}
        for row in con.execute(
            "SELECT current_state,COUNT(*) AS count FROM occurrence GROUP BY current_state"
        ):
            counts[row["current_state"]] = int(row["count"])
        original = sum(counts.values())
        false_positive = counts["false_positive_reviewed"]
        remediation_denominator = original - false_positive
        resolved = counts["resolved"]
        rechecked = resolved + counts["persisting"] + counts["regressed"]
        scope = _membership_scope(con, original)
        scope_complete = scope["state"] == "complete"
        represented_resolved_percent = (
            round(100 * resolved / remediation_denominator, 1) if remediation_denominator else None
        )
        represented_rechecked_percent = round(100 * rechecked / original, 1) if original else None
        task_counts: dict[str, dict[str, int]] = {}
        for row in con.execute(
            "SELECT o.current_state,COALESCE((SELECT va.task_id FROM decision d "
            "JOIN verification_result vr ON vr.verification_result_id=d.verification_result_id "
            "JOIN verification_artifact va ON va.verification_id=vr.verification_id "
            "WHERE d.occurrence_id=o.occurrence_id ORDER BY d.decision_id DESC LIMIT 1),"
            "'unassigned') AS task_id FROM occurrence o"
        ):
            bucket = task_counts.setdefault(
                row["task_id"], {state: 0 for state in _LIFECYCLE_STATES}
            )
            bucket[row["current_state"]] += 1

        def task_view(task_counts: dict[str, int]) -> dict[str, Any]:
            task_original = sum(task_counts.values())
            task_actionable = task_original - task_counts["false_positive_reviewed"]
            task_rechecked = (
                task_counts["resolved"] + task_counts["persisting"] + task_counts["regressed"]
            )
            if not task_original:
                status = "unavailable"
            elif task_counts["unverifiable"]:
                status = "blocked"
            elif any(
                task_counts[state]
                for state in ("detected", "verified", "fix_reported", "recheck_pending")
            ):
                status = "in_progress"
            else:
                status = "complete"
            if status == "complete" and not scope_complete:
                status = "scope_incomplete"
            return {
                "status": status,
                "counts": task_counts,
                "denominators": {
                    "verified_original_occurrences": task_original,
                    "remediation_cases": task_actionable,
                },
                "represented_resolved_percent": (
                    round(100 * task_counts["resolved"] / task_actionable, 1)
                    if task_actionable
                    else None
                ),
                "represented_rechecked_percent": (
                    round(100 * task_rechecked / task_original, 1) if task_original else None
                ),
                "scope_state": scope["state"],
                "resolved_percent": (
                    round(100 * task_counts["resolved"] / task_actionable, 1)
                    if scope_complete and task_actionable
                    else None
                ),
                "rechecked_percent": (
                    round(100 * task_rechecked / task_original, 1)
                    if scope_complete and task_original
                    else None
                ),
            }

        return {
            "ok": True,
            "ledger_revision": int(
                con.execute("SELECT ledger_revision FROM ledger WHERE singleton=1").fetchone()[0]
            ),
            "counts": counts,
            "denominators": {
                "verified_original_occurrences": original,
                "remediation_cases": remediation_denominator,
            },
            "scope": scope,
            "represented_resolved_percent": represented_resolved_percent,
            "represented_rechecked_percent": represented_rechecked_percent,
            "resolved_percent": represented_resolved_percent if scope_complete else None,
            "rechecked_percent": represented_rechecked_percent if scope_complete else None,
            "tasks": {name: task_view(task_counts[name]) for name in sorted(task_counts)},
            "reason": None if original else "the ledger has no occurrence population",
        }
    finally:
        if own:
            con.close()


def remediation_report(
    ledger: str | Path | sqlite3.Connection,
    *,
    limit: int = DEFAULT_CASE_LIMIT,
    offset: int = 0,
) -> dict[str, Any]:
    """Build one deterministic, bounded page of before/after evidence rows.

    The summary always covers the complete ledger population. Exact cases are
    deliberately paginated, so a million-row retained audit cannot turn a
    routine report read into a second full in-memory audit export.
    """
    summary = remediation_summary(ledger)
    cases = read_cases(ledger, limit=limit, offset=offset)
    rows = []
    for finding in cases["findings"]:
        for occurrence in finding["occurrences"]:
            decisions = occurrence["decisions"]
            latest = decisions[-1] if decisions else None
            rows.append(
                {
                    "finding_key": finding["finding_key"],
                    "check": finding["check"],
                    "subject": occurrence["subject_value"],
                    "occurrence_key": occurrence["occurrence_key"],
                    "baseline": occurrence["observations"][0]
                    if occurrence["observations"]
                    else None,
                    "state": occurrence["current_state"],
                    "decision": latest,
                    "observations": occurrence["observations"],
                }
            )
    return {
        "schema_version": "remediation-report.v1",
        "summary": summary,
        "pagination": {
            "total": cases["total"],
            "limit": cases["limit"],
            "offset": cases["offset"],
            "returned": len(cases["findings"]),
        },
        "cases": rows,
    }


def remediation_markdown(document: dict[str, Any]) -> str:
    """Render the retained remediation state without calculating or fetching evidence."""
    if document.get("schema_version") != "remediation-report.v1":
        raise LedgerError("unsupported remediation report document")
    summary = document.get("summary")
    cases = document.get("cases")
    pagination = document.get("pagination")
    if (
        not isinstance(summary, dict)
        or not isinstance(cases, list)
        or not isinstance(pagination, dict)
    ):
        raise LedgerError("invalid remediation report document")
    counts = summary.get("counts") or {}
    lines = [
        "# Remediation evidence report",
        "",
        f"Ledger revision: {summary.get('ledger_revision')}",
        "Cases: "
        f"{pagination.get('returned')} shown from {pagination.get('total')} "
        f"(limit={pagination.get('limit')}, offset={pagination.get('offset')})",
        "",
        "| State | Check | Subject | Evidence |",
        "|---|---|---|---|",
    ]
    for case in cases:
        if not isinstance(case, dict):
            raise LedgerError("invalid remediation report case")
        decision = case.get("decision") or {}
        verification = decision.get("verification") if isinstance(decision, dict) else None
        evidence = (
            "verification "
            f"{verification.get('artifact_sha256')} / {verification.get('outcome')} / "
            f"baseline {verification.get('baseline_audit_sha256')} / "
            f"scope {verification.get('scope_sha256')}"
            if isinstance(verification, dict)
            else (
                f"decision {decision.get('decision_id')} / observation {decision.get('observation_id')}"
                if decision
                else "no lifecycle decision"
            )
        )
        cells = [
            str(case.get("state") or ""),
            str(case.get("check") or ""),
            str(case.get("subject") or ""),
            evidence,
        ]
        lines.append(
            "| " + " | ".join(cell.replace("|", "\\|").replace("\n", " ") for cell in cells) + " |"
        )
    lines.extend(
        [
            "",
            "Counts: "
            + ", ".join(f"{state}={counts.get(state, 0)}" for state in sorted(_LIFECYCLE_STATES)),
            "",
            "Resolved percentage: "
            + (
                f"{summary['resolved_percent']}%"
                if summary.get("resolved_percent") is not None
                else "unavailable"
            ),
            "Represented resolved percentage: "
            + (
                f"{summary['represented_resolved_percent']}%"
                if summary.get("represented_resolved_percent") is not None
                else "unavailable"
            ),
            "Rechecked percentage: "
            + (
                f"{summary['rechecked_percent']}%"
                if summary.get("rechecked_percent") is not None
                else "unavailable"
            ),
            "Represented rechecked percentage: "
            + (
                f"{summary['represented_rechecked_percent']}%"
                if summary.get("represented_rechecked_percent") is not None
                else "unavailable"
            ),
            "Membership scope: " + str((summary.get("scope") or {}).get("state") or "unknown"),
            "",
        ]
    )
    tasks = summary.get("tasks") or {}
    if tasks:
        lines.extend(
            [
                "## Task coverage",
                "",
                "| Task | Status | Scope | Represented resolved | Full-scope resolved |",
                "|---|---|---|---:|---:|",
            ]
        )
        for task_id in sorted(tasks):
            task = tasks[task_id]
            lines.append(
                "| "
                + " | ".join(
                    (
                        task_id,
                        str(task.get("status") or "unavailable"),
                        str(task.get("scope_state") or "unknown"),
                        (
                            f"{task.get('represented_resolved_percent')}%"
                            if task.get("represented_resolved_percent") is not None
                            else "unavailable"
                        ),
                        (
                            f"{task.get('resolved_percent')}%"
                            if task.get("resolved_percent") is not None
                            else "unavailable"
                        ),
                    )
                )
                + " |"
            )
        lines.append("")
    return "\n".join(lines)


def write_remediation_report(
    ledger: str | Path | sqlite3.Connection,
    out_dir: str | Path,
    *,
    limit: int = DEFAULT_CASE_LIMIT,
    offset: int = 0,
) -> dict[str, Any]:
    """Write one new JSON/Markdown report directory from existing ledger evidence.

    The function never overwrites a report or mutates the ledger.  A caller can
    retain this directory alongside the ledger as an exact review snapshot.
    """
    destination = Path(out_dir).absolute()
    if destination.exists():
        raise FileExistsError(f"remediation report output already exists: {destination}")
    destination.mkdir(parents=True)
    document = remediation_report(ledger, limit=limit, offset=offset)

    def write_new(path: Path, content: str) -> None:
        fd, temporary = tempfile.mkstemp(prefix=".remediation-", dir=destination)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())
            os.link(temporary, path)
        finally:
            with contextlib.suppress(FileNotFoundError):
                os.unlink(temporary)

    json_path = destination / "remediation.json"
    markdown_path = destination / "remediation.md"
    write_new(json_path, json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    write_new(markdown_path, remediation_markdown(document))
    return {
        "ok": True,
        "report": str(markdown_path),
        "data": str(json_path),
        "summary": document["summary"],
    }


def ledger_summary(ledger: str | Path | sqlite3.Connection) -> dict[str, Any]:
    """Return ledger identity plus the independent finding/occurrence/URL counts."""
    own = not isinstance(ledger, sqlite3.Connection)
    con = open_ledger(ledger) if own else ledger
    try:
        counts = {}
        for table in (
            "site",
            "source_scan",
            "check_def",
            "finding",
            "occurrence",
            "affected_url",
            "finding_observation",
            "finding_group",
            "source_group",
            "source_group_member",
            "observation",
            "decision",
            "verification_artifact",
            "verification_result",
        ):
            counts[table] = con.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0]
        counts["distinct_affected_urls"] = con.execute(
            "SELECT COUNT(DISTINCT url) FROM affected_url"
        ).fetchone()[0]
        header = dict(con.execute("SELECT * FROM ledger WHERE singleton=1").fetchone())
        sites = [dict(row) for row in con.execute("SELECT * FROM site ORDER BY site_id")]
        return {
            "ok": True,
            "format_version": header["format_version"],
            "ledger_uuid": header["ledger_uuid"],
            "ledger_revision": header["ledger_revision"],
            "sites": sites,
            "counts": counts,
        }
    finally:
        if own:
            con.close()


def _observation_view(row: sqlite3.Row, previous: dict[str, Any] | None) -> dict[str, Any]:
    basis = {
        "config_fingerprint": row["config_fingerprint"],
        "producer_version": row["producer_version"],
        "producer_revision": row["producer_revision"],
    }
    changed = (
        None
        if previous is None
        else {name: basis[name] != previous["basis"][name] for name in basis}
    )
    return {
        "observation_id": row["observation_id"],
        "observation_revision": row["observation_revision"],
        "source_scan": {
            "source_scan_id": row["source_scan_id"],
            "scan_uuid": row["scan_uuid"],
            "format_version": row["format_version"],
            "evidence_revision": row["evidence_revision"],
            "audit_sha256": row["audit_sha256"],
            "artifact_state": row["artifact_state"],
            "crawl_partial": bool(row["crawl_partial"]),
            "corpus_partial": bool(row["corpus_partial"]),
        },
        "role": row["role"],
        "issue_ordinal": row["issue_ordinal"],
        "evidence_state": row["evidence_state"],
        "evidence_reason": row["evidence_reason"],
        "evidence": json.loads(row["evidence_json"]),
        "payload_sha256": row["payload_sha256"],
        "basis": basis,
        # Comparability is explicit data, not an inference: a changed basis
        # marks the observation as measured under different rules, never as
        # resolved or overwritten.
        "basis_changed": changed,
        "observed_at": row["observed_at"],
        "observed_at_state": row["observed_at_state"],
        "ingested_at": row["ingested_at"],
    }


def read_group_members(
    ledger: str | Path | sqlite3.Connection,
    *,
    source_scan_id: int,
    group_ref: str,
    offset: int = 0,
    limit: int = 100,
    max_bytes: int = 1024 * 1024,
) -> dict[str, Any]:
    """Read an explicit bounded page from the complete retained source group."""
    if (
        type(source_scan_id) is not int
        or source_scan_id < 1
        or not isinstance(group_ref, str)
        or not group_ref
    ):
        raise LedgerError("group members require a positive source_scan_id and group_ref")
    if (
        type(offset) is not int
        or offset < 0
        or type(limit) is not int
        or not 1 <= limit <= MAX_CASE_LIMIT
    ):
        raise LedgerError("group member offset or limit is invalid")
    if type(max_bytes) is not int or not 1 <= max_bytes <= MAX_PAYLOAD_BYTES:
        raise LedgerError("group member max_bytes must be between 1 and 8 MiB")
    own = not isinstance(ledger, sqlite3.Connection)
    con = open_ledger(ledger) if own else ledger
    try:
        group = con.execute(
            "SELECT * FROM source_group WHERE source_scan_id=? AND group_ref=?",
            (source_scan_id, group_ref),
        ).fetchone()
        if group is None:
            raise LedgerError("source group membership is unavailable")
        rows, used = [], 2
        for row in con.execute(
            "SELECT value_json FROM source_group_member WHERE source_scan_id=? AND group_ref=? AND ordinal>=? ORDER BY ordinal LIMIT ?",
            (source_scan_id, group_ref, offset, limit),
        ):
            size = len(row[0].encode("utf-8")) + int(bool(rows))
            if used + size > max_bytes:
                if not rows:
                    raise LedgerError("one group member exceeds the requested page byte bound")
                break
            rows.append(_loads(row[0], "group member"))
            used += size
        has_more = offset + len(rows) < group["member_count"]
        return {
            "source_scan_id": source_scan_id,
            "group_ref": group_ref,
            "rows": rows,
            "total": group["member_count"],
            "offset": offset,
            "limit": limit,
            "returned": len(rows),
            "has_more": has_more,
            "next_offset": offset + len(rows) if has_more else None,
            "bytes": used,
            "max_bytes": max_bytes,
            "members_sha256": group["members_sha256"],
        }
    finally:
        if own:
            con.close()


def read_cases(
    ledger: str | Path | sqlite3.Connection,
    *,
    check: str | None = None,
    url: str | None = None,
    finding_key: str | None = None,
    limit: int | None = DEFAULT_CASE_LIMIT,
    offset: int = 0,
) -> dict[str, Any]:
    """Read back exact cases and their per-occurrence history.

    Filters are exact: ``check`` is a registry check id, ``url`` is canonicalized
    with the audit's own URL policy before matching subjects or affected-URL
    membership, and ``finding_key`` selects one case directly.  The result is
    the ledger population itself -- findings, their occurrences, affected-URL
    membership, group memberships and ordered observations.  Results are
    paginated by finding to keep an observer responsive; pass ``limit=None``
    only for a bounded local export such as :func:`remediation_report`.
    """
    if limit is not None and (type(limit) is not int or not 1 <= limit <= MAX_CASE_LIMIT):
        raise LedgerError(f"limit must be an integer from 1 to {MAX_CASE_LIMIT} or None")
    if type(offset) is not int or not 0 <= offset <= 1_000_000:
        raise LedgerError("offset must be a nonnegative bounded integer")
    own = not isinstance(ledger, sqlite3.Connection)
    con = open_ledger(ledger) if own else ledger
    try:
        clauses, values = [], []
        if check is not None:
            clauses.append("c.check_key=?")
            values.append(check)
        if finding_key is not None:
            clauses.append("f.finding_key=?")
            values.append(finding_key)
        if url is not None:
            canon = canonical_url(url)
            clauses.append(
                "(f.subject_value=? OR EXISTS (SELECT 1 FROM affected_url a "
                "WHERE a.finding_id=f.finding_id AND a.url=?))"
            )
            values.extend([canon, canon])
        where = ("WHERE " + " AND ".join(clauses)) if clauses else ""
        findings = []
        total = con.execute(
            "SELECT COUNT(*) FROM finding f JOIN check_def c ON c.check_id=f.check_id " + where,
            values,
        ).fetchone()[0]
        page = ""
        page_values = list(values)
        if limit is not None:
            page = " LIMIT ? OFFSET ?"
            page_values.extend([limit, offset])
        finding_rows = con.execute(
            "SELECT f.*,c.check_key FROM finding f JOIN check_def c ON c.check_id=f.check_id "
            + where
            + " ORDER BY c.check_key,f.subject_value"
            + page,
            page_values,
        ).fetchall()
        for finding in finding_rows:
            members = [
                {"url": row["url"], "role": row["role"], "first_seen_at": row["first_seen_at"]}
                for row in con.execute(
                    "SELECT * FROM affected_url WHERE finding_id=? ORDER BY url,role",
                    (finding["finding_id"],),
                )
            ]
            projections = [
                {
                    "source_scan_id": row["source_scan_id"],
                    "issue_ordinal": row["issue_ordinal"],
                    "audit_fingerprint": row["audit_fingerprint"],
                    "severity": row["severity"],
                    "status_code": row["status_code"],
                    "message": row["message"],
                    "occurrences_count": row["occurrences_count"],
                    "enumerated_count": row["enumerated_count"],
                    "coverage_state": row["coverage_state"],
                    "payload_sha256": row["payload_sha256"],
                }
                for row in con.execute(
                    "SELECT * FROM finding_observation WHERE finding_id=? ORDER BY source_scan_id",
                    (finding["finding_id"],),
                )
            ]
            memberships = []
            for row in con.execute(
                "SELECT * FROM finding_group WHERE finding_id=? ORDER BY source_scan_id",
                (finding["finding_id"],),
            ):
                group_page = None
                if con.execute(
                    "SELECT 1 FROM source_group WHERE source_scan_id=? AND group_ref=?",
                    (row["source_scan_id"], row["group_ref"]),
                ).fetchone():
                    group_page = read_group_members(
                        con,
                        source_scan_id=row["source_scan_id"],
                        group_ref=row["group_ref"],
                        limit=25,
                    )
                memberships.append(
                    {
                        "source_scan_id": row["source_scan_id"],
                        "group_ref": row["group_ref"],
                        "group_check": row["group_check"],
                        "group_value": row["group_value"],
                        "group_count": row["group_count"],
                        "group_urls": group_page["rows"] if group_page else [],
                        "group_members_page": {
                            key: value for key, value in group_page.items() if key != "rows"
                        }
                        if group_page
                        else {"state": "unavailable"},
                    }
                )
            occurrences = []
            for occurrence in con.execute(
                "SELECT * FROM occurrence WHERE finding_id=? "
                "ORDER BY subject_value,representation,discriminator_value",
                (finding["finding_id"],),
            ):
                observations = []
                previous = None
                for row in con.execute(
                    "SELECT o.*,s.scan_uuid,s.format_version,s.evidence_revision,"
                    "s.audit_sha256,s.artifact_state,s.crawl_partial,s.corpus_partial "
                    "FROM observation o JOIN source_scan s ON s.source_scan_id=o.source_scan_id "
                    "WHERE o.occurrence_id=? ORDER BY o.observation_revision",
                    (occurrence["occurrence_id"],),
                ):
                    view = _observation_view(row, previous)
                    previous = view
                    observations.append(view)
                decisions = []
                for row in con.execute(
                    "SELECT d.*,vr.outcome AS verification_outcome,vr.measured AS verification_measured,"
                    "vr.before_sha256,vr.after_sha256,vr.scope_sha256,va.artifact_sha256,"
                    "va.artifact_path,va.task_id,va.baseline_audit_sha256,va.selection_sha256,"
                    "va.collection_sha256,va.collection_state "
                    "FROM decision d "
                    "LEFT JOIN verification_result vr ON vr.verification_result_id=d.verification_result_id "
                    "LEFT JOIN verification_artifact va ON va.verification_id=vr.verification_id "
                    "WHERE d.occurrence_id=? ORDER BY d.decision_id",
                    (occurrence["occurrence_id"],),
                ):
                    verification = None
                    if row["verification_result_id"] is not None:
                        verification = {
                            "artifact_sha256": row["artifact_sha256"],
                            "artifact_path": row["artifact_path"],
                            "task_id": row["task_id"],
                            "baseline_audit_sha256": row["baseline_audit_sha256"],
                            "selection_sha256": row["selection_sha256"],
                            "collection_sha256": row["collection_sha256"],
                            "collection_state": row["collection_state"],
                            "outcome": row["verification_outcome"],
                            "measured": bool(row["verification_measured"]),
                            "before_sha256": row["before_sha256"],
                            "after_sha256": row["after_sha256"],
                            "scope_sha256": row["scope_sha256"],
                        }
                    decisions.append(
                        {
                            "decision_id": row["decision_id"],
                            "state": row["state"],
                            "actor": row["actor"],
                            "reason": row["reason"],
                            "decided_at": row["decided_at"],
                            "decided_at_state": row["decided_at_state"],
                            "observation_id": row["observation_id"],
                            "ledger_revision": row["ledger_revision"],
                            "verification": verification,
                        }
                    )
                occurrences.append(
                    {
                        "occurrence_id": occurrence["occurrence_id"],
                        "occurrence_key": occurrence["occurrence_key"],
                        "subject_type": occurrence["subject_type"],
                        "subject_value": occurrence["subject_value"],
                        "representation": occurrence["representation"],
                        "discriminator_type": occurrence["discriminator_type"],
                        "discriminator_value": occurrence["discriminator_value"],
                        "current_state": occurrence["current_state"],
                        "first_seen_at": occurrence["first_seen_at"],
                        "observations": observations,
                        "decisions": decisions,
                    }
                )
            findings.append(
                {
                    "finding_id": finding["finding_id"],
                    "finding_key": finding["finding_key"],
                    "check": finding["check_key"],
                    "subject_type": finding["subject_type"],
                    "subject_value": finding["subject_value"],
                    "current_state": finding["current_state"],
                    "first_seen_at": finding["first_seen_at"],
                    "affected_urls": members,
                    "projections": projections,
                    "group_memberships": memberships,
                    "occurrences": occurrences,
                }
            )
        revision = con.execute("SELECT ledger_revision FROM ledger WHERE singleton=1").fetchone()[0]
        return {
            "ok": True,
            "ledger_revision": int(revision),
            "total": int(total),
            "limit": limit,
            "offset": offset,
            "findings": findings,
        }
    finally:
        if own:
            con.close()
