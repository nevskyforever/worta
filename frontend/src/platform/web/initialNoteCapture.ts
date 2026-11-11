import {CONTENT_AUTHORITY_STORES,readContentAuthorityInventory,authenticateContentAuthorityInventory,contentAuthorityAnchor,type ContentAuthorityInventory} from './contentAuthorityInventory'
import {validateNoteIntentRecord} from './noteIntentRecords'
import { NOTE_INTENT_STORE } from './noteIntentRecords'
import { makeHistoryCandidate,nextNoteHead,historyRange } from './noteHistory'
import { NOTE_HISTORY_STORE,validateNoteHistorySet,type BrowserNoteCandidate } from './noteHistoryRecords'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import { encryptObjectBytes, type ObjectCryptoEnvelope } from '@/crypto'
import { encodeBase64Url } from '@/api/base64url'
import { encodeV2Push } from '@/api/encryptedSyncV2'
import { createNoteSyncPlaintext, validateEncryptedSyncCryptoContext } from '@/cloud/encryptedSyncProtocol'
import { encodeNoteSyncPlaintext, noteSyncEligibility } from '@/cloud/noteSyncCodec'
import { BrowserPersistenceError } from './persistence'
import { BrowserLocalAuthority, assertLocalSourceInTransaction, type LocalSourceToken } from './localAuthority'
import { BrowserProjectBindings, validateBrowserProjectBinding, type BrowserProjectBinding } from './projectBindings'
import { immutableWireIdentity, readBrowserSyncCursor, snapshotBrowserSyncScope, validateBrowserSealedBody, validateBrowserInboxRecord, validateBrowserStoredUploadPair, type BrowserOutboxRecord, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'

export interface InitialLegacyNotePlan { readonly verifiedInitialLegacyNotePlan: unique symbol }
export interface InitialLegacyCohortStatus { event_count: number; accepted_count: number; complete: boolean; max_server_sequence: number }
export interface InitialLegacyCaptureResult { status: 'captured' | 'already_captured'; binding: BrowserProjectBinding }
interface PreparedEvent { noteId: string; eventId: string; raw: string; candidate:BrowserNoteCandidate }
interface Plan { authorityAnchor:string;owner: BrowserInitialNoteCaptureRuntime; scope: BrowserSyncScope; binding: BrowserProjectBinding; source: LocalSourceToken; sourceRevision: number; events: PreparedEvent[]; current: () => boolean }
const plans = new WeakMap<object, Plan>()
const same = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b)
const range = (account: string) => IDBKeyRange.bound([account], [account, []])
function fail(code: 'corrupt_record' | 'conflicting_record' | 'stale_source' | 'stale_lineage' | 'invalid_lineage_transition' | 'closed' | 'unsupported_local_source' | 'source_limit' = 'corrupt_record'): never { throw new BrowserPersistenceError(code) }
function eligible(row: BrowserProjectBinding): void {
  if (row.initial_metadata_event_id || row.mode !== 'upload_existing' || row.phase !== 'registered' || row.remote_state !== 'initializing') fail('invalid_lineage_transition')
}
function matchesCaptured(current: BrowserProjectBinding, expected: BrowserProjectBinding): void {
  if (current.initial_metadata_event_id || current.mode !== 'upload_existing' || current.phase !== 'captured') fail('invalid_lineage_transition')
  if (!same({ ...current, phase: 'registered', initial_event_count: 0, initial_local_ordinal_hi: 0 }, expected.phase === 'captured' ? { ...expected, phase: 'registered', initial_event_count: 0, initial_local_ordinal_hi: 0 } : expected)) fail('stale_lineage')
  if (expected.phase === 'captured' && !same(current, expected)) fail('stale_lineage')
}

/** Count+HI encode the EXACT contiguous allocation interval, not the whole prefix.
 * Only this producer writes upload_existing/captured; all producers append monotonically;
 * existing immutable ordinal index prevents replacement. No manifest/new record shape needed.
 */
