from __future__ import annotations

import base64
from datetime import date, datetime

from fastapi.testclient import TestClient

from backend.app.config import RuntimeConfig
from backend.app.main import create_app
from nfprogress.core.services.documents import ProjectDocumentService


def test_stage_document_migration_preserves_both_records_on_content_collision():
    service = ProjectDocumentService(None, None, allow_local_files=False)
    source_key = service._key('project', None)
    target_key = service._key('project', 'stage')
    source = {
        'project_id': 'project', 'stage_id': None, 'exists': True,
        'content': {'type': 'doc', 'content': []}, 'docx_path': '/book.docx',
    }
    target = {
        'project_id': 'project', 'stage_id': 'stage', 'exists': True,
        'content': {
            'type': 'doc', 'content': [{
                'type': 'paragraph',
                'content': [{'type': 'text', 'text': 'Existing stage text'}],
            }],
        },
    }
    records = {source_key: source.copy(), target_key: target.copy()}

    assert service._move_project_document(records, 'project', 'stage') is False
    assert records == {source_key: source, target_key: target}


def test_document_autosave_and_linked_word_change_are_tracked(tmp_path):
    app = create_app(RuntimeConfig(
        data_dir=tmp_path, platform='desktop', allow_local_files=True,
    ))
    with TestClient(app) as client:
        project = client.post(
            '/api/projects', json={'name': 'Роман', 'goal': 1000, 'work_method': 'app'},
        ).json()
        document = {'type': 'doc', 'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'Черновик'}]}]}
        saved = client.put(f"/api/documents/{project['id']}", json={'content': document})
        assert saved.status_code == 200
        assert saved.json()['content'] == document
        documents = client.get('/api/documents/list')
        assert documents.status_code == 200
        assert documents.json()[0]['project_id'] == project['id']
        assert documents.json()[0]['symbols'] == len('Черновик')
        assert documents.json()[0]['has_content'] is True

        path = tmp_path / 'roman.docx'
        assert client.put(f"/api/documents/{project['id']}/link", json={'path': str(path)}).status_code == 200
        payload = base64.b64encode(b'word-v1').decode('ascii')
        assert client.put(f"/api/documents/{project['id']}/docx", json={'content_base64': payload}).status_code == 200
        assert client.get(f"/api/documents/{project['id']}/external").json()['state'] == 'synced'

        path.write_bytes(b'word-v2')
        external = client.get(f"/api/documents/{project['id']}/external").json()
        assert external['state'] == 'word_changed'
        assert external['content_base64'] == base64.b64encode(b'word-v2').decode('ascii')


def test_document_list_is_sorted_by_latest_change(tmp_path, monkeypatch):
    app = create_app(RuntimeConfig(data_dir=tmp_path, platform='web'))
    timestamps = iter((
        '2026-08-31T09:00:00+00:00',
        '2026-08-31T11:00:00+00:00',
    ))
    monkeypatch.setattr(app.state.services.documents, '_now', lambda: next(timestamps))
    content = {'type': 'doc', 'content': [{'type': 'paragraph'}]}

    with TestClient(app) as client:
        first = client.post(
            '/api/projects', json={'name': 'Первый', 'goal': 1000, 'work_method': 'app'},
        ).json()
        second = client.post(
            '/api/projects', json={'name': 'Второй', 'goal': 1000, 'work_method': 'app'},
        ).json()
        client.put(f"/api/documents/{first['id']}", json={'content': content})
        client.put(f"/api/documents/{second['id']}", json={'content': content})

        documents = client.get('/api/documents/list').json()

    assert [item['project_id'] for item in documents] == [second['id'], first['id']]


def test_text_progress_uses_document_symbols_and_document_scope_rules(tmp_path):
    app = create_app(RuntimeConfig(data_dir=tmp_path, platform='desktop', allow_local_files=True))
    with TestClient(app) as client:
        project = client.post(
            '/api/projects', json={'name': 'Роман', 'goal': 1000, 'work_method': 'app'},
        ).json()
        content = {'type': 'doc', 'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'Текст'}]}]}
        assert client.put(f"/api/documents/{project['id']}", json={'content': content}).status_code == 200
        progress = client.post(f"/api/documents/{project['id']}/progress")
        assert progress.status_code == 200, progress.text
        assert progress.json()['progress']['added_symbols'] == len('Текст')

        created_staged_project = client.post(
            '/api/projects', json={'name': 'С этапами', 'goal': 1000},
        ).json()
        client.post(
            f"/api/projects/{created_staged_project['id']}/stages",
            json={'name': 'Этап', 'goal': 1000, 'work_method': 'app'},
        )
        staged_project = client.get(
            f"/api/projects/{created_staged_project['id']}",
        ).json()
        stage = staged_project['stages'][0]
        assert client.get(f"/api/documents/{staged_project['id']}").status_code == 422
        assert client.put(
            f"/api/documents/{staged_project['id']}", params={'stage_id': stage['id']},
            json={'content': content},
        ).status_code == 200


def test_recording_document_progress_uses_the_same_document_snapshot(tmp_path):
    """A late autosave must not change the text used by an explicit record."""
    app = create_app(RuntimeConfig(data_dir=tmp_path, platform='web'))
    initial = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'а' * 801}]}],
    }
    stale = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'а'}]}],
    }
    current = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'а' * 802}]}],
    }

    with TestClient(app) as client:
        project = client.post(
            '/api/projects', json={'name': 'Роман', 'goal': 1_000, 'work_method': 'app'},
        ).json()
        assert client.put(f"/api/documents/{project['id']}", json={'content': initial}).status_code == 200
        assert client.post(f"/api/documents/{project['id']}/progress").status_code == 200

        # This is the write order produced when an older debounced autosave
        # reaches the server after the immediate save from "Добавить запись".
        assert client.put(f"/api/documents/{project['id']}", json={'content': current}).status_code == 200
        assert client.put(f"/api/documents/{project['id']}", json={'content': stale}).status_code == 200

        recorded = client.post(
            f"/api/documents/{project['id']}/progress", json={'content': current},
        )

        assert recorded.status_code == 200, recorded.text
        assert recorded.json()['progress']['entry']['added_symbols'] == 1
        assert client.get(f"/api/documents/{project['id']}").json()['content'] == current


