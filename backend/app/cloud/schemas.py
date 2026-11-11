from __future__ import annotations

import base64
import re
from datetime import datetime
from typing import Literal
from uuid import UUID

from email_validator import EmailNotValidError, validate_email
from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator

from .passwords import MAX_PASSWORD_LENGTH


_BASE64URL_RE = re.compile(r'^[A-Za-z0-9_-]*$')
MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES = 8_388_624
MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES = 16_777_216
MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES = 33_554_432
SYNC_MAX_WIRE_INTEGER = 9_007_199_254_740_991  # JavaScript Number.MAX_SAFE_INTEGER


def decode_canonical_base64url(value: str, *, expected_length: int | None = None,
                               minimum_length: int | None = None,
                               maximum_length: int | None = None) -> bytes:
    """Decode only canonical, unpadded Base64URL binary wire values."""
    if not isinstance(value, str) or not _BASE64URL_RE.fullmatch(value) or len(value) % 4 == 1:
        raise ValueError('Invalid binary encoding.')
    try:
        decoded = base64.urlsafe_b64decode(value + '=' * (-len(value) % 4))
    except (ValueError, UnicodeEncodeError) as error:
        raise ValueError('Invalid binary encoding.') from error
    if base64.urlsafe_b64encode(decoded).decode('ascii').rstrip('=') != value:
        raise ValueError('Invalid binary encoding.')
    if expected_length is not None and len(decoded) != expected_length:
        raise ValueError('Invalid binary encoding.')
    if minimum_length is not None and len(decoded) < minimum_length:
        raise ValueError('Invalid binary encoding.')
    if maximum_length is not None and len(decoded) > maximum_length:
        raise ValueError('Invalid binary encoding.')
    return decoded


def encode_canonical_base64url(value: bytes) -> str:
    """Encode binary fields as canonical, unpadded Base64URL."""
    return base64.urlsafe_b64encode(value).decode('ascii').rstrip('=')


def _validate_binary(*, expected_length: int | None = None,
                     minimum_length: int | None = None,
                     maximum_length: int | None = None):
    def validator(value: str) -> str:
        decode_canonical_base64url(value, expected_length=expected_length, minimum_length=minimum_length,
                                   maximum_length=maximum_length)
        return value
    return validator


class LoginRequest(BaseModel):
    username: str = Field(min_length=1, max_length=128)
    password: str = Field(min_length=1, max_length=MAX_PASSWORD_LENGTH)


class RefreshRequest(BaseModel):
    refresh_token: str = Field(min_length=1, max_length=1024)


class VerificationTokenRequest(BaseModel):
    token: str = Field(min_length=1, max_length=1024)


class PasswordResetRequest(BaseModel):
    # Deliberately not EmailStr: malformed inputs receive the same public response.
    email: str = Field(min_length=1, max_length=320)


class RegistrationRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')

    username: str = Field(min_length=1, max_length=128)
    email: str = Field(min_length=1, max_length=320)
    password: str = Field(min_length=1, max_length=MAX_PASSWORD_LENGTH)

    @field_validator('username')
    @classmethod
    def username_must_not_be_whitespace(cls, value: str) -> str:
        if not value.strip():
            raise ValueError('Username must not be empty.')
        return value

    @field_validator('email')
    @classmethod
    def email_must_have_valid_syntax(cls, value: str) -> str:
        display_email = value.strip()
        try:
            validate_email(display_email, check_deliverability=False)
        except EmailNotValidError as error:
            raise ValueError('Email must have valid syntax.') from error
        return display_email


class AdminInvitationCreateRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    username: str = Field(min_length=1, max_length=128)
    email: str = Field(min_length=1, max_length=320)

    @field_validator('username')
    @classmethod
    def username_must_not_be_whitespace(cls, value: str) -> str:
        if not value.strip():
            raise ValueError('Username must not be empty.')
        return value

    @field_validator('email')
    @classmethod
    def email_must_have_valid_syntax(cls, value: str) -> str:
        display_email = value.strip()
        try:
            validate_email(display_email, check_deliverability=False)
        except EmailNotValidError as error:
            raise ValueError('Email must have valid syntax.') from error
        return display_email


class AdminInvitationResponse(BaseModel):
    id: UUID
    username: str
    email: str
    status: Literal['pending', 'accepted', 'revoked', 'expired']
    created_at: datetime
    expires_at: datetime


