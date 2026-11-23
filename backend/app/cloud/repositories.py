from __future__ import annotations

from sqlalchemy import String, and_, delete, func, literal_column, or_, select, text, update
from sqlalchemy.dialects.postgresql import insert
from sqlalchemy.orm import Session

from .models import (AuthRefreshToken, AuthSession, CloudProject, EncryptedBlob, EncryptedObject, GlobalLimits,
                     RegistrationSettings, ReservedUsername, User,
                     UserLimitOverrides, SyncDevice, SyncEvent, SyncUserState)
from .schemas import MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES, SYNC_MAX_WIRE_INTEGER


def normalize_username(value: str) -> str:
    return value.strip().casefold()


def normalize_email(value: str) -> str:
    return value.strip().casefold()


def lock_username_namespace(session: Session, username: str) -> None:
    """Serialize public username claims and administrator reservations.

    This is deliberately PostgreSQL transaction-scoped: no process-local lock
    can protect registration across API workers.
    """
    session.execute(text(
        "SELECT pg_advisory_xact_lock(hashtextextended(:username, 641257))",
    ), {'username': normalize_username(username)})


class UserRepository:
    def get_by_normalized_username(self, session: Session, username: str) -> User | None:
        return session.scalar(select(User).where(User.username_normalized == normalize_username(username)))

    def get_by_id(self, session: Session, user_id: object) -> User | None:
        return session.get(User, user_id)

    def list_admin(self, session: Session, *, limit: int, offset: int,
                   status: str | None = None, role: str | None = None,
                   search: str | None = None) -> tuple[list[User], int]:
        statement = select(User)
        if status is not None:
            statement = statement.where(User.status == status)
        if role is not None:
            statement = statement.where(User.role == role)
        if search:
            value = f"%{normalize_username(search)}%"
            statement = statement.where(
                User.username_normalized.like(value) | User.email_normalized.like(value),
            )
        total = session.scalar(select(func.count()).select_from(statement.subquery())) or 0
        users = session.scalars(statement.order_by(User.created_at, User.id).limit(limit).offset(offset)).all()
        return users, total

    def create(self, session: Session, *, username: str, email: str, password_hash: str,
               role: str = 'user', status: str = 'pending', registration_mode_at_signup: str | None = None) -> User:
        user = User(username=username.strip(), username_normalized=normalize_username(username),
                    email=email.strip(), email_normalized=normalize_email(email), password_hash=password_hash,
                    role=role, status=status, registration_mode_at_signup=registration_mode_at_signup)
        session.add(user)
        return user


class RegistrationSettingsRepository:
    SINGLETON_ID = 1

    def get(self, session: Session, *, lock: bool = False) -> RegistrationSettings | None:
        statement = select(RegistrationSettings).where(RegistrationSettings.id == self.SINGLETON_ID)
        if lock:
            statement = statement.with_for_update()
        return session.scalar(statement)


class ReservedUsernameRepository:
    def is_reserved(self, session: Session, username: str) -> bool:
        return session.get(ReservedUsername, normalize_username(username)) is not None

    def list(self, session: Session) -> list[ReservedUsername]:
        return session.scalars(select(ReservedUsername).order_by(
            ReservedUsername.username_normalized,
        )).all()

    def add(self, session: Session, username: str) -> ReservedUsername:
        row = ReservedUsername(username_normalized=normalize_username(username))
        session.add(row)
        return row

    def remove(self, session: Session, username: str) -> bool:
        result = session.execute(delete(ReservedUsername).where(
            ReservedUsername.username_normalized == normalize_username(username),
        ))
        return bool(result.rowcount)


class GlobalLimitsRepository:
    SINGLETON_ID = 1

    def get(self, session: Session, *, lock: bool = False) -> GlobalLimits | None:
        statement = select(GlobalLimits).where(GlobalLimits.id == self.SINGLETON_ID)
        if lock:
            statement = statement.with_for_update()
        return session.scalar(statement)


class UserLimitOverridesRepository:
    def get(self, session: Session, user_id: object) -> UserLimitOverrides | None:
        return session.get(UserLimitOverrides, user_id)

    def clear(self, session: Session, user_id: object) -> None:
        session.execute(delete(UserLimitOverrides).where(UserLimitOverrides.user_id == user_id))


