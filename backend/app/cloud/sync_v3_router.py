"""C18 mode-3 opaque project and account transport with one shared sequence."""
from __future__ import annotations

from uuid import UUID
from contextlib import contextmanager
from threading import BoundedSemaphore, Lock
from time import monotonic
from fastapi import APIRouter, Depends, HTTPException, Query, Request, Response, status
from fastapi.concurrency import run_in_threadpool
from fastapi.exceptions import RequestValidationError
from pydantic import ValidationError
from sqlalchemy.orm import Session
from sqlalchemy import select, text

from ..dependencies import AuthenticatedUser, get_cloud_session, get_current_accepted_user
from .schemas import (CompressionReaderCapability, CompressionWriterRequest,ProjectCoverReaderCapabilities, ObjectEnvelopeDto, SyncPushResult, V3EncryptedSyncPushRequest,
                      V3EncryptedSyncPushResponse, V3EncryptedSyncPullItem,
                      V3EncryptedSyncPullResponse, V3EncryptedSyncAckRequest,
                      DocumentReaderCapabilities,
    GameReaderCapabilities, ProgressReaderCapabilities, MapReaderCapabilities, ContentNoteReaderCapabilities, ContentNoteCapabilityGate, V3ReaderReadyRequest, V3CutoverRequest, V3CutoverResponse, V3SyncPullEvent,
                      encode_canonical_base64url, AccountEncryptedPushRequest, AccountSyncPullEvent)
from .services import SyncProtocolError, SyncService
from .models import AuthSession, SyncUserState, User
from .tokens import utc_now
from .sync_router import _error, _inline_json_schema, _read_encrypted_push_body

router = APIRouter(prefix='/api/v3/sync/encrypted', tags=['cloud sync v3'])
recovery_router = APIRouter(prefix='/api/v3/recovery/encrypted', tags=['encrypted recovery'])
_PUSH_SCHEMA = _inline_json_schema(V3EncryptedSyncPushRequest)
_ACCOUNT_PUSH_SCHEMA = _inline_json_schema(AccountEncryptedPushRequest)
_recovery_lock = Lock()
_recovery_slots = BoundedSemaphore(4)
_recovery_active: set[object] = set()
_recovery_rates: dict[tuple[str, object], tuple[float, float]] = {}


@contextmanager
def _recovery_budget(account_id: object, session_id: object):
    """Bounded per-worker budget; distributed deployments need an ingress limit."""
    now = monotonic()
    with _recovery_lock:
        for key, (_, touched) in list(_recovery_rates.items()):
            if now - touched > 120:
                del _recovery_rates[key]
        keys = [('account', account_id), ('session', session_id)]
        if (len(_recovery_rates) + sum(key not in _recovery_rates for key in keys) > 1024
                or account_id in _recovery_active or not _recovery_slots.acquire(blocking=False)):
            raise HTTPException(status_code=429, detail={'code': 'recovery_busy'}, headers={'Retry-After': '1'})
        try:
            limits = []
            for key in keys:
                available, last = _recovery_rates.get(key, (120.0, now))
                limits.append(min(120.0, available + (now - last) * 2))
            if any(available < 1 for available in limits):
                raise HTTPException(status_code=429, detail={'code': 'recovery_rate_limited'}, headers={'Retry-After': '1'})
            for key, available in zip(keys, limits, strict=True):
                _recovery_rates[key] = (available - 1, now)
            _recovery_active.add(account_id)
        except BaseException:
            _recovery_slots.release()
            raise
    try:
        yield
    finally:
        with _recovery_lock:
            _recovery_active.remove(account_id)
            _recovery_slots.release()


