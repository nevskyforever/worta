import {readFutureInboxRecord} from './resolutionRecords'
import {inspectResolutionDag} from './resolutionDag'
import type {AccountMasterKey} from '@/crypto'
import {BrowserPersistenceError} from './persistence'
import {validateNoteHistorySet} from './noteHistoryRecords'
import {validateNoteIntentRecord} from './noteIntentRecords'
import {validateBrowserProjectBinding} from './projectBindings'
import {validateBrowserStoredUploadPair,immutableWireIdentity,type BrowserSyncScope,type BrowserOutboxRecord,type BrowserSyncCheckpoint} from './syncDurability'
import {validateContentReceipt2,joinContentReceipt2,contentAuthorityCorrupt as bad,type BrowserContentCandidate3} from './contentAuthorityRecords'
import {authenticateContentIntent2,authenticateContentCandidate3,authenticateContentReceipt2,authenticateContentSelfEcho2} from './contentAuthorityAuthentication'
import {noteProofDigest} from './legacyNoteApply'
const same=(a:unknown,b:unknown)=>immutableWireIdentity(a)===immutableWireIdentity(b)
export interface ContentAuthorityInventory {history:readonly unknown[];out:readonly unknown[];receipts:readonly unknown[];rows?:readonly unknown[];intents?:readonly unknown[];bindings?:readonly unknown[];localNotes?:readonly unknown[];cursor?:BrowserSyncCheckpoint}
/** Structural dispatch only. These rows do not grant writer, self-echo or ACK authority. */
export function inspectContentAuthorityInventory(s:BrowserSyncScope,v:ContentAuthorityInventory){
 const history=(v.history.some(r=>(r as {record_kind:string}).record_kind==='resolution')?inspectResolutionDag(s,v.history).history.filter((r):r is ReturnType<typeof validateNoteHistorySet>[number]=>r.record_kind==='candidate'||r.record_kind==='group'||r.record_kind==='local_version'||r.record_kind==='head'&&r.record_version===1):validateNoteHistorySet(s,[...v.history])),intents=(v.intents??[]).map(i=>validateNoteIntentRecord(i,s)),rows=(v.rows??[]).map(r=>readFutureInboxRecord(r,s))
 const candidates=history.filter((h):h is BrowserContentCandidate3=>h.record_kind==='candidate'&&h.record_version===3)
 const out=v.out.map(o=>validateBrowserStoredUploadPair(o,v.receipts.find(r=>(r as any)?.event_id===(o as any)?.event_id),s))
 const receipts=v.receipts.filter(r=>(r as any)?.record_version===2).map(r=>validateContentReceipt2(r,s))
 if(new Set(out.map(o=>o.event_id)).size!==out.length||new Set(v.receipts.map(r=>(r as any).event_id)).size!==v.receipts.length||new Set(v.receipts.map(r=>(r as any).server_sequence)).size!==v.receipts.length)bad()
 if(candidates.length||receipts.length||intents.some(i=>i.record_version===2)||rows.some(r=>r.record_version===9)){
  const allocation=[...out,...intents].sort((a,b)=>a.local_ordinal-b.local_ordinal)
  if(new Set(allocation.map(r=>r.event_id)).size!==allocation.length||allocation.some((r,i)=>r.local_ordinal!==i+1)||v.cursor&&v.cursor.next_local_ordinal!==allocation.length+1)bad()
 }

 for(const raw of v.receipts){const o=out.find(o=>o.event_id===(raw as any)?.event_id);if(!o)bad();validateBrowserStoredUploadPair(o,raw,s)}
 if(new Set(intents.map(i=>i.event_id)).size!==intents.length||new Set(intents.map(i=>immutableWireIdentity([i.project_id,i.note_id]))).size!==intents.length)bad()
 for(const c of candidates){
  const o=out.find(o=>o.event_id===c.event_id),r=v.receipts.find(r=>(r as any)?.event_id===c.event_id)
  if(!o||o.protocol_version!==3||o.local_ordinal!==c.intent.local_ordinal||!same(JSON.parse(o.raw_body).items[0],JSON.parse(c.wire_item))||intents.some(i=>i.event_id===c.event_id))bad()
  if(o.state==='accepted'){if((r as any)?.record_version!==2)bad();joinContentReceipt2(s,r,c,o)}else if(r!==undefined)bad()
 }
 for(const r of receipts){const c=candidates.find(c=>c.event_id===r.event_id),o=out.find(o=>o.event_id===r.event_id);if(!c||!o)bad();joinContentReceipt2(s,r,c,o)}
 for(const i of intents)if(i.record_version===2){
  if(out.some(o=>o.event_id===i.event_id)||candidates.some(c=>c.event_id===i.event_id))bad()
  const own=history.filter(h=>h.project_id===i.project_id&&h.note_id===i.note_id),head=v.history.find(h=>(h as any).record_kind==='head'&&(h as any).project_id===i.project_id&&(h as any).note_id===i.note_id&&(h as any).bootstrap_id===i.bootstrap_id) as any, parent=v.history.find(h=>['candidate','resolution'].includes((h as any).record_kind)&&(h as any).event_id===i.parent_event_id&&(h as any).project_id===i.project_id&&(h as any).note_id===i.note_id&&(h as any).bootstrap_id===i.bootstrap_id) as any
  if(own.some(h=>h.record_kind==='group'&&!v.history.some(t=>(t as any).record_kind==='group_transition'&&(t as any).group_id===h.group_id&&(t as any).project_id===h.project_id&&(t as any).note_id===h.note_id&&(t as any).bootstrap_id===h.bootstrap_id))||i.parent_event_id===null&&own.length||i.parent_event_id!==null&&(!head||head.record_kind!=='head'||head.linear_head_event_id!==i.parent_event_id||head.tip_event_ids.length!==1||(!parent||parent.header.revision!==i.revision-1)))bad()
  if(v.localNotes&&!v.localNotes.some(n=>same(n,i.snapshot)))bad()
 }
 if(v.bindings){const bs=v.bindings.map(b=>validateBrowserProjectBinding(b,s));for(const c of [...candidates,...intents.filter(i=>i.record_version===2)])if(!bs.some(b=>b.project_id===c.project_id&&b.bootstrap_id===c.bootstrap_id&&b.mode==='import_remote'&&b.phase==='captured'&&b.remote_state==='active'))bad()}
 for(const row of rows)if(row.state==='content_note_self_echo_proven'){
  const p=row.content_note_self_echo_proof,c=candidates.find(c=>c.event_id===row.event_id),r=receipts.find(r=>r.event_id===row.event_id)
  if(!c||!r||p.candidate_digest!==c.record_digest||p.wire_digest!==c.wire_digest||p.receipt_digest!==r.receipt_digest||p.bootstrap_id!==c.bootstrap_id||r.server_sequence!==row.server_sequence||!same(p.header,c.header))bad()
  const w=JSON.parse(row.wire_item),{device_id:_device,server_sequence:_seq,...event}=w.event
  if(!same({event,object:w.object},JSON.parse(c.wire_item)))bad()
 }
 return{history,intents,rows,candidates,out,receipts}
}
/** Read-only AEAD gate; no producer and no sequence qualification is exposed. */
export async function authenticateContentAuthorityInventory(s:BrowserSyncScope,v:ContentAuthorityInventory,amk:AccountMasterKey,current:()=>boolean):Promise<void>{
 if(!current())throw new BrowserPersistenceError('closed')
 const parsed=inspectContentAuthorityInventory(s,v)
 for(const i of parsed.intents)if(i.record_version===2)await authenticateContentIntent2(s,i,amk,current)
 for(const c of parsed.candidates){const o=parsed.out.find(o=>o.event_id===c.event_id)!;await authenticateContentCandidate3(s,c,amk,current);if(await noteProofDigest(o.raw_body)!==c.raw_body_digest)bad()}
 for(const r of parsed.receipts)await authenticateContentReceipt2(s,r,parsed.candidates.find(c=>c.event_id===r.event_id),parsed.out.find(o=>o.event_id===r.event_id) as BrowserOutboxRecord,amk,current)
 for(const row of parsed.rows)if(row.state==='content_note_self_echo_proven')await authenticateContentSelfEcho2(s,row,parsed.candidates.find(c=>c.event_id===row.event_id)!,parsed.receipts.find(r=>r.event_id===row.event_id)!,parsed.out.find(o=>o.event_id===row.event_id)!,amk,current)
 if(!current())throw new BrowserPersistenceError('closed')
}