def test_recording_stage_document_progress_uses_the_same_document_snapshot(tmp_path):
    app = create_app(RuntimeConfig(data_dir=tmp_path, platform='web'))
    initial = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'а' * 10}]}],
    }
    stale = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'а'}]}],
    }
    current = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'а' * 11}]}],
    }

    with TestClient(app) as client:
        project = client.post(
            '/api/projects',
            json={
                'name': 'Роман',
                'goal': 1_000,
                'stages': [{'name': 'Глава', 'goal': 1_000, 'work_method': 'app'}],
            },
        ).json()
        stage_id = project['stages'][0]['id']
        params = {'stage_id': stage_id}
        assert client.put(
            f"/api/documents/{project['id']}", params=params, json={'content': initial},
        ).status_code == 200
        assert client.post(f"/api/documents/{project['id']}/progress", params=params).status_code == 200
        assert client.put(
            f"/api/documents/{project['id']}", params=params, json={'content': current},
        ).status_code == 200
        assert client.put(
            f"/api/documents/{project['id']}", params=params, json={'content': stale},
        ).status_code == 200

        recorded = client.post(
            f"/api/documents/{project['id']}/progress", params=params, json={'content': current},
        )

        assert recorded.status_code == 200, recorded.text
        assert recorded.json()['progress']['entry']['added_symbols'] == 1
        assert client.get(
            f"/api/documents/{project['id']}", params=params,
        ).json()['content'] == current


def test_first_stage_creation_moves_project_document_and_keeps_text_list_available(tmp_path):
    app = create_app(RuntimeConfig(
        data_dir=tmp_path, platform='desktop', allow_local_files=True,
    ))
    content = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [
            {'type': 'text', 'text': 'Текст проекта'},
        ]}],
    }
    with TestClient(app) as client:
        project = client.post(
            '/api/projects',
            json={'name': 'Роман', 'goal': 1000, 'work_method': 'app'},
        ).json()
        assert client.put(
            f"/api/documents/{project['id']}", json={'content': content},
        ).status_code == 200

        created = client.post(
            f"/api/projects/{project['id']}/stages",
            json={'name': 'Первый этап', 'goal': 1000, 'work_method': 'app'},
        )
        assert created.status_code == 201, created.text

        staged_project = client.get(f"/api/projects/{project['id']}").json()
        stage = staged_project['stages'][0]
        stage_document = client.get(
            f"/api/documents/{project['id']}", params={'stage_id': stage['id']},
        )
        assert stage_document.status_code == 200
        assert stage_document.json()['content'] == content

        documents = client.get('/api/documents/list')
        assert documents.status_code == 200, documents.text
        assert [
            (item['project_id'], item['stage_id'], item['has_content'])
            for item in documents.json()
        ] == [(project['id'], stage['id'], True)]


def test_project_stage_conversion_keeps_project_document_editable(tmp_path):
    app = create_app(RuntimeConfig(
        data_dir=tmp_path, platform='desktop', allow_local_files=True,
    ))
    content = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [
            {'type': 'text', 'text': 'Текст при конверсии'},
        ]}],
    }
    with TestClient(app) as client:
        project = client.post(
            '/api/projects',
            json={
                'name': 'Конверсия',
                'goal': 1000,
                'total': 50,
                'work_method': 'app',
            },
        ).json()
        assert client.put(
            f"/api/documents/{project['id']}", json={'content': content},
        ).status_code == 200

        converted = client.patch(
            f"/api/projects/{project['id']}", json={'stages_enabled': True},
        )
        assert converted.status_code == 200, converted.text
        stage = client.get(f"/api/projects/{project['id']}").json()['stages'][0]
        assert stage['work_method'] == 'app'

        document = client.get(
            f"/api/documents/{project['id']}", params={'stage_id': stage['id']},
        )
        assert document.status_code == 200
        assert document.json()['content'] == content


