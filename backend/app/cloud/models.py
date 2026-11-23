from __future__ import annotations

import uuid
from datetime import datetime

from sqlalchemy import (BIGINT, Boolean, CheckConstraint, DateTime, ForeignKey,
                        ForeignKeyConstraint, Integer, LargeBinary, String,
                        UniqueConstraint, text)
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column


class Base(DeclarativeBase):
    pass


UTC_NOW = text("timezone('utc', now())")


class User(Base):
    __tablename__ = 'users'
    __table_args__ = (
        CheckConstraint("role IN ('user', 'admin')", name='ck_users_role'),
        CheckConstraint(
            "status IN ('pending', 'active', 'rejected', 'blocked')",
            name='ck_users_status',
        ),
        CheckConstraint(
            "registration_mode_at_signup IS NULL OR registration_mode_at_signup IN ('open', 'approval')",
            name='ck_users_registration_mode_at_signup',
        ),
    )

    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    username: Mapped[str] = mapped_column(String(128), nullable=False)
    username_normalized: Mapped[str] = mapped_column(String(128), unique=True, nullable=False)
    email: Mapped[str] = mapped_column(String(320), nullable=False)
    email_normalized: Mapped[str] = mapped_column(String(320), unique=True, nullable=False)
    email_verified: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))
    password_hash: Mapped[str] = mapped_column(String(512), nullable=False)
    role: Mapped[str] = mapped_column(String(16), nullable=False, default='user', server_default=text("'user'"))
    status: Mapped[str] = mapped_column(String(16), nullable=False, default='pending', server_default=text("'pending'"))
    registration_mode_at_signup: Mapped[str | None] = mapped_column(String(16))
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW, onupdate=UTC_NOW)


class RegistrationSettings(Base):
    """The one PostgreSQL-backed authority for public registration policy."""

    __tablename__ = 'registration_settings'
    __table_args__ = (
        CheckConstraint('id = 1', name='ck_registration_settings_singleton'),
        CheckConstraint("mode IN ('open', 'approval', 'closed')", name='ck_registration_settings_mode'),
        CheckConstraint('max_users IS NULL OR max_users >= 0', name='ck_registration_settings_max_users'),
    )

    id: Mapped[int] = mapped_column(Integer, primary_key=True, default=1)
    mode: Mapped[str] = mapped_column(String(16), nullable=False)
    max_users: Mapped[int | None] = mapped_column(Integer)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW,
                                                   onupdate=UTC_NOW)


class ReservedUsername(Base):
    """A public-registration policy record, deliberately separate from User."""

    __tablename__ = 'reserved_usernames'

    username_normalized: Mapped[str] = mapped_column(String(128), primary_key=True)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=UTC_NOW,
    )


class GlobalLimits(Base):
    """The singleton PostgreSQL authority for per-user cloud resource defaults."""

    __tablename__ = 'global_limits'
    __table_args__ = (
        CheckConstraint('id = 1', name='ck_global_limits_singleton'),
        CheckConstraint('max_cloud_projects >= 0', name='ck_global_limits_max_cloud_projects'),
    )

    id: Mapped[int] = mapped_column(Integer, primary_key=True, default=1)
    max_cloud_projects: Mapped[int] = mapped_column(Integer, nullable=False)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW,
                                                   onupdate=UTC_NOW)


class UserLimitOverrides(Base):
    __tablename__ = 'user_limit_overrides'
    __table_args__ = (
        CheckConstraint(
            'max_cloud_projects_override IS NULL OR max_cloud_projects_override >= 0',
            name='ck_user_limit_overrides_max_cloud_projects',
        ),
    )

    user_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey('users.id', ondelete='CASCADE'), primary_key=True,
    )
    max_cloud_projects_override: Mapped[int | None] = mapped_column(Integer)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW,
                                                   onupdate=UTC_NOW)


