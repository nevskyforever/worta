import { decodeBase64Url, encodeBase64Url } from './base64url'
import { apiRequest, ApiError } from './client'
import { MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES, MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES, MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES } from './encryptedSync'
import { parseSyncTimestamp } from '@/cloud/syncTimestamp'
import { ACCOUNT_ENTITY_TYPES, encodeAccountTuple, type AccountObjectEnvelope } from '@/crypto/accountObjectCrypto'
import type { ObjectCryptoEnvelope } from '@/crypto'

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
const fail = (): never => { throw new TypeError('invalid_encrypted_sync_v3') }
const exact = (value: unknown, names: readonly string[]): boolean => typeof value === 'object' && value !== null
  && !Array.isArray(value) && Object.keys(value).length === names.length && names.every(name => Object.prototype.hasOwnProperty.call(value, name))
const safe = (value: unknown, minimum: number): value is number => Number.isSafeInteger(value) && (value as number) >= minimum
const mapId = (value: string): boolean => value === 'project-map' || /^stage-map-[0-9a-f]{64}$/.test(value)
const headers = (token: string) => new Headers({ Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' })

export interface V3Descriptor {
  event_id: string; device_id: string; server_sequence: number; project_id: string; entity_id: string
  entity_type: 'note' | 'project_metadata' | 'stage' | 'stage_order' | 'map' | 'document' | 'progress' | 'project_game'; operation: 'upsert' | 'delete' | 'resolution' | 'event'
  revision: number; updated_at: string; deleted_at: string | null
}
export interface AccountDescriptor {
  event_id: string; device_id: string; server_sequence: number; canonical_user_id: string; scope: 'account';
  entity_id: string; entity_type: typeof ACCOUNT_ENTITY_TYPES[number]; operation: 'upsert' | 'delete';
  revision: number; updated_at: string; deleted_at: string | null
}
export type V3PullItem = { event: V3Descriptor; object: ObjectCryptoEnvelope } | { event: AccountDescriptor; object: AccountObjectEnvelope }
export function isAccountItem(item: V3PullItem): item is { event: AccountDescriptor; object: AccountObjectEnvelope } { return 'scope' in item.event }

export interface V3PullResponse { protocol_version: 3; encrypted_sync_version: 3; items: V3PullItem[]; next_cursor: number; has_more: boolean }
export interface V3MetadataPushItem {
  event: Pick<V3Descriptor, 'event_id' | 'project_id' | 'entity_id' | 'entity_type' | 'operation' | 'revision' | 'updated_at' | 'deleted_at'>
  object: ObjectCryptoEnvelope
}
export interface V3PushResponse { protocol_version: 3; encrypted_sync_version: 3; results: Array<{ event_id: string; server_sequence: number; duplicate: boolean }>; current_cursor: number }

export function parseV3Pull(value: unknown, since: number, limit: number): V3PullResponse {
  if (!safe(since, 0) || !safe(limit, 1) || limit > 500 || !exact(value, ['protocol_version', 'encrypted_sync_version', 'items', 'next_cursor', 'has_more'])) fail()
  const page = value as V3PullResponse
  if (page.protocol_version !== 3 || page.encrypted_sync_version !== 3 || !Array.isArray(page.items)
    || page.items.length > limit || !safe(page.next_cursor, since) || typeof page.has_more !== 'boolean') fail()
  let previous = since, total = 0
  const ids = new Set<string>()
  const items = page.items.map((item: unknown): V3PullItem => {
    if (!exact(item, ['event', 'object'])) fail()
    const row = item as V3PullItem
    if (typeof row.event !== 'object' || row.event === null || Array.isArray(row.event)) fail()
    const account = 'scope' in row.event
    if (!exact(row.event, account ? ['event_id','device_id','server_sequence','canonical_user_id','scope','entity_id','entity_type','operation','revision','updated_at','deleted_at'] : ['event_id', 'device_id', 'server_sequence', 'project_id', 'entity_id', 'entity_type', 'operation', 'revision', 'updated_at', 'deleted_at'])
      || !exact(row.object, ['crypto_version', 'aad_version', 'nonce', 'ciphertext'])) fail()
    const e = row.event as Omit<V3Descriptor, 'entity_type'> & { entity_type: string; canonical_user_id?: string; scope?: 'account' }
    if (account) {
      if (!UUID.test(e.canonical_user_id ?? '') || e.scope !== 'account' || !ACCOUNT_ENTITY_TYPES.includes(e.entity_type as typeof ACCOUNT_ENTITY_TYPES[number]) || !['upsert','delete'].includes(e.operation)) fail()
      try { encodeAccountTuple({ userId: e.canonical_user_id!, scope: e.scope!, entityId: e.entity_id, entityType: e.entity_type }) } catch { fail() }
    }
    if (!UUID.test(e.event_id) || !UUID.test(e.device_id) || ids.has(e.event_id)
      || !account && (typeof e.project_id !== 'string' || !e.project_id || e.project_id.length > 512)
      || typeof e.entity_id !== 'string' || !e.entity_id || e.entity_id.length > 512
      || !account && !['note', 'project_metadata', 'stage', 'stage_order', 'map', 'document', 'progress', 'project_game'].includes(e.entity_type) || !['upsert', 'delete', 'resolution', 'event'].includes(e.operation)
      || e.operation === 'event' && !['note', 'map', 'document', 'progress', 'project_game'].includes(e.entity_type)
      || e.entity_type === 'map' && (e.operation !== 'event' || e.deleted_at !== null || !mapId(e.entity_id))
      || ['document','progress','project_game'].includes(e.entity_type) && (e.operation !== 'event' || e.deleted_at !== null)
      || e.entity_type === 'project_game' && !(e.entity_id === `game:project:${e.event_id}` || e.entity_id.startsWith('game:stage:') && e.entity_id.endsWith(`:${e.event_id}`) && e.entity_id.length > 48)
      || e.entity_type === 'account_game' && (e.entity_id !== `game:${e.event_id}` || e.operation !== 'upsert' || e.deleted_at !== null)
      || e.entity_type === 'project_metadata' && e.entity_id !== e.project_id
      || e.entity_type === 'stage_order' && (e.entity_id !== 'stage_order' || e.operation !== 'upsert')
      || e.entity_type === 'stage' && e.operation === 'resolution'
      || !safe(e.server_sequence, previous + 1) || !safe(e.revision, e.operation === 'resolution' ? 2 : 1)
      || typeof e.updated_at !== 'string' || (e.operation === 'delete' ? typeof e.deleted_at !== 'string' : e.deleted_at !== null)) fail()
    try { parseSyncTimestamp(e.updated_at); if (e.deleted_at !== null) parseSyncTimestamp(e.deleted_at) } catch { fail() }
    const object = row.object as unknown as { crypto_version: number; aad_version: number; nonce: string; ciphertext: string }
    if (object.crypto_version !== (account ? 2 : 1) || object.aad_version !== (account ? 2 : 1) || typeof object.nonce !== 'string' || typeof object.ciphertext !== 'string') fail()
    const nonce = decodeBase64Url(object.nonce, { expectedLength: 24 })
    const ciphertext = decodeBase64Url(object.ciphertext, { minimumLength: 16, maximumLength: MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES })
    if (encodeBase64Url(nonce) !== object.nonce || encodeBase64Url(ciphertext) !== object.ciphertext) fail()
    total += ciphertext.length
    if (total > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES) fail()
    ids.add(e.event_id); previous = e.server_sequence
    return account ? { event: row.event as AccountDescriptor, object: { crypto_version: 2, aad_version: 2, nonce, ciphertext } } : { event: e as V3Descriptor, object: { crypto_version: 1, aad_version: 1, nonce, ciphertext } }
  })
  if (page.next_cursor !== previous || (!items.length && page.has_more)) fail()
  return { ...page, items }
}

export function encodeV3MetadataPush(deviceId: string, items: readonly V3MetadataPushItem[]): string {
  return encodeV3ProjectPush(deviceId, items, false)
}
export function encodeV3MapPush(deviceId: string, items: readonly V3MetadataPushItem[]): string {
  return encodeV3ProjectPush(deviceId, items, true)
}
export function encodeV3DocumentPush(deviceId:string,items:readonly V3MetadataPushItem[]):string{return encodeV3ProjectPush(deviceId,items,'document')}
function encodeV3ProjectPush(deviceId: string, items: readonly V3MetadataPushItem[], maps: boolean | 'document' | 'progress' | 'project_game'): string {
  if (!UUID.test(deviceId) || !items.length || items.length > 100) fail()
  let total = 0
  const ids = new Set<string>()
  const wire = items.map(item => {
    const e = item.event
    if (!exact(e, ['event_id', 'project_id', 'entity_id', 'entity_type', 'operation', 'revision', 'updated_at', 'deleted_at'])
      || !UUID.test(e.event_id) || ids.has(e.event_id) || !(maps ? e.entity_type === (typeof maps==='string'?maps:'map') : ['project_metadata', 'stage', 'stage_order'].includes(e.entity_type))
      || maps && (e.operation !== 'event' || maps===true && !mapId(e.entity_id) || e.deleted_at !== null)
      || e.entity_type === 'project_game' && !(e.entity_id === `game:project:${e.event_id}` || e.entity_id.startsWith('game:stage:') && e.entity_id.endsWith(`:${e.event_id}`) && e.entity_id.length > 48)
      || e.entity_type === 'project_metadata' && e.entity_id !== e.project_id
      || e.entity_type === 'stage_order' && (e.entity_id !== 'stage_order' || e.operation !== 'upsert')
      || e.entity_type === 'stage' && e.operation === 'resolution'
      || !e.entity_id || e.entity_id.length > 512 || !e.project_id || e.project_id.length > 512
      || !(maps ? e.operation === 'event' : ['upsert', 'delete', 'resolution'].includes(e.operation)) || !safe(e.revision, e.operation === 'resolution' ? 2 : 1)
      || typeof e.updated_at !== 'string' || (e.operation === 'delete' ? typeof e.deleted_at !== 'string' : e.deleted_at !== null)
      || item.object.crypto_version !== 1 || item.object.aad_version !== 1
      || item.object.nonce.length !== 24 || item.object.ciphertext.length < 16) fail()
    try { parseSyncTimestamp(e.updated_at); if (e.deleted_at !== null) parseSyncTimestamp(e.deleted_at) } catch { fail() }
    total += item.object.ciphertext.length
    if (total > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES) fail()
    ids.add(e.event_id)
    return { event: e, object: { crypto_version: 1, aad_version: 1,
      nonce: encodeBase64Url(item.object.nonce), ciphertext: encodeBase64Url(item.object.ciphertext) } }
  })
  const body = JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3, device_id: deviceId, items: wire })
  if (new TextEncoder().encode(body).length > MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES) fail()
  return body
}

