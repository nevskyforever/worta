import json
import sqlite3
from datetime import datetime, timezone

import engine
import game
import pytest

from nfprogress.core.sqlite import (
    SQLiteMirrorRepository,
    StorageOwner,
    StorageOwnershipRepository,
    Subsystem,
)
from nfprogress.core.sqlite.connection import open_database
from nfprogress.core.sqlite.ordering import OrderInvariantError
from nfprogress.core.sqlite.schema import CURRENT_SCHEMA_VERSION
from nfprogress.core.storage import PickleRepository
from nfprogress.sqlite_verify import verify


def _seed_repository(tmp_path):
    repository = PickleRepository(tmp_path)
    project = engine.Project('Проект Ω', 100)
    stage = engine.Stage('Этап', 100, parent_project_name=project.name)
    stage.notes.append(engine.Note(10, 10, 10, entry_id='progress-stage'))
    project.stages = [stage]
    project.enable_stages = True
    project.notes.append(engine.Note(20, 20, 20, entry_id='progress-project'))
    project.project_notes = [{'id': 'note-1', 'content': None, 'updated_at': '2026-01-02T03:04:05+00:00'}]
    project.mindmap_data = {
        'nodeData': {'id': 'root-node', 'topic': 'Русский текст', 'children': []},
        'nfprogressCustom': {'ключ': None},
    }
    repository.write_projects({'projects': {project.name: project}, 'last': project.name})
    repository.write_settings({'language': 'ru', 'optional': None, 'date': '2026-01-02'})
    gamer = game.Gamer()
    gamer.custom_awards = []
    gamer.writing_session = {
        'started_at': datetime.now(timezone.utc),
        'duration_minutes': 0,
        'clock_source': 'wall',
    }
    repository.write_gamer(gamer)
    return repository


def test_empty_database_and_idempotent_migrations(tmp_path):
    first = open_database(tmp_path)
    first.close()
    second = open_database(tmp_path)
    assert second.execute('SELECT schema_version FROM schema_info').fetchone()[0] == CURRENT_SCHEMA_VERSION
    assert second.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='projects'").fetchone()
    assert second.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='project_order'").fetchone()
    second.close()


def test_mirror_rebuild_publishes_complete_legacy_project_order(tmp_path):
    repository = PickleRepository(tmp_path)
    projects = {}
    project_ids = []
    for index in range(5):
        project = engine.Project(f'Project {index}', 100)
        project.project_id = f'project-{index}'
        projects[project.name] = project
        project_ids.append(project.project_id)
    explicit_order = [project_ids[2], project_ids[0], project_ids[4], project_ids[3], project_ids[1]]

    SQLiteMirrorRepository(tmp_path).rebuild(
        {'projects': projects, 'project_order': explicit_order},
        {},
        game.Gamer(),
    )

    with open_database(tmp_path) as db:
        assert [row[0] for row in db.execute(
            'SELECT project_id FROM project_order ORDER BY position'
        )] == explicit_order


def test_existing_populated_schema_requires_order_recovery_before_upgrade(tmp_path):
    connection = sqlite3.connect(tmp_path / 'nfprogress.db')
    connection.executescript((__import__('pathlib').Path('nfprogress/core/sqlite/migrations/001_initial.sql')).read_text())
    connection.execute('INSERT INTO projects VALUES(?,?,?,?,?,?,?,?,?)', ('p', 'old', 1, 0, 'symbols', 'активен', None, None, '{}'))
    connection.execute('CREATE TABLE schema_info (schema_version INTEGER NOT NULL)')
    connection.execute('INSERT INTO schema_info VALUES(1)')
    connection.commit()
    connection.close()
    with pytest.raises(OrderInvariantError, match='project ordering is incomplete'):
        open_database(tmp_path)
    with sqlite3.connect(tmp_path / 'nfprogress.db') as db:
        assert db.execute('SELECT name FROM projects').fetchone()[0] == 'old'
        assert db.execute('SELECT schema_version FROM schema_info').fetchone()[0] == 2