class CloudProject(Base):
    """An owner's cloud-slot reservation; project content never belongs here."""

    __tablename__ = 'cloud_projects'
    __table_args__ = (
        CheckConstraint("char_length(project_id) >= 1", name='ck_cloud_projects_project_id_not_empty'),
        CheckConstraint(
            "bootstrap_state IN ('legacy', 'initializing', 'active')",
            name='ck_cloud_projects_bootstrap_state',
        ),
        CheckConstraint(
            "(bootstrap_state = 'legacy' AND bootstrap_id IS NULL AND bootstrap_device_id IS NULL "
            "AND bootstrap_completed_at IS NULL AND initial_event_count IS NULL "
            "AND initial_max_server_sequence IS NULL) OR "
            "(bootstrap_state = 'initializing' AND bootstrap_id IS NOT NULL "
            "AND bootstrap_device_id IS NOT NULL AND bootstrap_completed_at IS NULL "
            "AND initial_event_count IS NULL AND initial_max_server_sequence IS NULL) OR "
            "(bootstrap_state = 'active' AND bootstrap_id IS NOT NULL "
            "AND bootstrap_device_id IS NOT NULL AND bootstrap_completed_at IS NOT NULL "
            "AND initial_event_count IS NOT NULL AND initial_event_count >= 0 "
            "AND initial_max_server_sequence IS NOT NULL AND initial_max_server_sequence >= 0)",
            name='ck_cloud_projects_bootstrap_shape',
        ),
        UniqueConstraint('user_id', 'bootstrap_id', name='uq_cloud_projects_user_bootstrap_id'),
    )

    user_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey('users.id', ondelete='CASCADE'), primary_key=True,
    )
    # Legacy, SQLite and Tauri project IDs are string contracts.  Keep their
    # established 32-hex IDs and any compatible historical values losslessly.
    project_id: Mapped[str] = mapped_column(String(512), primary_key=True)
    retired_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    bootstrap_id: Mapped[uuid.UUID | None] = mapped_column()
    bootstrap_device_id: Mapped[uuid.UUID | None] = mapped_column()
    bootstrap_state: Mapped[str] = mapped_column(
        String(16), nullable=False, default='legacy', server_default=text("'legacy'"),
    )
    initial_event_count: Mapped[int | None] = mapped_column(BIGINT)
    initial_max_server_sequence: Mapped[int | None] = mapped_column(BIGINT)
    bootstrap_completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=UTC_NOW,
    )


class SyncDevice(Base):
    """A non-secret, account-scoped C9 transport identity."""

    __tablename__ = 'sync_devices'
    __table_args__ = (
        CheckConstraint('metadata_cover_reader_version IN (0, 2)', name='ck_sync_devices_metadata_cover_reader_version'),
        CheckConstraint('reader_transport_version IN (2, 3)', name='ck_sync_devices_reader_transport_version'),
        CheckConstraint('note_frame_version IN (0, 1)', name='ck_sync_devices_note_frame_version'),
        CheckConstraint('note_codec_version IN (0, 1)', name='ck_sync_devices_note_codec_version'),
        CheckConstraint('note_ordinary_reader_version IN (0, 1)', name='ck_sync_devices_note_ordinary_reader_version'),
        CheckConstraint('note_resolution_reader_version IN (0, 2)', name='ck_sync_devices_note_resolution_reader_version'),
        CheckConstraint('map_frame_version IN (0, 1)', name='ck_sync_devices_map_frame_version'),
        CheckConstraint('map_codec_version IN (0, 1)', name='ck_sync_devices_map_codec_version'),
        CheckConstraint('map_reader_version IN (0, 1)', name='ck_sync_devices_map_reader_version'),
        CheckConstraint('document_frame_version IN (0, 1)', name='ck_sync_devices_document_frame_version'),
        CheckConstraint('document_codec_version IN (0, 1)', name='ck_sync_devices_document_codec_version'),
        CheckConstraint('document_reader_version IN (0, 1)', name='ck_sync_devices_document_reader_version'),
        CheckConstraint('progress_frame_version IN (0, 1)', name='ck_sync_devices_progress_frame_version'),
        CheckConstraint('progress_codec_version IN (0, 1)', name='ck_sync_devices_progress_codec_version'),
        CheckConstraint('progress_reader_version IN (0, 1)', name='ck_sync_devices_progress_reader_version'),
        CheckConstraint('project_game_frame_version IN (0, 1)', name='ck_sync_devices_project_game_frame_version'),
        CheckConstraint('project_game_codec_version IN (0, 1)', name='ck_sync_devices_project_game_codec_version'),
        CheckConstraint('project_game_reader_version IN (0, 1)', name='ck_sync_devices_project_game_reader_version'),
        CheckConstraint('account_game_frame_version IN (0, 1)', name='ck_sync_devices_account_game_frame_version'),
        CheckConstraint('account_game_codec_version IN (0, 1, 2)', name='ck_sync_devices_account_game_codec_version'),
        CheckConstraint('account_game_reader_version IN (0, 1, 2)', name='ck_sync_devices_account_game_reader_version'),
    )

    user_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey('users.id', ondelete='CASCADE'), primary_key=True,
    )
    device_id: Mapped[uuid.UUID] = mapped_column(primary_key=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    last_seen_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    last_ack_sequence: Mapped[int] = mapped_column(BIGINT, nullable=False, default=0, server_default=text('0'))
    compression_id1: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))
    metadata_cover_reader_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    reader_transport_version: Mapped[int] = mapped_column(Integer, nullable=False, default=2, server_default=text('2'))
    note_frame_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    note_codec_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    note_compression_zero: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))
    note_ordinary_reader_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    note_resolution_reader_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    map_frame_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    map_codec_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    map_reader_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    map_compression_zero: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))
    document_frame_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    document_codec_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    document_reader_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    document_compression_zero: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))
    progress_frame_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    progress_codec_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    progress_reader_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    progress_compression_zero: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))
    project_game_frame_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    project_game_codec_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    project_game_reader_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    project_game_compression_zero: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))
    account_game_frame_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    account_game_codec_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    account_game_reader_version: Mapped[int] = mapped_column(Integer, nullable=False, default=0, server_default=text('0'))
    account_game_compression_zero: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))



