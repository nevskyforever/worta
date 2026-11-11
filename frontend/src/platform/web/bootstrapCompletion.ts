import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { AuthoritativeKeyContextLease, RuntimeKeyContext } from '@/auth/keyContext'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import { cloudProjectsApi, type CloudProjectBootstrapCompletion } from '@/api/cloudProjects'
import { BrowserPersistenceError } from './persistence'
import { validateBrowserProjectBinding, type BrowserProjectBinding } from './projectBindings'
import { deriveInitialLegacyCohort } from './initialNoteCapture'
import { authenticateSelfMetadataCreate } from './selfMetadataCreate'
import { validateBrowserInboxRecord, validateBrowserRetainedHistory, validateBrowserSyncCursor, immutableWireIdentity, snapshotBrowserSyncScope,
  type BrowserRetainedEncryptedHistory, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'
import { validateBrowserMetadataHead } from './metadataApplyRecords'

const same = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b)
const range = (account: string) => IDBKeyRange.bound([account], [account, []])
const safe = (n: unknown): n is number => Number.isSafeInteger(n) && (n as number) >= 0
const exact = (v: unknown, keys: string[]): v is Record<string,unknown> => !!v && typeof v === 'object' && !Array.isArray(v) && Object.keys(v).length === keys.length && keys.every(k => Object.hasOwn(v,k))
function fail(code: 'closed' | 'stale_lineage' | 'invalid_lineage_transition' | 'bootstrap_incomplete' | 'bootstrap_response_rejected' | 'bootstrap_completion_failed' | 'corrupt_record'): never { throw new BrowserPersistenceError(code) }
export const BOOTSTRAP_COMPLETION_TIMEOUT_MS = 30_000
function completing(row: BrowserProjectBinding): void { if (row.mode !== 'upload_existing' || row.phase !== 'completing') fail('invalid_lineage_transition') }
const metadataStores = ['projectBindings', 'outbox', 'receipts', 'inbox', 'ownerHeads', 'cursors']

interface MetadataCohort {
  binding: BrowserProjectBinding
  retained: BrowserRetainedEncryptedHistory
  ordinal: number
  sequence: number
}

/** Read every link from the same transaction. Crypto runs on the returned snapshot. */
function readMetadataCohort(
  tx: IDBTransaction, scope: BrowserSyncScope, expected: BrowserProjectBinding,
  result: (cohort: MetadataCohort) => void, abort: (error: unknown) => void,
): void {
  const id = expected.initial_metadata_event_id
  if (!id) fail('invalid_lineage_transition')
  const key = [scope.accountId, id]
  const binding = tx.objectStore('projectBindings').get([scope.accountId, expected.project_id])
  const outbound = tx.objectStore('outbox').get(key)
  const receipt = tx.objectStore('receipts').get(key)
  const inbound = tx.objectStore('inbox').index('event').get(key)
  const owner = tx.objectStore('ownerHeads').get([scope.accountId, 'project_metadata', expected.project_id])
  const cursor = tx.objectStore('cursors').get([scope.accountId, scope.deviceId])
  const reads = [binding, outbound, receipt, inbound, owner, cursor]
  let ready = 0
  const finish = () => {
    if (++ready !== reads.length) return
    try {
      if (!binding.result) fail('stale_lineage')
      const row = validateBrowserProjectBinding(binding.result, scope)
      if (row.initial_metadata_event_id !== id || row.bootstrap_id !== expected.bootstrap_id || row.project_id !== expected.project_id) fail('stale_lineage')
      if (!outbound.result || !receipt.result || !inbound.result || !owner.result) fail('bootstrap_incomplete')
      const inbox = validateBrowserInboxRecord(inbound.result, scope)
      if (inbox.state !== 'applied') fail('bootstrap_incomplete')
      const proof = inbox.applied_proof
      const retained = validateBrowserRetainedHistory(scope, proof.reference, outbound.result, receipt.result, inbox)
      if (!retained.outbox || !retained.receipt || proof.protocol_version !== 3
        || proof.operation !== 'create' || proof.event_id !== id || proof.project_id !== row.project_id
        || proof.bootstrap_id !== row.bootstrap_id || proof.source_device_id !== scope.deviceId
        || validateBrowserSyncCursor(cursor.result, scope).pull_cursor < proof.server_sequence) fail('bootstrap_incomplete')
      const head = validateBrowserMetadataHead(owner.result, scope)
      if (head.owner_id !== row.project_id || head.bootstrap_id !== row.bootstrap_id
        || head.history.length !== 1 || !same(head.history[0], proof)) fail('bootstrap_incomplete')
      const ordinal = retained.outbox.local_ordinal, sequence = proof.server_sequence
      if ((row.phase !== 'registered' && row.initial_local_ordinal_hi !== ordinal)
        || (row.phase === 'completing' && row.initial_max_server_sequence !== sequence)) fail('bootstrap_incomplete')
      result({ binding: row, retained, ordinal, sequence })
    } catch (error) { abort(error) }
  }
  reads.forEach(read => { read.onsuccess = finish })
}