def test_sqlite_owned_settings_are_written_to_sqlite_not_pickle(tmp_path):
    repository = _seed_repository(tmp_path)
    ownership = StorageOwnershipRepository(tmp_path)
    ownership.set_owner(Subsystem.SETTINGS, StorageOwner.SQLITE)
    with open_database(tmp_path) as db:
        db.execute("UPDATE settings SET value_json='\"sqlite-marker\"' WHERE key='language'")
        db.commit()
    repository.write_settings({'language': 'sqlite-change'})
    with open_database(tmp_path) as db:
        assert json.loads(db.execute("SELECT value_json FROM settings WHERE key='language'").fetchone()[0]) == 'sqlite-change'


def test_sqlite_owned_projects_domain_survives_rebuild(tmp_path):
    repository = _seed_repository(tmp_path)
    StorageOwnershipRepository(tmp_path).set_owner(Subsystem.PROJECTS, StorageOwner.SQLITE)
    with open_database(tmp_path) as db:
        db.execute("UPDATE projects SET name='sqlite-marker' WHERE id=(SELECT id FROM projects LIMIT 1)")
        db.commit()
    repository.write_settings({'new': True})
    with open_database(tmp_path) as db:
        assert db.execute('SELECT name FROM projects').fetchone()['name'] == 'sqlite-marker'
        assert db.execute("SELECT value_json FROM settings WHERE key='new'").fetchone()


def test_mixed_mode_writes_sqlite_owned_settings_without_pickle_mirror(tmp_path):
    repository = _seed_repository(tmp_path)
    ownership = StorageOwnershipRepository(tmp_path)
    ownership.set_owner(Subsystem.PROJECTS, StorageOwner.SQLITE)
    ownership.set_owner(Subsystem.SETTINGS, StorageOwner.SQLITE)
    with open_database(tmp_path) as db:
        db.execute("UPDATE settings SET value_json='\"sqlite-marker\"' WHERE key='language'")
        db.commit()
    repository.write_settings({'language': 'sqlite-change'})
    with open_database(tmp_path) as db:
        assert json.loads(db.execute("SELECT value_json FROM settings WHERE key='language'").fetchone()[0]) == 'sqlite-change'
        assert db.execute('SELECT COUNT(*) FROM notes').fetchone()[0] == 1
        assert db.execute('SELECT payload_json FROM game_state').fetchone()[0]


def test_verifier_ignores_sqlite_owned_domain(tmp_path):
    _seed_repository(tmp_path)
    StorageOwnershipRepository(tmp_path).set_owner(Subsystem.SETTINGS, StorageOwner.SQLITE)
    with open_database(tmp_path) as db:
        db.execute("UPDATE settings SET value_json='\"independent\"' WHERE key='language'")
        db.commit()
    consistent, messages = verify(tmp_path)
    assert consistent
    assert not any(message.startswith('Settings:') for message in messages)


def test_missing_ownership_is_fail_safe(tmp_path):
    _seed_repository(tmp_path)
    with open_database(tmp_path) as db:
        db.execute("DELETE FROM storage_ownership WHERE subsystem='settings'")
        db.commit()
    with pytest.raises(RuntimeError, match='missing storage ownership'):
        PickleRepository(tmp_path).synchronize_shadow()


def test_empty_import_and_metadata(tmp_path):
    repository = PickleRepository(tmp_path)
    repository.synchronize_shadow()
    with open_database(tmp_path) as db:
        assert db.execute('SELECT COUNT(*) FROM projects').fetchone()[0] == 0
        assert db.execute('SELECT sync_status FROM mirror_state').fetchone()[0] == 'healthy'
        assert db.execute('SELECT payload_json FROM game_state').fetchone()[0]


def test_infinite_project_is_stored_as_nullable_goal_flag(tmp_path):
    repository = PickleRepository(tmp_path)
    project = engine.Project('Бесконечный', float('inf'))
    repository.write_projects({'projects': {project.name: project}, 'last': project.name})
    with open_database(tmp_path) as db:
        row = db.execute('SELECT goal, infinite FROM projects').fetchone()
        assert row['goal'] is None
        assert row['infinite'] == 1


