from __future__ import annotations

import os
import re
import threading
import base64
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
from pathlib import Path

import pytest
import jwt
from alembic import command
from alembic.config import Config as AlembicConfig
from fastapi.testclient import TestClient
from sqlalchemy import create_engine, inspect, text
from sqlalchemy.exc import IntegrityError
from sqlalchemy.orm import Session

from backend.app.cloud.email import RecordingEmailSender
from backend.app.cloud.models import (AuthRefreshToken, AuthSession, EmailVerificationToken,
                                      PasswordResetToken, User)
from backend.app.cloud.repositories import normalize_username
from backend.app.cloud.services import AccountService, AuthenticationError, AuthenticationService
from backend.app.cloud.legal_documents import (DEFAULT_LEGAL_DOCUMENT_REGISTRY,
                                               DocumentConfirmation,
                                               LegalAcceptanceService)
from backend.app.cloud.tokens import JWT_ALGORITHM, JWT_AUDIENCE, JWT_ISSUER, TokenService, utc_now
from backend.app.config import RuntimeConfig
from backend.app.main import create_app


ROOT = Path(__file__).resolve().parents[1]
AUTH_SECRET = 'test-only-auth-secret-with-at-least-32-characters'


def _database_url() -> str:
    return os.environ['NFPROGRESS_TEST_DATABASE_URL']


@pytest.fixture()
def migrated_database(monkeypatch):
    if not os.environ.get('NFPROGRESS_TEST_DATABASE_URL'):
        pytest.skip('requires dedicated real PostgreSQL database in NFPROGRESS_TEST_DATABASE_URL')
    url = _database_url()
    monkeypatch.setenv('NFPROGRESS_DATABASE_URL', url)
    monkeypatch.setenv('NFPROGRESS_ENV', 'test')
    engine = create_engine(url)
    with engine.begin() as connection:
        connection.execute(text('DROP TABLE IF EXISTS user_document_acceptances CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS admin_invitations CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS encrypted_blobs CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS encrypted_objects CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS user_crypto CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS sync_events CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS sync_devices CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS sync_user_state CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS cloud_projects CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS reserved_usernames CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS user_limit_overrides CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS global_limits CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS registration_settings CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS password_reset_tokens CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS email_verification_tokens CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS auth_refresh_tokens CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS auth_sessions CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS users CASCADE'))
        connection.execute(text('DROP TABLE IF EXISTS alembic_version CASCADE'))
    alembic_config = AlembicConfig(str(ROOT / 'alembic.ini'))
    command.upgrade(alembic_config, 'head')
    try:
        yield engine
    finally:
        with engine.begin() as connection:
            connection.execute(text('DROP TABLE IF EXISTS user_document_acceptances CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS admin_invitations CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS encrypted_blobs CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS encrypted_objects CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS user_crypto CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS sync_events CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS sync_devices CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS sync_user_state CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS cloud_projects CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS reserved_usernames CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS user_limit_overrides CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS global_limits CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS registration_settings CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS password_reset_tokens CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS email_verification_tokens CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS auth_refresh_tokens CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS auth_sessions CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS users CASCADE'))
            connection.execute(text('DROP TABLE IF EXISTS alembic_version CASCADE'))
        engine.dispose()


@pytest.fixture()
def cloud_client(migrated_database, tmp_path):
    app = create_app(RuntimeConfig(data_dir=tmp_path, environment='test', database_url=_database_url(),
        auth_secret=AUTH_SECRET, public_web_url='https://app.example.test'))
    app.state.email_sender = RecordingEmailSender()
    with TestClient(app) as client:
        yield client, migrated_database


def create_user(engine, *, username='Arthur', email='arthur@example.test', password='correct horse battery staple',
                status='active', accept_legal=True):
    with Session(engine) as session:
        user = AccountService().create_user(session, username=username, email=email, password=password, status=status)
        session.flush()
        if accept_legal and status == 'active' and inspect(engine).has_table('user_document_acceptances'):
            versions = DEFAULT_LEGAL_DOCUMENT_REGISTRY.current('ru')
            LegalAcceptanceService().record(
                session,
                user_id=user.id,
                confirmations=[DocumentConfirmation(
                    document_id=version.document_id,
                    revision=version.revision,
                    content_checksum=version.content_checksum,
                    language=version.language,
                ) for version in versions if version.mandatory],
                preferred_language='ru',
                method='authenticated_confirmation',
            )
        session.commit()
        session.refresh(user)
        return user.id


def login(client, username='Arthur', password='correct horse battery staple'):
    return client.post('/api/v1/auth/login', json={'username': username, 'password': password})


