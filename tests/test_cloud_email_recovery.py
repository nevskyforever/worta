from __future__ import annotations

import re
import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
from unittest.mock import Mock

import pytest
from sqlalchemy import text
from sqlalchemy.orm import Session

from backend.app.cloud.email import EmailDeliveryError, OutgoingEmail, SmtpEmailSender
from backend.app.cloud.models import (AuthRefreshToken, AuthSession,
                                      EmailVerificationToken, PasswordResetToken,
                                      User)
from backend.app.cloud.services import AccountEmailService, RecoveryTokenError
from backend.app.cloud.tokens import TokenService, utc_now
from backend.app.config import RuntimeConfig
from test_cloud_auth import (AUTH_SECRET, cloud_client, create_user, login,
                             migrated_database)


def _token_from(sender, path: str) -> str:
    separator = '#' if path == '/reset-password' else r'\?'
    match = re.search(rf'{path}{separator}token=([^\s]+)', sender.messages[-1].body)
    assert match
    return match.group(1)


def _service(now=None) -> AccountEmailService:
    return AccountEmailService(TokenService(AUTH_SECRET), now_provider=now or utc_now)


def _issue_verification(engine, user_id, now=None, *, expect_token=True):
    with Session(engine) as db:
        raw = _service(now).issue_verification(db, db.get(User, user_id))
    if expect_token:
        assert raw
    return raw


def _issue_reset(engine, user_id, now=None, *, expect_token=True):
    with Session(engine) as db:
        email = db.get(User, user_id).email
        issued = _service(now).issue_password_reset(db, email)
    if expect_token:
        assert issued
    return issued[1] if issued else None


@pytest.mark.parametrize('prefix,model,issue_name', [
    ('ev1', EmailVerificationToken, 'issue_email_verification_token'),
    ('pr1', PasswordResetToken, 'issue_password_reset_token'),
])
def test_opaque_token_storage_and_modified_secret(migrated_database, prefix, model, issue_name):
    user_id = create_user(migrated_database)
    tokens = TokenService(AUTH_SECRET)
    token_id, raw, token_hash = getattr(tokens, issue_name)()
    parsed = tokens.parse_opaque_token(raw, prefix); assert parsed
    with Session(migrated_database) as db:
        kwargs = dict(id=token_id, user_id=user_id, token_hash=token_hash,
                      expires_at=utc_now() + timedelta(hours=1))
        if model is EmailVerificationToken:
            kwargs['email_normalized'] = 'arthur@example.test'
        db.add(model(**kwargs)); db.commit()
        stored = db.get(model, token_id)
        values = db.execute(text(f'SELECT token_hash FROM {model.__tablename__}')).scalars().all()
    assert raw not in values and parsed[1] not in values
    assert re.fullmatch(r'[0-9a-f]{64}', stored.token_hash)
    assert tokens.verify_refresh_secret(parsed[1], stored.token_hash)
    assert not tokens.verify_refresh_secret(parsed[1] + 'changed', stored.token_hash)


