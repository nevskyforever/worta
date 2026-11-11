from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timedelta

from sqlalchemy import func, select, update
from sqlalchemy.exc import IntegrityError
from sqlalchemy.orm import Session

from .models import (AuthRefreshToken, AuthSession, CloudProject, EmailVerificationToken, SyncDevice, SyncEvent,
                     GlobalLimits, PasswordResetToken, RegistrationSettings, SyncUserState,
                     ReservedUsername, User, UserLimitOverrides)
from .passwords import PasswordService
from .repositories import (AuthRepository, CloudProjectRepository, GlobalLimitsRepository,
                           RegistrationSettingsRepository,
                           ReservedUsernameRepository,
                           SyncRepository, UserLimitOverridesRepository, UserRepository,
                           lock_username_namespace, normalize_email,
                           normalize_username)
from .schemas import (ENCRYPTED_SYNC_VERSION, MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES,
                      MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES, SYNC_MAX_WIRE_INTEGER, SYNC_PROTOCOL_VERSION,
                      EncryptedSyncPushItem, SyncEventEnvelope, V2EncryptedSyncPushItem,
                      V2SyncEventEnvelope, decode_canonical_base64url, is_map_entity_id)
from .tokens import (ACCESS_TOKEN_LIFETIME, EMAIL_VERIFICATION_TOKEN_LIFETIME,
                     PASSWORD_RESET_TOKEN_LIFETIME, SESSION_LIFETIME,
                     TokenService, utc_now)


_DEFAULT_PASSWORDS = PasswordService()


class AuthenticationError(Exception):
    """A deliberately generic public authentication failure."""


class RecoveryTokenError(Exception):
    """A deliberately generic token failure."""


class RegistrationUnavailableError(Exception):
    """Public registration cannot safely create a verifiable account."""


class LimitsUnavailableError(Exception):
    """The authoritative global limits singleton is missing or unavailable."""


class AdminOperationError(Exception):
    def __init__(self, code: str, message: str, status_code: int = 409) -> None:
        super().__init__(message)
        self.code, self.message, self.status_code = code, message, status_code


@dataclass(frozen=True, slots=True)
class RegistrationResult:
    code: str
    email: str | None = None
    verification_token: str | None = None


@dataclass(frozen=True, slots=True)
class EmailVerificationResult:
    account_status: str
    activation: str


@dataclass(frozen=True, slots=True)
class IssuedTokens:
    access_token: str
    refresh_token: str
    access_expires_in: int = int(ACCESS_TOKEN_LIFETIME.total_seconds())


@dataclass(frozen=True, slots=True)
class EffectiveLimits:
    max_cloud_projects: int


class LimitsService:
    """Compute effective limits without exposing their global or override source."""

    def __init__(self) -> None:
        self._global_limits = GlobalLimitsRepository()
        self._overrides = UserLimitOverridesRepository()

    def effective_for_user(self, session: Session, user_id: object) -> EffectiveLimits:
        global_limits = self._global_limits.get(session)
        if global_limits is None:
            raise LimitsUnavailableError()
        override = self._overrides.get(session, user_id)
        if override is not None and override.max_cloud_projects_override is not None:
            return EffectiveLimits(max_cloud_projects=override.max_cloud_projects_override)
        return EffectiveLimits(max_cloud_projects=global_limits.max_cloud_projects)


class CloudProjectLimitError(Exception):
    """A new cloud slot cannot be allocated under the effective C5 limit."""


class CloudProjectBootstrapError(Exception):
    def __init__(self, code: str, message: str, status_code: int = 409) -> None:
        super().__init__(message)
        self.code, self.message, self.status_code = code, message, status_code


class SyncProtocolError(Exception):
    def __init__(self, code: str, message: str, status_code: int = 409) -> None:
        super().__init__(message)
        self.code, self.message, self.status_code = code, message, status_code


@dataclass(frozen=True, slots=True)
class CloudProjectState:
    project_ids: list[str]
    count: int
    max_cloud_projects: int


@dataclass(frozen=True, slots=True)
class CloudProjectBootstrapState:
    projects: list[CloudProject]
    current_cursor: int


class CloudProjectService:
    """C8's metadata-only cloud project registry.

    The owner row lock serializes slot allocation for one user across API
    workers.  It deliberately protects only creation; administrators may lower
    a limit below existing use without rewriting existing registry rows.
    """

    def __init__(self) -> None:
        self._projects = CloudProjectRepository()
        self._limits = LimitsService()
        self._sync = SyncRepository()

    def list(self, session: Session, user_id: object) -> CloudProjectState:
        limits = self._limits.effective_for_user(session, user_id)
        ids = self._projects.list_ids(session, user_id)
        return CloudProjectState(ids, len(ids), limits.max_cloud_projects)

    def enable(self, session: Session, user_id: object, project_id: str) -> CloudProjectState:
        try:
            # Authentication has already read through this request session.
            # Finish that read transaction before opening the allocation one.
            session.commit()
            with session.begin():
                # Locking the owner, rather than the project rows, also works
                # when the user has no rows yet and gives each user an
                # independent PostgreSQL transaction-scoped allocation lane.
                owner = session.scalar(select(User).where(User.id == user_id).with_for_update())
                if owner is None:
                    raise RuntimeError('Authenticated user disappeared.')
                existing = self._projects.get(session, user_id, project_id)
                if existing is None:
                    limits = self._limits.effective_for_user(session, user_id)
                    if self._projects.count(session, user_id) >= limits.max_cloud_projects:
                        raise CloudProjectLimitError()
                    self._projects.add(session, user_id, project_id)
                    session.flush()
            return self.list(session, user_id)
        except (CloudProjectLimitError, LimitsUnavailableError):
            session.rollback()
            raise

    def disable(self, session: Session, user_id: object, project_id: str) -> None:
        try:
            session.commit()
            with session.begin():
                project = self._projects.get(session, user_id, project_id, lock=True)
                if project is not None and project.bootstrap_state != 'legacy':
                    raise CloudProjectBootstrapError(
                        'cloud_project_bootstrap_managed',
                        'Bootstrap-managed cloud projects cannot be deleted by the legacy endpoint.',
                    )
                self._projects.remove(session, user_id, project_id)
        except Exception:
            session.rollback()
            raise

    def list_bootstrap(self, session: Session, user_id: object) -> CloudProjectBootstrapState:
        state = self._sync.ensure_user_state(session, user_id)
        return CloudProjectBootstrapState(self._projects.list(session, user_id), state.current_sequence)

    def register_bootstrap(self, session: Session, user_id: object, project_id: str,
                           bootstrap_id: object, device_id: object) -> CloudProjectBootstrapState:
        try:
            session.commit()
            with session.begin():
                owner = session.scalar(select(User).where(User.id == user_id).with_for_update())
                if owner is None:
                    raise RuntimeError('Authenticated user disappeared.')
                if self._sync.get_device(session, user_id, device_id) is None:
                    raise CloudProjectBootstrapError(
                        'sync_device_not_registered', 'Bootstrap origin device is not registered.',
                    )
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                project = self._projects.get(session, user_id, project_id, lock=True)
                if project is None:
                    history_count, _ = self._sync.project_event_stats(session, user_id, project_id)
                    if history_count != 0:
                        raise CloudProjectBootstrapError(
                            'cloud_project_orphaned_remote_history',
                            'Remote project history exists without a compatible bootstrap registration.',
                        )
                    limits = self._limits.effective_for_user(session, user_id)
                    if self._projects.count(session, user_id) >= limits.max_cloud_projects:
                        raise CloudProjectLimitError()
                    project = self._projects.add(session, user_id, project_id)
                    project.bootstrap_id = bootstrap_id
                    project.bootstrap_device_id = device_id
                    project.bootstrap_state = 'initializing'
                    session.flush()
                elif project.bootstrap_state == 'legacy':
                    raise CloudProjectBootstrapError(
                        'cloud_project_legacy_reservation',
                        'The existing cloud project reservation has no bootstrap lineage.',
                    )
                elif project.bootstrap_id != bootstrap_id or project.bootstrap_device_id != device_id:
                    raise CloudProjectBootstrapError(
                        'cloud_project_bootstrap_conflict',
                        'Cloud project bootstrap lineage does not match.',
                    )
                current_cursor = state.current_sequence
            return CloudProjectBootstrapState([project], current_cursor)
        except Exception:
            session.rollback()
            raise

    def complete_bootstrap(self, session: Session, user_id: object, project_id: str,
                           bootstrap_id: object, device_id: object, initial_event_count: int,
                           initial_max_server_sequence: int) -> CloudProjectBootstrapState:
        try:
            session.commit()
            with session.begin():
                if self._sync.get_device(session, user_id, device_id) is None:
                    raise CloudProjectBootstrapError(
                        'sync_device_not_registered', 'Bootstrap origin device is not registered.',
                    )
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                project = self._projects.get(session, user_id, project_id, lock=True)
                if project is None:
                    raise CloudProjectBootstrapError(
                        'cloud_project_bootstrap_missing', 'Cloud project bootstrap registration is missing.', 404,
                    )
                if project.bootstrap_state == 'legacy':
                    raise CloudProjectBootstrapError(
                        'cloud_project_legacy_reservation',
                        'The existing cloud project reservation has no bootstrap lineage.',
                    )
                if project.bootstrap_id != bootstrap_id or project.bootstrap_device_id != device_id:
                    raise CloudProjectBootstrapError(
                        'cloud_project_bootstrap_conflict',
                        'Cloud project bootstrap lineage does not match.',
                    )
                if project.bootstrap_state == 'active':
                    if (project.initial_event_count != initial_event_count
                            or project.initial_max_server_sequence != initial_max_server_sequence):
                        raise CloudProjectBootstrapError(
                            'cloud_project_bootstrap_conflict',
                            'Completed cloud project bootstrap metadata is immutable.',
                        )
                    current_cursor = state.current_sequence
                else:
                    event_count, max_sequence = self._sync.project_event_stats(
                        session, user_id, project_id, device_id=device_id,
                    )
                    if event_count < initial_event_count or max_sequence < initial_max_server_sequence:
                        raise CloudProjectBootstrapError(
                            'cloud_project_initial_upload_incomplete',
                            'Server has not accepted the declared initial event cohort.',
                        )
                    project.bootstrap_state = 'active'
                    project.initial_event_count = initial_event_count
                    project.initial_max_server_sequence = initial_max_server_sequence
                    project.bootstrap_completed_at = utc_now()
                    session.flush()
                    current_cursor = state.current_sequence
            return CloudProjectBootstrapState([project], current_cursor)
        except Exception:
            session.rollback()
            raise


