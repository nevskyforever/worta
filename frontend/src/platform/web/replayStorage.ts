import { commitProgressEvent, authenticateReplayProgress, progressPreparationInventory } from './progressApply'
import { prepareProgressEvent, inspectPreparedProgressEvent } from './progressPrepare'
import { commitMapEvent, authenticateReplayMaps, mapPreparationInventory } from './mapApply'
import { prepareMapEvent, inspectPreparedMapEvent } from './mapPrepare'
import { commitDocumentEvent, authenticateReplayDocuments, documentPreparationInventory } from './documentApply'
import { prepareDocumentEvent, inspectPreparedDocumentGenesis } from './documentPrepare'
import { readStageInventory } from './stageApply'
import {BrowserContentNoteIntents} from './contentNoteIntents'
import {BrowserV10Recovery} from './v10Recovery'
import {assertReplayAuthorTransition} from './replayInventory'
import { BrowserNoteReaderReadiness } from './noteReaderReadiness'
import { BrowserDeviceRegistration } from './deviceRegistration'
import { applyReplayContentUpdate } from './replayContentUpdate'
import type { MetadataPort } from './replayMetadata'
import { assertReplayOwnerTransition, replayCoverage, migrateReplay, scheduleReplaySnapshot, type ReplaySnapshot } from './replayInventory'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import { composeReplayMetadata, composeReplayOrderedContent, type ReplayMetadataResult, type ReplayStructuralResult, type ReplayInitialNoteResult } from './replayMetadata'
import { seedReplayNoteSource, type ReplaySourceSeedResult } from './replaySourceSeed'
import { prepareBrowserRemoteProjectImport } from './projectBindings'
import { BrowserLocalAuthority } from './localAuthority'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api } from '@/api/encryptedSyncV3'
import type { LocalProjectRecord } from './localAuthorityRecords'
import { BrowserPersistenceError, persistenceError, runBrowserTransaction } from './persistence'
import { BrowserSyncDurability, snapshotBrowserSyncScope,
  type BrowserReceivedInboxRecord, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'
import { REPLAY_LIMITS, prepareReplayPage, replayAnchor, replaySame, replayUuid,
  type ReplayAnchor, type ReplayManifest } from './replayRecords'
import type { ReplaySourceSeedReference } from './replayRecords'

export const REPLAY_DATABASE_NAME = 'worta-web-replay'
export const REPLAY_DATABASE_VERSION = 9
type Schema = readonly [string, string | string[], ReadonlyArray<readonly [string, string | string[], boolean]>]
const previousSchema: readonly Schema[] = [
  ['identities', 'canonical_user_id', [['local_account_id', 'local_account_id', true], ['device_id', 'device_id', true]]],
  ['accountBindings', 'local_account_id', [['canonical_user_id', 'canonical_user_id', true]]],
  ['outbox', ['account_id', 'event_id'], [['ordinal', ['account_id', 'device_id', 'local_ordinal'], true], ['state', ['account_id', 'state', 'local_ordinal'], false]]],
  ['inbox', ['account_id', 'server_sequence'], [['event', ['account_id', 'event_id'], true], ['state', ['account_id', 'state', 'server_sequence'], false]]],
  ['cursors', ['account_id', 'device_id'], []],
  ['receipts', ['account_id', 'event_id'], [['server_sequence', ['account_id', 'server_sequence'], true]]],
  ['projectBindings', ['account_id', 'project_id'], [['bootstrap', ['account_id', 'bootstrap_id'], true]]],
  ['ownerHeads', ['account_id', 'owner_kind', 'owner_id'], []],
  ['localProjects', ['account_id', 'project_id'], []],
  ['localNotes', ['account_id', 'project_id', 'note_id'], [['identity', ['account_id', 'note_id'], true]]],
  ['legacyNoteHistory', ['account_id', 'project_id', 'note_id', 'record_kind', 'record_id'], [['event', ['account_id', 'event_id'], true]]],
  ['legacyNoteIntents', ['account_id', 'event_id'], [['note', ['account_id', 'project_id', 'note_id'], true], ['ordinal', ['account_id', 'device_id', 'local_ordinal'], true]]],
  ['replayManifest', ['account_id', 'device_id'], []],
]
const documentSchema: readonly Schema[] = [
  ['documentLedger', ['account_id', 'event_id'], [['sequence', ['account_id', 'server_sequence'], true]]],
  ['documentProjection', ['account_id', 'project_id', 'entity_id'], []],
]
const mapSchema: readonly Schema[] = [
  ['mapLedger', ['account_id', 'event_id'], [['sequence', ['account_id', 'server_sequence'], true]]],
  ['mapProjection', ['account_id', 'project_id', 'entity_id'], []],
]
const v7Schema = [...previousSchema, ...documentSchema]
const v8Schema = [...v7Schema, ...mapSchema]
const progressSchema: readonly Schema[] = [
  ['progressLedger', ['account_id', 'event_id'], [['sequence', ['account_id', 'server_sequence'], true]]],
  ['progressProjection', ['account_id', 'project_id', 'entity_id'], []],
]
const schema = [...v8Schema, ...progressSchema]
const names = schema.map(([name]) => name)
function fail(): never { throw new BrowserPersistenceError('corrupt_record') }
const range = (account: string) => IDBKeyRange.bound([account], [account, []])
export type { ReplaySnapshot } from './replayInventory'

function validateSchema(database: IDBDatabase, upgrade?: IDBTransaction, expected: readonly Schema[] = schema): void {
  const expectedNames = expected.map(([name]) => name)
  if (!upgrade && database.version !== REPLAY_DATABASE_VERSION || !replaySame([...database.objectStoreNames].sort(), [...expectedNames].sort())) throw new BrowserPersistenceError('unsupported_version')
  const transaction = upgrade ?? database.transaction(expectedNames, 'readonly')
  for (const [name, path, indexes] of expected) {
    const store = transaction.objectStore(name)
    if (!replaySame(store.keyPath, path) || store.autoIncrement || store.indexNames.length !== indexes.length) fail()
    for (const [index, key, unique] of indexes) {
      if (!store.indexNames.contains(index)) fail()
      const value = store.index(index)
      if (!replaySame(value.keyPath, key) || value.unique !== unique || value.multiEntry) fail()
    }
  }
}

function openDatabase(signal: AbortSignal, current: () => boolean): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    let request: IDBOpenDBRequest, failed = false
    const stop = (error: unknown) => { failed = true; signal.removeEventListener('abort', cancel); try { request?.transaction?.abort() } catch { /* Already aborted. */ } reject(persistenceError(error)) }
    const cancel = () => stop(new BrowserPersistenceError('closed'))
    signal.addEventListener('abort', cancel, { once: true })
    if (signal.aborted) { cancel(); return }
    try { request = indexedDB.open(REPLAY_DATABASE_NAME, REPLAY_DATABASE_VERSION) } catch (error) { stop(error); return }
    request.onblocked = () => stop(new BrowserPersistenceError('upgrade_blocked'))
    request.onerror = () => stop(request.error)
    request.onupgradeneeded = event => {
      try {
        if (failed || signal.aborted || !current() || ![0, 1, 2, 3, 4, 5, 6, 7, 8].includes(event.oldVersion)) throw new BrowserPersistenceError('unsupported_version')
        if (event.oldVersion !== 0) {
          validateSchema(request.result, request.transaction!, event.oldVersion === 8 ? v8Schema : event.oldVersion === 7 ? v7Schema : previousSchema)
          for (const [name, keyPath, indexes] of (event.oldVersion === 8 ? progressSchema : event.oldVersion === 7 ? [...mapSchema, ...progressSchema] : [...documentSchema, ...mapSchema, ...progressSchema])) {
            const store = request.result.createObjectStore(name, { keyPath })
            for (const [index, path, unique] of indexes) store.createIndex(index, path, { unique })
          }
          migrateReplay(request.transaction!, (event.oldVersion === 8 ? v8Schema : event.oldVersion === 7 ? v7Schema : previousSchema).map(([name]) => name), () => { if (signal.aborted || !current()) throw new BrowserPersistenceError('closed') }, stop, Math.min(event.oldVersion, 6) as 1 | 2 | 3 | 4 | 5 | 6)
          return
        }
        for (const [name, keyPath, indexes] of schema) {
          const store = request.result.createObjectStore(name, { keyPath })
          for (const [index, path, unique] of indexes) store.createIndex(index, path, { unique })
        }
      } catch (error) { stop(error) }
    }
    request.onsuccess = () => {
      signal.removeEventListener('abort', cancel)
      if (failed || signal.aborted) { request.result.close(); reject(new BrowserPersistenceError('closed')); return }
      try { validateSchema(request.result); resolve(request.result) }
      catch (error) { request.result.close(); reject(persistenceError(error)) }
    }
  })
}

