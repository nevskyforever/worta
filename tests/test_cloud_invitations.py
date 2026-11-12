from __future__ import annotations

from datetime import timedelta
from pathlib import Path

import pytest
from alembic import command
from alembic.config import Config as AlembicConfig
from sqlalchemy import select
from sqlalchemy.orm import Session

from backend.app.cloud.email import (EmailDeliveryError, NullEmailSender, OutgoingEmail,
                                     RecordingEmailSender)
from backend.app.cloud.models import AdminInvitation, RegistrationSettings, User
from backend.app.cloud.tokens import utc_now
from test_cloud_auth import cloud_client, create_user, login, migrated_database


def _admin(client, engine) -> dict[str, str]:
    admin_id = create_user(engine, username='PrivateAdmin', email='admin@example.test', accept_legal=False)
    with Session(engine) as session:
        user = session.get(User, admin_id)
        user.role = 'admin'
        user.email_verified = True
        session.commit()
    return {'Authorization': f"Bearer {login(client, 'PrivateAdmin').json()['access_token']}"}


def _issued_token(client) -> str:
    message = client.app.state.email_sender.messages[-1]
    return message.body.split('#token=', 1)[1].split()[0]


def _invitation_documents(client, token: str) -> list[dict]:
    response = client.post('/api/v1/auth/invitations/documents', json={
        'token': token, 'language': 'ru',
    })
    assert response.status_code == 200, response.json()
    return [{
        'document_id': item['document_id'],
        'revision': item['revision'],
        'content_checksum': item['content_checksum'],
        'language': item['language'],
    } for item in response.json()['documents'] if item['mandatory']]


def _accept_payload(client, token: str, password: str = 'new private password long') -> dict:
    return {
        'token': token,
        'password': password,
        'language': 'ru',
        'documents': _invitation_documents(client, token),
    }


def test_private_invitation_authorization_creation_acceptance_and_replay(cloud_client):
    client, engine = cloud_client
    normal_id = create_user(engine, username='Normal', email='normal@example.test')
    normal_headers = {'Authorization': f"Bearer {login(client, 'Normal').json()['access_token']}"}
    payload = {'username': 'PrivateWriter', 'email': 'writer@example.com'}
    assert client.post('/api/v1/admin/invitations', json=payload).status_code == 401
    assert client.post('/api/v1/admin/invitations', json=payload, headers=normal_headers).status_code == 403
    headers = _admin(client, engine)
    assert client.get('/api/v1/auth/registration').json()['mode'] == 'closed'
    assert client.patch('/api/v1/admin/registration', json={'mode': 'open'}, headers=headers).status_code == 409
    issued = client.post('/api/v1/admin/invitations', json=payload, headers=headers)
    assert issued.status_code == 201, issued.json()
    assert issued.json()['status'] == 'pending'
    assert 'token' not in issued.text and 'password' not in issued.text
    message = client.app.state.email_sender.messages[-1]
    assert message.recipient == payload['email']
    assert message.subject == 'Приглашение в worta'
    assert 'закрытое тестирование worta.' in message.body
    token = _issued_token(client)
    invalid_token = token[:-1] + ('A' if token[-1] != 'A' else 'B')
    assert client.post('/api/v1/auth/invitations/documents', json={
        'token': invalid_token, 'language': 'ru',
    }).status_code == 400
    with Session(engine) as session:
        user = session.scalar(select(User).where(User.username_normalized == 'privatewriter'))
        row = session.scalar(select(AdminInvitation).where(AdminInvitation.user_id == user.id))
        assert user.status == 'pending' and not user.email_verified
        assert token not in row.token_hash and len(row.token_hash) == 64
    assert client.post('/api/v1/auth/login', json={
        'username': 'PrivateWriter', 'password': 'new private password long',
    }).status_code == 401
    assert client.post('/api/v1/auth/invitations/accept', json=_accept_payload(
        client, token, 'short',
    )).status_code == 422
    accepted = client.post('/api/v1/auth/invitations/accept', json=_accept_payload(client, token))
    assert accepted.status_code == 200 and accepted.json()['code'] == 'invitation_accepted'
    replay_payload = accepted.request.content
    assert client.post('/api/v1/auth/invitations/accept', content=replay_payload,
                       headers={'content-type': 'application/json'}).json()['detail']['code'] == 'invalid_or_expired_invitation'
    assert login(client, 'PrivateWriter', 'new private password long').status_code == 200
    listed = client.get('/api/v1/admin/invitations', headers=headers).json()
    assert listed[0]['status'] == 'accepted' and token not in str(listed)
    assert client.post('/api/v1/auth/register', json={
        'username': 'Public', 'email': 'public@example.com', 'password': 'public password long',
    }).status_code == 403