def test_projects_stages_progress_notes_settings_game_and_unicode(tmp_path):
    repository = _seed_repository(tmp_path)
    with open_database(tmp_path) as db:
        assert db.execute('SELECT COUNT(*) FROM projects').fetchone()[0] == 1
        assert db.execute('SELECT COUNT(*) FROM stages').fetchone()[0] == 1
        assert db.execute('SELECT COUNT(*) FROM progress_entries').fetchone()[0] == 2
        assert db.execute('SELECT COUNT(*) FROM notes').fetchone()[0] == 1
        project = db.execute('SELECT * FROM projects').fetchone()
        assert project['name'] == 'Проект Ω'
        assert json.loads(project['payload_json'])['infinite'] is False
        assert json.loads(db.execute("SELECT value_json FROM settings WHERE key='optional'").fetchone()[0]) is None
        game_payload = json.loads(db.execute('SELECT payload_json FROM game_state').fetchone()[0])
        assert isinstance(game_payload['writing_session']['started_at'], str)
        assert 'nodeData' in json.loads(project['payload_json'])['mindmap']
    assert verify(tmp_path)[0]


def test_project_order_is_mirrored_and_verified(tmp_path):
    repository = PickleRepository(tmp_path)
    projects = [engine.Project(name, 100) for name in ('A', 'B', 'C')]
    envelope = {
        'projects': {project.name: project for project in projects},
        'project_order': [projects[2].project_id, projects[0].project_id, projects[1].project_id],
    }
    repository.write_projects(envelope)
    with open_database(tmp_path) as db:
        assert [row['project_id'] for row in db.execute(
            'SELECT project_id FROM project_order ORDER BY position',
        )] == [project.project_id for project in (projects[2], projects[0], projects[1])]

        db.execute(
            'UPDATE project_order SET position = 3 WHERE project_id = ?',
            (projects[0].project_id,),
        )
        db.commit()
    assert not verify(tmp_path)[0]


def test_project_mirror_rebuild_preserves_sqlite_owned_notes(tmp_path):
    repository = _seed_repository(tmp_path)
    StorageOwnershipRepository(tmp_path).set_owner(Subsystem.NOTES, StorageOwner.SQLITE)
    with open_database(tmp_path) as db:
        db.execute(
            "UPDATE notes SET payload_json = ?, updated_at = ?",
            ('{"id":"note-1","content":"sqlite-owned"}', 'sqlite-marker'),
        )
        db.commit()
    repository.synchronize_shadow()
    with open_database(tmp_path) as db:
        note = db.execute('SELECT updated_at, payload_json FROM notes WHERE id = ?', ('note-1',)).fetchone()
        assert note['updated_at'] == 'sqlite-marker'
        assert json.loads(note['payload_json'])['content'] == 'sqlite-owned'


def test_repeated_import_has_no_duplicates_and_rebuild_is_possible(tmp_path):
    repository = _seed_repository(tmp_path)
    repository.synchronize_shadow()
    with open_database(tmp_path) as db:
        counts = [db.execute(f'SELECT COUNT(*) FROM {table}').fetchone()[0] for table in ('projects', 'stages', 'progress_entries', 'notes')]
    assert counts == [1, 1, 2, 1]
    assert verify(tmp_path)[0]


def test_verifier_reports_intentional_mismatch(tmp_path):
    repository = _seed_repository(tmp_path)
    with open_database(tmp_path) as db:
        payload = json.loads(db.execute('SELECT payload_json FROM projects').fetchone()[0])
        payload['name'] = 'Повреждено'
        db.execute("UPDATE projects SET name='Повреждено', payload_json=?", (json.dumps(payload, ensure_ascii=False),))
        db.commit()
    consistent, messages = verify(tmp_path)
    assert not consistent
    assert any(message.startswith('Projects: MISMATCH') for message in messages)


def test_sqlite_failure_after_pickle_save_is_non_fatal(tmp_path, monkeypatch):
    repository = PickleRepository(tmp_path)
    original = SQLiteMirrorRepository.rebuild

    def fail(*args, **kwargs):
        raise sqlite3.OperationalError('disk full')

    monkeypatch.setattr(SQLiteMirrorRepository, 'rebuild', fail)
    repository.write_projects({'projects': {}, 'last': None})
    assert repository.read_projects()['last'] is None
    monkeypatch.setattr(SQLiteMirrorRepository, 'rebuild', original)
    repository.synchronize_shadow()
    assert verify(tmp_path)[0]
