"""Opt-in local PostgreSQL/blob restore drill; never targets a configured server."""
from __future__ import annotations

import hashlib
import json
import os
import secrets
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest
from alembic import command
from alembic.config import Config as AlembicConfig
from fastapi.testclient import TestClient
from sqlalchemy import create_engine, text
from sqlalchemy.engine import URL
from sqlalchemy.orm import Session

from backend.app.config import RuntimeConfig
from backend.app.cloud.legal_documents import DEFAULT_LEGAL_DOCUMENT_REGISTRY
from backend.app.cloud.models import EncryptedObject, SyncEvent, UserDocumentAcceptance
from backend.app.main import create_app
from backend.app.cloud.tokens import utc_now
from test_cloud_auth import AUTH_SECRET, create_user, login


ROOT = Path(__file__).resolve().parents[1]


def _docker(container: str, *arguments: str, input_data: bytes | None = None,
            output=None) -> None:
    result = subprocess.run(['docker', 'exec', '-i', container, *arguments],
        input=input_data, stdout=output or subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, check=False, timeout=60)
    assert result.returncode == 0, f'Local Docker PostgreSQL command failed ({arguments[0]}).'


def _guarded_port(container: str) -> int:
    assert container.startswith('worta-mvp607-')
    result = subprocess.run(['docker', 'inspect', container], capture_output=True,
        check=True, timeout=10)
    info = json.loads(result.stdout)[0]
    assert info['Config']['Labels'].get('worta.restore-test') == '1'
    assert info['Config']['Image'] == 'postgres:16'
    ports = info['NetworkSettings']['Ports']['5432/tcp']
    assert len(ports) == 1 and ports[0]['HostIp'] == '127.0.0.1'
    return int(ports[0]['HostPort'])


def _config(url: str, data_dir: Path, blob_dir: Path) -> RuntimeConfig:
    return RuntimeConfig(environment='production', database_url=url,
        auth_secret=AUTH_SECRET, data_dir=data_dir, cloud_blob_dir=blob_dir,
        allowed_origins=('https://test.worta.ru',),
        public_web_url='https://test.worta.ru')


def _digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


@pytest.mark.skipif(not os.environ.get('NFPROGRESS_RESTORE_TEST_CONTAINER'),
                    reason='opt-in local labeled PostgreSQL Docker container required')
