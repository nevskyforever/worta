import {verifiedV10OrdinaryForLocal,type V10OrdinaryToken} from './v10OrdinaryApply'
import {verifiedContinuationForLocal,type ResolutionContinuationToken} from './resolutionContinuation'
import type {NoteSyncPlaintext} from '@/cloud/noteSyncCodec'
import {verifiedResolutionForLocal,type ResolutionApplyToken} from './resolutionPlanner'
import type { NoteMutationPlan } from './noteIntents'
import {validateNoteIntentRecord} from './noteIntentRecords'
import {validateNoteHistorySet} from './noteHistoryRecords'
import { currentLegacyNoteForLocal, verifiedLegacyNoteForLocal, verifiedNoteRouteForLocal, type VerifiedLegacyNotePlan } from './legacyNoteApply'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { AuthoritativeKeyContextLease, RuntimeKeyContext } from '@/auth/keyContext'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import { decryptObjectBytes, encryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { decodeBase64Url, encodeBase64Url } from '@/api/base64url'
import { canonical, timestamp } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { immutableWireIdentity, snapshotBrowserSyncScope, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'
import { localCanonicalBytes, localExact, localFailure as fail, localId, localSafe, MAX_LOCAL_NOTES, validateContentLocalNote, validatePersistedLocalNote, validateLocalNote, validateLocalNoteRecord, validateLocalProject, validateLocalProjectRecord, type LocalEnvelope, type LocalNotePayload, type LocalNoteRecord, type LocalNoteSource, type LocalProjectPayload, type LocalProjectRecord, type NativeLegacyNote, type NativeLocalProject } from './localAuthorityRecords'
import type { ProjectNotePatch } from '@/types/notes'

export interface LocalSourceToken { readonly verifiedLocalSource: unique symbol }
/** Internal source/intent transaction hook. Binding a writer never activates user composition. */
export interface LocalNoteMutationWriter {
  readonly transportKind?: 'content_note_v1'
  readonly mutationStores: string[]
  prepareMutation(scope: BrowserSyncScope, notes: LocalNoteRecord[], amk: AccountMasterKey, source: LocalSourceToken): Promise<NoteMutationPlan>
  scheduleMutation(tx: IDBTransaction, plan: NoteMutationPlan, ready: () => void, abort: (e: unknown) => void): void
}
export interface LocalSourceSnapshot {
  record_version: 1; account_id: string; project_id: string; source_revision: number
  project: LocalProjectRecord; notes: readonly LocalNoteRecord[]; token: LocalSourceToken
}
interface SourceProof { owner: BrowserLocalAuthority; scope: BrowserSyncScope; project: LocalProjectRecord; notes: LocalNoteRecord[]; current: () => boolean }
interface Guard { lease: AuthoritativeKeyContextLease; current: () => boolean }
interface Opened { project: LocalProjectPayload; notes: LocalNoteSource[] }
interface PeerSourceTransition { project: LocalProjectPayload; note: LocalNoteSource; sourceRevision: number }
const proofs = new WeakMap<object, SourceProof>()
const same = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b)
const noteRange = (s: BrowserSyncScope, p: string) => IDBKeyRange.bound([s.accountId, p], [s.accountId, p, []])
const order = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0
function freeze<T>(value: T): T {
  if (value && typeof value === 'object') { Object.values(value).forEach(freeze); Object.freeze(value) }
  return value
}
function proof(scope: BrowserSyncScope, token: LocalSourceToken): SourceProof {
  const p = token && proofs.get(token)
  if (!p || !same(p.scope, scope)) fail('stale_source')
  if (!p.current()) fail('closed')
  return p
}

/** Shared local-source envelope for the private replay seed and existing local owner. */
export async function sealEmptyLocalProject(scope: BrowserSyncScope, project: NativeLocalProject, amk: AccountMasterKey): Promise<LocalProjectRecord> {
  const checked = validateLocalProject(project)
  const payload: LocalProjectPayload = { local_format: 1, kind: 'project', account_id: scope.accountId, device_id: scope.deviceId,
    project_id: checked.id, source_revision: 1, deleted_at: null, project: checked, member_ids: [] }
  const bytes = localCanonicalBytes(payload)
  let nonce: Uint8Array | undefined, ciphertext: Uint8Array | undefined
  try {
    const sealed = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: checked.id,
      entityId: checked.id, entityType: 'project_metadata' }, bytes)
    nonce = sealed.nonce; ciphertext = sealed.ciphertext
    return { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, project_id: checked.id,
      source_revision: 1, deleted: false, envelope: { crypto_version: 1, aad_version: 1,
        nonce: encodeBase64Url(nonce), ciphertext: encodeBase64Url(ciphertext) } }
  } finally { bytes.fill(0); nonce?.fill(0); ciphertext?.fill(0) }
}