def test_smtp_modes_config_redaction_and_trusted_links(monkeypatch, caplog, cloud_client):
    for security, factory in [('implicit_tls', 'SMTP_SSL'), ('starttls', 'SMTP')]:
        config = RuntimeConfig(public_web_url='https://trusted.example.test', smtp_host='smtp.example.test',
            smtp_port=465, smtp_username='mailer', smtp_password='smtp-secret',
            smtp_from_email='no-reply@example.test', smtp_security=security)
        client = Mock(); client.__enter__ = Mock(return_value=client); client.__exit__ = Mock(return_value=False)
        mocked = Mock(return_value=client); monkeypatch.setattr(f'backend.app.cloud.email.smtplib.{factory}', mocked)
        SmtpEmailSender(config).send(OutgoingEmail('to@example.test', 'Subject', 'Body'))
        assert mocked.call_args.kwargs['timeout'] == 10
        assert client.send_message.call_args.args[0]['From'] == 'worta <no-reply@example.test>'
        if security == 'starttls': client.starttls.assert_called_once()
    assert 'smtp-secret' not in repr(config)
    with pytest.raises(ValueError): RuntimeConfig(smtp_host='smtp.example.test')
    with pytest.raises(ValueError): RuntimeConfig(public_web_url='http://bad.test', smtp_host='x', smtp_port=25,
        smtp_username='u', smtp_password='secret', smtp_from_email='a@b.test', smtp_security='plaintext')
    for unsafe_url in ('https:', 'https://user:secret@example.test',
                       'https://example.test/#token=secret'):
        with pytest.raises(ValueError) as error:
            RuntimeConfig(public_web_url=unsafe_url)
        assert 'secret' not in str(error.value)
    failure_client = Mock(); failure_client.__enter__ = Mock(return_value=failure_client); failure_client.__exit__ = Mock(return_value=False)
    failure_client.login.side_effect = OSError('smtp-secret must not leak')
    monkeypatch.setattr('backend.app.cloud.email.smtplib.SMTP', Mock(return_value=failure_client))
    with pytest.raises(EmailDeliveryError, match='SMTP delivery failed'):
        SmtpEmailSender(config).send(OutgoingEmail('to@example.test', 'Subject', 'ev1.secret'))
    assert 'smtp-secret' not in caplog.text and 'ev1.secret' not in caplog.text

    client, engine = cloud_client; create_user(engine)
    access = login(client).json()['access_token']
    client.post('/api/v1/account/email/verification/request', headers={
        'Authorization': f'Bearer {access}', 'Host': 'attacker.test', 'X-Forwarded-Host': 'attacker.test'})
    assert 'https://app.example.test/verify-email?' in client.app.state.email_sender.messages[-1].body


@pytest.mark.parametrize('security,factory', [
    ('implicit_tls', 'SMTP_SSL'), ('starttls', 'SMTP'),
])
def test_smtp_transport_failure_is_sanitized_for_both_tls_modes(
        monkeypatch, caplog, security, factory):
    config = RuntimeConfig(public_web_url='https://trusted.example.test',
        smtp_host='smtp.example.test', smtp_port=465, smtp_username='mailer',
        smtp_password='smtp-secret', smtp_from_email='no-reply@example.test',
        smtp_security=security)
    smtp = Mock()
    smtp.__enter__ = Mock(return_value=smtp)
    smtp.__exit__ = Mock(return_value=False)
    smtp.send_message.side_effect = OSError('smtp-secret and iv1.secret')
    monkeypatch.setattr(f'backend.app.cloud.email.smtplib.{factory}', Mock(return_value=smtp))
    with pytest.raises(EmailDeliveryError) as error:
        SmtpEmailSender(config).send(OutgoingEmail('to@example.test', 'Invitation', 'iv1.secret'))
    assert 'smtp-secret' not in str(error.value) + caplog.text
    assert 'iv1.secret' not in str(error.value) + caplog.text


def test_verification_valid_status_email_binding_and_all_invalid_states(migrated_database):
    user_id = create_user(migrated_database, status='pending')
    raw = _issue_verification(migrated_database, user_id)
    with Session(migrated_database) as db:
        _service().verify_email(db, raw)
        user = db.get(User, user_id)
        assert user.email_verified and user.status == 'pending'
    for state in ('used_at', 'revoked_at', 'expires_at'):
        user_id = create_user(migrated_database, username=f'User{state}', email=f'{state}@example.test')
        raw = _issue_verification(migrated_database, user_id)
        parsed = TokenService.parse_opaque_token(raw, 'ev1'); assert parsed
        with Session(migrated_database) as db:
            row = db.get(EmailVerificationToken, parsed[0])
            setattr(row, state, utc_now() - timedelta(seconds=1) if state == 'expires_at' else utc_now())
            db.commit()
        with Session(migrated_database) as db, pytest.raises(RecoveryTokenError):
            _service().verify_email(db, raw)
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError):
        _service().verify_email(db, 'ev1.not-a-uuid.secret')
    modified_user = create_user(migrated_database, username='ModifiedVerification', email='modified@example.test')
    modified = _issue_verification(migrated_database, modified_user)
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError):
        _service().verify_email(db, modified + 'changed')