@dataclass(frozen=True, slots=True)
class SyncPushResult:
    event_id: object
    server_sequence: int
    duplicate: bool


@dataclass(frozen=True, slots=True)
class EncryptedPullDescriptor:
    user_id: object
    event_id: object
    device_id: object
    project_id: str | None
    entity_id: str
    entity_type: str
    operation: str
    revision: int
    updated_at: datetime
    deleted_at: datetime | None
    server_sequence: int
    object_event_id: object | None
    crypto_version: int | None
    aad_version: int | None
    nonce: bytes | None
    ciphertext_size: int | None
    object_row_version: str | None

    @classmethod
    def from_row(cls, row) -> 'EncryptedPullDescriptor':
        event = row[0]
        return cls(
            user_id=event.user_id, event_id=event.event_id, device_id=event.device_id,
            project_id=event.project_id, entity_id=event.entity_id, entity_type=event.entity_type,
            operation=event.operation, revision=event.revision, updated_at=event.updated_at,
            deleted_at=event.deleted_at, server_sequence=event.server_sequence,
            object_event_id=row.object_event_id, crypto_version=row.crypto_version,
            aad_version=row.aad_version, nonce=row.nonce, ciphertext_size=row.ciphertext_size,
            object_row_version=row.object_row_version,
        )


