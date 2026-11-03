import type {AccountMasterKey} from '@/crypto'
import {validateMixedContinuationInventory,validateLegacy3HMixedContinuationInventory,MixedSourceWitnessError} from './mixedSourceValidation'
import type {NormalUserAuthRuntime} from '@/auth/userAuth'
import type {AuthoritativeAccountBinding} from '@/auth/accountBinding'
import type {RuntimeKeyContext} from '@/auth/keyContext'
import type {NoteSyncPlaintext} from '@/cloud/noteSyncCodec'
import {BrowserPersistenceError} from './persistence'
import {BrowserLocalAuthority,type BrowserLocalAuthorityRuntime,type LocalSourceToken,type ProtectedNoteSourcePlan,scheduleProtectedNoteApply,assertLocalSourceInTransaction} from './localAuthority'
import {currentBrowserDevice,type VerifiedBrowserDevice} from './deviceRegistration'
import {resolutionInventoryStores,scheduleResolutionInventory,type ResolutionInventorySnapshot,MAX_PLANNER_INVENTORY_BYTES,MAX_PLANNER_ROWS} from './resolutionPlanner'
import {boundResolutionJson,readFutureInboxRecord,type BrowserResolutionHead2} from './resolutionRecords'
import {authenticateContinuationHistory,authenticateHContinuationHistory,verifyContinuationSources,continuationDependencyView,sameResolutionValue as same,type HistoricalResolutionOutput} from './resolutionHistorical'
import {snapshotBrowserSyncScope,validateBrowserInboxRecord,type BrowserSyncScope,type BrowserSyncTransaction,type BrowserInboxRecord} from './syncDurability'
import {validateBrowserProjectBinding} from './projectBindings'
import {openNoteTransport} from './noteTransport'
import {makeHistoryCandidate} from './noteHistory'
import {prepareNotePrerequisite,prepareNoteTransportCommit,prepareNoteTransportConflictCommit} from './noteTransportPrerequisite'
import {noteProofDigest,noteOutcomeDigest} from './legacyNoteApply'
import {contentProofFor} from './contentNoteProof'
import {contentConflictProofFor} from './contentNoteConflictProof'
import {validateBrowserNoteProof,validateBrowserNoteConflictProof,type BrowserNoteProof,type BrowserNoteConflictProof} from './noteApplyRecords'
import {assertContentNoteRoute} from './localAuthorityRecords'
import type {BrowserNoteCandidate,BrowserNoteConflictGroup} from './noteHistoryRecords'
export interface ResolutionContinuationToken {readonly resolutionContinuation:unique symbol}
interface Plan {owner:BrowserResolutionContinuationRuntime;scope:BrowserSyncScope;inventory:ResolutionInventorySnapshot;row:BrowserInboxRecord;current:()=>boolean;local:BrowserLocalAuthorityRuntime;plaintext:NoteSyncPlaintext;content:boolean;sourceToken:LocalSourceToken;source?:ProtectedNoteSourcePlan;candidate?:BrowserNoteCandidate;head?:BrowserResolutionHead2;group?:BrowserNoteConflictGroup;newGroup?:boolean;nextRow?:BrowserInboxRecord}
const plans=new WeakMap<object,Plan>()
export interface HHistoryToken {readonly hHistory:unique symbol}
interface HHistoryRequest {owner:BrowserResolutionContinuationRuntime;scope:BrowserSyncScope;inventory:ResolutionInventorySnapshot;amk:AccountMasterKey;current:()=>boolean}
const historyRequests=new WeakMap<object,HHistoryRequest>()
function fail(code:'closed'|'corrupt_record'|'stale_source'|'unsupported_local_source'|'note_orphan'|'note_conflict_unproven'|'conflicting_record'|'source_limit'='corrupt_record'):never{throw new BrowserPersistenceError(code)}
/** Internal one-use handoff; no caller-supplied inventory, key or allow flag. */
export function consumeHHistoryRequest(token:HHistoryToken,owner:BrowserResolutionContinuationRuntime){
 const p=historyRequests.get(token)
 if(!p||p.owner!==owner||!p.current())fail('closed')
 historyRequests.delete(token)
 return p
}
export function verifiedContinuationForLocal(token:ResolutionContinuationToken,local:BrowserLocalAuthorityRuntime,scope:BrowserSyncScope){
 const p=plans.get(token);if(!p||p.local!==local||!same(p.scope,scope)||!p.current()||p.candidate)fail('closed')
 return{plaintext:p.plaintext,content:p.content,current:p.current}
}
type SourceRoute='historical'|'mixed'
type AuthenticatedHistory=Awaited<ReturnType<typeof authenticateContinuationHistory>>
/** Select ONLY from fully authenticated evidence, before source verification.
 * Never try the old verifier after a failed mixed check. A post-F independent
 * outcome (including unsupported ownership/forks) cannot borrow H-only authority. */