class CloudProjectRepository:
    def list(self, session: Session, user_id: object) -> list[CloudProject]:
        return session.scalars(select(CloudProject).where(
            CloudProject.user_id == user_id,
        ).order_by(CloudProject.created_at, CloudProject.project_id)).all()

    def list_ids(self, session: Session, user_id: object) -> list[str]:
        return session.scalars(select(CloudProject.project_id).where(
            CloudProject.user_id == user_id, CloudProject.retired_at.is_(None),
        ).order_by(CloudProject.created_at, CloudProject.project_id)).all()

    def count(self, session: Session, user_id: object) -> int:
        return int(session.scalar(select(func.count()).select_from(CloudProject).where(
            CloudProject.user_id == user_id, CloudProject.retired_at.is_(None),
        )) or 0)

    def get(self, session: Session, user_id: object, project_id: str, *, lock: bool = False) -> CloudProject | None:
        statement = select(CloudProject).where(
            CloudProject.user_id == user_id,
            CloudProject.project_id == project_id,
        )
        if lock:
            statement = statement.with_for_update()
        return session.scalar(statement)

    def add(self, session: Session, user_id: object, project_id: str) -> CloudProject:
        row = CloudProject(user_id=user_id, project_id=project_id)
        session.add(row)
        return row

    def remove(self, session: Session, user_id: object, project_id: str) -> bool:
        result = session.execute(delete(CloudProject).where(
            CloudProject.user_id == user_id,
            CloudProject.project_id == project_id,
        ))
        return bool(result.rowcount)


class EncryptedBlobRepository:
    def get(self, session: Session, user_id: object, blob_id: object) -> EncryptedBlob | None:
        return session.get(EncryptedBlob, (user_id, blob_id))

    def add(self, session: Session, **values: object) -> EncryptedBlob:
        row = EncryptedBlob(**values)
        session.add(row)
        return row


