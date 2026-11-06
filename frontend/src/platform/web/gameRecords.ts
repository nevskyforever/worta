import { canonical } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { validateBrowserInboxRecord, type BrowserReceivedInboxRecord, type BrowserSyncScope } from './syncDurability'
import { decodeBase64Url } from '@/api/base64url'
export interface GameLedger {
 account_id:string;device_id:string;project_id:string;entity_id:string;event_id:string;server_sequence:number;outcome:'applied';wire_item:string;previous_projection?:GameProjection
}
export interface GameProjection {
 account_id:string;device_id:string;project_id:string;entity_id:string;head_event_id:string;envelope:{crypto_version:1|2;aad_version:1|2;nonce:string;ciphertext:string}
}
export interface GameProof {record_version:1;owner_kind:'game';outcome:'applied';ledger_digest:string;projection_digest:string}
export interface GameInbox extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'>{record_version:14;state:'game_proven';game_proof:GameProof}
function fail():never{throw new BrowserPersistenceError('corrupt_record')}
export async function gameDigest(value:unknown){const bytes=new TextEncoder().encode(canonical(value));try{return Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',bytes)),x=>x.toString(16).padStart(2,'0')).join('')}finally{bytes.fill(0)}}
export function validateGameInbox(value:unknown,s:BrowserSyncScope):GameInbox{
 const r=value as GameInbox
 if(!r||Object.keys(r).sort().join(',')!=='account_id,device_id,event_id,game_proof,protocol_version,record_version,server_sequence,state,wire_item'||r.record_version!==14||r.state!=='game_proven')fail()
 const {game_proof:p,...base}=r;validateBrowserInboxRecord({...base,record_version:1,state:'received'},s)
 const e=JSON.parse(r.wire_item).event
 if(r.protocol_version!==3||!['project_game','account_game'].includes(e.entity_type)||(e.entity_type==='account_game'?e.scope!=='account'||e.canonical_user_id!==s.canonicalUserId||e.operation!=='upsert':e.scope!==undefined||e.operation!=='event')||!Number.isSafeInteger(e.revision)||e.revision<1||e.device_id===s.deviceId||e.deleted_at!==null
  ||!p||Object.keys(p).sort().join(',')!=='ledger_digest,outcome,owner_kind,projection_digest,record_version'||p.record_version!==1||p.owner_kind!=='game'||p.outcome!=='applied'||![p.ledger_digest,p.projection_digest].every(x=>/^[a-f0-9]{64}$/.test(x)))fail()
 return structuredClone(r)
}
export function joinGameRecords(s:BrowserSyncScope,r:GameInbox,ledgers:unknown[],projections:unknown[]){
 const e=JSON.parse(r.wire_item).event,l=ledgers.find(x=>(x as GameLedger).event_id===r.event_id) as GameLedger|undefined
 const archived=ledgers.flatMap(x=>(x as GameLedger).previous_projection?[(x as GameLedger).previous_projection!]:[])
 const matching=[...projections,...archived].filter(x=>(x as GameProjection).head_event_id===r.event_id) as GameProjection[]
 const p=matching[0],keys='account_id,device_id,entity_id,event_id,outcome,project_id,server_sequence,wire_item'
 if(!l||!p||matching.length!==1||Object.keys(l).sort().join(',')!==(e.revision===1?keys:'account_id,device_id,entity_id,event_id,outcome,previous_projection,project_id,server_sequence,wire_item')
 ||Object.keys(p).sort().join(',')!=='account_id,device_id,entity_id,envelope,head_event_id,project_id'
 ||l.account_id!==s.accountId||l.device_id!==s.deviceId||l.project_id!==(e.scope==='account'?'account':e.project_id)||l.server_sequence!==r.server_sequence||l.outcome!=='applied'||l.wire_item!==r.wire_item
 ||p.account_id!==s.accountId||p.device_id!==s.deviceId||p.project_id!==l.project_id||p.entity_id!==l.entity_id)fail()
 if(e.revision>1){
  const previous=l.previous_projection,parent=ledgers.find(x=>(x as GameLedger).event_id===previous?.head_event_id) as GameLedger|undefined
  if(!previous||!parent||previous.account_id!==s.accountId||previous.device_id!==s.deviceId||previous.project_id!==l.project_id||previous.entity_id!==l.entity_id
   ||parent.project_id!==l.project_id||parent.entity_id!==l.entity_id||parent.server_sequence>=r.server_sequence||JSON.parse(parent.wire_item).event.revision!==e.revision-1)fail()
 }
 const env=p.envelope,version=e.scope==='account'?2:1
 if(Object.keys(env).sort().join(',')!=='aad_version,ciphertext,crypto_version,nonce'||env.crypto_version!==version||env.aad_version!==version||decodeBase64Url(env.nonce).length!==24||decodeBase64Url(env.ciphertext).length<16)fail()
 return {ledger:l,projection:p}
}