async function verifyHSource(s:BrowserSyncScope,v:ResolutionInventorySnapshot,h:AuthenticatedHistory,amk:AccountMasterKey,current:()=>boolean,expected?:SourceRoute,mixedValidated=false):Promise<SourceRoute>{
 // Retained post-F sequences cannot disappear and turn a mixed state into an
 // apparent historical-only one. Pre-F historical admission stays unchanged.
 const ordered=[...h.rows].sort((a,b)=>a.server_sequence-b.server_sequence)
 for(const root of h.resolutions.values()){
  let last=root.record.server_sequence
  for(const row of ordered)if(row.server_sequence>root.record.server_sequence){if(row.server_sequence!==last+1)fail('corrupt_record');last=row.server_sequence}
  if(last!==v.cursor.pull_cursor)fail('corrupt_record')
 }
 const mixed=[...h.resolutions.values()].some(root=>[...h.dag.events.values()].some(c=>
  c.record_kind==='candidate'&&c.project_id===root.record.project_id&&![root.record.note_id,root.record.retained_note_id].includes(c.note_id)&&
  ((h.rows.find(r=>r.event_id===c.event_id)?.server_sequence??0)>root.record.server_sequence||(c.projection?.source_revision??0)>root.project.source_revision)))
 const selected:SourceRoute=mixed?'mixed':'historical'
 if(expected&&selected!==expected)fail('unsupported_local_source')
 if(selected==='historical')await verifyContinuationSources(s,v,h,amk,current)
 else{
  try{
   if(mixedValidated)await validateLegacy3HMixedContinuationInventory(s,v,amk,current,h)
   else await validateMixedContinuationInventory(s,v,amk,current)
  }catch(e){
   if(e instanceof BrowserPersistenceError||e instanceof MixedSourceWitnessError)throw e
   fail('corrupt_record') // Decoder/parser errors never expose source plaintext.
  }
 }
 return selected
}
const route=(n:NoteSyncPlaintext['note'])=>[n.id,n.project_id,n.stage_id,n.source_type,n.source_map_id,n.source_node_id,n.content_format]
const orderedNotes=<T extends {note_id:string}>(notes:readonly T[])=>[...notes].sort((a,b)=>a.note_id.localeCompare(b.note_id))
/** Explicit isolated v10 entry. Absent from production persistence/editor/schedulers. */
export class BrowserResolutionContinuation {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly local:BrowserLocalAuthority,private readonly current:()=>boolean){}
 bind(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext,device:VerifiedBrowserDevice){return new BrowserResolutionContinuationRuntime(this.transaction,this.local.bind(auth,bindings,keys),this.current,auth,bindings,keys,device)}
}
export class BrowserResolutionContinuationRuntime {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly local:BrowserLocalAuthorityRuntime,private readonly profileCurrent:()=>boolean,private readonly auth:NormalUserAuthRuntime,private readonly bindings:AuthoritativeAccountBinding,private readonly keys:RuntimeKeyContext,private readonly device:VerifiedBrowserDevice){}
 private async guard(s:BrowserSyncScope){
  const initial=this.auth.requireContext(),{context}=await this.bindings.ensureForCurrentUser(s.accountId),lease=this.keys.leaseForAccount(s.accountId)
  const current=()=>this.profileCurrent()&&this.auth.isCurrent(initial)&&this.auth.isCurrent(context)&&!!lease?.isCurrent()&&currentBrowserDevice(this.device,s,this.auth)
  if(!lease||lease.authEpoch!==context.authEpoch||lease.localAccountId!==s.accountId||lease.canonicalUserId!==s.canonicalUserId||context.userId!==s.canonicalUserId||!current())fail('closed')
  return{lease,current}
 }
 private load(s:BrowserSyncScope,current:()=>boolean){return this.transaction<ResolutionInventorySnapshot>(s,resolutionInventoryStores,'readonly',(tx,result,abort)=>scheduleResolutionInventory(tx,s,current,result,abort))}
 async #history(s:BrowserSyncScope,v:ResolutionInventorySnapshot,amk:AccountMasterKey,current:()=>boolean){
  if(!v.history.some((r:any)=>r.record_kind==='resolution'&&r.protocol_version===3&&r.transport_kind==='legacy_resolution_v2'))return{history:await authenticateContinuationHistory(s,v,amk,current),mixedValidated:false}
  const token=Object.freeze({}) as HHistoryToken
  historyRequests.set(token,{owner:this,scope:s,inventory:v,amk,current})
  try{return{history:await authenticateHContinuationHistory(token,this),mixedValidated:true}}finally{historyRequests.delete(token)}
 }

 async prepare(input:BrowserSyncScope,sequence:number):Promise<ResolutionContinuationToken>{
  const s=snapshotBrowserSyncScope(input),{lease,current}=await this.guard(s),v=await this.load(s,current)
  const check=()=>{if(!current())fail('closed')},step=async<T>(p:Promise<T>)=>{const r=await p;check();return r}
  return lease.use(async amk=>{
   const authenticated=await step(this.#history(s,v,amk,current)),history=authenticated.history,sourceRoute=await step(verifyHSource(s,v,history,amk,current,undefined,authenticated.mixedValidated))
   const future=readFutureInboxRecord(v.rows.find((r:any)=>r.server_sequence===sequence),s)
   if(future.state!=='received'||future.server_sequence>v.cursor.pull_cursor)fail('conflicting_record')
   if(sourceRoute==='mixed'&&history.rows.some(r=>r.state==='received'&&r.server_sequence<sequence))fail('note_orphan')
   const row=validateBrowserInboxRecord(future,s),wire=JSON.parse(row.wire_item),content=row.protocol_version===3
   if(wire.event.device_id===s.deviceId||wire.event.entity_type!=='note'||content&&wire.event.operation!=='event'||!content&&!['upsert','delete'].includes(wire.event.operation))fail('unsupported_local_source')
   const binding=v.bindings.map(b=>validateBrowserProjectBinding(b,s)).find(b=>b.project_id===wire.event.project_id)
   if(!binding||binding.remote_state!=='active'||!(binding.mode==='import_remote'&&binding.phase==='captured'||binding.mode==='upload_existing'&&binding.phase==='completing'))fail('unsupported_local_source')
   const prerequisite=content?await step(prepareNotePrerequisite(s,row,binding,amk,current,undefined,continuationDependencyView(v,row))):undefined
   const plaintext=prerequisite?.plaintext??(await step(openNoteTransport(s,amk,2,row.wire_item,'legacy'))).plaintext,h=plaintext.header
   if(plaintext.mutation==='create'||h.parent_event_id===null)fail('unsupported_local_source')
   assertContentNoteRoute(plaintext.note)
   if(!content&&(plaintext.note.stage_id!==null||plaintext.note.content_format!=='html'))fail('unsupported_local_source')
   const head=history.dag.history.find(r=>r.record_kind==='head'&&r.project_id===h.project_id&&r.note_id===h.entity_id&&r.bootstrap_id===binding.bootstrap_id)
   if(!head||head.record_kind!=='head'||head.record_version!==2)fail('note_orphan')
   const rootFor=(id:string):HistoricalResolutionOutput|undefined=>{
    const node=history.dag.events.get(id);if(!node)return undefined
    if(node.record_kind==='resolution')return(node.note_id===h.entity_id||node.retained_note_id===h.entity_id)?history.resolutions.get(id):undefined
    if(node.project_id!==h.project_id||node.note_id!==h.entity_id||node.bootstrap_id!==binding.bootstrap_id||!node.header.parent_event_id)return undefined
    return rootFor(node.header.parent_event_id)
   }
   const root=rootFor(h.parent_event_id),workingRoot=rootFor(head.linear_head_event_id),parent=history.dag.events.get(h.parent_event_id),parentRow=history.rows.find(r=>r.event_id===h.parent_event_id)
   if(!root||root!==workingRoot||!parent||parent.project_id!==h.project_id||parent.bootstrap_id!==binding.bootstrap_id||!parentRow||parentRow.server_sequence>=sequence||h.revision!==parent.header.revision+1)fail('note_orphan')
   const parentNote=parent.record_kind==='candidate'?history.opened.get(parent.event_id)!.note:parent.note_id===h.entity_id?root.payload.result.note:root.payload.resolution.strategy==='keep_both'?root.payload.resolution.retained_note:undefined
   if(!parentNote||!same(route(parentNote),route(plaintext.note)))fail('unsupported_local_source')
   if('deleted_at'in parentNote&&plaintext.mutation!=='delete')fail('unsupported_local_source') // No implicit resurrection.
   if((v.intents as any[]).some(i=>i.project_id===h.project_id&&i.note_id===h.entity_id))fail('unsupported_local_source')
   const source=await step(this.local.readSnapshot(s,h.project_id))
   if(!same(source.project,v.projects.find((p:any)=>p.project_id===h.project_id))||!same(orderedNotes(source.notes),orderedNotes(v.localNotes.filter((n:any)=>n.project_id===h.project_id) as Array<{note_id:string}>)))fail('stale_source')
   const open=history.groups.find(g=>g.effective==='open'&&g.group.project_id===h.project_id&&g.group.note_id===h.entity_id)?.group
   const linear=history.dag.events.get(head.linear_head_event_id)
   const sibling=linear?.record_kind==='candidate'&&linear.header.parent_event_id===h.parent_event_id&&linear.header.revision===h.revision
   let group:BrowserNoteConflictGroup|undefined,newGroup=false
   if(open){if(open.common_parent_event_id!==h.parent_event_id||open.tip_revision!==h.revision||open.members.some(m=>m.source==='local_unsealed')||open.tip_event_ids.length>=64)fail('note_conflict_unproven');group=structuredClone(open);group.conflict_generation++}
   else if(sibling){
    const prior=history.rows.find(r=>r.event_id===head.linear_head_event_id)
    if(!prior||!['note_proven','content_note_proven'].includes(prior.state))fail('note_conflict_unproven')
    const id=crypto.randomUUID();newGroup=true
    group={record_version:1,account_id:s.accountId,device_id:s.deviceId,project_id:h.project_id,note_id:h.entity_id,bootstrap_id:binding.bootstrap_id,record_kind:'group',record_id:id,group_id:id,common_parent_event_id:h.parent_event_id,tip_revision:h.revision,conflict_generation:1,lifecycle:'open',members:[{version_id:`applied:${s.accountId}:${linear!.event_id}`,event_id:linear!.event_id,source:'remote_applied',server_sequence:prior.server_sequence,conflict_generation:1}],tip_event_ids:[]}
   }else if(head.tip_event_ids.length!==1||head.linear_head_event_id!==h.parent_event_id)fail('note_conflict_unproven')
   if(sourceRoute==='mixed'&&!group&&history.groups.some(g=>g.effective==='open'))fail('note_conflict_unproven')
   const token=Object.freeze({}) as ResolutionContinuationToken,p:Plan={owner:this,scope:s,inventory:v,row,current,local:this.local,plaintext,content,sourceToken:source.token,newGroup}
   plans.set(token,p)
   try{
    const prepared=group?undefined:await step(this.local.prepareProtectedContinuationApply(s,source.token,token));p.source=prepared?.token
    let candidate:BrowserNoteCandidate
    if(content){if(!prerequisite)fail();candidate=(group?await step(prepareNoteTransportConflictCommit(prerequisite.certificate,plaintext)):await step(prepareNoteTransportCommit(prerequisite.certificate,plaintext,prepared!.note))).candidate}
    else{const {device_id:_device,server_sequence:_seq,...event}=wire.event;candidate=await step(makeHistoryCandidate(s,binding,2,wire.event.device_id,{event,object:wire.object},plaintext,'peer',prepared?.note??null))}
    if(group){group.members.push({version_id:`remote:${s.accountId}:${h.event_id}`,event_id:h.event_id,source:'remote',server_sequence:sequence,conflict_generation:group.conflict_generation});group.members.sort((a,b)=>a.event_id<b.event_id?-1:1);group.tip_event_ids=group.members.map(m=>m.event_id)}
    let nextRow:BrowserInboxRecord
    if(content){
     const proof=group?await step(contentConflictProofFor(s,row,candidate,group,plaintext.mutation as 'update'|'delete')):await step(contentProofFor(s,row,candidate,plaintext.mutation))
     nextRow=validateBrowserInboxRecord(group?{...row,record_version:8,state:'content_note_conflict_preserved',content_note_conflict_proof:proof}:{...row,record_version:7,state:'content_note_proven',content_note_proof:proof},s)
    }else{
     const common={record_version:1 as const,owner_kind:'legacy_note' as const,account_id:s.accountId,bootstrap_id:binding.bootstrap_id,protocol_version:2 as const,reference:wire.event,header:h,mutation:plaintext.mutation,wire_digest:await step(noteProofDigest({protocol_version:2,event:wire.event,object:wire.object})),outcome_digest:''}
     const proof:BrowserNoteProof|BrowserNoteConflictProof=group?{...common,outcome:'conflict_preserved',mutation:plaintext.mutation as 'update'|'delete',group_id:group.group_id,conflict_generation:group.conflict_generation}:{...common,outcome:'applied',source_revision:prepared!.source_revision,local_note_digest:await step(noteProofDigest(prepared!.note))}
     proof.outcome_digest=await step(noteOutcomeDigest(proof));if(proof.outcome==='applied')validateBrowserNoteProof(proof,s);else validateBrowserNoteConflictProof(proof,s)
     nextRow=validateBrowserInboxRecord(group?{...row,record_version:4,state:'note_conflict_preserved',note_conflict_proof:proof}:{...row,record_version:3,state:'note_proven',note_proof:proof},s)
    }
    const nextHead:BrowserResolutionHead2={...head,head_revision:head.head_revision+1,linear_head_event_id:group?head.linear_head_event_id:h.event_id,tip_event_ids:group?group.tip_event_ids:[h.event_id],node_ref:{kind:'candidate',event_id:group?head.linear_head_event_id:h.event_id}}
    const predicted:ResolutionInventorySnapshot={...v,history:[...v.history.filter((r:any)=>!(r.record_kind==='head'&&r.project_id===h.project_id&&r.note_id===h.entity_id)&&!(group&&r.record_kind==='group'&&r.project_id===h.project_id&&r.note_id===h.entity_id&&r.group_id===group.group_id)),candidate,...(group?[group]:[]),nextHead],rows:v.rows.map(r=>same(r,row)?nextRow:r),projects:prepared?v.projects.map((r:any)=>r.project_id===h.project_id?prepared.project:r):v.projects,localNotes:prepared?v.localNotes.map((n:any)=>n.project_id===h.project_id&&n.note_id===h.entity_id?prepared.note:n):v.localNotes}
    let remaining=MAX_PLANNER_INVENTORY_BYTES
    for(const values of [predicted.rows,predicted.out,predicted.receipts,predicted.bindings,predicted.owners,predicted.history,predicted.intents,predicted.projects,predicted.localNotes,predicted.checkpoints]){
     if(values.length>MAX_PLANNER_ROWS)fail('source_limit');for(const value of values){try{boundResolutionJson(value,remaining)}catch{fail('source_limit')}const bytes=new TextEncoder().encode(JSON.stringify(value));try{remaining-=bytes.length+2;if(remaining<0)fail('source_limit')}finally{bytes.fill(0)}}
    }
    const final=await step(this.#history(s,predicted,amk,current));await step(verifyHSource(s,predicted,final.history,amk,current,sourceRoute,final.mixedValidated))
    p.candidate=candidate;p.head=nextHead;p.group=group;p.nextRow=nextRow
    if(!same(await step(this.load(s,current)),v))fail('stale_source')
    return token
   }catch(e){plans.delete(token);throw e}
  })
 }
 async commit(token:ResolutionContinuationToken):Promise<'applied'|'conflict_preserved'>{
  const p=plans.get(token);if(!p||p.owner!==this||!p.current()||!p.candidate||!p.head||!p.nextRow)fail('closed');plans.delete(token)
  let activeAbort:((e:unknown)=>void)|undefined;const stop=()=>activeAbort?.(new BrowserPersistenceError('closed')),unAuth=this.auth.onInvalidated(stop),unKey=this.keys.onInvalidated(stop)
  try{return await this.transaction(p.scope,resolutionInventoryStores,'readwrite',(tx,result,abort)=>{
   activeAbort=abort
   scheduleResolutionInventory(tx,p.scope,p.current,v=>{try{
    if(!p.current())fail('closed');if(!same(v,p.inventory))fail('stale_source')
    const write=()=>{
     if(!p.current())fail('closed')
     const store=tx.objectStore('legacyNoteHistory'),requests=[store.add(p.candidate)]
     if(p.group)requests.push(p.newGroup?store.add(p.group):store.put(p.group))
     requests.push(store.put(p.head),tx.objectStore('inbox').put(p.nextRow))
     requests.forEach(r=>r.onsuccess=()=>{try{if(!p.current())fail('closed')}catch(e){abort(e)}})
     result(p.group?'conflict_preserved':'applied')
    }
    if(p.source)scheduleProtectedNoteApply(tx,p.scope,p.source,write,abort);else assertLocalSourceInTransaction(tx,p.scope,p.sourceToken,write,abort)
   }catch(e){abort(e)}},abort)
  })}finally{activeAbort=undefined;unAuth();unKey()}
 }
 async history(input:BrowserSyncScope){
  const s=snapshotBrowserSyncScope(input),{lease,current}=await this.guard(s),v=await this.load(s,current)
  const result=await lease.use(async amk=>{const value=await this.#history(s,v,amk,current),h=value.history;await verifyHSource(s,v,h,amk,current,undefined,value.mixedValidated);return{readOnly:true as const,resolutions:h.resolutions.size,ordinary:h.opened.size,closedGroups:h.groups.filter(g=>g.effective==='resolved').length}})
  if(!current()||!same(await this.load(s,current),v))fail('stale_source');return result
 }
 async reconcile(input:BrowserSyncScope,sequence:number):Promise<'already_applied'>{
  const s=snapshotBrowserSyncScope(input),{lease,current}=await this.guard(s),v=await this.load(s,current)
  await lease.use(async amk=>{
   const value=await this.#history(s,v,amk,current),h=value.history;await verifyHSource(s,v,h,amk,current,undefined,value.mixedValidated)
   const row=h.rows.find(r=>r.server_sequence===sequence),c=row&&h.dag.events.get(row.event_id)
   if(!row||!['note_proven','content_note_proven','note_conflict_preserved','content_note_conflict_preserved'].includes(row.state)||!c||c.record_kind!=='candidate')fail()
   let parent=c.header.parent_event_id
   while(parent){const p=h.dag.events.get(parent);if(p?.record_kind==='resolution')return;if(!p||p.note_id!==c.note_id||p.project_id!==c.project_id)fail();parent=p.header.parent_event_id}
   fail('unsupported_local_source')
  })
  if(!current()||!same(await this.load(s,current),v))fail('stale_source');return'already_applied'
 }
 async apply(s:BrowserSyncScope,sequence:number){
  const {current}=await this.guard(s),v=await this.load(s,current),row=readFutureInboxRecord(v.rows.find((r:any)=>r.server_sequence===sequence),s)
  return row.state==='received'?this.commit(await this.prepare(s,sequence)):this.reconcile(s,sequence)
 }
}