class SyncService:
    """C9 metadata-only transport; server order is not conflict resolution."""

    def __init__(self) -> None:
        self._sync = SyncRepository()
        self._projects = CloudProjectRepository()

    @staticmethod
    def require_protocol(version: int) -> None:
        if version != SYNC_PROTOCOL_VERSION:
            raise SyncProtocolError('sync_protocol_version_unsupported', 'Unsupported sync protocol version.', 422)

    @staticmethod
    def require_encrypted_protocol(version: int) -> None:
        if version != ENCRYPTED_SYNC_VERSION:
            raise SyncProtocolError('encrypted_sync_version_unsupported', 'Unsupported encrypted sync version.', 422)

    def register_device(self, session: Session, user_id: object, device_id: object, *, initial_web_setup: bool = False):
        try:
            session.commit()
            with session.begin():
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                if initial_web_setup:
                    self._require_transport_mode(state, 2)
                    self._require_initial_web_setup(session, user_id, state, device_id)
                device = self._sync.register_device(session, user_id, device_id)
                device.last_seen_at = utc_now()
                session.flush()
                return device.device_id, device.last_ack_sequence, state.current_sequence
        except Exception:
            session.rollback()
            raise

    def _registered_device(self, session: Session, user_id: object, device_id: object):
        device = self._sync.get_device(session, user_id, device_id, lock=True)
        if device is None:
            raise SyncProtocolError('sync_device_not_registered', 'Sync device is not registered.')
        return device

    @staticmethod
    def _same_event(row, event: SyncEventEnvelope | V2SyncEventEnvelope, device_id: object) -> bool:
        return (row.device_id == device_id and row.project_id == event.project_id
                and row.entity_id == event.entity_id and row.entity_type == event.entity_type
                and row.operation == event.operation and row.revision == event.revision
                and row.updated_at == event.updated_at and row.deleted_at == event.deleted_at)

    @staticmethod
    def _require_transport_mode(state, expected_version: int) -> None:
        if state.writer_transport_version != expected_version:
            raise SyncProtocolError(
                'sync_transport_mode_incompatible',
                'The account requires a different encrypted sync transport version.',
                409,
            )

    def capabilities(self, session: Session, user_id: object) -> tuple[int, int]:
        state = self._sync.user_state(session, user_id)
        if state is None:
            return 1, 0
        return state.writer_transport_version, state.cutover_epoch

    def _require_initial_web_setup(self, session: Session, user_id: object,
                                   state: SyncUserState, device_id: object) -> None:
        # Called under the account-state lock shared by registration and sync.
        other_device = session.scalar(select(SyncDevice.device_id).where(
            SyncDevice.user_id == user_id, SyncDevice.device_id != device_id,
        ).limit(1))
        if (state.current_sequence != 0 or state.compression_id1_required
                or self._projects.count(session, user_id) != 0 or other_device is not None):
            raise SyncProtocolError(
                'initial_web_sync_requires_empty_account',
                'Initial Web sync requires an empty account and no other registered devices.', 409,
            )

    def cutover_to_v2(self, session: Session, user_id: object, expected_cutover_epoch: int, *,
                      initial_web_device_id: object | None = None) -> tuple[int, int]:
        try:
            session.commit()
            with session.begin():
                # Push and bootstrap writers use this same row lock. Nothing can append
                # an event between the complete-history check and the mode change.
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                if initial_web_device_id is not None:
                    self._require_initial_web_setup(session, user_id, state, initial_web_device_id)
                if state.writer_transport_version == 2:
                    return 2, state.cutover_epoch
                if state.writer_transport_version != 1:
                    raise SyncProtocolError(
                        'sync_transport_mode_incompatible',
                        'The account requires a different encrypted sync transport version.', 409,
                    )
                if state.cutover_epoch != expected_cutover_epoch:
                    raise SyncProtocolError('sync_transport_cutover_stale', 'Transport cutover epoch is stale.', 409)
                if state.cutover_epoch >= SYNC_MAX_WIRE_INTEGER:
                    raise SyncProtocolError('sync_transport_cutover_epoch_exhausted', 'Transport cutover epoch limit reached.', 409)
                if self._sync.has_v2_incompatible_history(session, user_id):
                    raise SyncProtocolError('sync_transport_cutover_blocked', 'Sync history is incompatible with transport v2.', 409)
                state.writer_transport_version = 2
                state.cutover_epoch += 1
                session.flush()
                return 2, state.cutover_epoch
        except Exception:
            session.rollback()
            raise

    def compression_gate(self, session: Session, user_id: object) -> tuple[bool, int]:
        devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user_id)).all()
        missing = sum(not device.compression_id1 for device in devices)
        mode, _ = self.capabilities(session, user_id)
        return bool(devices) and missing == 0 and mode == 3, missing

    def declare_compression_reader(self, session: Session, user_id: object, request) -> None:
        try:
            session.commit()
            with session.begin():
                self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, request.device_id)
                device.compression_id1 = request.compression_id1
                device.last_seen_at = utc_now()
        except Exception:
            session.rollback()
            raise

    def authorize_compressed_seal(self, session: Session, user_id: object, device_id: object) -> tuple[bool, int]:
        # Same account lock as device registration/capability changes. This
        # conservative sticky read barrier precedes encryption/publication, so a
        # late legacy registration cannot consume already sealed ID1 history.
        try:
            session.commit()
            with session.begin():
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                self._registered_device(session, user_id, device_id)
                ready, missing = self.compression_gate(session, user_id)
                if ready:
                    state.compression_id1_required = True
                return ready, missing
        except Exception:
            session.rollback()
            raise

    @staticmethod
    def require_compression_reader(state, device) -> None:
        if state.compression_id1_required and not device.compression_id1:
            raise SyncProtocolError('compression_reader_required', 'Explicit ID1 reader support is required for retained account history.', 409)

    def declare_v3_reader_ready(self, session: Session, user_id: object, device_id: object, *,
                                initial_web_setup: bool = False) -> None:
        try:
            session.commit()
            with session.begin():
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                if initial_web_setup:
                    self._require_initial_web_setup(session, user_id, state, device_id)
                if state.writer_transport_version not in (2, 3):
                    raise SyncProtocolError('sync_transport_mode_incompatible', 'Mode 2 is required before reader readiness.', 409)
                device = self._registered_device(session, user_id, device_id)
                device.reader_transport_version = 3
                device.last_seen_at = utc_now()
        except Exception:
            session.rollback()
            raise

    def cover_gate(self, session: Session, user_id: object) -> tuple[bool, int]:
        devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user_id)).all()
        missing = sum(device.reader_transport_version != 3 or device.metadata_cover_reader_version != 2 for device in devices)
        mode, _ = self.capabilities(session, user_id)
        return bool(devices) and missing == 0 and mode == 3, missing

    def declare_cover_reader(self, session: Session, user_id: object, request) -> None:
        try:
            session.commit()
            with session.begin():
                self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, request.device_id)
                if device.reader_transport_version != 3:
                    raise SyncProtocolError('cover_readers_not_ready', 'Mode 3 reader is required.', 409)
                device.metadata_cover_reader_version = request.reader_version
                device.last_seen_at = utc_now()
        except Exception:
            session.rollback()
            raise

    @staticmethod
    def content_note_reader_ready(device) -> bool:
        return (device.reader_transport_version == 3 and device.note_frame_version == 1
                and device.note_codec_version == 1 and device.note_compression_zero
                and device.note_ordinary_reader_version == 1 and device.note_resolution_reader_version == 2)

    def content_note_gate(self, session: Session, user_id: object) -> tuple[bool, int]:
        devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user_id)).all()
        missing = sum(not self.content_note_reader_ready(device) for device in devices)
        mode, _ = self.capabilities(session, user_id)
        return bool(devices) and missing == 0 and mode == 3, missing

    def declare_content_note_reader(self, session: Session, user_id: object, request) -> None:
        try:
            session.commit()
            with session.begin():
                self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, request.device_id)
                device.reader_transport_version = request.reader_transport_version
                device.note_frame_version = request.frame_version
                device.note_codec_version = request.codec8_version
                device.note_compression_zero = request.compression_zero
                device.note_ordinary_reader_version = request.ordinary_reader_version
                device.note_resolution_reader_version = request.resolution_reader_version
                device.last_seen_at = utc_now()
        except Exception:
            session.rollback()
            raise

    @classmethod
    def map_reader_ready(cls, device) -> bool:
        return (cls.content_note_reader_ready(device) and device.map_frame_version == 1
                and device.map_codec_version == 1 and device.map_reader_version == 1
                and device.map_compression_zero)

    def map_gate(self, session: Session, user_id: object) -> tuple[bool, int]:
        devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user_id)).all()
        missing = sum(not self.map_reader_ready(device) for device in devices)
        mode, _ = self.capabilities(session, user_id)
        return bool(devices) and missing == 0 and mode == 3, missing

    def declare_map_reader(self, session: Session, user_id: object, request) -> None:
        try:
            session.commit()
            with session.begin():
                self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, request.device_id)
                device.map_frame_version = request.frame_version
                device.map_codec_version = request.codec_version
                device.map_reader_version = request.reader_version
                device.map_compression_zero = request.compression_zero
                device.last_seen_at = utc_now()
        except Exception:
            session.rollback()
            raise

    @classmethod
    def document_reader_ready(cls, device) -> bool:
        return (cls.content_note_reader_ready(device) and device.document_frame_version == 1
                and device.document_codec_version == 1 and device.document_reader_version == 1
                and device.document_compression_zero)

    def document_gate(self, session: Session, user_id: object) -> tuple[bool, int]:
        devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user_id)).all()
        missing = sum(not self.document_reader_ready(device) for device in devices)
        mode, _ = self.capabilities(session, user_id)
        return bool(devices) and missing == 0 and mode == 3, missing

    def declare_document_reader(self, session: Session, user_id: object, request) -> None:
        try:
            session.commit()
            with session.begin():
                self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, request.device_id)
                device.document_frame_version = request.frame_version
                device.document_codec_version = request.codec_version
                device.document_reader_version = request.reader_version
                device.document_compression_zero = request.compression_zero
                device.last_seen_at = utc_now()
        except Exception:
            session.rollback()
            raise

    @classmethod
    def progress_reader_ready(cls, device) -> bool:
        return (cls.document_reader_ready(device) and cls.map_reader_ready(device) and device.progress_frame_version == 1
                and device.progress_codec_version == 1 and device.progress_reader_version == 1
                and device.progress_compression_zero)

    def progress_gate(self, session: Session, user_id: object) -> tuple[bool, int]:
        devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user_id)).all()
        missing = sum(not self.progress_reader_ready(device) for device in devices)
        mode, _ = self.capabilities(session, user_id)
        return bool(devices) and missing == 0 and mode == 3, missing

    def declare_progress_reader(self, session: Session, user_id: object, request) -> None:
        try:
            session.commit()
            with session.begin():
                self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, request.device_id)
                device.progress_frame_version = request.frame_version
                device.progress_codec_version = request.codec_version
                device.progress_reader_version = request.reader_version
                device.progress_compression_zero = request.compression_zero
                device.last_seen_at = utc_now()
        except Exception:
            session.rollback()
            raise

    @classmethod
    def game_reader_ready(cls, device) -> bool:
        return cls.progress_reader_ready(device) and all(
            getattr(device, f'{domain}_game_frame_version') == 1
            and getattr(device, f'{domain}_game_codec_version') == (2 if domain == 'account' else 1)
            and getattr(device, f'{domain}_game_reader_version') == (2 if domain == 'account' else 1)
            and getattr(device, f'{domain}_game_compression_zero')
            for domain in ('project', 'account')
        )

    def game_gate(self, session: Session, user_id: object) -> tuple[bool, int]:
        devices = session.scalars(select(SyncDevice).where(SyncDevice.user_id == user_id)).all()
        missing = sum(not self.game_reader_ready(device) for device in devices)
        mode, _ = self.capabilities(session, user_id)
        return bool(devices) and missing == 0 and mode == 3, missing

    def declare_game_reader(self, session: Session, user_id: object, request) -> None:
        try:
            session.commit()
            with session.begin():
                self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, request.device_id)
                for domain in ('project', 'account'):
                    support = getattr(request, domain)
                    for field in ('frame_version', 'codec_version', 'reader_version', 'compression_zero'):
                        setattr(device, f'{domain}_game_{field}', getattr(support, field))
                device.last_seen_at = utc_now()
        except Exception:
            session.rollback()
            raise

    def cutover_to_v3(self, session: Session, user_id: object, expected_cutover_epoch: int, *,
                      initial_web_device_id: object | None = None) -> tuple[int, int]:
        try:
            session.commit()
            with session.begin():
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                if initial_web_device_id is not None:
                    self._require_initial_web_setup(session, user_id, state, initial_web_device_id)
                if state.writer_transport_version == 3:
                    return 3, state.cutover_epoch
                self._require_transport_mode(state, 2)
                if initial_web_device_id is not None:
                    self._registered_device(session, user_id, initial_web_device_id)
                if state.cutover_epoch != expected_cutover_epoch:
                    raise SyncProtocolError('sync_transport_cutover_stale', 'Transport cutover epoch is stale.', 409)
                if state.cutover_epoch >= SYNC_MAX_WIRE_INTEGER:
                    raise SyncProtocolError('sync_transport_cutover_epoch_exhausted', 'Transport cutover epoch limit reached.', 409)
                not_ready = session.scalar(select(func.count()).select_from(SyncDevice).where(
                    SyncDevice.user_id == user_id, SyncDevice.reader_transport_version != 3,
                ))
                devices = session.scalar(select(func.count()).select_from(SyncDevice).where(SyncDevice.user_id == user_id))
                if not devices or not_ready:
                    raise SyncProtocolError('sync_transport_readers_not_ready', 'Registered devices must declare mode-3 reader support.', 409)
                if self._sync.has_v2_incompatible_history(session, user_id):
                    raise SyncProtocolError('sync_transport_cutover_blocked', 'Sync history is incompatible with mode 3.', 409)
                state.writer_transport_version = 3
                state.cutover_epoch += 1
                session.flush()
                return 3, state.cutover_epoch
        except Exception:
            session.rollback()
            raise

    def push(self, session: Session, user_id: object, device_id: object,
             events: list[SyncEventEnvelope], *, transport_version: int = 1) -> tuple[list[SyncPushResult], int]:
        try:
            session.commit()
            with session.begin():
                device = self._registered_device(session, user_id, device_id)
                device.last_seen_at = utc_now()
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                self._require_transport_mode(state, transport_version)
                results: list[SyncPushResult] = []
                seen: dict[object, SyncEventEnvelope] = {}
                for event in events:
                    prior = seen.get(event.event_id)
                    if prior is not None and prior != event:
                        raise SyncProtocolError('sync_event_id_conflict', 'Event ID was reused with different metadata.')
                    seen[event.event_id] = event
                    existing = self._sync.event(session, user_id, event.event_id)
                    if existing is not None:
                        if not self._same_event(existing, event, device_id):
                            raise SyncProtocolError('sync_event_id_conflict', 'Event ID was reused with different metadata.')
                        results.append(SyncPushResult(event.event_id, existing.server_sequence, True))
                        continue
                    # C8 validation applies only to a newly accepted event.
                    # An accepted retry remains confirmable after C8 disable.
                    project = self._projects.get(session, user_id, event.project_id)
                    if project is None:
                        raise SyncProtocolError('cloud_project_not_enabled', 'Cloud project is not enabled.')
                    if (project.bootstrap_state == 'initializing'
                            and project.bootstrap_device_id != device_id):
                        raise SyncProtocolError(
                            'cloud_project_bootstrap_origin_required',
                            'Only the bootstrap origin device may create events while initialization is in progress.',
                        )
                    if state.current_sequence >= SYNC_MAX_WIRE_INTEGER:
                        raise SyncProtocolError('sync_sequence_exhausted', 'Sync sequence limit reached.', 409)
                    state.current_sequence += 1
                    row = self._sync.add_event(
                        session, user_id=user_id, event_id=event.event_id, device_id=device_id,
                        project_id=event.project_id, entity_id=event.entity_id,
                        entity_type=event.entity_type, operation=event.operation,
                        revision=event.revision, updated_at=event.updated_at,
                        deleted_at=event.deleted_at, server_sequence=state.current_sequence,
                    )
                    session.flush()
                    results.append(SyncPushResult(event.event_id, row.server_sequence, False))
                return results, state.current_sequence
        except Exception:
            session.rollback()
            raise

    @staticmethod
    def _same_encrypted_object(row, item: EncryptedSyncPushItem | V2EncryptedSyncPushItem) -> bool:
        envelope = item.object
        return (row.crypto_version == envelope.crypto_version and row.aad_version == envelope.aad_version
                and row.nonce == decode_canonical_base64url(envelope.nonce, expected_length=24)
                and row.ciphertext == decode_canonical_base64url(envelope.ciphertext, minimum_length=16))

    @staticmethod
    def _validate_encrypted_item(item: EncryptedSyncPushItem | V2EncryptedSyncPushItem, *, transport_version: int) -> None:
        from .schemas import AccountSyncEventEnvelope
        if isinstance(item.event, AccountSyncEventEnvelope):
            if transport_version != 3 or item.object.crypto_version != 2 or item.object.aad_version != 2:
                raise SyncProtocolError('encrypted_sync_version_unsupported', 'Unsupported account object version.', 422)
            return
        allowed_operations = ('upsert', 'delete', 'resolution', 'event') if transport_version == 3 else ('upsert', 'delete') if transport_version == 1 else ('upsert', 'delete', 'resolution')
        allowed_types = ('note', 'project_metadata', 'stage', 'stage_order', 'map', 'document', 'progress', 'project_game') if transport_version == 3 else ('note',)
        if (item.event.entity_type not in allowed_types or item.event.operation not in allowed_operations
                or (item.event.operation == 'event' and item.event.entity_type not in ('note', 'map', 'document', 'progress', 'project_game'))
                or (item.event.entity_type == 'map' and (item.event.operation != 'event'
                    or item.event.deleted_at is not None or not is_map_entity_id(item.event.entity_id)))
                or (item.event.entity_type in ('document', 'progress', 'project_game') and (item.event.operation != 'event' or item.event.deleted_at is not None))
                or (item.event.entity_type == 'project_metadata'
                    and item.event.entity_id != item.event.project_id)
                or (item.event.entity_type == 'stage' and item.event.operation not in ('upsert', 'delete'))
                or (item.event.entity_type == 'stage_order' and (item.event.entity_id != 'stage_order' or item.event.operation != 'upsert'))):
            raise SyncProtocolError('encrypted_sync_event_unsupported', 'Unsupported encrypted sync event.', 422)
        if item.object.crypto_version != 1 or item.object.aad_version != 1:
            raise SyncProtocolError('encrypted_sync_version_unsupported', 'Unsupported encrypted object version.', 422)

    @classmethod
    def _validate_encrypted_batch(cls, items: list[EncryptedSyncPushItem] | list[V2EncryptedSyncPushItem], *, transport_version: int = 1) -> None:
        total = 0
        for item in items:
            cls._validate_encrypted_item(item, transport_version=transport_version)
            total += len(decode_canonical_base64url(
                item.object.ciphertext, minimum_length=16,
                maximum_length=MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES,
            ))
            if item.event.entity_type in ('project_game', 'account_game') and len(decode_canonical_base64url(item.object.ciphertext, minimum_length=16)) > 1024 * 1024 + 16:
                raise SyncProtocolError('encrypted_sync_object_too_large', 'Game encrypted object exceeds its size limit.', 413)
            if total > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES:
                raise SyncProtocolError(
                    'encrypted_sync_batch_too_large', 'Encrypted sync batch exceeds ciphertext size limit.', 413,
                )

    def push_encrypted(self, session: Session, user_id: object, device_id: object,
                       items: list[EncryptedSyncPushItem] | list[V2EncryptedSyncPushItem], *,
                       transport_version: int = 1, metadata_cover: bool = False) -> tuple[list[SyncPushResult], int]:
        try:
            from .schemas import AccountSyncEventEnvelope
            for item in items:
                if isinstance(item.event, AccountSyncEventEnvelope) and item.event.canonical_user_id != user_id:
                    raise SyncProtocolError('account_scope_rejected', 'Account scope does not match authenticated identity.', 403)
            self._validate_encrypted_batch(items, transport_version=transport_version)
            session.commit()
            with session.begin():
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, device_id)
                device.last_seen_at = utc_now()
                if metadata_cover and (transport_version != 3 or any(item.event.entity_type != 'project_metadata' for item in items)
                                       or not self.cover_gate(session, user_id)[0]):
                    raise SyncProtocolError('cover_readers_not_ready', 'All registered devices must support metadata-v2 covers.', 409)
                # A mode-3 reader keeps the frozen Note-v2 writer endpoint.
                # Its request schema admits Notes only; metadata uses v3.
                if not (transport_version == 2 and state.writer_transport_version == 3):
                    self._require_transport_mode(state, transport_version)
                if any(item.event.entity_type in ('project_game', 'account_game') for item in items) and not self.game_gate(session, user_id)[0]:
                    raise SyncProtocolError('game_readers_not_ready', 'Registered devices must support both Game readers and their dependencies.', 409)
                if any(item.event.entity_type in ('document', 'progress', 'project_game') for item in items) and not self.document_gate(session, user_id)[0]:
                    raise SyncProtocolError('document_readers_not_ready', 'Registered devices must support document readers.', 409)
                if any(item.event.entity_type == 'progress' for item in items) and not self.progress_gate(session, user_id)[0]:
                    raise SyncProtocolError('progress_readers_not_ready', 'Registered devices must support progress readers.', 409)
                if any(item.event.entity_type == 'map' for item in items) and not self.map_gate(session, user_id)[0]:
                    raise SyncProtocolError('map_readers_not_ready', 'Registered devices must support map readers.', 409)
                if any(item.event.entity_type == 'note' and item.event.operation == 'event' for item in items):
                    if not self.content_note_gate(session, user_id)[0]:
                        raise SyncProtocolError('content_note_readers_not_ready', 'Registered devices must support Note readers.', 409)
                results: list[SyncPushResult] = []
                for item in items:
                    event = item.event
                    existing = self._sync.event(session, user_id, event.event_id)
                    if existing is not None:
                        if not self._same_event(existing, event, device_id):
                            raise SyncProtocolError('sync_event_id_conflict', 'Event ID was reused with different metadata.')
                        object_row = self._sync.encrypted_object(session, user_id, event.event_id)
                        if object_row is None:
                            raise SyncProtocolError('encrypted_sync_event_incomplete', 'Historical sync event has no encrypted object.')
                        if not self._same_encrypted_object(object_row, item):
                            raise SyncProtocolError('sync_event_id_conflict', 'Event ID was reused with different encrypted object.')
                        results.append(SyncPushResult(event.event_id, existing.server_sequence, True))
                        continue
                    project = self._projects.get(session, user_id, event.project_id) if event.project_id is not None else None
                    if event.project_id is not None and project is None:
                        raise SyncProtocolError('cloud_project_not_enabled', 'Cloud project is not enabled.')
                    if (project is not None and project.bootstrap_state == 'initializing'
                            and project.bootstrap_device_id != device_id):
                        raise SyncProtocolError(
                            'cloud_project_bootstrap_origin_required',
                            'Only the bootstrap origin device may create events while initialization is in progress.',
                        )
                    if state.current_sequence >= SYNC_MAX_WIRE_INTEGER:
                        raise SyncProtocolError('sync_sequence_exhausted', 'Sync sequence limit reached.', 409)
                    state.current_sequence += 1
                    row = self._sync.add_event(
                        session, user_id=user_id, event_id=event.event_id, device_id=device_id,
                        project_id=event.project_id, entity_id=event.entity_id,
                        entity_type=event.entity_type, operation=event.operation,
                        revision=event.revision, updated_at=event.updated_at,
                        deleted_at=event.deleted_at, server_sequence=state.current_sequence,
                    )
                    # C13's composite FK is intentionally schema-only: the
                    # ORM models have no relationship that would order these
                    # inserts for us.  Flush the metadata event first, while
                    # keeping both inserts in this one transaction.
                    session.flush()
                    self._sync.add_encrypted_object(
                        session, user_id=user_id, event_id=event.event_id,
                        crypto_version=item.object.crypto_version, aad_version=item.object.aad_version,
                        nonce=decode_canonical_base64url(item.object.nonce, expected_length=24),
                        ciphertext=decode_canonical_base64url(item.object.ciphertext, minimum_length=16),
                    )
                    session.flush()
                    results.append(SyncPushResult(event.event_id, row.server_sequence, False))
                return results, state.current_sequence
        except Exception:
            session.rollback()
            raise

    def pull(self, session: Session, user_id: object, device_id: object, since: int, limit: int, *, transport_version: int = 1):
        device = self._sync.get_device(session, user_id, device_id)
        if device is None:
            raise SyncProtocolError('sync_device_not_registered', 'Sync device is not registered.')
        state = self._sync.ensure_user_state(session, user_id)
        self.require_compression_reader(state, device)
        self._require_transport_mode(state, transport_version)
        if since > state.current_sequence:
            raise SyncProtocolError('sync_cursor_invalid', 'Cursor is beyond the current server sequence.', 422)
        events = self._sync.pull(session, user_id, since, limit)
        has_more = len(events) > limit
        visible = events[:limit]
        next_cursor = visible[-1].server_sequence if visible else since
        return visible, next_cursor, has_more, state.current_sequence

    @staticmethod
    def _encrypted_pull_inconsistent() -> SyncProtocolError:
        return SyncProtocolError(
            'encrypted_sync_pull_inconsistent',
            'Encrypted sync pull changed during materialization.',
            409,
        )

    @classmethod
    def _validate_materialized_encrypted_prefix(cls, descriptors: list[EncryptedPullDescriptor], rows) -> None:
        if len(rows) != len(descriptors):
            raise cls._encrypted_pull_inconsistent()
        ciphertext_total = 0
        for descriptor, row in zip(descriptors, rows, strict=True):
            event, encrypted, object_row_version = row
            if (event.user_id != descriptor.user_id or event.event_id != descriptor.event_id
                    or event.device_id != descriptor.device_id or event.project_id != descriptor.project_id
                    or event.entity_id != descriptor.entity_id or event.entity_type != descriptor.entity_type
                    or event.operation != descriptor.operation or event.revision != descriptor.revision
                    or event.updated_at != descriptor.updated_at or event.deleted_at != descriptor.deleted_at
                    or event.server_sequence != descriptor.server_sequence):
                raise cls._encrypted_pull_inconsistent()
            expected_object = descriptor.object_event_id is not None
            if (expected_object != (encrypted is not None)
                    or expected_object != (object_row_version is not None)):
                raise cls._encrypted_pull_inconsistent()
            if encrypted is None:
                continue
            actual_size = len(encrypted.ciphertext)
            if (encrypted.event_id != descriptor.object_event_id
                    or object_row_version != descriptor.object_row_version
                    or encrypted.crypto_version != descriptor.crypto_version
                    or encrypted.aad_version != descriptor.aad_version
                    or encrypted.nonce != descriptor.nonce
                    or actual_size != descriptor.ciphertext_size
                    or actual_size > MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES):
                raise cls._encrypted_pull_inconsistent()
            ciphertext_total += actual_size
            if ciphertext_total > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES:
                raise cls._encrypted_pull_inconsistent()

    def pull_encrypted(self, session: Session, user_id: object, device_id: object, since: int, limit: int, *, transport_version: int = 1):
        device = self._sync.get_device(session, user_id, device_id)
        if device is None:
            raise SyncProtocolError('sync_device_not_registered', 'Sync device is not registered.')
        state = self._sync.ensure_user_state(session, user_id)
        self.require_compression_reader(state, device)
        self._require_transport_mode(state, transport_version)
        if since > state.current_sequence:
            raise SyncProtocolError('sync_cursor_invalid', 'Cursor is beyond the current server sequence.', 422)
        if transport_version == 3 and not self.content_note_reader_ready(device):
            emitted = session.scalar(select(SyncEvent.event_id).where(
                SyncEvent.user_id == user_id, SyncEvent.entity_type == 'note', SyncEvent.operation == 'event').limit(1))
            if emitted is not None:
                raise SyncProtocolError('content_note_reader_required', 'This device must support Note readers.', 409)
        if transport_version == 3 and not self.map_reader_ready(device):
            emitted = session.scalar(select(SyncEvent.event_id).where(
                SyncEvent.user_id == user_id, SyncEvent.entity_type == 'map').limit(1))
            if emitted is not None:
                raise SyncProtocolError('map_reader_required', 'This device must support map readers.', 409)
        if transport_version == 3 and not self.document_reader_ready(device):
            emitted = session.scalar(select(SyncEvent.event_id).where(
                SyncEvent.user_id == user_id, SyncEvent.entity_type == 'document').limit(1))
            if emitted is not None:
                raise SyncProtocolError('document_reader_required', 'This device must support document readers.', 409)
        if transport_version == 3 and not self.progress_reader_ready(device):
            emitted = session.scalar(select(SyncEvent.event_id).where(
                SyncEvent.user_id == user_id, SyncEvent.entity_type == 'progress').limit(1))
            if emitted is not None:
                raise SyncProtocolError('progress_reader_required', 'This device must support progress readers.', 409)
        visible, next_cursor, has_more = self._materialize_encrypted_page(session, user_id, since, limit)
        return visible, next_cursor, has_more, state.current_sequence

    def _materialize_encrypted_page(self, session: Session, user_id: object, since: int, limit: int):
        """Materialize one opaque page after the caller has enforced its access policy.

        This helper does not authorize reads or modify device capabilities. The
        existing pull entry point performs all gates before invoking it.
        """
        descriptors = self._sync.pull_encrypted_descriptors(session, user_id, since, limit)
        selected_descriptors: list[EncryptedPullDescriptor] = []
        ciphertext_total = 0
        stopped_for_size = False
        for row in descriptors[:limit]:
            descriptor = EncryptedPullDescriptor.from_row(row)
            if descriptor.object_event_id is not None and (
                    descriptor.ciphertext_size is None or descriptor.object_row_version is None):
                raise self._encrypted_pull_inconsistent()
            ciphertext_size = descriptor.ciphertext_size if descriptor.object_event_id is not None else 0
            if ciphertext_size is not None and ciphertext_size > MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES:
                if not selected_descriptors:
                    raise SyncProtocolError(
                        'encrypted_sync_object_too_large', 'Encrypted sync object exceeds size limit.', 413,
                    )
                stopped_for_size = True
                break
            if ciphertext_size is not None and ciphertext_total + ciphertext_size > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES:
                stopped_for_size = True
                break
            ciphertext_total += ciphertext_size or 0
            selected_descriptors.append(descriptor)
        selected_event_ids = [descriptor.event_id for descriptor in selected_descriptors]
        materialized = self._sync.pull_encrypted_objects(session, user_id, selected_event_ids)
        self._validate_materialized_encrypted_prefix(selected_descriptors, materialized)
        visible = [(row[0], row[1]) for row in materialized]
        has_more = stopped_for_size or len(descriptors) > len(selected_descriptors)
        next_cursor = visible[-1][0].server_sequence if visible else since
        return visible, next_cursor, has_more

    def pull_encrypted_v2(self, session: Session, user_id: object, device_id: object, since: int, limit: int):
        rows, next_cursor, has_more, high_water = self.pull_encrypted(
            session, user_id, device_id, since, limit, transport_version=2,
        )
        # V2 has no safe representation for legacy metadata-only/future events.
        # Refuse before emitting a partial page or advancing a client cursor.
        for event, encrypted in rows:
            if (encrypted is None or event.entity_type != 'note'
                    or event.operation not in ('upsert', 'delete', 'resolution')):
                raise SyncProtocolError(
                    'encrypted_sync_event_incomplete',
                    'V2 encrypted pull encountered an event without a supported opaque object.',
                    409,
                )
        return rows, next_cursor, has_more, high_water

    def pull_encrypted_v3(self, session: Session, user_id: object, device_id: object, since: int, limit: int):
        rows, next_cursor, has_more, high_water = self.pull_encrypted(
            session, user_id, device_id, since, limit, transport_version=3,
        )
        self._validate_v3_encrypted_rows(rows)
        return rows, next_cursor, has_more, high_water

    @staticmethod
    def _validate_v3_encrypted_rows(rows) -> None:
        from .schemas import ACCOUNT_ENTITY_TYPES
        for event, encrypted in rows:
            if event.project_id is None:
                if event.entity_type not in ACCOUNT_ENTITY_TYPES or event.operation not in ('upsert', 'delete') or encrypted is None or encrypted.crypto_version != 2 or encrypted.aad_version != 2:
                    raise SyncProtocolError('encrypted_sync_event_incomplete', 'Invalid account object.', 409)
                continue
            if (encrypted is None or event.entity_type not in ('note', 'project_metadata', 'stage', 'stage_order', 'map', 'document', 'progress', 'project_game')
                    or event.operation not in ('upsert', 'delete', 'resolution', 'event')
                    or (event.operation == 'event' and event.entity_type not in ('note', 'map', 'document', 'progress', 'project_game'))
                    or (event.entity_type == 'map' and (event.operation != 'event'
                        or event.deleted_at is not None or not is_map_entity_id(event.entity_id)))
                    or (event.entity_type in ('document', 'progress', 'project_game') and (event.operation != 'event' or event.deleted_at is not None))
                    or (event.entity_type == 'project_metadata' and event.entity_id != event.project_id)
                    or (event.entity_type == 'stage' and event.operation not in ('upsert', 'delete'))
                    or (event.entity_type == 'stage_order' and (event.entity_id != 'stage_order' or event.operation != 'upsert'))):
                raise SyncProtocolError(
                    'encrypted_sync_event_incomplete',
                    'V3 encrypted pull encountered an event without a supported opaque object.', 409,
                )
    def ack(self, session: Session, user_id: object, device_id: object, cursor: int, *, transport_version: int = 1) -> int:
        try:
            session.commit()
            with session.begin():
                state = self._sync.ensure_user_state(session, user_id, lock=True)
                device = self._registered_device(session, user_id, device_id)
                self.require_compression_reader(state, device)
                self._require_transport_mode(state, transport_version)
                if cursor > state.current_sequence:
                    raise SyncProtocolError('sync_cursor_invalid', 'Cursor is beyond the current server sequence.', 422)
                if cursor > device.last_ack_sequence:
                    device.last_ack_sequence = cursor
                return device.last_ack_sequence
        except Exception:
            session.rollback()
            raise