def test_verification_resend_email_change_concurrency_and_verified_noop(migrated_database):
    user_id = create_user(migrated_database)
    first = _issue_verification(migrated_database, user_id, lambda: utc_now() - timedelta(minutes=2))
    second = _issue_verification(migrated_database, user_id)
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError): _service().verify_email(db, first)
    with Session(migrated_database) as db:
        db.get(User, user_id).email_normalized = 'new@example.test'; db.commit()
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError): _service().verify_email(db, second)
    user_id = create_user(migrated_database, username='ConcurrentV', email='cv@example.test')
    raw = _issue_verification(migrated_database, user_id)
    barrier = threading.Barrier(2)
    def consume():
        with Session(migrated_database) as db:
            barrier.wait(timeout=10)
            try: _service().verify_email(db, raw); return True
            except RecoveryTokenError: return False
    with ThreadPoolExecutor(max_workers=2) as executor: results = list(executor.map(lambda _: consume(), range(2)))
    assert results.count(True) == 1
    with Session(migrated_database) as db:
        assert _service().issue_verification(db, db.get(User, user_id)) is None


def test_throttle_and_concurrent_issuance_are_postgresql_backed(migrated_database):
    user_id = create_user(migrated_database)
    clock = [utc_now()]
    now = lambda: clock[0]
    first = _issue_verification(migrated_database, user_id, now)
    assert _issue_verification(migrated_database, user_id, now, expect_token=False) is None
    clock[0] += timedelta(seconds=61)
    for _ in range(4):
        assert _issue_verification(migrated_database, user_id, now)
        clock[0] += timedelta(seconds=61)
    assert _issue_verification(migrated_database, user_id, now, expect_token=False) is None
    clock[0] += timedelta(hours=1)
    assert _issue_verification(migrated_database, user_id, now)
    # Independent sessions lock the stable user row, so two workers cannot bypass cooldown.
    other_id = create_user(migrated_database, username='IssueRace', email='issue@example.test')
    barrier = threading.Barrier(2)
    def issue():
        with Session(migrated_database) as db:
            user = db.get(User, other_id); barrier.wait(timeout=10)
            return _service().issue_verification(db, user)
    with ThreadPoolExecutor(max_workers=2) as executor: issued = list(executor.map(lambda _: issue(), range(2)))
    assert sum(token is not None for token in issued) == 1
    with Session(migrated_database) as db:
        assert db.query(EmailVerificationToken).filter_by(user_id=other_id, revoked_at=None).count() == 1
    assert first.startswith('ev1.')


def test_reset_generic_states_tokens_sessions_and_notification(cloud_client):
    client, engine = cloud_client
    active_id = create_user(engine)
    for status in ('pending', 'blocked', 'rejected'):
        create_user(engine, username=status, email=f'{status}@example.test', status=status)
    sender = client.app.state.email_sender
    responses = [client.post('/api/v1/auth/password-reset/request', json={'email': value}) for value in
        ('arthur@example.test', 'none@example.test', 'pending@example.test', 'blocked@example.test', 'rejected@example.test')]
    assert all(item.status_code == 202 and item.json() == responses[0].json() for item in responses)
    assert len(sender.messages) == 1
    throttled = client.post('/api/v1/auth/password-reset/request', json={'email': 'arthur@example.test'})
    assert throttled.status_code == responses[0].status_code and throttled.json() == responses[0].json()
    raw = _token_from(sender, '/reset-password')
    old_one, old_two = login(client).json(), login(client).json()
    parsed = TokenService.parse_opaque_token(raw, 'pr1'); assert parsed
    with Session(engine) as db:
        old_session_ids = {item.id for item in db.query(AuthSession).filter_by(user_id=active_id)}
        old_refresh = [item.id for item in db.query(AuthRefreshToken).filter(AuthRefreshToken.session_id.in_(old_session_ids))]
    assert client.post('/api/v1/auth/password-reset/confirm', json={'token': raw, 'new_password': 'new password with enough unicode длина'}).status_code == 200
    with Session(engine) as db:
        sessions = {item.id: item for item in db.query(AuthSession).filter(AuthSession.id.in_(old_session_ids))}
        assert all(item.revoked_at for item in sessions.values())
        assert all(db.get(AuthRefreshToken, token_id).revoked_at for token_id in old_refresh)
        assert db.get(PasswordResetToken, parsed[0]).used_at
    for old in (old_one, old_two):
        assert client.get('/api/v1/account/me', headers={'Authorization': f"Bearer {old['access_token']}"}).status_code == 401
        assert client.post('/api/v1/auth/refresh', json={'refresh_token': old['refresh_token']}).status_code == 401
    assert login(client).status_code == 401
    fresh = login(client, password='new password with enough unicode длина').json()
    assert client.get('/api/v1/account/me', headers={'Authorization': f"Bearer {fresh['access_token']}"}).status_code == 200
    with Session(engine) as db:
        new_session = db.get(AuthSession, TokenService(AUTH_SECRET).decode_access_token(fresh['access_token']).session_id)
        assert new_session.revoked_at is None
    assert raw not in sender.messages[-1].body and 'new password' not in sender.messages[-1].body