class LegalDocumentConfirmation(BaseModel):
    model_config = ConfigDict(extra='forbid')
    document_id: str = Field(min_length=1, max_length=128)
    revision: str = Field(min_length=1, max_length=64)
    content_checksum: str = Field(pattern=r'^[0-9a-f]{64}$')
    language: Literal['ru', 'en', 'es', 'de', 'fr', 'pt_BR']


class LegalDocumentResponse(BaseModel):
    document_id: str
    document_type: Literal['user_agreement', 'privacy_policy', 'personal_data_consent', 'beta_terms']
    revision: str
    content_checksum: str
    language: Literal['ru', 'en', 'es', 'de', 'fr', 'pt_BR']
    title: str
    content: str
    mandatory: bool
    requires_explicit_acceptance: bool
    accepted: bool


class LegalDocumentStatusResponse(BaseModel):
    documents: list[LegalDocumentResponse]
    missing_required_document_ids: list[str]


class InvitationDocumentsRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    token: str = Field(min_length=1, max_length=1024)
    language: Literal['ru', 'en', 'es', 'de', 'fr', 'pt_BR'] = 'ru'


class InvitationAcceptRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    token: str = Field(min_length=1, max_length=1024)
    password: str = Field(min_length=1, max_length=MAX_PASSWORD_LENGTH)
    language: Literal['ru', 'en', 'es', 'de', 'fr', 'pt_BR'] = 'ru'
    documents: list[LegalDocumentConfirmation] = Field(min_length=1, max_length=8)


class AuthenticatedLegalAcceptRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    language: Literal['ru', 'en', 'es', 'de', 'fr', 'pt_BR'] = 'ru'
    documents: list[LegalDocumentConfirmation] = Field(min_length=1, max_length=8)


class PublicVerificationRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')

    # Deliberately keep this generic like password reset: malformed and unknown
    # inputs cannot become an account-existence oracle.
    email: str = Field(min_length=1, max_length=320)


class PasswordResetConfirmRequest(VerificationTokenRequest):
    new_password: str = Field(min_length=1, max_length=MAX_PASSWORD_LENGTH)


class TokenResponse(BaseModel):
    access_token: str
    refresh_token: str
    token_type: str = 'bearer'
    access_expires_in: int


class AccountResponse(BaseModel):
    id: UUID
    username: str
    email: str
    email_verified: bool
    role: str
    status: str
    created_at: datetime


class AccountLimitsResponse(BaseModel):
    max_cloud_projects: int


class CloudProjectsResponse(BaseModel):
    cloud_project_ids: list[str]
    cloud_project_count: int
    max_cloud_projects: int


class CloudProjectBootstrapRegistrationRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')

    bootstrap_id: UUID
    device_id: UUID


class CloudProjectBootstrapCompletionRequest(CloudProjectBootstrapRegistrationRequest):
    initial_event_count: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)
    initial_max_server_sequence: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)

    @model_validator(mode='after')
    def event_count_and_sequence_must_agree(self):
        if (self.initial_event_count == 0) != (self.initial_max_server_sequence == 0):
            raise ValueError('Empty initial upload requires a zero server sequence.')
        return self


class CloudProjectBootstrapDescriptor(BaseModel):
    project_id: str
    bootstrap_id: UUID | None
    origin_device_id: UUID | None
    state: Literal['legacy', 'initializing', 'active']
    initial_event_count: int | None = Field(default=None, ge=0, le=SYNC_MAX_WIRE_INTEGER)
    initial_max_server_sequence: int | None = Field(default=None, ge=0, le=SYNC_MAX_WIRE_INTEGER)


class CloudProjectBootstrapResponse(BaseModel):
    project: CloudProjectBootstrapDescriptor
    current_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class CloudProjectBootstrapListResponse(BaseModel):
    projects: list[CloudProjectBootstrapDescriptor]
    current_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class PasswordKdfDto(BaseModel):
    """C11 password-KDF public metadata, with binary values encoded as Base64URL."""

    model_config = ConfigDict(extra='forbid')
    kdf_version: int = Field(ge=1)
    algorithm: Literal['argon2id13']
    salt: str
    opslimit: int = Field(ge=1)
    memlimit: int = Field(ge=1)

    _salt = field_validator('salt')(_validate_binary(expected_length=16))