export { parsePush as parseV3PushResponse }

function parsePush(value: unknown, expected: readonly string[]): V3PushResponse {
  if (!exact(value, ['protocol_version', 'encrypted_sync_version', 'results', 'current_cursor'])) fail()
  const result = value as V3PushResponse
  if (result.protocol_version !== 3 || result.encrypted_sync_version !== 3 || !safe(result.current_cursor, 0)
    || !Array.isArray(result.results) || result.results.length !== expected.length) fail()
  const remaining = new Set(expected)
  for (const receipt of result.results) {
    if (!exact(receipt, ['event_id', 'server_sequence', 'duplicate']) || !remaining.delete(receipt.event_id)
      || !safe(receipt.server_sequence, 1) || receipt.server_sequence > result.current_cursor
      || typeof receipt.duplicate !== 'boolean') fail()
  }
  return result
}

export function encodeV3ProgressPush(deviceId:string,items:readonly V3MetadataPushItem[]):string{return encodeV3ProjectPush(deviceId,items,'progress')}
export function encodeV3GamePush(deviceId:string,items:readonly V3MetadataPushItem[]):string{return encodeV3ProjectPush(deviceId,items,'project_game')}
export interface AccountPushItem { event: Omit<AccountDescriptor,'device_id'|'server_sequence'>; object: AccountObjectEnvelope }
export function encodeAccountPush(deviceId:string,items:readonly AccountPushItem[]):string {
  if (!UUID.test(deviceId)||!items.length||items.length>100) fail()
  const wire=items.map(({event:e,object:o})=>{
    if (!exact(e,['event_id','canonical_user_id','scope','entity_id','entity_type','operation','revision','updated_at','deleted_at']) || !UUID.test(e.event_id)||!UUID.test(e.canonical_user_id)||e.scope!=='account'||!ACCOUNT_ENTITY_TYPES.includes(e.entity_type)||!safe(e.revision,1)||!['upsert','delete'].includes(e.operation)||o.crypto_version!==2||o.aad_version!==2||o.nonce.length!==24||o.ciphertext.length<16||o.ciphertext.length>MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES) fail()
    if(e.entity_type==='account_game'&&(e.entity_id!==`game:${e.event_id}`||e.operation!=='upsert'||e.deleted_at!==null))fail()
    encodeAccountTuple({userId:e.canonical_user_id,scope:'account',entityId:e.entity_id,entityType:e.entity_type})
    parseSyncTimestamp(e.updated_at); if(e.operation==='delete'){if(e.deleted_at===null)fail();parseSyncTimestamp(e.deleted_at!)}else if(e.deleted_at!==null)fail()
    return {event:e,object:{crypto_version:2,aad_version:2,nonce:encodeBase64Url(o.nonce),ciphertext:encodeBase64Url(o.ciphertext)}}
  })
  if (new Set(items.map(i=>i.event.event_id)).size!==items.length||items.reduce((n,i)=>n+i.object.ciphertext.length,0)>MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES) fail()
  const body=JSON.stringify({protocol_version:3,encrypted_sync_version:3,device_id:deviceId,items:wire});if(new TextEncoder().encode(body).length>MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES)fail();return body
}
async function requestV3Pull(token: string, deviceId: string, since: number, limit: number, signal?: AbortSignal): Promise<unknown> {
  if (!UUID.test(deviceId) || !safe(since, 0) || !safe(limit, 1) || limit > 500) fail()
  const query = new URLSearchParams({ device_id: deviceId, since: String(since), limit: String(limit), protocol_version: '3', encrypted_sync_version: '3' })
  return apiRequest<unknown>(`/api/v3/sync/encrypted/pull?${query}`, { headers: headers(token), signal, maxResponseBytes: MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES })
}

