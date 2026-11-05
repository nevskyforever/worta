import { canonical } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { validateBrowserInboxRecord, type BrowserReceivedInboxRecord, type BrowserSyncScope } from './syncDurability'
import { validateLocalEnvelope, type LocalEnvelope } from './localAuthorityRecords'
export interface DocumentLedger {
 account_id:string;device_id:string;project_id:string;entity_id:string;event_id:string;server_sequence:number;outcome:'applied';wire_item:string
}
export interface DocumentProjection {
 account_id:string;device_id:string;project_id:string;entity_id:string;head_event_id:string;envelope:LocalEnvelope
}
export interface DocumentProof {record_version:1;owner_kind:'document';outcome:'applied';ledger_digest:string;projection_digest:string}
export interface DocumentInbox extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'>{record_version:11;state:'document_proven';document_proof:DocumentProof}
const fail=():never=>{throw new BrowserPersistenceError('corrupt_record')}
export async function documentDigest(value:unknown){const bytes=new TextEncoder().encode(canonical(value));try{return Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',bytes)),x=>x.toString(16).padStart(2,'0')).join('')}finally{bytes.fill(0)}}
export function validateDocumentInbox(value:unknown,s:BrowserSyncScope):DocumentInbox{
 const r=value as DocumentInbox
 if(!r||Object.keys(r).sort().join(',')!=='account_id,device_id,document_proof,event_id,protocol_version,record_version,server_sequence,state,wire_item'||r.record_version!==11||r.state!=='document_proven')fail()
 const {document_proof:p,...base}=r;validateBrowserInboxRecord({...base,record_version:1,state:'received'},s)
 const e=JSON.parse(r.wire_item).event
 if(r.protocol_version!==3||e.entity_type!=='document'||e.operation!=='event'||e.revision!==1||e.device_id===s.deviceId||e.deleted_at!==null
  ||!p||Object.keys(p).sort().join(',')!=='ledger_digest,outcome,owner_kind,projection_digest,record_version'||p.record_version!==1||p.owner_kind!=='document'||p.outcome!=='applied'||![p.ledger_digest,p.projection_digest].every(x=>/^[a-f0-9]{64}$/.test(x)))fail()
 return structuredClone(r)
}
export function joinDocumentRecords(s:BrowserSyncScope,r:DocumentInbox,ledgers:unknown[],projections:unknown[]){
 const e=JSON.parse(r.wire_item).event,l=ledgers.find(x=>(x as DocumentLedger).event_id===r.event_id) as DocumentLedger|undefined,p=projections.find(x=>(x as DocumentProjection).head_event_id===r.event_id) as DocumentProjection|undefined
 if(!l||!p||Object.keys(l).sort().join(',')!=='account_id,device_id,entity_id,event_id,outcome,project_id,server_sequence,wire_item'||Object.keys(p).sort().join(',')!=='account_id,device_id,entity_id,envelope,head_event_id,project_id'
 ||l.account_id!==s.accountId||l.device_id!==s.deviceId||l.project_id!==e.project_id||l.entity_id!==e.entity_id||l.server_sequence!==r.server_sequence||l.outcome!=='applied'||l.wire_item!==r.wire_item
 ||p.account_id!==s.accountId||p.device_id!==s.deviceId||p.project_id!==l.project_id||p.entity_id!==l.entity_id)fail()
 validateLocalEnvelope(p!.envelope);return {ledger:l!,projection:p!}
}