/** Reauthenticate a cache-only seed without exposing it through local CRUD. */
export async function openEmptyLocalProject(scope: BrowserSyncScope, raw: unknown, amk: AccountMasterKey): Promise<NativeLocalProject> {
  const row = validateLocalProjectRecord(raw, scope)
  if (row.deleted || row.source_revision !== 1) fail('corrupt_record')
  let nonce: Uint8Array | undefined, ciphertext: Uint8Array | undefined, bytes: Uint8Array | undefined
  try {
    nonce = decodeBase64Url(row.envelope.nonce); ciphertext = decodeBase64Url(row.envelope.ciphertext)
    bytes = await decryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: row.project_id,
      entityId: row.project_id, entityType: 'project_metadata' }, { ...row.envelope, nonce, ciphertext })
    const rawPayload = new TextDecoder('utf-8', { fatal: true }).decode(bytes)
    const payload: unknown = JSON.parse(rawPayload)
    if (canonical(payload) !== rawPayload || !localExact(payload, ['local_format','kind','account_id','device_id','project_id','source_revision','deleted_at','project','member_ids'])) fail('corrupt_record')
    const p = payload as unknown as LocalProjectPayload
    if (p.local_format !== 1 || p.kind !== 'project' || p.account_id !== scope.accountId || p.device_id !== scope.deviceId
      || p.project_id !== row.project_id || p.source_revision !== 1 || p.deleted_at !== null || !same(p.member_ids, [])) fail('corrupt_record')
    const project = validateLocalProject(p.project)
    if (project.id !== row.project_id) fail('corrupt_record')
    return project
  } catch { return fail('corrupt_record') }
  finally { nonce?.fill(0); ciphertext?.fill(0); bytes?.fill(0) }
}

/** Pure local projection calculation. A caller must still prove its source and Note authority. */
function planPeerSourceTransition(scope: BrowserSyncScope, opened: Opened, sourceRevision: number, incoming: NoteSyncPlaintext, content: boolean): PeerSourceTransition {
  const before = opened.project
  if (!localSafe(sourceRevision, 1) || sourceRevision === Number.MAX_SAFE_INTEGER) fail('source_limit')
  if (before.local_format !== 1 || before.kind !== 'project' || before.account_id !== scope.accountId || before.device_id !== scope.deviceId
    || !before.project || before.project_id !== before.project.id || before.source_revision !== sourceRevision || before.deleted_at !== null) fail('stale_source')
  if (opened.notes.length > MAX_LOCAL_NOTES || !Array.isArray(before.member_ids) || before.member_ids.length > MAX_LOCAL_NOTES) fail('source_limit')
  const project = validateLocalProject(before.project)
  const notes = opened.notes.map(note => validatePersistedLocalNote(note, project.id))
  const members = notes.map(note => note.note.id).sort(order)
  if (!same(before.member_ids, members) || members.some((id, index) => index > 0 && id === members[index - 1])) fail('corrupt_record')
  if (!incoming || !['create', 'update', 'delete'].includes(incoming.mutation) || incoming.header?.project_id !== project.id
    || incoming.header.entity_id !== incoming.note?.id || incoming.note?.project_id !== project.id) fail('invalid_local_source')
  // Native peer payload has local mutation revision 0; cloud revision stays in the Note proof.
  const admit = content ? validateContentLocalNote : validateLocalNote
  const note = admit(incoming.mutation === 'delete'
    ? { state: 'tombstone', note: incoming.note, revision: 0 }
    : { state: 'live', note: { ...incoming.note, revision: 0 } }, project.id)
  const old = notes.findIndex(value => value.note.id === note.note.id)
  if (old < 0) notes.push(note); else notes[old] = note
  if (notes.length > MAX_LOCAL_NOTES) fail('source_limit')
  const revision = sourceRevision + 1
  return { project: { ...before, source_revision: revision, project, member_ids: notes.map(value => value.note.id).sort(order) }, note, sourceRevision: revision }
}

/** Future capture infrastructure: synchronous requests in its SAME source+outbox transaction. No raw put API. */
export function assertLocalSourceInTransaction(tx: IDBTransaction, scope: BrowserSyncScope, token: LocalSourceToken, ready: () => void, abort: (error: unknown) => void): void {
  try {
    const p = proof(scope, token)
    const project = tx.objectStore('localProjects').get([scope.accountId, p.project.project_id])
    const notes = tx.objectStore('localNotes').getAll(noteRange(scope, p.project.project_id))
    let count = 0
    const finish = () => {
      if (++count !== 2) return
      try {
        if (!p.current()) fail('closed')
        const current = validateLocalProjectRecord(project.result, scope)
        const rows = notes.result.map(value => validateLocalNoteRecord(value, scope, p.project.project_id)).sort((a, b) => order(a.note_id, b.note_id))
        if (current.deleted) fail('deleted_local_source')
        if (current.source_revision !== p.project.source_revision || !same(current, p.project) || !same(rows, p.notes)) fail('stale_source')
        ready()
      } catch (error) { abort(error) }
    }
    project.onsuccess = finish; notes.onsuccess = finish
  } catch (error) { abort(error) }
}

