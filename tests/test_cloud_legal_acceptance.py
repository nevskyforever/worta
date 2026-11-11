from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor

from sqlalchemy import func, select, text
from sqlalchemy.orm import Session

from backend.app.cloud.legal_documents import (BETA_WARNING_RU,
                                               DEFAULT_LEGAL_DOCUMENTS,
                                               LegalAcceptanceService,
                                               LegalDocumentError,
                                               LegalDocumentRegistry,
                                               LegalDocumentVersion)
from backend.app.cloud.models import AdminInvitation, User, UserDocumentAcceptance
from test_cloud_auth import cloud_client, create_user, login, migrated_database
from test_cloud_invitations import _accept_payload, _admin, _issued_token


def _headers(client, username: str, password: str = 'correct horse battery staple') -> dict[str, str]:
    token = login(client, username, password).json()['access_token']
    return {'Authorization': f'Bearer {token}'}


def _second_registry() -> LegalDocumentRegistry:
    return LegalDocumentRegistry((*DEFAULT_LEGAL_DOCUMENTS, LegalDocumentVersion(
        document_id='wotra.beta_terms',
        document_type='beta_terms',
        revision='private-beta-warning-2',
        language='ru',
        title='Закрытое тестирование worta',
        content=f'{BETA_WARNING_RU}\n\nРедакция для проверки повторного подтверждения.',
        mandatory=True,
        requires_explicit_acceptance=True,
        approved=True,
    )))


def test_invitation_requires_exact_documents_and_records_server_evidence(cloud_client):
    client, engine = cloud_client
    headers = _admin(client, engine)
    assert client.post('/api/v1/admin/invitations', headers=headers, json={
        'username': 'LegalInvite', 'email': 'legal-invite@example.com',
    }).status_code == 201
    token = _issued_token(client)
    documents = client.post('/api/v1/auth/invitations/documents', json={
        'token': token, 'language': 'en',
    }).json()['documents']
    assert [item['document_type'] for item in documents] == ['beta_terms']
    assert documents[0]['language'] == 'ru'
    assert documents[0]['title'] == 'Закрытое тестирование worta'
    assert documents[0]['content'] == BETA_WARNING_RU

    missing = client.post('/api/v1/auth/invitations/accept', json={
        'token': token, 'password': 'new private password long', 'language': 'en', 'documents': [],
    })
    assert missing.status_code == 422
    payload = _accept_payload(client, token)
    payload['documents'][0]['content_checksum'] = '0' * 64
    tampered = client.post('/api/v1/auth/invitations/accept', json=payload)
    assert tampered.status_code == 409
    assert tampered.json()['detail']['code'] == 'legal_document_version_changed'

    accepted = client.post('/api/v1/auth/invitations/accept', json=_accept_payload(client, token))
    assert accepted.status_code == 200
    with Session(engine) as session:
        user = session.scalar(select(User).where(User.username_normalized == 'legalinvite'))
        evidence = session.scalars(select(UserDocumentAcceptance).where(
            UserDocumentAcceptance.user_id == user.id)).all()
        assert len(evidence) == 1
        assert evidence[0].document_type == 'beta_terms'
        assert evidence[0].revision == documents[0]['revision']
        assert evidence[0].content_checksum == documents[0]['content_checksum']
        assert evidence[0].language == 'ru'
        assert evidence[0].acceptance_method == 'invitation'
        assert evidence[0].accepted_at is not None