class SyncUserState(Base):
    __tablename__ = 'sync_user_state'
    __table_args__ = (
        CheckConstraint('current_sequence >= 0', name='ck_sync_user_state_sequence_nonnegative'),
        CheckConstraint('writer_transport_version IN (1, 2, 3)', name='ck_sync_user_state_writer_transport_version'),
        CheckConstraint('cutover_epoch >= 0 AND cutover_epoch <= 9007199254740991', name='ck_sync_user_state_cutover_epoch_safe_integer'),
    )

    user_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id', ondelete='CASCADE'), primary_key=True)
    compression_id1_required: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False, server_default=text('false'))
    current_sequence: Mapped[int] = mapped_column(BIGINT, nullable=False, default=0, server_default=text('0'))
    writer_transport_version: Mapped[int] = mapped_column(Integer, nullable=False, default=1, server_default=text('1'))
    cutover_epoch: Mapped[int] = mapped_column(BIGINT, nullable=False, default=0, server_default=text('0'))


class SyncEvent(Base):
    """Metadata-only transport log. It intentionally has no payload column."""

    __tablename__ = 'sync_events'
    __table_args__ = (
        CheckConstraint("operation IN ('upsert', 'delete', 'event', 'resolution')", name='ck_sync_events_operation'),
        CheckConstraint('revision >= 1', name='ck_sync_events_revision_positive'),
        CheckConstraint('server_sequence > 0', name='ck_sync_events_sequence_positive'),
        CheckConstraint("(operation = 'delete' AND deleted_at IS NOT NULL) OR (operation != 'delete' AND deleted_at IS NULL)", name='ck_sync_events_tombstone'),
        CheckConstraint("(project_id IS NULL AND entity_type IN ('folder','folder_order','folder_membership','project_order','account_game')) OR (project_id IS NOT NULL AND entity_type NOT IN ('folder','folder_order','folder_membership','project_order','account_game'))", name='ck_sync_events_scope'),
        UniqueConstraint('user_id', 'server_sequence', name='uq_sync_events_user_sequence'),
    )

    user_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id', ondelete='CASCADE'), primary_key=True)
    event_id: Mapped[uuid.UUID] = mapped_column(primary_key=True)
    device_id: Mapped[uuid.UUID] = mapped_column(nullable=False)
    project_id: Mapped[str | None] = mapped_column(String(512), nullable=True)
    entity_id: Mapped[str] = mapped_column(String(512), nullable=False)
    entity_type: Mapped[str] = mapped_column(String(128), nullable=False)
    operation: Mapped[str] = mapped_column(String(16), nullable=False)
    revision: Mapped[int] = mapped_column(BIGINT, nullable=False)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    deleted_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    server_sequence: Mapped[int] = mapped_column(BIGINT, nullable=False)
    accepted_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)


