import { decodeBase64Url, encodeBase64Url } from './base64url'
import { ApiError, apiRequest } from './client'
import { MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES, MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES, MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES } from './encryptedSync'
import { parseSyncTimestamp } from '@/cloud/syncTimestamp'
import type { ObjectCryptoEnvelope } from '@/crypto'

export const V2_SYNC_PROTOCOL_VERSION = 2 as const
const MAX_SAFE = Number.MAX_SAFE_INTEGER
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
export interface V2Capabilities { supported_transport_version: 2, writer_transport_version: 1 | 2 | 3, cutover_epoch: number }
export interface V2CutoverResponse { supported_transport_version: 2, writer_transport_version: 2, cutover_epoch: number }
interface V2PushEventBase { event_id:string, project_id:string, entity_id:string, entity_type:'note', revision:number, updated_at:string }
type V2PushEvent =
  | (V2PushEventBase & { operation:'upsert' | 'resolution', deleted_at:null })
  | (V2PushEventBase & { operation:'delete', deleted_at:string })
export interface V2PushItem { event: V2PushEvent, object: { crypto_version:1, aad_version:1, nonce:string, ciphertext:string } }
export interface V2ResolutionPushItem extends V2PushItem { event: V2PushEventBase & { operation:'resolution', deleted_at:null } }
export interface V2PushRequest { protocol_version:2, encrypted_sync_version:2, device_id:string, items: V2PushItem[] }
export interface V2PushResponse { protocol_version:2, encrypted_sync_version:2, results:Array<{event_id:string,server_sequence:number,duplicate:boolean}>, current_cursor:number }
export interface ValidatedV2PushItem { eventId:string, ciphertextBytes:number }
export interface V2PullItem { event: { event_id:string, device_id:string, server_sequence:number, project_id:string, entity_id:string, entity_type:'note', operation:'upsert'|'delete'|'resolution', revision:number, updated_at:string, deleted_at:string|null }, object:ObjectCryptoEnvelope }
export interface V2PullResponse { protocol_version:2, encrypted_sync_version:2, items:V2PullItem[], next_cursor:number, has_more:boolean }
export interface V2AckRequest { protocol_version:2, encrypted_sync_version:2, device_id:string, cursor:number }
const fail = (): never => { throw new TypeError('Invalid encrypted sync v2 payload.') }
export class V2UnsupportedEventError extends TypeError {
  readonly code = 'unsupported_event'
  constructor() { super('Unsupported encrypted sync v2 event.') }
}
export class V2MalformedPullResponseError extends TypeError {
  readonly code = 'malformed_v2_response'
  constructor() { super('Malformed encrypted sync v2 pull response.') }
}
const keys = (v: unknown, k: readonly string[]) => typeof v === 'object' && v !== null && Object.keys(v).length === k.length && k.every(key => Object.prototype.hasOwnProperty.call(v, key))
function uuid(v: unknown): string { if (typeof v !== 'string') throw new TypeError('Invalid encrypted sync v2 UUID.'); if (!UUID.test(v)) throw new TypeError('Invalid encrypted sync v2 UUID.'); return v.toLowerCase() }
const safe = (v: unknown, min: number): number => { if (!Number.isSafeInteger(v)) fail(); const number = v as number; if (number < min || number > MAX_SAFE) fail(); return number }
const boundedText = (v: unknown, maximum: number): v is string => typeof v === 'string' && v.length >= 1 && v.length <= maximum
const headers = (token:string) => new Headers({ Authorization:`Bearer ${token}`, 'Content-Type':'application/json' })