/** Exact backend response, not HTTP2xx, proves remote active. No caller-supplied evidence commit API. */
export function validateBootstrapCompletionResponse(scope: BrowserSyncScope, expected: BrowserProjectBinding, value: unknown): BrowserProjectBinding {
  const old = validateBrowserProjectBinding(expected,scope); completing(old)
  if (!exact(value,['project','current_cursor']) || !safe(value.current_cursor) || !exact(value.project,['project_id','bootstrap_id','origin_device_id','state','initial_event_count','initial_max_server_sequence'])) fail('bootstrap_response_rejected')
  const p = value.project
  if (p.project_id !== old.project_id || p.bootstrap_id !== old.bootstrap_id || p.origin_device_id !== old.device_id || p.state !== 'active'
    || p.initial_event_count !== old.initial_event_count || p.initial_max_server_sequence !== old.initial_max_server_sequence
    || value.current_cursor < old.remote_high_water! || value.current_cursor < old.initial_max_server_sequence!) fail('bootstrap_response_rejected')
  return validateBrowserProjectBinding({...old,remote_state:'active',remote_high_water:value.current_cursor},scope)
}
export class BrowserBootstrapCompletion {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly sessionCurrent: () => boolean) {}
  bind(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext): BrowserBootstrapCompletionRuntime { return new BrowserBootstrapCompletionRuntime(this.transaction,this.sessionCurrent,auth,bindings,keys) }
}
export class BrowserBootstrapCompletionRuntime {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly sessionCurrent: () => boolean, private readonly auth: NormalUserAuthRuntime, private readonly bindings: AuthoritativeAccountBinding, private readonly keys: RuntimeKeyContext) {}
  private async guard(scope: BrowserSyncScope, signal?: AbortSignal) {
    if (signal?.aborted || !this.sessionCurrent() || this.auth.state !== 'authenticated') fail('closed')
    const {context} = await this.bindings.ensureForCurrentUser(scope.accountId), lease = this.keys.leaseForAccount(scope.accountId)
    const current = () => !signal?.aborted && this.sessionCurrent() && this.auth.isCurrent(context) && !!lease?.isCurrent()
    if (!lease || context.userId !== scope.canonicalUserId || lease.canonicalUserId !== context.userId || lease.localAccountId !== scope.accountId || lease.authEpoch !== context.authEpoch) throw new BrowserPersistenceError('account_mismatch')
    if (!current()) fail('closed')
    return { current, lease }
  }
  private async metadataProof(scope: BrowserSyncScope, expected: BrowserProjectBinding, lease: AuthoritativeKeyContextLease, current: () => boolean): Promise<MetadataCohort> {
    const cohort = await this.transaction<MetadataCohort>(scope, metadataStores, 'readonly', (tx, result, abort) =>
      readMetadataCohort(tx, scope, expected, result, abort))
    if (!current()) fail('closed')
    const inbox = cohort.retained.inbox
    if (inbox?.state !== 'applied') fail('bootstrap_incomplete')
    const proof = await lease.use(amk => authenticateSelfMetadataCreate(scope, cohort.binding, cohort.retained, amk))
    if (!current()) fail('closed')
    if (!same(proof, inbox.applied_proof)) fail('bootstrap_incomplete')
    return cohort
  }

  private async commitMetadata(
    scope: BrowserSyncScope, expected: BrowserProjectBinding, target: BrowserProjectBinding,
    verified: MetadataCohort, current: () => boolean, signal?: AbortSignal,
  ): Promise<BrowserProjectBinding> {
    const cleanup: Array<() => void> = []
    return this.transaction<BrowserProjectBinding>(scope, metadataStores, 'readwrite', (tx, result, abort) => {
      const revoke = () => abort(new BrowserPersistenceError('closed'))
      cleanup.push(this.auth.onInvalidated(revoke), this.keys.onInvalidated(revoke))
      signal?.addEventListener('abort', revoke, { once: true })
      cleanup.push(() => signal?.removeEventListener('abort', revoke))
      if (!current()) { revoke(); return }
      readMetadataCohort(tx, scope, expected, actual => {
        if (!current()) fail('closed')
        if (!same(actual.retained, verified.retained)) fail('stale_lineage')
        if (same(actual.binding, target)) { result(actual.binding); return }
        if (!same(actual.binding, expected) || !same(verified.binding, expected)) fail('stale_lineage')
        const write = tx.objectStore('projectBindings').put(target)
        write.onsuccess = () => { if (!current()) revoke() }
        result(target)
      }, abort)
    }).finally(() => cleanup.forEach(remove => remove()))
  }

  /** Freeze the one authenticated Create as the initial cohort. */
  async captureMetadataCreate(scope: BrowserSyncScope, expected: BrowserProjectBinding, signal?: AbortSignal): Promise<BrowserProjectBinding> {
    scope = snapshotBrowserSyncScope(scope)
    expected = validateBrowserProjectBinding(expected, scope)
    if (!expected.initial_metadata_event_id || !['registered', 'captured'].includes(expected.phase)) fail('invalid_lineage_transition')
    const { current, lease } = await this.guard(scope, signal)
    const verified = await this.metadataProof(scope, expected, lease, current)
    const target = validateBrowserProjectBinding({ ...expected, phase: 'captured', initial_event_count: 1,
      initial_local_ordinal_hi: verified.ordinal }, scope)
    return this.commitMetadata(scope, expected, target, verified, current, signal)
  }

  private async proof(scope: BrowserSyncScope, expected: BrowserProjectBinding, current: () => boolean): Promise<BrowserProjectBinding> {
    return this.transaction(scope,['projectBindings','outbox','receipts'],'readonly',(tx,result,abort) => {
      this.read(tx,scope,(row,out,receipts) => { try {
        if (!current()) fail('closed'); if (!same(row,expected)) fail('stale_lineage'); completing(row)
        deriveInitialLegacyCohort(scope,row,out,receipts); result(row)
      } catch(e) {abort(e)} },abort,expected.project_id)
    })
  }
  private read(tx: IDBTransaction, scope: BrowserSyncScope, consume: (row: BrowserProjectBinding, out: unknown[], receipts: unknown[]) => void, abort: (e: unknown) => void, projectId: string): void {
    // Project ID is supplied by the verified expected binding, never a registry sweep.
    const binding = tx.objectStore('projectBindings').get([scope.accountId,projectId]), out = tx.objectStore('outbox').getAll(range(scope.accountId)), receipts = tx.objectStore('receipts').getAll(range(scope.accountId))
    let count = 0
    const finish = () => { if (++count !== 3) return; try {consume(validateBrowserProjectBinding(binding.result,scope),out.result,receipts.result)} catch(e){abort(e)} }
    binding.onsuccess=finish;out.onsuccess=finish;receipts.onsuccess=finish
  }
  async markCompleting(scope: BrowserSyncScope, expected: BrowserProjectBinding, signal?: AbortSignal): Promise<BrowserProjectBinding> {
    scope=snapshotBrowserSyncScope(scope);expected=validateBrowserProjectBinding(expected,scope)
    if (expected.mode !== 'upload_existing' || !['captured','completing'].includes(expected.phase)) fail('invalid_lineage_transition')
    const { current, lease } = await this.guard(scope, signal)
    if (expected.initial_metadata_event_id) {
      const verified = await this.metadataProof(scope, expected, lease, current)
      const target = validateBrowserProjectBinding({ ...expected, phase: 'completing',
        initial_max_server_sequence: verified.sequence }, scope)
      if (expected.phase === 'completing' && !same(target, expected)) fail('stale_lineage')
      return this.commitMetadata(scope, expected, target, verified, current, signal)
    }
    let unsubscribe:(()=>void)|undefined
    return this.transaction<BrowserProjectBinding>(scope,['projectBindings','outbox','receipts'],'readwrite',(tx,result,abort)=>{
      unsubscribe=this.auth.onInvalidated(()=>abort(new BrowserPersistenceError('closed')))
      this.read(tx,scope,(row,out,receipts)=>{try{
        if(!current())fail('closed')
        const status=deriveInitialLegacyCohort(scope,row,out,receipts).status
        if(!status.complete || status.accepted_count!==row.initial_event_count)fail('bootstrap_incomplete')
        if(expected.phase==='completing'&&!same(row,expected))fail('stale_lineage')
        const target=validateBrowserProjectBinding({...expected,phase:'completing',initial_max_server_sequence:status.max_server_sequence},scope)
        if(row.phase==='completing'){
          if(!same(row,target))fail('stale_lineage');result(row);return
        }
        if(row.mode!=='upload_existing'||row.phase!=='captured'||row.remote_state!=='initializing')fail('invalid_lineage_transition')
        if(!same(row,expected))fail('stale_lineage')
        const write=tx.objectStore('projectBindings').put(target)
        write.onsuccess=()=>{if(!current())abort(new BrowserPersistenceError('closed'))}
        result(target)
      }catch(e){abort(e)}},abort,expected.project_id)
    }).finally(()=>unsubscribe?.())
  }
  async completeRemote(scope: BrowserSyncScope, expected: BrowserProjectBinding, signal?: AbortSignal): Promise<BrowserProjectBinding> {
    scope=snapshotBrowserSyncScope(scope);expected=validateBrowserProjectBinding(expected,scope);completing(expected)
    const { current, lease } = await this.guard(scope, signal)
    const metadata = expected.initial_metadata_event_id ? await this.metadataProof(scope, expected, lease, current) : null
    const old = metadata ? metadata.binding : await this.proof(scope, expected, current)
    if (!same(old, expected)) fail('stale_lineage')
    if(old.remote_state==='active')return old // Exact durable replay; no repeat HTTP/ready.
    const body:CloudProjectBootstrapCompletion={bootstrap_id:old.bootstrap_id,device_id:old.device_id,initial_event_count:old.initial_event_count,initial_max_server_sequence:old.initial_max_server_sequence!}
    const controller=new AbortController(),timeout=setTimeout(()=>controller.abort(),BOOTSTRAP_COMPLETION_TIMEOUT_MS)
    const requestSignal = signal ? AbortSignal.any([signal, controller.signal]) : controller.signal
    const unsubscribe=this.auth.onInvalidated(()=>controller.abort())
    const unsubscribeKey = metadata ? this.keys.onInvalidated(() => controller.abort()) : undefined
    let target:BrowserProjectBinding
    try{
      if(!current())fail('closed')
      const response=await this.auth.authorized(token=>{if(!current())fail('closed');return cloudProjectsApi.completeBootstrap(token,old.project_id,body,requestSignal)})
      if(!current()||response.context.userId!==scope.canonicalUserId)fail('closed')
      target=validateBootstrapCompletionResponse(scope,old,response.value)
    }catch(e){if(!current())fail('closed');if(e instanceof BrowserPersistenceError)throw e;fail('bootstrap_completion_failed')}
    finally{clearTimeout(timeout);unsubscribe();unsubscribeKey?.()}
    // HTTP has finished. Never keep a transaction alive across the request.
    if (metadata) return this.commitMetadata(scope, old, target, metadata, current, signal)
    let unsub:(()=>void)|undefined
    return this.transaction<BrowserProjectBinding>(scope,['projectBindings','outbox','receipts'],'readwrite',(tx,result,abort)=>{
      unsub=this.auth.onInvalidated(()=>abort(new BrowserPersistenceError('closed')))
      this.read(tx,scope,(row,out,receipts)=>{try{
        if(!current())fail('closed')
        deriveInitialLegacyCohort(scope,row,out,receipts)
        if(same(row,target)){result(row);return}
        if(!same(row,old))fail('stale_lineage')
        const write=tx.objectStore('projectBindings').put(target)
        write.onsuccess=()=>{if(!current())abort(new BrowserPersistenceError('closed'))}
        result(target)
      }catch(e){abort(e)}},abort,old.project_id)
    }).finally(()=>unsub?.())
  }
}