def test_private_invitation_revoke_expiry_reissue_and_capacity(cloud_client):
    client, engine = cloud_client
    headers = _admin(client, engine)
    payload = {'username': 'ReissueWriter', 'email': 'reissue@example.com'}
    first_response = client.post('/api/v1/admin/invitations', json=payload, headers=headers)
    assert first_response.status_code == 201, first_response.json()
    first = first_response.json()
    first_token = _issued_token(client)
    assert client.post('/api/v1/admin/invitations', json=payload, headers=headers).json()['detail']['code'] == 'invitation_pending'
    assert client.post(f"/api/v1/admin/invitations/{first['id']}/revoke", headers=headers).json()['status'] == 'revoked'
    assert client.post('/api/v1/auth/invitations/documents', json={
        'token': first_token, 'language': 'ru',
    }).status_code == 400
    second = client.post('/api/v1/admin/invitations', json=payload, headers=headers).json()
    second_token = _issued_token(client)
    with Session(engine) as session:
        session.get(AdminInvitation, second['id']).expires_at = utc_now() - timedelta(seconds=1)
        session.commit()
    assert any(row['id'] == second['id'] and row['status'] == 'expired'
               for row in client.get('/api/v1/admin/invitations', headers=headers).json())
    assert client.post('/api/v1/auth/invitations/documents', json={
        'token': second_token, 'language': 'ru',
    }).status_code == 400
    third = client.post('/api/v1/admin/invitations', json=payload, headers=headers).json()
    third_token = _issued_token(client)
    with Session(engine) as session:
        session.get(RegistrationSettings, 1).max_users = 1  # The administrator already uses the slot.
        session.commit()
    third_payload = _accept_payload(client, third_token)
    blocked = client.post('/api/v1/auth/invitations/accept', json=third_payload)
    assert blocked.status_code == 409 and blocked.json()['detail']['code'] == 'capacity_reached'
    with Session(engine) as session:
        assert session.get(AdminInvitation, third['id']).used_at is None
        session.get(RegistrationSettings, 1).max_users = 2
        session.commit()
    assert client.post('/api/v1/auth/invitations/accept', json=third_payload).status_code == 200


def test_invitation_requires_email_delivery_without_creating_an_account(cloud_client):
    client, engine = cloud_client
    headers = _admin(client, engine)
    client.app.state.email_sender = NullEmailSender()
    response = client.post('/api/v1/admin/invitations', headers=headers,
        json={'username': 'NoDelivery', 'email': 'no-delivery@example.com'})
    assert response.status_code == 503
    with Session(engine) as session:
        assert session.scalar(select(User).where(User.username_normalized == 'nodelivery')) is None


def test_failed_invitation_delivery_revokes_token_and_allows_retry(cloud_client, caplog):
    client, engine = cloud_client
    headers = _admin(client, engine)

    class FailingSender:
        def send(self, message: OutgoingEmail) -> None:
            raise EmailDeliveryError('SMTP delivery failed.')

    client.app.state.email_sender = FailingSender()
    payload = {'username': 'DeliveryRetry', 'email': 'retry@example.com'}
    response = client.post('/api/v1/admin/invitations', headers=headers, json=payload)
    assert response.status_code == 503
    assert response.json()['detail']['code'] == 'invitation_delivery_unavailable'
    with Session(engine) as session:
        user = session.scalar(select(User).where(User.username_normalized == 'deliveryretry'))
        row = session.scalar(select(AdminInvitation).where(AdminInvitation.user_id == user.id))
        assert user.status == 'pending' and row.revoked_at is not None and row.used_at is None
        assert row.expires_at - row.created_at == timedelta(hours=72)
    assert 'iv1.' not in response.text and 'iv1.' not in caplog.text
    client.app.state.email_sender = RecordingEmailSender()
    retry = client.post('/api/v1/admin/invitations', headers=headers, json=payload)
    assert retry.status_code == 201 and retry.json()['status'] == 'pending'
    assert client.app.state.email_sender.messages[-1].body.startswith(
        'Администратор пригласил вас в закрытое тестирование worta.')
    assert 'https://app.example.test/invite#token=iv1.' in client.app.state.email_sender.messages[-1].body


def test_populated_invitation_history_blocks_downgrade(cloud_client, monkeypatch):
    client, engine = cloud_client
    headers = _admin(client, engine)
    assert client.post('/api/v1/admin/invitations', headers=headers,
        json={'username': 'KeptWriter', 'email': 'kept@example.com'}).status_code == 201
    monkeypatch.setenv('NFPROGRESS_DATABASE_URL', engine.url.render_as_string(hide_password=False))
    config = AlembicConfig(str(Path(__file__).resolve().parents[1] / 'alembic.ini'))
    with pytest.raises(RuntimeError, match='invitation history'):
        command.downgrade(config, 'c18_compression_readers')
    with Session(engine) as session:
        assert session.scalar(select(AdminInvitation.id)) is not None
