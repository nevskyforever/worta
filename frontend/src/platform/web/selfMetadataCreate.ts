import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { V3Descriptor } from '@/api/encryptedSyncV3'
import type { AccountMasterKey } from '@/crypto'
import { decodeBase64Url } from '@/api/base64url'
import { openProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { metadataProofFor } from './importedMetadataApply'
import { validateBrowserProjectBinding, type BrowserProjectBinding } from './projectBindings'
import { validateBrowserMetadataHead, type BrowserMetadataHead, type BrowserMetadataProof } from './metadataApplyRecords'
import { BrowserSyncDurability, immutableWireIdentity, snapshotBrowserSyncScope, validateBrowserRetainedHistory, validateBrowserSyncCursor, type BrowserRetainedEncryptedHistory, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'

export interface SelfMetadataCreatePlan { readonly selfMetadataCreatePlan: unique symbol }
interface Plan { owner: BrowserSelfMetadataCreate; scope: BrowserSyncScope; lineage: BrowserProjectBinding; retained: BrowserRetainedEncryptedHistory; proof: BrowserMetadataProof; expected: BrowserMetadataHead | null; current: () => boolean; auth: NormalUserAuthRuntime; keys: RuntimeKeyContext; signal?: AbortSignal }
const plans = new WeakMap<object, Plan>()
const same = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b)
function fail(code: 'closed' | 'account_mismatch' | 'missing_history' | 'conflicting_record' | 'stale_authority' | 'stale_lineage' | 'metadata_rejected' | 'ineligible_lineage'): never { throw new BrowserPersistenceError(code) }

/** Reproduce the owner proof from authenticated retained history, outside IDB. */
export async function authenticateSelfMetadataCreate(
  scope: BrowserSyncScope, lineage: BrowserProjectBinding, retained: BrowserRetainedEncryptedHistory, amk: AccountMasterKey,
): Promise<BrowserMetadataProof> {
  const { reference, inbox, outbox, receipt } = retained
  if (!inbox || !outbox || !receipt || outbox.protocol_version !== 3 || inbox.protocol_version !== 3) fail('missing_history')
  if ('scope' in reference || reference.project_id !== lineage.project_id
    || (lineage.initial_metadata_event_id !== undefined && lineage.initial_metadata_event_id !== reference.event_id)) fail('metadata_rejected')
  const raw = JSON.parse(inbox.wire_item)
  const nonce = decodeBase64Url(raw.object.nonce), ciphertext = decodeBase64Url(raw.object.ciphertext)
  try {
    const event = await openProjectMetadataEvent(amk, {
      account_id: scope.canonicalUserId, project_id: reference.project_id, entity_id: reference.entity_id, event_id: reference.event_id,
    }, { crypto_version: raw.object.crypto_version, aad_version: raw.object.aad_version, nonce, ciphertext })
    const identity = new TextEncoder().encode(immutableWireIdentity({ protocol_version: 3, event: reference, object: raw.object }))
    let digest: Uint8Array
    try { digest = new Uint8Array(await crypto.subtle.digest('SHA-256', identity)) } finally { identity.fill(0) }
    const wireDigest = Array.from(digest, b => b.toString(16).padStart(2, '0')).join('')
    digest.fill(0)
    const proof = metadataProofFor(scope, reference, event, wireDigest, 3)
    if (event.version !== 1 || proof.operation !== 'create' || proof.bootstrap_id !== lineage.bootstrap_id || proof.source_device_id !== scope.deviceId) fail('metadata_rejected')
    return proof
  } finally { nonce.fill(0); ciphertext.fill(0) }
}

/** Explicit local author proof; never declares device readiness or advances ACK. */
export class BrowserSelfMetadataCreate {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly durability: BrowserSyncDurability) {}

  async prepare(scope: BrowserSyncScope, reference: V3Descriptor, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext, signal?: AbortSignal): Promise<SelfMetadataCreatePlan> {
    scope = snapshotBrowserSyncScope(scope); reference = structuredClone(reference)
    const { context } = await bindings.ensureForCurrentUser(scope.accountId), lease = keys.leaseForAccount(scope.accountId)
    const current = () => !signal?.aborted && auth.isCurrent(context) && !!lease?.isCurrent()
    if (!lease || context.userId !== scope.canonicalUserId || lease.localAccountId !== scope.accountId || lease.canonicalUserId !== context.userId || lease.authEpoch !== context.authEpoch || !current()) fail('account_mismatch')
    if (reference.device_id !== scope.deviceId || reference.entity_type !== 'project_metadata' || reference.operation !== 'upsert' || reference.revision !== 1 || reference.entity_id !== reference.project_id) fail('metadata_rejected')
    const snapshot = await this.transaction<{ lineage: BrowserProjectBinding; head: BrowserMetadataHead | null }>(scope, ['projectBindings', 'ownerHeads'], 'readonly', (tx, result, abort) => {
      const lineage = tx.objectStore('projectBindings').get([scope.accountId, reference.project_id]), head = tx.objectStore('ownerHeads').get([scope.accountId, 'project_metadata', reference.project_id])
      let count = 0
      const done = () => { if (++count !== 2) return; try {
        if (!lineage.result) fail('missing_history')
        result({ lineage: validateBrowserProjectBinding(lineage.result, scope), head: head.result === undefined ? null : validateBrowserMetadataHead(head.result, scope) })
      } catch (error) { abort(error) } }
      lineage.onsuccess = done; head.onsuccess = done
    })
    if (snapshot.lineage.mode !== 'upload_existing' || !['registered', 'captured', 'completing'].includes(snapshot.lineage.phase) || !['initializing', 'active'].includes(snapshot.lineage.remote_state!)) fail('ineligible_lineage')
    const [retained] = await this.durability.readRetainedHistory(scope, [reference])
    if (!retained?.outbox || !retained.receipt || !retained.inbox || retained.outbox.protocol_version !== 3 || retained.inbox.protocol_version !== 3) fail('missing_history')
    return lease.use(async amk => {
      const proof = await authenticateSelfMetadataCreate(scope, snapshot.lineage, retained, amk)
      if (!current()) fail('closed')
      const token = Object.freeze({}) as SelfMetadataCreatePlan
      plans.set(token, { owner: this, scope, lineage: snapshot.lineage, expected: snapshot.head, retained: structuredClone(retained), proof, current, auth, keys, signal })
      return token
    })
  }

  async commit(scope: BrowserSyncScope, token: SelfMetadataCreatePlan): Promise<'applied' | 'already_applied'> {
    scope = snapshotBrowserSyncScope(scope)
    const p = plans.get(token)
    if (!p || p.owner !== this || !same(p.scope, scope)) fail('metadata_rejected')
    if (!p.current()) fail('closed')
    const cleanup: Array<() => void> = []
    return this.transaction<'applied' | 'already_applied'>(scope, ['projectBindings','ownerHeads','inbox','outbox','receipts','cursors'], 'readwrite', (tx, result, abort) => {
      const revoke = () => abort(new BrowserPersistenceError('closed'))
      cleanup.push(p.auth.onInvalidated(revoke), p.keys.onInvalidated(revoke))
      p.signal?.addEventListener('abort', revoke, { once: true })
      cleanup.push(() => p.signal?.removeEventListener('abort', revoke))
      if (!p.current()) { revoke(); return }
      const id = p.proof.event_id
      const reads = [tx.objectStore('projectBindings').get([scope.accountId, p.proof.project_id]), tx.objectStore('ownerHeads').get([scope.accountId, 'project_metadata', p.proof.project_id]), tx.objectStore('outbox').get([scope.accountId,id]), tx.objectStore('receipts').get([scope.accountId,id]), tx.objectStore('inbox').index('event').get([scope.accountId,id]), tx.objectStore('cursors').get([scope.accountId,scope.deviceId])]
      let count = 0
      const done = () => { if (++count !== reads.length) return; try {
        if (!p.current()) fail('closed')
        const lineage = validateBrowserProjectBinding(reads[0]!.result, scope)
        const head = reads[1]!.result === undefined ? null : validateBrowserMetadataHead(reads[1]!.result, scope)
        const retained = validateBrowserRetainedHistory(scope, p.proof.reference, reads[2]!.result, reads[3]!.result, reads[4]!.result)
        if (!retained.outbox || !retained.receipt || !retained.inbox || validateBrowserSyncCursor(reads[5]!.result, scope).pull_cursor < p.proof.server_sequence) fail('missing_history')
        if (!same(lineage, p.lineage)) fail('stale_lineage')
        const row = retained.inbox
        if (row.state === 'applied') {
          if (row.wire_item !== p.retained.inbox?.wire_item || row.protocol_version !== p.retained.inbox.protocol_version
            || !same(row.applied_proof, p.proof) || !head || !same(head.history.find(v => v.event_id === id), p.proof) || !same(retained.outbox,p.retained.outbox) || !same(retained.receipt,p.retained.receipt)) fail('conflicting_record')
          result('already_applied'); return
        }
        if (!same(retained, p.retained)) fail('conflicting_record')
        if (!same(head, p.expected) || head !== null) fail('stale_authority')
        if (row.state !== 'received') fail('conflicting_record')
        const next = validateBrowserMetadataHead({ record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, owner_kind: 'project_metadata', owner_id: p.proof.project_id, bootstrap_id: p.proof.bootstrap_id, tip_event_ids: [id], history: [p.proof] }, scope)
        const writes = [tx.objectStore('ownerHeads').add(next), tx.objectStore('inbox').put({ ...row, record_version: 2, state: 'applied', applied_proof: p.proof })]
        writes.forEach(write => { write.onsuccess = () => { if (!p.current()) abort(new BrowserPersistenceError('closed')) } })
        result('applied')
      } catch (error) { abort(error) } }
      reads.forEach(read => { read.onsuccess = done })
    }).finally(() => cleanup.forEach(remove => remove()))
  }
}