def test_old_account_is_gated_then_new_revision_is_append_only(cloud_client):
    client, engine = cloud_client
    user_id = create_user(
        engine, username='LegacyLegal', email='legacy-legal@example.test', accept_legal=False,
    )
    headers = _headers(client, 'LegacyLegal')
    status = client.get('/api/v1/legal/documents/status?language=ru', headers=headers)
    assert status.status_code == 200
    assert status.json()['missing_required_document_ids'] == ['wotra.beta_terms']
    assert client.get('/api/v1/account/crypto', headers=headers).status_code == 451
    assert client.post('/api/v1/auth/logout', headers=headers).status_code == 204

    headers = _headers(client, 'LegacyLegal')
    document = status.json()['documents'][0]
    confirmation = {key: document[key] for key in (
        'document_id', 'revision', 'content_checksum', 'language',
    )}
    first = client.post('/api/v1/legal/documents/accept', headers=headers, json={
        'language': 'ru', 'documents': [confirmation],
    })
    assert first.status_code == 200 and first.json()['missing_required_document_ids'] == []
    duplicate = client.post('/api/v1/legal/documents/accept', headers=headers, json={
        'language': 'ru', 'documents': [confirmation],
    })
    assert duplicate.status_code == 200
    with Session(engine) as session:
        assert session.scalar(select(func.count()).select_from(UserDocumentAcceptance).where(
            UserDocumentAcceptance.user_id == user_id)) == 1

    client.app.state.legal_document_registry = _second_registry()
    changed = client.get('/api/v1/legal/documents/status?language=ru', headers=headers).json()
    assert changed['documents'][0]['revision'] == 'private-beta-warning-2'
    assert changed['missing_required_document_ids'] == ['wotra.beta_terms']
    blocked = client.get('/api/v1/account/crypto', headers=headers)
    assert blocked.status_code == 451 and blocked.json()['detail']['code'] == 'legal_acceptance_required'
    second_document = changed['documents'][0]
    second_confirmation = {key: second_document[key] for key in (
        'document_id', 'revision', 'content_checksum', 'language',
    )}
    assert client.post('/api/v1/legal/documents/accept', headers=headers, json={
        'language': 'ru', 'documents': [second_confirmation],
    }).status_code == 200
    with Session(engine) as session:
        rows = session.scalars(select(UserDocumentAcceptance).where(
            UserDocumentAcceptance.user_id == user_id).order_by(
                UserDocumentAcceptance.accepted_at)).all()
        assert [row.revision for row in rows] == [
            'private-beta-warning-brand-2', 'private-beta-warning-2',
        ]


def test_current_revision_acceptance_survives_interface_language_change(cloud_client):
    client, engine = cloud_client
    create_user(engine, username='LocalizedLegal', email='localized-legal@example.test')
    client.app.state.legal_document_registry = LegalDocumentRegistry((
        *DEFAULT_LEGAL_DOCUMENTS,
        LegalDocumentVersion(
            document_id='wotra.beta_terms',
            document_type='beta_terms',
            revision='private-beta-warning-brand-2',
            language='en',
            title='Private beta fixture translation',
            content='Test-only approved translation fixture.',
            mandatory=True,
            requires_explicit_acceptance=True,
            approved=True,
        ),
    ))
    headers = _headers(client, 'LocalizedLegal')
    status = client.get('/api/v1/legal/documents/status?language=en', headers=headers)
    assert status.status_code == 200
    assert status.json()['documents'][0]['language'] == 'en'
    assert status.json()['documents'][0]['accepted'] is True
    assert status.json()['missing_required_document_ids'] == []
    assert client.get('/api/v1/account/crypto', headers=headers).status_code == 200


def test_invitation_activation_rolls_back_when_evidence_write_fails(cloud_client, monkeypatch):
    client, engine = cloud_client
    headers = _admin(client, engine)
    assert client.post('/api/v1/admin/invitations', headers=headers, json={
        'username': 'AtomicLegal', 'email': 'atomic-legal@example.com',
    }).status_code == 201
    token = _issued_token(client)
    payload = _accept_payload(client, token)

    def fail_record(*_args, **_kwargs):
        raise LegalDocumentError('evidence_write_failed', 503)

    monkeypatch.setattr(LegalAcceptanceService, 'record', fail_record)
    response = client.post('/api/v1/auth/invitations/accept', json=payload)
    assert response.status_code == 503
    with Session(engine) as session:
        user = session.scalar(select(User).where(User.username_normalized == 'atomiclegal'))
        invitation = session.scalar(select(AdminInvitation).where(AdminInvitation.user_id == user.id))
        assert user.status == 'pending' and not user.email_verified
        assert invitation.used_at is None
        assert session.scalar(select(func.count()).select_from(UserDocumentAcceptance).where(
            UserDocumentAcceptance.user_id == user.id)) == 0


