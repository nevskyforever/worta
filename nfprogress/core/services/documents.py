"""Rich project documents stored independently from legacy progress pickles."""
from __future__ import annotations

import base64
import hashlib
import json
import os
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from nfprogress.core.errors import NotFoundError, ValidationError
import engine


EMPTY_DOCUMENT = {'type': 'doc', 'content': [{'type': 'paragraph'}]}


class ProjectDocumentService:
    """Persist Tiptap JSON and mediate binary DOCX links on desktop.

    DOCX conversion intentionally remains in the TypeScript client.  The sidecar
    only owns local paths and bytes, exactly as the existing integration service.
    """

    def __init__(self, repository, project_service, *, allow_local_files: bool) -> None:
        self.repository = repository
        self.project_service = project_service
        self.allow_local_files = allow_local_files

    def get(self, project_id: str, stage_id: str | None = None) -> dict[str, Any]:
        key = self._key(project_id, stage_id)
        with self.repository.locked():
            records = self._load_with_migrations()
            self._validate_document_owner(project_id, stage_id)
            return self._public(records.get(key) or self._new(project_id, stage_id))

    def move_project_document_to_stage(self, project_id: str, stage_id: str) -> None:
        """Move a legacy project document to the first stage after conversion."""
        with self.repository.locked():
            self._validate_owner(project_id, stage_id)
            records = self._load()
            if self._move_project_document(records, project_id, stage_id):
                self._write(records)

    def list_existing(self) -> list[dict[str, Any]]:
        with self.repository.locked():
            records = self._load_with_migrations()
            data = self.repository.read_projects()
            visible = []
            for record in records.values():
                if not isinstance(record, dict) or not record.get('exists'):
                    continue
                project_id = record.get('project_id')
                if not isinstance(project_id, str) or not project_id:
                    continue
                stage_id = record.get('stage_id')
                try:
                    project = self.project_service._find_project(data, project_id)
                    entity = (
                        self.project_service._find_stage(project, stage_id)
                        if stage_id else project
                    )
                except NotFoundError:
                    continue
                if self._work_method(entity) != 'app':
                    continue
                visible.append(self._public(record))
            visible.sort(
                key=lambda record: self._parse_updated_at(
                    record.get('updated_at'),
                ) if record.get('updated_at') else datetime.min,
                reverse=True,
            )
            return visible

    def save(self, project_id: str, content: dict[str, Any], stage_id: str | None = None) -> dict[str, Any]:
        if not isinstance(content, dict) or content.get('type') != 'doc':
            raise ValidationError('Документ должен быть в формате Tiptap JSON.')
        key = self._key(project_id, stage_id)
        with self.repository.locked():
            records = self._load_with_migrations()
            self._validate_document_owner(project_id, stage_id)
            record = records.get(key) or self._new(project_id, stage_id)
            record.update({'content': content, 'exists': True, 'updated_at': self._now(), 'local_dirty': True})
            records[key] = record
            self._write(records)
            return self._public(record)

    def link(self, project_id: str, path: str, stage_id: str | None = None) -> dict[str, Any]:
        if not self.allow_local_files:
            raise ValidationError('Связь с локальным Word-файлом доступна только в desktop-приложении.')
        source = Path(path).expanduser().resolve()
        if source.suffix.lower() != '.docx':
            raise ValidationError('Поддерживаются только документы Word .docx.')
        key = self._key(project_id, stage_id)
        with self.repository.locked():
            records = self._load_with_migrations()
            self._validate_document_owner(project_id, stage_id)
            record = records.get(key) or self._new(project_id, stage_id)
            record.update({'docx_path': str(source), 'exists': True, 'updated_at': self._now(), 'local_dirty': True})
            records[key] = record
            self._write(records)
            return self._public(record)

    def write_docx(self, project_id: str, payload: str, stage_id: str | None = None) -> dict[str, Any]:
        if not self.allow_local_files:
            raise ValidationError('Связь с локальным Word-файлом доступна только в desktop-приложении.')
        key = self._key(project_id, stage_id)
        try:
            raw = base64.b64decode(payload, validate=True)
        except (ValueError, TypeError) as error:
            raise ValidationError('Некорректные данные DOCX.') from error
        with self.repository.locked():
            records = self._load_with_migrations()
            self._validate_document_owner(project_id, stage_id)
            record = records.get(key)
            if not record or not record.get('docx_path'):
                raise ValidationError('Сначала свяжите документ с файлом Word.')
            path = Path(record['docx_path'])
            path.parent.mkdir(parents=True, exist_ok=True)
            descriptor, temporary = tempfile.mkstemp(prefix='.nfprogress-', suffix='.docx', dir=path.parent)
            try:
                with os.fdopen(descriptor, 'wb') as stream:
                    stream.write(raw); stream.flush(); os.fsync(stream.fileno())
                os.replace(temporary, path)
            finally:
                if os.path.exists(temporary): os.unlink(temporary)
            record.update({'last_synced_hash': self._hash(raw), 'last_synced_at': self._now(), 'local_dirty': False, 'word_dirty': False, 'sync_state': 'synced'})
            records[key] = record; self._write(records)
            return self._public(record)

    def read_external_docx(self, project_id: str, stage_id: str | None = None) -> dict[str, Any]:
        if not self.allow_local_files:
            raise ValidationError('Связь с локальным Word-файлом доступна только в desktop-приложении.')
        key = self._key(project_id, stage_id)
        with self.repository.locked():
            records = self._load_with_migrations()
            self._validate_document_owner(project_id, stage_id)
            record = records.get(key)
            if not record or not record.get('docx_path'): return {'state': 'unlinked'}
            path = Path(record['docx_path'])
            if not path.is_file(): return {'state': 'missing'}
            raw = path.read_bytes(); digest = self._hash(raw)
            if digest == record.get('last_synced_hash'): return {'state': 'synced'}
            state = 'conflict' if record.get('local_dirty') else 'word_changed'
            record.update({'word_dirty': True, 'sync_state': state})
            records[key] = record; self._write(records)
            return {'state': state, 'content_base64': base64.b64encode(raw).decode('ascii'), 'hash': digest}

    def accept_word(self, project_id: str, content: dict[str, Any], source_hash: str, stage_id: str | None = None) -> dict[str, Any]:
        if not self.allow_local_files:
            raise ValidationError('Связь с локальным Word-файлом доступна только в desktop-приложении.')
        key = self._key(project_id, stage_id)
        with self.repository.locked():
            records = self._load_with_migrations()
            self._validate_document_owner(project_id, stage_id)
            record = records.get(key) or self._new(project_id, stage_id)
            record.update({'content': content, 'exists': True, 'updated_at': self._now(), 'last_synced_hash': source_hash, 'last_synced_at': self._now(), 'local_dirty': False, 'word_dirty': False, 'sync_state': 'synced'})
            records[key] = record; self._write(records)
            return self._public(record)

    def record_text_progress(
            self, project_id: str, stage_id: str | None = None,
            content: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Persist the current text and record its progress as one operation."""
        if content is not None and (
                not isinstance(content, dict) or content.get('type') != 'doc'
        ):
            raise ValidationError('Документ должен быть в формате Tiptap JSON.')
        key = self._key(project_id, stage_id)
        with self.repository.locked():
            records = self._load_with_migrations()
            self._validate_document_owner(project_id, stage_id)
            record = records.get(key)
            if content is not None:
                record = record or self._new(project_id, stage_id)
                record.update({
                    'content': content,
                    'exists': True,
                    'updated_at': self._now(),
                    'local_dirty': True,
                })
                records[key] = record
                self._write(records)
            symbols = self._symbol_count(record.get('content', {})) if record else 0
            if symbols <= 0:
                raise ValidationError('Сначала добавьте текст в документ.')
            document = self._public(record)
            data = self.repository.read_projects()
            project = self.project_service._find_project(data, project_id)
            entity = self.project_service._find_stage(project, stage_id) if stage_id else project
            total = engine.unit_converter('symbols', symbols, entity.unit)
            if abs(float(total) - float(entity.total_units)) < 0.009:
                return {
                    'changed': False,
                    'symbols': symbols,
                    'progress': None,
                    'document': document,
                }
            # An in-app edit is a writing action, not an external sync event.
            # Let the project service timestamp it with ``today_for_test`` so
            # developer-mode dates and daily streaks use the same calendar day
            # as every other in-app progress entry.
            progress = self.project_service.record_document_progress(
                project_id, stage_id=stage_id, new_total=total,
            )
            return {
                'changed': True,
                'symbols': symbols,
                'progress': progress,
                'document': document,
            }

    def ensure_external_sync_can_be_configured(
            self, project_id: str, stage_id: str | None = None,
    ) -> None:
        """Keep file synchronization and an internal manuscript mutually exclusive."""
        key = self._key(project_id, stage_id)
        with self.repository.locked():
            record = self._load_with_migrations().get(key)
            if record and self._symbol_count(record.get('content', {})) > 0:
                raise ValidationError(
                    'Сначала очистите текст документа или используйте «Добавить запись».',
                )

    def _validate_owner(self, project_id: str, stage_id: str | None) -> None:
        data = self.repository.read_projects()
        project = self.project_service._find_project(data, project_id)
        if stage_id: self.project_service._find_stage(project, stage_id)

    def _validate_document_owner(self, project_id: str, stage_id: str | None) -> None:
        data = self.repository.read_projects()
        project = self.project_service._find_project(data, project_id)
        entity = self.project_service._find_stage(project, stage_id) if stage_id else project
        if stage_id is None and getattr(project, 'stages', []):
            raise ValidationError('У проекта с этапами нет отдельного текста.')
        if self._work_method(entity) != 'app':
            raise ValidationError('Для работы с текстом выберите метод «В приложении».')

    @staticmethod
    def _work_method(entity: object) -> str:
        return getattr(
            entity,
            'work_method',
            'sync' if getattr(entity, 'synch', None) is not None else 'manual',
        )

    def _load_with_migrations(self) -> dict[str, dict[str, Any]]:
        records = self._load()
        if self._migrate_project_documents_to_first_stages(records):
            self._write(records)
        return records

    def _migrate_project_documents_to_first_stages(
            self, records: dict[str, dict[str, Any]],
    ) -> bool:
        data = self.repository.read_projects()
        projects = data.get('projects', {}) if isinstance(data, dict) else {}
        changed = False
        for project in projects.values():
            project_id = getattr(project, 'project_id', None)
            stages = getattr(project, 'stages', [])
            first_stage_id = (
                getattr(stages[0], 'stage_id', None)
                if isinstance(stages, list) and stages else None
            )
            if project_id and first_stage_id:
                changed = self._move_project_document(
                    records, project_id, first_stage_id,
                ) or changed
        return changed

    def _move_project_document(
            self, records: dict[str, dict[str, Any]],
            project_id: str, stage_id: str,
    ) -> bool:
        source_key = self._key(project_id, None)
        target_key = self._key(project_id, stage_id)
        source = records.get(source_key)
        if not isinstance(source, dict) or not source.get('exists'):
            return False

        target = records.get(target_key)
        if isinstance(target, dict) and target.get('exists') and (
                self._symbol_count(target.get('content', {})) > 0
                or target.get('docx_path')
        ):
            # Both records are recovery-significant. Preserve the original
            # scope conflict until a caller resolves it explicitly.
            return False

        moved = dict(source)
        moved.update({'project_id': project_id, 'stage_id': stage_id})
        records[target_key] = moved
        del records[source_key]
        return True

    @staticmethod
    def _symbol_count(value: object) -> int:
        if isinstance(value, dict):
            return len(str(value.get('text', ''))) + sum(
                ProjectDocumentService._symbol_count(item)
                for item in value.get('content', [])
                if isinstance(item, dict)
            )
        return 0

    @staticmethod
    def _parse_updated_at(value: object) -> datetime:
        if isinstance(value, str):
            try:
                parsed = datetime.fromisoformat(value)
                return parsed.astimezone().replace(tzinfo=None) if parsed.tzinfo else parsed
            except ValueError:
                pass
        return datetime.now()

    @staticmethod
    def _key(project_id: str, stage_id: str | None) -> str: return f'{project_id}:{stage_id or "project"}'
    @staticmethod
    def _now() -> str: return datetime.now(timezone.utc).isoformat()
    @staticmethod
    def _hash(raw: bytes) -> str: return hashlib.sha256(raw).hexdigest()
    @staticmethod
    def _new(project_id: str, stage_id: str | None) -> dict[str, Any]:
        return {'project_id': project_id, 'stage_id': stage_id, 'content': EMPTY_DOCUMENT, 'exists': False, 'updated_at': None, 'docx_path': None, 'sync_state': 'unlinked', 'last_synced_hash': None, 'last_synced_at': None, 'local_dirty': False, 'word_dirty': False}
    def _public(self, record: dict[str, Any]) -> dict[str, Any]:
        result = dict(record) if self.allow_local_files else {key: value for key, value in record.items() if key in {'project_id','stage_id','document_id','title','content','content_format','created_at','updated_at','exists','revision'}}
        if not self.allow_local_files:
            result.update(docx_path=None,sync_state='unlinked',last_synced_hash=None,last_synced_at=None,local_dirty=False,word_dirty=False)
        result['symbols'] = self._symbol_count(result.get('content', {}))
        result['has_content'] = result['symbols'] > 0
        return result
    def _path(self) -> Path: return self.repository.base_dir / 'documents.json'
    def _load(self) -> dict[str, dict[str, Any]]:
        path = self._path()
        if not path.exists(): return {}
        try:
            value = json.loads(path.read_text(encoding='utf-8'))
            return value if isinstance(value, dict) else {}
        except (OSError, json.JSONDecodeError): return {}
    def _write(self, records: dict[str, dict[str, Any]]) -> None:
        path = self._path(); descriptor, temporary = tempfile.mkstemp(prefix='.documents-', suffix='.json', dir=path.parent)
        try:
            with os.fdopen(descriptor, 'w', encoding='utf-8') as stream:
                json.dump(records, stream, ensure_ascii=False); stream.flush(); os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary): os.unlink(temporary)
