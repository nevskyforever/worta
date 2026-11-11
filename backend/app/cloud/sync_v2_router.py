from __future__ import annotations

from uuid import UUID

from fastapi import APIRouter, Depends, Query, Request, status
from fastapi.concurrency import run_in_threadpool
from fastapi.exceptions import RequestValidationError
from pydantic import ValidationError
from sqlalchemy.orm import Session

from ..dependencies import AuthenticatedUser, get_cloud_session, get_current_accepted_user
from .schemas import (MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES, ObjectEnvelopeDto,
                      V2EncryptedSyncAckRequest, V2EncryptedSyncCapabilitiesResponse,
                      V2EncryptedSyncCutoverRequest, V2EncryptedSyncCutoverResponse,
                      V2EncryptedSyncPullItem, V2EncryptedSyncPullResponse,
                      V2EncryptedSyncPushRequest, V2EncryptedSyncPushResponse,
                      V2SyncPullEvent, V2_SYNC_PROTOCOL_VERSION,
                      V2_ENCRYPTED_SYNC_VERSION, SyncPushResult,
                      encode_canonical_base64url)
from .services import SyncProtocolError, SyncService
from .sync_router import _error, _inline_json_schema, _read_encrypted_push_body


router = APIRouter(prefix='/api/v2/sync/encrypted', tags=['cloud sync v2'])
_V2_ENCRYPTED_PUSH_OPENAPI_SCHEMA = _inline_json_schema(V2EncryptedSyncPushRequest)


@router.get('/capabilities', response_model=V2EncryptedSyncCapabilitiesResponse)
def capabilities(current: AuthenticatedUser = Depends(get_current_accepted_user),
                 session: Session = Depends(get_cloud_session)) -> V2EncryptedSyncCapabilitiesResponse:
    writer_transport_version, cutover_epoch = SyncService().capabilities(session, current.user.id)
    return V2EncryptedSyncCapabilitiesResponse(
        writer_transport_version=writer_transport_version,
        cutover_epoch=cutover_epoch,
    )


@router.post('/cutover', response_model=V2EncryptedSyncCutoverResponse)
def cutover(request: V2EncryptedSyncCutoverRequest,
            current: AuthenticatedUser = Depends(get_current_accepted_user),
            session: Session = Depends(get_cloud_session)) -> V2EncryptedSyncCutoverResponse:
    try:
        writer_transport_version, cutover_epoch = SyncService().cutover_to_v2(
            session, current.user.id, request.expected_cutover_epoch,
            initial_web_device_id=request.initial_web_device_id,
        )
    except SyncProtocolError as error:
        raise _error(error) from None
    return V2EncryptedSyncCutoverResponse(
        writer_transport_version=writer_transport_version, cutover_epoch=cutover_epoch,
    )


@router.post('/push', response_model=V2EncryptedSyncPushResponse, openapi_extra={
    'requestBody': {'required': True, 'content': {'application/json': {'schema': _V2_ENCRYPTED_PUSH_OPENAPI_SCHEMA}}},
})
async def encrypted_push(http_request: Request, current: AuthenticatedUser = Depends(get_current_accepted_user),
                         session: Session = Depends(get_cloud_session)) -> V2EncryptedSyncPushResponse:
    body = await _read_encrypted_push_body(http_request)
    try:
        request = V2EncryptedSyncPushRequest.model_validate_json(body)
    except ValidationError as error:
        raise RequestValidationError(error.errors()) from None

    def push_in_worker():
        return SyncService().push_encrypted(
            session, current.user.id, request.device_id, request.items, transport_version=2,
        )

    try:
        results, cursor = await run_in_threadpool(push_in_worker)
    except SyncProtocolError as error:
        raise _error(error) from None
    return V2EncryptedSyncPushResponse(
        results=[SyncPushResult(event_id=row.event_id, server_sequence=row.server_sequence,
                                duplicate=row.duplicate) for row in results],
        current_cursor=cursor,
    )


@router.get('/pull', response_model=V2EncryptedSyncPullResponse)
def encrypted_pull(device_id: UUID, since: int = Query(ge=0, le=9_007_199_254_740_991),
                   limit: int = Query(default=200, ge=1, le=500),
                   protocol_version: int = Query(...), encrypted_sync_version: int = Query(...),
                   current: AuthenticatedUser = Depends(get_current_accepted_user),
                   session: Session = Depends(get_cloud_session)) -> V2EncryptedSyncPullResponse:
    if protocol_version != V2_SYNC_PROTOCOL_VERSION or encrypted_sync_version != V2_ENCRYPTED_SYNC_VERSION:
        raise _error(SyncProtocolError('encrypted_sync_version_unsupported', 'Unsupported encrypted sync version.', 422))
    try:
        rows, next_cursor, has_more, _high_water = SyncService().pull_encrypted_v2(
            session, current.user.id, device_id, since, limit,
        )
    except SyncProtocolError as error:
        raise _error(error) from None
    return V2EncryptedSyncPullResponse(items=[V2EncryptedSyncPullItem(
        event=V2SyncPullEvent(
            event_id=event.event_id, device_id=event.device_id, project_id=event.project_id,
            entity_id=event.entity_id, entity_type=event.entity_type, operation=event.operation,
            revision=event.revision, updated_at=event.updated_at, deleted_at=event.deleted_at,
            server_sequence=event.server_sequence,
        ),
        object=None if encrypted is None else ObjectEnvelopeDto(
            crypto_version=encrypted.crypto_version, aad_version=encrypted.aad_version,
            nonce=encode_canonical_base64url(encrypted.nonce), ciphertext=encode_canonical_base64url(encrypted.ciphertext),
        ),
    ) for event, encrypted in rows], next_cursor=next_cursor, has_more=has_more)


@router.post('/ack', status_code=status.HTTP_204_NO_CONTENT)
def ack(request: V2EncryptedSyncAckRequest, current: AuthenticatedUser = Depends(get_current_accepted_user),
        session: Session = Depends(get_cloud_session)) -> None:
    try:
        SyncService().ack(session, current.user.id, request.device_id, request.cursor, transport_version=2)
    except SyncProtocolError as error:
        raise _error(error) from None