def test_append_only_trigger_rejects_mutation(cloud_client):
    _client, engine = cloud_client
    user_id = create_user(engine, username='AppendOnlyLegal', email='append-only@example.test')
    with engine.begin() as connection:
        try:
            connection.execute(text(
                "UPDATE user_document_acceptances SET language='en' WHERE user_id=:user_id"
            ), {'user_id': user_id})
        except Exception as error:
            assert 'append-only' in str(error)
        else:
            raise AssertionError('Append-only legal evidence was mutable.')
    with engine.begin() as connection:
        try:
            connection.execute(text(
                'DELETE FROM user_document_acceptances WHERE user_id=:user_id'
            ), {'user_id': user_id})
        except Exception as error:
            assert 'append-only' in str(error)
        else:
            raise AssertionError('Append-only legal evidence was deletable directly.')


def test_concurrent_invitation_activation_has_one_winner(cloud_client):
    client, engine = cloud_client
    headers = _admin(client, engine)
    assert client.post('/api/v1/admin/invitations', headers=headers, json={
        'username': 'ConcurrentLegal', 'email': 'concurrent-legal@example.com',
    }).status_code == 201
    token = _issued_token(client)
    payload = _accept_payload(client, token)
    with ThreadPoolExecutor(max_workers=2) as executor:
        responses = list(executor.map(
            lambda _index: client.post('/api/v1/auth/invitations/accept', json=payload), range(2),
        ))
    assert sorted(response.status_code for response in responses) == [200, 400]
    with Session(engine) as session:
        user = session.scalar(select(User).where(User.username_normalized == 'concurrentlegal'))
        assert user.status == 'active'
        assert session.scalar(select(func.count()).select_from(UserDocumentAcceptance).where(
            UserDocumentAcceptance.user_id == user.id)) == 1


def test_unapproved_mandatory_registry_fails_closed(cloud_client):
    client, engine = cloud_client
    headers = _admin(client, engine)
    client.app.state.legal_document_registry = LegalDocumentRegistry((LegalDocumentVersion(
        document_id='wotra.beta_terms',
        document_type='beta_terms',
        revision='owner-review-required',
        language='ru',
        title='Закрытое тестирование worta',
        content='DRAFT',
        mandatory=True,
        requires_explicit_acceptance=True,
        approved=False,
    ),))
    assert client.post('/api/v1/admin/invitations', headers=headers, json={
        'username': 'UnapprovedLegal', 'email': 'unapproved-legal@example.com',
    }).status_code == 201
    token = _issued_token(client)
    preview = client.post('/api/v1/auth/invitations/documents', json={
        'token': token, 'language': 'ru',
    })
    assert preview.status_code == 503
    assert preview.json()['detail']['code'] == 'legal_documents_unavailable'

    user_id = create_user(
        engine, username='BlockedLegal', email='blocked-legal@example.test', accept_legal=False,
    )
    user_headers = _headers(client, 'BlockedLegal')
    status = client.get('/api/v1/legal/documents/status?language=ru', headers=user_headers)
    assert status.status_code == 503
    assert status.json()['detail']['code'] == 'legal_documents_unavailable'
    blocked = client.get('/api/v1/account/crypto', headers=user_headers)
    assert blocked.status_code == 503
    assert blocked.json()['detail']['code'] == 'legal_documents_unavailable'
    with Session(engine) as session:
        assert session.get(User, user_id).status == 'active'
        invitation = session.scalar(select(AdminInvitation).where(
            AdminInvitation.token_hash.is_not(None),
            AdminInvitation.used_at.is_(None),
        ).order_by(AdminInvitation.created_at.desc()))
        assert invitation is not None
        assert session.scalar(select(func.count()).select_from(UserDocumentAcceptance).where(
            UserDocumentAcceptance.user_id == invitation.user_id)) == 0