/** Separate reconstructible encrypted replay cache; only the approved real owners may add proofs. */
class BrowserReplayStorage {
  private closed = false
  private readonly active = new Set<IDBTransaction>()
  private readonly cancel = () => { void this.close() }
  constructor(private readonly db: IDBDatabase, private readonly scope: BrowserSyncScope, private readonly signal: AbortSignal,
    private readonly current: () => boolean, private readonly release: () => void, private readonly flight: () => Promise<unknown>, private readonly cleanup: () => void) {
    signal.addEventListener('abort', this.cancel, { once: true })
    db.onversionchange = this.cancel
    db.onclose = this.cancel
  }
  private check(): void { if (this.closed || this.signal.aborted || !this.current()) throw new BrowserPersistenceError('closed') }
  async close(): Promise<void> {
    if (!this.closed) {
      this.closed = true; this.signal.removeEventListener('abort', this.cancel); this.cleanup()
      for (const transaction of this.active) { try { transaction.abort() } catch { /* Native transaction already ended. */ } }
      this.db.close(); this.release()
    }
    await this.flight().catch(() => undefined)
  }
  private run<T>(mode: IDBTransactionMode, schedule: (tx: IDBTransaction, result: (value: T) => void, abort: (error: unknown) => void) => void): Promise<T> {
    this.check()
    return runBrowserTransaction<T>(this.db, names, mode === 'readonly' ? 'readonly' : 'readwrite', (tx, result, abort) => {
      this.check(); schedule(tx, value => { this.check(); result(value) }, abort)
    }, (tx, completion) => {
      this.active.add(tx)
      void completion.finally(() => this.active.delete(tx)).catch(() => undefined)
    }).then(value => { this.check(); return value }).catch(error => { this.check(); throw error })
  }
  private load(tx: IDBTransaction, result: (value: ReplaySnapshot) => void, abort: (error: unknown) => void): void {
    scheduleReplaySnapshot(tx, this.scope, () => this.check(), result, abort)
  }
  read(): Promise<ReplaySnapshot> { return this.run('readonly', (tx, result, abort) => this.load(tx, result, abort)) }
  applyMetadata(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext): Promise<ReplayMetadataResult> {
    return this.applyOwners(auth, bindings, keys, false) as Promise<ReplayMetadataResult>
  }
  applyStructure(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext): Promise<ReplayStructuralResult> {
    return this.applyOwners(auth, bindings, keys, true) as Promise<ReplayStructuralResult>
  }
  async applyInitialNotes(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext): Promise<ReplayInitialNoteResult> {
    const snapshot = await this.read(), projects = new Set(snapshot.rows.filter(row => JSON.parse(row.wire_item).event.entity_type === 'note')
      .map(row => JSON.parse(row.wire_item).event.project_id as string))
    // All source admissions finish before the first Note outcome. A later restart
    // never needs to recreate one from an already mutated source.
    for (const projectId of projects) if (!snapshot.bindings.some(binding => binding.project_id === projectId)) {
      await this.seedNoteSource(projectId, auth, bindings, keys)
      await this.admitInitialNoteBinding(projectId, auth, keys)
    }
    return this.applyOwners(auth, bindings, keys, true, true) as Promise<ReplayInitialNoteResult>
  }
  private async admitInitialNoteBinding(projectId: string, auth: NormalUserAuthRuntime, keys: RuntimeKeyContext, delivery: 'ordinary' | 'recovery' = 'ordinary'): Promise<void> {
    const stop = () => { void this.close() }
    const unAuth = auth.onInvalidated(stop), unKey = keys.onInvalidated(stop)
    try {
      const context = auth.requireContext(), lease = keys.leaseForAccount(this.scope.accountId)
      const check = () => { this.check(); if (!auth.isCurrent(context) || !lease?.isCurrent() || lease.localAccountId !== this.scope.accountId
        || lease.canonicalUserId !== this.scope.canonicalUserId || lease.authEpoch !== context.authEpoch) throw new BrowserPersistenceError('closed') }
      check()
      const before = await this.read(), manifest = before.manifest, seed = manifest?.source_seeds?.find(value => value.project_id === projectId)
      if (!manifest || !seed || !before.sources?.some(value => value.project_id === projectId) || before.bindings.some(value => value.project_id === projectId)
        || before.rows.some(row => row.state !== 'received')) throw new BrowserPersistenceError('stale_authority')
      const server = async () => {
        check()
        const registry = await auth.authorized(token => cloudProjectsApi.listBootstraps(token, this.signal)); check()
        const mode = await auth.authorized(token => encryptedSyncV2Api.capabilities(token, this.signal)); check()
        if (!replaySame(replayAnchor(registry.value, mode.value), manifest.anchor)) throw new BrowserPersistenceError('stale_authority')
        const tail = await auth.authorized(token => delivery === 'recovery'
          ? encryptedSyncV3Api.recoveryRaw(token, manifest.anchor.registry.current_cursor, 1, manifest.anchor.registry.current_cursor, manifest.anchor.transport.cutover_epoch, this.signal)
          : encryptedSyncV3Api.pullRaw(token, this.scope.deviceId, manifest.anchor.registry.current_cursor, 1, this.signal)); check()
        const { page } = prepareReplayPage(tail.value, manifest.anchor.registry.current_cursor, this.scope, manifest.anchor)
        if (page.items.length || page.has_more) throw new BrowserPersistenceError('stale_authority')
      }
      await server()
      const record = prepareBrowserRemoteProjectImport(this.scope, { project_id: projectId, bootstrap_id: seed.bootstrap_id,
        remote_state: 'active', remote_high_water: manifest.anchor.registry.current_cursor })
      await this.run('readwrite', (tx, result, abort) => this.load(tx, now => {
        try {
          check()
          if (!replaySame(now, before)) throw new BrowserPersistenceError('stale_authority')
          tx.objectStore('projectBindings').add(record)
          result(undefined)
        } catch (error) { abort(error) }
      }, abort))
    } finally { unAuth(); unKey() }
  }
  seedNoteSource(projectId: string, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding,
    keys: RuntimeKeyContext, delivery: 'ordinary' | 'recovery' = 'ordinary'): Promise<ReplaySourceSeedResult> {
    const stop = () => { void this.close() }
    const unAuth = auth.onInvalidated(stop), unKey = keys.onInvalidated(stop)
    return seedReplayNoteSource({ read: () => this.read(), readSource: async (expected, projectId) => {
      const transaction: BrowserSyncTransaction = (scope, _stores, mode, schedule) => {
        if (!replaySame(scope, this.scope) || mode !== 'readonly') fail()
        return this.run(mode, (tx, result, abort) => this.load(tx, before => {
          try { if (!replaySame(before, expected)) throw new BrowserPersistenceError('stale_authority'); schedule(tx, result, abort) }
          catch (error) { abort(error) }
        }, abort))
      }
      const local = new BrowserLocalAuthority(transaction).bind(auth, bindings, keys)
      const source = await local.readSnapshot(this.scope, projectId)
      const project = await local.withDecryptedSnapshot(this.scope, source.token, async value => structuredClone(value.project))
      return { source, project }
    }, commit: async (expected: ReplaySnapshot, row: LocalProjectRecord,
      reference: ReplaySourceSeedReference, server: () => Promise<void>) => {
      await server()
      await this.run('readwrite', (tx, result, abort) => this.load(tx, before => {
        try {
          if (!replaySame(before, expected) || !before.manifest || before.manifest.revision === Number.MAX_SAFE_INTEGER
            || before.sources?.some(source => source.project_id === row.project_id)
            || before.manifest.source_seeds?.some(seed => seed.project_id === row.project_id)) throw new BrowserPersistenceError('stale_authority')
          const writes = [tx.objectStore('localProjects').add(row), tx.objectStore('replayManifest').put({ ...before.manifest,
            source_seeds: [...before.manifest.source_seeds!, reference], revision: before.manifest.revision + 1 })]
          writes.forEach(write => { write.onsuccess = () => { try { this.check() } catch (error) { abort(error) } } })
          result(undefined)
        } catch (error) { abort(error) }
      }, abort))
    } }, this.scope, projectId, this.signal, () => { try { this.check(); return true } catch { return false } },
    auth, bindings, keys, delivery).finally(() => { unAuth(); unKey() })
  }
  private applyOwners(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext, structural: boolean, initialNotes = false) {
    const stop = () => { void this.close() }
    const unAuth = auth.onInvalidated(stop), unKey = keys.onInvalidated(stop)
    return composeReplayMetadata(this.ownerPort(), this.scope, this.signal, () => { try { this.check(); return true } catch { return false } }, auth, bindings, keys, structural, initialNotes).finally(() => { unAuth(); unKey() })
  }
  applyContentUpdate(sequence: number, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const stop = () => { void this.close() }
    const unAuth = auth.onInvalidated(stop), unKey = keys.onInvalidated(stop)
    return applyReplayContentUpdate(this.ownerPort(), this.scope, sequence, this.signal,
      () => { try { this.check(); return true } catch { return false } }, auth, bindings, keys).finally(() => { unAuth(); unKey() })
  }
  async applyOrderedContentUpdate(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const stop = () => { void this.close() }
    const unAuth = auth.onInvalidated(stop), unKey = keys.onInvalidated(stop)
    try {
      const snapshot = await this.read(), projects = new Set(snapshot.rows.filter(row => JSON.parse(row.wire_item).event.entity_type === 'note')
        .map(row => JSON.parse(row.wire_item).event.project_id as string))
      for (const projectId of projects) if (!snapshot.bindings.some(binding => binding.project_id === projectId)) {
        await this.seedNoteSource(projectId, auth, bindings, keys)
        await this.admitInitialNoteBinding(projectId, auth, keys)
      }
      return await composeReplayOrderedContent(this.ownerPort(), this.scope, this.signal,
        () => { try { this.check(); return true } catch { return false } }, auth, bindings, keys)
    } finally { unAuth(); unKey() }
  }
  /** Internal owner binding only. No UI composition, upload, capability or writer grant. */
  contentAuthorOwner(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext){
    const current=()=>{try{this.check();return true}catch{return false}}
    const transaction:BrowserSyncTransaction=async(scope,_stores,mode,schedule)=>{
      if(!replaySame(scope,this.scope))fail()
      return this.run(mode,(tx,result,abort)=>this.load(tx,before=>{
        try{if(mode==='readwrite'&&(!before.manifest||before.manifest.phase!=='received'||before.manifest.revision===Number.MAX_SAFE_INTEGER||!before.noteHistory?.some(r=>r.record_kind==='resolution'||r.record_kind==='group')))throw new BrowserPersistenceError('unsupported_local_source')
          schedule(tx,value=>{
          if(mode==='readonly'){result(value);return}
          this.load(tx,unchanged=>{try{
            if(replaySame(before,unchanged)){result(value);return}
            tx.objectStore('replayManifest').put({...before.manifest!,revision:before.manifest!.revision+1})
            this.load(tx,after=>{try{assertReplayAuthorTransition(before,after);result(value)}catch(e){abort(e)}},abort)
          }catch(e){abort(e)}},abort)
        },abort)}catch(e){abort(e)}
      },abort))
    }
    return new BrowserContentNoteIntents(transaction,new BrowserLocalAuthority(transaction),current).bind(auth,bindings,keys)
  }
  /** Exact accepted author echo only, through the normal authenticated pull (and its reader gate). */
  async receiveContentAuthorEcho(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext){
    const stop=()=>{void this.close()},unAuth=auth.onInvalidated(stop),unKey=keys.onInvalidated(stop)
    try{
    const context=auth.requireContext(),lease=keys.leaseForAccount(this.scope.accountId),before=await this.read()
    const check=()=>{this.check();if(!auth.isCurrent(context)||!lease?.isCurrent()||context.userId!==this.scope.canonicalUserId)throw new BrowserPersistenceError('closed')}
    check();await this.contentAuthorOwner(auth,bindings,keys).authority.authenticate(this.scope);check()
    const registry=await auth.authorized(token=>cloudProjectsApi.listBootstraps(token,this.signal));check()
    const mode=await auth.authorized(token=>encryptedSyncV2Api.capabilities(token,this.signal));check()
    const anchor=replayAnchor(registry.value,mode.value),previous=before.manifest!
    if(previous&&replaySame(anchor,previous.anchor)&&(before.rows.at(-1)?.state==='content_note_self_echo_proven'||before.rows.at(-1)?.state==='note_resolution_proven'&&JSON.parse(before.rows.at(-1)!.wire_item).event.device_id===this.scope.deviceId)){
      await this.applyOrderedContentUpdate(auth,bindings,keys);check()
      return {committed_cursor:previous.receive_cursor,new_events:0,replayed_events:1,has_more:false}
    }
    if(!previous||previous.phase!=='received'||previous.applied_cursor!==previous.receive_cursor||anchor.registry.current_cursor!==previous.receive_cursor+1
      ||!replaySame(anchor.transport,previous.anchor.transport)||!replaySame(anchor.registry.projects,previous.anchor.registry.projects))fail()
    const response=await auth.authorized(token=>encryptedSyncV3Api.pullRaw(token,this.scope.deviceId,previous.receive_cursor,100,this.signal));check()
    const {page}=prepareReplayPage(response.value,previous.receive_cursor,this.scope,anchor)
    const item=page.items[0],receipt=before.author?.receipts.find((r:any)=>r.event_id===item?.event.event_id) as {server_sequence:number}|undefined
    if(page.items.length!==1||page.has_more||!receipt||receipt.server_sequence!==page.next_cursor||item!.event.device_id!==this.scope.deviceId)fail()
    const transaction:BrowserSyncTransaction=(scope,_stores,_mode,schedule)=>this.run('readwrite',(tx,result,abort)=>this.load(tx,now=>{
      try{check();if(!replaySame(scope,this.scope)||!replaySame(now,before))throw new BrowserPersistenceError('stale_source')
        schedule(tx,value=>{
          try{check();const bytes=Math.floor((JSON.parse(response.value).items[0].object.ciphertext as string).length*3/4)
            tx.objectStore('replayManifest').put({...previous,anchor,receive_cursor:page.next_cursor,ciphertext_bytes:previous.ciphertext_bytes+bytes,revision:previous.revision+1})
            this.load(tx,after=>{try{if(!before.checkpoint||!after.checkpoint||!replaySame(after.checkpoint,{...before.checkpoint,pull_cursor:page.next_cursor})||!replaySame({...after,checkpoint:before.checkpoint,manifest:before.manifest,rows:before.rows},before)||after.rows.length!==before.rows.length+1||!replaySame(after.rows.slice(0,-1),before.rows))fail();result(value)}catch(e){abort(e)}},abort)
          }catch(e){abort(e)}
        },abort)
      }catch(e){abort(e)}
    },abort))
    return await new BrowserSyncDurability(transaction).commitInboundPage(this.scope,previous.receive_cursor,response.value)
    }finally{unAuth();unKey()}
  }
  /** Negative qualification diagnostic over the real replay inventory. It never
   * pulls data, declares a reader, advances coverage or opens a project. */
  async inspectNoteReaderReadiness(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding,
    keys: RuntimeKeyContext, options: { checkServerGate?: boolean } = {}) {
    const stop = () => { void this.close() }
    const unAuth = auth.onInvalidated(stop), unKey = keys.onInvalidated(stop)
    try {
      const current = () => { try { this.check(); return true } catch { return false } }
      const snapshot = await this.read()
      const transaction = this.ownerPort().transaction(snapshot, 0, async () => { throw new BrowserPersistenceError('corrupt_record') })
      const device = await new BrowserDeviceRegistration(transaction, current).bind(auth, bindings).registerVerified(this.scope)
      return await new BrowserNoteReaderReadiness(transaction, current).bind(auth, bindings, keys, device).inspect(this.scope, options)
    } finally { unAuth(); unKey() }
  }
  /** Bounded recovery; each owner commit retains the ordinary replay CAS and coverage. */
  async importRecoveryProject(projectId: string, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext, delivery: 'ordinary' | 'recovery' = 'ordinary') {
    await this.seedNoteSource(projectId, auth, bindings, keys, delivery)
    const snapshot = await this.read()
    if (!snapshot.bindings.some(binding => binding.project_id === projectId))
      await this.admitInitialNoteBinding(projectId, auth, keys, delivery)
  }
  async recoverV10(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext, delivery: 'ordinary' | 'recovery' = 'ordinary') {
    const stop = () => { void this.close() }
    const unAuth = auth.onInvalidated(stop), unKey = keys.onInvalidated(stop)
    try {
      const current = () => { try { this.check(); return true } catch { return false } }
      const port = this.ownerPort()
      const readonly: BrowserSyncTransaction = async (scope, stores, mode, schedule) => {
        if (mode !== 'readonly') throw new BrowserPersistenceError('unsupported_local_source')
        return port.transaction(await this.read(), 0, async () => { throw new BrowserPersistenceError('closed') })(scope, stores, mode, schedule)
      }
      const registration = new BrowserDeviceRegistration(readonly, current).bind(auth, bindings)
      const recoveryAnchor = (await this.read()).manifest?.anchor
      if (delivery === 'recovery' && !recoveryAnchor) throw new BrowserPersistenceError('stale_authority')
      const device = delivery === 'recovery'
        ? await registration.verifyRecovery(this.scope, recoveryAnchor!.registry.current_cursor, recoveryAnchor!.transport.cutover_epoch, this.signal)
        : await registration.registerVerified(this.scope)
      const ownerTransaction = async (sequence: number) => {
        const expected = await this.read(), manifest = expected.manifest
        if (!manifest || manifest.contract_version !== 6 || manifest.phase !== 'received' || sequence !== 0 && sequence !== (manifest.applied_cursor ?? 0) + 1) throw new BrowserPersistenceError('stale_cursor')
        const context = auth.requireContext(), lease = keys.leaseForAccount(this.scope.accountId)
        const server = async () => {
          this.check()
          if (!auth.isCurrent(context) || !lease?.isCurrent()) throw new BrowserPersistenceError('closed')
          const registry = await auth.authorized(token => cloudProjectsApi.listBootstraps(token, this.signal))
          const mode = await auth.authorized(token => encryptedSyncV2Api.capabilities(token, this.signal))
          if (!replaySame(replayAnchor(registry.value, mode.value), manifest.anchor)) throw new BrowserPersistenceError('stale_authority')
          const tail = await auth.authorized(token => delivery === 'recovery'
            ? encryptedSyncV3Api.recoveryRaw(token, manifest.anchor.registry.current_cursor, 1, manifest.anchor.registry.current_cursor, manifest.anchor.transport.cutover_epoch, this.signal)
            : encryptedSyncV3Api.pullRaw(token, this.scope.deviceId, manifest.anchor.registry.current_cursor, 1, this.signal))
          const { page } = prepareReplayPage(tail.value, manifest.anchor.registry.current_cursor, this.scope, manifest.anchor)
          if (page.items.length || page.has_more) throw new BrowserPersistenceError('stale_authority')
          this.check()
          if (!auth.isCurrent(context) || !lease.isCurrent()) throw new BrowserPersistenceError('closed')
        }
        if (sequence !== 0) return port.transaction(expected, sequence, server)
        const transaction: BrowserSyncTransaction = async (scope, _stores, mode, schedule) => {
          if (!replaySame(scope, this.scope)) throw new BrowserPersistenceError('account_mismatch')
          if (mode === 'readwrite') await server()
          return this.run(mode, (tx, result, abort) => this.load(tx, before => {
            try {
              if (!replaySame(before, expected)) throw new BrowserPersistenceError('stale_authority')
              schedule(tx, value => this.load(tx, after => {
                try {
                  const checkpoint = after.checkpoint, previous = before.checkpoint
                  if (!checkpoint || !previous || checkpoint.proven_prefix < previous.proven_prefix
                    || checkpoint.proven_prefix > (manifest.applied_cursor ?? 0)
                    || !replaySame(checkpoint, { ...previous, proven_prefix: checkpoint.proven_prefix })
                    || !replaySame({ ...after, checkpoint: previous }, before)) throw new BrowserPersistenceError('corrupt_record')
                  result(value)
                } catch (error) { abort(error) }
              }, abort), abort)
            } catch (error) { abort(error) }
          }, abort))
        }
        return transaction
      }
      const validatePrefix = async (candidate: number) => {
        const snapshot = await this.read(), manifest = snapshot.manifest
        if (!manifest || manifest.contract_version !== 6 || manifest.phase !== 'received'
          || candidate > (manifest.applied_cursor ?? 0)) throw new BrowserPersistenceError('corrupt_record')
      }
      return await new BrowserV10Recovery(readonly, new BrowserLocalAuthority(readonly), current, ownerTransaction, validatePrefix, () => ownerTransaction(0), {
        authenticate: () => this.authenticateDocuments(auth, bindings, keys),
        apply: sequence => this.applyDocumentEvent(sequence, auth, bindings, keys),
      }, { authenticate: () => this.authenticateMaps(auth, bindings, keys), apply: sequence => this.applyMapEvent(sequence, auth, bindings, keys) }, { authenticate: () => this.authenticateProgress(auth, bindings, keys), apply: sequence => this.applyProgressEvent(sequence, auth, bindings, keys) })
        .bind(auth, bindings, keys, device).run(this.scope, { maxEvents: 1 })
    } finally { unAuth(); unKey() }
  }
  async authenticateProgress(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const snapshot=await this.read(),{context}=await bindings.ensureForCurrentUser(this.scope.accountId),lease=keys.leaseForAccount(this.scope.accountId)
    const current=()=>{try{this.check();return auth.isCurrent(context)&&!!lease?.isCurrent()}catch{return false}}
    if(!lease||lease.localAccountId!==this.scope.accountId||context.userId!==this.scope.canonicalUserId||lease.canonicalUserId!==context.userId||lease.authEpoch!==context.authEpoch||!current())throw new BrowserPersistenceError('closed')
    const progress=await lease.use(amk=>authenticateReplayProgress(this.scope,snapshot,amk,current))
    if(!current()||!replaySame(snapshot,await this.read()))throw new BrowserPersistenceError('stale_authority')
    return progress
  }