class AccountService:
    """Creation primitive for C3 registration; C2 exposes no registration route."""

    def __init__(self, passwords: PasswordService | None = None) -> None:
        self._passwords = passwords or _DEFAULT_PASSWORDS
        self._users = UserRepository()

    def create_user(self, session: Session, *, username: str, email: str, password: str,
                    role: str = 'user', status: str = 'pending') -> User:
        return self._users.create(session, username=username, email=email,
                                  password_hash=self._passwords.hash(password),
                                  role=role, status=status)


class RegistrationService:
    """C4 public sign-up policy; delivery happens after its committed transaction."""

    def __init__(self, tokens: TokenService, passwords: PasswordService | None = None,
                 now_provider=utc_now) -> None:
        self._tokens = tokens
        self._passwords = passwords or _DEFAULT_PASSWORDS
        self._now = now_provider
        self._users = UserRepository()
        self._settings = RegistrationSettingsRepository()
        self._reserved_usernames = ReservedUsernameRepository()

    def public_policy(self, session: Session) -> RegistrationSettings:
        settings = self._settings.get(session)
        if settings is None:
            raise RuntimeError('Registration settings are unavailable.')
        return settings

    def register(self, session: Session, *, username: str, email: str, password: str,
                 email_delivery_available: bool) -> RegistrationResult:
        # CLOSED and unavailable production registration are public policy
        # outcomes, so reject them before creating an avoidable Argon2 DoS
        # surface. This read is deliberately not the authoritative decision:
        # the locked re-check below governs creation if policy changes while
        # Argon2 is running.
        settings = self.public_policy(session)
        if settings.mode == 'closed':
            return RegistrationResult('registration_closed')
        if not email_delivery_available:
            raise RegistrationUnavailableError()

        # Preserve C2's public password-validation contract while avoiding
        # Argon2 work for a known policy-rejected username.
        self._passwords.validate_new_password(password)
        if self._reserved_usernames.is_reserved(session, username):
            return RegistrationResult('username_reserved')
        session.commit()

        # Preserve C2 password cost for accepted and duplicate requests without
        # holding the shared policy lock through Argon2 work.
        password_hash = self._passwords.hash(password)
        try:
            with session.begin():
                settings = self._settings.get(session, lock=True)
                if settings is None:
                    raise RegistrationUnavailableError()
                if settings.mode == 'closed':
                    return RegistrationResult('registration_closed')
                if not email_delivery_available:
                    raise RegistrationUnavailableError()
                # The authoritative claim happens after Argon2 and is shared
                # with C7 reservation management. Never hold it during hashing.
                lock_username_namespace(session, username)
                if self._reserved_usernames.is_reserved(session, username):
                    return RegistrationResult('username_reserved')
                token_id, raw_token, token_hash = self._tokens.issue_email_verification_token()
                now = self._now()
                user = self._users.create(
                    session, username=username, email=email, password_hash=password_hash,
                    role='user', status='pending', registration_mode_at_signup=settings.mode,
                )
                session.flush()
                session.add(EmailVerificationToken(
                    id=token_id, user_id=user.id, token_hash=token_hash,
                    email_normalized=user.email_normalized, created_at=now,
                    expires_at=now + EMAIL_VERIFICATION_TOKEN_LIFETIME,
                ))
                recipient = user.email
            return RegistrationResult('registration_request_accepted', recipient, raw_token)
        except IntegrityError:
            session.rollback()
            return RegistrationResult('registration_request_accepted')

    def issue_public_verification(self, session: Session, email: str) -> tuple[str, str] | None:
        """Issue only for an eligible C4 registrant under the existing C3 lock/throttle."""
        return AccountEmailService(self._tokens, self._passwords, self._now).issue_verification_for_public_email(
            session, email,
        )