export interface ProtectedNoteSourcePlan { readonly protectedNoteSource: unique symbol }
interface ProtectedChange {scope:BrowserSyncScope;source:LocalSourceToken;project:LocalProjectRecord;note:LocalNoteRecord;current:()=>boolean}
const protectedChanges=new WeakMap<object,ProtectedChange>()
/** Internal verified Note capability only; encrypted writes share the inbox-proof transaction. */
export function scheduleProtectedNoteApply(tx:IDBTransaction,scope:BrowserSyncScope,token:ProtectedNoteSourcePlan,ready:()=>void,abort:(e:unknown)=>void):void {
  const p=protectedChanges.get(token)
  if(!p||!same(p.scope,scope)||!p.current()){abort(new BrowserPersistenceError('stale_source'));return}
  assertLocalSourceInTransaction(tx,scope,p.source,()=>{
    const identity=tx.objectStore('localNotes').index('identity').get([scope.accountId,p.note.note_id])
    identity.onsuccess=()=>{try{if(!p.current())fail('closed');if(identity.result&&validateLocalNoteRecord(identity.result,scope,identity.result.project_id).project_id!==p.project.project_id)fail('conflicting_record')
      const writes=[tx.objectStore('localProjects').put(p.project),tx.objectStore('localNotes').put(p.note)]
      writes.forEach(r=>r.onsuccess=()=>{if(!p.current())abort(new BrowserPersistenceError('closed'))});ready()
    }catch(e){abort(e)}}
  },abort)
}

export interface ProtectedResolutionSourcePlan {readonly protectedResolutionSource:unique symbol}
interface ResolutionSourceChange {scope:BrowserSyncScope;source:LocalSourceToken;project:LocalProjectRecord;notes:LocalNoteRecord[];original:string;current:()=>boolean}
const resolutionChanges=new WeakMap<object,ResolutionSourceChange>()
/** All reads finish before beforeWrites; paired source writes cannot be separated. */
export function scheduleProtectedResolutionApply(tx:IDBTransaction,scope:BrowserSyncScope,token:ProtectedResolutionSourcePlan,beforeWrites:()=>void,afterWrites:()=>void,abort:(e:unknown)=>void):void {
 const p=resolutionChanges.get(token)
 if(!p||!same(p.scope,scope)||!p.current()){abort(new BrowserPersistenceError('stale_source'));return}
 assertLocalSourceInTransaction(tx,scope,p.source,()=>{
  let left=p.notes.length
  for(const note of p.notes){
   const request=tx.objectStore('localNotes').index('identity').get([scope.accountId,note.note_id])
   request.onsuccess=()=>{try{
    if(!p.current())fail('closed')
    if(note.note_id===p.original){if(!request.result||request.result.project_id!==p.project.project_id)fail('stale_source')}
    else if(request.result)fail('conflicting_record')
    if(--left)return
    beforeWrites()
    const original=p.notes.find(n=>n.note_id===p.original)!,clone=p.notes.find(n=>n.note_id!==p.original)
    const requests=[tx.objectStore('localNotes').put(original)]
    if(clone)requests.push(tx.objectStore('localNotes').add(clone))
    requests.push(tx.objectStore('localProjects').put(p.project))
    requests.forEach(r=>r.onsuccess=()=>{if(!p.current())abort(new BrowserPersistenceError('closed'))})
    afterWrites()
   }catch(e){abort(e)}}
  }
 },abort)
}