  async authenticateMaps(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const snapshot=await this.read(),{context}=await bindings.ensureForCurrentUser(this.scope.accountId),lease=keys.leaseForAccount(this.scope.accountId)
    const current=()=>{try{this.check();return auth.isCurrent(context)&&!!lease?.isCurrent()}catch{return false}}
    if(!lease||lease.localAccountId!==this.scope.accountId||context.userId!==this.scope.canonicalUserId||lease.canonicalUserId!==context.userId||lease.authEpoch!==context.authEpoch||!current())throw new BrowserPersistenceError('closed')
    const maps=await lease.use(amk=>authenticateReplayMaps(this.scope,snapshot,amk,current))
    if(!current()||!replaySame(snapshot,await this.read()))throw new BrowserPersistenceError('stale_authority')
    return maps
  }

  async authenticateDocuments(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const snapshot=await this.read(),{context}=await bindings.ensureForCurrentUser(this.scope.accountId),lease=keys.leaseForAccount(this.scope.accountId)
    const current=()=>{try{this.check();return auth.isCurrent(context)&&!!lease?.isCurrent()}catch{return false}}
    if(!lease||lease.localAccountId!==this.scope.accountId||context.userId!==this.scope.canonicalUserId||lease.canonicalUserId!==context.userId||lease.authEpoch!==context.authEpoch||!current())throw new BrowserPersistenceError('closed')
    const documents=await lease.use(amk=>authenticateReplayDocuments(this.scope,snapshot,amk,current))
    if(!current()||!replaySame(snapshot,await this.read()))throw new BrowserPersistenceError('stale_authority')
    return documents
  }