class PasswordWrappedAmkDto(BaseModel):
    """Future API DTO only; it contains no passphrase or plaintext key material."""

    model_config = ConfigDict(extra='forbid')
    crypto_version: int = Field(ge=1)
    wrapping_version: int = Field(ge=1)
    kdf: PasswordKdfDto
    nonce: str
    ciphertext: str

    _nonce = field_validator('nonce')(_validate_binary(expected_length=24))
    _ciphertext = field_validator('ciphertext')(_validate_binary(expected_length=48))


class RecoveryWrappedAmkDto(BaseModel):
    """Future API DTO only; the Recovery Key itself is never represented."""

    model_config = ConfigDict(extra='forbid')
    crypto_version: int = Field(ge=1)
    wrapping_version: int = Field(ge=1)
    nonce: str
    ciphertext: str

    _nonce = field_validator('nonce')(_validate_binary(expected_length=24))
    _ciphertext = field_validator('ciphertext')(_validate_binary(expected_length=48))


class InitialCryptoProvisioningRequest(BaseModel):
    """Immutable first-time C11 wrapper set for the authenticated user."""

    model_config = ConfigDict(extra='forbid')
    password: PasswordWrappedAmkDto
    recovery: RecoveryWrappedAmkDto

    @model_validator(mode='after')
    def require_supported_initial_wrappers(self):
        password = self.password
        recovery = self.recovery
        if (password.crypto_version != 1 or password.wrapping_version != 1
                or password.kdf.kdf_version != 1
                or password.kdf.algorithm != 'argon2id13'
                or password.kdf.opslimit != 2 or password.kdf.memlimit != 67_108_864
                or recovery.crypto_version != 1 or recovery.wrapping_version != 1):
            raise ValueError('Unsupported initial crypto wrapper parameters.')
        return self


class CurrentUserCryptoResponse(BaseModel):
    """Wrapped key material for the authenticated current user only."""

    model_config = ConfigDict(extra='forbid')
    provisioned: bool
    password: PasswordWrappedAmkDto | None = None
    recovery: RecoveryWrappedAmkDto | None = None

    @model_validator(mode='after')
    def records_match_provisioning_state(self):
        if self.provisioned != (self.password is not None):
            raise ValueError('Provisioned crypto state requires a password wrapper.')
        if not self.provisioned and self.recovery is not None:
            raise ValueError('Unprovisioned crypto state cannot contain Recovery material.')
        return self


class ObjectEnvelopeDto(BaseModel):
    """Future API DTO for opaque client ciphertext; this server does not decrypt."""

    model_config = ConfigDict(extra='forbid')
    crypto_version: int = Field(ge=1)
    aad_version: int = Field(ge=1)
    nonce: str
    ciphertext: str

    _nonce = field_validator('nonce')(_validate_binary(expected_length=24))
    _ciphertext = field_validator('ciphertext')(_validate_binary(
        minimum_length=16, maximum_length=MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES,
    ))


SYNC_PROTOCOL_VERSION = 1
ENCRYPTED_SYNC_VERSION = 1
V2_SYNC_PROTOCOL_VERSION = 2
V2_ENCRYPTED_SYNC_VERSION = 2


class SyncEventEnvelope(BaseModel):
    model_config = ConfigDict(extra='forbid')
    event_id: UUID
    project_id: str = Field(min_length=1, max_length=512)
    entity_id: str = Field(min_length=1, max_length=512)
    entity_type: str = Field(min_length=1, max_length=128, pattern=r'^[a-z][a-z0-9_:-]*$')
    operation: Literal['upsert', 'delete', 'event']
    revision: int = Field(ge=1, le=SYNC_MAX_WIRE_INTEGER)
    updated_at: datetime
    deleted_at: datetime | None = None

    @model_validator(mode='after')
    def validate_tombstone_and_timestamps(self) -> 'SyncEventEnvelope':
        if self.updated_at.tzinfo is None or self.updated_at.utcoffset() is None:
            raise ValueError('updated_at must include a timezone.')
        if self.deleted_at is not None and (self.deleted_at.tzinfo is None or self.deleted_at.utcoffset() is None):
            raise ValueError('deleted_at must include a timezone.')
        if (self.operation == 'delete') != (self.deleted_at is not None):
            raise ValueError('delete requires deleted_at; upsert and event forbid it.')
        return self


class SyncPushRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    protocol_version: int
    device_id: UUID
    events: list[SyncEventEnvelope] = Field(max_length=100)


class SyncPushResult(BaseModel):
    event_id: UUID
    server_sequence: int = Field(ge=1, le=SYNC_MAX_WIRE_INTEGER)
    duplicate: bool