def accept_current_legal_documents(client, headers, language='ru'):
    status = client.get(f'/api/v1/legal/documents/status?language={language}', headers=headers)
    assert status.status_code == 200, status.text
    documents = [item for item in status.json()['documents'] if item['mandatory']]
    assert documents
    response = client.post('/api/v1/legal/documents/accept', headers=headers, json={
        'language': language,
        'documents': [{key: item[key] for key in (
            'document_id', 'revision', 'content_checksum', 'language',
        )} for item in documents],
    })
    assert response.status_code == 200, response.text
    assert response.json()['missing_required_document_ids'] == []
    return response


def _b64(value: bytes) -> str:
    return base64.urlsafe_b64encode(value).decode('ascii').rstrip('=')


def _crypto_provisioning_payload(marker: bytes = b'a') -> dict:
    return {
        'password': {
            'crypto_version': 1,
            'wrapping_version': 1,
            'kdf': {
                'kdf_version': 1,
                'algorithm': 'argon2id13',
                'salt': _b64(marker * 16),
                'opslimit': 2,
                'memlimit': 67_108_864,
            },
            'nonce': _b64(marker * 24),
            'ciphertext': _b64(marker * 48),
        },
        'recovery': {
            'crypto_version': 1,
            'wrapping_version': 1,
            'nonce': _b64(bytes([marker[0] + 1]) * 24),
            'ciphertext': _b64(bytes([marker[0] + 1]) * 48),
        },
    }


def _authorization(token: str) -> dict[str, str]:
    return {'Authorization': f'Bearer {token}'}


def test_current_user_crypto_is_auth_scoped_and_never_auto_provisions(cloud_client):
    client, engine = cloud_client
    first_id = create_user(engine, username='CryptoOne', email='crypto-one@example.test')
    create_user(engine, username='CryptoTwo', email='crypto-two@example.test')
    first_token = login(client, 'CryptoOne').json()['access_token']
    second_token = login(client, 'CryptoTwo').json()['access_token']

    with engine.begin() as connection:
        connection.execute(text("""INSERT INTO user_crypto(
            user_id,password_crypto_version,password_wrapping_version,kdf_version,
            kdf_algorithm,kdf_salt,kdf_opslimit,kdf_memlimit,password_nonce,
            password_wrapped_amk,recovery_crypto_version,recovery_wrapping_version,
            recovery_nonce,recovery_wrapped_amk
        ) VALUES(:user_id,1,1,1,'argon2id13',:salt,2,67108864,:nonce,:wrapped,NULL,NULL,NULL,NULL)"""), {
            'user_id': first_id, 'salt': b'a' * 16, 'nonce': b'b' * 24, 'wrapped': b'c' * 48,
        })

    first = client.get('/api/v1/account/crypto', headers={'Authorization': f'Bearer {first_token}'})
    assert first.status_code == 200
    assert first.headers['cache-control'] == 'private, no-store'
    assert first.json() == {
        'provisioned': True,
        'password': {
            'crypto_version': 1,
            'wrapping_version': 1,
            'kdf': {
                'kdf_version': 1,
                'algorithm': 'argon2id13',
                'salt': _b64(b'a' * 16),
                'opslimit': 2,
                'memlimit': 67_108_864,
            },
            'nonce': _b64(b'b' * 24),
            'ciphertext': _b64(b'c' * 48),
        },
        'recovery': None,
    }
    serialized = first.text.lower()
    assert not any(secret in serialized for secret in ('masterkey', 'master_key', 'plaintext', 'object_key'))

    second = client.get('/api/v1/account/crypto', headers={'Authorization': f'Bearer {second_token}'})
    assert second.status_code == 200
    assert second.json() == {'provisioned': False, 'password': None, 'recovery': None}
    with engine.connect() as connection:
        assert connection.execute(text('SELECT count(*) FROM user_crypto')).scalar_one() == 1

    assert client.get('/api/v1/account/crypto').status_code == 401