class SyncRepository:
    def has_v2_incompatible_history(self, session: Session, user_id: object) -> bool:
        """Check every retained event using only server-visible envelope metadata."""
        row = session.scalar(select(SyncEvent.event_id).outerjoin(
            EncryptedObject,
            and_(EncryptedObject.user_id == SyncEvent.user_id,
                 EncryptedObject.event_id == SyncEvent.event_id),
        ).where(
            SyncEvent.user_id == user_id,
            or_(
                SyncEvent.entity_type != 'note',
                SyncEvent.operation.notin_(('upsert', 'delete', 'resolution')),
                SyncEvent.revision > SYNC_MAX_WIRE_INTEGER,
                SyncEvent.server_sequence > SYNC_MAX_WIRE_INTEGER,
                and_(SyncEvent.operation == 'resolution', SyncEvent.revision < 2),
                EncryptedObject.event_id.is_(None),
                EncryptedObject.crypto_version != 1,
                EncryptedObject.aad_version != 1,
                func.octet_length(EncryptedObject.nonce) != 24,
                func.octet_length(EncryptedObject.ciphertext) < 16,
                func.octet_length(EncryptedObject.ciphertext) > MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES,
            ),
        ).limit(1))
        return row is not None

    def project_event_stats(self, session: Session, user_id: object, project_id: str,
                            *, device_id: object | None = None) -> tuple[int, int]:
        filters = [SyncEvent.user_id == user_id, SyncEvent.project_id == project_id]
        if device_id is not None:
            filters.append(SyncEvent.device_id == device_id)
        row = session.execute(select(
            func.count(SyncEvent.event_id), func.coalesce(func.max(SyncEvent.server_sequence), 0),
        ).where(*filters)).one()
        return int(row[0]), int(row[1])

    def get_device(self, session: Session, user_id: object, device_id: object, *, lock: bool = False) -> SyncDevice | None:
        statement = select(SyncDevice).where(SyncDevice.user_id == user_id, SyncDevice.device_id == device_id)
        if lock:
            statement = statement.with_for_update()
        return session.scalar(statement)

    def register_device(self, session: Session, user_id: object, device_id: object) -> SyncDevice:
        session.execute(insert(SyncDevice).values(user_id=user_id, device_id=device_id).on_conflict_do_nothing())
        row = self.get_device(session, user_id, device_id, lock=True)
        assert row is not None
        return row

    def user_state(self, session: Session, user_id: object, *, lock: bool = False) -> SyncUserState | None:
        statement = select(SyncUserState).where(SyncUserState.user_id == user_id)
        if lock:
            statement = statement.with_for_update()
        return session.scalar(statement)

    def ensure_user_state(self, session: Session, user_id: object, *, lock: bool = False) -> SyncUserState:
        session.execute(insert(SyncUserState).values(user_id=user_id).on_conflict_do_nothing())
        state = self.user_state(session, user_id, lock=lock)
        assert state is not None
        return state

    def event(self, session: Session, user_id: object, event_id: object) -> SyncEvent | None:
        return session.get(SyncEvent, (user_id, event_id))

    def add_event(self, session: Session, **values: object) -> SyncEvent:
        row = SyncEvent(**values)
        session.add(row)
        return row

    def encrypted_object(self, session: Session, user_id: object, event_id: object) -> EncryptedObject | None:
        return session.get(EncryptedObject, (user_id, event_id))

    def add_encrypted_object(self, session: Session, **values: object) -> EncryptedObject:
        row = EncryptedObject(**values)
        session.add(row)
        return row

    def pull(self, session: Session, user_id: object, since: int, limit: int) -> list[SyncEvent]:
        return session.scalars(select(SyncEvent).where(
            SyncEvent.user_id == user_id, SyncEvent.server_sequence > since,
        ).order_by(SyncEvent.server_sequence).limit(limit + 1)).all()

    def pull_encrypted_descriptors(self, session: Session, user_id: object, since: int, limit: int):
        return session.execute(select(
            SyncEvent,
            EncryptedObject.event_id.label('object_event_id'),
            EncryptedObject.crypto_version.label('crypto_version'),
            EncryptedObject.aad_version.label('aad_version'),
            EncryptedObject.nonce.label('nonce'),
            func.octet_length(EncryptedObject.ciphertext).label('ciphertext_size'),
            literal_column('encrypted_objects.xmin::text', type_=String).label('object_row_version'),
        ).outerjoin(
            EncryptedObject,
            and_(EncryptedObject.user_id == SyncEvent.user_id, EncryptedObject.event_id == SyncEvent.event_id),
        ).where(
            SyncEvent.user_id == user_id, SyncEvent.server_sequence > since,
        ).order_by(SyncEvent.server_sequence).limit(limit + 1)).all()

    def pull_encrypted_objects(self, session: Session, user_id: object, event_ids: list[object]):
        if not event_ids:
            return []
        statement = select(
            SyncEvent,
            EncryptedObject,
            literal_column('encrypted_objects.xmin::text', type_=String).label('object_row_version'),
        ).outerjoin(
            EncryptedObject,
            and_(EncryptedObject.user_id == SyncEvent.user_id, EncryptedObject.event_id == SyncEvent.event_id),
        ).where(
            SyncEvent.user_id == user_id, SyncEvent.event_id.in_(event_ids),
        ).order_by(SyncEvent.server_sequence).execution_options(populate_existing=True)
        return session.execute(statement).all()


class AuthRepository:
    def get_session(self, session: Session, session_id: object, *, lock: bool = False) -> AuthSession | None:
        statement = select(AuthSession).where(AuthSession.id == session_id)
        if lock:
            statement = statement.with_for_update()
        return session.scalar(statement)

    def get_refresh_token(self, session: Session, token_id: object, *, lock: bool = False) -> AuthRefreshToken | None:
        statement = select(AuthRefreshToken).where(AuthRefreshToken.id == token_id)
        if lock:
            statement = statement.with_for_update()
        return session.scalar(statement)

    def revoke_active_tokens(self, session: Session, session_id: object, now: object) -> None:
        session.execute(update(AuthRefreshToken).where(
            AuthRefreshToken.session_id == session_id,
            AuthRefreshToken.revoked_at.is_(None),
        ).values(revoked_at=now))

    def revoke_user_sessions(self, session: Session, user_id: object, now: object) -> tuple[int, int]:
        sessions = session.execute(update(AuthSession).where(
            AuthSession.user_id == user_id, AuthSession.revoked_at.is_(None),
        ).values(revoked_at=now))
        tokens = session.execute(update(AuthRefreshToken).where(
            AuthRefreshToken.session_id.in_(select(AuthSession.id).where(AuthSession.user_id == user_id)),
            AuthRefreshToken.revoked_at.is_(None),
        ).values(revoked_at=now))
        return int(sessions.rowcount or 0), int(tokens.rowcount or 0)