class UserCrypto(Base):
    """Wrapped client key material and public crypto metadata; the server never decrypts it."""

    __tablename__ = 'user_crypto'
    __table_args__ = (
        CheckConstraint('password_crypto_version >= 1', name='ck_user_crypto_password_crypto_version_positive'),
        CheckConstraint('password_wrapping_version >= 1', name='ck_user_crypto_password_wrapping_version_positive'),
        CheckConstraint('kdf_version >= 1', name='ck_user_crypto_kdf_version_positive'),
        CheckConstraint('kdf_opslimit >= 1', name='ck_user_crypto_kdf_opslimit_positive'),
        CheckConstraint('kdf_memlimit >= 1', name='ck_user_crypto_kdf_memlimit_positive'),
        CheckConstraint('octet_length(kdf_salt) = 16', name='ck_user_crypto_kdf_salt_length'),
        CheckConstraint('octet_length(password_nonce) = 24', name='ck_user_crypto_password_nonce_length'),
        CheckConstraint('octet_length(password_wrapped_amk) = 48', name='ck_user_crypto_password_wrapped_amk_length'),
        CheckConstraint(
            "(recovery_crypto_version IS NULL AND recovery_wrapping_version IS NULL AND recovery_nonce IS NULL AND recovery_wrapped_amk IS NULL) OR "
            "(recovery_crypto_version IS NOT NULL AND recovery_wrapping_version IS NOT NULL AND recovery_nonce IS NOT NULL AND recovery_wrapped_amk IS NOT NULL)",
            name='ck_user_crypto_recovery_all_or_none',
        ),
        CheckConstraint('recovery_crypto_version IS NULL OR recovery_crypto_version >= 1', name='ck_user_crypto_recovery_crypto_version_positive'),
        CheckConstraint('recovery_wrapping_version IS NULL OR recovery_wrapping_version >= 1', name='ck_user_crypto_recovery_wrapping_version_positive'),
        CheckConstraint('recovery_nonce IS NULL OR octet_length(recovery_nonce) = 24', name='ck_user_crypto_recovery_nonce_length'),
        CheckConstraint('recovery_wrapped_amk IS NULL OR octet_length(recovery_wrapped_amk) = 48', name='ck_user_crypto_recovery_wrapped_amk_length'),
    )

    user_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id', ondelete='CASCADE'), primary_key=True)
    password_crypto_version: Mapped[int] = mapped_column(Integer, nullable=False)
    password_wrapping_version: Mapped[int] = mapped_column(Integer, nullable=False)
    kdf_version: Mapped[int] = mapped_column(Integer, nullable=False)
    kdf_algorithm: Mapped[str] = mapped_column(String(32), nullable=False)
    kdf_salt: Mapped[bytes] = mapped_column(LargeBinary, nullable=False)
    kdf_opslimit: Mapped[int] = mapped_column(BIGINT, nullable=False)
    kdf_memlimit: Mapped[int] = mapped_column(BIGINT, nullable=False)
    password_nonce: Mapped[bytes] = mapped_column(LargeBinary, nullable=False)
    password_wrapped_amk: Mapped[bytes] = mapped_column(LargeBinary, nullable=False)
    recovery_crypto_version: Mapped[int | None] = mapped_column(Integer)
    recovery_wrapping_version: Mapped[int | None] = mapped_column(Integer)
    recovery_nonce: Mapped[bytes | None] = mapped_column(LargeBinary)
    recovery_wrapped_amk: Mapped[bytes | None] = mapped_column(LargeBinary)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW, onupdate=UTC_NOW)


class EncryptedObject(Base):
    """An immutable opaque client ciphertext version; the server never decrypts it."""

    __tablename__ = 'encrypted_objects'
    __table_args__ = (
        CheckConstraint('crypto_version >= 1', name='ck_encrypted_objects_crypto_version_positive'),
        CheckConstraint('aad_version >= 1', name='ck_encrypted_objects_aad_version_positive'),
        CheckConstraint('octet_length(nonce) = 24', name='ck_encrypted_objects_nonce_length'),
        CheckConstraint('octet_length(ciphertext) >= 16', name='ck_encrypted_objects_ciphertext_min_length'),
        ForeignKeyConstraint(['user_id', 'event_id'], ['sync_events.user_id', 'sync_events.event_id'], ondelete='CASCADE'),
    )

    user_id: Mapped[uuid.UUID] = mapped_column(primary_key=True)
    event_id: Mapped[uuid.UUID] = mapped_column(primary_key=True)
    crypto_version: Mapped[int] = mapped_column(Integer, nullable=False)
    aad_version: Mapped[int] = mapped_column(Integer, nullable=False)
    nonce: Mapped[bytes] = mapped_column(LargeBinary, nullable=False)
    ciphertext: Mapped[bytes] = mapped_column(LargeBinary, nullable=False)
    stored_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)