/** Dormant entry point. No UI/HTTP fallback; mutation runtime requires existing normal-user key context. */
export class BrowserLocalAuthority {
  constructor(private readonly transaction: BrowserSyncTransaction) {}
  bind(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext): BrowserLocalAuthorityRuntime {
    return new BrowserLocalAuthorityRuntime(this, this.transaction, auth, bindings, keys)
  }
}
export class BrowserLocalAuthorityRuntime {
  constructor(private readonly owner: BrowserLocalAuthority, private readonly transaction: BrowserSyncTransaction, private readonly auth: NormalUserAuthRuntime, private readonly bindings: AuthoritativeAccountBinding, private readonly keys: RuntimeKeyContext, private readonly writer?: LocalNoteMutationWriter) {}
  /** Internal dormant writer binding; ordinary local-only runtime remains unchanged. */
  withNoteWriter(writer: LocalNoteMutationWriter): BrowserLocalAuthorityRuntime { return new BrowserLocalAuthorityRuntime(this.owner,this.transaction,this.auth,this.bindings,this.keys,writer) }
  private async guard(scope: BrowserSyncScope): Promise<Guard> {
    const { context } = await this.bindings.ensureForCurrentUser(scope.accountId)
    const lease = this.keys.leaseForAccount(scope.accountId)
    const current = () => this.auth.isCurrent(context) && !!lease?.isCurrent()
    if (!lease || context.userId !== scope.canonicalUserId || lease.localAccountId !== scope.accountId || lease.canonicalUserId !== context.userId || lease.authEpoch !== context.authEpoch || !current()) throw new BrowserPersistenceError('account_mismatch')
    return { lease, current }
  }
  private token(scope: BrowserSyncScope, token: LocalSourceToken): SourceProof {
    const p = proof(scope, token)
    if (p.owner !== this.owner) fail('stale_source')
    return p
  }
  private load(scope: BrowserSyncScope, id: string): Promise<{ project: LocalProjectRecord; notes: LocalNoteRecord[] }> {
    return this.transaction(scope, ['localProjects','localNotes'], 'readonly', (tx, result, abort) => {
      const project = tx.objectStore('localProjects').get([scope.accountId, id]), notes = tx.objectStore('localNotes').getAll(noteRange(scope, id))
      let count = 0
      const finish = () => { if (++count !== 2) return; try {
        if (!project.result) fail(notes.result.length ? 'corrupt_record' : 'missing_local_source')
        const p = validateLocalProjectRecord(project.result, scope)
        if (p.deleted) fail('deleted_local_source')
        if (notes.result.length > MAX_LOCAL_NOTES) fail('source_limit')
        result({ project: p, notes: notes.result.map(value => validateLocalNoteRecord(value, scope, id)).sort((a, b) => order(a.note_id, b.note_id)) })
      } catch (error) { abort(error) } }
      project.onsuccess = finish; notes.onsuccess = finish
    })
  }
  /** Enumerate only this account's encrypted local projects; names never enter an index. */
  async listProjects(scope: BrowserSyncScope): Promise<NativeLocalProject[]> {
    scope = snapshotBrowserSyncScope(scope)
    const guard = await this.guard(scope)
    const rows = await this.transaction<LocalProjectRecord[]>(scope, ['localProjects'], 'readonly', (tx, result, abort) => {
      const request = tx.objectStore('localProjects').getAll(IDBKeyRange.bound([scope.accountId], [scope.accountId, []]))
      request.onsuccess = () => { try { result(request.result.map(row => validateLocalProjectRecord(row, scope))) } catch (error) { abort(error) } }
    })
    const projects: NativeLocalProject[] = []
    for (const row of rows) {
      if (row.deleted) continue
      const snapshot = await this.readSnapshot(scope, row.project_id)
      const project = await this.withDecryptedSnapshot(scope, snapshot.token, async source => structuredClone(source.project))
      projects.push(project)
    }
    if (!guard.current()) fail('closed')
    return projects
  }
  private async seal(scope: BrowserSyncScope, id: string, kind: 'project' | 'note', payload: LocalProjectPayload | LocalNotePayload, amk: AccountMasterKey): Promise<LocalEnvelope> {
    const bytes = localCanonicalBytes(payload)
    let nonce: Uint8Array | undefined, ciphertext: Uint8Array | undefined
    try {
      const e = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: payload.project_id, entityId: id, entityType: kind === 'project' ? 'project_metadata' : 'note' }, bytes)
      nonce = e.nonce; ciphertext = e.ciphertext
      return { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(nonce), ciphertext: encodeBase64Url(ciphertext) }
    } finally { bytes.fill(0); nonce?.fill(0); ciphertext?.fill(0) }
  }
  private async decode(scope: BrowserSyncScope, row: LocalProjectRecord | LocalNoteRecord, kind: 'project' | 'note', amk: AccountMasterKey): Promise<unknown> {
    let nonce: Uint8Array | undefined, ciphertext: Uint8Array | undefined, bytes: Uint8Array | undefined
    try {
      nonce = decodeBase64Url(row.envelope.nonce); ciphertext = decodeBase64Url(row.envelope.ciphertext)
      bytes = await decryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: row.project_id, entityId: 'note_id' in row ? row.note_id : row.project_id, entityType: kind === 'project' ? 'project_metadata' : 'note' }, { crypto_version: 1, aad_version: 1, nonce, ciphertext })
      const raw = new TextDecoder('utf-8', { fatal: true }).decode(bytes), value: unknown = JSON.parse(raw)
      if (canonical(value) !== raw) fail('corrupt_record')
      return value
    } catch { fail('corrupt_record') }
    finally { bytes?.fill(0); nonce?.fill(0); ciphertext?.fill(0) }
  }
  private async open(scope: BrowserSyncScope, p: LocalProjectRecord, rows: readonly LocalNoteRecord[], amk: AccountMasterKey): Promise<Opened> {
    const value = await this.decode(scope, p, 'project', amk)
    if (!localExact(value, ['local_format','kind','account_id','device_id','project_id','source_revision','deleted_at','project','member_ids'])) fail('corrupt_record')
    const project = value as unknown as LocalProjectPayload
    if (project.local_format !== 1 || project.kind !== 'project' || project.account_id !== scope.accountId || project.device_id !== scope.deviceId || project.project_id !== p.project_id || project.source_revision !== p.source_revision || project.deleted_at !== null || !Array.isArray(project.member_ids)
      || !same(project.member_ids, rows.map(r => r.note_id))) fail('corrupt_record')
    project.project = validateLocalProject(project.project)
    if (project.project.id !== p.project_id) fail('corrupt_record')
    const notes: LocalNoteSource[] = []
    for (const row of rows) {
      const raw = await this.decode(scope, row, 'note', amk)
      if (!localExact(raw, ['local_format','kind','account_id','device_id','project_id','note_id','source_revision','source'])) fail('corrupt_record')
      const note = raw as unknown as LocalNotePayload
      if (note.local_format !== 1 || note.kind !== 'note' || note.account_id !== scope.accountId || note.device_id !== scope.deviceId || note.project_id !== p.project_id || note.note_id !== row.note_id || note.source_revision !== row.source_revision || row.source_revision > p.source_revision) fail('corrupt_record')
      const source = validatePersistedLocalNote(note.source, p.project_id)
      if (source.note.id !== row.note_id || (source.state === 'tombstone') !== row.deleted || (source.state === 'live' ? source.note.revision : source.revision) !== row.note_revision) fail('corrupt_record')
      notes.push(source)
    }
    return { project, notes }
  }
  async readSnapshot(scope: BrowserSyncScope, projectId: string): Promise<LocalSourceSnapshot> {
    scope = snapshotBrowserSyncScope(scope); localId(projectId)
    const guard = await this.guard(scope), stored = await this.load(scope, projectId)
    await guard.lease.use(async amk => { await this.open(scope, stored.project, stored.notes, amk) })
    if (!guard.current()) fail('closed')
    const token = Object.freeze({}) as LocalSourceToken
    proofs.set(token, { owner: this.owner, scope, project: structuredClone(stored.project), notes: structuredClone(stored.notes), current: guard.current })
    return freeze({ record_version: 1, account_id: scope.accountId, project_id: projectId, source_revision: stored.project.source_revision, ...stored, token })
  }
  async withDecryptedSnapshot<T>(scope: BrowserSyncScope, token: LocalSourceToken, consume: (source: { project: NativeLocalProject; notes: readonly LocalNoteSource[]; source_revision: number }) => Promise<T>): Promise<T> {
    scope = snapshotBrowserSyncScope(scope)
    const p = this.token(scope, token), guard = await this.guard(scope)
    return guard.lease.use(async amk => {
      const source = await this.open(scope, p.project, p.notes, amk)
      if (!guard.current() || !p.current()) fail('closed')
      const result = await consume(freeze({ project: source.project.project, notes: source.notes, source_revision: p.project.source_revision }))
      if (!guard.current() || !p.current()) fail('closed')
      return result
    })
  }
  async assertSnapshot(scope: BrowserSyncScope, token: LocalSourceToken): Promise<void> {
    scope = snapshotBrowserSyncScope(scope); this.token(scope, token)
    await this.transaction<void>(scope, ['localProjects','localNotes'], 'readonly', (tx, result, abort) => assertLocalSourceInTransaction(tx, scope, token, () => result(undefined), abort))
  }
  initializeProject(scope: BrowserSyncScope, input: { project: NativeLocalProject; notes: readonly LocalNoteSource[] }): Promise<number> {
    return this.initialize(scope, input, true)
  }
  private async initialize(scope: BrowserSyncScope, input: { project: NativeLocalProject; notes: readonly LocalNoteSource[] }, retry: boolean): Promise<number> {
    scope = snapshotBrowserSyncScope(scope)
    if (!localExact(input, ['project','notes']) || !Array.isArray(input.notes) || input.notes.length > MAX_LOCAL_NOTES) fail()
    const project = validateLocalProject(input.project), sources = input.notes.map(n => validateLocalNote(n, project.id)).sort((a, b) => order(a.note.id, b.note.id))
    if (sources.some((n, i) => i > 0 && sources[i - 1]!.note.id === n.note.id)) fail('conflicting_record')
    if (retry) {
      let prior: LocalSourceSnapshot | undefined
      try { prior = await this.readSnapshot(scope, project.id) } catch (e) { if (!(e instanceof BrowserPersistenceError) || e.code !== 'missing_local_source') throw e }
      if (prior) {
        const matches = await this.withDecryptedSnapshot(scope, prior.token, async source => source.source_revision === 1 && same(source.project, project) && same(source.notes, sources))
        if (!matches) fail('conflicting_record')
        await this.assertSnapshot(scope, prior.token)
        return 1
      }
    }
    const guard = await this.guard(scope)
    return guard.lease.use(async amk => {
      const payload: LocalProjectPayload = { local_format: 1, kind: 'project', account_id: scope.accountId, device_id: scope.deviceId, project_id: project.id, source_revision: 1, deleted_at: null, project, member_ids: sources.map(n => n.note.id) }
      const p: LocalProjectRecord = sources.length === 0 ? await sealEmptyLocalProject(scope, project, amk)
        : { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, project_id: project.id, source_revision: 1,
          deleted: false, envelope: await this.seal(scope, project.id, 'project', payload, amk) }
      const notes: LocalNoteRecord[] = []
      for (const source of sources) notes.push(await this.noteRow(scope, source, 1, amk))
      return this.write(scope, guard, null, p, notes)
    })
  }
  createProject(scope: BrowserSyncScope, project: NativeLocalProject): Promise<number> { return this.initialize(scope, { project, notes: [] }, false) }
  private async noteRow(scope: BrowserSyncScope, source: LocalNoteSource, revision: number, amk: AccountMasterKey): Promise<LocalNoteRecord> {
    return { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, project_id: source.note.project_id, note_id: source.note.id, source_revision: revision, note_revision: source.state === 'live' ? source.note.revision : source.revision, deleted: source.state === 'tombstone', envelope: await this.seal(scope, source.note.id, 'note', { local_format: 1, kind: 'note', account_id: scope.accountId, device_id: scope.deviceId, project_id: source.note.project_id, note_id: source.note.id, source_revision: revision, source }, amk) }
  }
  private write(scope: BrowserSyncScope, guard: Guard, expected: LocalSourceToken | null, project: LocalProjectRecord, notes: LocalNoteRecord[], deletion = false, mutation?: NoteMutationPlan): Promise<number> {
    if (!guard.current()) fail('closed')
    let unsubscribe: (() => void) | undefined
    return this.transaction<number>(scope, [...new Set(['localProjects','localNotes','legacyNoteIntents','legacyNoteHistory', ...(deletion ? ['projectBindings', 'localStages', 'localDocuments'] : []), ...(mutation ? this.writer!.mutationStores : [])])], 'readwrite', (tx, result, abort) => {
      unsubscribe = this.auth.onInvalidated(() => abort(new BrowserPersistenceError('closed')))
      const commit = () => {
        const reads = notes.map(note => tx.objectStore('localNotes').index('identity').get([scope.accountId, note.note_id]))
        const binding = deletion ? tx.objectStore('projectBindings').get([scope.accountId, project.project_id]) : null
        const stages = deletion ? tx.objectStore('localStages').count(IDBKeyRange.bound([scope.accountId, project.project_id], [scope.accountId, project.project_id, []])) : null
        const documents = deletion ? tx.objectStore('localDocuments').count(IDBKeyRange.bound([scope.accountId, project.project_id], [scope.accountId, project.project_id, []])) : null
        const requests = [...reads, ...(binding ? [binding] : []), ...(stages ? [stages] : []), ...(documents ? [documents] : [])]
        let ready = 0
        const finish = () => { if (++ready < requests.length) return; try {
          if (!guard.current()) fail('closed')
          if (binding?.result) fail('unsupported_local_source') // No cloud deletion/bootstrap semantics here.
          if (stages?.result) fail('unsupported_local_source') // A project with stages cannot become an orphaned source.
          if (documents?.result) fail('unsupported_local_source') // A manuscript must not be orphaned.
          reads.forEach(read => { if (read.result) { const existing = validateLocalNoteRecord(read.result, scope, read.result.project_id); if (existing.project_id !== project.project_id) fail('conflicting_record') } })
          const persist = () => {
          const writes = [tx.objectStore('localProjects').put(project), ...notes.map(n => tx.objectStore('localNotes')[expected ? 'put' : 'add'](n))]
          writes.forEach(write => { write.onsuccess = () => { if (!guard.current()) abort(new BrowserPersistenceError('closed')) } })
          result(project.source_revision)
          }
          if(mutation)this.writer!.scheduleMutation(tx,mutation,persist,abort)
          else {
            // Source-only access cannot overwrite a Content writer's canonical Note.
            const range=IDBKeyRange.bound([scope.accountId],[scope.accountId,[]])
            const intents=tx.objectStore('legacyNoteIntents').getAll(range),history=tx.objectStore('legacyNoteHistory').getAll(range);let n=0
            const check=()=>{if(++n!==2)return;try{
              if(!guard.current())fail('closed')
              const claimed=new Set([...intents.result.map(v=>validateNoteIntentRecord(v,scope)).filter(i=>i.record_version===2).map(i=>i.note_id),...validateNoteHistorySet(scope,history.result).filter(h=>h.record_kind==='candidate'&&h.record_version===3).map(h=>h.note_id)])
              if(notes.some(note=>claimed.has(note.note_id)))fail('unsupported_local_source')
              persist()
            }catch(e){abort(e)}}
            intents.onsuccess=check;history.onsuccess=check
          }
        } catch (error) { abort(error) } }
        if (!requests.length) finish(); else requests.forEach(read => { read.onsuccess = finish })
      }
      if (expected) assertLocalSourceInTransaction(tx, scope, expected, commit, abort)
      else {
        const p = tx.objectStore('localProjects').get([scope.accountId, project.project_id]), members = tx.objectStore('localNotes').count(noteRange(scope, project.project_id))
        let count = 0
        const finish = () => { if (++count !== 2) return; try { if (p.result || members.result) fail('conflicting_record'); commit() } catch (e) { abort(e) } }
        p.onsuccess = finish; members.onsuccess = finish
      }
    }).finally(() => unsubscribe?.())
  }
  private async mutate(scope: BrowserSyncScope, token: LocalSourceToken, action: (opened: Opened) => LocalNoteSource[] | void, deletedAt: string | null = null): Promise<number> {
    scope = snapshotBrowserSyncScope(scope)
    const p = this.token(scope, token), guard = await this.guard(scope)
    if (p.project.source_revision === Number.MAX_SAFE_INTEGER) fail('source_limit')
    return guard.lease.use(async amk => {
      const opened = await this.open(scope, p.project, p.notes, amk)
      if(this.writer&&opened.project.project.status==='завершен')fail('unsupported_local_source') // Native direct Note writer keeps completed projects readonly.
      const changed = action(opened) ?? []
      const revision = p.project.source_revision + 1
      opened.project.source_revision = revision; opened.project.deleted_at = deletedAt
      opened.project.member_ids = opened.notes.map(n => n.note.id).sort(order)
      if (opened.project.member_ids.length > MAX_LOCAL_NOTES) fail('source_limit')
      const row: LocalProjectRecord = { ...p.project, source_revision: revision, deleted: deletedAt !== null, envelope: await this.seal(scope, p.project.project_id, 'project', opened.project, amk) }
      const notes: LocalNoteRecord[] = []
      for (const source of changed) notes.push(await this.noteRow(scope, source, revision, amk))
      const mutation=this.writer ? await this.writer.prepareMutation(scope,notes,amk,token) : undefined
      return this.write(scope, guard, token, row, notes, deletedAt !== null, mutation)
    })
  }
  async prepareProtectedNoteApply(scope:BrowserSyncScope,token:LocalSourceToken,verified:VerifiedLegacyNotePlan):Promise<{token:ProtectedNoteSourcePlan;source_revision:number;note:LocalNoteRecord}> {
    return this.preparePeerSource(scope,token,verifiedLegacyNoteForLocal(verified,this,scope),verifiedNoteRouteForLocal(verified,this,scope)==='content_note_v1',()=>currentLegacyNoteForLocal(verified,this,scope))
  }
  /** Narrow dormant handoff; cannot be called with a caller-provided plaintext DTO. */
  async prepareProtectedContinuationApply(scope:BrowserSyncScope,token:LocalSourceToken,verified:ResolutionContinuationToken){
    const authority=verifiedContinuationForLocal(verified,this,scope)
    return this.preparePeerSource(scope,token,authority.plaintext,authority.content,authority.current)
  }
  /** O private owner handoff. N diagnostics and caller plaintext cannot authorize it. */
  async prepareProtectedV10OrdinaryApply(scope:BrowserSyncScope,token:LocalSourceToken,verified:V10OrdinaryToken){
    const authority=verifiedV10OrdinaryForLocal(verified,this,scope)
    return this.preparePeerSource(scope,token,authority.plaintext,authority.content,authority.current)
  }
  private async preparePeerSource(scope:BrowserSyncScope,token:LocalSourceToken,incoming:NoteSyncPlaintext,content:boolean,authorityCurrent:()=>boolean=()=>true){
    scope=snapshotBrowserSyncScope(scope);const p=this.token(scope,token),guard=await this.guard(scope)
    const check=()=>{if(!guard.current()||!authorityCurrent())fail('closed')}
    check()
    if(p.project.source_revision===Number.MAX_SAFE_INTEGER)fail('source_limit')
    return guard.lease.use(async amk=>{
      const opened=await this.open(scope,p.project,p.notes,amk)
      check()
      const next=planPeerSourceTransition(scope,opened,p.project.source_revision,incoming,content)
      const project:LocalProjectRecord={...p.project,source_revision:next.sourceRevision,envelope:await this.seal(scope,p.project.project_id,'project',next.project,amk)}
      check()
      const note=await this.noteRow(scope,next.note,next.sourceRevision,amk)
      check();const plan=Object.freeze({}) as ProtectedNoteSourcePlan
      protectedChanges.set(plan,{scope,source:token,project,note,current:()=>guard.current()&&authorityCurrent()});return{token:plan,source_revision:next.sourceRevision,note,project}
    })
  }
  async prepareProtectedResolutionApply(scope:BrowserSyncScope,token:LocalSourceToken,verified:ResolutionApplyToken){
    scope=snapshotBrowserSyncScope(scope)
    const authority=verifiedResolutionForLocal(verified,this,scope),p=this.token(scope,token),guard=await this.guard(scope)
    if(p.project.source_revision===Number.MAX_SAFE_INTEGER)fail('source_limit')
    const check=()=>{if(!guard.current()||!authority.current())fail('closed')}
    check()
    return guard.lease.use(async amk=>{
      const opened=await this.open(scope,p.project,p.notes,amk),revision=p.project.source_revision+1
      check()
      const decision=authority.resolution.resolution,original=authority.resolution.result.note.id
      const outputs=[authority.resolution.result.note,...(decision.strategy==='keep_both'?[decision.retained_note]:[])]
      if(!opened.notes.some(n=>n.note.id===original))fail('missing_local_source')
      if(decision.strategy==='keep_both'&&(decision.retained_note.id===original||opened.notes.some(n=>n.note.id===decision.retained_note.id)))fail('conflicting_record')
      const admit=authority.transportKind==='content_note_resolution_v2'?validateContentLocalNote:validateLocalNote
      const sources=outputs.map(note=>admit('deleted_at'in note?{state:'tombstone',note,revision:0}:{state:'live',note:{...note,revision:0}},p.project.project_id))
      for(const n of sources){const i=opened.notes.findIndex(old=>old.note.id===n.note.id);if(i<0)opened.notes.push(n);else opened.notes[i]=n}
      if(opened.notes.length>MAX_LOCAL_NOTES)fail('source_limit')
      opened.project.source_revision=revision;opened.project.member_ids=opened.notes.map(n=>n.note.id).sort(order)
      const project:LocalProjectRecord={...p.project,source_revision:revision,envelope:await this.seal(scope,p.project.project_id,'project',opened.project,amk)}
      check()
      const notes:LocalNoteRecord[]=[]
      for(const n of sources){notes.push(await this.noteRow(scope,n,revision,amk));check()}
      notes.sort((a,b)=>order(a.note_id,b.note_id))
      const current=()=>guard.current()&&authority.current()
      if(!current())fail('closed')
      const prepared=Object.freeze({}) as ProtectedResolutionSourcePlan
      resolutionChanges.set(prepared,{scope,source:token,project,notes,original,current})
      return {token:prepared,project,notes}
    })
  }
  createNote(scope: BrowserSyncScope, token: LocalSourceToken, input: NativeLegacyNote): Promise<number> {
    const source = (this.writer?.transportKind==='content_note_v1'?validateContentLocalNote:validateLocalNote)({ state: 'live', note: input }, input.project_id)
    if (source.state !== 'live' || source.note.revision !== 0) fail()
    return this.mutate(scope, token, opened => { if (source.note.project_id !== opened.project.project_id || opened.notes.some(n => n.note.id === source.note.id)) fail('conflicting_record'); opened.notes.push(source); return [source] })
  }
  updateNote(scope: BrowserSyncScope, token: LocalSourceToken, id: string, patch: ProjectNotePatch, updatedAt: string): Promise<number> {
    localId(id)
    if (!timestamp(updatedAt) || !patch || typeof patch !== 'object' || Array.isArray(patch) || Object.keys(patch).some(k => !['title','content','tags','checklist','color','pinned','archived'].includes(k))) fail()
    const fields = structuredClone(patch)
    return this.mutate(scope, token, opened => {
      const source = opened.notes.find(n => n.note.id === id)
      if (!source || source.state !== 'live') fail('missing_local_source')
      if (!localSafe(source.note.revision + 1)) fail('source_limit')
      const note = structuredClone(source.note)
      for (const [key, value] of Object.entries(fields)) {
        if (['title','content','color'].includes(key) && value !== null && (typeof value !== 'string' || value.includes('\0'))) fail()
        let normalized = value ?? (['tags','checklist'].includes(key) ? [] : ['pinned','archived'].includes(key) ? false : '')
        if (key === 'color' && !['default','coral','orange','yellow','green','teal','blue','purple','pink','brown','gray'].includes(normalized as string)) normalized = 'default'
        ;(note as unknown as Record<string, unknown>)[key] = normalized
      }
      note.revision++; note.updated_at = updatedAt
      const changed = (this.writer?.transportKind==='content_note_v1'?validateContentLocalNote:validateLocalNote)({ state: 'live', note }, opened.project.project_id)
      opened.notes[opened.notes.indexOf(source)] = changed
      return [changed]
    })
  }
  tombstoneNote(scope: BrowserSyncScope, token: LocalSourceToken, id: string, deletedAt: string): Promise<number> {
    localId(id); if (!timestamp(deletedAt)) fail()
    return this.mutate(scope, token, opened => {
      const source = opened.notes.find(n => n.note.id === id)
      if (!source || source.state !== 'live') fail('missing_local_source')
      if (!localSafe(source.note.revision + 1)) fail('source_limit')
      const n = source.note
      const changed = (this.writer?.transportKind==='content_note_v1'?validateContentLocalNote:validateLocalNote)({ state: 'tombstone', revision: n.revision + 1, note: { id: n.id, project_id: n.project_id, stage_id: n.stage_id, source_type: n.source_type, source_map_id: n.source_map_id, source_node_id: n.source_node_id, content_format: n.content_format, deleted_at: deletedAt } }, opened.project.project_id)
      opened.notes[opened.notes.indexOf(source)] = changed
      return [changed]
    })
  }
  replaceProject(scope: BrowserSyncScope, token: LocalSourceToken, input: NativeLocalProject): Promise<number> {
    const project = validateLocalProject(input)
    return this.mutate(scope, token, opened => { if (project.id !== opened.project.project_id || project.created_at !== opened.project.project.created_at) fail(); opened.project.project = project })
  }
  deleteProject(scope: BrowserSyncScope, token: LocalSourceToken, deletedAt: string): Promise<number> {
    if (!timestamp(deletedAt)) fail()
    return this.mutate(scope, token, () => {}, deletedAt)
  }
}