def test_disposable_postgresql_and_encrypted_blob_backup_restore(tmp_path, monkeypatch):
    container = os.environ['NFPROGRESS_RESTORE_TEST_CONTAINER']
    port = _guarded_port(container)
    suffix = uuid4().hex[:12]
    role = f'worta_mvp607_{suffix}'
    source_db = f'worta_mvp607_src_{suffix}'
    target_db = f'worta_mvp607_dst_{suffix}'
    password = secrets.token_urlsafe(24)
    source_url = URL.create('postgresql+psycopg', username=role, password=password,
        host='127.0.0.1', port=port, database=source_db).render_as_string(hide_password=False)
    target_url = URL.create('postgresql+psycopg', username=role, password=password,
        host='127.0.0.1', port=port, database=target_db).render_as_string(hide_password=False)
    source_engine = None
    try:
        _docker(container, 'psql', '-U', 'postgres', '-d', 'postgres', '-v', 'ON_ERROR_STOP=1',
            input_data=(f'CREATE ROLE "{role}" LOGIN PASSWORD \'{password}\' '
                'NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION;\n'
                f'CREATE DATABASE "{source_db}" OWNER "{role}";\n'
                f'CREATE DATABASE "{target_db}" OWNER "{role}";\n').encode())
        monkeypatch.setenv('NFPROGRESS_DATABASE_URL', source_url)
        command.upgrade(AlembicConfig(str(ROOT / 'alembic.ini')), 'head')
        source_engine = create_engine(source_url)
        user_id = create_user(source_engine)
        version = next(item for item in DEFAULT_LEGAL_DOCUMENT_REGISTRY.current('ru')
                       if item.document_type == 'beta_terms')
        with Session(source_engine) as session:
            event_id = uuid4()
            session.add(SyncEvent(
                user_id=user_id, event_id=event_id, device_id=uuid4(),
                project_id='restore-project', entity_id='restore-note', entity_type='note',
                operation='upsert', revision=1, updated_at=utc_now(), server_sequence=1,
            ))
            session.flush()
            session.add(EncryptedObject(
                user_id=user_id, event_id=event_id, crypto_version=1, aad_version=1,
                nonce=b'n' * 24, ciphertext=b'opaque test ciphertext for restore',
            ))
            acceptance = session.query(UserDocumentAcceptance).filter_by(
                user_id=user_id, document_id=version.document_id,
                revision=version.revision, content_checksum=version.content_checksum,
                language=version.language,
            ).one()
            acceptance_snapshot = (
                acceptance.document_type, acceptance.document_id, acceptance.revision,
                acceptance.content_checksum, acceptance.language,
                acceptance.acceptance_method, acceptance.accepted_at,
            )
            session.commit()
        with source_engine.connect() as connection:
            encrypted_snapshot = connection.execute(text(
                'SELECT user_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at '
                'FROM encrypted_objects WHERE user_id=:user_id'), {'user_id': user_id}).one()
        source_blobs = tmp_path / 'source-blobs'
        source_app = create_app(_config(source_url, tmp_path / 'source-data', source_blobs))
        blob_id = uuid4()
        ciphertext = secrets.token_bytes(48)  # Opaque synthetic fixture, not user content.
        route = f'/api/v1/cloud/projects/restore/covers/{blob_id}'
        with TestClient(source_app) as client:
            token = login(client).json()['access_token']
            headers = {'Authorization': f'Bearer {token}'}
            assert client.post('/api/v1/cloud/projects/restore', headers=headers).status_code == 200
            upload_headers = {**headers, 'Content-Type': 'application/octet-stream',
                'X-WORTA-Crypto-Version': '1', 'X-WORTA-AAD-Version': '1',
                'X-WORTA-Nonce': 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'}
            assert client.put(route, content=ciphertext, headers=upload_headers).status_code == 200
            assert client.get(route, headers=headers).content == ciphertext

        # No writer runs between the database dump and filesystem snapshot.
        archive = tmp_path / 'database.dump'
        with archive.open('wb') as output:
            _docker(container, 'pg_dump', '-U', role, '-d', source_db, '-Fc', output=output)
        assert archive.stat().st_size > 0
        archive_digest = _digest(archive)
        backup_blobs = tmp_path / 'backup-blobs'
        shutil.copytree(source_blobs, backup_blobs)
        relative_blob = Path(user_id.hex) / f'{blob_id.hex}.blob'
        assert _digest(source_blobs / relative_blob) == _digest(backup_blobs / relative_blob)

        with archive.open('rb') as source:
            _docker(container, 'pg_restore', '-U', role, '-d', target_db,
                '--no-owner', '--no-acl', input_data=source.read())
        assert _digest(archive) == archive_digest
        restored_blobs = tmp_path / 'restored-blobs'
        shutil.copytree(backup_blobs, restored_blobs)
        assert _digest(restored_blobs / relative_blob) == _digest(backup_blobs / relative_blob)

        restored_engine = create_engine(target_url)
        try:
            with restored_engine.connect() as connection:
                assert connection.scalar(text('SELECT count(*) FROM users')) == 1
                assert connection.scalar(text('SELECT id FROM users')) == user_id
                assert connection.scalar(text('SELECT count(*) FROM encrypted_blobs')) == 1
                assert connection.scalar(text('SELECT count(*) FROM encrypted_objects')) == 1
                assert connection.scalar(text('SELECT version_num FROM alembic_version')) == 'cloud_project_retirement'
                restored_encrypted = connection.execute(text(
                    'SELECT user_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at '
                    'FROM encrypted_objects WHERE user_id=:user_id'), {'user_id': user_id}).one()
                assert restored_encrypted == encrypted_snapshot
            with Session(restored_engine) as session:
                restored_acceptance = session.query(UserDocumentAcceptance).filter_by(
                    user_id=user_id, document_id=version.document_id,
                    revision=version.revision, content_checksum=version.content_checksum,
                    language=version.language,
                ).one()
                assert (
                    restored_acceptance.document_type, restored_acceptance.document_id,
                    restored_acceptance.revision, restored_acceptance.content_checksum,
                    restored_acceptance.language, restored_acceptance.acceptance_method,
                    restored_acceptance.accepted_at,
                ) == acceptance_snapshot
            with restored_engine.connect() as connection:
                try:
                    connection.execute(text(
                        "UPDATE user_document_acceptances SET language='en' WHERE user_id=:user_id"
                    ), {'user_id': user_id})
                except Exception as error:
                    assert 'append-only' in str(error)
                    connection.rollback()
                else:
                    raise AssertionError('Restored legal acceptance was mutable.')
            with restored_engine.connect() as connection:
                try:
                    connection.execute(text(
                        'DELETE FROM user_document_acceptances WHERE user_id=:user_id'
                    ), {'user_id': user_id})
                except Exception as error:
                    assert 'append-only' in str(error)
                    connection.rollback()
                else:
                    raise AssertionError('Restored legal acceptance was deletable.')
        finally:
            restored_engine.dispose()

        restored_app = create_app(_config(target_url, tmp_path / 'restored-data', restored_blobs))
        with TestClient(restored_app) as client:
            assert client.get('/ready').status_code == 200
            # Cloud-only settings are a static compatibility response. They
            # accept no edits and must not expose the plaintext workspace APIs.
            settings = client.get('/api/settings').json()
            assert settings['editable_keys'] == []
            assert client.patch('/api/settings', json={'language': 'en'}).json() == settings
            paths = client.get('/openapi.json').json()['paths']
            assert all(not path.startswith(('/api/projects', '/api/documents', '/api/notes')) for path in paths)
            assert client.get('/api/v1/auth/registration').json()['mode'] == 'closed'
            assert client.post('/api/v1/auth/register', json={
                'username': 'Blocked', 'email': 'blocked@example.com',
                'password': 'test-only-long-password',
            }).status_code == 403
            assert client.get('/api/v1/admin/users').status_code == 401
            token = login(client).json()['access_token']
            headers = {'Authorization': f'Bearer {token}'}
            assert client.get(route, headers=headers).content == ciphertext
            target_blob = restored_blobs / relative_blob
            target_blob.unlink()
            missing = client.get(route, headers=headers)
            assert missing.status_code == 503
            assert missing.json()['detail']['code'] == 'encrypted_blob_unavailable'
            target_blob.write_bytes(b'x' * len(ciphertext))
            corrupt = client.get(route, headers=headers)
            assert corrupt.status_code == 503
            assert corrupt.json()['detail']['code'] == 'encrypted_blob_corrupt'
            shutil.copy2(backup_blobs / relative_blob, target_blob)
            assert client.get(route, headers=headers).content == ciphertext
    finally:
        if source_engine is not None:
            source_engine.dispose()
        _docker(container, 'psql', '-U', 'postgres', '-d', 'postgres', '-v', 'ON_ERROR_STOP=1',
            input_data=(f'DROP DATABASE IF EXISTS "{target_db}" WITH (FORCE);\n'
                f'DROP DATABASE IF EXISTS "{source_db}" WITH (FORCE);\n'
                f'DROP ROLE IF EXISTS "{role}";\n').encode())
