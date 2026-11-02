"""One-shot nfprogress legacy migration helper and Bridge Release entrypoint.

The helper is deliberately separate from the normal Tauri runtime.  It reads
legacy stores through :mod:`nfprogress.core.legacy_decoder`, emits the same
MigrationBundle v1 contract used by existing importers, validates that bundle,
builds a staging SQLite profile, and activates only after integrity and
semantic checks pass.

The supported production bridge invokes this module from the already-packaged
Python nfprogress runtime.  A standalone native helper is a later artifact;
this module never becomes a Tauri sidecar or a startup dependency.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sqlite3
import sys
import tempfile
import traceback
import uuid
from contextlib import closing
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping

import engine

from nfprogress.core.legacy_decoder import LegacyDecodeError, load_legacy_pickle
from nfprogress.core.migration import (
    MigrationBundle,
    bundle_checksum,
    import_projects_bundle,
    verify_projects_bundle,
)
from nfprogress.core.game_state import _game_payload
from nfprogress.core.recovery import (
    RecoveryError,
    _atomic_copy,
    create_application_backup,
    sha256_file,
    source_fingerprint,
    validate_migration_bundle,
    validate_sqlite_file,
)
from nfprogress.core.sqlite.connection import open_database, register_remote_apply_authorization_guard
from nfprogress.core.sqlite.schema import CURRENT_SCHEMA_VERSION
from nfprogress.core.sqlite.ordering import (
    OrderInvariantError,
    OrderTableProposal,
    ProjectOrderProposal,
    apply_order_recovery,
    propose_project_order_recovery,
    propose_progress_order_recovery,
    propose_stage_order_recovery,
    validate_progress_order,
    validate_project_order,
    validate_stage_order,
)
from nfprogress.core.serialization.projections import to_json_safe
from nfprogress.core.sqlite.notes import canonical_notes_from_projects


HELPER_VERSION = "f9.1"
LATEST_BUNDLE_VERSION = 1
EMPTY_GAME_PAYLOAD = {
    "dto_version": 1,
    "state_schema_version": 2,
    "gamer": {},
    "notifications": {"new": [], "read": []},
    "global_streak": {},
    "project_game_state": {},
    "extensions": {},
}

OUTCOME_READY = "ready"
OUTCOME_MIGRATION_VERIFIED = "migration_verified"
OUTCOME_MIGRATION_REQUIRED = "migration_required"
OUTCOME_UNSUPPORTED_SOURCE = "unsupported_source"
OUTCOME_SOURCE_CORRUPT = "source_corrupt"
OUTCOME_BUNDLE_INVALID = "bundle_invalid"
OUTCOME_DESTINATION_INVALID = "destination_invalid"
OUTCOME_MIGRATION_FAILED = "migration_failed"
SQLITE_PROJECT_ORDER_RECOVERY_PROFILE = "K_sqlite_project_order_recovery"

EXIT_CODES = {
    OUTCOME_READY: 0,
    OUTCOME_MIGRATION_VERIFIED: 0,
    OUTCOME_MIGRATION_REQUIRED: 10,
    OUTCOME_UNSUPPORTED_SOURCE: 11,
    OUTCOME_SOURCE_CORRUPT: 12,
    OUTCOME_BUNDLE_INVALID: 13,
    OUTCOME_DESTINATION_INVALID: 14,
    OUTCOME_MIGRATION_FAILED: 15,
}


class HelperError(RuntimeError):
    def __init__(self, code: str, message: str, *, details: list[str] | None = None) -> None:
        super().__init__(message)
        self.code = code
        self.details = details or []


@dataclass(slots=True)
class SourceInspection:
    source_root: str
    source_profile: str
    source_version: str
    files_present: list[str]
    sqlite_schema: dict[str, Any]
    ownership: dict[str, str]
    migration_markers: dict[str, Any]
    fingerprint: str
    warnings: list[str] = field(default_factory=list)
    supported: bool = False

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(slots=True)
class MigrationPreview:
    projects: int = 0
    stages: int = 0
    progress_entries: int = 0
    notes: int = 0
    game: bool = False
    maps: int = 0
    documents: int = 0
    external_bindings: int = 0
    warnings: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(slots=True)
class MigrationReport:
    outcome: str
    source_profile: str
    source_fingerprint: str
    preview: MigrationPreview
    backup: str | None = None
    bundle: str | None = None
    staging: str | None = None
    details: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        value = asdict(self)
        value["preview"] = self.preview.to_dict()
        return value


@dataclass(slots=True)
class ProjectOrderRecoveryPreview:
    source_root: str
    source_sha256: str
    schema_version: int
    project_ids: list[str]
    existing_order: list[str]
    existing_positions: list[int]
    proposed_order: list[str]
    order_source: str
    existing_stage_order: dict[str, list[str]]
    proposed_stage_order: dict[str, list[str]]
    existing_progress_order: list[str]
    proposed_progress_order: list[str]
    issues: list[str] = field(default_factory=list)

    @property
    def requires_recovery(self) -> bool:
        return (
            bool(self.issues)
            or self.existing_order != self.proposed_order
            or self.existing_positions != list(range(len(self.existing_positions)))
            or self.existing_stage_order != self.proposed_stage_order
            or self.existing_progress_order != self.proposed_progress_order
        )

    def to_dict(self) -> dict[str, Any]:
        return asdict(self) | {"requires_recovery": self.requires_recovery}


@dataclass(slots=True)
class ProjectOrderRecoveryReport:
    outcome: str
    preview: ProjectOrderRecoveryPreview
    backup: str | None = None
    rollback: str | None = None
    source_sha256: str | None = None
    backup_sha256: str | None = None
    details: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "outcome": self.outcome,
            "preview": self.preview.to_dict(),
            "backup": self.backup,
            "rollback": self.rollback,
            "source_sha256": self.source_sha256,
            "backup_sha256": self.backup_sha256,
            "details": self.details,
        }


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def _known_files(root: Path) -> list[str]:
    return sorted(
        name for name in (
            "nfprogress.db", "data.pkl", "settings.pkl", "gamer.pkl",
            "documents.json", "external_file_manifest.json",
        ) if (root / name).is_file()
    )


def _read_sqlite_metadata(path: Path) -> tuple[dict[str, Any], dict[str, str], dict[str, Any], list[str]]:
    schema: dict[str, Any] = {"version": None, "tables": []}
    ownership: dict[str, str] = {}
    markers: dict[str, Any] = {}
    warnings: list[str] = []
    if not path.is_file():
        return schema, ownership, markers, warnings
    connection: sqlite3.Connection | None = None
    try:
        connection = sqlite3.connect(f"file:{path.resolve().as_posix()}?mode=ro", uri=True)
        connection.row_factory = sqlite3.Row
        schema_rows = connection.execute(
            "SELECT schema_version FROM schema_info"
        ).fetchall()
        if len(schema_rows) != 1:
            warnings.append("schema marker is missing or not singular")
        else:
            schema["version"] = int(schema_rows[0][0])
        schema["tables"] = sorted(row[0] for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type='table'"
        ))
        if "storage_ownership" in schema["tables"]:
            rows = connection.execute(
                "SELECT subsystem, owner FROM storage_ownership"
            ).fetchall()
            ownership = {str(row[0]): str(row[1]) for row in rows}
            if len(rows) != 4 or set(ownership) != {"projects", "settings", "notes", "game"}:
                warnings.append("ownership markers are incomplete")
        for table, key in (
            ("game_metadata", "migration_status"),
            ("document_metadata", "documents_json_migration"),
            ("game_metadata", "source_manifest"),
        ):
            if table in schema["tables"]:
                row = connection.execute(
                    f"SELECT value_json FROM {table} WHERE key=?", (key,)
                ).fetchone()
                if row:
                    try:
                        markers[key] = json.loads(row[0])
                    except (TypeError, json.JSONDecodeError):
                        markers[key] = row[0]
    except (sqlite3.DatabaseError, OSError, ValueError) as error:
        warnings.append(f"SQLite metadata unavailable: {type(error).__name__}")
    finally:
        if connection is not None:
            connection.close()
    return schema, ownership, markers, warnings


def _legacy_project_order_hint(
    root: Path, project_ids: set[str]
) -> tuple[list[str] | None, str]:
    """Read an explicit legacy order without mutating the source profile."""
    legacy_path = root / "data.pkl"
    if legacy_path.is_file():
        try:
            envelope = load_legacy_pickle(legacy_path)
        except (LegacyDecodeError, OSError, ValueError):
            envelope = None
        if isinstance(envelope, Mapping):
            raw_order = envelope.get("project_order")
            if isinstance(raw_order, list):
                explicit = [
                    project_id for project_id in raw_order
                    if isinstance(project_id, str) and project_id in project_ids
                ]
                if explicit:
                    return list(dict.fromkeys(explicit)), "legacy data.pkl project_order"
            project_map = envelope.get("projects")
            if isinstance(project_map, Mapping):
                insertion_order = [
                    getattr(project, "project_id", None)
                    for project in project_map.values()
                ]
                insertion_order = [
                    project_id for project_id in insertion_order
                    if isinstance(project_id, str) and project_id in project_ids
                ]
                if insertion_order:
                    return list(dict.fromkeys(insertion_order)), "legacy data.pkl mapping order"

    bundle_path = root / "nfprogress-migration.json"
    if bundle_path.is_file() and bundle_path.stat().st_size <= 100 * 1024 * 1024:
        try:
            bundle = json.loads(bundle_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError):
            bundle = None
        if isinstance(bundle, Mapping):
            raw_order = bundle.get("project_order")
            if isinstance(raw_order, list):
                explicit = [
                    project_id for project_id in raw_order
                    if isinstance(project_id, str) and project_id in project_ids
                ]
                if explicit:
                    return list(dict.fromkeys(explicit)), "MigrationBundle project_order"
    return None, "SQLite created_at/updated_at/project ID fallback"


def analyze_project_order_recovery(source_root: str | Path) -> ProjectOrderRecoveryPreview:
    """Analyze a v6 database without opening it through the mutating runtime."""
    root = Path(source_root).expanduser().resolve()
    database = root / "nfprogress.db"
    if not database.is_file():
        raise RecoveryError("missing_sqlite")
    if (root / "nfprogress.db-wal").exists() or (root / "nfprogress.db-shm").exists():
        raise RecoveryError("SQLite must be closed before order recovery")
    connection: sqlite3.Connection | None = None
    try:
        connection = sqlite3.connect(f"file:{database.as_posix()}?mode=ro", uri=True)
        connection.row_factory = sqlite3.Row
        if connection.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
            raise RecoveryError("corrupt_sqlite: integrity_check failed")
        if connection.execute("PRAGMA foreign_key_check").fetchall():
            raise RecoveryError("corrupt_sqlite: foreign_key_check failed")
        version = int(connection.execute("SELECT schema_version FROM schema_info").fetchone()[0])
        if version not in (6, 7, CURRENT_SCHEMA_VERSION):
            raise RecoveryError(f"unsupported_sqlite_schema: {version}")
        project_ids = {
            row[0] for row in connection.execute("SELECT id FROM projects")
        }
        preferred_order, order_source = _legacy_project_order_hint(root, project_ids)
        proposal = propose_project_order_recovery(connection, preferred_order)
        stage_proposal = propose_stage_order_recovery(connection)
        progress_proposal = propose_progress_order_recovery(connection)
        positions = [
            row[1] for row in connection.execute(
                "SELECT project_id, position FROM project_order ORDER BY position, project_id"
            )
        ]
        issues: list[str] = []
        try:
            validate_project_order(connection)
        except OrderInvariantError as error:
            issues.append(str(error))
        try:
            validate_stage_order(connection)
        except OrderInvariantError as error:
            issues.append(str(error))
        try:
            validate_progress_order(connection)
        except OrderInvariantError as error:
            issues.append(str(error))
        return ProjectOrderRecoveryPreview(
            source_root=str(root),
            source_sha256=sha256_file(database),
            schema_version=version,
            project_ids=list(proposal.project_ids),
            existing_order=list(proposal.existing_order),
            existing_positions=positions,
            proposed_order=list(proposal.proposed_order),
            order_source=order_source,
            existing_stage_order={
                project_id: list(stage_ids)
                for project_id, stage_ids in stage_proposal.existing_order.items()
            },
            proposed_stage_order={
                project_id: list(stage_ids)
                for project_id, stage_ids in stage_proposal.proposed_order.items()
            },
            existing_progress_order=list(progress_proposal.existing_order),
            proposed_progress_order=list(progress_proposal.proposed_order),
            issues=issues,
        )
    except RecoveryError:
        raise
    except (sqlite3.DatabaseError, OSError, ValueError, TypeError, IndexError) as error:
        raise RecoveryError("corrupt_sqlite") from error
    finally:
        if connection is not None:
            connection.close()


def _project_rows(path: Path) -> list[tuple[Any, ...]]:
    connection = sqlite3.connect(f"file:{path.resolve().as_posix()}?mode=ro", uri=True)
    try:
        return [
            tuple(row) for row in connection.execute(
                "SELECT id, name, goal, infinite, unit, status, created_at, updated_at, payload_json "
                "FROM projects ORDER BY id"
            )
        ]
    finally:
        connection.close()


def recover_project_order(data_root: str | Path) -> ProjectOrderRecoveryReport:
    """Explicitly repair project order through backup, staging, and activation."""
    root = Path(data_root).expanduser().resolve()
    preview = analyze_project_order_recovery(root)
    if not preview.requires_recovery:
        return ProjectOrderRecoveryReport(
            OUTCOME_READY,
            preview,
            source_sha256=preview.source_sha256,
            details=["project order is already complete; no mutation performed"],
        )

    database = root / "nfprogress.db"
    before_projects = _project_rows(database)
    backup = create_application_backup(root, {"nfprogress.db"})
    backup_sha256 = sha256_file(backup / "nfprogress.db")
    if backup_sha256 != preview.source_sha256:
        raise RecoveryError("backup checksum does not match source database")
    staging: Path | None = Path(tempfile.mkdtemp(
        prefix=f".{root.name}-order-recovery-", dir=root.parent
    ))
    try:
        _atomic_copy(database, staging / "nfprogress.db")
        project_proposal = ProjectOrderProposal(
            tuple(preview.project_ids),
            tuple(preview.existing_order),
            tuple(preview.proposed_order),
        )
        stage_proposal = OrderTableProposal(
            {
                project_id: tuple(stage_ids)
                for project_id, stage_ids in preview.existing_stage_order.items()
            },
            {
                project_id: tuple(stage_ids)
                for project_id, stage_ids in preview.proposed_stage_order.items()
            },
        )
        progress_proposal = OrderTableProposal(
            tuple(preview.existing_progress_order),
            tuple(preview.proposed_progress_order),
        )
        connection = sqlite3.connect(staging / "nfprogress.db")
        connection.row_factory = sqlite3.Row
        register_remote_apply_authorization_guard(connection)
        try:
            connection.execute("PRAGMA foreign_keys = ON")
            with connection:
                apply_order_recovery(
                    connection, project_proposal, stage_proposal, progress_proposal
                )
                validate_project_order(connection)
        finally:
            connection.close()

        if _project_rows(staging / "nfprogress.db") != before_projects:
            raise RecoveryError("project rows changed during order recovery")
        validate_sqlite_file(
            staging / "nfprogress.db", allow_versions={6, 7, CURRENT_SCHEMA_VERSION},
        )
        if sha256_file(database) != preview.source_sha256:
            raise RecoveryError("source_changed: database checksum changed during recovery")
        rollback = _activate(staging, root)
        shutil.rmtree(staging, ignore_errors=True)
        staging = None
        return ProjectOrderRecoveryReport(
            OUTCOME_MIGRATION_VERIFIED,
            preview,
            backup=str(backup),
            rollback=str(rollback) if rollback else None,
            source_sha256=preview.source_sha256,
            backup_sha256=backup_sha256,
            details=[
                "staging integrity_check=ok",
                "staging foreign_key_check=ok",
                "staging semantic order verification=ok",
                "activation=atomic",
            ],
        )
    finally:
        if staging is not None:
            shutil.rmtree(staging, ignore_errors=True)


def _profile_for(
    files: list[str], schema: dict[str, Any], ownership: dict[str, str], warnings: list[str]
) -> tuple[str, bool, str, list[str]]:
    has_data = "data.pkl" in files
    has_game = "gamer.pkl" in files
    has_docs = "documents.json" in files
    version = schema.get("version")
    if not files:
        return "fresh_install", True, "none", warnings
    if "nfprogress.db" in files and version is None:
        return "J_corrupt_or_ambiguous", False, "unknown", warnings
    if has_data and version is None:
        if has_game and has_docs:
            return "D_pkl_game_documents", True, "legacy", warnings
        if has_game:
            return "B_pkl_game", True, "legacy", warnings
        if has_docs:
            return "C_pkl_documents", True, "legacy", warnings
        return "A_pkl_only", True, "legacy", warnings
    if version == 3:
        supported = has_data
        if not supported:
            warnings.append("v3 database without PKL cannot prove omitted envelope fields")
        return "E_sqlite_v3_pkl_authority", supported, "3", warnings
    if version == 4:
        supported = has_data
        if not supported:
            warnings.append("v4 database without PKL cannot prove complete Game/Documents state")
        return "F_sqlite_v4_mixed", supported, "4", warnings
    if version == 5:
        supported = has_data
        warnings.append("v5 Game/Documents migration is incomplete until the full bundle is imported")
        return "G_sqlite_v5_game_incomplete", supported, "5", warnings
    if version in (6, 7, CURRENT_SCHEMA_VERSION):
        # A v6 profile is prepared only when all authority and completion
        # markers are verified by verify_prepared_profile().
        if ownership == {name: "sqlite" for name in ("projects", "settings", "notes", "game")} and not has_data:
            return "I_prepared_v6", True, str(version), warnings
        if has_data:
            return "H_sqlite_v6_ownership_incomplete", True, str(version), warnings
        return "J_corrupt_or_ambiguous", False, str(version), warnings
    return "J_corrupt_or_ambiguous", False, str(version or "unknown"), warnings


def inspect_source(source_root: str | Path) -> SourceInspection:
    """Perform read-only source detection; no database or legacy file is opened for writing."""
    root = Path(source_root).expanduser().resolve()
    if not root.is_dir():
        raise HelperError(OUTCOME_SOURCE_CORRUPT, "source directory does not exist")
    files = _known_files(root)
    schema, ownership, markers, warnings = _read_sqlite_metadata(root / "nfprogress.db")
    profile, supported, version, warnings = _profile_for(files, schema, ownership, warnings)
    if schema.get("version") in range(1, CURRENT_SCHEMA_VERSION + 1):
        try:
            validate_sqlite_file(root / "nfprogress.db")
        except RecoveryError as error:
            warnings.append(str(error))
            if (
                schema.get("version") in (6, 7, CURRENT_SCHEMA_VERSION)
                and "project ordering" in str(error)
            ):
                profile = SQLITE_PROJECT_ORDER_RECOVERY_PROFILE
                supported = False
                warnings.append("explicit project-order recovery is available")
                return SourceInspection(
                    source_root=str(root), source_profile=profile,
                    source_version=version, files_present=files,
                    sqlite_schema=schema, ownership=ownership,
                    migration_markers=markers, fingerprint=source_fingerprint(root),
                    warnings=warnings, supported=supported,
                )
            # A present-but-partial SQLite profile is ambiguous even when a
            # PKL happens to be readable.  The helper must not guess which
            # source owns a domain.
            profile, supported = "J_corrupt_or_ambiguous", False
    migration_marker = markers.get("migration_status")
    documents_marker = markers.get("documents_json_migration")
    markers_complete = (
        isinstance(migration_marker, Mapping)
        and migration_marker.get("status") == "ready_for_tauri"
        and isinstance(documents_marker, Mapping)
        and documents_marker.get("status") == "complete"
    )
    sqlite_owners = ownership == {name: "sqlite" for name in ("projects", "settings", "notes", "game")}
    if schema.get("version") in (6, 7, CURRENT_SCHEMA_VERSION) and sqlite_owners and markers_complete:
        # Recovery copies may remain beside a prepared database.  The marker,
        # not the mere presence of stale PKL/JSON, proves SQLite authority.
        profile, supported = "I_prepared_v6", True
    elif profile == "I_prepared_v6" and not markers_complete:
        profile, supported = "H_sqlite_v6_ownership_incomplete", False
        warnings.append("prepared marker is not complete")
    elif profile == "H_sqlite_v6_ownership_incomplete" and sqlite_owners and not markers_complete:
        # Legacy files alongside fully SQLite-owned data are ambiguous until
        # the explicit completion markers prove which copy is authoritative.
        supported = False
        warnings.append("SQLite ownership and legacy files are ambiguous without completion markers")
    return SourceInspection(
        source_root=str(root), source_profile=profile, source_version=version,
        files_present=files, sqlite_schema=schema, ownership=ownership,
        migration_markers=markers, fingerprint=source_fingerprint(root),
        warnings=warnings, supported=supported,
    )


def _repair_legacy_entity(entity: Any, *, project_id: str, stage_index: int | None = None) -> None:
    """Repair only missing identifiers/default containers in memory."""
    defaults = {
        "_name": "Без имени", "_goal": None, "create_date": None,
        "edit_date": None, "complete_date": None, "_total_symbols": 0,
        "_progress": 0, "_deadline": "Нет", "_status": "активен",
        "notes": [], "streaks": [], "max_streak": 0, "streak_status": "No",
        "unit": "symbols", "synch": None, "last_synch": None,
        "work_method": "manual", "personal_goal_for_the_day": 0,
        "auto_freeze": True, "project_plan": {}, "enable_stages": False,
        "stages": [], "is_stage": stage_index is not None, "mindmap_data": None,
        "mindmap_updated_at": None, "combine_stage_mindmaps": False,
        "project_notes": [], "notes_updated_at": None, "cover_image": None,
        "folder_id": None,
    }
    for key, value in defaults.items():
        if not hasattr(entity, key):
            setattr(entity, key, value)
    if stage_index is None:
        if not isinstance(getattr(entity, "project_id", None), str) or not entity.project_id:
            entity.project_id = uuid.uuid5(uuid.NAMESPACE_URL, f"nfprogress-project:{project_id}").hex
    elif not isinstance(getattr(entity, "stage_id", None), str) or not entity.stage_id:
        entity.stage_id = uuid.uuid5(
            uuid.NAMESPACE_URL,
            f"nfprogress-stage:{project_id}:{stage_index}:{getattr(entity, 'name', '')}:{getattr(entity, 'create_date', None)}",
        ).hex
    if not isinstance(entity.notes, list):
        entity.notes = []
    for index, note in enumerate(entity.notes):
        if not isinstance(getattr(note, "entry_id", None), str) or not note.entry_id:
            note.entry_id = uuid.uuid5(
                uuid.NAMESPACE_URL,
                f"nfprogress-progress:{project_id}:{stage_index or 'project'}:{index}:{getattr(note, 'date_create', None)}:{getattr(note, 'new_total', None)}:{getattr(note, 'added_symbols', None)}",
            ).hex


def _load_projects(root: Path) -> tuple[Mapping[str, Any], MigrationBundle]:
    source = root / "data.pkl"
    if not source.is_file():
        raise HelperError(OUTCOME_UNSUPPORTED_SOURCE, "data.pkl is required for this source profile")
    try:
        envelope = load_legacy_pickle(source)
    except LegacyDecodeError as error:
        raise HelperError(getattr(error, "code", OUTCOME_SOURCE_CORRUPT), str(error)) from error
    if not isinstance(envelope, Mapping) or not isinstance(envelope.get("projects", {}), Mapping):
        raise HelperError(OUTCOME_SOURCE_CORRUPT, "legacy data.pkl has no projects mapping")
    projects = envelope["projects"]
    for index, (stored_name, project) in enumerate(projects.items()):
        _repair_legacy_entity(project, project_id=str(stored_name or index))
        for stage_index, stage in enumerate(getattr(project, "stages", [])):
            _repair_legacy_entity(stage, project_id=project.project_id, stage_index=stage_index)
    try:
        with engine.data_directory_context(root):
            bundle = MigrationBundle.from_legacy(envelope)
    except (TypeError, ValueError, AttributeError, KeyError) as error:
        raise HelperError(OUTCOME_SOURCE_CORRUPT, f"legacy project conversion failed: {error}") from error
    return envelope, bundle


def _source_manifest(root: Path, inspection: SourceInspection) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    for name in inspection.files_present:
        path = root / name
        result[name] = {
            "source_format": "sqlite" if name == "nfprogress.db" else "legacy-json" if name.endswith(".json") else "pickle",
            "source_schema_version": inspection.source_version,
            "checksum": f"sha256:{sha256_file(path)}",
            "size_bytes": path.stat().st_size,
        }
    return result


def _read_sqlite_section(root: Path, query: str, column: str) -> dict[str, Any]:
    connection = sqlite3.connect(f"file:{(root / 'nfprogress.db').resolve().as_posix()}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        return {
            str(row[0]): json.loads(row[column])
            for row in connection.execute(query)
        }
    finally:
        connection.close()


def _load_documents(root: Path, project_ids: set[str], stage_ids: set[str], warnings: list[str]) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    path = root / "documents.json"
    if not path.is_file():
        return [], [], []
    if path.stat().st_size > 100 * 1024 * 1024:
        raise HelperError(OUTCOME_SOURCE_CORRUPT, "documents.json exceeds the 100 MiB limit")
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise HelperError(OUTCOME_SOURCE_CORRUPT, "documents.json is invalid") from error
    if not isinstance(raw, Mapping):
        raise HelperError(OUTCOME_SOURCE_CORRUPT, "documents.json must contain an object")
    known = {
        "document_id", "id", "project_id", "stage_id", "title", "content",
        "content_format", "created_at", "updated_at", "exists", "docx_path",
        "sync_state", "last_synced_hash", "last_synced_at", "local_dirty",
        "word_dirty", "symbols", "has_content",
    }
    documents: list[dict[str, Any]] = []
    bindings: list[dict[str, Any]] = []
    external: list[dict[str, Any]] = []
    for source_key, item in raw.items():
        if not isinstance(item, Mapping):
            raise HelperError(OUTCOME_SOURCE_CORRUPT, f"documents.json record {source_key!r} is not an object")
        project_id = item.get("project_id")
        if not isinstance(project_id, str) or not project_id:
            suffix = str(source_key).removesuffix(":project")
            project_id = suffix if suffix in project_ids else None
        stage_id = item.get("stage_id")
        if project_id not in project_ids or (stage_id is not None and stage_id not in stage_ids):
            raise HelperError(OUTCOME_SOURCE_CORRUPT, f"document {source_key!r} has a broken project/stage relation")
        content = item.get("content", {"type": "doc", "content": [{"type": "paragraph"}]})
        if not isinstance(content, Mapping) or content.get("type") != "doc":
            raise HelperError(OUTCOME_SOURCE_CORRUPT, f"document {source_key!r} has invalid Tiptap content")
        scope = f"{project_id}:{stage_id or 'project'}"
        document_id = item.get("document_id") or item.get("id")
        if not isinstance(document_id, str) or not document_id:
            document_id = f"document-{hashlib.sha256(scope.encode()).hexdigest()[:40]}"
        record = dict(item)
        record.update({"document_id": document_id, "project_id": project_id, "stage_id": stage_id, "content": dict(content)})
        record["extensions"] = {str(key): item[key] for key in item if key not in known}
        documents.append(record)
        external_path = item.get("docx_path")
        if isinstance(external_path, str) and external_path:
            exists = Path(external_path).expanduser().is_file()
            sync_state = item.get("sync_state", "unlinked")
            if not exists:
                sync_state = "missing_external"
                warnings.append(f"missing external file: {external_path}")
            bindings.append({
                "id": uuid.uuid5(uuid.NAMESPACE_URL, f"nfprogress-document-binding:{document_id}").hex,
                "document_id": document_id, "project_id": project_id, "stage_id": stage_id,
                "binding_type": "word", "external_path": external_path, "source_id": None,
                "last_external_hash": item.get("last_synced_hash"),
                "last_synced_revision": 0, "last_synced_hash": item.get("last_synced_hash"),
                "last_synced_at": item.get("last_synced_at"), "sync_state": sync_state,
                "payload": {"legacy_source_key": str(source_key)},
            })
            external.append({
                "document_id": document_id, "external_path": external_path,
                "content_hash": item.get("last_synced_hash"), "exists": exists,
            })
    return documents, bindings, external


def build_bundle(inspection: SourceInspection, root: Path) -> tuple[MigrationBundle, MigrationPreview]:
    """Decode the snapshot and build the complete canonical v1 bundle."""
    warnings = list(inspection.warnings)
    envelope: Mapping[str, Any] = {}
    if "data.pkl" in inspection.files_present:
        envelope, bundle = _load_projects(root)
    else:
        bundle = MigrationBundle()
    project_ids = {item["id"] for item in bundle.projects}
    stage_ids = {stage["id"] for project in bundle.projects for stage in project.get("stages", [])}

    owners = inspection.ownership
    if "settings.pkl" in inspection.files_present and owners.get("settings", "pickle") != "sqlite":
        try:
            settings = load_legacy_pickle(root / "settings.pkl")
        except LegacyDecodeError as error:
            raise HelperError(getattr(error, "code", OUTCOME_SOURCE_CORRUPT), str(error)) from error
        if not isinstance(settings, Mapping):
            raise HelperError(OUTCOME_SOURCE_CORRUPT, "settings.pkl does not contain an object")
        bundle.settings = {str(key): value for key, value in settings.items()}
    elif "nfprogress.db" in inspection.files_present and owners.get("settings") == "sqlite":
        try:
            bundle.settings = _read_sqlite_section(root, "SELECT key, value_json FROM settings", 1)
        except (sqlite3.Error, json.JSONDecodeError) as error:
            raise HelperError(OUTCOME_SOURCE_CORRUPT, "SQLite settings are invalid") from error
    else:
        bundle.settings = {}

    if owners.get("notes") == "sqlite" and "nfprogress.db" in inspection.files_present:
        try:
            bundle.notes = list(_read_sqlite_section(root, "SELECT id, payload_json FROM notes", 1).values())
        except (sqlite3.Error, json.JSONDecodeError) as error:
            raise HelperError(OUTCOME_SOURCE_CORRUPT, "SQLite Notes are invalid") from error
    elif envelope:
        bundle.notes = canonical_notes_from_projects(envelope)
    else:
        bundle.notes = []

    if owners.get("game") == "sqlite" and "nfprogress.db" in inspection.files_present:
        try:
            game_payload = next(iter(_read_sqlite_section(root, "SELECT id, payload_json FROM game_state", 1).values()), EMPTY_GAME_PAYLOAD)
        except (sqlite3.Error, json.JSONDecodeError) as error:
            raise HelperError(OUTCOME_SOURCE_CORRUPT, "SQLite Game state is invalid") from error
        bundle.game = game_payload
    elif "gamer.pkl" in inspection.files_present:
        try:
            gamer = load_legacy_pickle(root / "gamer.pkl")
        except LegacyDecodeError as error:
            raise HelperError(getattr(error, "code", OUTCOME_SOURCE_CORRUPT), str(error)) from error
        bundle.game = _game_payload(gamer, envelope)
    else:
        bundle.game = dict(EMPTY_GAME_PAYLOAD)

    documents, bindings, external = _load_documents(root, project_ids, stage_ids, warnings)
    bundle.documents, bundle.document_bindings, bundle.external_file_manifest = documents, bindings, external
    bundle.source_manifest = _source_manifest(root, inspection)
    preview = MigrationPreview(
        projects=len(bundle.projects),
        stages=sum(len(project.get("stages", [])) for project in bundle.projects),
        progress_entries=sum(
            len(project.get("payload", {}).get("progress_entries", []))
            + sum(len(stage.get("payload", {}).get("progress_entries", [])) for stage in project.get("stages", []))
            for project in bundle.projects
        ),
        notes=len(bundle.notes or []), game=bundle.game is not None,
        maps=sum(bool(project.get("payload", {}).get("mindmap")) for project in bundle.projects)
        + sum(bool(stage.get("payload", {}).get("mindmap")) for project in bundle.projects for stage in project.get("stages", [])),
        documents=len(documents), external_bindings=len(bindings) + sum(
            bool(project.get("binding"))
            + sum(bool(stage.get("binding")) for stage in project.get("stages", []))
            for project in bundle.projects
        ), warnings=warnings,
    )
    bundle.bundle_manifest = {
        "bundle_version": LATEST_BUNDLE_VERSION,
        "helper_version": HELPER_VERSION,
        "source_fingerprint": inspection.fingerprint,
        "created_at": _now(),
        "source_profile": inspection.source_profile,
        "entity_counts": preview.to_dict() | {"warnings": warnings},
        "checksums": {name: details["checksum"] for name, details in bundle.source_manifest.items()},
    }
    bundle.bundle_manifest["bundle_checksum"] = bundle_checksum(bundle)
    try:
        validate_migration_bundle(bundle)
    except RecoveryError as error:
        raise HelperError(OUTCOME_BUNDLE_INVALID, str(error)) from error
    return bundle, preview


def _json(value: Any) -> str:
    # ``settings.pkl`` is a legacy Python store and may contain date/datetime
    # values.  Normalize it through the same strict JSON projection used by
    # the migration bundle instead of widening the restricted unpickler.
    return json.dumps(to_json_safe(value), ensure_ascii=False, allow_nan=False, sort_keys=True)


def _import_complete_bundle(bundle: MigrationBundle, target: Path, inspection: SourceInspection) -> None:
    """Trusted importer for a full bundle; target is always a staging profile."""
    import_projects_bundle(bundle, target)
    with closing(open_database(target)) as db:
        with db:
            db.execute("DELETE FROM settings")
            db.executemany("INSERT INTO settings(key,value_json) VALUES(?,?)", [(str(k), _json(v)) for k, v in (bundle.settings or {}).items()])
            db.execute("DELETE FROM notes")
            db.executemany(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES(?,?,?,?,?)",
                [(n["id"], n["project_id"], n.get("stage_id"), n.get("updated_at"), _json(n)) for n in (bundle.notes or [])],
            )
            game_payload = bundle.game or dict(EMPTY_GAME_PAYLOAD)
            db.execute(
                "INSERT INTO game_state(id,schema_version,payload_json,updated_at) VALUES(1,2,?,datetime('now')) ON CONFLICT(id) DO UPDATE SET schema_version=2,payload_json=excluded.payload_json,updated_at=excluded.updated_at",
                (_json(game_payload),),
            )
            db.execute("DELETE FROM game_metadata")
            db.execute("INSERT INTO game_metadata(key,value_json) VALUES('dto_version',?)", (_json(bundle.dto_version),))
            db.execute("INSERT INTO game_metadata(key,value_json) VALUES('source_manifest',?)", (_json(bundle.source_manifest),))
            db.execute("DELETE FROM documents")
            db.execute("DELETE FROM document_bindings")
            db.execute("DELETE FROM document_migration_orphans")
            for document in bundle.documents:
                db.execute(
                    "INSERT INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,created_at,updated_at,revision,extensions_json) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                    (document["document_id"], f'{document["project_id"]}:{document.get("stage_id") or "project"}', document["project_id"], document.get("stage_id"), document.get("title", "Текст"), _json(document.get("content", {"type": "doc", "content": [{"type": "paragraph"}]})), document.get("content_format", "tiptap-json/v1"), document.get("created_at"), document.get("updated_at"), int(document.get("revision", 0) or 0), _json(document.get("extensions", {}))),
                )
            for binding in bundle.document_bindings:
                db.execute(
                    "INSERT INTO document_bindings(id,document_id,binding_type,external_path,source_id,last_external_hash,last_synced_revision,last_synced_hash,last_synced_at,sync_state,expected_external_hash,payload_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                    (binding["id"], binding["document_id"], binding["binding_type"], binding["external_path"], binding.get("source_id"), binding.get("last_external_hash"), binding.get("last_synced_revision", 0), binding.get("last_synced_hash"), binding.get("last_synced_at"), binding.get("sync_state", "unlinked"), None, _json(binding.get("payload", {}))),
                )
            documents_checksum = bundle.source_manifest.get("documents.json", {}).get("checksum")
            db.execute("INSERT INTO document_metadata(key,value_json) VALUES('documents_json_migration',?)", (_json({"status": "complete", "source_checksum": documents_checksum}),))
            marker = {
                "status": "ready_for_tauri", "source_fingerprint": inspection.fingerprint,
                "helper_version": HELPER_VERSION, "bundle_version": bundle.dto_version,
                "bundle_checksum": bundle.bundle_manifest.get("bundle_checksum"),
                "completed_domains": ["projects", "settings", "notes", "game", "documents"],
                "prepared_at": _now(), "source_profile": inspection.source_profile,
            }
            db.execute("INSERT OR REPLACE INTO game_metadata(key,value_json) VALUES('migration_status',?)", (_json(marker),))
            for key, value in (("source_fingerprint", inspection.fingerprint), ("helper_version", HELPER_VERSION), ("bundle_checksum", bundle.bundle_manifest.get("bundle_checksum"))):
                db.execute("INSERT OR REPLACE INTO game_metadata(key,value_json) VALUES(?,?)", (key, _json(value)))
            db.execute(
                "UPDATE storage_ownership SET owner='sqlite', schema_version=?, updated_at=datetime('now')",
                (CURRENT_SCHEMA_VERSION,),
            )
            db.execute("INSERT INTO mirror_state(id,source_format,source_schema_version,sync_status,last_full_sync_at,last_successful_sync_at,last_error) VALUES(1,'migration_bundle',?,'healthy',datetime('now'),datetime('now'),NULL) ON CONFLICT(id) DO UPDATE SET source_format='migration_bundle', source_schema_version=excluded.source_schema_version, sync_status='healthy', last_full_sync_at=datetime('now'), last_successful_sync_at=datetime('now'), last_error=NULL", (str(CURRENT_SCHEMA_VERSION),))


def _semantic_verify(bundle: MigrationBundle, target: Path, inspection: SourceInspection) -> list[str]:
    errors: list[str] = []
    try:
        validate_sqlite_file(
            target / "nfprogress.db", allow_versions={CURRENT_SCHEMA_VERSION},
        )
    except RecoveryError as error:
        return [str(error)]
    try:
        projects_ok, project_errors = verify_projects_bundle(bundle, target)
        if not projects_ok:
            errors.extend(f"projects: {error}" for error in project_errors)
    except (OSError, sqlite3.Error, ValueError, KeyError) as error:
        errors.append(f"projects semantic verifier error: {error}")
    connection = sqlite3.connect(f"file:{(target / 'nfprogress.db').resolve().as_posix()}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        owners = {row[0]: row[1] for row in connection.execute("SELECT subsystem,owner FROM storage_ownership")}
        if owners != {name: "sqlite" for name in ("projects", "settings", "notes", "game")}:
            errors.append("all domain owners are not SQLite")
        marker = json.loads(connection.execute("SELECT value_json FROM game_metadata WHERE key='migration_status'").fetchone()[0])
        if marker.get("status") != "ready_for_tauri" or marker.get("source_fingerprint") != inspection.fingerprint:
            errors.append("prepared migration marker is invalid")
        expected_counts = {
            "projects": len(bundle.projects), "stages": sum(len(p.get("stages", [])) for p in bundle.projects),
            "progress_entries": sum(len(p.get("payload", {}).get("progress_entries", [])) + sum(len(s.get("payload", {}).get("progress_entries", [])) for s in p.get("stages", [])) for p in bundle.projects),
            "notes": len(bundle.notes or []), "documents": len(bundle.documents),
        }
        for table, expected in expected_counts.items():
            if connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] != expected:
                errors.append(f"{table} count mismatch")
        if connection.execute("SELECT sync_status FROM mirror_state WHERE id=1").fetchone()[0] != "healthy":
            errors.append("mirror is not healthy")
        if bundle.settings is not None:
            actual_settings = {row[0]: json.loads(row[1]) for row in connection.execute("SELECT key,value_json FROM settings")}
            expected_settings = {
                str(key): to_json_safe(value)
                for key, value in bundle.settings.items()
            }
            if actual_settings != expected_settings:
                errors.append("settings mismatch")
        actual_docs = {
            row[0]: {
                "title": row[1], "content": json.loads(row[2]),
                "content_format": row[3], "created_at": row[4],
                "updated_at": row[5], "revision": row[6],
                "extensions": json.loads(row[7]),
            }
            for row in connection.execute(
                "SELECT id,title,content_json,content_format,created_at,updated_at,revision,extensions_json FROM documents"
            )
        }
        expected_docs = {
            d["document_id"]: {
                "title": d.get("title", "Текст"), "content": d.get("content"),
                "content_format": d.get("content_format", "tiptap-json/v1"),
                "created_at": d.get("created_at"), "updated_at": d.get("updated_at"),
                "revision": int(d.get("revision", 0) or 0),
                "extensions": d.get("extensions", {}),
            }
            for d in bundle.documents
        }
        if actual_docs != expected_docs:
            errors.append("documents mismatch")
        actual_document_bindings = {
            row[0]: {
                "document_id": row[1], "binding_type": row[2],
                "external_path": row[3], "source_id": row[4],
                "last_external_hash": row[5], "last_synced_revision": row[6],
                "last_synced_hash": row[7], "last_synced_at": row[8],
                "sync_state": row[9], "payload": json.loads(row[10]),
            }
            for row in connection.execute(
                "SELECT id,document_id,binding_type,external_path,source_id,last_external_hash,last_synced_revision,last_synced_hash,last_synced_at,sync_state,payload_json FROM document_bindings"
            )
        }
        expected_document_bindings = {
            binding["id"]: {
                "document_id": binding["document_id"], "binding_type": binding["binding_type"],
                "external_path": binding["external_path"], "source_id": binding.get("source_id"),
                "last_external_hash": binding.get("last_external_hash"),
                "last_synced_revision": binding.get("last_synced_revision", 0),
                "last_synced_hash": binding.get("last_synced_hash"),
                "last_synced_at": binding.get("last_synced_at"),
                "sync_state": binding.get("sync_state", "unlinked"),
                "payload": binding.get("payload", {}),
            }
            for binding in bundle.document_bindings
        }
        if actual_document_bindings != expected_document_bindings:
            errors.append("document bindings mismatch")
        actual_notes = {row[0]: json.loads(row[1]) for row in connection.execute("SELECT id,payload_json FROM notes")}
        if actual_notes != {n["id"]: n for n in (bundle.notes or [])}:
            errors.append("notes mismatch")
        actual_game = json.loads(connection.execute("SELECT payload_json FROM game_state WHERE id=1").fetchone()[0])
        if actual_game != (bundle.game or EMPTY_GAME_PAYLOAD):
            errors.append("Game payload mismatch")
    except (sqlite3.Error, TypeError, KeyError, json.JSONDecodeError) as error:
        errors.append(f"semantic verifier error: {error}")
    finally:
        connection.close()
    return errors


def verify_prepared_profile(data_root: str | Path) -> tuple[bool, list[str]]:
    """Independently answer READY_FOR_TAURI/NOT_READY without mutation."""
    root = Path(data_root).expanduser().resolve()
    database = root / "nfprogress.db"
    if not database.is_file():
        legacy_present = any(
            (root / name).is_file() for name in ("data.pkl", "gamer.pkl", "settings.pkl", "documents.json")
        )
        return (not legacy_present), ["fresh_install" if not legacy_present else "migration_required"]
    errors: list[str] = []
    try:
        version = validate_sqlite_file(
            database, allow_versions={6, 7, CURRENT_SCHEMA_VERSION},
        )
        if version not in (6, 7, CURRENT_SCHEMA_VERSION):
            errors.append("schema is not supported")
    except RecoveryError as error:
        errors.append(str(error))
        return False, errors
    connection = sqlite3.connect(f"file:{database.resolve().as_posix()}?mode=ro", uri=True)
    try:
        owners = {row[0]: row[1] for row in connection.execute("SELECT subsystem,owner FROM storage_ownership")}
        if owners != {name: "sqlite" for name in ("projects", "settings", "notes", "game")}:
            errors.append("ownership is not fully SQLite")
        row = connection.execute("SELECT value_json FROM game_metadata WHERE key='migration_status'").fetchone()
        marker = json.loads(row[0]) if row else None
        if marker is not None and marker.get("status") != "ready_for_tauri":
            errors.append("migration_required")
        doc_marker = connection.execute("SELECT value_json FROM document_metadata WHERE key='documents_json_migration'").fetchone()
        if doc_marker and json.loads(doc_marker[0]).get("status") != "complete":
            errors.append("documents migration is incomplete")
        if (marker is None) != (doc_marker is None):
            errors.append("migration markers are incomplete")
    finally:
        connection.close()
    return not errors, errors


def refresh_test_data_profile(
    source_root: str | Path,
    destination_root: str | Path,
) -> MigrationReport:
    """Refresh a canonical developer profile through the migration contract.

    The source is copied to a private staging directory first.  This allows
    the existing project-order recovery to repair a legacy SQLite mirror
    without ever mutating the production root.  ``prepare`` then builds and
    atomically activates the complete SQLite-authoritative profile.
    """
    source = Path(source_root).expanduser().resolve()
    destination = Path(destination_root).expanduser().resolve()
    if source == destination:
        raise HelperError(
            OUTCOME_DESTINATION_INVALID,
            "test-data refresh requires distinct source and destination roots",
        )

    snapshot = _snapshot(source)
    try:
        inspection = inspect_source(snapshot)
        if inspection.source_profile == SQLITE_PROJECT_ORDER_RECOVERY_PROFILE:
            recover_project_order(snapshot)
            inspection = inspect_source(snapshot)
        if not inspection.supported:
            raise HelperError(
                OUTCOME_UNSUPPORTED_SOURCE,
                f"source profile {inspection.source_profile} is not qualified",
                details=inspection.warnings,
            )
        report = prepare(snapshot, destination, replace=True)
        ready, reasons = verify_prepared_profile(destination)
        if not ready:
            raise HelperError(
                OUTCOME_MIGRATION_FAILED,
                "refreshed test-data profile is not ready for Tauri",
                details=reasons,
            )
        return report
    finally:
        shutil.rmtree(snapshot, ignore_errors=True)


def _snapshot(root: Path) -> Path:
    directory = Path(tempfile.mkdtemp(prefix="nfprogress-migration-source-"))
    try:
        for name in _known_files(root):
            shutil.copy2(root / name, directory / name)
        return directory
    except Exception:
        shutil.rmtree(directory, ignore_errors=True)
        raise


def _activate(staging: Path, destination: Path) -> Path | None:
    destination.mkdir(parents=True, exist_ok=True)
    target_db = destination / "nfprogress.db"
    rollback: Path | None = None
    if target_db.exists():
        rollback = destination.parent / f".{destination.name}-migration-rollback-{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S.%fZ')}"
        os.replace(target_db, rollback)
    try:
        os.replace(staging / "nfprogress.db", target_db)
    except Exception:
        if target_db.exists():
            target_db.unlink()
        if rollback and rollback.exists():
            os.replace(rollback, target_db)
        raise
    return rollback


def prepare(
    source_root: str | Path,
    destination_root: str | Path | None = None,
    *,
    dry_run: bool = False,
    replace: bool = False,
    bundle_out: str | Path | None = None,
) -> MigrationReport:
    phase = "path_resolution"
    source = Path(source_root).expanduser().resolve()
    destination = Path(destination_root or source_root).expanduser().resolve()
    phase = "source_inspection"
    inspection = inspect_source(source)
    already_ready, _reasons = verify_prepared_profile(source)
    if already_ready and destination == source:
        # A prepared SQLite profile remains authoritative even when retained
        # recovery PKL/JSON files are stale or later become unreadable.
        return MigrationReport(
            OUTCOME_READY, inspection.source_profile, inspection.fingerprint,
            MigrationPreview(), details=["already prepared; SQLite remains authoritative"],
        )
    if inspection.source_profile == "fresh_install":
        return MigrationReport(OUTCOME_READY, inspection.source_profile, inspection.fingerprint, MigrationPreview())
    if not inspection.supported:
        raise HelperError(OUTCOME_UNSUPPORTED_SOURCE, f"source profile {inspection.source_profile} is not qualified", details=inspection.warnings)
    if destination.exists() and destination != source and (destination / "nfprogress.db").exists() and not replace:
        raise HelperError(OUTCOME_DESTINATION_INVALID, "non-empty destination requires explicit --replace")
    source_before = inspection.fingerprint
    phase = "source_snapshot"
    snapshot = _snapshot(source)
    staging: Path | None = None
    backup: Path | None = None
    try:
        phase = "bundle_build"
        snapshot_inspection = inspect_source(snapshot)
        bundle, preview = build_bundle(snapshot_inspection, snapshot)
        # Preserve the original source fingerprint, not the private snapshot
        # path; its bytes are identical and the fingerprint is path-independent.
        bundle.source_manifest = _source_manifest(snapshot, inspection)
        bundle.bundle_manifest["source_fingerprint"] = source_before
        bundle.bundle_manifest["bundle_checksum"] = bundle_checksum(bundle)
        validate_migration_bundle(bundle)
        if bundle_out is not None and not dry_run:
            output = Path(bundle_out).expanduser().resolve()
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(bundle.to_json() + "\n", encoding="utf-8")
        if dry_run:
            return MigrationReport(OUTCOME_MIGRATION_VERIFIED, inspection.source_profile, source_before, preview, bundle=str(bundle_out) if bundle_out else None, details=["dry_run: no source or destination mutation"])
        if any(path.is_file() for path in (destination / name for name in _known_files(destination))):
            phase = "backup_sealing"
            backup = create_application_backup(destination)
        phase = "staging_create"
        staging = Path(tempfile.mkdtemp(prefix=f".{destination.name}-migration-", dir=destination.parent))
        phase = "staging_import"
        _import_complete_bundle(bundle, staging, inspection)
        phase = "staging_semantic_verify"
        errors = _semantic_verify(bundle, staging, inspection)
        if errors:
            raise HelperError(OUTCOME_MIGRATION_FAILED, "staging semantic verification failed", details=errors)
        if source_fingerprint(source) != source_before:
            raise HelperError(OUTCOME_MIGRATION_FAILED, "source_changed: source fingerprint changed during migration")
        phase = "staging_activation"
        rollback = _activate(staging, destination)
        staging = None
        if bundle_out is None:
            phase = "migration_manifest_write"
            output = destination / "nfprogress-migration.json"
            output.write_text(bundle.to_json() + "\n", encoding="utf-8")
            bundle_path = str(output)
        else:
            bundle_path = str(Path(bundle_out).expanduser().resolve())
        return MigrationReport(OUTCOME_MIGRATION_VERIFIED, inspection.source_profile, source_before, preview, backup=str(backup) if backup else None, bundle=bundle_path, details=[f"integrity_check=ok", f"foreign_key_check=ok", "activation=atomic", *( [f"rollback={rollback}"] if rollback else [])])
    except HelperError:
        raise
    except (OSError, sqlite3.Error, RecoveryError, ValueError) as error:
        details = [f"phase={phase}", *inspection.warnings]
        if os.environ.get("NFPROGRESS_HELPER_DEBUG") == "1":
            print(f"[nfprogress-helper-debug] phase={phase}", file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
        raise HelperError(OUTCOME_MIGRATION_FAILED, str(error), details=details) from error
    finally:
        shutil.rmtree(snapshot, ignore_errors=True)
        if staging is not None:
            # Retain no executable/partial DB in the active destination.  The
            # error report remains the diagnostic record; pre-existing data is
            # untouched because activation has not happened.
            shutil.rmtree(staging, ignore_errors=True)


def _print(value: Any, *, as_json: bool) -> None:
    if as_json:
        print(json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2))
    elif isinstance(value, SourceInspection):
        print(f"Profile: {value.source_profile}\nVersion: {value.source_version}\nFingerprint: {value.fingerprint}\nSupported: {value.supported}\nFiles: {', '.join(value.files_present) or 'none'}")
    elif isinstance(value, MigrationReport):
        print(f"Migration {value.outcome}\nProjects: {value.preview.projects}\nStages: {value.preview.stages}\nProgress entries: {value.preview.progress_entries}\nNotes: {value.preview.notes}\nDocuments: {value.preview.documents}\nExternal bindings: {value.preview.external_bindings}")
        if value.backup: print(f"Backup: {value.backup}")
        if value.preview.warnings: print(f"Warnings: {len(value.preview.warnings)}")
        for detail in value.details: print(detail)
    elif isinstance(value, ProjectOrderRecoveryPreview):
        print(
            f"Project order recovery required: {value.requires_recovery}\n"
            f"Projects: {len(value.project_ids)}\n"
            f"Existing order: {', '.join(value.existing_order) or 'empty'}\n"
            f"Proposed order: {', '.join(value.proposed_order) or 'empty'}\n"
            f"Order source: {value.order_source}"
        )
        for issue in value.issues: print(f"Issue: {issue}")
    elif isinstance(value, ProjectOrderRecoveryReport):
        print(f"Order recovery {value.outcome}")
        if value.backup: print(f"Backup: {value.backup}")
        if value.rollback: print(f"Rollback: {value.rollback}")
        if value.source_sha256: print(f"Source SHA-256: {value.source_sha256}")
        if value.backup_sha256: print(f"Backup SHA-256: {value.backup_sha256}")
        for detail in value.details: print(detail)
    else:
        print(value)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command",
        choices=("detect", "preview", "prepare", "verify", "recover-preview", "recover"),
    )
    parser.add_argument("--source", "--data-dir", dest="source", required=False)
    parser.add_argument("--destination", dest="destination")
    parser.add_argument("--bundle-out", dest="bundle_out")
    parser.add_argument("--replace", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args(argv)
    try:
        if args.command == "verify":
            if not args.source:
                parser.error("verify requires --source")
            ready, reasons = verify_prepared_profile(args.source)
            value = {"status": "READY_FOR_TAURI" if ready else "NOT_READY", "reasons": reasons}
            _print(value, as_json=args.as_json)
            return 0 if ready else EXIT_CODES[OUTCOME_MIGRATION_REQUIRED]
        if not args.source:
            parser.error(f"{args.command} requires --source")
        if args.command == "recover-preview":
            preview = analyze_project_order_recovery(args.source)
            _print(preview.to_dict() if args.as_json else preview, as_json=args.as_json)
            return 0
        if args.command == "recover":
            report = recover_project_order(args.source)
            _print(report.to_dict() if args.as_json else report, as_json=args.as_json)
            return EXIT_CODES[report.outcome]
        inspection = inspect_source(args.source)
        if args.command == "preview" and inspection.source_profile == SQLITE_PROJECT_ORDER_RECOVERY_PROFILE:
            preview = analyze_project_order_recovery(args.source)
            _print(preview.to_dict() if args.as_json else preview, as_json=args.as_json)
            return 0
        if args.command == "detect":
            _print(inspection.to_dict() if args.as_json else inspection, as_json=args.as_json)
            return 0 if inspection.supported else EXIT_CODES[OUTCOME_UNSUPPORTED_SOURCE]
        if args.command == "preview":
            snapshot = _snapshot(Path(args.source).expanduser().resolve())
            try:
                bundle, preview = build_bundle(inspect_source(snapshot), snapshot)
            finally:
                shutil.rmtree(snapshot, ignore_errors=True)
            _print(preview.to_dict() if args.as_json else MigrationReport(OUTCOME_MIGRATION_VERIFIED, inspection.source_profile, inspection.fingerprint, preview), as_json=args.as_json)
            return 0
        report = prepare(args.source, args.destination, dry_run=args.dry_run, replace=args.replace, bundle_out=args.bundle_out)
        _print(report.to_dict() if args.as_json else report, as_json=args.as_json)
        return EXIT_CODES[report.outcome]
    except HelperError as error:
        value = {"status": error.code, "error": str(error), "details": error.details}
        _print(value, as_json=args.as_json)
        return EXIT_CODES.get(error.code, EXIT_CODES[OUTCOME_MIGRATION_FAILED])
    except LegacyDecodeError as error:
        value = {"status": getattr(error, "code", OUTCOME_SOURCE_CORRUPT), "error": str(error)}
        _print(value, as_json=args.as_json)
        return EXIT_CODES.get(value["status"], EXIT_CODES[OUTCOME_SOURCE_CORRUPT])
    except (OSError, RecoveryError, sqlite3.Error, ValueError) as error:
        _print({"status": OUTCOME_MIGRATION_FAILED, "error": str(error)}, as_json=args.as_json)
        return EXIT_CODES[OUTCOME_MIGRATION_FAILED]


if __name__ == "__main__":
    raise SystemExit(main())