class AuthenticationService:
    def __init__(self, token_service: TokenService, passwords: PasswordService | None = None) -> None:
        self._tokens = token_service
        self._passwords = passwords or _DEFAULT_PASSWORDS
        self._users = UserRepository()
        self._auth = AuthRepository()

    @staticmethod
    def _usable(user: User, session: AuthSession, now: datetime) -> bool:
        return user.status == 'active' and session.revoked_at is None and session.expires_at > now

    def login(self, session: Session, *, username: str, password: str) -> IssuedTokens:
        user = self._users.get_by_normalized_username(session, username)
        if user is None:
            self._passwords.verify_dummy(password)
            raise AuthenticationError()
        valid, updated_hash = self._passwords.verify(password, user.password_hash)
        if not valid or user.status != 'active':
            raise AuthenticationError()
        if updated_hash is not None:
            user.password_hash = updated_hash
        now = utc_now()
        auth_session = AuthSession(user_id=user.id, expires_at=now + SESSION_LIFETIME, last_used_at=now)
        session.add(auth_session)
        session.flush()
        issued, raw_refresh_token = self._issue_refresh(session, auth_session, now)
        session.commit()
        return self._issued(user, auth_session, raw_refresh_token)

    def refresh(self, session: Session, raw_token: str) -> IssuedTokens:
        parsed = self._tokens.parse_refresh_token(raw_token)
        if parsed is None:
            raise AuthenticationError()
        token_id, secret = parsed
        now = utc_now()
        try:
            replay_detected = False
            result: IssuedTokens | None = None
            with session.begin():
                refresh = self._auth.get_refresh_token(session, token_id, lock=True)
                if refresh is None:
                    raise AuthenticationError()
                auth_session = self._auth.get_session(session, refresh.session_id, lock=True)
                if auth_session is None:
                    raise AuthenticationError()
                # Any previously consumed token signals replay, even if a prior
                # rotation already revoked it. Locking serializes concurrent use.
                if refresh.used_at is not None or refresh.replaced_by_id is not None:
                    self._revoke_session(session, auth_session, now)
                    replay_detected = True
                else:
                    user = self._users.get_by_id(session, auth_session.user_id)
                    if (user is None or not self._usable(user, auth_session, now)
                            or refresh.revoked_at is not None or refresh.expires_at <= now
                            or not self._tokens.verify_refresh_secret(secret, refresh.token_hash)):
                        raise AuthenticationError()
                    refresh.used_at = now
                    refresh.revoked_at = now
                    auth_session.last_used_at = now
                    replacement, raw_refresh_token = self._issue_refresh(session, auth_session, now)
                    refresh.replaced_by_id = replacement.id
                    session.flush()
                    result = self._issued(user, auth_session, raw_refresh_token)
            if replay_detected or result is None:
                raise AuthenticationError()
            return result
        except AuthenticationError:
            session.rollback()
            raise

    def logout(self, session: Session, auth_session: AuthSession) -> None:
        now = utc_now()
        try:
            locked = self._auth.get_session(session, auth_session.id, lock=True)
            if locked is not None:
                self._revoke_session(session, locked, now)
            session.commit()
        except Exception:
            session.rollback()
            raise

    def _issue_refresh(self, session: Session, auth_session: AuthSession, now: datetime) -> tuple[AuthRefreshToken, str]:
        token_id, raw_token, token_hash = self._tokens.issue_refresh_token()
        token = AuthRefreshToken(id=token_id, session_id=auth_session.id, token_hash=token_hash,
                                 expires_at=auth_session.expires_at)
        session.add(token)
        session.flush()
        return token, raw_token

    def _issued(self, user: User, auth_session: AuthSession, raw_token: str) -> IssuedTokens:
        return IssuedTokens(access_token=self._tokens.issue_access_token(user.id, auth_session.id),
                            refresh_token=raw_token)

    def _revoke_session(self, session: Session, auth_session: AuthSession, now: datetime) -> None:
        if auth_session.revoked_at is None:
            auth_session.revoked_at = now
        self._auth.revoke_active_tokens(session, auth_session.id, now)


