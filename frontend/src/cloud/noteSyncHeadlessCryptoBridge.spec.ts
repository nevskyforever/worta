import { sealDocumentForOutbox } from '@/platform/web/documentOutbound'
import {selectWriterFrame,normalizeAuthenticatedFrame,COMPRESSION_LIMITS} from './frameCompression'
import {encryptProjectCover} from './projectCoverCrypto'
import {createCoverReference,authenticateCoverReference,type ProjectCoverReference} from './projectCoverReference'
import {frameProgressEvent,unframeProgressEvent} from './progressCodec'
import {frameGameEvent,unframeGameEvent} from './gameCodec'
import {encryptAccountObject,decryptAccountObject} from '@/crypto/accountObjectCrypto'
import {unframeDocumentEvent,frameDocumentEvent} from './documentCodec'
import {unframeMapEvent,frameMapEvent} from './mapCodec'
import {unframeContentNote,frameContentNote} from './contentNoteCodec'
import {frameCatalogEvent,sealCatalogEvent,openCatalogEvent,type CatalogEvent} from './accountCatalogCodec'
// @vitest-environment node
import { openProjectMetadataEvent, sealProjectMetadataEvent, encodeProjectMetadataEvent, type ProjectMetadataEvent } from './projectMetadataCodec'
import { openStructuralEvent, sealStructuralEvent, frameStructuralEvent, type StructuralEvent } from './stageCodec'
import { readFileSync, writeFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { asAccountMasterKey, decryptObjectBytes, encryptObjectBytes, generateAccountMasterKey } from '@/crypto'
import { decodeBase64Url, encodeBase64Url } from '@/api/base64url'
import { encryptedSyncObjectFromWire, encryptedSyncPushBodyBytes } from '@/api/encryptedSync'
import { encodeNoteSyncPlaintext, type NoteSyncRecord } from './noteSyncCodec'
import { normalizeC15SyncEvent, openNoteSyncEvent, sealNoteSyncEvent } from './encryptedSyncProtocol'
import type { SyncEventEnvelope } from './syncProtocol'
import { decodeNoteSyncResolutionV2, encodeNoteSyncResolutionV2, type NoteSyncResolutionV2 } from './noteSyncResolutionV2Codec'
import { resolutionEnvelopeWire } from './noteSyncResolutionSealing'
import { sealPendingNoteSyncIntents, type UnsealedNoteSyncIntent } from './noteSyncIntent'
import type { RuntimeKeyContext } from '@/auth/keyContext'

interface SealRequest {
  action: 'seal'
  canonical_user_id: string
  device_id: string
  event: SyncEventEnvelope
  note: NoteSyncRecord
  amk?: number[]
  parent_event_id?: string | null
}

interface OpenRequest {
  action: 'open'
  canonical_user_id: string
  amk: number[]
  item: {
    event: SyncEventEnvelope & { device_id: string; server_sequence: number }
    object: { crypto_version: number; aad_version: number; nonce: string; ciphertext: string }
  }
}

interface ResolutionEncodeRequest { action: 'resolution_encode'; payload: NoteSyncResolutionV2 }
interface ResolutionSealRequest {
  action: 'resolution_seal'; canonical_user_id: string; amk: number[]; canonical_payload: string
  event: SyncEventEnvelope; device_id: string
}
interface ResolutionOpenRequest { action: 'resolution_open'; canonical_user_id: string; amk: number[]; item: OpenRequest['item'] }
interface SealIntentRequest { action: 'seal_intent'; canonical_user_id: string; amk: number[]; intent: UnsealedNoteSyncIntent }

interface MetadataSealRequest { action: 'metadata_seal'; payload: ProjectMetadataEvent; amk: number[] }
interface MetadataOpenRequest { action: 'metadata_open'; canonical_user_id: string; amk: number[]; item: OpenRequest['item'] }
interface StructuralSealRequest { action: 'structural_seal'; payload: StructuralEvent; amk: number[] }
interface StructuralOpenRequest { action: 'structural_open'; canonical_user_id: string; amk: number[]; item: { event: StructuralEvent['header']; object: OpenRequest['item']['object'] } }
interface CatalogSealRequest {action:'catalog_seal';payload:CatalogEvent;amk:number[]}
interface CatalogOpenRequest {action:'catalog_open';canonical_user_id:string;amk:number[];item:{event:{entity_id:string;entity_type:string};object:OpenRequest['item']['object']}}
interface ContentSealRequest{action:'content_note_seal';frame:number[];amk:number[]}
interface ContentOpenRequest{action:'content_note_open';canonical_user_id:string;amk:number[];item:OpenRequest['item']}
type DocumentEventSealRequest = { payload: import('./documentCodec').DocumentEvent; amk: number[] }
  & ({ action: 'document_event_seal' } | { action: 'web_document_seal' })
interface DocumentSealRequest{action:'document_seal';frame:number[];amk:number[];compression_ready?:boolean}
interface DocumentOpenRequest{action:'document_open';canonical_user_id:string;amk:number[];item:OpenRequest['item']}
interface MapEventSealRequest{action:'map_event_seal';payload:import('./mapCodec').MapEvent;amk:number[]}
interface MapSealRequest{action:'map_seal';frame:number[];amk:number[];compression_ready?:boolean}
interface MapOpenRequest{action:'map_open';canonical_user_id:string;amk:number[];item:OpenRequest['item']}
interface ProgressEventSealRequest{action:'progress_event_seal';payload:import('./progressCodec').ProgressEvent;amk:number[]}
interface ProgressSealRequest{action:'progress_seal';frame:number[];amk:number[]}
interface ProgressOpenRequest{action:'progress_open';canonical_user_id:string;amk:number[];item:OpenRequest['item']}
interface GameSealRequest{action:'game_seal';scope:'project'|'account';frame:number[];amk:number[]}
interface GameShopRequest {
  action: 'game_shop'; amk: number[]; scope: import('@/platform/web/syncDurability').BrowserSyncScope
  head: import('./gameCodec').AccountGameEvent; base: import('./gameCodec').AccountGameBase
  command: import('./gameShop').GameShopCommand; now: string; event_id: string
}
interface GameOpenRequest{action:'game_open';scope:'project'|'account';canonical_user_id:string;amk:number[];item:{event:{event_id:string;device_id:string;project_id?:string;canonical_user_id?:string;entity_id:string;entity_type:string};object:OpenRequest['item']['object']}}
interface CoverSealRequest {action:'cover_seal';amk:number[];identity:{userId:string;projectId:string;blobId:string};jpeg:number[]}
interface CoverOpenRequest {action:'cover_open';amk:number[];identity:{userId:string;projectId:string;blobId:string};reference:ProjectCoverReference;object:OpenRequest['item']['object']}
type BridgeRequest = CoverSealRequest | CoverOpenRequest | GameSealRequest | GameOpenRequest | GameShopRequest | ProgressEventSealRequest | ProgressSealRequest | ProgressOpenRequest | DocumentEventSealRequest | DocumentSealRequest | DocumentOpenRequest | MapEventSealRequest | MapSealRequest | MapOpenRequest | ContentSealRequest | ContentOpenRequest | CatalogSealRequest | CatalogOpenRequest | StructuralSealRequest | StructuralOpenRequest | MetadataSealRequest | MetadataOpenRequest | SealRequest | OpenRequest | ResolutionEncodeRequest | ResolutionSealRequest | ResolutionOpenRequest | SealIntentRequest

async function execute(request: BridgeRequest): Promise<Record<string, unknown>> {
  if (request.action === 'game_shop') {
    const { prepareGameShopAction } = await import('./gameShop')
    const { sealAccountGame } = await import('@/platform/web/gameOutbound')
    const event = prepareGameShopAction(request.base, request.head, request.scope.deviceId,
      request.command, request.now, request.event_id)
    const raw_body = await sealAccountGame(request.scope, event, asAccountMasterKey(Uint8Array.from(request.amk)))
    return { raw_body }
  }
  if(request.action==='cover_seal'){
    const amk=asAccountMasterKey(Uint8Array.from(request.amk)),jpeg=Uint8Array.from(request.jpeg)
    const e=await encryptProjectCover(amk,request.identity,jpeg),reference=await createCoverReference(amk,request.identity,jpeg,e)
    return {reference,object:resolutionEnvelopeWire(e),nonce:[...e.nonce],ciphertext:[...e.ciphertext],jpeg:[...jpeg]}
  }
  if(request.action==='cover_open'){
    const e=encryptedSyncObjectFromWire(request.object),jpeg=await authenticateCoverReference(asAccountMasterKey(Uint8Array.from(request.amk)),request.identity,request.reference,e)
    return {reference:request.reference,nonce:[...e.nonce],ciphertext:[...e.ciphertext],jpeg:[...jpeg]}
  }
  if(request.action==='game_seal'){
    const frame=Uint8Array.from(request.frame),e=unframeGameEvent(frame,request.scope),h=e.header
    expect([...frameGameEvent(e)]).toEqual(request.frame)
    const amk=asAccountMasterKey(Uint8Array.from(request.amk))
    const o=h.scope==='project'?await encryptObjectBytes(amk,{userId:h.account_id,projectId:h.project_id,entityType:'project_game',entityId:h.entity_id},frame):await encryptAccountObject(amk,{userId:h.account_id,scope:'account',entityType:'account_game',entityId:h.entity_id},frame)
    return {object:{crypto_version:o.crypto_version,aad_version:o.aad_version,nonce:encodeBase64Url(o.nonce),ciphertext:encodeBase64Url(o.ciphertext)},nonce:[...o.nonce],ciphertext:[...o.ciphertext],frame:[...frame]}
  }
  if(request.action==='game_open'){
    const d=request.item.event,wire=request.item.object,nonce=decodeBase64Url(wire.nonce),ciphertext=decodeBase64Url(wire.ciphertext),amk=asAccountMasterKey(Uint8Array.from(request.amk))
    expect(wire.crypto_version).toBe(request.scope==='project'?1:2);expect(wire.aad_version).toBe(wire.crypto_version)
    const frame=request.scope==='project'?await decryptObjectBytes(amk,{userId:request.canonical_user_id,projectId:d.project_id!,entityType:'project_game',entityId:d.entity_id},{crypto_version:1,aad_version:1,nonce,ciphertext}):await decryptAccountObject(amk,{userId:request.canonical_user_id,scope:'account',entityType:'account_game',entityId:d.entity_id},{crypto_version:2,aad_version:2,nonce,ciphertext})
    const decoded=unframeGameEvent(frame,request.scope);expect(decoded.header.event_id).toBe(d.event_id);expect(decoded.header.account_id).toBe(request.canonical_user_id);expect(decoded.header.device_id).toBe(d.device_id);expect(decoded.header.entity_id).toBe(d.entity_id)
    if(decoded.header.scope==='project')expect(decoded.header.project_id).toBe(d.project_id);else expect(d.canonical_user_id).toBe(request.canonical_user_id)
    return {decoded,nonce:[...nonce],ciphertext:[...ciphertext],frame:[...frame]}
  }
  if (request.action === 'web_document_seal') {
    const h = request.payload.header
    const body = await sealDocumentForOutbox({ canonicalUserId: h.account_id, deviceId: h.device_id, accountId: h.account_id },
      request.payload, asAccountMasterKey(Uint8Array.from(request.amk)))
    return JSON.parse(body)
  }
  if(request.action==='document_event_seal'){return execute({action:'document_seal',frame:[...await frameDocumentEvent(request.payload)],amk:request.amk})}
  if(request.action==='progress_event_seal'){return execute({action:'progress_seal',frame:[...await frameProgressEvent(request.payload)],amk:request.amk})}
  if(request.action==='progress_seal'){
    const frame=Uint8Array.from(request.frame),e=await unframeProgressEvent(frame),h=e.header
    expect([...await frameProgressEvent(e)]).toEqual(request.frame)
    const o=await encryptObjectBytes(asAccountMasterKey(Uint8Array.from(request.amk)),{userId:h.account_id,projectId:h.project_id,entityType:'progress',entityId:h.entity_id},frame)
    return {object:resolutionEnvelopeWire(o),nonce:[...o.nonce],ciphertext:[...o.ciphertext],frame:[...frame]}
  }
  if(request.action==='progress_open'){
    const e=request.item.event,o=encryptedSyncObjectFromWire(request.item.object)
    const frame=await decryptObjectBytes(asAccountMasterKey(Uint8Array.from(request.amk)),{userId:request.canonical_user_id,projectId:e.project_id,entityType:'progress',entityId:e.entity_id},o)
    const decoded=await unframeProgressEvent(frame);expect(decoded.header.event_id).toBe(e.event_id)
    return {decoded,nonce:[...o.nonce],ciphertext:[...o.ciphertext],frame:[...frame]}
  }
  if(request.action==='document_seal'){
    const original=Uint8Array.from(request.frame),e=await unframeDocumentEvent(original),h=e.header
    expect([...await frameDocumentEvent(e)]).toEqual([...normalizeAuthenticatedFrame(original,[10],[1],COMPRESSION_LIMITS.bytes)])
    const frame=await selectWriterFrame(original,async()=>request.compression_ready===true)
    const o=await encryptObjectBytes(asAccountMasterKey(Uint8Array.from(request.amk)),{userId:h.account_id,projectId:h.project_id,entityType:'document',entityId:h.entity_id},frame)
    return {object:resolutionEnvelopeWire(o),nonce:[...o.nonce],ciphertext:[...o.ciphertext],frame:[...frame]}
  }
  if(request.action==='document_open'){
    const e=request.item.event,o=encryptedSyncObjectFromWire(request.item.object)
    const frame=await decryptObjectBytes(asAccountMasterKey(Uint8Array.from(request.amk)),{userId:request.canonical_user_id,projectId:e.project_id,entityType:'document',entityId:e.entity_id},o)
    const decoded=await unframeDocumentEvent(frame);expect(decoded.header.event_id).toBe(e.event_id)
    return {decoded,nonce:[...o.nonce],ciphertext:[...o.ciphertext],frame:[...frame]}
  }
  if(request.action==='map_event_seal'){return execute({action:'map_seal',frame:[...await frameMapEvent(request.payload)],amk:request.amk})}
  if(request.action==='map_seal'){
    const original=Uint8Array.from(request.frame),e=await unframeMapEvent(original),h=e.header
    expect([...await frameMapEvent(e)]).toEqual([...normalizeAuthenticatedFrame(original,[9],[1],COMPRESSION_LIMITS.bytes)])
    const frame=await selectWriterFrame(original,async()=>request.compression_ready===true)
    const o=await encryptObjectBytes(asAccountMasterKey(Uint8Array.from(request.amk)),{userId:h.account_id,projectId:h.project_id,entityType:'map',entityId:h.entity_id},frame)
    return {object:resolutionEnvelopeWire(o),nonce:[...o.nonce],ciphertext:[...o.ciphertext],frame:[...frame]}
  }
  if(request.action==='map_open'){
    const e=request.item.event,o=encryptedSyncObjectFromWire(request.item.object)
    const frame=await decryptObjectBytes(asAccountMasterKey(Uint8Array.from(request.amk)),{userId:request.canonical_user_id,projectId:e.project_id,entityType:'map',entityId:e.entity_id},o)
    const decoded=await unframeMapEvent(frame);expect(decoded.header.event_id).toBe(e.event_id)
    return {decoded,nonce:[...o.nonce],ciphertext:[...o.ciphertext],frame:[...frame]}
  }
  if(request.action==='content_note_seal'){
    const frame=new Uint8Array(request.frame),event=unframeContentNote(frame),h=event.event.header
    expect([...frameContentNote(event)]).toEqual(request.frame)
    const object=await encryptObjectBytes(asAccountMasterKey(new Uint8Array(request.amk)),{userId:event.account_id,projectId:h.project_id,entityType:'note',entityId:h.entity_id},frame)
    return {object:resolutionEnvelopeWire(object),nonce:[...object.nonce],ciphertext:[...object.ciphertext],frame:[...frame]}
  }
  if(request.action==='content_note_open'){
    const e=request.item.event,o=encryptedSyncObjectFromWire(request.item.object)
    const frame=await decryptObjectBytes(asAccountMasterKey(new Uint8Array(request.amk)),{userId:request.canonical_user_id,projectId:e.project_id,entityType:'note',entityId:e.entity_id},o)
    const decoded=unframeContentNote(frame)
    expect(decoded.event.header.event_id).toBe(e.event_id)
    return {decoded,nonce:[...o.nonce],ciphertext:[...o.ciphertext],frame:[...frame]}
  }
  if(request.action==='catalog_seal'){
    const object=await sealCatalogEvent(asAccountMasterKey(Uint8Array.from(request.amk)),request.payload)
    return {object:{crypto_version:2,aad_version:2,nonce:encodeBase64Url(object.nonce),ciphertext:encodeBase64Url(object.ciphertext)},nonce:Array.from(object.nonce),ciphertext:Array.from(object.ciphertext),frame:Array.from(frameCatalogEvent(request.payload))}
  }
  if(request.action==='catalog_open'){
    const o=request.item.object,nonce=decodeBase64Url(o.nonce),ciphertext=decodeBase64Url(o.ciphertext)
    if(o.crypto_version!==2||o.aad_version!==2)throw new TypeError('invalid_catalog_frame')
    const decoded=await openCatalogEvent(asAccountMasterKey(Uint8Array.from(request.amk)),request.canonical_user_id,request.item.event.entity_id,request.item.event.entity_type,{crypto_version:2,aad_version:2,nonce,ciphertext})
    return {decoded,nonce:Array.from(nonce),ciphertext:Array.from(ciphertext),frame:Array.from(frameCatalogEvent(decoded))}
  }
  if (request.action === 'structural_seal') {
    const object = await sealStructuralEvent(asAccountMasterKey(Uint8Array.from(request.amk)), request.payload)
    return { object: resolutionEnvelopeWire(object), nonce: Array.from(object.nonce), ciphertext: Array.from(object.ciphertext), frame: Array.from(frameStructuralEvent(request.payload)) }
  }
  if (request.action === 'structural_open') {
    const envelope = encryptedSyncObjectFromWire(request.item.object), h = request.item.event
    const decoded = await openStructuralEvent(asAccountMasterKey(Uint8Array.from(request.amk)), {account_id: request.canonical_user_id, project_id: h.project_id, entity_id: h.entity_id, entity_type: h.entity_type, event_id: h.event_id}, envelope)
    return { decoded, nonce: Array.from(envelope.nonce), ciphertext: Array.from(envelope.ciphertext), frame: Array.from(frameStructuralEvent(decoded)) }
  }
  if (request.action === 'metadata_seal') {
    const object = await sealProjectMetadataEvent(asAccountMasterKey(Uint8Array.from(request.amk)), request.payload)
    return { object: resolutionEnvelopeWire(object), nonce: Array.from(object.nonce), ciphertext: Array.from(object.ciphertext) }
  }
  if (request.action === 'metadata_open') {
    const envelope=encryptedSyncObjectFromWire(request.item.object)
    const event=request.item.event
    const decoded=await openProjectMetadataEvent(asAccountMasterKey(Uint8Array.from(request.amk)),
      { account_id: request.canonical_user_id, project_id: event.project_id, entity_id: event.entity_id, event_id: event.event_id },envelope)
    return { decoded, nonce:Array.from(envelope.nonce), ciphertext:Array.from(envelope.ciphertext), plaintext:Array.from(encodeProjectMetadataEvent(decoded)) }
  }
  if (request.action === 'seal_intent') {
    let object: ReturnType<typeof resolutionEnvelopeWire> | undefined
    const keyContext = {
      leaseForAccount: (accountId: string) => ({
        localAccountId: accountId, canonicalUserId: request.canonical_user_id,
        isCurrent: () => true,
        use: async <T>(operation: (key: ReturnType<typeof asAccountMasterKey>) => Promise<T>) =>
          operation(asAccountMasterKey(Uint8Array.from(request.amk))),
      }),
    } as RuntimeKeyContext
    const result = await sealPendingNoteSyncIntents({
      list: async () => [request.intent],
      recordSealFailure: async () => { throw new Error('Production Note intent sealing failed.') },
      commitSealedEvent: async input => { object = resolutionEnvelopeWire(input.envelope); return 'sealed' },
    }, keyContext)
    expect(result.results[0]?.status).toBe('sealed')
    expect(object).toBeDefined()
    return { object, event: normalizeC15SyncEvent({
      event_id: request.intent.event_id, project_id: request.intent.project_id,
      entity_id: request.intent.entity_id, entity_type: 'note', operation: request.intent.operation,
      revision: request.intent.revision, updated_at: request.intent.updated_at,
      deleted_at: request.intent.deleted_at,
    }) }
  }
  if (request.action === 'resolution_encode') {
    const bytes = encodeNoteSyncResolutionV2(request.payload)
    return { canonical_payload: encodeBase64Url(bytes), decoded: decodeNoteSyncResolutionV2(bytes) }
  }
  if (request.action === 'resolution_seal') {
    const bytes = decodeBase64Url(request.canonical_payload)
    const payload = decodeNoteSyncResolutionV2(bytes)
    expect(payload.header.event_id).toBe(request.event.event_id)
    expect(payload.header.revision).toBe(request.event.revision)
    const object = await encryptObjectBytes(asAccountMasterKey(Uint8Array.from(request.amk)), {
      userId: request.canonical_user_id, projectId: payload.header.project_id,
      entityId: payload.header.entity_id, entityType: 'note',
    }, bytes)
    return { object: resolutionEnvelopeWire(object) }
  }
  if (request.action === 'resolution_open') {
    const envelope = encryptedSyncObjectFromWire(request.item.object)
    const plaintext = await decryptObjectBytes(asAccountMasterKey(Uint8Array.from(request.amk)), {
      userId: request.canonical_user_id, projectId: request.item.event.project_id,
      entityId: request.item.event.entity_id, entityType: 'note',
    }, envelope)
    return { crypto_version: envelope.crypto_version, aad_version: envelope.aad_version,
      nonce: Array.from(envelope.nonce), ciphertext: Array.from(envelope.ciphertext),
      plaintext: Array.from(plaintext), decoded: decodeNoteSyncResolutionV2(plaintext) }
  }
  if (request.action === 'seal') {
    const amk = request.amk === undefined
      ? await generateAccountMasterKey()
      : asAccountMasterKey(Uint8Array.from(request.amk))
    const sealed = await sealNoteSyncEvent(
      amk, request.canonical_user_id, request.event, request.parent_event_id ?? null, request.note,
    )
    const push = {
      protocol_version: 1 as const,
      encrypted_sync_version: 1 as const,
      device_id: request.device_id,
      items: [sealed],
    }
    expect(encryptedSyncPushBodyBytes(push)).toBeGreaterThan(0)
    return {
      amk: Array.from(amk),
      push: {
        protocol_version: 1,
        encrypted_sync_version: 1,
        device_id: request.device_id,
        items: [{
          event: sealed.event,
          object: {
            crypto_version: sealed.object.crypto_version,
            aad_version: sealed.object.aad_version,
            nonce: encodeBase64Url(sealed.object.nonce),
            ciphertext: encodeBase64Url(sealed.object.ciphertext),
          },
        }],
      },
    }
  }

  const envelope = encryptedSyncObjectFromWire(request.item.object)
  const plaintext = await openNoteSyncEvent(
    asAccountMasterKey(Uint8Array.from(request.amk)),
    request.canonical_user_id,
    request.item.event,
    envelope,
  )
  return {
    crypto_version: envelope.crypto_version,
    aad_version: envelope.aad_version,
    nonce: Array.from(envelope.nonce),
    ciphertext: Array.from(envelope.ciphertext),
    plaintext: Array.from(encodeNoteSyncPlaintext(plaintext)),
    decoded: plaintext,
  }
}

function defaultRequest(): SealRequest {
  const timestamp = '2026-09-23T00:00:00.000000Z'
  return {
    action: 'seal',
    canonical_user_id: '123e4567-e89b-42d3-a456-426614174099',
    device_id: '123e4567-e89b-42d3-a456-426614174001',
    event: {
      event_id: '123e4567-e89b-42d3-a456-426614174010',
      project_id: 'project-1',
      entity_id: 'note-1',
      entity_type: 'note',
      operation: 'upsert',
      revision: 1,
      updated_at: timestamp,
      deleted_at: null,
    },
    note: {
      id: 'note-1', project_id: 'project-1', stage_id: null, source_type: 'project',
      source_map_id: null, source_node_id: null, content_format: 'html', title: 'headless title',
      content: '<p>headless private body</p>', checklist: [], color: 'default', pinned: false,
      archived: false, sort_order: 0, tags: [], created_at: timestamp, updated_at: timestamp, metadata: {},
    },
  }
}

describe('C15 test-only headless crypto bridge', () => {
  it('seals an ordinary causal update with the supplied parent', async () => {
    const request = defaultRequest()
    request.event.event_id = '123e4567-e89b-42d3-a456-426614174011'
    request.event.revision = 2
    request.parent_event_id = '123e4567-e89b-42d3-a456-426614174010'
    const sealed = await execute(request)
    const opened = await execute({
      action: 'open', canonical_user_id: request.canonical_user_id,
      amk: sealed.amk as number[], item: {
        event: { ...request.event, device_id: request.device_id, server_sequence: 1 },
        object: (sealed.push as { items: Array<{ object: OpenRequest['item']['object'] }> }).items[0]!.object,
      },
    })
    const decoded = opened.decoded as { mutation: string; header: { parent_event_id: string } }
    expect(decoded.mutation).toBe('update')
    expect(decoded.header.parent_event_id).toBe(request.parent_event_id)
  })

  it('uses production sealing/opening code and emits only an explicit temporary response', async () => {
    const requestPath = process.env.NFPROGRESS_C15_CRYPTO_BRIDGE_REQUEST
    const responsePath = process.env.NFPROGRESS_C15_CRYPTO_BRIDGE_RESPONSE
    if (requestPath === undefined && responsePath === undefined) {
      const sealed = await execute(defaultRequest())
      const push = sealed.push as { items: Array<{ object: { ciphertext: string } }> }
      expect(decodeBase64Url(push.items[0]!.object.ciphertext).byteLength).toBeGreaterThan(16)
      return
    }
    if (requestPath === undefined || responsePath === undefined) throw new Error('Incomplete crypto bridge environment.')
    const request = JSON.parse(readFileSync(requestPath, 'utf8')) as BridgeRequest
    const response = await execute(request)
    writeFileSync(responsePath, JSON.stringify(response), { encoding: 'utf8', mode: 0o600 })
    expect(response).toBeDefined()
  })
})