@router.post('/reader-ready', status_code=status.HTTP_204_NO_CONTENT)
def reader_ready(request: V3ReaderReadyRequest,
                 current: AuthenticatedUser = Depends(get_current_accepted_user),
                 session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().declare_v3_reader_ready(
            session, current.user.id, request.device_id, initial_web_setup=request.initial_web_setup,
        )
    except SyncProtocolError as error:
        raise _error(error) from None


@router.post('/cutover', response_model=V3CutoverResponse)
def cutover(request: V3CutoverRequest,
            current: AuthenticatedUser = Depends(get_current_accepted_user),
            session: Session = Depends(get_cloud_session)) -> V3CutoverResponse:
    try:
        mode, epoch = SyncService().cutover_to_v3(
            session, current.user.id, request.expected_cutover_epoch,
            initial_web_device_id=request.initial_web_device_id,
        )
    except SyncProtocolError as error:
        raise _error(error) from None
    return V3CutoverResponse(writer_transport_version=mode, cutover_epoch=epoch)


@router.post('/push', response_model=V3EncryptedSyncPushResponse, openapi_extra={
    'requestBody': {'required': True, 'content': {'application/json': {'schema': _PUSH_SCHEMA}}},
})
async def encrypted_push(http_request: Request,
                         current: AuthenticatedUser = Depends(get_current_accepted_user),
                         session: Session = Depends(get_cloud_session)) -> V3EncryptedSyncPushResponse:
    body = await _read_encrypted_push_body(http_request)
    try:
        request = V3EncryptedSyncPushRequest.model_validate_json(body)
    except ValidationError as error:
        raise RequestValidationError(error.errors()) from None
    try:
        results, cursor = await run_in_threadpool(
            SyncService().push_encrypted, session, current.user.id, request.device_id,
            request.items, transport_version=3,
        )
    except SyncProtocolError as error:
        raise _error(error) from None
    return V3EncryptedSyncPushResponse(
        results=[SyncPushResult(event_id=row.event_id, server_sequence=row.server_sequence,
                                duplicate=row.duplicate) for row in results],
        current_cursor=cursor,
    )


@router.get('/pull', response_model=V3EncryptedSyncPullResponse)
def encrypted_pull(device_id: UUID, since: int = Query(ge=0, le=9_007_199_254_740_991),
                   limit: int = Query(default=200, ge=1, le=500),
                   protocol_version: int = Query(...), encrypted_sync_version: int = Query(...),
                   current: AuthenticatedUser = Depends(get_current_accepted_user),
                   session: Session = Depends(get_cloud_session)) -> V3EncryptedSyncPullResponse:
    if protocol_version != 3 or encrypted_sync_version != 3:
        raise _error(SyncProtocolError('encrypted_sync_version_unsupported', 'Unsupported encrypted sync version.', 422))
    try:
        rows, next_cursor, has_more, _ = SyncService().pull_encrypted_v3(
            session, current.user.id, device_id, since, limit,
        )
    except SyncProtocolError as error:
        raise _error(error) from None
    return V3EncryptedSyncPullResponse(items=_v3_pull_items(rows, current.user.id), next_cursor=next_cursor, has_more=has_more)


def _v3_pull_items(rows, user_id):
    return [V3EncryptedSyncPullItem(
        event=AccountSyncPullEvent(
            event_id=event.event_id, device_id=event.device_id, canonical_user_id=user_id, scope='account',
            entity_id=event.entity_id, entity_type=event.entity_type, operation=event.operation,
            revision=event.revision, updated_at=event.updated_at, deleted_at=event.deleted_at, server_sequence=event.server_sequence,
        ) if event.project_id is None else V3SyncPullEvent(
            event_id=event.event_id, device_id=event.device_id, project_id=event.project_id,
            entity_id=event.entity_id, entity_type=event.entity_type, operation=event.operation,
            revision=event.revision, updated_at=event.updated_at, deleted_at=event.deleted_at,
            server_sequence=event.server_sequence,
        ),
        object=ObjectEnvelopeDto(
            crypto_version=encrypted.crypto_version, aad_version=encrypted.aad_version,
            nonce=encode_canonical_base64url(encrypted.nonce), ciphertext=encode_canonical_base64url(encrypted.ciphertext),
        ),
    ) for event, encrypted in rows]


def _recovery_read_only(session: Session = Depends(get_cloud_session)) -> None:
    # This dependency runs before authentication on the same cached session.
    session.execute(text('SET TRANSACTION READ ONLY'))
    session.execute(text("SET LOCAL statement_timeout = '10s'"))


def _limit_recovery(current: AuthenticatedUser = Depends(get_current_accepted_user)):
    with _recovery_budget(current.user.id, current.session.id):
        yield


@recovery_router.get('/history')
def recovery_history(request: Request, response: Response,
                     expected_cursor: int = Query(ge=0, le=9_007_199_254_740_991),
                     expected_epoch: int = Query(ge=0, le=9_007_199_254_740_991),
                     since: int = Query(ge=0, le=9_007_199_254_740_991),
                     limit: int = Query(default=100, ge=1, le=100),
                     _read_only: None = Depends(_recovery_read_only),
                     current: AuthenticatedUser = Depends(get_current_accepted_user),
                     _budget: None = Depends(_limit_recovery),
                     session: Session = Depends(get_cloud_session)):
    """Read-only, account-bound ciphertext delivery; never certifies a reader."""
    keys = [key for key, _ in request.query_params.multi_items()]
    if (not {'expected_cursor', 'expected_epoch', 'since'} <= set(keys)
            or not set(keys) <= {'expected_cursor', 'expected_epoch', 'since', 'limit'}
            or len(keys) != len(set(keys))):
        raise _error(SyncProtocolError('recovery_parameters_invalid', 'Invalid recovery parameters.', 422))
    def state():
        return session.execute(select(SyncUserState.current_sequence, SyncUserState.writer_transport_version,
                                      SyncUserState.cutover_epoch).where(SyncUserState.user_id == current.user.id)).one_or_none()
    expected = (expected_cursor, 3, expected_epoch)
    if since > expected_cursor or state() != expected:
        raise _error(SyncProtocolError('recovery_snapshot_changed', 'Recovery snapshot changed.', 409))
    service = SyncService()
    try:
        rows, next_cursor, has_more = service._materialize_encrypted_page(session, current.user.id, since, limit)
        service._validate_v3_encrypted_rows(rows)
        if any(event.server_sequence != since + index + 1 for index, (event, _) in enumerate(rows)):
            raise SyncProtocolError('encrypted_sync_event_incomplete', 'Recovery history has a gap.', 409)
        if next_cursor > expected_cursor or has_more != (next_cursor < expected_cursor):
            raise SyncProtocolError('encrypted_sync_event_incomplete', 'Recovery history is incomplete.', 409)
        if state() != expected:
            raise SyncProtocolError('recovery_snapshot_changed', 'Recovery snapshot changed.', 409)
        active = session.execute(select(AuthSession.revoked_at, AuthSession.expires_at, User.status)
            .join(User, User.id == AuthSession.user_id)
            .where(AuthSession.id == current.session.id, AuthSession.user_id == current.user.id)).one_or_none()
        if not active or active.revoked_at is not None or active.expires_at <= utc_now() or active.status != 'active':
            raise SyncProtocolError('invalid_token', 'Invalid authentication token.', 401)
    except SyncProtocolError as error:
        raise _error(error) from None
    response.headers['Cache-Control'] = 'no-store'
    return {'delivery_version': 1, 'current_cursor': expected_cursor, 'cutover_epoch': expected_epoch,
            'writer_transport_version': 3, 'items': _v3_pull_items(rows, current.user.id),
            'next_cursor': next_cursor, 'has_more': has_more}


@router.post('/ack', status_code=status.HTTP_204_NO_CONTENT)
def ack(request: V3EncryptedSyncAckRequest,
        current: AuthenticatedUser = Depends(get_current_accepted_user),
        session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().ack(session, current.user.id, request.device_id, request.cursor, transport_version=3)
    except SyncProtocolError as error:
        raise _error(error) from None


@router.post('/account/push', response_model=V3EncryptedSyncPushResponse, openapi_extra={
    'requestBody': {'required': True, 'content': {'application/json': {'schema': _ACCOUNT_PUSH_SCHEMA}}},
})
async def account_push(http_request: Request,
                       current: AuthenticatedUser = Depends(get_current_accepted_user),
                       session: Session = Depends(get_cloud_session)) -> V3EncryptedSyncPushResponse:
    body = await _read_encrypted_push_body(http_request)
    try:
        request = AccountEncryptedPushRequest.model_validate_json(body)
    except ValidationError as error:
        raise RequestValidationError(error.errors()) from None
    try:
        results, cursor = await run_in_threadpool(SyncService().push_encrypted, session,
            current.user.id, request.device_id, request.items, transport_version=3)
    except SyncProtocolError as error:
        raise _error(error) from None
    return V3EncryptedSyncPushResponse(results=[SyncPushResult(event_id=row.event_id,
        server_sequence=row.server_sequence, duplicate=row.duplicate) for row in results], current_cursor=cursor)


@router.put('/note-reader-capabilities', status_code=status.HTTP_204_NO_CONTENT)
def note_reader_capabilities(request: ContentNoteReaderCapabilities,
                             current: AuthenticatedUser = Depends(get_current_accepted_user),
                             session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().declare_content_note_reader(session, current.user.id, request)
    except SyncProtocolError as error:
        raise _error(error) from None


@router.get('/note-reader-capabilities', response_model=ContentNoteCapabilityGate)
def note_reader_gate(current: AuthenticatedUser = Depends(get_current_accepted_user),
                     session: Session = Depends(get_cloud_session)) -> ContentNoteCapabilityGate:
    ready, missing = SyncService().content_note_gate(session, current.user.id)
    return ContentNoteCapabilityGate(ready=ready, missing_devices=missing)


@router.put('/map-reader-capabilities', status_code=status.HTTP_204_NO_CONTENT)
def map_reader_capabilities(request: MapReaderCapabilities,
                            current: AuthenticatedUser = Depends(get_current_accepted_user),
                            session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().declare_map_reader(session, current.user.id, request)
    except SyncProtocolError as error:
        raise _error(error) from None


@router.get('/map-reader-capabilities', response_model=ContentNoteCapabilityGate)
def map_reader_gate(current: AuthenticatedUser = Depends(get_current_accepted_user),
                    session: Session = Depends(get_cloud_session)) -> ContentNoteCapabilityGate:
    ready, missing = SyncService().map_gate(session, current.user.id)
    return ContentNoteCapabilityGate(ready=ready, missing_devices=missing)


@router.put('/document-reader-capabilities', status_code=status.HTTP_204_NO_CONTENT)
def document_reader_capabilities(request: DocumentReaderCapabilities,
                            current: AuthenticatedUser = Depends(get_current_accepted_user),
                            session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().declare_document_reader(session, current.user.id, request)
    except SyncProtocolError as error:
        raise _error(error) from None


@router.get('/document-reader-capabilities', response_model=ContentNoteCapabilityGate)
def document_reader_gate(current: AuthenticatedUser = Depends(get_current_accepted_user),
                    session: Session = Depends(get_cloud_session)) -> ContentNoteCapabilityGate:
    ready, missing = SyncService().document_gate(session, current.user.id)
    return ContentNoteCapabilityGate(ready=ready, missing_devices=missing)

@router.put('/progress-reader-capabilities', status_code=status.HTTP_204_NO_CONTENT)
def progress_reader_capabilities(request: ProgressReaderCapabilities,
                            current: AuthenticatedUser = Depends(get_current_accepted_user),
                            session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().declare_progress_reader(session, current.user.id, request)
    except SyncProtocolError as error:
        raise _error(error) from None


@router.get('/progress-reader-capabilities', response_model=ContentNoteCapabilityGate)
def progress_reader_gate(current: AuthenticatedUser = Depends(get_current_accepted_user),
                    session: Session = Depends(get_cloud_session)) -> ContentNoteCapabilityGate:
    ready, missing = SyncService().progress_gate(session, current.user.id)
    return ContentNoteCapabilityGate(ready=ready, missing_devices=missing)


@router.put('/game-reader-capabilities', status_code=status.HTTP_204_NO_CONTENT)
def game_reader_capabilities(request: GameReaderCapabilities,
                             current: AuthenticatedUser = Depends(get_current_accepted_user),
                             session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().declare_game_reader(session, current.user.id, request)
    except SyncProtocolError as error:
        raise _error(error) from None


@router.get('/game-reader-capabilities', response_model=ContentNoteCapabilityGate)
def game_reader_gate(current: AuthenticatedUser = Depends(get_current_accepted_user),
                     session: Session = Depends(get_cloud_session)) -> ContentNoteCapabilityGate:
    ready, missing = SyncService().game_gate(session, current.user.id)
    return ContentNoteCapabilityGate(ready=ready, missing_devices=missing)


@router.put('/cover-reader-capabilities', status_code=status.HTTP_204_NO_CONTENT)
def declare_cover_reader(request: ProjectCoverReaderCapabilities,
                         current: AuthenticatedUser = Depends(get_current_accepted_user),
                         session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().declare_cover_reader(session, current.user.id, request)
    except SyncProtocolError as error:
        raise _error(error) from None


@router.get('/cover-reader-capabilities', response_model=ContentNoteCapabilityGate)
def cover_reader_gate(current: AuthenticatedUser = Depends(get_current_accepted_user),
                      session: Session = Depends(get_cloud_session)) -> ContentNoteCapabilityGate:
    ready, missing = SyncService().cover_gate(session, current.user.id)
    return ContentNoteCapabilityGate(ready=ready, missing_devices=missing)


@router.post('/cover-metadata/push', response_model=V3EncryptedSyncPushResponse, openapi_extra={
    'requestBody': {'required': True, 'content': {'application/json': {'schema': _PUSH_SCHEMA}}},
})
async def cover_metadata_push(http_request: Request,
                         current: AuthenticatedUser = Depends(get_current_accepted_user),
                         session: Session = Depends(get_cloud_session)) -> V3EncryptedSyncPushResponse:
    body = await _read_encrypted_push_body(http_request)
    try:
        request = V3EncryptedSyncPushRequest.model_validate_json(body)
    except ValidationError as error:
        raise RequestValidationError(error.errors()) from None
    try:
        results, cursor = await run_in_threadpool(
            SyncService().push_encrypted, session, current.user.id, request.device_id,
            request.items, transport_version=3, metadata_cover=True,
        )
    except SyncProtocolError as error:
        raise _error(error) from None
    return V3EncryptedSyncPushResponse(
        results=[SyncPushResult(event_id=row.event_id, server_sequence=row.server_sequence,
                                duplicate=row.duplicate) for row in results],
        current_cursor=cursor,
    )


@router.put('/compression-reader-capabilities', status_code=status.HTTP_204_NO_CONTENT)
def compression_reader(request: CompressionReaderCapability,
                       current: AuthenticatedUser = Depends(get_current_accepted_user),
                       session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().declare_compression_reader(session, current.user.id, request)
    except SyncProtocolError as error:
        raise _error(error) from None


@router.get('/compression-reader-capabilities', response_model=ContentNoteCapabilityGate)
def compression_gate(current: AuthenticatedUser = Depends(get_current_accepted_user),
                     session: Session = Depends(get_cloud_session)) -> ContentNoteCapabilityGate:
    ready, missing = SyncService().compression_gate(session, current.user.id)
    return ContentNoteCapabilityGate(ready=ready, missing_devices=missing)


@router.post('/compression-writer', response_model=ContentNoteCapabilityGate)
def compression_writer(request: CompressionWriterRequest,
                       current: AuthenticatedUser = Depends(get_current_accepted_user),
                       session: Session = Depends(get_cloud_session)) -> ContentNoteCapabilityGate:
    try:
        ready, missing = SyncService().authorize_compressed_seal(session, current.user.id, request.device_id)
    except SyncProtocolError as error:
        raise _error(error) from None
    return ContentNoteCapabilityGate(ready=ready, missing_devices=missing)