export function deriveInitialLegacyCohort(scope: BrowserSyncScope, value: unknown, outbound: unknown[], receipts: unknown[]): { status: InitialLegacyCohortStatus; rows: BrowserOutboxRecord[] } {
  const binding = validateBrowserProjectBinding(value, scope)
  if (binding.initial_metadata_event_id || binding.mode !== 'upload_existing' || !['captured','completing'].includes(binding.phase)) fail('invalid_lineage_transition')
  const count = binding.initial_event_count, hi = binding.initial_local_ordinal_hi, lo = hi - count + 1
  const candidates = outbound.filter(v => { const r = v as BrowserOutboxRecord; return r.account_id === scope.accountId && r.device_id === scope.deviceId && count > 0 && r.local_ordinal >= lo && r.local_ordinal <= hi })
  if (candidates.length !== count) fail()
  const rows = candidates.map(v => validateBrowserStoredUploadPair(v, receipts.find(p => (p as { account_id: string; event_id: string }).account_id === scope.accountId && (p as { event_id: string }).event_id === (v as BrowserOutboxRecord).event_id), scope)).sort((a,b) => a.local_ordinal - b.local_ordinal)
  const entities = new Set<string>(), events = new Set<string>(), sequences = new Set<number>()
  let accepted = 0, max = 0, previousId: string | undefined
  rows.forEach((row, i) => {
    const e = JSON.parse(row.raw_body).items[0].event
    if (row.local_ordinal !== lo + i || e.project_id !== binding.project_id || e.entity_type !== 'note' || e.operation !== 'upsert' || e.revision !== 1 || e.deleted_at !== null || entities.has(e.entity_id) || events.has(row.event_id) || previousId !== undefined && previousId >= e.entity_id) fail()
    entities.add(e.entity_id); events.add(row.event_id); previousId = e.entity_id
    const pair = receipts.filter(p => (p as {account_id: string;event_id: string}).account_id === scope.accountId && (p as {event_id: string}).event_id === row.event_id)
    if (pair.length > 1) fail()
    if (row.state === 'accepted') { const sequence = (pair[0] as {server_sequence: number}).server_sequence; if (sequences.has(sequence) || receipts.some(p => (p as {account_id: string;event_id: string;server_sequence: number}).account_id === scope.accountId && (p as {server_sequence: number}).server_sequence === sequence && (p as {event_id: string}).event_id !== row.event_id)) fail('conflicting_record'); sequences.add(sequence); accepted++; max = Math.max(max, sequence) }
  })
  if (binding.phase === 'completing' && (accepted !== count || max !== binding.initial_max_server_sequence)) fail()
  return { rows, status: { event_count: count, accepted_count: accepted, complete: accepted === count, max_server_sequence: max } }
}

