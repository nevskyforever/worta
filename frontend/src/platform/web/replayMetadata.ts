import { BrowserV10Transport } from './v10Transport'
import { BrowserResolutionContinuation } from './resolutionContinuation'
import { BrowserResolutionPlanner } from './resolutionPlanner'
import { applyReplayResolutionOwner, openReplayContent } from './replayResolution'
import { BrowserDeviceRegistration } from './deviceRegistration'
import { CryptoError } from '@/crypto'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api, type V3Descriptor } from '@/api/encryptedSyncV3'
import { BrowserSyncDurability, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'
import { BrowserStageApply, prepareStagePrefix, stageSequences } from './stageApply'
import { BrowserStageOrderApply, prepareStageOrderPrefix, stageOrderSequences } from './stageOrderApply'
import { BrowserImportedMetadataApply } from './importedMetadataApply'
import { prepareMetadataPrefix, verifiedMetadataSequences } from './metadataPrefixProof'
import { BrowserReplayError, prepareReplayPage, replayAnchor, replaySame } from './replayRecords'
import type { ReplaySnapshot } from './replayInventory'
import type { AccountMasterKey } from '@/crypto'
import { reauthenticateReplayNotes } from './replayNoteProof'
import { BrowserLocalAuthority } from './localAuthority'
import { BrowserLegacyNoteApply } from './legacyNoteApply'
import { openNoteTransport } from './noteTransport'

export interface ReplayMetadataResult {
  readonly kind: 'metadata_owner_applied'; readonly cursor: number; readonly ownerProofs: number; readonly projects: number
  readonly generation: string; readonly projectOpened: false; readonly ready: false; readonly declarationAllowed: false; readonly writerAuthorized: false
}
export interface ReplayStructuralResult extends Omit<ReplayMetadataResult, 'kind'> {
  readonly kind: 'structural_owners_applied'; readonly metadataProofs: number; readonly stageProofs: number; readonly stageOrderProofs: number
}
export interface ReplayInitialNoteResult extends Omit<ReplayStructuralResult, 'kind'> {
  readonly kind: 'initial_legacy_notes_applied'; readonly noteProofs: number
}
export interface ReplayOrderedContentResult extends Omit<ReplayInitialNoteResult, 'kind'> {
  readonly kind: 'bounded_ordered_content_update_applied' | 'bounded_peer_content_conflict_preserved' | 'bounded_resolution_owner_applied' | 'bounded_resolution_continuation_applied' | 'bounded_resolution_chain_applied'; readonly contentProofs: number
}
export interface MetadataPort {
  read(): Promise<ReplaySnapshot>
  transaction(expected: ReplaySnapshot, sequence: number, server: () => Promise<void>): BrowserSyncTransaction
}

/** Account-wide admission only. It neither advances coverage nor creates an owner proof. */
export async function assertReplayHistoryAdmissible(snapshot: ReplaySnapshot, scope: BrowserSyncScope,
  structural = false, initialNotes = false, terminalContentAmk?: AccountMasterKey): Promise<void> {
  if (!snapshot.manifest || snapshot.manifest.phase !== 'received'
    || snapshot.rows.length !== snapshot.manifest.anchor.registry.current_cursor || !snapshot.rows.length)
    throw new BrowserReplayError('metadata_history_incomplete')
  const anchor = snapshot.manifest.anchor
  const last = snapshot.rows.slice(-2)
  let contentGenesisSequence: number | undefined
  const peerConflict = !!terminalContentAmk && snapshot.manifest.contract_version === 6 && last.length === 2
    && last.every(value => { const event = JSON.parse(value.wire_item).event; return event.entity_type === 'note' && event.operation === 'event' })
  if (peerConflict) {
    const [first, second] = last as [typeof snapshot.rows[number], typeof snapshot.rows[number]]
    const a = JSON.parse(first.wire_item).event as V3Descriptor, b = JSON.parse(second.wire_item).event as V3Descriptor
    const parents = snapshot.rows.slice(0, -2).filter(value => { const event = JSON.parse(value.wire_item).event; return event.entity_type === 'note' && event.project_id === a.project_id && event.entity_id === a.entity_id })
    const base = parents.at(-1)
    if (!base || a.project_id !== b.project_id || a.entity_id !== b.entity_id || a.device_id === b.device_id
      || a.device_id === scope.deviceId || b.device_id === scope.deviceId || a.revision !== b.revision
      || a.deleted_at !== null || b.deleted_at !== null || first.protocol_version !== 3 || second.protocol_version !== 3
      || !['received', 'content_note_proven'].includes(first.state) || !['received', 'content_note_conflict_preserved'].includes(second.state))
      throw new BrowserReplayError('unsupported_content_cohort')
    const baseEvent = JSON.parse(base.wire_item).event as V3Descriptor
    const contentGenesis = baseEvent.operation === 'event'
    const seed = snapshot.manifest.source_seeds?.find(value => value.project_id === baseEvent.project_id)
    if (contentGenesis && (parents.length !== 1 || !seed || base.server_sequence <= seed.initial_max_server_sequence
      || base.server_sequence !== snapshot.rows.length - 2 || baseEvent.revision !== 1
      || baseEvent.device_id === scope.deviceId || !['received', 'content_note_proven'].includes(base.state)))
      throw new BrowserReplayError('unsupported_content_cohort')
    if (contentGenesis) contentGenesisSequence = base.server_sequence
    const parent = await openNoteTransport(scope, terminalContentAmk!, 3, base.wire_item, contentGenesis ? 'content_note_v1' : 'legacy')
    const one = await openNoteTransport(scope, terminalContentAmk!, 3, first.wire_item, 'content_note_v1')
    const two = await openNoteTransport(scope, terminalContentAmk!, 3, second.wire_item, 'content_note_v1')
    if (contentGenesis && (parent.content?.event.mutation !== 'create' || parent.plaintext.header.revision !== 1 || parent.plaintext.header.parent_event_id !== null)
      || one.content?.event.mutation !== 'update' || two.content?.event.mutation !== 'update'
      || one.plaintext.header.parent_event_id !== parent.plaintext.header.event_id
      || two.plaintext.header.parent_event_id !== parent.plaintext.header.event_id
      || one.plaintext.header.revision !== parent.plaintext.header.revision + 1
      || two.plaintext.header.revision !== parent.plaintext.header.revision + 1)
      throw new BrowserReplayError('unsupported_content_cohort')
  }
  for (const [index, row] of snapshot.rows.entries()) {
    const event = JSON.parse(row.wire_item).event as V3Descriptor
    if (row.server_sequence !== index + 1 || event.server_sequence !== row.server_sequence)
      throw new BrowserReplayError('history_gap')
    if (peerConflict && index >= snapshot.rows.length - 2) continue
    if (row.server_sequence === contentGenesisSequence) continue
    const terminal = terminalContentAmk && row.server_sequence === snapshot.rows.length
      && event.entity_type === 'note' && event.operation === 'event'
    if (terminal) {
      const previous = snapshot.rows.slice(0, -1).filter(value => {
        const prior = JSON.parse(value.wire_item).event as V3Descriptor
        return prior.entity_type === 'note' && prior.project_id === event.project_id && prior.entity_id === event.entity_id
      }).at(-1)
      if (row.protocol_version !== 3 || !['received', 'content_note_proven'].includes(row.state) || !previous || previous.protocol_version !== 3
        || !['received', 'note_proven'].includes(previous.state) || event.device_id === scope.deviceId
        || event.device_id !== anchor.registry.projects.find(project => project.project_id === event.project_id)?.origin_device_id
        || event.deleted_at !== null || event.revision !== (JSON.parse(previous.wire_item).event as V3Descriptor).revision + 1
        || (JSON.parse(previous.wire_item).event as V3Descriptor).operation !== 'upsert')
        throw new BrowserReplayError('unsupported_content_cohort')
      const prior = await openNoteTransport(scope, terminalContentAmk, 3, previous.wire_item, 'legacy')
      const content = await openNoteTransport(scope, terminalContentAmk, 3, row.wire_item, 'content_note_v1')
      if (content.content?.event.mutation !== 'update'
        || content.plaintext.header.parent_event_id !== prior.plaintext.header.event_id)
        throw new BrowserReplayError('unsupported_content_cohort')
      continue
    }
    if (row.protocol_version !== 3 || !(initialNotes ? ['note', 'project_metadata', 'stage', 'stage_order'] : structural ? ['project_metadata', 'stage', 'stage_order'] : ['project_metadata']).includes(event.entity_type)) throw new BrowserReplayError('unsupported_history_family')
    if (event.entity_type === 'note' && event.operation === 'delete') {
      if (event.deleted_at === null) throw new BrowserReplayError('unsupported_metadata_operation')
    } else if (event.operation !== 'upsert' || event.deleted_at !== null) throw new BrowserReplayError('unsupported_metadata_operation')
    if (event.device_id === scope.deviceId) throw new BrowserReplayError('metadata_self_echo_unsupported')
    if (event.device_id !== anchor.registry.projects.find(project => project.project_id === event.project_id)?.origin_device_id) throw new BrowserReplayError('metadata_origin_mismatch')
    if (event.entity_type === 'note') {
      const seed = snapshot.manifest.source_seeds?.find(value => value.project_id === event.project_id)
      const initial = snapshot.rows.some(value => value.server_sequence <= (seed?.initial_max_server_sequence ?? 0)
        && JSON.parse(value.wire_item).event.entity_type === 'note'
        && JSON.parse(value.wire_item).event.project_id === event.project_id
        && JSON.parse(value.wire_item).event.entity_id === event.entity_id)
      const previous = snapshot.rows.filter(value => value.server_sequence < row.server_sequence).filter(value => {
        const prior = JSON.parse(value.wire_item).event
        return prior.entity_type === 'note' && prior.project_id === event.project_id && prior.entity_id === event.entity_id
      }).at(-1)
      const priorEvent = previous ? JSON.parse(previous.wire_item).event : null
      if (!seed || row.state !== 'received' && row.state !== 'note_proven'
        || (row.server_sequence <= seed.initial_max_server_sequence ? event.revision !== 1 || event.operation !== 'upsert'
          : event.revision < 2 || !initial || !priorEvent || priorEvent.operation === 'delete'))
        throw new BrowserReplayError('unsupported_initial_note_history')
      if (event.operation === 'delete' && row.server_sequence <= seed.initial_max_server_sequence)
        throw new BrowserReplayError('unsupported_initial_note_history')
    }
  }
  // Current owners reauthenticate full current Stage membership. Later membership
  // expansion after an order is a separate cohort, never a silently weakened proof.
  if (structural) {
    const ordered = new Set<string>(), known = new Set<string>()
    for (const row of snapshot.rows) {
      const event = JSON.parse(row.wire_item).event as V3Descriptor
      if (event.entity_type === 'stage_order') ordered.add(event.project_id)
      if (event.entity_type === 'stage') {
        const id = JSON.stringify([event.project_id, event.entity_id])
        if (!known.has(id) && ordered.has(event.project_id)) throw new BrowserReplayError('structural_membership_expansion_unsupported')
        known.add(id)
      }
    }
  }
}

/** Force the genuine owner certificates over every retained applied row. */
export async function reauthenticateReplayOwners(scope: BrowserSyncScope, snapshot: ReplaySnapshot, amk: AccountMasterKey,
  current: () => boolean, structural = true, notes: ReadonlySet<number> = new Set()): Promise<void> {
  const inventory = { rows: snapshot.rows.filter(row => ['project_metadata', 'stage', 'stage_order'].includes(JSON.parse(row.wire_item).event.entity_type)), bindings: snapshot.bindings, owners: snapshot.owners, out: [], receipts: [], history: [],
    cursor: { pull_cursor: snapshot.manifest!.receive_cursor, ack_cursor: 0, proven_prefix: 0, next_local_ordinal: 1 } }
  const meta = await prepareMetadataPrefix(scope, inventory, amk, current, true)
  const certificates = new Map<string, ReadonlySet<number>>([['applied', verifiedMetadataSequences(meta, scope, inventory)]])
  if (structural) {
    certificates.set('stage_proven', stageSequences(await prepareStagePrefix(scope, inventory, amk, current, meta, true), scope, inventory))
    certificates.set('stage_order_proven', stageOrderSequences(await prepareStageOrderPrefix(scope, inventory, amk, current, meta, true), scope, inventory))
  }
  let authenticated = 0
  for (const row of snapshot.rows) if (row.state !== 'received') {
    if (!(row.state === 'note_proven' || row.state === 'content_note_proven' || row.state === 'content_note_conflict_preserved' || row.state === 'note_resolution_proven' ? notes.has(row.server_sequence) : certificates.get(row.state)?.has(row.server_sequence)))
      throw new BrowserReplayError(structural ? 'structural_proof_reauthentication_failed' : 'metadata_proof_reauthentication_failed')
    authenticated++
  }
  if (authenticated !== snapshot.manifest!.applied_cursor) throw new BrowserReplayError(structural ? 'structural_proof_reauthentication_failed' : 'metadata_proof_reauthentication_failed')
  if (!current()) throw new BrowserReplayError('session_closed')
}

/** No reducer: the existing Metadata/Structural commits and prefix verifiers own every outcome. */
async function composeReplayOwners(port: MetadataPort, scope: BrowserSyncScope, signal: AbortSignal, current: () => boolean,
  auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext,
  structural: boolean, initialNotes: boolean, terminalContent: boolean): Promise<ReplayMetadataResult | ReplayStructuralResult | ReplayInitialNoteResult | ReplayOrderedContentResult> {
  const context = auth.requireContext(), lease = keys.leaseForAccount(scope.accountId)
  const check = () => {
    if (signal.aborted || !current() || !auth.isCurrent(context) || !lease?.isCurrent() || lease.localAccountId !== scope.accountId
      || lease.canonicalUserId !== scope.canonicalUserId || lease.authEpoch !== context.authEpoch) throw new BrowserReplayError('session_closed')
  }
  check()
  let snapshot = await port.read(); check()
  if (!snapshot.manifest) throw new BrowserReplayError('metadata_history_incomplete')
  const anchor = snapshot.manifest.anchor
  const resolutionSequence = terminalContent && snapshot.manifest.contract_version === 6
    ? snapshot.rows.find(r => { const e = JSON.parse(r.wire_item).event; return e.entity_type === 'note' && e.operation === 'event' && e.revision === 3 })?.server_sequence : undefined
  const resolutionTail = resolutionSequence !== undefined
  const contentEnd = resolutionTail ? resolutionSequence! - 1 : snapshot.rows.length
  const contentRows = snapshot.rows.slice(0, contentEnd)
  const peerConflict = terminalContent && snapshot.manifest.contract_version === 6 && contentRows.slice(-2).length === 2
    && contentRows.slice(-2).every(row => { const event = JSON.parse(row.wire_item).event; return event.entity_type === 'note' && event.operation === 'event' })
  const contentGenesis = peerConflict && snapshot.rows.length >= 3 && (() => {
    const event = JSON.parse(contentRows[contentRows.length - 3]!.wire_item).event
    return event.entity_type === 'note' && event.operation === 'event' && event.revision === 1
  })()
  if (snapshot.manifest.phase !== 'received' || snapshot.rows.length !== anchor.registry.current_cursor || !snapshot.rows.length) throw new BrowserReplayError('metadata_history_incomplete')
  if (terminalContent && snapshot.manifest.contract_version !== 5 && snapshot.manifest.contract_version !== 6) throw new BrowserReplayError('unsupported_content_cohort')
  // Authenticate the entire account history before the first owner commit.
  if (terminalContent) await lease!.use(amk => assertReplayHistoryAdmissible(resolutionTail ? { ...snapshot, rows: contentRows, manifest: { ...snapshot.manifest!, anchor: { ...anchor, registry: { ...anchor.registry, current_cursor: contentEnd } } } } : snapshot, scope, true, true, amk))
  else await assertReplayHistoryAdmissible(snapshot, scope, structural, initialNotes)
  check()
  const server = async () => {
    check()
    const registry = await auth.authorized(token => cloudProjectsApi.listBootstraps(token, signal)); check()
    const mode = await auth.authorized(token => encryptedSyncV2Api.capabilities(token, signal)); check()
    if (!replaySame(replayAnchor(registry.value, mode.value), anchor)) throw new BrowserReplayError('server_changed')
    // Fresh device/family gate check, including a concurrently advanced account tail.
    const tail = await auth.authorized(token => encryptedSyncV3Api.pullRaw(token, scope.deviceId, anchor.registry.current_cursor, 1, signal)); check()
    const { page } = prepareReplayPage(tail.value, anchor.registry.current_cursor, scope, anchor)
    if (page.items.length || page.has_more) throw new BrowserReplayError('server_changed')
  }
  const reauthenticate = async () => {
    check()
    await lease!.use(async amk => {
      const noteSequences = initialNotes ? await reauthenticateReplayNotes(scope, snapshot, amk, () => { try { check(); return true } catch { return false } },
        async projectId => {
          const local = new BrowserLocalAuthority(port.transaction(snapshot, 0, server)).bind(auth, bindings, keys)
          const source = await local.readSnapshot(scope, projectId)
          const project = await local.withDecryptedSnapshot(scope, source.token, async opened => structuredClone(opened.project))
          return { source, project }
        }) : new Set<number>()
      await reauthenticateReplayOwners(scope, snapshot, amk, () => { try { check(); return true } catch { return false } }, structural, noteSequences)
    })
    check()
  }
  await reauthenticate()
  for (const row of snapshot.rows) {
    if (terminalContent && row.server_sequence >= contentEnd - (contentGenesis ? 2 : peerConflict ? 1 : 0)) break
    if (row.state !== 'received') continue // Only after actual owner proof reauthentication above.
    check()
    const reference = JSON.parse(row.wire_item).event as V3Descriptor
    const transaction = port.transaction(snapshot, row.server_sequence, server)
    if (reference.entity_type === 'note') {
      const owner = new BrowserLegacyNoteApply(transaction, new BrowserLocalAuthority(transaction), current).bind(auth, bindings, keys)
      const token = await owner.prepare(scope, row.server_sequence); check()
      if (await owner.commit(token) !== 'applied') throw new BrowserReplayError('initial_note_not_applied')
      check()
    } else if (reference.entity_type === 'project_metadata') {
      const owner = new BrowserImportedMetadataApply(transaction, new BrowserSyncDurability(transaction))
      const token = snapshot.bindings.some(binding => binding.project_id === reference.project_id)
        ? await owner.prepareVerifiedOrdinary(scope, reference, auth, bindings, keys)
        : await owner.prepareVerifiedRemoteGenesis(scope, reference, auth, bindings, keys, signal)
      check(); await owner.commitVerifiedImportedMetadataApply(scope, token); check()
    } else {
      try {
        // Existing opaque preparation and actual owner commit, through the same CAS adapter.
        if (reference.entity_type === 'stage') {
          const owner = new BrowserStageApply(transaction, current).bind(auth, bindings, keys)
          const token = await owner.prepare(scope, row.server_sequence); check(); await owner.commit(token)
        } else {
          const owner = new BrowserStageOrderApply(transaction, current).bind(auth, bindings, keys)
          const token = await owner.prepare(scope, row.server_sequence); check(); await owner.commit(token)
        }
      } catch (error) {
        if (error instanceof CryptoError) throw new BrowserReplayError('structural_ciphertext_rejected')
        throw error
      }
      check()
    }
    snapshot = await port.read(); check()
  }
  if (terminalContent) {
    const sequence = contentEnd
    const { applyReplayContentUpdate } = await import('./replayContentUpdate')
    for (const target of contentGenesis ? [sequence - 2, sequence - 1, sequence] : peerConflict ? [sequence - 1, sequence] : [sequence]) {
      if (snapshot.manifest!.applied_cursor! < target - 1) throw new BrowserReplayError('content_prefix_incomplete')
      if (snapshot.manifest!.applied_cursor! > target) continue
      await applyReplayContentUpdate(port, scope, target, signal, current, auth, bindings, keys)
      snapshot = await port.read(); check()
    }
    if ((!resolutionTail && snapshot.manifest!.applied_cursor !== sequence) || snapshot.rows[sequence - 1]?.state !== (peerConflict ? 'content_note_conflict_preserved' : 'content_note_proven'))
      throw new BrowserReplayError('content_not_applied')
  }
  if (resolutionTail) {
    const transaction: BrowserSyncTransaction = async (...args) => {
      const expected = await port.read()
      const target = Math.min(expected.manifest!.applied_cursor! + 1, snapshot.rows.length)
      return port.transaction(expected, target, server)(...args)
    }
    const device = await new BrowserDeviceRegistration(transaction, current).bind(auth, bindings).registerVerified(scope)
    if (snapshot.manifest!.applied_cursor! <= resolutionSequence!) await applyReplayResolutionOwner({ contractVersion: 6, scope, sequence: resolutionSequence!,
      transaction, current, auth, bindings, keys, device })
    if (snapshot.rows.length > resolutionSequence!) {
      const owner = new BrowserResolutionContinuation(transaction, new BrowserLocalAuthority(transaction), current).bind(auth, bindings, keys, device)
      for (let sequence = resolutionSequence! + 1; sequence <= snapshot.rows.length; sequence++) {
        const row=(await port.read()).rows[sequence-1]!
        if(row.state==='note_resolution_proven')continue // Whole history is reauthenticated below.
        const content=await lease!.use(amk=>openReplayContent(scope,row,amk));check()
        if(content.event.version===2){
          const planner=new BrowserResolutionPlanner(transaction,new BrowserLocalAuthority(transaction),current).bind(auth,bindings,keys,device)
          const plan=await planner.prepareSuccessor(scope,sequence),apply=planner.atomicApply()
          await apply.commit(await apply.prepare(plan.token));check()
        }else{await owner.apply(scope,sequence);check()}
      }
      await owner.history(scope)
    }
    snapshot = await port.read(); check()
    // Re-deliver the complete committed account history through the existing
    // v10 transport owner. Never reinterpret inbox10 with the old strict reader.
    // J authenticates all retained owner history and performs exact sequence/index/wire joins here.
    const pages: string[] = []
    let since = 0
    while (since < snapshot.rows.length) {
      const response = await auth.authorized(token => encryptedSyncV3Api.pullRaw(token, scope.deviceId, since, 100, signal)); check()
      const { page } = prepareReplayPage(response.value, since, scope, anchor)
      if (page.next_cursor <= since || page.has_more !== (page.next_cursor < snapshot.rows.length)) throw new BrowserReplayError('history_incomplete')
      pages.push(response.value); since = page.next_cursor
    }
    const transport = new BrowserV10Transport(port.transaction(snapshot, 0, server), current).bind(auth, bindings, keys, device)
    await transport.replayInboundHistory(scope, pages); check()
  }
  await reauthenticate(); await server()
  if (!replaySame(await port.read(), snapshot)) throw new BrowserReplayError('metadata_inventory_changed')
  check()
  const metadataProofs = snapshot.rows.filter(row => row.state === 'applied').length
  const stageProofs = snapshot.rows.filter(row => row.state === 'stage_proven').length
  const stageOrderProofs = snapshot.rows.filter(row => row.state === 'stage_order_proven').length
  const result = { cursor: snapshot.manifest!.receive_cursor, generation: snapshot.manifest!.generation,
    ownerProofs: snapshot.manifest!.applied_cursor!, projects: snapshot.owners.filter(head => head.owner_kind === 'project_metadata').length,
    projectOpened: false as const, ready: false as const, declarationAllowed: false as const, writerAuthorized: false as const }
  if (terminalContent) return { ...result, kind: resolutionTail ? (snapshot.rows.filter(r=>r.state==='note_resolution_proven').length>1?'bounded_resolution_chain_applied':snapshot.rows.length > resolutionSequence! ? 'bounded_resolution_continuation_applied' : 'bounded_resolution_owner_applied') : peerConflict ? 'bounded_peer_content_conflict_preserved' : 'bounded_ordered_content_update_applied', metadataProofs, stageProofs,
    stageOrderProofs, noteProofs: snapshot.rows.filter(row => row.state === 'note_proven').length, contentProofs: snapshot.rows.filter(r => r.state === 'content_note_proven' || r.state === 'content_note_conflict_preserved').length }
  return initialNotes ? { ...result, kind: 'initial_legacy_notes_applied', metadataProofs, stageProofs, stageOrderProofs,
    noteProofs: snapshot.rows.filter(row => row.state === 'note_proven').length }
    : structural ? { ...result, kind: 'structural_owners_applied', metadataProofs, stageProofs, stageOrderProofs }
    : { ...result, kind: 'metadata_owner_applied' }
}

export function composeReplayMetadata(port: MetadataPort, scope: BrowserSyncScope, signal: AbortSignal, current: () => boolean,
  auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext, structural = false, initialNotes = false): Promise<ReplayMetadataResult | ReplayStructuralResult | ReplayInitialNoteResult> {
  return composeReplayOwners(port, scope, signal, current, auth, bindings, keys, structural, initialNotes, false) as Promise<ReplayMetadataResult | ReplayStructuralResult | ReplayInitialNoteResult>
}

/** One terminal peer Content update after the existing ordered owner prefix. */
export function composeReplayOrderedContent(port: MetadataPort, scope: BrowserSyncScope, signal: AbortSignal, current: () => boolean,
  auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext): Promise<ReplayOrderedContentResult> {
  return composeReplayOwners(port, scope, signal, current, auth, bindings, keys, true, true, true) as Promise<ReplayOrderedContentResult>
}