def test_initial_crypto_provisioning_is_authenticated_immutable_and_recovery_complete(cloud_client):
    client, engine = cloud_client
    first_id = create_user(engine, username='ProvisionOne', email='provision-one@example.test')
    create_user(engine, username='ProvisionTwo', email='provision-two@example.test')
    first_token = login(client, 'ProvisionOne').json()['access_token']
    second_token = login(client, 'ProvisionTwo').json()['access_token']
    payload = _crypto_provisioning_payload(b'p')

    assert client.post('/api/v1/account/crypto', json=payload).status_code == 401
    created = client.post('/api/v1/account/crypto', json=payload, headers=_authorization(first_token))
    assert created.status_code == 201
    assert created.headers['cache-control'] == 'private, no-store'
    assert created.json() == client.get('/api/v1/account/crypto', headers=_authorization(first_token)).json()
    assert created.json()['recovery'] == payload['recovery']
    assert client.get('/api/v1/account/crypto', headers=_authorization(second_token)).json() == {
        'provisioned': False, 'password': None, 'recovery': None,
    }

    replay = client.post('/api/v1/account/crypto', json=payload, headers=_authorization(first_token))
    assert replay.status_code == 200 and replay.json() == created.json()
    conflict = client.post('/api/v1/account/crypto', json=_crypto_provisioning_payload(b'q'), headers=_authorization(first_token))
    assert conflict.status_code == 409
    assert conflict.json() == {'detail': {
        'code': 'crypto_already_provisioned',
        'message': 'Account encryption is already provisioned.',
    }}
    with engine.connect() as connection:
        row = connection.execute(text('''SELECT password_wrapped_amk,recovery_wrapped_amk
            FROM user_crypto WHERE user_id=:user_id'''), {'user_id': first_id}).one()
    assert row.password_wrapped_amk == b'p' * 48
    assert row.recovery_wrapped_amk == b'q' * 48
    assert b'plaintext' not in row.password_wrapped_amk + row.recovery_wrapped_amk


@pytest.mark.parametrize('mutate', [
    lambda payload: payload.pop('recovery'),
    lambda payload: payload.update({'unexpected': True}),
    lambda payload: payload['password']['kdf'].update({'opslimit': 3}),
    lambda payload: payload['password'].update({'nonce': _b64(b'n' * 25)}),
    lambda payload: payload['password']['kdf'].update({'salt': _b64(b's' * 16) + '='}),
])
def test_initial_crypto_provisioning_rejects_incomplete_or_noncanonical_records(cloud_client, mutate):
    client, engine = cloud_client
    create_user(engine, username='ProvisionValidation', email='provision-validation@example.test')
    token = login(client, 'ProvisionValidation').json()['access_token']
    payload = _crypto_provisioning_payload(b'v')
    mutate(payload)
    response = client.post('/api/v1/account/crypto', json=payload, headers=_authorization(token))
    assert response.status_code == 422
    with engine.connect() as connection:
        assert connection.execute(text('SELECT count(*) FROM user_crypto')).scalar_one() == 0


def test_initial_crypto_provisioning_concurrent_create_uses_database_identity_constraint(cloud_client):
    client, engine = cloud_client
    create_user(engine, username='ProvisionRaceSame', email='provision-race-same@example.test')
    same_token = login(client, 'ProvisionRaceSame').json()['access_token']
    same_payload = _crypto_provisioning_payload(b'r')
    app = client.app
    barrier = threading.Barrier(2)

    def create_in_isolated_client(payload: dict) -> int:
        with TestClient(app) as isolated:
            barrier.wait()
            return isolated.post('/api/v1/account/crypto', json=payload, headers=_authorization(same_token)).status_code

    with ThreadPoolExecutor(max_workers=2) as executor:
        same_statuses = list(executor.map(create_in_isolated_client, [same_payload, same_payload]))
    assert sorted(same_statuses) == [200, 201]

    create_user(engine, username='ProvisionRaceDifferent', email='provision-race-different@example.test')
    different_token = login(client, 'ProvisionRaceDifferent').json()['access_token']
    different_barrier = threading.Barrier(2)

    def create_different(payload: dict) -> int:
        with TestClient(app) as isolated:
            different_barrier.wait()
            return isolated.post('/api/v1/account/crypto', json=payload, headers=_authorization(different_token)).status_code

    with ThreadPoolExecutor(max_workers=2) as executor:
        different_statuses = list(executor.map(create_different, [
            _crypto_provisioning_payload(b'x'), _crypto_provisioning_payload(b'y'),
        ]))
    assert sorted(different_statuses) == [201, 409]