class SyncPushResponse(BaseModel):
    protocol_version: int = SYNC_PROTOCOL_VERSION
    results: list[SyncPushResult]
    current_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class SyncDeviceResponse(BaseModel):
    protocol_version: int = SYNC_PROTOCOL_VERSION
    device_id: UUID
    last_ack_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class SyncPullEvent(SyncEventEnvelope):
    device_id: UUID
    server_sequence: int = Field(ge=1, le=SYNC_MAX_WIRE_INTEGER)


class SyncPullResponse(BaseModel):
    protocol_version: int = SYNC_PROTOCOL_VERSION
    events: list[SyncPullEvent]
    next_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)
    has_more: bool


class SyncAckRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    protocol_version: int
    device_id: UUID
    cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class EncryptedSyncPushItem(BaseModel):
    model_config = ConfigDict(extra='forbid')
    event: SyncEventEnvelope
    object: ObjectEnvelopeDto


class EncryptedSyncPushRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    protocol_version: int
    encrypted_sync_version: int
    device_id: UUID
    items: list[EncryptedSyncPushItem] = Field(max_length=100)


class EncryptedSyncPushResponse(BaseModel):
    protocol_version: int = SYNC_PROTOCOL_VERSION
    encrypted_sync_version: int = ENCRYPTED_SYNC_VERSION
    results: list[SyncPushResult]
    current_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class EncryptedSyncPullItem(BaseModel):
    event: SyncPullEvent
    object: ObjectEnvelopeDto | None


class EncryptedSyncPullResponse(BaseModel):
    protocol_version: int = SYNC_PROTOCOL_VERSION
    encrypted_sync_version: int = ENCRYPTED_SYNC_VERSION
    items: list[EncryptedSyncPullItem]
    next_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)
    has_more: bool


class V2SyncEventEnvelope(BaseModel):
    """Opaque transport metadata for protocol-v2 encrypted Note events."""
    model_config = ConfigDict(extra='forbid')
    event_id: UUID
    project_id: str = Field(min_length=1, max_length=512)
    entity_id: str = Field(min_length=1, max_length=512)
    entity_type: Literal['note']
    operation: Literal['upsert', 'delete', 'resolution']
    revision: int = Field(ge=1, le=SYNC_MAX_WIRE_INTEGER)
    updated_at: datetime
    deleted_at: datetime | None = None

    @model_validator(mode='after')
    def validate_tombstone_and_timestamps(self) -> 'V2SyncEventEnvelope':
        if self.updated_at.tzinfo is None or self.updated_at.utcoffset() is None:
            raise ValueError('updated_at must include a timezone.')
        if self.deleted_at is not None and (self.deleted_at.tzinfo is None or self.deleted_at.utcoffset() is None):
            raise ValueError('deleted_at must include a timezone.')
        if (self.operation == 'delete') != (self.deleted_at is not None):
            raise ValueError('delete requires deleted_at; upsert and resolution forbid it.')
        if self.operation == 'resolution' and self.revision < 2:
            raise ValueError('resolution requires revision at least 2.')
        return self


class V2SyncPullEvent(V2SyncEventEnvelope):
    device_id: UUID
    server_sequence: int = Field(ge=1, le=SYNC_MAX_WIRE_INTEGER)


class V2EncryptedSyncPushItem(BaseModel):
    model_config = ConfigDict(extra='forbid')
    event: V2SyncEventEnvelope
    object: ObjectEnvelopeDto


class V2EncryptedSyncPushRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    protocol_version: Literal[2]
    encrypted_sync_version: Literal[2]
    device_id: UUID
    items: list[V2EncryptedSyncPushItem] = Field(max_length=100)


class V2EncryptedSyncPushResponse(BaseModel):
    protocol_version: Literal[2] = V2_SYNC_PROTOCOL_VERSION
    encrypted_sync_version: Literal[2] = V2_ENCRYPTED_SYNC_VERSION
    results: list[SyncPushResult]
    current_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class V2EncryptedSyncPullItem(BaseModel):
    event: V2SyncPullEvent
    object: ObjectEnvelopeDto | None


class V2EncryptedSyncPullResponse(BaseModel):
    protocol_version: Literal[2] = V2_SYNC_PROTOCOL_VERSION
    encrypted_sync_version: Literal[2] = V2_ENCRYPTED_SYNC_VERSION
    items: list[V2EncryptedSyncPullItem]
    next_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)
    has_more: bool