export function parseV2Capabilities(value: unknown): V2Capabilities {
  if (!keys(value,['supported_transport_version','writer_transport_version','cutover_epoch'])) fail()
  const v=value as V2Capabilities
  if (v.supported_transport_version!==2 || ![1,2,3].includes(v.writer_transport_version)) fail()
  safe(v.cutover_epoch,0); return v
}
export function parseV2CutoverResponse(value: unknown): V2CutoverResponse {
  const parsed = parseV2Capabilities(value)
  if (parsed.writer_transport_version !== 2) fail()
  return parsed as V2CutoverResponse
}
export function validateV2PushItem(item: unknown): ValidatedV2PushItem {
  if (!keys(item,['event','object'])) fail()
  const value=item as V2PushItem
  if (!keys(value.event,['event_id','project_id','entity_id','entity_type','operation','revision','updated_at','deleted_at']) || !keys(value.object,['crypto_version','aad_version','nonce','ciphertext'])) fail()
  const e=value.event; const eventId=uuid(e.event_id); if (eventId!==e.event_id || !boundedText(e.project_id,512) || !boundedText(e.entity_id,512) || e.entity_type!=='note' || !['upsert','delete','resolution'].includes(e.operation) || !Number.isSafeInteger(e.revision) || e.revision<(e.operation==='resolution'?2:1) || typeof e.updated_at!=='string' || (e.operation==='delete')!==(e.deleted_at!==null)) fail()
  if (e.deleted_at!==null && typeof e.deleted_at!=='string') fail()
  try { parseSyncTimestamp(e.updated_at); if(e.deleted_at!==null)parseSyncTimestamp(e.deleted_at) } catch { fail() }
  const o=value.object; if (o.crypto_version!==1 || o.aad_version!==1) fail()
  const nonce=decodeBase64Url(o.nonce,{expectedLength:24}); const ciphertext=decodeBase64Url(o.ciphertext,{minimumLength:16,maximumLength:MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES})
  if (encodeBase64Url(nonce)!==o.nonce || encodeBase64Url(ciphertext)!==o.ciphertext) fail()
  return { eventId, ciphertextBytes:ciphertext.byteLength }
}
export function encodeV2Push(request: V2PushRequest): string {
  if (!keys(request,['protocol_version','encrypted_sync_version','device_id','items']) || request.protocol_version!==2 || request.encrypted_sync_version!==2 || !Array.isArray(request.items) || request.items.length<1 || request.items.length>100) fail()
  if(uuid(request.device_id)!==request.device_id)fail(); let total=0; const eventIds=new Set<string>()
  for (const item of request.items) {
    const validated=validateV2PushItem(item); if(eventIds.has(validated.eventId))fail(); eventIds.add(validated.eventId); total+=validated.ciphertextBytes; if(total>MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES) fail()
  }
  const body=JSON.stringify(request); if(new TextEncoder().encode(body).byteLength>MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES) throw new RangeError('Encrypted sync v2 request exceeds wire limit.'); return body
}
export function parseV2PushResponse(value: unknown, expected: readonly string[]): V2PushResponse {
  if(!keys(value,['protocol_version','encrypted_sync_version','results','current_cursor'])) fail(); const r=value as V2PushResponse
  if(r.protocol_version!==2||r.encrypted_sync_version!==2||!Array.isArray(r.results)) fail(); safe(r.current_cursor,0)
  const want=new Set(expected.map(uuid)); if(r.results.length!==want.size) fail(); const seq=new Set<number>()
  for(const row of r.results){if(!keys(row,['event_id','server_sequence','duplicate']))fail();const id=uuid(row.event_id);const n=safe(row.server_sequence,1);if(id!==row.event_id||!want.delete(id)||seq.has(n)||typeof row.duplicate!=='boolean'||r.current_cursor<n)fail();seq.add(n)}
  if(want.size)fail(); return r
}
export function parseV2PullResponse(value: unknown, since:number, limit:number): V2PullResponse {
  if(!Number.isSafeInteger(since)||since<0||!Number.isSafeInteger(limit)||limit<1||limit>500)throw new RangeError('Invalid encrypted sync v2 pagination.')
  if(!keys(value,['protocol_version','encrypted_sync_version','items','next_cursor','has_more']))fail()
  const page=value as V2PullResponse
  if(page.protocol_version!==2||page.encrypted_sync_version!==2||!Array.isArray(page.items)||page.items.length>limit||typeof page.has_more!=='boolean')fail()
  safe(page.next_cursor,0)
  let previous=since, aggregate=0
  const ids=new Set<string>(), sequences=new Set<number>()
  const items=page.items.map((item:unknown):V2PullItem=>{
    if(!keys(item,['event','object']))fail()
    const row=item as V2PullItem
    if(!keys(row.event,['event_id','device_id','server_sequence','project_id','entity_id','entity_type','operation','revision','updated_at','deleted_at']))fail()
    const event=row.event
    if(uuid(event.event_id)!==event.event_id||uuid(event.device_id)!==event.device_id||!boundedText(event.project_id,512)||!boundedText(event.entity_id,512))fail()
    if(event.entity_type!=='note'||!(['upsert','delete','resolution'] as string[]).includes(event.operation)||row.object===null)throw new V2UnsupportedEventError()
    safe(event.revision,event.operation==='resolution'?2:1)
    const sequence=safe(event.server_sequence,1)
    if(sequence<=previous||ids.has(event.event_id)||sequences.has(sequence))fail()
    if(typeof event.updated_at!=='string'||!(event.deleted_at===null||typeof event.deleted_at==='string')||(event.operation==='delete')!==(event.deleted_at!==null))fail()
    try {parseSyncTimestamp(event.updated_at);if(event.deleted_at!==null)parseSyncTimestamp(event.deleted_at)}catch{fail()}
    if(!keys(row.object,['crypto_version','aad_version','nonce','ciphertext']))fail()
    const object=row.object as unknown as {crypto_version:number,aad_version:number,nonce:string,ciphertext:string}
    if(object.crypto_version!==1||object.aad_version!==1)fail()
    const nonce=decodeBase64Url(object.nonce,{expectedLength:24})
    const ciphertext=decodeBase64Url(object.ciphertext,{minimumLength:16,maximumLength:MAX_ENCRYPTED_SYNC_CIPHERTEXT_BYTES})
    if(encodeBase64Url(nonce)!==object.nonce||encodeBase64Url(ciphertext)!==object.ciphertext)fail()
    aggregate+=ciphertext.byteLength;if(aggregate>MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES)fail()
    ids.add(event.event_id);sequences.add(sequence);previous=sequence
    return {event,object:{crypto_version:1,aad_version:1,nonce,ciphertext}}
  })
  if(page.next_cursor!==previous||(!items.length&&page.has_more))fail()
  return {protocol_version:2,encrypted_sync_version:2,items,next_cursor:page.next_cursor,has_more:page.has_more}
}
export function encodeV2Ack(request:V2AckRequest):V2AckRequest {
  if(!keys(request,['protocol_version','encrypted_sync_version','device_id','cursor'])||request.protocol_version!==2||request.encrypted_sync_version!==2||uuid(request.device_id)!==request.device_id)fail()
  safe(request.cursor,0);return request
}
export const encryptedSyncV2Api={
  capabilities:(token:string,signal?:AbortSignal)=>apiRequest<unknown>('/api/v2/sync/encrypted/capabilities',{headers:headers(token),signal}).then(parseV2Capabilities),
  cutover:(token:string,expectedCutoverEpoch:number, initialWebDeviceId?: string, signal?: AbortSignal):Promise<V2CutoverResponse>=>{
    safe(expectedCutoverEpoch,0)
    return apiRequest<unknown>('/api/v2/sync/encrypted/cutover',{method:'POST',headers:headers(token),body:{expected_cutover_epoch:expectedCutoverEpoch, initial_web_device_id:initialWebDeviceId},signal}).then(parseV2CutoverResponse)
  },
  push:(token:string,request:V2PushRequest)=>apiRequest<unknown>('/api/v2/sync/encrypted/push',{method:'POST',headers:headers(token),rawBody:encodeV2Push(request)}).then(value=>parseV2PushResponse(value,request.items.map(i=>i.event.event_id))),
  async pull(token:string,deviceId:string,since:number,limit=200,signal?:AbortSignal):Promise<V2PullResponse>{
    if(!Number.isSafeInteger(since)||since<0||!Number.isSafeInteger(limit)||limit<1||limit>500)throw new RangeError('Invalid encrypted sync v2 pagination.')
    if(uuid(deviceId)!==deviceId)fail()
    const query=new URLSearchParams({device_id:deviceId,since:String(since),limit:String(limit),protocol_version:'2',encrypted_sync_version:'2'})
    let response:unknown
    try { response=await apiRequest<unknown>(`/api/v2/sync/encrypted/pull?${query}`,{headers:headers(token),signal,maxResponseBytes:MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES}) }
    catch(error) { if(error instanceof ApiError&&error.code==='encrypted_sync_event_incomplete')throw new V2UnsupportedEventError();if(error instanceof SyntaxError)throw new V2MalformedPullResponseError();throw error }
    try { return parseV2PullResponse(response,since,limit) }
    catch(error) { if(error instanceof V2UnsupportedEventError)throw error;throw new V2MalformedPullResponseError() }
  },
  async ack(token:string,request:V2AckRequest):Promise<void>{
    const response=await apiRequest<unknown>('/api/v2/sync/encrypted/ack',{method:'POST',headers:headers(token),body:encodeV2Ack(request)})
    if(response!==undefined)fail()
  },
}