def test_c2_schema_and_repeated_head_upgrade(migrated_database, monkeypatch):
    assert set(inspect(migrated_database).get_table_names()) == {
        'alembic_version', 'users', 'auth_sessions', 'auth_refresh_tokens',
        'email_verification_tokens', 'password_reset_tokens', 'registration_settings',
        'global_limits', 'user_limit_overrides', 'reserved_usernames', 'cloud_projects', 'admin_invitations',
        'user_document_acceptances',
        'sync_user_state', 'sync_devices', 'sync_events', 'user_crypto', 'encrypted_objects', 'encrypted_blobs',
    }
    assert inspect(migrated_database).get_columns('users')[0]['name'] == 'id'
    monkeypatch.setenv('NFPROGRESS_DATABASE_URL', _database_url())
    command.upgrade(AlembicConfig(str(ROOT / 'alembic.ini')), 'head')
    with migrated_database.connect() as connection:
        assert connection.execute(text('SELECT version_num FROM alembic_version')).scalar_one() == 'cloud_project_retirement'
    command.downgrade(AlembicConfig(str(ROOT / 'alembic.ini')), 'c2_account_auth_core')
    assert 'email_verification_tokens' not in inspect(migrated_database).get_table_names()
    assert 'password_reset_tokens' not in inspect(migrated_database).get_table_names()
    assert 'registration_settings' not in inspect(migrated_database).get_table_names()
    command.upgrade(AlembicConfig(str(ROOT / 'alembic.ini')), 'head')


def test_user_normalization_password_hash_and_unique_constraints(migrated_database):
    user_id = create_user(migrated_database)
    with Session(migrated_database) as session:
        user = session.get(User, user_id)
        assert normalize_username(' Arthur ') == 'arthur'
        assert user.password_hash.startswith('$argon2id$')
        assert 'correct horse battery staple' not in user.password_hash
        AccountService().create_user(session, username='arthur', email='another@example.test', password='x' * 15, status='active')
        with pytest.raises(IntegrityError):
            session.commit()

    with Session(migrated_database) as session:
        AccountService().create_user(session, username='Different', email='ARTHUR@example.test', password='x' * 15, status='active')
        with pytest.raises(IntegrityError):
            session.commit()


def test_login_account_access_logout_and_ownership(cloud_client):
    client, engine = cloud_client
    first_id = create_user(engine)
    second_id = create_user(engine, username='Other', email='other@example.test')
    response = login(client, 'ARTHUR')
    assert response.status_code == 200
    tokens = response.json()
    assert tokens['token_type'] == 'bearer' and tokens['access_expires_in'] == 900
    me = client.get('/api/v1/account/me', headers={'Authorization': f"Bearer {tokens['access_token']}"})
    assert me.status_code == 200 and me.json()['id'] == str(first_id)
    assert me.json()['id'] != str(second_id)
    assert 'password_hash' not in me.json()
    logout = client.post('/api/v1/auth/logout', headers={'Authorization': f"Bearer {tokens['access_token']}"})
    assert logout.status_code == 204
    assert client.get('/api/v1/account/me', headers={'Authorization': f"Bearer {tokens['access_token']}"}).status_code == 401
    assert client.post('/api/v1/auth/refresh', json={'refresh_token': tokens['refresh_token']}).status_code == 401


@pytest.mark.parametrize('status', ['pending', 'rejected', 'blocked'])
def test_non_active_users_cannot_login(cloud_client, status):
    client, engine = cloud_client
    create_user(engine, status=status)
    response = login(client)
    assert response.status_code == 401
    assert response.json()['detail']['code'] == 'invalid_credentials'