def test_text_progress_runs_the_game_and_streak_pipeline(tmp_path):
    app = create_app(RuntimeConfig(data_dir=tmp_path, platform='desktop', allow_local_files=True))
    with TestClient(app) as client:
        assert client.patch(
            '/api/settings',
            json={'values': {'game_mode': True, 'global_streak': True}},
        ).status_code == 200
        project = client.post(
            '/api/projects',
            json={
                'name': 'Роман',
                'goal': 1000,
                'personal_goal': 1,
                'work_method': 'app',
            },
        ).json()
        content = {
            'type': 'doc',
            'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'Текст'}]}],
        }
        assert client.put(
            f"/api/documents/{project['id']}", json={'content': content},
        ).status_code == 200

        result = client.post(f"/api/documents/{project['id']}/progress")
        assert result.status_code == 200, result.text
        progress = result.json()['progress']
        assert progress['project']['streak_status'] == 'Start'
        assert progress['game']['result']['rewarded'] is True
        assert progress['game']['result']['streak_rewards'] == 2
        assert progress['game']['state']['profile']['experience'] > 0


def test_internal_document_progress_uses_the_application_day(tmp_path, monkeypatch):
    application_day = date(2026, 9, 1)
    monkeypatch.setattr('engine.today_for_test', lambda: application_day)
    app = create_app(RuntimeConfig(data_dir=tmp_path, platform='desktop', allow_local_files=True))
    with TestClient(app) as client:
        assert client.patch(
            '/api/settings',
            json={'values': {'game_mode': True, 'global_streak': True}},
        ).status_code == 200
        project = client.post(
            '/api/projects',
            json={
                'name': 'Тестовый стрик',
                'goal': 1000,
                'personal_goal': 1,
                'work_method': 'app',
            },
        ).json()
        content = {
            'type': 'doc',
            'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'Текст'}]}],
        }
        assert client.put(
            f"/api/documents/{project['id']}", json={'content': content},
        ).status_code == 200

        result = client.post(f"/api/documents/{project['id']}/progress")

        assert result.status_code == 200, result.text
        payload = result.json()['progress']
        assert payload['project']['streak_enabled'] is True
        assert payload['project']['streak_status'] == 'Start'
        assert payload['project']['added_today'] == len('Текст')
        assert payload['entry']['created_at'].startswith(application_day.isoformat())


def test_saved_document_timestamp_is_normalized_to_the_local_day():
    saved_at = datetime.fromisoformat('2026-08-31T20:00:00+00:00')

    parsed = ProjectDocumentService._parse_updated_at(saved_at.isoformat())

    assert parsed == saved_at.astimezone().replace(tzinfo=None)


def test_work_methods_keep_existing_data_and_gate_record_sources(tmp_path):
    app = create_app(RuntimeConfig(data_dir=tmp_path, platform='desktop', allow_local_files=True))
    content = {'type': 'doc', 'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'Текст'}]}]}
    with TestClient(app) as client:
        project = client.post('/api/projects', json={'name': 'Роман', 'goal': 1000}).json()
        assert project['work_method'] == 'manual'
        assert client.post(f"/api/projects/{project['id']}/progress", json={'new_total': 1}).status_code == 200
        assert client.put(f"/api/documents/{project['id']}", json={'content': content}).status_code == 422

        app_project = client.patch(
            f"/api/projects/{project['id']}", json={'work_method': 'app'},
        ).json()
        assert app_project['work_method'] == 'app'
        assert client.put(f"/api/documents/{project['id']}", json={'content': content}).status_code == 200
        assert client.post(f"/api/projects/{project['id']}/progress", json={'new_total': 20}).status_code == 422

        restored = client.patch(
            f"/api/projects/{project['id']}", json={'work_method': 'manual'},
        ).json()
        assert restored['work_method'] == 'manual'
        assert client.get(f"/api/documents/{project['id']}").status_code == 422


def test_document_list_excludes_documents_of_non_app_entities(tmp_path):
    app = create_app(RuntimeConfig(data_dir=tmp_path, platform='desktop'))
    content = {
        'type': 'doc',
        'content': [{'type': 'paragraph', 'content': [{'type': 'text', 'text': 'Текст'}]}],
    }
    with TestClient(app) as client:
        project = client.post(
            '/api/projects',
            json={'name': 'Роман', 'goal': 1000, 'work_method': 'app'},
        ).json()
        assert client.put(
            f"/api/documents/{project['id']}", json={'content': content},
        ).status_code == 200

        assert client.patch(
            f"/api/projects/{project['id']}", json={'work_method': 'manual'},
        ).status_code == 200
        assert client.get('/api/documents/list').json() == []

        assert client.patch(
            f"/api/projects/{project['id']}", json={'work_method': 'app'},
        ).status_code == 200
        assert [item['project_id'] for item in client.get('/api/documents/list').json()] == [project['id']]