class EncryptedBlob(Base):
    """Metadata for an immutable external ciphertext blob; bytes stay outside PostgreSQL."""

    __tablename__ = 'encrypted_blobs'
    __table_args__ = (
        CheckConstraint("kind = 'project_cover'", name='ck_encrypted_blobs_kind'),
        CheckConstraint('crypto_version >= 1', name='ck_encrypted_blobs_crypto_version_positive'),
        CheckConstraint('aad_version >= 1', name='ck_encrypted_blobs_aad_version_positive'),
        CheckConstraint('octet_length(nonce) = 24', name='ck_encrypted_blobs_nonce_length'),
        CheckConstraint('ciphertext_size >= 16', name='ck_encrypted_blobs_ciphertext_size_min'),
        CheckConstraint('ciphertext_size <= 2097168', name='ck_encrypted_blobs_ciphertext_size_max'),
        CheckConstraint('octet_length(ciphertext_sha256) = 32', name='ck_encrypted_blobs_ciphertext_sha256_length'),
    )

    user_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id', ondelete='CASCADE'), primary_key=True)
    blob_id: Mapped[uuid.UUID] = mapped_column(primary_key=True)
    project_id: Mapped[str] = mapped_column(String(512), nullable=False)
    kind: Mapped[str] = mapped_column(String(32), nullable=False)
    crypto_version: Mapped[int] = mapped_column(Integer, nullable=False)
    aad_version: Mapped[int] = mapped_column(Integer, nullable=False)
    nonce: Mapped[bytes] = mapped_column(LargeBinary, nullable=False)
    ciphertext_size: Mapped[int] = mapped_column(Integer, nullable=False)
    ciphertext_sha256: Mapped[bytes] = mapped_column(LargeBinary, nullable=False)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)


class AuthSession(Base):
    __tablename__ = 'auth_sessions'

    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    user_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id', ondelete='CASCADE'), nullable=False, index=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    last_used_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    revoked_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))


class AuthRefreshToken(Base):
    __tablename__ = 'auth_refresh_tokens'

    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    session_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('auth_sessions.id', ondelete='CASCADE'), nullable=False, index=True)
    token_hash: Mapped[str] = mapped_column(String(64), nullable=False)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    used_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    revoked_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    replaced_by_id: Mapped[uuid.UUID | None] = mapped_column(ForeignKey('auth_refresh_tokens.id', ondelete='SET NULL'))


class EmailVerificationToken(Base):
    __tablename__ = 'email_verification_tokens'
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    user_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id', ondelete='CASCADE'), nullable=False, index=True)
    token_hash: Mapped[str] = mapped_column(String(64), nullable=False)
    email_normalized: Mapped[str] = mapped_column(String(320), nullable=False)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    used_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    revoked_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))


class PasswordResetToken(Base):
    __tablename__ = 'password_reset_tokens'
    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    user_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id', ondelete='CASCADE'), nullable=False, index=True)
    token_hash: Mapped[str] = mapped_column(String(64), nullable=False)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    used_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    revoked_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))


class AdminInvitation(Base):
    """Single-use, administrator-issued access to a pending user account."""

    __tablename__ = 'admin_invitations'

    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    user_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id', ondelete='CASCADE'), nullable=False, index=True)
    created_by_id: Mapped[uuid.UUID] = mapped_column(ForeignKey('users.id'), nullable=False)
    token_hash: Mapped[str] = mapped_column(String(64), nullable=False)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False, server_default=UTC_NOW)
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    used_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    revoked_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))


class UserDocumentAcceptance(Base):
    """Append-only evidence for an exact account-level legal document version."""

    __tablename__ = 'user_document_acceptances'
    __table_args__ = (
        CheckConstraint(
            "document_type IN ('user_agreement', 'privacy_policy', 'personal_data_consent', 'beta_terms')",
            name='ck_user_document_acceptances_type',
        ),
        CheckConstraint(
            "acceptance_method IN ('invitation', 'authenticated_confirmation')",
            name='ck_user_document_acceptances_method',
        ),
        CheckConstraint("content_checksum ~ '^[0-9a-f]{64}$'", name='ck_user_document_acceptances_checksum'),
        CheckConstraint("char_length(document_id) >= 1", name='ck_user_document_acceptances_document_id'),
        CheckConstraint("char_length(revision) >= 1", name='ck_user_document_acceptances_revision'),
        UniqueConstraint(
            'user_id', 'document_id', 'revision', 'content_checksum', 'language',
            name='uq_user_document_acceptance_exact',
        ),
    )

    id: Mapped[uuid.UUID] = mapped_column(primary_key=True, default=uuid.uuid4)
    user_id: Mapped[uuid.UUID] = mapped_column(
        ForeignKey('users.id', ondelete='CASCADE'), nullable=False, index=True,
    )
    document_type: Mapped[str] = mapped_column(String(32), nullable=False)
    document_id: Mapped[str] = mapped_column(String(128), nullable=False)
    revision: Mapped[str] = mapped_column(String(64), nullable=False)
    content_checksum: Mapped[str] = mapped_column(String(64), nullable=False)
    language: Mapped[str] = mapped_column(String(16), nullable=False)
    acceptance_method: Mapped[str] = mapped_column(String(32), nullable=False)
    accepted_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=UTC_NOW,
    )