/** Dormant factory: normal auth+keys only; no HTTP, arbitrary plan DTO or phase setter. */
export class BrowserInitialNoteCapture {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly local: BrowserLocalAuthority, private readonly projects: BrowserProjectBindings) {}
  bind(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext): BrowserInitialNoteCaptureRuntime { return new BrowserInitialNoteCaptureRuntime(this.transaction, this.local, this.projects, auth, bindings, keys) }
}
export class BrowserInitialNoteCaptureRuntime {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly local: BrowserLocalAuthority, private readonly projects: BrowserProjectBindings, private readonly auth: NormalUserAuthRuntime, private readonly bindings: AuthoritativeAccountBinding, private readonly keys: RuntimeKeyContext) {}
  private async guard(scope: BrowserSyncScope): Promise<() => boolean> {
    const { context } = await this.bindings.ensureForCurrentUser(scope.accountId), lease = this.keys.leaseForAccount(scope.accountId)
    const current = () => this.auth.isCurrent(context) && !!lease?.isCurrent()
    if (!lease || context.userId !== scope.canonicalUserId || lease.canonicalUserId !== scope.canonicalUserId || lease.localAccountId !== scope.accountId || lease.authEpoch !== context.authEpoch || !current()) throw new BrowserPersistenceError('account_mismatch')
    return current
  }
  async prepare(scope: BrowserSyncScope, expected: BrowserProjectBinding): Promise<InitialLegacyNotePlan> {
    scope = snapshotBrowserSyncScope(scope); expected = validateBrowserProjectBinding(expected, scope); eligible(expected)
    const current = await this.guard(scope)
    const authority=await this.transaction<ContentAuthorityInventory>(scope,CONTENT_AUTHORITY_STORES,'readonly',(tx,result,abort)=>readContentAuthorityInventory(tx,scope,result,abort))
    if (!same(await this.projects.read(scope, expected.project_id), expected)) fail('stale_lineage')
    const source = this.local.bind(this.auth, this.bindings, this.keys), snapshot = await source.readSnapshot(scope, expected.project_id)
    const events = await source.withDecryptedSnapshot(scope, snapshot.token, async data => {
      // Validate ALL live/deleted source members before any cloud event preparation.
      const members = data.notes.filter(n => n.state === 'live')
      const prepared = members.map(member => {
        if (member.state !== 'live') fail()
        const { revision: _nativeLocalRevision, ...note } = member.note
        const event = { event_id: crypto.randomUUID(), project_id: expected.project_id, entity_id: note.id, entity_type: 'note' as const, operation: 'upsert' as const, revision: 1, updated_at: note.updated_at, deleted_at: null }
        const value = createNoteSyncPlaintext(event, null, note)
        if (!noteSyncEligibility(value.plaintext).eligible) fail('unsupported_local_source')
        return { noteId: note.id, ...value }
      })
      const lease = this.keys.leaseForAccount(scope.accountId)
      if (!lease || !current()) fail('closed')
      return lease.use(async amk => {
        await authenticateContentAuthorityInventory(scope,authority,amk,current)
        const result: PreparedEvent[] = []
        for (const p of prepared) {
          let bytes: Uint8Array | undefined, object: ObjectCryptoEnvelope | undefined
          try {
            // Exact legacy Note writer path: canonical C15 JSON, unchanged object tuple/AAD.
            // Legacy Notes are NOT framed C18 Content Notes; no invented compression/frame gate.
            bytes = encodeNoteSyncPlaintext(p.plaintext)
            const context = { userId: scope.canonicalUserId, projectId: p.event.project_id, entityId: p.event.entity_id, entityType: p.event.entity_type }
            validateEncryptedSyncCryptoContext(context)
            object = await encryptObjectBytes(amk, context, bytes)
            const raw = encodeV2Push({ protocol_version: 2, encrypted_sync_version: 2, device_id: scope.deviceId, items: [{ event: { ...p.event, entity_type: 'note', operation: 'upsert', deleted_at: null }, object: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) } }] })
            const sealed = validateBrowserSealedBody(raw, scope)
            result.push({ noteId: p.noteId, eventId: sealed.eventId, raw, candidate:await makeHistoryCandidate(scope,expected,2,scope.deviceId,JSON.parse(raw).items[0],p.plaintext,'initial_capture',snapshot.notes.find(n=>n.note_id===p.noteId)!) })
            if (!current()) fail('closed')
          } finally { bytes?.fill(0); object?.nonce.fill(0); object?.ciphertext.fill(0) }
        }
        return result
      })
    })
    if (!current()) fail('closed')
    const token = Object.freeze({}) as InitialLegacyNotePlan
    plans.set(token, { authorityAnchor:contentAuthorityAnchor(scope,authority),owner: this, scope, binding: expected, source: snapshot.token, sourceRevision: snapshot.source_revision, events, current })
    return token
  }
  async commit(scope: BrowserSyncScope, token: InitialLegacyNotePlan): Promise<InitialLegacyCaptureResult> {
    scope = snapshotBrowserSyncScope(scope)
    const p = token && plans.get(token)
    if (!p || p.owner !== this || !same(p.scope, scope)) fail('conflicting_record')
    if (!p.current()) fail('closed')
    let unsubscribe: (() => void) | undefined
    return this.transaction<InitialLegacyCaptureResult>(scope, ['localProjects','localNotes','projectBindings','outbox','inbox','cursors','receipts',NOTE_HISTORY_STORE,NOTE_INTENT_STORE], 'readwrite', (tx, result, abort) => {
      unsubscribe = this.auth.onInvalidated(() => abort(new BrowserPersistenceError('closed')))
      const binding = tx.objectStore('projectBindings').get([scope.accountId,p.binding.project_id]), out = tx.objectStore('outbox').getAll(range(scope.accountId)), receipts = tx.objectStore('receipts').getAll(range(scope.accountId)), incoming = tx.objectStore('inbox').getAll(range(scope.accountId)), history=tx.objectStore(NOTE_HISTORY_STORE).getAll(historyRange(scope,p.binding.project_id))
      const intents=tx.objectStore(NOTE_INTENT_STORE).getAll(range(scope.accountId))
      let reads = 0
      const finish = () => { if (++reads !== 6) return; try {
        if (!p.current()) fail('closed')
        const old = validateBrowserProjectBinding(binding.result, scope)
        if (old.phase === 'captured' && old.mode === 'upload_existing') {
          matchesCaptured(old, p.binding)
          const cohort = deriveInitialLegacyCohort(scope, old, out.result, receipts.result)
          if (!same(cohort.rows.map(r => ({ noteId: JSON.parse(r.raw_body).items[0].event.entity_id, eventId: r.event_id, raw: r.raw_body })), p.events.map(e=>({noteId:e.noteId,eventId:e.eventId,raw:e.raw})))) fail('conflicting_record')
          result({ status: 'already_captured', binding: old }); return // No current source read/re-capture.
        }
        readContentAuthorityInventory(tx,scope,authority=>{try{if(contentAuthorityAnchor(scope,authority)!==p.authorityAnchor)fail('stale_source')
        eligible(old); if (!same(old, p.binding)) fail('stale_lineage')
        assertLocalSourceInTransaction(tx, scope, p.source, () => {
          readBrowserSyncCursor(tx, scope, state => { try {
            if (!p.current()) fail('closed')
            const rows = out.result.map(v => validateBrowserStoredUploadPair(v, receipts.result.find(receipt => receipt.event_id === v.event_id), scope))
            if (receipts.result.some(receipt => !rows.some(r => r.event_id === receipt.event_id))) fail()
            const pending=intents.result.map(v=>validateNoteIntentRecord(v,scope)),ordinals=[...rows,...pending].map(r=>r.local_ordinal).sort((a,b)=>a-b),highest=ordinals.at(-1)??0
            if(ordinals.some((v,i)=>v!==i+1)||state.next_local_ordinal!==highest+1)fail()
            const count = p.events.length, end = state.next_local_ordinal + count
            if (!Number.isSafeInteger(end)) fail('source_limit')
            const inbox = incoming.result.map(value => validateBrowserInboxRecord(value, scope))
            const priorNote = (event: { project_id?: string; entity_type?: string; entity_id?: string }) => event.project_id === old.project_id && event.entity_type === 'note' && p.events.some(e => e.noteId === event.entity_id)
            if (rows.some(r => priorNote(JSON.parse(r.raw_body).items[0].event)) || inbox.some(r => priorNote(JSON.parse(r.wire_item).event))) fail('conflicting_record') // A previous cloud Note cannot become a second genesis.
            const ids = new Set<string>()
            const additions = p.events.map((e, i): BrowserOutboxRecord => {
              if (ids.has(e.eventId) || inbox.some(r => r.event_id === e.eventId) || rows.some(r => r.event_id === e.eventId || r.local_ordinal === state.next_local_ordinal + i)) fail('conflicting_record')
              ids.add(e.eventId)
              return { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, event_id: e.eventId, local_ordinal: state.next_local_ordinal + i, protocol_version: 2, state: 'sealed', raw_body: e.raw }
            })
            const target = validateBrowserProjectBinding({ ...old, phase: 'captured', initial_event_count: count, initial_local_ordinal_hi: count ? end - 1 : 0 }, scope)
            // Check exact proof against the rows before queueing any changes.
            deriveInitialLegacyCohort(scope, target, [...rows,...additions], receipts.result)
            const oldHistory=validateNoteHistorySet(scope,history.result)
            if(oldHistory.some(r=>p.events.some(e=>e.noteId===r.note_id)))fail('conflicting_record')
            const historyWrites=p.events.flatMap(e=>[tx.objectStore(NOTE_HISTORY_STORE).add(e.candidate),tx.objectStore(NOTE_HISTORY_STORE).add(nextNoteHead(scope,target,[e.candidate],e.eventId))])
            const writes = [...historyWrites,...additions.map(r => tx.objectStore('outbox').add(r))]
            if (count) writes.push(tx.objectStore('cursors').put({ ...state, next_local_ordinal: end }))
            writes.push(tx.objectStore('projectBindings').put(target))
            writes.forEach(w => { w.onsuccess = () => { if (!p.current()) abort(new BrowserPersistenceError('closed')) } })
            result({ status: 'captured', binding: target })
          } catch (e) { abort(e) } }, abort)
        }, abort)
        }catch(e){abort(e)}},abort)
      } catch (e) { abort(e) } }
      binding.onsuccess = finish; out.onsuccess = finish; receipts.onsuccess = finish; incoming.onsuccess = finish; history.onsuccess=finish; intents.onsuccess=finish
    }).finally(() => unsubscribe?.())
  }
  async capture(scope: BrowserSyncScope, expected: BrowserProjectBinding): Promise<InitialLegacyCaptureResult> {
    scope = snapshotBrowserSyncScope(scope); expected = validateBrowserProjectBinding(expected, scope)
    const current = await this.guard(scope), stored = await this.projects.read(scope, expected.project_id)
    if (stored?.mode === 'upload_existing' && stored.phase === 'captured') {
      if (expected.phase !== 'captured') eligible(expected)
      matchesCaptured(stored, expected); await this.readStatus(scope, stored)
      if (!current()) fail('closed')
      return { status: 'already_captured', binding: stored }
    }
    return this.commit(scope, await this.prepare(scope, expected))
  }
  async readStatus(scope: BrowserSyncScope, expected: BrowserProjectBinding): Promise<InitialLegacyCohortStatus> {
    scope = snapshotBrowserSyncScope(scope); expected = validateBrowserProjectBinding(expected, scope)
    return this.transaction(scope, ['projectBindings','outbox','receipts'], 'readonly', (tx, result, abort) => {
      const b = tx.objectStore('projectBindings').get([scope.accountId,expected.project_id]), out = tx.objectStore('outbox').getAll(range(scope.accountId)), receipts = tx.objectStore('receipts').getAll(range(scope.accountId))
      let read = 0
      const finish = () => { if (++read !== 3) return; try { const current = validateBrowserProjectBinding(b.result, scope); if (!same(current, expected)) fail('stale_lineage'); result(deriveInitialLegacyCohort(scope, current, out.result, receipts.result).status) } catch (e) { abort(e) } }
      b.onsuccess = finish; out.onsuccess = finish; receipts.onsuccess = finish
    })
  }
}