class AdminService:
    """Small C7 administration service over the existing C2--C6 authority."""

    def __init__(self, now_provider=utc_now) -> None:
        self._now = now_provider
        self._users = UserRepository()
        self._auth = AuthRepository()
        self._settings = RegistrationSettingsRepository()
        self._limits = GlobalLimitsRepository()
        self._overrides = UserLimitOverridesRepository()
        self._reserved = ReservedUsernameRepository()

    @staticmethod
    def _protected(user: User) -> None:
        if user.role == 'admin':
            raise AdminOperationError('admin_account_protected', 'Administrator accounts are protected.')

    @staticmethod
    def _not_found() -> None:
        raise AdminOperationError('user_not_found', 'User was not found.', 404)

    def _locked_user(self, session: Session, user_id: object) -> User:
        user = session.scalar(select(User).where(User.id == user_id).with_for_update())
        if user is None:
            self._not_found()
        return user

    @staticmethod
    def _require_capacity(session: Session, settings: RegistrationSettings) -> None:
        active_count = session.scalar(select(func.count()).select_from(User).where(User.status == 'active')) or 0
        if settings.max_users is not None and active_count >= settings.max_users:
            raise AdminOperationError('user_capacity_reached', 'User capacity has been reached.')

    def approve(self, session: Session, user_id: object) -> User:
        try:
            session.commit()
            with session.begin():
                user = self._locked_user(session, user_id)
                self._protected(user)
                if user.status == 'active':
                    return user
                if user.status not in {'pending', 'rejected'}:
                    raise AdminOperationError('invalid_user_status', 'User cannot be approved in this state.')
                if not user.email_verified:
                    raise AdminOperationError('email_not_verified', 'Email must be verified before activation.')
                settings = self._settings.get(session, lock=True)
                if settings is None:
                    raise AdminOperationError('registration_unavailable', 'Registration settings are unavailable.', 503)
                self._require_capacity(session, settings)
                user.status = 'active'
                return user
        except AdminOperationError:
            session.rollback()
            raise

    def reject(self, session: Session, user_id: object) -> User:
        try:
            session.commit()
            with session.begin():
                user = self._locked_user(session, user_id)
                self._protected(user)
                if user.status == 'rejected':
                    return user
                if user.status != 'pending':
                    raise AdminOperationError('invalid_user_status', 'Only pending users can be rejected.')
                user.status = 'rejected'
                return user
        except AdminOperationError:
            session.rollback()
            raise

    def block(self, session: Session, user_id: object) -> User:
        try:
            session.commit()
            with session.begin():
                user = self._locked_user(session, user_id)
                self._protected(user)
                if user.status == 'blocked':
                    return user
                if user.status != 'active':
                    raise AdminOperationError('invalid_user_status', 'Only active users can be blocked.')
                user.status = 'blocked'
                self._auth.revoke_user_sessions(session, user.id, self._now())
                return user
        except AdminOperationError:
            session.rollback()
            raise

    def unblock(self, session: Session, user_id: object) -> User:
        try:
            session.commit()
            with session.begin():
                user = self._locked_user(session, user_id)
                self._protected(user)
                if user.status == 'active':
                    return user
                if user.status != 'blocked':
                    raise AdminOperationError('invalid_user_status', 'Only blocked users can be unblocked.')
                if not user.email_verified:
                    raise AdminOperationError('email_not_verified', 'Email must be verified before activation.')
                settings = self._settings.get(session, lock=True)
                if settings is None:
                    raise AdminOperationError('registration_unavailable', 'Registration settings are unavailable.', 503)
                self._require_capacity(session, settings)
                user.status = 'active'
                return user
        except AdminOperationError:
            session.rollback()
            raise

    def revoke_sessions(self, session: Session, user_id: object) -> tuple[int, int]:
        try:
            session.commit()
            with session.begin():
                user = self._locked_user(session, user_id)
                self._protected(user)
                return self._auth.revoke_user_sessions(session, user.id, self._now())
        except AdminOperationError:
            session.rollback()
            raise

    def update_registration(self, session: Session, *, mode: str | None, max_users: int | None | object) -> RegistrationSettings:
        if mode is not None and mode != 'closed':
            raise AdminOperationError('registration_locked', 'Public registration remains closed.', 409)
        session.commit()
        with session.begin():
            settings = self._settings.get(session, lock=True)
            if settings is None:
                raise AdminOperationError('registration_unavailable', 'Registration settings are unavailable.', 503)
            if mode is not None:
                settings.mode = mode
            if max_users is not _UNSET:
                settings.max_users = max_users
            return settings

    def update_global_limits(self, session: Session, max_cloud_projects: int) -> GlobalLimits:
        session.commit()
        with session.begin():
            limits = self._limits.get(session, lock=True)
            if limits is None:
                raise AdminOperationError('limits_unavailable', 'Limits are unavailable.', 503)
            limits.max_cloud_projects = max_cloud_projects
            return limits

    def update_override(self, session: Session, user_id: object, value: int | None) -> tuple[int | None, int]:
        try:
            session.commit()
            with session.begin():
                user = self._locked_user(session, user_id)
                limits = self._limits.get(session, lock=True)
                if limits is None:
                    raise AdminOperationError('limits_unavailable', 'Limits are unavailable.', 503)
                if value is None:
                    self._overrides.clear(session, user.id)
                    return None, limits.max_cloud_projects
                row = self._overrides.get(session, user.id)
                if row is None:
                    session.add(UserLimitOverrides(user_id=user.id, max_cloud_projects_override=value))
                else:
                    row.max_cloud_projects_override = value
                return value, value
        except AdminOperationError:
            session.rollback()
            raise

    def add_reserved_username(self, session: Session, username: str) -> ReservedUsername:
        normalized = normalize_username(username)
        try:
            session.commit()
            with session.begin():
                lock_username_namespace(session, normalized)
                existing = session.get(ReservedUsername, normalized)
                if existing is not None:
                    return existing
                if self._users.get_by_normalized_username(session, normalized) is not None:
                    raise AdminOperationError('username_in_use', 'Username is already in use.')
                row = self._reserved.add(session, normalized)
                session.flush()
                return row
        except AdminOperationError:
            session.rollback()
            raise

    def remove_reserved_username(self, session: Session, username: str) -> bool:
        normalized = normalize_username(username)
        session.commit()
        with session.begin():
            lock_username_namespace(session, normalized)
            return self._reserved.remove(session, normalized)


