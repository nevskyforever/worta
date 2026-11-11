from __future__ import annotations

from uuid import UUID

from fastapi import APIRouter, Depends, HTTPException, Query, Request, status
from fastapi.concurrency import run_in_threadpool
from fastapi.exceptions import RequestValidationError
from pydantic import ValidationError
from sqlalchemy.orm import Session

from ..dependencies import AuthenticatedUser, get_cloud_session, get_current_accepted_user
from .schemas import (ENCRYPTED_SYNC_VERSION, MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES,
                      SYNC_PROTOCOL_VERSION, EncryptedSyncPullItem,
                      EncryptedSyncPullResponse, EncryptedSyncPushRequest,
                      EncryptedSyncPushResponse, ObjectEnvelopeDto, SyncAckRequest,
                      SyncDeviceResponse, SyncPullEvent, SyncPullResponse,
                      SyncPushRequest, SyncPushResponse, SyncPushResult,
                      encode_canonical_base64url)
from .services import SyncProtocolError, SyncService


router = APIRouter(prefix='/api/v1/sync', tags=['cloud sync'])


def _inline_json_schema(model: type[EncryptedSyncPushRequest]) -> dict:
    schema = model.model_json_schema()
    definitions = schema.pop('$defs', {})

    def resolve(value):
        if isinstance(value, list):
            return [resolve(item) for item in value]
        if not isinstance(value, dict):
            return value
        reference = value.get('$ref')
        if isinstance(reference, str) and reference.startswith('#/$defs/'):
            definition = definitions[reference.removeprefix('#/$defs/')]
            return resolve({**definition, **{key: item for key, item in value.items() if key != '$ref'}})
        return {key: resolve(item) for key, item in value.items()}

    return resolve(schema)


_ENCRYPTED_PUSH_OPENAPI_SCHEMA = _inline_json_schema(EncryptedSyncPushRequest)


def _error(error: SyncProtocolError) -> HTTPException:
    return HTTPException(status_code=error.status_code, detail={'code': error.code, 'message': error.message})


def _body_too_large() -> HTTPException:
    return HTTPException(status_code=413, detail={
        'code': 'encrypted_sync_batch_too_large',
        'message': 'Encrypted sync request exceeds wire size limit.',
    })


async def _read_encrypted_push_body(request: Request) -> bytes:
    content_length = request.headers.get('content-length')
    if content_length is not None:
        try:
            if int(content_length) > MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES:
                raise _body_too_large()
        except ValueError:
            pass
    body = bytearray()
    async for chunk in request.stream():
        if len(body) + len(chunk) > MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES:
            raise _body_too_large()
        body.extend(chunk)
    return bytes(body)


@router.put('/devices/{device_id}', response_model=SyncDeviceResponse)
def register_device(device_id: UUID, current: AuthenticatedUser = Depends(get_current_accepted_user),
                    session: Session = Depends(get_cloud_session), initial_web_setup: bool = False) -> SyncDeviceResponse:
    try:
        device_id, ack, _cursor = SyncService().register_device(
            session, current.user.id, device_id, initial_web_setup=initial_web_setup,
        )
    except SyncProtocolError as error:
        raise _error(error) from None
    return SyncDeviceResponse(device_id=device_id, last_ack_cursor=ack)


@router.post('/push', response_model=SyncPushResponse)
def push(request: SyncPushRequest, current: AuthenticatedUser = Depends(get_current_accepted_user),
         session: Session = Depends(get_cloud_session)) -> SyncPushResponse:
    service = SyncService()
    try:
        service.require_protocol(request.protocol_version)
        results, cursor = service.push(session, current.user.id, request.device_id, request.events)
    except SyncProtocolError as error:
        raise _error(error) from None
    return SyncPushResponse(results=[SyncPushResult(event_id=row.event_id, server_sequence=row.server_sequence,
                                                    duplicate=row.duplicate) for row in results], current_cursor=cursor)


