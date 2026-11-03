import {decryptObjectBytes} from '@/crypto'
import {decodeBase64Url} from '@/api/base64url'
import {unframeContentNote} from '@/cloud/contentNoteCodec'
import {verifyContentGroup} from './contentNoteConflictProof'
import {BrowserSyncDurability,validateBrowserSealedBody} from './syncDurability'
import {inspectResolutionDag} from './resolutionDag'
import {authenticateContinuationHistory,verifyContinuationSources} from './resolutionHistorical'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import { encryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { encodeBase64Url } from '@/api/base64url'
import { frameContentNote } from '@/cloud/contentNoteCodec'
import { BrowserPersistenceError, type BrowserPersistenceCode } from './persistence'
import { localCanonicalBytes, type LocalEnvelope } from './localAuthorityRecords'
import { authenticateContentAuthorityInventory, inspectContentAuthorityInventory } from './contentAuthorityInventory'
import { authenticateContentIntent2, authenticateContentCandidate3, authenticateContentSelfEcho2 } from './contentAuthorityAuthentication'
import { type BrowserContentCandidate3, type BrowserContentReceipt2, type BrowserContentSelfEchoProof2 } from './contentAuthorityRecords'
import { historyRange, nextNoteHead, openHistoryCandidate } from './noteHistory'
import { validateNoteHistorySet } from './noteHistoryRecords'
import { noteProofDigest, verifyStoredNoteProof, type VerifiedNoteCausalPlan } from './legacyNoteApply'
import { readBrowserSyncCursor, snapshotBrowserSyncScope, immutableWireIdentity, prepareBrowserUploadAcceptance, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'
import type { BrowserPrefixInventory } from './provenPrefixRecords'
import { prepareMetadataPrefix, verifiedMetadataSequences } from './metadataPrefixProof'
import { validateBrowserMetadataHead } from './metadataApplyRecords'
import { verifyStoredContentNoteProof } from './contentNoteProof'
import { prepareNotePrerequisite } from './noteTransportPrerequisite'
import { validateBrowserProjectBinding } from './projectBindings'
const same = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b)
function fail(code: BrowserPersistenceCode = 'corrupt_record'): never { throw new BrowserPersistenceError(code) }
const stores = ['inbox', 'outbox', 'receipts', 'legacyNoteHistory', 'legacyNoteIntents', 'localNotes', 'projectBindings', 'ownerHeads', 'cursors', 'localProjects']
interface Change { echo?:boolean;scope: BrowserSyncScope; before: BrowserPrefixInventory; after: BrowserPrefixInventory; current: () => boolean }
/** Existing C3/R2/E2 lifecycle. Internal only: no transport, capability declaration or user writer grant. */
export class BrowserContentAuthorityOwner {
 private readonly echoes = new WeakMap<object, Change>()
 constructor(private readonly transaction: BrowserSyncTransaction, private readonly sessionCurrent: () => boolean,
  private readonly auth: NormalUserAuthRuntime, private readonly bindings: AuthoritativeAccountBinding, private readonly keys: RuntimeKeyContext) {}
 private async guard(s: BrowserSyncScope) {
  if (!this.sessionCurrent() || this.auth.state !== 'authenticated') fail('closed')
  const { context } = await this.bindings.ensureForCurrentUser(s.accountId), lease = this.keys.leaseForAccount(s.accountId)
  const current = () => this.sessionCurrent() && this.auth.isCurrent(context) && !!lease?.isCurrent()
  if (!lease || context.userId !== s.canonicalUserId || lease.localAccountId !== s.accountId || lease.canonicalUserId !== s.canonicalUserId || lease.authEpoch !== context.authEpoch || !current()) fail('account_mismatch')
  return { lease, current }
 }
 private read(tx: IDBTransaction, s: BrowserSyncScope, done: (v: BrowserPrefixInventory) => void, abort: (e: unknown) => void) {
  const names = ['inbox', 'outbox', 'receipts', 'legacyNoteHistory', 'legacyNoteIntents', 'localNotes', 'projectBindings', 'ownerHeads', 'localProjects']
  const rs = names.map(n => tx.objectStore(n).getAll(historyRange(s))); let count = 0
  rs.forEach(r => { r.onsuccess = () => { if (++count !== rs.length) return
   readBrowserSyncCursor(tx, s, cursor => { try { done({ rows: rs[0]!.result, out: rs[1]!.result, receipts: rs[2]!.result, history: rs[3]!.result,
    intents: rs[4]!.result, localNotes: rs[5]!.result, bindings: rs[6]!.result, owners: rs[7]!.result, projects:rs[8]!.result, cursor }) } catch (e) { abort(e) } }, abort)
  } })
 }
 private inventory(s: BrowserSyncScope) { return this.transaction<BrowserPrefixInventory>(s, stores, 'readonly', (tx, result, abort) => this.read(tx, s, result, abort)) }
 private async envelope(s: BrowserSyncScope, project: string, note: string, value: unknown, amk: AccountMasterKey): Promise<LocalEnvelope> {
  const bytes = localCanonicalBytes(value)
  try { const o = await encryptObjectBytes(amk, { userId: s.canonicalUserId, projectId: project, entityId: note, entityType: 'note' }, bytes)
   try { return { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(o.nonce), ciphertext: encodeBase64Url(o.ciphertext) } } finally { o.nonce.fill(0); o.ciphertext.fill(0) }
  } finally { bytes.fill(0) }
 }
 private sourceTip(v: BrowserPrefixInventory, c: BrowserContentCandidate3) {
  const head = v.history.find(r => (r as {record_kind:string;project_id:string;note_id:string}).record_kind === 'head' && (r as {project_id:string}).project_id === c.project_id && (r as {note_id:string}).note_id === c.note_id&&(r as {bootstrap_id:string}).bootstrap_id===c.bootstrap_id) as {linear_head_event_id:string;tip_event_ids:string[]} | undefined
  if (!head) fail('candidate_history_unavailable')
  if (head.linear_head_event_id === c.event_id && head.tip_event_ids.length === 1 && !v.localNotes?.some(n => same(n, c.intent.snapshot))&&!v.intents?.some((i:any)=>i.record_version===2&&i.parent_event_id===c.event_id&&v.localNotes?.some(n=>same(n,i.snapshot)))) fail('stale_source')
 }
 private async prerequisites(s: BrowserSyncScope, v: BrowserPrefixInventory, c: {project_id:string;note_id:string;bootstrap_id:string;dependencies:{metadata_event_id:string;stage_event_ids:string[]}}, amk: AccountMasterKey, current:()=>boolean, sequence?:number) {
  // Dormant producer supports project HTML/plain with no Stage dependency.
  if (c.dependencies.stage_event_ids.length) fail('unsupported_local_source')
  const binding = validateBrowserProjectBinding(v.bindings.find(b => (b as {project_id:string}).project_id === c.project_id), s)
  if (binding.mode !== 'import_remote' || binding.phase !== 'captured' || binding.remote_state !== 'active' || binding.bootstrap_id !== c.bootstrap_id) fail('stale_authority')
  const view={...v,rows:v.rows.filter(r=>['project_metadata','stage','stage_order'].includes(JSON.parse((r as {wire_item:string}).wire_item).event.entity_type))}
  const token = await prepareMetadataPrefix(s, view, amk, current, true), sequences = verifiedMetadataSequences(token, s, view)
  const head = validateBrowserMetadataHead(v.owners.find(o => (o as {owner_kind:string;owner_id:string}).owner_kind === 'project_metadata' && (o as {owner_id:string}).owner_id === c.project_id), s)
  const dependency = head.history.find(p => p.event_id === c.dependencies.metadata_event_id), tip = head.history.find(p => p.event_id === head.tip_event_ids[0])
  if (head.bootstrap_id !== c.bootstrap_id || head.tip_event_ids.length !== 1 || !dependency || !tip || dependency.operation === 'delete' || tip.operation === 'delete' || !sequences.has(dependency.server_sequence) || !sequences.has(tip.server_sequence) || sequence !== undefined && dependency.server_sequence >= sequence) fail('stale_authority')
  if(v.history.some(r=>(r as {record_kind:string}).record_kind==='resolution')){
   const history=await authenticateContinuationHistory(s,v,amk,current)
   await verifyContinuationSources(s,{...v,projects:v.projects!},history,amk,current)
   const parentId=(c as {parent_event_id?:string;header?:{parent_event_id:string}}).parent_event_id??(c as {header?:{parent_event_id:string}}).header?.parent_event_id
   const parent=v.rows.find(r=>(r as {event_id:string}).event_id===parentId) as {server_sequence:number}|undefined
   if(!parent||sequence!==undefined&&parent.server_sequence>=sequence)fail('missing_history')
   return
  }
  const history = validateNoteHistorySet(s, v.history)
  for (const candidate of history) if (candidate.record_kind === 'candidate' && candidate.record_version !== 3 && candidate.project_id === c.project_id && candidate.note_id === c.note_id) {
   const row = v.rows.find(r => (r as {event_id:string}).event_id === candidate.event_id) as ReturnType<typeof inspectContentAuthorityInventory>['rows'][number] | undefined
   if (!row||row.state==='note_resolution_proven') fail('candidate_history_unavailable')
   if (sequence !== undefined && row.server_sequence >= sequence) fail('missing_history')
   await openHistoryCandidate(s, candidate, amk)
   if (candidate.record_version === 2) { await verifyStoredContentNoteProof(s, amk, row, history); await prepareNotePrerequisite(s, row, binding, amk, current, undefined, v) }
   else { await verifyStoredNoteProof(s, amk, row); if (candidate.source_device_id === s.deviceId) fail('unsupported_local_source') }
  }
 }
 private commit(p: Change): Promise<boolean> {
  if (!p.current()) fail('closed')
  let unsubscribe: (() => void) | undefined
  return this.transaction<boolean|{status:'self_echo_applied'|'already_applied'}>(p.scope, stores, 'readwrite', (tx, result, abort) => {
   unsubscribe = this.auth.onInvalidated(() => abort(new BrowserPersistenceError('closed')))
   this.read(tx, p.scope, now => { try {
    if (!p.current()) fail('closed')
    if (same(now, p.after)) { result(p.echo?{status:'already_applied'}:false); return }
    if (!same(now, p.before)) fail('stale_source')
    const sets: Array<[string, unknown[], unknown[]]> = [['inbox', p.before.rows, p.after.rows], ['outbox', p.before.out, p.after.out],
     ['receipts', p.before.receipts, p.after.receipts], ['legacyNoteHistory', p.before.history, p.after.history]]
    for (const [name, before, after] of sets) for (const row of after) if (!before.some(r => same(r, row))) {
     const w = tx.objectStore(name).put(row); w.onsuccess = () => { if (!p.current()) abort(new BrowserPersistenceError('closed')) }
    }
    for (const i of p.before.intents ?? []) if (!p.after.intents?.some(n => same(n, i))) {
     const w = tx.objectStore('legacyNoteIntents').delete([p.scope.accountId, (i as { event_id: string }).event_id]); w.onsuccess = () => { if (!p.current()) abort(new BrowserPersistenceError('closed')) }
    }
    result(p.echo?{status:'self_echo_applied'}:true)
   } catch (e) { abort(e) } }, abort)
  }).then(value=>value===true||typeof value==='object'&&value.status==='self_echo_applied').finally(() => unsubscribe?.())
 }
 private publicationTransaction(s:BrowserSyncScope,before:BrowserPrefixInventory,current:()=>boolean):BrowserSyncTransaction{
  return async(scope,ns,mode,schedule)=>{
   let activeAbort:((e:unknown)=>void)|undefined
   const stop=()=>activeAbort?.(new BrowserPersistenceError('closed')),unAuth=this.auth.onInvalidated(stop),unKey=this.keys.onInvalidated(stop)
   try{return await this.transaction(scope,[...new Set([...stores,...ns])],mode,(tx,result,abort)=>{
    activeAbort=abort;this.read(tx,s,now=>{try{if(!current())fail('closed');if(!same(now,before))fail('stale_source');schedule(tx,value=>{if(!current()){abort(new BrowserPersistenceError('closed'));return}result(value)},abort)}catch(e){abort(e)}},abort)
   })}finally{activeAbort=undefined;unAuth();unKey()}
  }
 }
 /** Existing production writer handoff, not a browser Resolution writer. Only
  * the unchanged original sealed wire is retained; publication stays separate. */
 async stageResolution(scope:BrowserSyncScope,rawBody:string){
  const s=snapshotBrowserSyncScope(scope),{lease,current}=await this.guard(s),before=await this.inventory(s)
  validateBrowserSealedBody(rawBody,s)
  await lease.use(async amk=>{
   await authenticateContentAuthorityInventory(s,before,amk,current)
   const item=JSON.parse(rawBody).items[0],e=item.event,nonce=decodeBase64Url(item.object.nonce),ciphertext=decodeBase64Url(item.object.ciphertext)
   let bytes:Uint8Array|undefined
   try{
    bytes=await decryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:e.project_id,entityId:e.entity_id,entityType:'note'},{...item.object,nonce,ciphertext})
    const c=unframeContentNote(bytes),r=c.event
    if(c.account_id!==s.canonicalUserId||c.device_id!==s.deviceId||r.version!==2||e.operation!=='event'||e.entity_type!=='note'||r.header.event_id!==e.event_id||r.header.revision!==e.revision||r.header.project_id!==e.project_id||r.header.entity_id!==e.entity_id||r.header.updated_at!==e.updated_at||r.resolution.strategy!=='choose_version')fail('unsupported_local_source')
    const retained=before.history.find((v:any)=>v.record_kind==='resolution'&&v.event_id===r.header.event_id)
    if(retained){const verified=await authenticateContinuationHistory(s,before,amk,current);await verifyContinuationSources(s,{...before,projects:before.projects!},verified,amk,current);if(!same(JSON.parse((before.out.find((o:any)=>o.event_id===r.header.event_id) as {raw_body:string}).raw_body).items[0],item))fail('conflicting_record');return}
    const dag=inspectResolutionDag(s,before.history),group=dag.history.find(g=>g.record_kind==='group'&&g.project_id===e.project_id&&g.note_id===e.entity_id&&same(g.tip_event_ids,r.resolution.resolved_event_ids)&&!dag.history.some(t=>t.record_kind==='group_transition'&&t.group_id===g.group_id&&t.project_id===g.project_id&&t.note_id===g.note_id))
    if(!group||group.record_kind!=='group'||before.rows.some(v=>(v as {state:string}).state==='received'))fail('note_conflict_unproven')
    await verifyContentGroup(s,group,dag.history as never,before.rows as never,amk,{out:before.out,receipts:before.receipts})
    const selected=dag.events.get(r.resolution.selected_event_id)
    if(!selected||selected.record_kind!=='candidate'||r.header.revision!==group.tip_revision+1)fail('note_conflict_unproven')
    const original=await openHistoryCandidate(s,selected,amk)
    if(original.header.operation!==r.result.operation||!same(original.note,r.result.note)||group.bootstrap_id!==c.dependencies.bootstrap_id)fail('note_conflict_unproven')
    const view={...before,rows:before.rows.filter(v=>['project_metadata','stage','stage_order'].includes(JSON.parse((v as {wire_item:string}).wire_item).event.entity_type))}
    const token=await prepareMetadataPrefix(s,view,amk,current,true),seqs=verifiedMetadataSequences(token,s,view)
    const head=validateBrowserMetadataHead(before.owners.find((o:any)=>o.owner_kind==='project_metadata'&&o.owner_id===e.project_id),s)
    if(!head.history.some(p=>p.event_id===c.dependencies.metadata_event_id&&p.operation!=='delete'&&seqs.has(p.server_sequence))||c.dependencies.stage_event_ids.length)fail('stale_authority')
   }finally{bytes?.fill(0);nonce.fill(0);ciphertext.fill(0)}
   if(!current())fail('closed')
  })
  const transaction=this.publicationTransaction(s,before,current)
  return new BrowserSyncDurability(transaction).enqueue(s,[rawBody])
 }
 async acceptResolution(scope:BrowserSyncScope,eventId:string,response:unknown){
  const publicationResponse=structuredClone(response)
  const s=snapshotBrowserSyncScope(scope),{current}=await this.guard(s),before=await this.inventory(s)
  const out=before.out.find((o:any)=>o.event_id===eventId) as import('./syncDurability').BrowserOutboxRecord|undefined
  if(!out||before.history.some((r:any)=>r.record_kind==='candidate'&&r.event_id===eventId))fail('unsupported_local_source')
  // Reauthenticate the original sealed decision, group and source before accepting.
  await this.stageResolution(s,out.raw_body)
  const transaction=this.publicationTransaction(s,before,current)
  if(out.state==='accepted'){
   const expected=prepareBrowserUploadAcceptance(s,out,publicationResponse),old=before.receipts.find((r:any)=>r.event_id===eventId) as {record_version:number;server_sequence:number}|undefined
   if(!old||old.record_version!==1||old.server_sequence!==expected.server_sequence)fail('conflicting_record')
   return transaction<'already_accepted'>(s,stores,'readonly',(_tx,result)=>result('already_accepted'))
  }
  return new BrowserSyncDurability(transaction).commitUploadAcceptance(s,out,publicationResponse)
 }
 /** Seal only a genuine authenticated I2 prepared by the existing local mutation owner. */
 async seal(scope: BrowserSyncScope, eventId: string): Promise<'sealed' | 'already_sealed'> {
  const s = snapshotBrowserSyncScope(scope), { lease, current } = await this.guard(s), before = await this.inventory(s)
  return lease.use(async amk => {
   await authenticateContentAuthorityInventory(s, before, amk, current)
   const parsed = inspectContentAuthorityInventory(s, before), existing = parsed.candidates.find(c => c.event_id === eventId)
   if (existing) return 'already_sealed'
   const intent = parsed.intents.find(i => i.event_id === eventId)
   if (!intent || intent.record_version !== 2 || intent.seal_state !== 'pending') fail('unsupported_local_source')
   await this.prerequisites(s, before, intent, amk, current)
   const event = await authenticateContentIntent2(s, intent, amk, current)
   if (event.event.version !== 1) fail('unsupported_local_source')
   const frame = frameContentNote(event)
   let object
   try { object = await encryptObjectBytes(amk, { userId: s.canonicalUserId, projectId: intent.project_id, entityId: intent.note_id, entityType: 'note' }, frame) } finally { frame.fill(0) }
   const h = event.event.header
   let wire
   try { wire = { event: { event_id: h.event_id, project_id: h.project_id, entity_id: h.entity_id, entity_type: h.entity_type, operation: 'event', revision: h.revision, updated_at: h.updated_at, deleted_at: null },
    object: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) } } } finally { object.nonce.fill(0); object.ciphertext.fill(0) }
   const raw = JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3, device_id: s.deviceId, items: [wire] })
   const body = { record_version: 3 as const, record_kind: 'candidate' as const, record_id: eventId, account_id: s.accountId, device_id: s.deviceId,
    project_id: intent.project_id, note_id: intent.note_id, bootstrap_id: intent.bootstrap_id, event_id: eventId, source_device_id: s.deviceId,
    protocol_version: 3 as const, transport_kind: 'content_note_v1' as const, origin: 'local_sealed' as const, codec_id: 8 as const, frame_version: 1 as const, codec_version: 1 as const, compression_id: 0 as const,
    header: structuredClone(h), intent, wire_item: JSON.stringify(wire), wire_digest: await noteProofDigest({ protocol_version: 3, item: wire }), raw_body_digest: await noteProofDigest(raw), logical_digest: await noteProofDigest(event) }
   const authorship_envelope = await this.envelope(s, intent.project_id, intent.note_id, { content_outbound_format: 1, kind: 'sealed_authorship', candidate_body_digest: await noteProofDigest(body) }, amk)
   const record = { ...body, authorship_envelope }, candidate: BrowserContentCandidate3 = { ...record, record_digest: await noteProofDigest(record) }
   await authenticateContentCandidate3(s, candidate, amk, current)
   const ownHistory = parsed.history.filter(r => r.project_id === intent.project_id && r.note_id === intent.note_id), binding = before.bindings.find(b => (b as { project_id: string }).project_id === intent.project_id)
   const candidates = [...ownHistory.filter(r => r.record_kind === 'candidate'), candidate], previous = before.history.find((r):r is any => (r as any).record_kind === 'head'&&(r as any).project_id===intent.project_id&&(r as any).note_id===intent.note_id)
   const head = previous?.record_version===2?{...previous,head_revision:previous.head_revision+1,linear_head_event_id:eventId,tip_event_ids:[eventId],node_ref:{kind:'candidate',event_id:eventId}}:nextNoteHead(s, binding as Parameters<typeof nextNoteHead>[1], candidates, eventId, previous?.record_kind === 'head' ? previous : undefined)
   const after = structuredClone(before)
   after.history = [...before.history.filter(r => !same(r, previous)), candidate, head]
   after.intents = before.intents!.filter(i => (i as { event_id: string }).event_id !== eventId)
   after.out.push({ record_version: 1, account_id: s.accountId, device_id: s.deviceId, event_id: eventId, local_ordinal: intent.local_ordinal, protocol_version: 3, state: 'sealed', raw_body: raw })
   if(previous?.record_version===2)inspectResolutionDag(s,after.history);else validateNoteHistorySet(s, after.history); await authenticateContentAuthorityInventory(s, after, amk, current)
   await this.commit({ scope: s, before, after, current }); return 'sealed'
  })
 }
 /** Response from the existing authenticated uploader, exact event/sequence join. Never sends a request. */
 async accept(scope: BrowserSyncScope, eventId: string, response: unknown): Promise<'accepted' | 'already_accepted'> {
  const s = snapshotBrowserSyncScope(scope), { lease, current } = await this.guard(s), before = await this.inventory(s)
  return lease.use(async amk => {
   await authenticateContentAuthorityInventory(s, before, amk, current)
   const parsed = inspectContentAuthorityInventory(s, before), c = parsed.candidates.find(c => c.event_id === eventId), out = parsed.out.find(o => o.event_id === eventId)
   if (!c || !out) fail('self_echo_unavailable')
   const base = prepareBrowserUploadAcceptance(s, out, response), old = parsed.receipts.find(r => r.event_id === eventId)
   if (old) { if (old.server_sequence !== base.server_sequence) fail('conflicting_record'); return 'already_accepted' }
   if (before.receipts.some(r => (r as { server_sequence: number }).server_sequence === base.server_sequence)) fail('conflicting_record')
   for (const row of parsed.rows) if (row.server_sequence === base.server_sequence || row.event_id === eventId) {
    const wire = JSON.parse(row.wire_item), { device_id, server_sequence, ...event } = wire.event
    if (row.event_id !== eventId || server_sequence !== base.server_sequence || device_id !== s.deviceId || !same({ event, object: wire.object }, JSON.parse(c.wire_item))) fail('conflicting_record')
   }
   const body = { record_version: 2 as const, owner_kind: 'content_note_acceptance' as const, account_id: s.accountId, device_id: s.deviceId, event_id: eventId,
    protocol_version: 3 as const, server_sequence: base.server_sequence, duplicate: base.duplicate, candidate_digest: c.record_digest, wire_digest: c.wire_digest,
    acceptance_envelope: await this.envelope(s, c.project_id, c.note_id, { content_receipt_format: 1, receipt: base, candidate_digest: c.record_digest, wire_digest: c.wire_digest }, amk) }
   const receipt: BrowserContentReceipt2 = { ...body, receipt_digest: await noteProofDigest(body) }, after = structuredClone(before)
   after.out = before.out.map(o => (o as { event_id: string }).event_id === eventId ? { ...out, state: 'accepted' } : o); after.receipts.push(receipt)
   await authenticateContentAuthorityInventory(s, after, amk, current)
   await this.commit({ scope: s, before, after, current }); return 'accepted'
  })
 }
 async prepareEcho(scope: BrowserSyncScope, sequence: number): Promise<VerifiedNoteCausalPlan | null> {
  const s = snapshotBrowserSyncScope(scope), { lease, current } = await this.guard(s), before = await this.inventory(s)
  const row = before.rows.find(r => (r as { server_sequence: number }).server_sequence === sequence) as ReturnType<typeof inspectContentAuthorityInventory>['rows'][number] | undefined
  if (!row) fail('missing_history')
  const wire = JSON.parse(row.wire_item)
  if (wire.event.device_id !== s.deviceId) return null
  if (row.protocol_version !== 3 || wire.event.entity_type !== 'note' || wire.event.operation !== 'event') fail('unsupported_local_source')
  return lease.use(async amk => {
   await authenticateContentAuthorityInventory(s, before, amk, current)
   const parsed = inspectContentAuthorityInventory(s, before), c = parsed.candidates.find(c => c.event_id === row.event_id), r = parsed.receipts.find(r => r.event_id === row.event_id), out = parsed.out.find(o => o.event_id === row.event_id)
   if (!c) fail('unsupported_local_source')
   if (!r || !out) fail('self_echo_unavailable')
   this.sourceTip(before, c)
   await this.prerequisites(s, before, { ...c, dependencies: c.intent.dependencies }, amk, current, sequence)
   if (sequence > before.cursor.pull_cursor || !['received', 'content_note_self_echo_proven'].includes(row.state)) fail('conflicting_record')
   const plain = await authenticateContentCandidate3(s, c, amk, current)
   const body = { record_version: 2 as const, owner_kind: 'content_note_self_echo' as const, outcome: 'acknowledged' as const, account_id: s.accountId, device_id: s.deviceId,
    bootstrap_id: c.bootstrap_id, protocol_version: 3 as const, reference: wire.event, server_sequence: sequence, codec_id: 8 as const, frame_version: 1 as const, codec_version: 1 as const,
    header: c.header, mutation: plain.mutation, history_event_id: c.event_id, candidate_digest: c.record_digest, wire_digest: c.wire_digest, receipt_digest: r.receipt_digest }
   const proof: BrowserContentSelfEchoProof2 = { ...body, outcome_digest: await noteProofDigest(body) }
   const proven = { ...row, record_version: 9 as const, state: 'content_note_self_echo_proven' as const, content_note_self_echo_proof: proof }
   await authenticateContentSelfEcho2(s, proven, c, r, out, amk, current)
   const after = structuredClone(before); after.rows = before.rows.map(v => same(v, row) ? proven : v)
   const token = Object.freeze({}) as VerifiedNoteCausalPlan; this.echoes.set(token, { echo:true,scope: s, before, after, current }); return token
  })
 }
 owns(token: VerifiedNoteCausalPlan) { return this.echoes.has(token) }
 async commitEcho(token: VerifiedNoteCausalPlan): Promise<'self_echo_applied' | 'already_applied'> {
  const p = this.echoes.get(token); if (!p) fail('stale_source')
  return await this.commit(p) ? 'self_echo_applied' : 'already_applied'
 }
 /** Re-authenticate the retained encrypted evidence after reopen; no ACK or readiness grant. */
 async authenticate(scope: BrowserSyncScope) {
  const s = snapshotBrowserSyncScope(scope), { lease, current } = await this.guard(s), v = await this.inventory(s)
  await lease.use(async amk => {
   await authenticateContentAuthorityInventory(s, v, amk, current)
   for (const c of inspectContentAuthorityInventory(s, v).candidates) {
    this.sourceTip(v, c)
    const row = v.rows.find(r => (r as {event_id:string}).event_id === c.event_id) as {server_sequence:number} | undefined
    await this.prerequisites(s, v, {...c, dependencies:c.intent.dependencies}, amk, current, row?.server_sequence)
   }
  })
 }
}