def test_bad_credentials_have_same_contract(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    wrong_password = login(client, password='wrong password that is long enough')
    unknown = login(client, username='Nobody', password='wrong password that is long enough')
    assert wrong_password.status_code == unknown.status_code == 401
    assert wrong_password.json() == unknown.json()


def test_refresh_rotation_replay_revokes_session(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    first = login(client).json()
    rotated = client.post('/api/v1/auth/refresh', json={'refresh_token': first['refresh_token']})
    assert rotated.status_code == 200
    second = rotated.json()
    assert second['refresh_token'] != first['refresh_token']
    with Session(engine) as session:
        parsed = TokenService.parse_refresh_token(first['refresh_token'])
        assert parsed is not None
        token_id, secret = parsed
        row = session.get(AuthRefreshToken, token_id)
        assert row is not None
        stored_hashes = session.execute(text('SELECT token_hash FROM auth_refresh_tokens')).scalars().all()
        assert first['refresh_token'] not in stored_hashes
        assert secret not in stored_hashes
        assert re.fullmatch(r'[0-9a-f]{64}', row.token_hash)
        token_service = TokenService(AUTH_SECRET)
        assert token_service.verify_refresh_secret(secret, row.token_hash)
        assert token_service.hash_refresh_secret(secret) == row.token_hash
    replay = client.post('/api/v1/auth/refresh', json={'refresh_token': first['refresh_token']})
    assert replay.status_code == 401
    assert client.get('/api/v1/account/me', headers={'Authorization': f"Bearer {second['access_token']}"}).status_code == 401
    assert client.post('/api/v1/auth/refresh', json={'refresh_token': second['refresh_token']}).status_code == 401


def test_expired_refresh_does_not_rotate_or_extend_session(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    issued = login(client).json()
    parsed = TokenService.parse_refresh_token(issued['refresh_token'])
    assert parsed is not None
    token_id, _secret = parsed
    with Session(engine) as session:
        refresh = session.get(AuthRefreshToken, token_id)
        assert refresh is not None
        auth_session = session.get(AuthSession, refresh.session_id)
        assert auth_session is not None
        original_session_expiry = auth_session.expires_at
        refresh.expires_at = utc_now() - timedelta(seconds=1)
        session.commit()

    response = client.post('/api/v1/auth/refresh', json={'refresh_token': issued['refresh_token']})
    assert response.status_code == 401
    assert 'access_token' not in response.json()
    assert 'refresh_token' not in response.json()
    with Session(engine) as session:
        refresh = session.get(AuthRefreshToken, token_id)
        auth_session = session.get(AuthSession, refresh.session_id)
        assert auth_session.expires_at == original_session_expiry
        assert session.query(AuthRefreshToken).count() == 1


def test_concurrent_refresh_allows_one_rotation_then_revokes_replay_session(migrated_database, tmp_path):
    app = create_app(RuntimeConfig(
        data_dir=tmp_path,
        environment='test',
        database_url=_database_url(),
        auth_secret=AUTH_SECRET,
    ))
    with TestClient(app) as client:
        create_user(migrated_database)
        issued = login(client).json()
    barrier = threading.Barrier(2)

    def refresh_in_isolated_session() -> tuple[str, str | None]:
        with Session(migrated_database) as session:
            service = AuthenticationService(TokenService(AUTH_SECRET))
            barrier.wait(timeout=10)
            try:
                result = service.refresh(session, issued['refresh_token'])
            except AuthenticationError:
                return 'replay', None
            return 'rotated', result.refresh_token

    with ThreadPoolExecutor(max_workers=2) as executor:
        outcomes = list(executor.map(lambda _index: refresh_in_isolated_session(), range(2)))

    assert [status for status, _token in outcomes].count('rotated') == 1
    assert [status for status, _token in outcomes].count('replay') == 1
    replacement = next(token for status, token in outcomes if status == 'rotated')
    assert replacement is not None
    with Session(migrated_database) as session:
        auth_session = session.query(AuthSession).one()
        refresh_tokens = session.query(AuthRefreshToken).all()
        assert auth_session.revoked_at is not None
        assert len(refresh_tokens) == 2
        assert sum(token.replaced_by_id is not None for token in refresh_tokens) == 1
        assert all(token.revoked_at is not None for token in refresh_tokens)


def test_access_token_validation_and_blocked_user(cloud_client):
    client, engine = cloud_client
    user_id = create_user(engine)
    tokens = login(client).json()
    token_service = TokenService(AUTH_SECRET)
    with Session(engine) as session:
        auth_session = session.query(AuthSession).one()
        expired = token_service.issue_access_token(user_id, auth_session.id, utc_now() - timedelta(hours=1))
    assert client.get('/api/v1/account/me', headers={'Authorization': f'Bearer {expired}'}).status_code == 401
    wrong_signature = TokenService('another-secret-that-is-definitely-long-enough').issue_access_token(user_id, auth_session.id)
    assert client.get('/api/v1/account/me', headers={'Authorization': f'Bearer {wrong_signature}'}).status_code == 401
    with Session(engine) as session:
        session.get(User, user_id).status = 'blocked'
        session.commit()
    assert client.get('/api/v1/account/me', headers={'Authorization': f"Bearer {tokens['access_token']}"}).status_code == 401


def test_access_jwt_requires_expected_claims_issuer_audience_and_algorithm(cloud_client):
    client, engine = cloud_client
    create_user(engine)
    tokens = login(client).json()
    claims = jwt.decode(tokens['access_token'], AUTH_SECRET, algorithms=[JWT_ALGORITHM], audience=JWT_AUDIENCE)
    invalid_payloads = [
        {key: value for key, value in claims.items() if key != 'jti'},
        {**claims, 'iss': 'wrong-issuer'},
        {**claims, 'aud': 'wrong-audience'},
    ]
    for payload in invalid_payloads:
        forged = jwt.encode(payload, AUTH_SECRET, algorithm=JWT_ALGORITHM)
        assert client.get('/api/v1/account/me', headers={'Authorization': f'Bearer {forged}'}).status_code == 401
    unsigned = jwt.encode(claims, key='', algorithm='none')
    assert client.get('/api/v1/account/me', headers={'Authorization': f'Bearer {unsigned}'}).status_code == 401