  async applyDocumentGenesis(sequence:number, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const row=(await this.read()).rows.find(row=>row.server_sequence===sequence)
    if(!row||JSON.parse(row.wire_item).event.revision!==1)throw new BrowserPersistenceError('unsupported_local_source')
    return this.applyDocumentEvent(sequence,auth,bindings,keys)
  }
  private async applyDocumentEvent(sequence:number, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const stop=()=>{void this.close()},unAuth=auth.onInvalidated(stop),unKey=keys.onInvalidated(stop)
    try {
    await this.authenticateDocuments(auth,bindings,keys)
    const expected=await this.read(),manifest=expected.manifest,row=expected.rows.find(row=>row.server_sequence===sequence)
    if(!manifest||manifest.phase!=='received'||!row)throw new BrowserPersistenceError('stale_cursor')
    if(row.state==='document_proven')return 'already_applied' as const
    if(row.state!=='received'||sequence!==manifest.applied_cursor!+1)throw new BrowserPersistenceError('stale_cursor')
    const {context}=await bindings.ensureForCurrentUser(this.scope.accountId),lease=keys.leaseForAccount(this.scope.accountId)
    const current=()=>{try{this.check();return auth.isCurrent(context)&&!!lease?.isCurrent()}catch{return false}}
    if(!lease||!current())throw new BrowserPersistenceError('closed')
    const server=async()=>{
      if(!current())throw new BrowserPersistenceError('closed')
      const registry=await auth.authorized(token=>cloudProjectsApi.listBootstraps(token,this.signal)),mode=await auth.authorized(token=>encryptedSyncV2Api.capabilities(token,this.signal))
      if(!replaySame(replayAnchor(registry.value,mode.value),manifest.anchor))throw new BrowserPersistenceError('stale_authority')
      let since=0,more=true
      while(more){
        const raw=await auth.authorized(token=>encryptedSyncV3Api.recoveryRaw(token,since,REPLAY_LIMITS.page,manifest.anchor.registry.current_cursor,manifest.anchor.transport.cutover_epoch,this.signal))
        const page=prepareReplayPage(raw.value,since,this.scope,manifest.anchor).page
        if(page.items.some((value,index)=>!expected.rows[since+index]||!replaySame(value,JSON.parse(expected.rows[since+index]!.wire_item))))throw new BrowserPersistenceError('stale_authority')
        if(page.has_more&&page.next_cursor<=since)throw new BrowserPersistenceError('stale_authority')
        since=page.next_cursor;more=page.has_more
      }
      if(since!==expected.rows.length)throw new BrowserPersistenceError('stale_authority')
      if(!current())throw new BrowserPersistenceError('closed')
    }
    return await lease.use(async amk=>{
      const inventory=documentPreparationInventory(expected),token=await prepareDocumentEvent(this.scope,row,inventory,amk,current)
      const document=inspectPreparedDocumentGenesis(token,this.scope,inventory).projection
      const existing=await authenticateReplayDocuments(this.scope,expected,amk,current)
      if(existing.some(value=>value.project_id===document.project_id&&value.stage_id===document.stage_id&&value.id!==document.id))throw new BrowserPersistenceError('unsupported_local_source')
      return commitDocumentEvent(this.ownerPort().transaction(expected,sequence,server),this.scope,token,inventory,amk,current,
        (tx,done,abort)=>readStageInventory(tx,this.scope,done,abort))
    })
    } finally { unAuth();unKey() }
  }

