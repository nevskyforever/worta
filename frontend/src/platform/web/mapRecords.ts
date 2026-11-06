import { canonical } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { validateBrowserInboxRecord, type BrowserReceivedInboxRecord, type BrowserSyncScope } from './syncDurability'
import { validateLocalEnvelope, type LocalEnvelope } from './localAuthorityRecords'
export interface MapLedger {
 account_id:string;device_id:string;project_id:string;entity_id:string;event_id:string;server_sequence:number;outcome:'applied';wire_item:string;previous_projection?:MapProjection
}
export interface MapProjection {
 account_id:string;device_id:string;project_id:string;entity_id:string;head_event_id:string;envelope:LocalEnvelope
}
export interface MapProof {record_version:1;owner_kind:'map';outcome:'applied';ledger_digest:string;projection_digest:string}
export interface MapInbox extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'>{record_version:12;state:'map_proven';map_proof:MapProof}
function fail():never{throw new BrowserPersistenceError('corrupt_record')}
export async function mapDigest(value:unknown){const bytes=new TextEncoder().encode(canonical(value));try{return Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',bytes)),x=>x.toString(16).padStart(2,'0')).join('')}finally{bytes.fill(0)}}
export function validateMapInbox(value:unknown,s:BrowserSyncScope):MapInbox{
 const r=value as MapInbox
 if(!r||Object.keys(r).sort().join(',')!=='account_id,device_id,event_id,map_proof,protocol_version,record_version,server_sequence,state,wire_item'||r.record_version!==12||r.state!=='map_proven')fail()
 const {map_proof:p,...base}=r;validateBrowserInboxRecord({...base,record_version:1,state:'received'},s)
 const e=JSON.parse(r.wire_item).event
 if(r.protocol_version!==3||e.entity_type!=='map'||e.operation!=='event'||!Number.isSafeInteger(e.revision)||e.revision<1||e.device_id===s.deviceId||e.deleted_at!==null
  ||!p||Object.keys(p).sort().join(',')!=='ledger_digest,outcome,owner_kind,projection_digest,record_version'||p.record_version!==1||p.owner_kind!=='map'||p.outcome!=='applied'||![p.ledger_digest,p.projection_digest].every(x=>/^[a-f0-9]{64}$/.test(x)))fail()
 return structuredClone(r)
}
export function joinMapRecords(s:BrowserSyncScope,r:MapInbox,ledgers:unknown[],projections:unknown[]){
 const e=JSON.parse(r.wire_item).event,l=ledgers.find(x=>(x as MapLedger).event_id===r.event_id) as MapLedger|undefined
 const archived=ledgers.flatMap(x=>(x as MapLedger).previous_projection?[(x as MapLedger).previous_projection!]:[])
 const matching=[...projections,...archived].filter(x=>(x as MapProjection).head_event_id===r.event_id) as MapProjection[]
 const p=matching[0],keys='account_id,device_id,entity_id,event_id,outcome,project_id,server_sequence,wire_item'
 if(!l||!p||matching.length!==1||Object.keys(l).sort().join(',')!==(e.revision===1?keys:'account_id,device_id,entity_id,event_id,outcome,previous_projection,project_id,server_sequence,wire_item')
 ||Object.keys(p).sort().join(',')!=='account_id,device_id,entity_id,envelope,head_event_id,project_id'
 ||l.account_id!==s.accountId||l.device_id!==s.deviceId||l.project_id!==e.project_id||l.entity_id!==e.entity_id||l.server_sequence!==r.server_sequence||l.outcome!=='applied'||l.wire_item!==r.wire_item
 ||p.account_id!==s.accountId||p.device_id!==s.deviceId||p.project_id!==l.project_id||p.entity_id!==l.entity_id)fail()
 if(e.revision>1){
  const previous=l.previous_projection,parent=ledgers.find(x=>(x as MapLedger).event_id===previous?.head_event_id) as MapLedger|undefined
  if(!previous||!parent||previous.account_id!==s.accountId||previous.device_id!==s.deviceId||previous.project_id!==e.project_id||previous.entity_id!==e.entity_id
   ||parent.project_id!==e.project_id||parent.entity_id!==e.entity_id||parent.server_sequence>=r.server_sequence||JSON.parse(parent.wire_item).event.revision!==e.revision-1)fail()
 }
 validateLocalEnvelope(p.envelope);return {ledger:l,projection:p}
}