_UNSET = object()


class AccountEmailService:
    """C3 token issuance/consumption. Email delivery stays outside DB transactions."""
    ISSUE_LIMIT = 5

    def __init__(self, tokens: TokenService, passwords: PasswordService | None = None,
                 now_provider=utc_now) -> None:
        self._tokens = tokens
        self._passwords = passwords or _DEFAULT_PASSWORDS
        self._now = now_provider

    def issue_verification(self, session: Session, user: User) -> str | None:
        now = self._now()
        user_id = user.id
        # Authentication dependencies may already have opened a read transaction.
        session.commit()
        token_id, raw, token_hash = self._tokens.issue_email_verification_token()
        with session.begin():
            locked_user = session.scalar(select(User).where(User.id == user_id).with_for_update())
            if locked_user is None or locked_user.email_verified:
                return None
            if self._is_throttled(session, EmailVerificationToken, locked_user.id, now):
                return None
            session.execute(update(EmailVerificationToken).where(
                EmailVerificationToken.user_id == locked_user.id,
                EmailVerificationToken.email_normalized == locked_user.email_normalized,
                EmailVerificationToken.used_at.is_(None), EmailVerificationToken.revoked_at.is_(None),
            ).values(revoked_at=now))
            session.add(EmailVerificationToken(id=token_id, user_id=locked_user.id, token_hash=token_hash,
                email_normalized=locked_user.email_normalized,
                created_at=now, expires_at=now + EMAIL_VERIFICATION_TOKEN_LIFETIME))
        return raw

    def issue_verification_for_public_email(self, session: Session, email: str) -> tuple[str, str] | None:
        normalized = normalize_email(email)
        now = self._now()
        session.commit()
        token_id, raw, token_hash = self._tokens.issue_email_verification_token()
        try:
            with session.begin():
                user = session.scalar(select(User).where(User.email_normalized == normalized).with_for_update())
                if (user is None or user.registration_mode_at_signup not in {'open', 'approval'}
                        or user.status != 'pending' or user.email_verified):
                    return None
                if self._is_throttled(session, EmailVerificationToken, user.id, now):
                    return None
                session.execute(update(EmailVerificationToken).where(
                    EmailVerificationToken.user_id == user.id,
                    EmailVerificationToken.email_normalized == user.email_normalized,
                    EmailVerificationToken.used_at.is_(None), EmailVerificationToken.revoked_at.is_(None),
                ).values(revoked_at=now))
                session.add(EmailVerificationToken(
                    id=token_id, user_id=user.id, token_hash=token_hash,
                    email_normalized=user.email_normalized, created_at=now,
                    expires_at=now + EMAIL_VERIFICATION_TOKEN_LIFETIME,
                ))
                recipient = user.email
            return recipient, raw
        except Exception:
            session.rollback()
            raise

    def issue_password_reset(self, session: Session, email: str) -> tuple[str, str] | None:
        from .repositories import normalize_email
        normalized = normalize_email(email)
        now = self._now()
        session.commit()
        token_id, raw, token_hash = self._tokens.issue_password_reset_token()
        try:
            with session.begin():
                user = session.scalar(select(User).where(
                    User.email_normalized == normalized).with_for_update())
                if user is None or user.status != 'active':
                    return None
                if self._is_throttled(session, PasswordResetToken, user.id, now):
                    return None
                user_id, email = user.id, user.email
                session.execute(update(PasswordResetToken).where(
                    PasswordResetToken.user_id == user_id, PasswordResetToken.used_at.is_(None),
                    PasswordResetToken.revoked_at.is_(None),
                ).values(revoked_at=now))
                session.add(PasswordResetToken(id=token_id, user_id=user_id, token_hash=token_hash,
                    created_at=now, expires_at=now + PASSWORD_RESET_TOKEN_LIFETIME))
            return email, raw
        except Exception:
            session.rollback()
            raise

    def verify_email(self, session: Session, raw_token: str) -> EmailVerificationResult:
        parsed = self._tokens.parse_opaque_token(raw_token, 'ev1')
        if parsed is None:
            raise RecoveryTokenError()
        token_id, secret = parsed
        now = self._now()
        try:
            with session.begin():
                token = session.scalar(select(EmailVerificationToken).where(
                    EmailVerificationToken.id == token_id).with_for_update())
                if token is None or token.used_at or token.revoked_at or token.expires_at <= now \
                        or not self._tokens.verify_refresh_secret(secret, token.token_hash):
                    raise RecoveryTokenError()
                user = session.scalar(select(User).where(User.id == token.user_id).with_for_update())
                if user is None or user.email_normalized != token.email_normalized:
                    raise RecoveryTokenError()
                user.email_verified = True
                token.used_at = now
                if user.registration_mode_at_signup == 'open':
                    settings = session.scalar(select(RegistrationSettings).where(
                        RegistrationSettings.id == RegistrationSettingsRepository.SINGLETON_ID,
                    ).with_for_update())
                    if settings is None:
                        raise RecoveryTokenError()
                    active_count = session.scalar(select(func.count()).select_from(User).where(
                        User.status == 'active',
                    ))
                    if settings.max_users is None or active_count < settings.max_users:
                        user.status = 'active'
                        result = EmailVerificationResult('active', 'active')
                    else:
                        result = EmailVerificationResult('pending', 'capacity_reached')
                elif user.registration_mode_at_signup == 'approval':
                    result = EmailVerificationResult('pending', 'approval_required')
                else:
                    result = EmailVerificationResult(user.status, 'unchanged')
            return result
        except RecoveryTokenError:
            session.rollback()
            raise

    def confirm_password_reset(self, session: Session, raw_token: str, new_password: str) -> str:
        self._passwords.validate_new_password(new_password)
        parsed = self._tokens.parse_opaque_token(raw_token, 'pr1')
        if parsed is None:
            raise RecoveryTokenError()
        token_id, secret = parsed
        now = self._now()
        try:
            with session.begin():
                token = session.scalar(select(PasswordResetToken).where(
                    PasswordResetToken.id == token_id).with_for_update())
                if token is None or token.used_at or token.revoked_at or token.expires_at <= now \
                        or not self._tokens.verify_refresh_secret(secret, token.token_hash):
                    raise RecoveryTokenError()
                user = session.scalar(select(User).where(User.id == token.user_id).with_for_update())
                if user is None or user.status != 'active':
                    raise RecoveryTokenError()
                email = user.email
                user.password_hash = self._passwords.hash(new_password)
                token.used_at = now
                session.execute(update(PasswordResetToken).where(
                    PasswordResetToken.user_id == user.id, PasswordResetToken.id != token.id,
                    PasswordResetToken.used_at.is_(None), PasswordResetToken.revoked_at.is_(None),
                ).values(revoked_at=now))
                session.execute(update(AuthSession).where(AuthSession.user_id == user.id,
                    AuthSession.revoked_at.is_(None)).values(revoked_at=now))
                session.execute(update(AuthRefreshToken).where(AuthRefreshToken.session_id.in_(
                    select(AuthSession.id).where(AuthSession.user_id == user.id)),
                    AuthRefreshToken.revoked_at.is_(None)).values(revoked_at=now))
            return email
        except RecoveryTokenError:
            session.rollback()
            raise

    def _is_throttled(self, session: Session, model, user_id, now: datetime) -> bool:
        # Token history is PostgreSQL-backed, shared across workers, and is audit retained.
        cutoff = now - timedelta(hours=1)
        recent = session.scalars(select(model.created_at).where(
            model.user_id == user_id, model.created_at >= cutoff).order_by(model.created_at.desc())).all()
        if len(recent) >= self.ISSUE_LIMIT:
            return True
        return bool(recent and recent[0] > now - timedelta(seconds=60))