  private async applyProgressEvent(sequence:number, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const stop=()=>{void this.close()},unAuth=auth.onInvalidated(stop),unKey=keys.onInvalidated(stop)
    try {
    await this.authenticateProgress(auth,bindings,keys)
    const expected=await this.read(),manifest=expected.manifest,row=expected.rows.find(row=>row.server_sequence===sequence)
    if(!manifest||manifest.phase!=='received'||!row)throw new BrowserPersistenceError('stale_cursor')
    if(row.state==='progress_proven')return 'already_applied' as const
    if(row.state!=='received'||sequence!==manifest.applied_cursor!+1)throw new BrowserPersistenceError('stale_cursor')
    const {context}=await bindings.ensureForCurrentUser(this.scope.accountId),lease=keys.leaseForAccount(this.scope.accountId)
    const current=()=>{try{this.check();return auth.isCurrent(context)&&!!lease?.isCurrent()}catch{return false}}
    if(!lease||!current())throw new BrowserPersistenceError('closed')
    const server=async()=>{
      if(!current())throw new BrowserPersistenceError('closed')
      const registry=await auth.authorized(token=>cloudProjectsApi.listBootstraps(token,this.signal)),mode=await auth.authorized(token=>encryptedSyncV2Api.capabilities(token,this.signal))
      if(!replaySame(replayAnchor(registry.value,mode.value),manifest.anchor))throw new BrowserPersistenceError('stale_authority')
      let since=0,more=true
      while(more){
        const raw=await auth.authorized(token=>encryptedSyncV3Api.recoveryRaw(token,since,REPLAY_LIMITS.page,manifest.anchor.registry.current_cursor,manifest.anchor.transport.cutover_epoch,this.signal))
        const page=prepareReplayPage(raw.value,since,this.scope,manifest.anchor).page
        if(page.items.some((value,index)=>!expected.rows[since+index]||!replaySame(value,JSON.parse(expected.rows[since+index]!.wire_item))))throw new BrowserPersistenceError('stale_authority')
        if(page.has_more&&page.next_cursor<=since)throw new BrowserPersistenceError('stale_authority')
        since=page.next_cursor;more=page.has_more
      }
      if(since!==expected.rows.length)throw new BrowserPersistenceError('stale_authority')
      if(!current())throw new BrowserPersistenceError('closed')
    }
    return await lease.use(async amk=>{
      const inventory=progressPreparationInventory(expected),token=await prepareProgressEvent(this.scope,row,inventory,amk,current)
      const plan = inspectPreparedProgressEvent(token, this.scope, inventory)
      const other = (await authenticateReplayProgress(this.scope, expected, amk, current))
        .filter(value => value.project_id !== plan.event.header.project_id || value.stage_id !== plan.event.header.stage_id)
      const ids = new Set(other.flatMap(value => value.entries.map(entry => entry.entry_id)))
      if (plan.projection.entries.some(entry => ids.has(entry.entry_id))) throw new BrowserPersistenceError('conflicting_record')
      return commitProgressEvent(this.ownerPort().transaction(expected,sequence,server),this.scope,token,inventory,amk,current,
        (tx,done,abort)=>readStageInventory(tx,this.scope,done,abort))
    })
    } finally { unAuth();unKey() }
  }
  private async applyMapEvent(sequence:number, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    const stop=()=>{void this.close()},unAuth=auth.onInvalidated(stop),unKey=keys.onInvalidated(stop)
    try {
    await this.authenticateMaps(auth,bindings,keys)
    const expected=await this.read(),manifest=expected.manifest,row=expected.rows.find(row=>row.server_sequence===sequence)
    if(!manifest||manifest.phase!=='received'||!row)throw new BrowserPersistenceError('stale_cursor')
    if(row.state==='map_proven')return 'already_applied' as const
    if(row.state!=='received'||sequence!==manifest.applied_cursor!+1)throw new BrowserPersistenceError('stale_cursor')
    const {context}=await bindings.ensureForCurrentUser(this.scope.accountId),lease=keys.leaseForAccount(this.scope.accountId)
    const current=()=>{try{this.check();return auth.isCurrent(context)&&!!lease?.isCurrent()}catch{return false}}
    if(!lease||!current())throw new BrowserPersistenceError('closed')
    const server=async()=>{
      if(!current())throw new BrowserPersistenceError('closed')
      const registry=await auth.authorized(token=>cloudProjectsApi.listBootstraps(token,this.signal)),mode=await auth.authorized(token=>encryptedSyncV2Api.capabilities(token,this.signal))
      if(!replaySame(replayAnchor(registry.value,mode.value),manifest.anchor))throw new BrowserPersistenceError('stale_authority')
      let since=0,more=true
      while(more){
        const raw=await auth.authorized(token=>encryptedSyncV3Api.recoveryRaw(token,since,REPLAY_LIMITS.page,manifest.anchor.registry.current_cursor,manifest.anchor.transport.cutover_epoch,this.signal))
        const page=prepareReplayPage(raw.value,since,this.scope,manifest.anchor).page
        if(page.items.some((value,index)=>!expected.rows[since+index]||!replaySame(value,JSON.parse(expected.rows[since+index]!.wire_item))))throw new BrowserPersistenceError('stale_authority')
        if(page.has_more&&page.next_cursor<=since)throw new BrowserPersistenceError('stale_authority')
        since=page.next_cursor;more=page.has_more
      }
      if(since!==expected.rows.length)throw new BrowserPersistenceError('stale_authority')
      if(!current())throw new BrowserPersistenceError('closed')
    }
    return await lease.use(async amk=>{
      const inventory=mapPreparationInventory(expected),token=await prepareMapEvent(this.scope,row,inventory,amk,current)
      const plan = inspectPreparedMapEvent(token, this.scope, inventory)
      const existing = await authenticateReplayMaps(this.scope, expected, amk, current)
      // Native projection() refuses a derived Note identity already owned by
      // ordinary Content or by another project/node. Read-only does not waive
      // that ownership rule or create a second Note authority.
      for (const [node, value] of Object.entries(plan.projection.annotations)) {
        const noteId = (value as { note_id: string }).note_id
        if (expected.notes?.some(note => note.note_id === noteId)
          || existing.some(map => Object.entries(map.annotations).some(([oldNode, annotation]) =>
            (annotation as { note_id: string }).note_id === noteId && (map.project_id !== plan.event.header.project_id || map.stage_id !== plan.event.header.stage_id || oldNode !== node))))
          throw new BrowserPersistenceError('unsupported_local_source')
      }
      return commitMapEvent(this.ownerPort().transaction(expected,sequence,server),this.scope,token,inventory,amk,current,
        (tx,done,abort)=>readStageInventory(tx,this.scope,done,abort))
    })
    } finally { unAuth();unKey() }
  }