class V2EncryptedSyncAckRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    protocol_version: Literal[2]
    encrypted_sync_version: Literal[2]
    device_id: UUID
    cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class V2EncryptedSyncCapabilitiesResponse(BaseModel):
    model_config = ConfigDict(extra='forbid')
    supported_transport_version: Literal[2] = V2_ENCRYPTED_SYNC_VERSION
    writer_transport_version: Literal[1, 2, 3]
    cutover_epoch: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class V2EncryptedSyncCutoverRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    expected_cutover_epoch: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER, strict=True)
    initial_web_device_id: UUID | None = None


class V2EncryptedSyncCutoverResponse(BaseModel):
    model_config = ConfigDict(extra='forbid')
    supported_transport_version: Literal[2] = V2_ENCRYPTED_SYNC_VERSION
    writer_transport_version: Literal[2]
    cutover_epoch: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


# C18 metadata mode 3 retains the frozen Note envelope alongside opaque metadata.
class V3ReaderReadyRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
    reader_transport_version: Literal[3]
    initial_web_setup: bool = Field(default=False, strict=True)


class V3CutoverRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    expected_cutover_epoch: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER, strict=True)
    initial_web_device_id: UUID | None = None


class V3CutoverResponse(BaseModel):
    model_config = ConfigDict(extra='forbid')
    writer_transport_version: Literal[3]
    cutover_epoch: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class V3EncryptedSyncAckRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    protocol_version: Literal[3]
    encrypted_sync_version: Literal[3]
    device_id: UUID
    cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER, strict=True)


def is_map_entity_id(entity_id: str) -> bool:
    return entity_id == 'project-map' or re.fullmatch(r'stage-map-[0-9a-f]{64}', entity_id) is not None


class V3SyncEventEnvelope(V2SyncEventEnvelope):
    operation: Literal['upsert', 'delete', 'resolution', 'event']
    entity_type: Literal['note', 'project_metadata', 'stage', 'stage_order', 'map', 'document', 'progress', 'project_game']

    @model_validator(mode='after')
    def validate_metadata_scope(self) -> 'V3SyncEventEnvelope':
        if self.operation == 'event' and self.entity_type not in ('note', 'map', 'document', 'progress', 'project_game'):
            raise ValueError('Framed operation requires a Note, map, document or progress entity.')
        if self.entity_type == 'map' and (
                self.operation != 'event' or self.deleted_at is not None or
                not is_map_entity_id(self.entity_id)):
            raise ValueError('Map requires its framed owning-map identity.')
        if self.entity_type in ('document', 'progress', 'project_game') and (self.operation != 'event' or self.deleted_at is not None):
            raise ValueError('Document and progress require framed stable identities.')
        if self.entity_type == 'project_game' and (not self.entity_id.startswith('game:') or not self.entity_id.endswith(f':{self.event_id}')):
            raise ValueError('Game requires its immutable action identity.')
        if self.entity_type == 'project_metadata' and self.entity_id != self.project_id:
            raise ValueError('Project metadata entity ID must equal project ID.')
        if self.entity_type == 'stage_order' and (self.entity_id != 'stage_order' or self.operation != 'upsert'):
            raise ValueError('Stage order requires its project-scoped identity and upsert.')
        if self.entity_type == 'stage' and self.operation not in ('upsert', 'delete'):
            raise ValueError('Unsupported stage operation.')
        return self


class V3EncryptedSyncPushItem(BaseModel):
    model_config = ConfigDict(extra='forbid')
    event: V3SyncEventEnvelope
    object: ObjectEnvelopeDto


class V3EncryptedSyncPushRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    protocol_version: Literal[3]
    encrypted_sync_version: Literal[3]
    device_id: UUID
    items: list[V3EncryptedSyncPushItem] = Field(max_length=100)


class V3EncryptedSyncPushResponse(BaseModel):
    protocol_version: Literal[3] = 3
    encrypted_sync_version: Literal[3] = 3
    results: list[SyncPushResult]
    current_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)


class V3SyncPullEvent(V3SyncEventEnvelope):
    device_id: UUID
    server_sequence: int = Field(ge=1, le=SYNC_MAX_WIRE_INTEGER)


ACCOUNT_ENTITY_TYPES = ('folder', 'folder_order', 'folder_membership', 'project_order', 'account_game')


