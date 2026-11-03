import {decryptObjectBytes,type AccountMasterKey} from '@/crypto'
import {decodeBase64Url} from '@/api/base64url'
import {unframeContentNote} from '@/cloud/contentNoteCodec'
import {immutableWireIdentity,validateBrowserUploadPair,type BrowserSyncScope,type BrowserInboxRecord} from './syncDurability'
import {BrowserPersistenceError} from './persistence'
/** Existing sealed writer output + actual upload receipt. Neither a device ID nor
 * a pulled event alone proves own publication. No proof/receipt is manufactured. */
export function requireOwnResolutionPublication(s:BrowserSyncScope,row:Pick<BrowserInboxRecord,'event_id'|'server_sequence'|'protocol_version'|'wire_item'>,v:{out:readonly unknown[];receipts:readonly unknown[]}){
 const bad=():never=>{throw new BrowserPersistenceError('self_echo_unavailable')}
 const o=v.out.find(o=>(o as {event_id:string}).event_id===row.event_id),r=v.receipts.find(r=>(r as {event_id:string}).event_id===row.event_id)
 if(!o||!r||(r as {record_version:number}).record_version!==1)bad()
 const out=validateBrowserUploadPair(o,r,s),wire=JSON.parse(row.wire_item),{device_id,server_sequence,...event}=wire.event
 if(device_id!==s.deviceId||server_sequence!==row.server_sequence||out.state!=='accepted'||out.protocol_version!==row.protocol_version||(r as {server_sequence:number}).server_sequence!==server_sequence
  ||immutableWireIdentity(JSON.parse(out.raw_body).items[0])!==immutableWireIdentity({event,object:wire.object}))bad()
 return out
}

/** AEAD route check for a future own reservation during historical peer replay.
 * This authenticates bytes only; the existing planner still owns all application. */
export async function authenticateOwnResolutionReservation(s:BrowserSyncScope,row:Pick<BrowserInboxRecord,'event_id'|'server_sequence'|'protocol_version'|'wire_item'>,v:{out:readonly unknown[];receipts:readonly unknown[]},amk:AccountMasterKey){
 requireOwnResolutionPublication(s,row,v)
 const wire=JSON.parse(row.wire_item),e=wire.event,nonce=decodeBase64Url(wire.object.nonce),ciphertext=decodeBase64Url(wire.object.ciphertext);let bytes:Uint8Array|undefined
 try{bytes=await decryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:e.project_id,entityId:e.entity_id,entityType:'note'},{...wire.object,nonce,ciphertext});const c=unframeContentNote(bytes),r=c.event
  if(row.protocol_version!==3||e.operation!=='event'||c.device_id!==s.deviceId||c.account_id!==s.canonicalUserId||r.version!==2||r.header.event_id!==row.event_id||r.header.project_id!==e.project_id||r.header.entity_id!==e.entity_id||r.header.revision!==e.revision||r.header.updated_at!==e.updated_at)throw new BrowserPersistenceError('self_echo_unavailable')
 }finally{bytes?.fill(0);nonce.fill(0);ciphertext.fill(0)}
}