  private ownerPort(): MetadataPort {
    return { read: () => this.read(), transaction: (expected, sequence, server) => async (scope, _stores, mode, schedule) => {
      if (!replaySame(scope, this.scope)) fail()
      if (mode === 'readwrite') await server()
      return this.run(mode, (tx, result, abort) => this.load(tx, before => {
        try {
          if (!replaySame(before, expected)) throw new BrowserPersistenceError('stale_authority')
          schedule(tx, value => {
            try {
              this.check()
              if (mode === 'readonly') { result(value); return }
              const finish = (after: ReplaySnapshot) => {
                try { assertReplayOwnerTransition(before, after, sequence, value && typeof value === 'object' && 'status' in value ? value.status : value); result(value) } catch (error) { abort(error) }
              }
              if (value === 'applied' || value === 'conflict_preserved' || value==='self_echo_applied'|| value && typeof value === 'object' && 'status' in value && (value.status === 'applied'||value.status==='self_echo_applied')) {
                const manifest = before.manifest!
                if (manifest.revision === Number.MAX_SAFE_INTEGER) fail()
                const row = tx.objectStore('inbox').get([this.scope.accountId, sequence])
                row.onsuccess = () => { try { tx.objectStore('replayManifest').put(replayCoverage(manifest, row.result)); this.load(tx, finish, abort) } catch (error) { abort(error) } }
                return
              }
              this.load(tx, finish, abort)
            } catch (error) { abort(error) }
          }, abort)
        } catch (error) { abort(error) }
      }, abort))
    } }
  }

