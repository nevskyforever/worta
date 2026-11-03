import {authenticateContinuationHistory,verifyContinuationSources} from './resolutionHistorical'
import {BrowserContentAuthorityOwner} from './contentAuthorityOwner'
import type {NormalUserAuthRuntime} from '@/auth/userAuth'
import type {AuthoritativeAccountBinding} from '@/auth/accountBinding'
import type {RuntimeKeyContext} from '@/auth/keyContext'
import {encryptObjectBytes,type AccountMasterKey} from '@/crypto'
import {encodeBase64Url} from '@/api/base64url'
import {canonicalizeSyncTimestamp} from '@/cloud/syncTimestamp'
import type {NoteMutationPlan} from './noteIntents'
import {BrowserLocalAuthority,type LocalSourceToken,type LocalNoteMutationWriter} from './localAuthority'
import {localCanonicalBytes,localSafe,type LocalNoteRecord,type LocalNoteSource} from './localAuthorityRecords'
import {BrowserPersistenceError,type BrowserPersistenceCode} from './persistence'
import {historyRange,openHistoryCandidate} from './noteHistory'
import {validateNoteHistorySet,type BrowserNoteCandidate} from './noteHistoryRecords'
import {validateNoteIntentRecord} from './noteIntentRecords'
import {validateContentIntent2,contentIntentIdentity,type BrowserContentIntent2} from './contentAuthorityRecords'
import {authenticateContentIntent2,openContentIntentSource} from './contentAuthorityAuthentication'
import {authenticateContentAuthorityInventory,inspectContentAuthorityInventory} from './contentAuthorityInventory'
import {validateBrowserProjectBinding} from './projectBindings'
import {prepareMetadataPrefix,verifiedMetadataSequences} from './metadataPrefixProof'
import {validateBrowserMetadataHead} from './metadataApplyRecords'
import {prepareNotePrerequisite} from './noteTransportPrerequisite'
import {verifyStoredContentNoteProof} from './contentNoteProof'
import {noteProofDigest,verifyStoredNoteProof} from './legacyNoteApply'
import {immutableWireIdentity,snapshotBrowserSyncScope,readBrowserSyncCursor,validateBrowserUploadPair,type BrowserSyncScope,type BrowserSyncTransaction} from './syncDurability'
import type {BrowserPrefixInventory} from './provenPrefixRecords'
const same=(a:unknown,b:unknown)=>immutableWireIdentity(a)===immutableWireIdentity(b)
function fail(code:BrowserPersistenceCode='corrupt_record'):never{throw new BrowserPersistenceError(code)}
interface Mutation {owner:BrowserContentNoteIntentRuntime;scope:BrowserSyncScope;old:BrowserPrefixInventory;intent:BrowserContentIntent2;nextOrdinal:number;current:()=>boolean}
const mutations=new WeakMap<object,Mutation>()
const material=(source:LocalNoteSource)=>{const n={...source.note} as Record<string,unknown>;delete n.revision;for(const key of source.state==='live'?['created_at','updated_at']:['deleted_at'])n[key]=canonicalizeSyncTimestamp(n[key] as string);return n}
/** Explicit internal producer only. Deliberately absent from BrowserPersistence and user composition. */
export class BrowserContentNoteIntents {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly source:BrowserLocalAuthority,private readonly current:()=>boolean){}
 bind(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext){return new BrowserContentNoteIntentRuntime(this.transaction,this.source,this.current,auth,bindings,keys)}
}
export class BrowserContentNoteIntentRuntime implements LocalNoteMutationWriter {
 readonly transportKind='content_note_v1' as const
 readonly mutationStores=['localProjects','localNotes','legacyNoteIntents','cursors','projectBindings','legacyNoteHistory','outbox','receipts','inbox','ownerHeads']
 readonly local
 readonly authority:BrowserContentAuthorityOwner
 constructor(private readonly transaction:BrowserSyncTransaction,source:BrowserLocalAuthority,private readonly sessionCurrent:()=>boolean,private readonly auth:NormalUserAuthRuntime,private readonly bindings:AuthoritativeAccountBinding,private readonly keys:RuntimeKeyContext){this.local=source.bind(auth,bindings,keys).withNoteWriter(this);this.authority=new BrowserContentAuthorityOwner(transaction,sessionCurrent,auth,bindings,keys)}
 private async guard(s:BrowserSyncScope){
  if(!this.sessionCurrent()||this.auth.state!=='authenticated')fail('closed')
  const {context}=await this.bindings.ensureForCurrentUser(s.accountId),lease=this.keys.leaseForAccount(s.accountId)
  const current=()=>this.sessionCurrent()&&this.auth.isCurrent(context)&&!!lease?.isCurrent()
  if(!lease||context.userId!==s.canonicalUserId||lease.localAccountId!==s.accountId||lease.canonicalUserId!==s.canonicalUserId||lease.authEpoch!==context.authEpoch||!current())fail('account_mismatch')
  return{lease,current}
 }
 private read(tx:IDBTransaction,s:BrowserSyncScope,done:(v:BrowserPrefixInventory)=>void,abort:(e:unknown)=>void){
  const names=['inbox','outbox','receipts','projectBindings','ownerHeads','legacyNoteHistory','legacyNoteIntents','localNotes','localProjects'],rs=names.map(n=>tx.objectStore(n).getAll(historyRange(s)));let n=0
  const finish=()=>{if(++n!==rs.length)return;readBrowserSyncCursor(tx,s,cursor=>{try{done({rows:rs[0]!.result,out:rs[1]!.result,receipts:rs[2]!.result,bindings:rs[3]!.result,owners:rs[4]!.result,history:rs[5]!.result,intents:rs[6]!.result,localNotes:rs[7]!.result,projects:rs[8]!.result,cursor})}catch(e){abort(e)}},abort)}
  rs.forEach(r=>r.onsuccess=finish)
 }
 private inventory(s:BrowserSyncScope){return this.transaction<BrowserPrefixInventory>(s,this.mutationStores,'readonly',(tx,result,abort)=>this.read(tx,s,result,abort))}
 private async metadata(s:BrowserSyncScope,v:BrowserPrefixInventory,project:string,bootstrap:string,amk:AccountMasterKey,current:()=>boolean,pinned?:string){
  const view={...v,rows:v.rows.filter(r=>['project_metadata','stage','stage_order'].includes(JSON.parse((r as {wire_item:string}).wire_item).event.entity_type))}
  const cert=await prepareMetadataPrefix(s,view,amk,current,true),seqs=verifiedMetadataSequences(cert,s,view)
  const raw=v.owners.find(o=>(o as {owner_kind:string;owner_id:string}).owner_kind==='project_metadata'&&(o as {owner_id:string}).owner_id===project)
  if(!raw)fail('stale_authority')
  const head=validateBrowserMetadataHead(raw,s),tip=head.history.find(p=>p.event_id===head.tip_event_ids[0]),dependency=pinned?head.history.find(p=>p.event_id===pinned):tip
  if(head.bootstrap_id!==bootstrap||head.tip_event_ids.length!==1||!tip||!dependency||tip.operation==='delete'||dependency.operation==='delete'||!seqs.has(tip.server_sequence)||!seqs.has(dependency.server_sequence))fail('stale_authority')
  return dependency.event_id
 }
 async list(s:BrowserSyncScope,project:string):Promise<BrowserContentIntent2[]>{
  s=snapshotBrowserSyncScope(s);const {lease,current}=await this.guard(s),v=await this.inventory(s)
  return lease.use(async amk=>{await authenticateContentAuthorityInventory(s,v,amk,current);const rows=(v.intents??[]).map(i=>validateNoteIntentRecord(i,s)).filter((i):i is BrowserContentIntent2=>i.record_version===2&&i.project_id===project);for(const i of rows)await this.metadata(s,v,project,i.bootstrap_id,amk,current,i.dependencies.metadata_event_id);if(!current())fail('closed');return structuredClone(rows).sort((a,b)=>a.local_ordinal-b.local_ordinal)})
 }
 async prepareMutation(s:BrowserSyncScope,notes:LocalNoteRecord[],amk:AccountMasterKey,source:LocalSourceToken):Promise<NoteMutationPlan>{
  s=snapshotBrowserSyncScope(s);const {current}=await this.guard(s)
  if(notes.length!==1)fail('unsupported_local_source')
  const row=structuredClone(notes[0]!),v=await this.inventory(s),parsed=inspectContentAuthorityInventory(s,v)
  const allocation=[...parsed.out,...parsed.intents].sort((a,b)=>a.local_ordinal-b.local_ordinal)
  if(allocation.some((r,i)=>r.local_ordinal!==i+1)||new Set(allocation.map(r=>r.event_id)).size!==allocation.length||v.cursor.next_local_ordinal!==allocation.length+1)fail()
  await authenticateContentAuthorityInventory(s,v,amk,current)
  const binding=validateBrowserProjectBinding(v.bindings.find(b=>(b as {project_id:string}).project_id===row.project_id),s)
  if(binding.mode!=='import_remote'||binding.phase!=='captured'||binding.remote_state!=='active')fail('ineligible_lineage')
  if(v.history.some(h=>(h as {record_kind:string}).record_kind==='resolution'))return this.prepareResolutionMutation(s,row,amk,source,v,current,binding.bootstrap_id)
  const history=validateNoteHistorySet(s,v.history).filter(h=>h.note_id===row.note_id)
  if(history.some(h=>h.project_id!==row.project_id||h.bootstrap_id!==binding.bootstrap_id))fail('stale_lineage')
  if(history.some(h=>h.record_kind==='group'||h.record_kind==='local_version'))fail('note_conflict_unproven')
  if(history.some(h=>h.record_kind==='candidate'&&h.record_version===3))fail('unsupported_local_source') // .08 owns the sealed lifecycle.
  const candidates=history.filter((h):h is BrowserNoteCandidate=>h.record_kind==='candidate'),head=history.find(h=>h.record_kind==='head')
  const parent=head&&candidates.find(c=>c.event_id===head.linear_head_event_id)
  if(candidates.length&&(!head||head.record_kind!=='head'||head.tip_event_ids.length!==1||head.tip_event_ids[0]!==head.linear_head_event_id||!parent))fail('candidate_history_unavailable')
  const chain=new Set<string>();let event=parent?.event_id??null
  while(event!==null){if(chain.has(event))fail();chain.add(event);const c=candidates.find(c=>c.event_id===event);if(!c)fail();event=c.header.parent_event_id}
  if(chain.size!==candidates.length)fail('note_conflict_unproven')
  const existing=parsed.intents.filter(i=>i.note_id===row.note_id);if(existing.length>1||existing.some(i=>i.project_id!==row.project_id))fail()
  const prev=existing[0];if(prev&&prev.record_version!==2)fail('unsupported_local_source')
  // Every retained same-Note input must already have a final clean outcome. A received
  // sibling/resolution/own event is retained evidence, never silently skipped.
  const incoming=parsed.rows.filter(r=>{const e=JSON.parse(r.wire_item).event;return e.entity_type==='note'&&e.entity_id===row.note_id})
  for(const r of incoming){const c=candidates.find(c=>c.event_id===r.event_id);if(!c||!['note_proven','content_note_proven'].includes(r.state))fail('note_conflict_unproven')}
  for(const c of candidates){
   const r=incoming.find(r=>r.event_id===c.event_id);if(!r||r.state==='note_resolution_proven')fail('candidate_history_unavailable')
   await openHistoryCandidate(s,c,amk)
   if(c.record_version===2){await verifyStoredContentNoteProof(s,amk,r,v.history as never);await prepareNotePrerequisite(s,r,binding,amk,current,undefined,v)}
   else{
    await verifyStoredNoteProof(s,amk,r)
    const wire=JSON.parse(r.wire_item),{device_id,server_sequence,...e}=wire.event
    if(r.protocol_version!==c.protocol_version||device_id!==c.source_device_id||!same(c.header,r.state==='note_proven'?r.note_proof.header:null)||!same(JSON.parse(c.wire_item),{event:e,object:wire.object}))fail()
    if(c.source_device_id===s.deviceId){const out=v.out.find(o=>(o as {event_id:string}).event_id===c.event_id),receipt=v.receipts.find(r=>(r as {event_id:string}).event_id===c.event_id);validateBrowserUploadPair(out,receipt,s);if(r.state!=='note_proven'||r.note_proof.outcome!=='self_echo_applied'||(receipt as {server_sequence:number}).server_sequence!==server_sequence||!same(JSON.parse((out as {raw_body:string}).raw_body).items[0],JSON.parse(c.wire_item)))fail('self_echo_unavailable')}
   }
  }
  for(const out of parsed.out){const e=JSON.parse(out.raw_body).items[0].event;if(e.entity_type==='note'&&e.entity_id===row.note_id&&(!candidates.some(c=>c.event_id===out.event_id)||out.state!=='accepted'))fail('self_echo_unavailable')}
  const dependency=await this.metadata(s,v,row.project_id,binding.bootstrap_id,amk,current,prev?.dependencies.metadata_event_id)
  // Canonical source was AEAD-authenticated by the existing source token. Compare
  // its old material to the authenticated head, never choose a winner by revision/time.
  await this.local.withDecryptedSnapshot(s,source,async prior=>{
   const before=prior.notes.find(n=>n.note.id===row.note_id)
   if(prev){await authenticateContentIntent2(s,prev,amk,current);if(!before||!same(prev.snapshot,v.localNotes?.find(n=>(n as LocalNoteRecord).note_id===row.note_id)))fail('stale_source')}
   else if(parent){const plain=await openHistoryCandidate(s,parent,amk);if(!before||before.state!=='live'||plain.mutation==='delete'||!same(material(before),material({state:'live',note:{...plain.note,revision:0}} as LocalNoteSource)))fail('stale_source')}
   else if(before||v.localNotes?.some(n=>(n as LocalNoteRecord).note_id===row.note_id)||incoming.length||existing.length)fail('unsupported_local_source')
  })
  const generation=(prev?.mutation_generation??0)+1,revision=prev?.revision??(parent?.header.revision??0)+1,ordinal=prev?.local_ordinal??v.cursor.next_local_ordinal,nextOrdinal=prev?v.cursor.next_local_ordinal:ordinal+1
  if(![generation,revision,ordinal,nextOrdinal].every(n=>localSafe(n,1)))fail('source_limit')
  if(prev&&(prev.bootstrap_id!==binding.bootstrap_id||prev.operation!==(row.deleted?'delete':'upsert')))fail('unsupported_local_source')
  if(row.deleted&&(!parent||prev))fail('unsupported_local_source')
  const eventId=prev?.event_id??crypto.randomUUID()
  if(!prev&&(parsed.intents.some(i=>i.event_id===eventId)||parsed.out.some(o=>o.event_id===eventId)||parsed.rows.some(r=>r.event_id===eventId)||v.history.some(h=>(h as {event_id?:string}).event_id===eventId)))fail('conflicting_record')
  // Source decryption and ordinary route admission use the frozen I2 verifier below.
  // No codec frame, compression selection or outbound object is produced here.
  const intent:BrowserContentIntent2={record_version:2,transport_kind:'content_note_v1',account_id:s.accountId,device_id:s.deviceId,project_id:row.project_id,note_id:row.note_id,bootstrap_id:binding.bootstrap_id,event_id:eventId,operation:row.deleted?'delete':'upsert',revision,parent_event_id:prev?prev.parent_event_id:parent?.event_id??null,updated_at:'',deleted_at:null,local_ordinal:ordinal,mutation_generation:generation,dependencies:{bootstrap_id:binding.bootstrap_id,metadata_event_id:dependency,stage_event_ids:[]},snapshot:row,snapshot_digest:await noteProofDigest(row),intent_envelope:row.envelope,seal_state:'pending',seal_attempt_count:0,last_error_code:null,next_attempt_at:null}
  // The already prepared source has no plaintext side channel; decrypt with existing
  // source validation through a temporary immutable source row, using the same AAD.
  const next=await openContentIntentSource(s,row,amk,current)
  const note=next.note
  if(note.stage_id!==null||note.source_type!=='project'||note.source_map_id!==null||note.source_node_id!==null||!['html','plain'].includes(note.content_format))fail('unsupported_local_source')
  intent.updated_at=next.state==='live'?next.note.updated_at:next.note.deleted_at;intent.deleted_at=next.state==='live'?null:next.note.deleted_at
  const bytes=localCanonicalBytes({intent_format:2,identity:contentIntentIdentity(intent)});let encrypted
  try{encrypted=await encryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:row.project_id,entityId:row.note_id,entityType:'note'},bytes);intent.intent_envelope={crypto_version:1,aad_version:1,nonce:encodeBase64Url(encrypted.nonce),ciphertext:encodeBase64Url(encrypted.ciphertext)}}finally{bytes.fill(0);encrypted?.nonce.fill(0);encrypted?.ciphertext.fill(0)}
  validateContentIntent2(intent,s);await authenticateContentIntent2(s,intent,amk,current)
  if(!current())fail('closed');const token=Object.freeze({}) as NoteMutationPlan;mutations.set(token,{owner:this,scope:s,old:v,intent,nextOrdinal,current});return token
 }
 private async prepareResolutionMutation(s:BrowserSyncScope,row:LocalNoteRecord,amk:AccountMasterKey,source:LocalSourceToken,v:BrowserPrefixInventory,current:()=>boolean,bootstrap:string):Promise<NoteMutationPlan>{
  if(v.rows.some(r=>(r as {state:string}).state==='received'))fail('note_conflict_unproven')
  const history=await authenticateContinuationHistory(s,v,amk,current)
  await verifyContinuationSources(s,{...v,projects:v.projects!},history,amk,current)
  if(v.intents?.length)fail('unsupported_local_source')
  const head=history.dag.history.find(h=>h.record_kind==='head'&&h.project_id===row.project_id&&h.note_id===row.note_id)
  if(!head||head.record_kind!=='head'||head.record_version!==2||head.tip_event_ids.length!==1||history.groups.some(g=>g.effective==='open'))fail('note_conflict_unproven')
  const parent=history.dag.events.get(head.linear_head_event_id),payload=history.opened.get(head.linear_head_event_id)??history.resolutions.get(head.linear_head_event_id)?.payload
  if(!parent||!payload||parent.header.operation==='delete'||parent.record_kind==='resolution'&&parent.result_operation==='delete')fail('unsupported_local_source')
  if(parent.record_kind==='candidate'&&parent.record_version===3&&!v.rows.some(r=>(r as any).event_id===parent.event_id&&(r as any).state==='content_note_self_echo_proven'))fail('self_echo_unavailable')
  await this.local.withDecryptedSnapshot(s,source,async prior=>{const note=prior.notes.find(n=>n.note.id===row.note_id);const expected=payload.version===2?payload.result.note:payload.note;if(!note||note.state!=='live'||!same(material(note),material({state:'live',note:{...expected,revision:0}} as LocalNoteSource)))fail('stale_source')})
  const dependency=await this.metadata(s,v,row.project_id,bootstrap,amk,current),next=await openContentIntentSource(s,row,amk,current)
  if(next.note.stage_id!==null)fail('unsupported_local_source')
  const ordinal=v.cursor.next_local_ordinal,intent:BrowserContentIntent2={record_version:2,transport_kind:'content_note_v1',account_id:s.accountId,device_id:s.deviceId,project_id:row.project_id,note_id:row.note_id,bootstrap_id:bootstrap,event_id:crypto.randomUUID(),operation:next.state==='live'?'upsert':'delete',revision:parent.header.revision+1,parent_event_id:parent.event_id,updated_at:next.state==='live'?next.note.updated_at:next.note.deleted_at,deleted_at:next.state==='live'?null:next.note.deleted_at,local_ordinal:ordinal,mutation_generation:1,dependencies:{bootstrap_id:bootstrap,metadata_event_id:dependency,stage_event_ids:[]},snapshot:row,snapshot_digest:await noteProofDigest(row),intent_envelope:row.envelope,seal_state:'pending',seal_attempt_count:0,last_error_code:null,next_attempt_at:null}
  const bytes=localCanonicalBytes({intent_format:2,identity:contentIntentIdentity(intent)})
  try{const e=await encryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:row.project_id,entityId:row.note_id,entityType:'note'},bytes);try{intent.intent_envelope={crypto_version:1,aad_version:1,nonce:encodeBase64Url(e.nonce),ciphertext:encodeBase64Url(e.ciphertext)}}finally{e.nonce.fill(0);e.ciphertext.fill(0)}}finally{bytes.fill(0)}
  await authenticateContentIntent2(s,intent,amk,current);if(!current())fail('closed')
  const token=Object.freeze({}) as NoteMutationPlan;mutations.set(token,{owner:this,scope:s,old:v,intent,nextOrdinal:ordinal+1,current});return token
 }
 scheduleMutation(tx:IDBTransaction,token:NoteMutationPlan,done:()=>void,abort:(e:unknown)=>void){
  const p=mutations.get(token);if(!p||p.owner!==this||!p.current()){abort(new BrowserPersistenceError('closed'));return}
  this.read(tx,p.scope,v=>{try{
   if(!p.current())fail('closed');if(!same(v,p.old))fail('stale_source')
   const writes=[tx.objectStore('legacyNoteIntents')[p.old.intents?.some(i=>(i as {event_id:string}).event_id===p.intent.event_id)?'put':'add'](p.intent)]
   if(p.nextOrdinal!==v.cursor.next_local_ordinal)writes.push(tx.objectStore('cursors').put({record_version:1,account_id:p.scope.accountId,device_id:p.scope.deviceId,...v.cursor,next_local_ordinal:p.nextOrdinal}))
   writes.forEach(r=>r.onsuccess=()=>{if(!p.current())abort(new BrowserPersistenceError('closed'))});done()
  }catch(e){abort(e)}},abort)
 }
}