@router.post('/encrypted/push', response_model=EncryptedSyncPushResponse, openapi_extra={
    'requestBody': {
        'required': True,
        'content': {'application/json': {'schema': _ENCRYPTED_PUSH_OPENAPI_SCHEMA}},
    },
})
async def encrypted_push(http_request: Request, current: AuthenticatedUser = Depends(get_current_accepted_user),
                         session: Session = Depends(get_cloud_session)) -> EncryptedSyncPushResponse:
    body = await _read_encrypted_push_body(http_request)
    try:
        request = EncryptedSyncPushRequest.model_validate_json(body)
    except ValidationError as error:
        raise RequestValidationError(error.errors()) from None

    def push_in_worker() -> tuple[list, int]:
        service = SyncService()
        service.require_protocol(request.protocol_version)
        service.require_encrypted_protocol(request.encrypted_sync_version)
        return service.push_encrypted(session, current.user.id, request.device_id, request.items)

    try:
        results, cursor = await run_in_threadpool(push_in_worker)
    except SyncProtocolError as error:
        raise _error(error) from None
    return EncryptedSyncPushResponse(
        results=[SyncPushResult(event_id=row.event_id, server_sequence=row.server_sequence,
                                duplicate=row.duplicate) for row in results], current_cursor=cursor,
    )


@router.get('/pull', response_model=SyncPullResponse)
def pull(device_id: UUID, since: int = Query(ge=0, le=9_007_199_254_740_991), limit: int = Query(default=200, ge=1, le=500),
         protocol_version: int = Query(default=SYNC_PROTOCOL_VERSION),
         current: AuthenticatedUser = Depends(get_current_accepted_user),
         session: Session = Depends(get_cloud_session)) -> SyncPullResponse:
    service = SyncService()
    try:
        service.require_protocol(protocol_version)
        events, next_cursor, has_more, _high_water = service.pull(session, current.user.id, device_id, since, limit)
    except SyncProtocolError as error:
        raise _error(error) from None
    return SyncPullResponse(events=[SyncPullEvent(
        event_id=row.event_id, device_id=row.device_id, project_id=row.project_id, entity_id=row.entity_id,
        entity_type=row.entity_type, operation=row.operation, revision=row.revision, updated_at=row.updated_at,
        deleted_at=row.deleted_at, server_sequence=row.server_sequence,
    ) for row in events], next_cursor=next_cursor, has_more=has_more)


@router.get('/encrypted/pull', response_model=EncryptedSyncPullResponse)
def encrypted_pull(device_id: UUID, since: int = Query(ge=0, le=9_007_199_254_740_991),
                   limit: int = Query(default=200, ge=1, le=500),
                   protocol_version: int = Query(default=SYNC_PROTOCOL_VERSION),
                   encrypted_sync_version: int = Query(default=ENCRYPTED_SYNC_VERSION),
                   current: AuthenticatedUser = Depends(get_current_accepted_user),
                   session: Session = Depends(get_cloud_session)) -> EncryptedSyncPullResponse:
    service = SyncService()
    try:
        service.require_protocol(protocol_version)
        service.require_encrypted_protocol(encrypted_sync_version)
        rows, next_cursor, has_more, _high_water = service.pull_encrypted(
            session, current.user.id, device_id, since, limit,
        )
    except SyncProtocolError as error:
        raise _error(error) from None
    return EncryptedSyncPullResponse(items=[EncryptedSyncPullItem(
        event=SyncPullEvent(
            event_id=event.event_id, device_id=event.device_id, project_id=event.project_id,
            entity_id=event.entity_id, entity_type=event.entity_type, operation=event.operation,
            revision=event.revision, updated_at=event.updated_at, deleted_at=event.deleted_at,
            server_sequence=event.server_sequence,
        ),
        object=None if encrypted is None else ObjectEnvelopeDto(
            crypto_version=encrypted.crypto_version, aad_version=encrypted.aad_version,
            nonce=encode_canonical_base64url(encrypted.nonce),
            ciphertext=encode_canonical_base64url(encrypted.ciphertext),
        ),
    ) for event, encrypted in rows], next_cursor=next_cursor, has_more=has_more)


@router.post('/ack', status_code=status.HTTP_204_NO_CONTENT)
def ack(request: SyncAckRequest, current: AuthenticatedUser = Depends(get_current_accepted_user),
        session: Session = Depends(get_cloud_session)) -> None:
    service = SyncService()
    try:
        service.require_protocol(request.protocol_version)
        service.ack(session, current.user.id, request.device_id, request.cursor)
    except SyncProtocolError as error:
        raise _error(error) from None