class AccountSyncEventEnvelope(BaseModel):
    model_config = ConfigDict(extra='forbid')
    event_id: UUID
    canonical_user_id: UUID
    scope: Literal['account']
    entity_id: str = Field(min_length=1)
    entity_type: Literal['folder', 'folder_order', 'folder_membership', 'project_order', 'account_game']
    operation: Literal['upsert', 'delete']
    revision: int = Field(ge=1, le=SYNC_MAX_WIRE_INTEGER)
    updated_at: datetime
    deleted_at: datetime | None = None

    @property
    def project_id(self) -> None:
        return None

    @model_validator(mode='after')
    def validate_account_descriptor(self) -> 'AccountSyncEventEnvelope':
        if len(self.entity_id.encode('utf-8')) > 512:
            raise ValueError('Account identity exceeds UTF-8 byte limit.')
        if self.updated_at.utcoffset() is None or (self.deleted_at is not None and self.deleted_at.utcoffset() is None):
            raise ValueError('Account timestamps require a timezone.')
        if (self.operation == 'delete') != (self.deleted_at is not None):
            raise ValueError('Invalid account tombstone.')
        if self.entity_type == 'account_game' and (self.operation != 'upsert' or self.entity_id != f'game:{self.event_id}'):
            raise ValueError('Account Game requires its immutable action identity.')
        return self


class AccountSyncPullEvent(AccountSyncEventEnvelope):
    device_id: UUID
    server_sequence: int = Field(ge=1, le=SYNC_MAX_WIRE_INTEGER)


class AccountEncryptedPushItem(BaseModel):
    model_config = ConfigDict(extra='forbid')
    event: AccountSyncEventEnvelope
    object: ObjectEnvelopeDto

    @model_validator(mode='after')
    def validate_versions(self) -> 'AccountEncryptedPushItem':
        if self.object.crypto_version != 2 or self.object.aad_version != 2:
            raise ValueError('Account objects require version 2/2.')
        return self


class AccountEncryptedPushRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    protocol_version: Literal[3]
    encrypted_sync_version: Literal[3]
    device_id: UUID
    items: list[AccountEncryptedPushItem] = Field(max_length=100)


class V3EncryptedSyncPullItem(BaseModel):
    event: V3SyncPullEvent | AccountSyncPullEvent
    object: ObjectEnvelopeDto


class V3EncryptedSyncPullResponse(BaseModel):
    protocol_version: Literal[3] = 3
    encrypted_sync_version: Literal[3] = 3
    items: list[V3EncryptedSyncPullItem]
    next_cursor: int = Field(ge=0, le=SYNC_MAX_WIRE_INTEGER)
    has_more: bool


class AdminUserResponse(BaseModel):
    id: UUID
    username: str
    email: str
    email_verified: bool
    role: str
    status: str
    registration_mode_at_signup: str | None
    created_at: datetime
    max_cloud_projects_override: int | None
    effective_max_cloud_projects: int


class AdminUsersResponse(BaseModel):
    users: list[AdminUserResponse]
    total: int
    limit: int
    offset: int


class AdminRegistrationPatch(BaseModel):
    model_config = ConfigDict(extra='forbid')
    mode: Literal['open', 'approval', 'closed'] | None = None
    max_users: int | None = Field(default=None, ge=0)

    @model_validator(mode='after')
    def explicit_mode_must_be_valid(self) -> 'AdminRegistrationPatch':
        if 'mode' in self.model_fields_set and self.mode is None:
            raise ValueError('Registration mode must be open, approval, or closed.')
        return self


class AdminRegistrationResponse(BaseModel):
    mode: Literal['open', 'approval', 'closed']
    max_users: int | None
    active_users: int


class AdminLimitsPatch(BaseModel):
    model_config = ConfigDict(extra='forbid')
    max_cloud_projects: int = Field(ge=0)


class AdminLimitsResponse(BaseModel):
    max_cloud_projects: int


class AdminUserLimitPatch(BaseModel):
    model_config = ConfigDict(extra='forbid')
    max_cloud_projects_override: int | None = Field(ge=0)


class AdminUserLimitResponse(BaseModel):
    max_cloud_projects_override: int | None
    effective_max_cloud_projects: int


class ReservedUsernameRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    username: str = Field(min_length=1, max_length=128)


class ReservedUsernameResponse(BaseModel):
    username_normalized: str
    created_at: datetime