def test_reset_invalid_states_rotation_concurrency_and_throttle(migrated_database):
    user_id = create_user(migrated_database)
    clock = [utc_now()]
    now = lambda: clock[0]
    first = _issue_reset(migrated_database, user_id, now)
    clock[0] += timedelta(seconds=61)
    second = _issue_reset(migrated_database, user_id, now)
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError): _service().confirm_password_reset(db, first, 'password sufficient for C3 reset')
    parsed = TokenService.parse_opaque_token(second, 'pr1'); assert parsed
    with Session(migrated_database) as db:
        row = db.get(PasswordResetToken, parsed[0]); row.expires_at = utc_now() - timedelta(seconds=1); db.commit()
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError): _service().confirm_password_reset(db, second, 'password sufficient for C3 reset')
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError): _service().confirm_password_reset(db, 'pr1.bad.secret', 'password sufficient for C3 reset')
    assert _issue_reset(migrated_database, user_id, now, expect_token=False) is None
    clock[0] += timedelta(seconds=61)
    raw = _issue_reset(migrated_database, user_id, now)
    altered = raw + 'changed'
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError): _service().confirm_password_reset(db, altered, 'password sufficient for C3 reset')
    barrier = threading.Barrier(2)
    def consume():
        with Session(migrated_database) as db:
            barrier.wait(timeout=10)
            try: _service().confirm_password_reset(db, raw, 'password sufficient for C3 reset'); return True
            except RecoveryTokenError: return False
    with ThreadPoolExecutor(max_workers=2) as executor: results = list(executor.map(lambda _: consume(), range(2)))
    assert results.count(True) == 1
    with Session(migrated_database) as db, pytest.raises(RecoveryTokenError):
        _service(now).confirm_password_reset(db, raw, 'password sufficient for C3 reset')


def test_reset_throttle_and_concurrent_issuance(migrated_database):
    user_id = create_user(migrated_database, username='ResetThrottle', email='reset-throttle@example.test')
    clock = [utc_now()]
    now = lambda: clock[0]
    assert _issue_reset(migrated_database, user_id, now)
    assert _issue_reset(migrated_database, user_id, now, expect_token=False) is None
    clock[0] += timedelta(seconds=61)
    for _ in range(4):
        assert _issue_reset(migrated_database, user_id, now)
        clock[0] += timedelta(seconds=61)
    assert _issue_reset(migrated_database, user_id, now, expect_token=False) is None
    clock[0] += timedelta(hours=1)
    assert _issue_reset(migrated_database, user_id, now)
    racer_id = create_user(migrated_database, username='ResetRace', email='reset-race@example.test')
    barrier = threading.Barrier(2)
    def issue():
        with Session(migrated_database) as db:
            email = db.get(User, racer_id).email; barrier.wait(timeout=10)
            return _service().issue_password_reset(db, email)
    with ThreadPoolExecutor(max_workers=2) as executor: issued = list(executor.map(lambda _: issue(), range(2)))
    assert sum(item is not None for item in issued) == 1
    with Session(migrated_database) as db:
        assert db.query(PasswordResetToken).filter_by(user_id=racer_id, revoked_at=None).count() == 1