  async start(input: ReplayAnchor): Promise<ReplaySnapshot> {
    const anchor = replayAnchor(input.registry, input.transport), s = this.scope
    await this.run('readwrite', (tx, result, abort) => this.load(tx, old => {
      try {
        if (old.manifest && replaySame(old.manifest.anchor, anchor)) { result(undefined); return }
        if (old.manifest) {
          if (old.manifest.applied_cursor || old.manifest.source_seeds?.length) throw new BrowserPersistenceError('stale_authority')
          const prior = old.manifest.anchor
          if (anchor.registry.current_cursor < prior.registry.current_cursor || anchor.transport.cutover_epoch < prior.transport.cutover_epoch) throw new BrowserPersistenceError('stale_cursor')
          if (prior.registry.projects.some(project => !replaySame(project, anchor.registry.projects.find(next => next.project_id === project.project_id)))) throw new BrowserPersistenceError('stale_lineage')
        }
        // Only this account's known, owner-free replay cache can be rebuilt.
        for (const name of ['inbox', 'cursors', 'replayManifest']) tx.objectStore(name).delete(range(s.accountId))
        tx.objectStore('identities').put({ record_version: 1, canonical_user_id: s.canonicalUserId, local_account_id: s.accountId, device_id: s.deviceId })
        tx.objectStore('accountBindings').put({ record_version: 1, local_account_id: s.accountId, canonical_user_id: s.canonicalUserId })
        tx.objectStore('cursors').put({ record_version: 1, account_id: s.accountId, device_id: s.deviceId, pull_cursor: 0, ack_cursor: 0, proven_prefix: 0, next_local_ordinal: 1 })
        const manifest: ReplayManifest = { record_version: 6, contract_version: 6, metadata_cursor: 0, applied_cursor: 0, source_seeds: [], account_id: s.accountId, canonical_user_id: s.canonicalUserId, device_id: s.deviceId,
          generation: crypto.randomUUID(), anchor, receive_cursor: 0, ciphertext_bytes: 0, phase: 'receiving', revision: 1 }
        tx.objectStore('replayManifest').put(manifest); result(undefined)
      } catch (error) { abort(error) }
    }, abort))
    return this.read()
  }
  /** Atomically stage one bounded authenticated recovery extension; owner proofs remain pending. */
  async stageRecoveryExtension(input: ReplaySnapshot, nextAnchor: ReplayAnchor, raw: string, server: () => Promise<void>): Promise<ReplaySnapshot> {
    const expected = structuredClone(input), prior = expected.manifest, anchor = replayAnchor(nextAnchor.registry, nextAnchor.transport)
    if (!prior || prior.contract_version !== 6 || anchor.registry.current_cursor <= prior.anchor.registry.current_cursor
      || prior.receive_cursor > prior.anchor.registry.current_cursor
      || !replaySame(prior.anchor.transport, anchor.transport) || !replaySame(prior.anchor.registry.projects, anchor.registry.projects)
      || prior.revision === Number.MAX_SAFE_INTEGER) throw new BrowserPersistenceError('stale_authority')
    const { page, bytes } = prepareReplayPage(raw, prior.receive_cursor, this.scope, anchor)
    if (page.next_cursor > anchor.registry.current_cursor || !page.items.length
      || prior.ciphertext_bytes + bytes > REPLAY_LIMITS.ciphertextBytes) throw new BrowserPersistenceError('source_limit')
    const nextManifest = { ...prior, anchor, receive_cursor: page.next_cursor, ciphertext_bytes: prior.ciphertext_bytes + bytes,
      phase: page.has_more ? 'receiving' as const : 'received' as const, revision: prior.revision + 1 }
    const transaction: BrowserSyncTransaction = async (scope, _stores, mode, schedule) => {
      if (!replaySame(scope, this.scope) || mode !== 'readwrite') fail()
      await server(); this.check()
      return this.run('readwrite', (tx, result, abort) => this.load(tx, before => {
        try {
          if (!replaySame(before, expected)) throw new BrowserPersistenceError('stale_authority')
          schedule(tx, value => {
            try {
              this.check(); tx.objectStore('replayManifest').put(nextManifest)
              this.load(tx, after => {
                try {
                  if (!before.checkpoint || !after.checkpoint || !replaySame(after.checkpoint, { ...before.checkpoint, pull_cursor: page.next_cursor })
                    || !replaySame(after.rows.slice(0, before.rows.length), before.rows)
                    || after.rows.length !== before.rows.length + page.items.length
                    || !replaySame({ ...after, manifest: before.manifest, checkpoint: before.checkpoint, rows: before.rows }, before)) fail()
                  result(value)
                } catch (error) { abort(error) }
              }, abort)
            } catch (error) { abort(error) }
          }, abort)
        } catch (error) { abort(error) }
      }, abort))
    }
    await new BrowserSyncDurability(transaction).commitInboundPage(this.scope, prior.receive_cursor, raw)
    return this.read()
  }
  async commitPage(input: ReplaySnapshot, since: number, raw: string): Promise<ReplaySnapshot> {
    const expected = structuredClone(input), manifest = expected.manifest
    if (!manifest) fail()
    if (manifest.revision === Number.MAX_SAFE_INTEGER) fail()
    const { page, bytes } = prepareReplayPage(raw, since, this.scope, manifest.anchor)
    const replay = since < manifest.receive_cursor && page.next_cursor === manifest.receive_cursor
    const nextBytes = manifest.ciphertext_bytes + (replay ? 0 : bytes)
    const nextManifest = replay ? manifest : { ...manifest, receive_cursor: page.next_cursor, ciphertext_bytes: nextBytes, phase: 'receiving' as const, revision: manifest.revision + 1 }
    const newRows: BrowserReceivedInboxRecord[] = replay ? [] : page.items.map(item => ({ record_version: 1, account_id: this.scope.accountId, device_id: this.scope.deviceId,
      event_id: item.event.event_id, server_sequence: item.event.server_sequence!, protocol_version: 3, state: 'received', wire_item: JSON.stringify(item) }))
    // Match the persisted row representation, including escaped wire_item JSON.
    // Reserve bounds the three fixed-shape identity/binding/cursor control rows.
    const retainedBytes = [...expected.rows, ...newRows].reduce((total, row) => total + new TextEncoder().encode(JSON.stringify(row)).length,
      REPLAY_LIMITS.controlReserveBytes + new TextEncoder().encode(JSON.stringify(nextManifest)).length)
    if (nextBytes > REPLAY_LIMITS.ciphertextBytes || retainedBytes > REPLAY_LIMITS.inventoryBytes) throw new BrowserPersistenceError('source_limit')
    const transaction: BrowserSyncTransaction = (scope, _stores, mode, schedule) => {
      if (!replaySame(scope, this.scope) || mode !== 'readwrite') fail()
      return this.run(mode, (tx, result, abort) => this.load(tx, now => {
        try {
          if (!replaySame(now, expected)) throw new BrowserPersistenceError('stale_cursor')
          schedule(tx, value => {
            try {
              this.check()
              if (!replay) tx.objectStore('replayManifest').put(nextManifest)
              result(value)
            } catch (error) { abort(error) }
          }, abort)
        } catch (error) { abort(error) }
      }, abort))
    }
    await new BrowserSyncDurability(transaction).commitInboundPage(this.scope, since, raw)
    return this.read()
  }
  async finish(input: ReplaySnapshot): Promise<ReplaySnapshot> {
    const expected = structuredClone(input)
    await this.run('readwrite', (tx, result, abort) => this.load(tx, now => {
      try {
        if (!replaySame(now, expected)) throw new BrowserPersistenceError('stale_cursor')
        const manifest = now.manifest
        if (!manifest || manifest.receive_cursor !== manifest.anchor.registry.current_cursor) fail()
        if (manifest.revision === Number.MAX_SAFE_INTEGER) fail()
        if (manifest.phase !== 'received') tx.objectStore('replayManifest').put({ ...manifest, phase: 'received', revision: manifest.revision + 1 })
        result(undefined)
      } catch (error) { abort(error) }
    }, abort))
    return this.read()
  }
}