export const CONTENT_AUTHORITY_STORES=['legacyNoteHistory','outbox','receipts','inbox','legacyNoteIntents','projectBindings','localNotes']
export function readContentAuthorityInventory(tx:IDBTransaction,s:BrowserSyncScope,done:(v:ContentAuthorityInventory)=>void,abort:(e:unknown)=>void):void{
 const requests=CONTENT_AUTHORITY_STORES.map(name=>tx.objectStore(name).getAll(IDBKeyRange.bound([s.accountId],[s.accountId,[]])));let n=0
 for(const r of requests)r.onsuccess=()=>{if(++n!==requests.length)return;try{done({history:requests[0]!.result,out:requests[1]!.result,receipts:requests[2]!.result,rows:requests[3]!.result,intents:requests[4]!.result,bindings:requests[5]!.result,localNotes:requests[6]!.result})}catch(e){abort(e)}}
}
/** Keep old unrelated Legacy progress legal, while CAS-binding every new-format dependency. */
export function contentAuthorityAnchor(s:BrowserSyncScope,v:ContentAuthorityInventory):string{
 const p=inspectContentAuthorityInventory(s,v),intents=p.intents.filter(i=>i.record_version===2),projects=new Set([...intents,...p.candidates].map(i=>i.project_id))
 return immutableWireIdentity({intents,candidates:p.candidates,receipts:p.receipts,rows:p.rows.filter(r=>r.record_version===9),out:p.out.filter(o=>p.candidates.some(c=>c.event_id===o.event_id)),bindings:v.bindings?.filter(b=>projects.has((b as any).project_id)),notes:v.localNotes?.filter(n=>intents.some(i=>i.note_id===(n as any).note_id))})
}