class ContentNoteReaderCapabilities(BaseModel):
    """Protocol metadata only; zero explicitly withdraws support."""
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
    reader_transport_version: Literal[2, 3]
    frame_version: Literal[0, 1]
    codec8_version: Literal[0, 1]
    compression_zero: bool = Field(strict=True)
    ordinary_reader_version: Literal[0, 1]
    resolution_reader_version: Literal[0, 2]

    @field_validator('reader_transport_version', 'frame_version', 'codec8_version',
                     'ordinary_reader_version', 'resolution_reader_version', mode='before')
    @classmethod
    def strict_version(cls, value):
        if type(value) is not int:
            raise ValueError('Reader version must be an integer')
        return value


class ContentNoteCapabilityGate(BaseModel):
    model_config = ConfigDict(extra='forbid')
    ready: bool
    missing_devices: int = Field(ge=0)


class MapReaderCapabilities(BaseModel):
    """Independent map-format evidence under the accepted SyncDevice model."""
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
    frame_version: Literal[0, 1]
    codec_id: Literal[9]
    codec_version: Literal[0, 1]
    reader_version: Literal[0, 1]
    compression_zero: bool = Field(strict=True)

    @field_validator('frame_version', 'codec_id', 'codec_version', 'reader_version', mode='before')
    @classmethod
    def strict_version(cls, value):
        if type(value) is not int:
            raise ValueError('Reader version must be an integer')
        return value


class DocumentReaderCapabilities(BaseModel):
    """Independent project-object reader evidence under the accepted SyncDevice model."""
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
    frame_version: Literal[0, 1]
    codec_id: Literal[10]
    codec_version: Literal[0, 1]
    reader_version: Literal[0, 1]
    compression_zero: bool = Field(strict=True)

    @field_validator('frame_version', 'codec_id', 'codec_version', 'reader_version', mode='before')
    @classmethod
    def strict_version(cls, value):
        if type(value) is not int:
            raise ValueError('Reader version must be an integer')
        return value

class ProgressReaderCapabilities(BaseModel):
    """Independent project-object reader evidence under the accepted SyncDevice model."""
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
    frame_version: Literal[0, 1]
    codec_id: Literal[11]
    codec_version: Literal[0, 1]
    reader_version: Literal[0, 1]
    compression_zero: bool = Field(strict=True)

    @field_validator('frame_version', 'codec_id', 'codec_version', 'reader_version', mode='before')
    @classmethod
    def strict_version(cls, value):
        if type(value) is not int:
            raise ValueError('Reader version must be an integer')
        return value


class ProjectGameReaderSupport(BaseModel):
    model_config = ConfigDict(extra='forbid')
    frame_version: Literal[0, 1]
    codec_id: Literal[12]
    codec_version: Literal[0, 1]
    reader_version: Literal[0, 1]
    compression_zero: bool = Field(strict=True)

    @field_validator('frame_version', 'codec_id', 'codec_version', 'reader_version', mode='before')
    @classmethod
    def strict_version(cls, value):
        if type(value) is not int:
            raise ValueError('Reader version must be an integer')
        return value


class AccountGameReaderSupport(ProjectGameReaderSupport):
    codec_id: Literal[13]
    codec_version: Literal[0, 1, 2]
    reader_version: Literal[0, 1, 2]


class GameReaderCapabilities(BaseModel):
    """Both Game domains are required; Progress dependencies are checked separately."""
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
    project: ProjectGameReaderSupport
    account: AccountGameReaderSupport


class ProjectCoverReaderCapabilities(BaseModel):
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
    frame_version: Literal[1]
    codec_id: Literal[1]
    metadata_codec_version: Literal[2]
    cover_crypto_version: Literal[1]
    cover_aad_version: Literal[1]
    compression_zero: Literal[True]
    reader_version: Literal[0, 2]

    @field_validator('frame_version', 'codec_id', 'metadata_codec_version',
                     'cover_crypto_version', 'cover_aad_version', 'reader_version', mode='before')
    @classmethod
    def strict_cover_version(cls, value):
        if type(value) is not int:
            raise ValueError('Reader version must be an integer')
        return value

    @field_validator('compression_zero', mode='before')
    @classmethod
    def strict_cover_compression(cls, value):
        if type(value) is not bool:
            raise ValueError('Compression evidence must be a boolean')
        return value


class CompressionReaderCapability(BaseModel):
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
    compression_id1: bool = Field(strict=True)


class CompressionWriterRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    device_id: UUID