export type ReplayStorage = Pick<BrowserReplayStorage, 'authenticateProgress' | 'authenticateMaps' | 'authenticateDocuments' | 'applyDocumentGenesis' | 'importRecoveryProject' | 'recoverV10' | 'start' | 'read' | 'stageRecoveryExtension' | 'commitPage' | 'finish' | 'close' | 'applyMetadata' | 'applyStructure' | 'seedNoteSource' | 'applyInitialNotes' | 'applyContentUpdate' | 'applyOrderedContentUpdate' | 'inspectNoteReaderReadiness'|'contentAuthorOwner'|'receiveContentAuthorEcho'>

export async function openBrowserReplayStorage(input: BrowserSyncScope, signal: AbortSignal, current: () => boolean): Promise<ReplayStorage> {
  const scope = snapshotBrowserSyncScope(input)
  if (![scope.accountId, scope.canonicalUserId, scope.deviceId].every(replayUuid)) throw new BrowserPersistenceError('account_mismatch')
  if (signal.aborted || !current()) throw new BrowserPersistenceError('closed')
  if (typeof window === 'undefined' || !globalThis.isSecureContext || !globalThis.indexedDB || !navigator.locks) throw new BrowserPersistenceError('unavailable')
  const controller = new AbortController(), cancel = () => controller.abort()
  signal.addEventListener('abort', cancel, { once: true }); window.addEventListener('pagehide', cancel, { once: true })
  const cleanup = () => { signal.removeEventListener('abort', cancel); window.removeEventListener('pagehide', cancel) }
  let resolve!: (store: ReplayStorage) => void, reject!: (error: unknown) => void, store: BrowserReplayStorage | undefined
  const ready = new Promise<ReplayStorage>((yes, no) => { resolve = yes; reject = no })
  let flight: Promise<unknown>
  try { flight = navigator.locks.request(`${REPLAY_DATABASE_NAME}:writer`, { mode: 'exclusive', ifAvailable: true }, async lock => {
    if (controller.signal.aborted || !current()) throw new BrowserPersistenceError('closed')
    if (!lock) throw new BrowserPersistenceError('writer_busy')
    const db = await openDatabase(controller.signal, current)
    if (controller.signal.aborted || !current()) { db.close(); throw new BrowserPersistenceError('closed') }
    let release!: () => void
    const held = new Promise<void>(yes => { release = yes })
    store = new BrowserReplayStorage(db, scope, controller.signal, current, release, () => flight, cleanup)
    resolve(store)
    await held
  }) } catch (error) { cleanup(); throw persistenceError(error) }
  void flight.catch(error => { cleanup(); reject(persistenceError(error)); if (store) void store.close() })
  return ready
}