export const encryptedSyncV3Api = {
  compressionReaderCapabilities: (token: string, deviceId: string): Promise<void> => apiRequest('/api/v3/sync/encrypted/compression-reader-capabilities', {method:'PUT', headers:headers(token), body:{device_id:deviceId, compression_id1:true}}),
  compressionWriter: (token: string, deviceId: string): Promise<{ready:boolean;missing_devices:number}> => apiRequest<unknown>('/api/v3/sync/encrypted/compression-writer', {method:'POST', headers:headers(token), body:{device_id:deviceId}}).then(value => {
    if (!exact(value,['ready','missing_devices'])) fail()
    const gate=value as {ready:boolean;missing_devices:number}
    if (typeof gate.ready!=='boolean'||!safe(gate.missing_devices,0)||gate.ready&&gate.missing_devices!==0) fail()
    return gate
  }),
  gameReaderCapabilities:(token:string,deviceId:string):Promise<void>=>{
    if(!UUID.test(deviceId))fail()
    return apiRequest('/api/v3/sync/encrypted/game-reader-capabilities',{method:'PUT',headers:headers(token),body:{device_id:deviceId,project:{frame_version:1,codec_id:12,codec_version:1,reader_version:1,compression_zero:true},account:{frame_version:1,codec_id:13,codec_version:2,reader_version:2,compression_zero:true}}})
  },
  gameReaderGate:(token:string):Promise<{ready:boolean;missing_devices:number}>=>apiRequest<unknown>('/api/v3/sync/encrypted/game-reader-capabilities',{headers:headers(token)}).then(value=>{
    if(!exact(value,['ready','missing_devices']))fail()
    const result=value as {ready:boolean;missing_devices:number}
    if(typeof result.ready!=='boolean'||!safe(result.missing_devices,0))fail()
    return result
  }),
  pushGame:(token:string,deviceId:string,items:readonly V3MetadataPushItem[]):Promise<V3PushResponse>=>apiRequest<unknown>('/api/v3/sync/encrypted/push',{method:'POST',headers:headers(token),rawBody:encodeV3GamePush(deviceId,items)}).then(value=>parsePush(value,items.map(i=>i.event.event_id))),
  progressReaderCapabilities:(token:string,deviceId:string,support:{frame_version:1;codec_id:11;codec_version:1;reader_version:1;compression_zero:true}):Promise<void>=>{
    if(!UUID.test(deviceId))fail()
    return apiRequest('/api/v3/sync/encrypted/progress-reader-capabilities',{method:'PUT',headers:headers(token),body:{device_id:deviceId,...support}})
  },
  progressReaderGate:(token:string):Promise<{ready:boolean;missing_devices:number}>=>apiRequest<unknown>('/api/v3/sync/encrypted/progress-reader-capabilities',{headers:headers(token)}).then(value=>{
    if(!exact(value,['ready','missing_devices']))fail()
    const result=value as {ready:boolean;missing_devices:number}
    if(typeof result.ready!=='boolean'||!safe(result.missing_devices,0))fail()
    return result
  }),
  documentReaderCapabilities:(token:string,deviceId:string,support:{frame_version:1;codec_id:10;codec_version:1;reader_version:1;compression_zero:true}):Promise<void>=>{
    if(!UUID.test(deviceId))fail()
    return apiRequest('/api/v3/sync/encrypted/document-reader-capabilities',{method:'PUT',headers:headers(token),body:{device_id:deviceId,...support}})
  },
  documentReaderGate:(token:string):Promise<{ready:boolean;missing_devices:number}>=>apiRequest<unknown>('/api/v3/sync/encrypted/document-reader-capabilities',{headers:headers(token)}).then(value=>{
    if(!exact(value,['ready','missing_devices']))fail()
    const result=value as {ready:boolean;missing_devices:number}
    if(typeof result.ready!=='boolean'||!safe(result.missing_devices,0))fail()
    return result
  }),
  mapReaderCapabilities:(token:string,deviceId:string,support:{frame_version:1;codec_id:9;codec_version:1;reader_version:1;compression_zero:true}):Promise<void>=>{
    if(!UUID.test(deviceId))fail()
    return apiRequest('/api/v3/sync/encrypted/map-reader-capabilities',{method:'PUT',headers:headers(token),body:{device_id:deviceId,...support}})
  },
  mapReaderGate:(token:string):Promise<{ready:boolean;missing_devices:number}>=>apiRequest<unknown>('/api/v3/sync/encrypted/map-reader-capabilities',{headers:headers(token)}).then(value=>{
    if(!exact(value,['ready','missing_devices']))fail()
    const result=value as {ready:boolean;missing_devices:number}
    if(typeof result.ready!=='boolean'||!safe(result.missing_devices,0))fail()
    return result
  }),
  pushMaps:(token:string,deviceId:string,items:readonly V3MetadataPushItem[]):Promise<V3PushResponse>=>apiRequest<unknown>('/api/v3/sync/encrypted/push',{method:'POST',headers:headers(token),rawBody:encodeV3MapPush(deviceId,items)}).then(value=>parsePush(value,items.map(i=>i.event.event_id))),
  pushProgress:(token:string,deviceId:string,items:readonly V3MetadataPushItem[]):Promise<V3PushResponse>=>apiRequest<unknown>('/api/v3/sync/encrypted/push',{method:'POST',headers:headers(token),rawBody:encodeV3ProgressPush(deviceId,items)}).then(value=>parsePush(value,items.map(i=>i.event.event_id))),
  pushDocuments:(token:string,deviceId:string,items:readonly V3MetadataPushItem[]):Promise<V3PushResponse>=>apiRequest<unknown>('/api/v3/sync/encrypted/push',{method:'POST',headers:headers(token),rawBody:encodeV3DocumentPush(deviceId,items)}).then(value=>parsePush(value,items.map(i=>i.event.event_id))),
  noteReaderCapabilities:(token:string,deviceId:string,support:{reader_transport_version:3;frame_version:1;codec8_version:1;compression_zero:true;ordinary_reader_version:1;resolution_reader_version:2}):Promise<void>=>{
    if(!UUID.test(deviceId))fail()
    return apiRequest('/api/v3/sync/encrypted/note-reader-capabilities',{method:'PUT',headers:headers(token),body:{device_id:deviceId,...support}})
  },
  noteReaderGate:(token:string):Promise<{ready:boolean;missing_devices:number}>=>apiRequest<unknown>('/api/v3/sync/encrypted/note-reader-capabilities',{headers:headers(token)}).then(value=>{
    if(!exact(value,['ready','missing_devices']))fail()
    const result=value as {ready:boolean;missing_devices:number}
    if(typeof result.ready!=='boolean'||!safe(result.missing_devices,0))fail()
    return result
  }),
  pushAccount:(token:string,deviceId:string,items:readonly AccountPushItem[]):Promise<V3PushResponse> => apiRequest<unknown>('/api/v3/sync/encrypted/account/push',{method:'POST',headers:headers(token),rawBody:encodeAccountPush(deviceId,items)}).then(value=>parsePush(value,items.map(i=>i.event.event_id))),
  readerReady: (token: string, deviceId: string): Promise<void> => {
    if (!UUID.test(deviceId)) fail()
    return apiRequest<void>('/api/v3/sync/encrypted/reader-ready', { method: 'POST', headers: headers(token), body: { device_id: deviceId, reader_transport_version: 3 } })
  },
  cutover: (token: string, epoch: number): Promise<{ writer_transport_version: 3; cutover_epoch: number }> => {
    if (!safe(epoch, 0)) fail()
    return apiRequest<unknown>('/api/v3/sync/encrypted/cutover', { method: 'POST', headers: headers(token), body: { expected_cutover_epoch: epoch } }).then(value => {
      if (!exact(value, ['writer_transport_version', 'cutover_epoch'])) fail()
      const result = value as { writer_transport_version: 3; cutover_epoch: number }
      if (result.writer_transport_version !== 3 || !safe(result.cutover_epoch, epoch + 1)) fail()
      return result
    })
  },
  coverReaderCapabilities: (token:string,deviceId:string,readerVersion:0|2=2):Promise<void> => apiRequest('/api/v3/sync/encrypted/cover-reader-capabilities', {method:'PUT',headers:headers(token),body:{device_id:deviceId,frame_version:1,codec_id:1,metadata_codec_version:2,cover_crypto_version:1,cover_aad_version:1,compression_zero:true,reader_version:readerVersion}}),
  coverReaderGate: (token:string):Promise<{ready:boolean;missing_devices:number}> => apiRequest<unknown>('/api/v3/sync/encrypted/cover-reader-capabilities',{headers:headers(token)}).then(value=>{
    if(!exact(value,['ready','missing_devices']))fail()
    const gate=value as {ready:boolean;missing_devices:number}
    if(typeof gate.ready!=='boolean'||!safe(gate.missing_devices,0))fail()
    return gate
  }),
  pushCoverMetadata: (token:string,deviceId:string,items:readonly V3MetadataPushItem[]):Promise<V3PushResponse> =>
    apiRequest<unknown>('/api/v3/sync/encrypted/cover-metadata/push',{method:'POST',headers:headers(token),rawBody:encodeV3MetadataPush(deviceId,items)}).then(value=>parsePush(value,items.map(item=>item.event.event_id))),
  pushMetadata: (token: string, deviceId: string, items: readonly V3MetadataPushItem[], signal?: AbortSignal): Promise<V3PushResponse> =>
    apiRequest<unknown>('/api/v3/sync/encrypted/push', { method: 'POST', headers: headers(token), rawBody: encodeV3MetadataPush(deviceId, items), signal })
      .then(value => parsePush(value, items.map(item => item.event.event_id))),
  async pull(token: string, deviceId: string, since: number, limit = 200, signal?: AbortSignal): Promise<V3PullResponse> {
    return parseV3Pull(await requestV3Pull(token, deviceId, since, limit, signal), since, limit)
  },
  /** Accepted original wire values for durable staging; no crypto normalization or apply evidence. */
  async pullRaw(token: string, deviceId: string, since: number, limit = 100, signal?: AbortSignal): Promise<string> {
    const value = await requestV3Pull(token, deviceId, since, limit, signal)
    const parsed = parseV3Pull(value, since, limit)
    try { return JSON.stringify(value) }
    finally { for (const item of parsed.items) { item.object.nonce.fill(0); item.object.ciphertext.fill(0) } }
  },
  /** Account-scoped ciphertext only; this is neither a device capability nor an ACK. */
  async recoveryRaw(token: string, since: number, limit: number, cursor: number, epoch: number, signal?: AbortSignal): Promise<string> {
    if (!safe(since, 0) || !safe(limit, 1) || limit > 100 || !safe(cursor, since) || !safe(epoch, 0)) fail()
    const query = new URLSearchParams({ since: String(since), limit: String(limit), expected_cursor: String(cursor), expected_epoch: String(epoch) })
    let value: unknown
    for (let attempt = 0; ; attempt++) {
      try {
        value = await apiRequest<unknown>(`/api/v3/recovery/encrypted/history?${query}`, { headers: headers(token), signal, maxResponseBytes: MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES })
        break
      } catch (error) {
        if (!(error instanceof ApiError) || error.status !== 429 || attempt >= 5) throw error
        // Recovery GET's existing budget returns Retry-After: 1. Keep the
        // same pinned snapshot and respect its budget; cancellation still wins.
        await new Promise<void>((resolve, reject) => {
          const abort = () => { clearTimeout(timer); signal?.removeEventListener('abort', abort); reject(new DOMException('Aborted', 'AbortError')) }
          const timer = setTimeout(() => { signal?.removeEventListener('abort', abort); resolve() }, 1000)
          if (signal?.aborted) abort()
          else signal?.addEventListener('abort', abort, { once: true })
        })
      }
    }
    if (!exact(value, ['delivery_version', 'current_cursor', 'cutover_epoch', 'writer_transport_version', 'items', 'next_cursor', 'has_more'])) fail()
    const outer = value as {delivery_version:number;current_cursor:number;cutover_epoch:number;writer_transport_version:number;items:unknown[];next_cursor:number;has_more:boolean}
    if (outer.delivery_version !== 1 || outer.current_cursor !== cursor || outer.cutover_epoch !== epoch || outer.writer_transport_version !== 3) fail()
    const wire = { protocol_version: 3, encrypted_sync_version: 3, items: outer.items, next_cursor: outer.next_cursor, has_more: outer.has_more }
    const parsed = parseV3Pull(wire, since, limit)
    try {
      if (parsed.items.some((item, index) => item.event.server_sequence !== since + index + 1)
        || parsed.next_cursor > cursor || parsed.has_more !== (parsed.next_cursor < cursor)) fail()
      return JSON.stringify(wire)
    } finally { for (const item of parsed.items) { item.object.nonce.fill(0); item.object.ciphertext.fill(0) } }
  },
  ack: (token: string, deviceId: string, cursor: number): Promise<void> => {
    if (!UUID.test(deviceId) || !safe(cursor, 0)) fail()
    return apiRequest<void>('/api/v3/sync/encrypted/ack', { method: 'POST', headers: headers(token), body: { protocol_version: 3, encrypted_sync_version: 3, device_id: deviceId, cursor } })
  },
}
