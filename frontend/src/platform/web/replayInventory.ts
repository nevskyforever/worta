import { contentHistoryJoin } from './contentNoteProof'
import { contentConflictHistoryJoin } from './contentNoteConflictProof'
import { BrowserPersistenceError } from './persistence'
import { validateBrowserInboxRecord, validateBrowserSyncCursor, type BrowserInboxRecord, type BrowserSyncScope } from './syncDurability'
import { validateBrowserProjectBinding, type BrowserProjectBinding } from './projectBindings'
import type { BrowserMetadataHead, BrowserMetadataProof } from './metadataApplyRecords'
import { validateBrowserOwnerHead, stageOwnerId, type BrowserStageHead, type BrowserStageProof } from './stageRecords'
import type { BrowserStageOrderHead, BrowserStageOrderProof } from './stageOrderRecords'
import { validateLocalProjectRecord, validateLocalNoteRecord, type LocalProjectRecord, type LocalNoteRecord } from './localAuthorityRecords'
import { validateNoteHistorySet, type BrowserNoteHistoryRecord, type BrowserNoteCandidate, type BrowserNoteHead } from './noteHistoryRecords'
import { REPLAY_LIMITS, replayExact, replaySame, replayUuid, validateReplayManifest, type ReplayManifest } from './replayRecords'

export interface ReplaySnapshot {
  manifest: ReplayManifest | null; rows: BrowserInboxRecord[]; bindings: BrowserProjectBinding[]; owners: Array<BrowserMetadataHead | BrowserStageHead | BrowserStageOrderHead>
  sources?: LocalProjectRecord[]
  notes?: LocalNoteRecord[]; noteHistory?: BrowserNoteHistoryRecord[]
}
/** Derived coverage is never a qualified cursor or owner certificate. */
export function replayNoteOutcome(row: BrowserInboxRecord) {
  if (row.state === 'note_proven') return row.note_proof
  if (row.state === 'content_note_proven') return row.content_note_proof
  throw new BrowserPersistenceError('corrupt_record')
}
export function replayCoverage(manifest: ReplayManifest, row: BrowserInboxRecord) {
  const sequence = row.server_sequence
  return { ...manifest, applied_cursor: sequence, metadata_cursor: row.state === 'applied' && sequence === (manifest.metadata_cursor ?? 0) + 1 ? sequence : manifest.metadata_cursor,
    revision: manifest.revision + 1 }
}
export function replayAppliedProof(row: BrowserInboxRecord): BrowserMetadataProof | BrowserStageProof | BrowserStageOrderProof | undefined {
  return row.state === 'applied' ? row.applied_proof : row.state === 'stage_proven' ? row.stage_proof : row.state === 'stage_order_proven' ? row.stage_order_proof : undefined
}
export function assertReplayOwnerTransition(before: ReplaySnapshot, after: ReplaySnapshot, sequence: number, status: unknown): void {
  if (status === 'already_applied') { if (!replaySame(before, after)) fail(); return }
  const manifest = before.manifest!, previous = before.rows[sequence - 1], row = after.rows[sequence - 1]
  if (row?.state === 'content_note_conflict_preserved') {
    if (manifest.contract_version !== 6 || status !== 'conflict_preserved' || previous?.state !== 'received'
      || sequence !== (manifest.applied_cursor ?? 0) + 1 || !replaySame(after.manifest, replayCoverage(manifest, row))
      || !replaySame(before.bindings, after.bindings) || !replaySame(before.owners, after.owners)
      || !replaySame(before.sources, after.sources) || !replaySame(before.notes, after.notes)
      || !replaySame(before.rows.filter(value => value.server_sequence !== sequence), after.rows.filter(value => value.server_sequence !== sequence))) fail()
    const original = { ...row } as Record<string, unknown>; delete original.content_note_conflict_proof
    if (!replaySame({ ...original, record_version: 1, state: 'received' }, previous)) fail()
    const scope = { accountId: manifest.account_id, canonicalUserId: manifest.canonical_user_id, deviceId: manifest.device_id }
    const { candidate, group } = contentConflictHistoryJoin(scope, row, after.noteHistory ?? [])
    const old = before.noteHistory ?? [], next = after.noteHistory ?? []
    const winner = group.members.find(member => member.source === 'remote_applied')
    const prior = winner && before.rows.find(value => value.event_id === winner.event_id)
    const winningCandidate = winner && old.find(value => value.record_kind === 'candidate' && value.event_id === winner.event_id)
    const oldHead = old.find(value => value.record_kind === 'head' && value.project_id === group.project_id && value.note_id === group.note_id)
    const newHead = next.find(value => value.record_kind === 'head' && value.project_id === group.project_id && value.note_id === group.note_id)
    if (row.content_note_conflict_proof.record_version !== 1 || row.content_note_conflict_proof.mutation !== 'update'
      || group.conflict_generation !== 1 || group.members.length !== 2 || !winner || !prior || prior.state !== 'content_note_proven'
      || !winningCandidate || winningCandidate.record_kind !== 'candidate' || winningCandidate.record_version !== 2
      || winningCandidate.source_device_id === candidate.source_device_id || winner.server_sequence !== prior.server_sequence
      || winningCandidate.header.parent_event_id !== group.common_parent_event_id
      || winningCandidate.header.revision !== group.tip_revision
      || !oldHead || oldHead.record_kind !== 'head' || !newHead || newHead.record_kind !== 'head'
      || oldHead.linear_head_event_id !== winner.event_id || newHead.linear_head_event_id !== winner.event_id
      || newHead.head_revision !== oldHead.head_revision + 1 || !replaySame(newHead.tip_event_ids, group.tip_event_ids)
      || !replaySame(newHead, { ...oldHead, head_revision: oldHead.head_revision + 1, tip_event_ids: group.tip_event_ids })
      || old.some(value => value.record_kind === 'group' || value.record_kind === 'local_version')
      || next.length !== old.length + 2 || old.some(value => value !== oldHead && !next.some(record => replaySame(record, value)))
      || next.filter(value => !old.some(record => replaySame(record, value))).some(value => value !== candidate && value !== group && value !== newHead)) fail()
    return
  }
  if (row?.state === 'note_proven' || row?.state === 'content_note_proven') {
    if (row.state === 'content_note_proven') {
      if (row.content_note_proof.mutation !== 'update' && !(manifest.contract_version === 6 && row.content_note_proof.mutation === 'create')) fail()
      contentHistoryJoin({ accountId: manifest.account_id, canonicalUserId: manifest.canonical_user_id, deviceId: manifest.device_id }, row, after.noteHistory ?? [])
    }
    const proof = replayNoteOutcome(row), project = proof.header.project_id, note = proof.header.entity_id
    if (status !== 'applied' || previous?.state !== 'received' || proof.outcome !== 'applied'
      || sequence !== (manifest.applied_cursor ?? 0) + 1 || !replaySame(after.manifest, replayCoverage(manifest, row))
      || !replaySame(before.bindings, after.bindings) || !replaySame(before.owners, after.owners)
      || !replaySame(before.rows.filter(value => value.server_sequence !== sequence), after.rows.filter(value => value.server_sequence !== sequence))) fail()
    const original = { ...row } as Record<string, unknown>; delete original.note_proof; delete original.content_note_proof
    if (!replaySame({ ...original, record_version: 1, state: 'received' }, previous)) fail()
    const sourceBefore = before.sources?.find(value => value.project_id === project)
    const sourceAfter = after.sources?.find(value => value.project_id === project)
    const noteBefore = before.notes?.find(value => value.project_id === project && value.note_id === note)
    const noteAfter = after.notes?.find(value => value.project_id === project && value.note_id === note)
    const prior = before.noteHistory?.filter(value => value.project_id === project && value.note_id === note) ?? []
    const next = after.noteHistory?.filter(value => value.project_id === project && value.note_id === note) ?? []
    const oldHead = prior.find((value): value is BrowserNoteHead => value.record_kind === 'head')
    const newHead = next.find((value): value is BrowserNoteHead => value.record_kind === 'head')
    const oldCandidates = prior.filter((value): value is BrowserNoteCandidate => value.record_kind === 'candidate')
    const newCandidates = next.filter((value): value is BrowserNoteCandidate => value.record_kind === 'candidate')
    if (!sourceBefore || !sourceAfter || sourceAfter.source_revision !== sourceBefore.source_revision + 1
      || sourceAfter.source_revision !== proof.source_revision || !noteAfter
      || (proof.mutation === 'create' ? !!noteBefore || !!oldHead || oldCandidates.length !== 0
        : (proof.mutation !== 'update' && proof.mutation !== 'delete') || !noteBefore || noteBefore.deleted
          || !oldHead || oldHead.linear_head_event_id !== proof.header.parent_event_id)
      || !replaySame(newCandidates.find(candidate => candidate.event_id === row.event_id)?.projection, noteAfter)
      || noteAfter.deleted !== (proof.mutation === 'delete')
      || !newHead || newHead.head_revision !== (oldHead?.head_revision ?? 0) + 1
      || newHead.linear_head_event_id !== row.event_id || newCandidates.length !== oldCandidates.length + 1
      || oldCandidates.some(value => !newCandidates.some(candidate => replaySame(candidate, value)))
      || !replaySame(before.sources?.filter(value => value.project_id !== project), after.sources?.filter(value => value.project_id !== project))
      || !replaySame(before.notes?.filter(value => value.project_id !== project || value.note_id !== note),
        after.notes?.filter(value => value.project_id !== project || value.note_id !== note))
      || !replaySame(before.noteHistory?.filter(value => value.project_id !== project || value.note_id !== note),
        after.noteHistory?.filter(value => value.project_id !== project || value.note_id !== note))) fail()
    return
  }
  const proof = row && replayAppliedProof(row)
  if (status !== 'applied' || !previous || previous.state !== 'received' || !proof
    || sequence !== (manifest.applied_cursor ?? 0) + 1 || !replaySame(after.manifest, replayCoverage(manifest, row!))) fail()
  const wire = { ...row } as Record<string, unknown>
  for (const key of ['applied_proof', 'stage_proof', 'stage_order_proof']) delete wire[key]
  if (!replaySame({ ...wire, record_version: 1, state: 'received' }, previous)
    || !replaySame(before.rows.filter(value => value.server_sequence !== sequence), after.rows.filter(value => value.server_sequence !== sequence))) fail()
  const ownerId = proof.owner_kind === 'stage' ? stageOwnerId(proof.project_id, proof.stage_id) : proof.project_id
  const other = (head: ReplaySnapshot['owners'][number]) => head.owner_kind !== proof.owner_kind || head.owner_id !== ownerId
  if (!replaySame(before.owners.filter(other), after.owners.filter(other))) fail()
  if (!replaySame(before.sources, after.sources)) fail()
  if (!replaySame(before.notes, after.notes) || !replaySame(before.noteHistory, after.noteHistory)) fail()
  const oldBinding = before.bindings.find(binding => binding.project_id === proof.project_id)
  if (proof.owner_kind !== 'project_metadata' || oldBinding) {
    if (!replaySame(before.bindings, after.bindings)) fail()
  } else if (!replaySame(before.bindings, after.bindings.filter(binding => binding.project_id !== proof.project_id))) fail()
}
export const replayEmptyStores = ['outbox', 'receipts', 'legacyNoteIntents']
function fail(): never { throw new BrowserPersistenceError('corrupt_record') }
const range = (account: string) => IDBKeyRange.bound([account], [account, []])

/** Complete account snapshot, with structural joins only. Crypto is owned by the existing family certificates. */
export function scheduleReplaySnapshot(tx: IDBTransaction, s: BrowserSyncScope, check: () => void,
  result: (snapshot: ReplaySnapshot) => void, abort: (error: unknown) => void, version: 1 | 2 | 3 | 4 | 5 | 6 = 6): void {
  const bound = range(s.accountId), rows: BrowserInboxRecord[] = []
  const identity = tx.objectStore('identities').get(s.canonicalUserId), alias = tx.objectStore('identities').index('local_account_id').get(s.accountId)
  const binding = tx.objectStore('accountBindings').get(s.accountId), cursors = tx.objectStore('cursors').getAll(bound)
  const manifests = tx.objectStore('replayManifest').getAll(bound), bindings = tx.objectStore('projectBindings').getAll(bound), owners = tx.objectStore('ownerHeads').getAll(bound)
  const projects = tx.objectStore('localProjects').getAll(bound)
  const localNotes = tx.objectStore('localNotes').getAll(bound), noteHistory = tx.objectStore('legacyNoteHistory').getAll(bound)
  const counts = replayEmptyStores.map(name => tx.objectStore(name).count(bound))
  let pending = 11 + counts.length, bytes = 0
  const finish = () => {
    try {
      check(); if (--pending) return
      if (counts.some(request => request.result !== 0) || version < 4 && projects.result.length || cursors.result.length > 1 || manifests.result.length > 1
        || bindings.result.length > 200 || owners.result.length > 200 || version === 1 && (bindings.result.length || owners.result.length)) fail()
      const i: unknown = identity.result, b: unknown = binding.result
      if (i === undefined ? b !== undefined || alias.result !== undefined : !replayExact(i, ['record_version', 'canonical_user_id', 'local_account_id', 'device_id'])
        || i.record_version !== 1 || i.canonical_user_id !== s.canonicalUserId || i.local_account_id !== s.accountId || i.device_id !== s.deviceId
        || !replaySame(alias.result, i) || !replayExact(b, ['record_version', 'local_account_id', 'canonical_user_id'])
        || b.record_version !== 1 || b.local_account_id !== s.accountId || b.canonical_user_id !== s.canonicalUserId) fail()
      const manifest = manifests.result.length ? validateReplayManifest(manifests.result[0], s, version) : null
      const checkpoint = validateBrowserSyncCursor(cursors.result[0], s), metadata = manifest?.metadata_cursor ?? 0, applied = version >= 3 ? manifest?.applied_cursor ?? 0 : metadata
      if (checkpoint.ack_cursor !== 0 || checkpoint.proven_prefix !== 0 || checkpoint.next_local_ordinal !== 1
        || checkpoint.pull_cursor !== rows.length || !manifest && (rows.length || cursors.result.length || bindings.result.length || owners.result.length || projects.result.length)
        || manifest && (i === undefined || !cursors.result.length || manifest.receive_cursor !== rows.length)) fail()
      const imported = bindings.result.map(raw => validateBrowserProjectBinding(raw, s)), heads = owners.result.map(raw => validateBrowserOwnerHead(raw, s))
      const sources = projects.result.map(raw => validateLocalProjectRecord(raw, s))
      const notes = localNotes.result.map(raw => validateLocalNoteRecord(raw, s, raw.project_id))
      const history = validateNoteHistorySet(s, noteHistory.result)
      const noteProofs = rows.filter(row => row.state === 'note_proven' || row.state === 'content_note_proven')
      const conflicts = rows.filter(row => row.state === 'content_note_conflict_preserved')
      const noteKey = (project: string, note: string) => JSON.stringify([project, note])
      const noteKeys = new Set(noteProofs.map(row => noteKey(replayNoteOutcome(row).header.project_id, replayNoteOutcome(row).header.entity_id)))
      const candidates = history.filter((record): record is BrowserNoteCandidate => record.record_kind === 'candidate')
      const noteHeads = history.filter((record): record is BrowserNoteHead => record.record_kind === 'head')
      const groups = history.filter(record => record.record_kind === 'group')
      if (version < 4 && (notes.length || history.length) || sources.length !== (manifest?.source_seeds?.length ?? 0) || sources.some(row => row.deleted
        || !manifest?.source_seeds?.some(seed => seed.project_id === row.project_id))
        || new Set(sources.map(row => row.project_id)).size !== sources.length) fail()
      if (version < 6 && (conflicts.length || groups.length) || conflicts.length > 1 || groups.length !== conflicts.length
        || notes.length !== noteKeys.size || candidates.length !== noteProofs.length + conflicts.length || noteHeads.length !== noteKeys.size
        || history.length !== candidates.length + noteHeads.length + groups.length
        || notes.some(note => !noteKeys.has(noteKey(note.project_id, note.note_id)))
        || candidates.some(candidate => ![...noteProofs, ...conflicts].some(row => row.event_id === candidate.event_id))
        || noteHeads.some(head => !noteKeys.has(noteKey(head.project_id, head.note_id)))) fail()
      for (const source of sources) {
        const appliedNotes = noteProofs.filter(row => replayNoteOutcome(row).header.project_id === source.project_id)
        if (source.source_revision !== appliedNotes.length + 1 || appliedNotes.some((row, index) => replayNoteOutcome(row).source_revision !== index + 2)) fail()
      }
      for (const seed of manifest?.source_seeds ?? []) {
        const genesis = rows.find(row => row.event_id === seed.genesis_event_id)
        if (!genesis || !sources.some(source => source.project_id === seed.project_id)) fail()
        const descriptor = JSON.parse(genesis.wire_item).event
        if (descriptor.entity_type !== 'project_metadata' || descriptor.project_id !== seed.project_id
          || descriptor.device_id !== seed.origin_device_id || descriptor.revision !== 1) fail()
      }
      if (version < 3 && heads.some(head => head.owner_kind !== 'project_metadata')) fail()
      let ciphertextBytes = 0
      const ids = new Set<string>()
      for (const [index, row] of rows.entries()) {
        if (row.server_sequence !== index + 1 || ids.has(row.event_id) || (row.server_sequence <= applied) !== (!!replayAppliedProof(row) || row.state === 'note_proven' || row.state === 'content_note_proven' || row.state === 'content_note_conflict_preserved')) fail()
        ids.add(row.event_id)
        const wire = JSON.parse(row.wire_item) as { event: { project_id: string }; object: { ciphertext: string } }
        ciphertextBytes += Math.floor(wire.object.ciphertext.length * 3 / 4)
        const proof = replayAppliedProof(row)
        if (row.state === 'note_proven' || row.state === 'content_note_proven') {
          if (row.state === 'content_note_proven') contentHistoryJoin(s, row, history)
          const p = replayNoteOutcome(row), seed = manifest?.source_seeds?.find(value => value.project_id === p.header.project_id)
          const retained = JSON.parse(row.wire_item), { device_id: sourceDevice, server_sequence: sourceSequence, ...event } = retained.event
          const candidate = candidates.find(value => value.event_id === row.event_id)
          const sameNote = noteProofs.filter(value => value.server_sequence <= row.server_sequence
            && replayNoteOutcome(value).header.project_id === p.header.project_id && replayNoteOutcome(value).header.entity_id === p.header.entity_id)
          const previous = sameNote.at(-2), isInitial = row.server_sequence <= (seed?.initial_max_server_sequence ?? 0)
          const isLatest = !noteProofs.some(value => value.server_sequence > row.server_sequence
            && replayNoteOutcome(value).header.project_id === p.header.project_id && replayNoteOutcome(value).header.entity_id === p.header.entity_id)
          const note = notes.find(value => value.note_id === p.header.entity_id)
          const contentGenesis = version >= 6 && row.state === 'content_note_proven' && p.mutation === 'create'
            && p.header.revision === 1 && p.header.parent_event_id === null && !previous && !isInitial
          if (!seed || p.outcome !== 'applied' || p.bootstrap_id !== seed.bootstrap_id || (row.state !== 'content_note_proven' || version < 6) && p.reference.device_id !== seed.origin_device_id
            || (!contentGenesis && (isInitial ? !!previous || p.header.revision !== 1 || p.mutation !== 'create' || p.header.parent_event_id !== null
              : !previous || replayNoteOutcome(previous).mutation === 'delete' || !['update', 'delete'].includes(p.mutation)
                || p.header.revision !== replayNoteOutcome(previous).header.revision + 1 || p.header.parent_event_id !== previous.event_id))
            || !candidate || candidate.record_version !== (row.state === 'content_note_proven' ? 2 : 1) || candidate.origin !== 'peer' || candidate.bootstrap_id !== seed.bootstrap_id
            || candidate.source_device_id !== sourceDevice || sourceSequence !== row.server_sequence
            || !replaySame(JSON.parse(candidate.wire_item), row.state === 'content_note_proven' ? retained : { event, object: retained.object })
            || !replaySame(candidate.header, p.header) || candidate.projection === null || candidate.projection_digest !== p.local_note_digest
            || row.state === 'content_note_proven' && !contentGenesis && (p.mutation !== 'update' || !previous || !(previous.state === 'note_proven' && previous.note_proof.mutation !== 'delete' || version >= 6 && previous.state === 'content_note_proven' && previous.content_note_proof.mutation === 'create'))
            || candidate.projection.source_revision !== p.source_revision || candidate.projection.deleted !== (p.mutation === 'delete')
            || !replaySame(p.reference, retained.event)) fail()
          if (previous?.state === 'content_note_proven' && !(version >= 6 && previous.content_note_proof.mutation === 'create')) fail()
          if (isLatest && !replaySame(candidate.projection, note)) fail()
        }
        if (proof) {
          const head = heads.find(value => value.owner_kind === proof.owner_kind && value.owner_id === (proof.owner_kind === 'stage' ? stageOwnerId(proof.project_id, proof.stage_id) : proof.project_id))
          if (!head || !replaySame(head.history.find(value => value.event_id === row.event_id), proof)) fail()
          if (proof.owner_kind !== 'project_metadata') {
            const metadataHead = heads.find(value => value.owner_kind === 'project_metadata' && value.owner_id === proof.project_id)
            const dependency = metadataHead?.history.find(value => value.event_id === proof.metadata_event_id)
            if (!dependency || dependency.server_sequence >= proof.server_sequence) fail()
            if (proof.owner_kind === 'stage_order') for (const stage of proof.stage_dependencies) for (const id of stage.event_ids) {
              const head = heads.find(value => value.owner_kind === 'stage' && value.project_id === proof.project_id && value.stage_id === stage.stage_id)
              const parent = head?.history.find(value => value.event_id === id)
              if (!parent || parent.server_sequence >= proof.server_sequence) fail()
            }
          }
        }
      }
      for (const head of noteHeads) {
        const own = noteProofs.filter(row => replayNoteOutcome(row).header.project_id === head.project_id && replayNoteOutcome(row).header.entity_id === head.note_id)
        const group = groups.find(value => value.project_id === head.project_id && value.note_id === head.note_id)
        if (!own.length || head.head_revision !== own.length + (group ? 1 : 0) || head.linear_head_event_id !== own.at(-1)!.event_id
          || !replaySame(head.tip_event_ids, group ? group.tip_event_ids : [own.at(-1)!.event_id])) fail()
      }
      for (const row of conflicts) {
        if (row.state !== 'content_note_conflict_preserved') fail()
        const { candidate, group } = contentConflictHistoryJoin(s, row, history)
        const winner = group.members.find(member => member.source === 'remote_applied')
        const winning = winner && candidates.find(value => value.event_id === winner.event_id)
        const original = winner && rows.find(value => value.event_id === winner.event_id)
        if (row.content_note_conflict_proof.record_version !== 1 || row.content_note_conflict_proof.mutation !== 'update'
          || group.members.length !== 2 || group.conflict_generation !== 1 || !winner || !winning || winning.record_version !== 2
          || !original || original.state !== 'content_note_proven' || original.server_sequence !== winner.server_sequence
          || original.server_sequence >= row.server_sequence || winning.source_device_id === candidate.source_device_id
          || winning.header.parent_event_id !== group.common_parent_event_id || winning.header.revision !== group.tip_revision) fail()
      }
      const metadataPrefix = rows.findIndex(row => row.state !== 'applied')
      if (metadata !== (metadataPrefix < 0 ? rows.length : metadataPrefix)) fail()
      for (const record of imported) {
        const remote = manifest?.anchor.registry.projects.find(project => project.project_id === record.project_id)
        if (!remote || remote.bootstrap_id !== record.bootstrap_id || record.mode !== 'import_remote' || record.phase !== 'captured' || record.remote_state !== 'active' || record.remote_high_water !== manifest!.anchor.registry.current_cursor
          || !(heads.some(head => head.owner_kind === 'project_metadata' && head.owner_id === record.project_id)
            || manifest?.source_seeds?.some(seed => seed.project_id === record.project_id))) fail()
      }
      for (const head of heads) {
        const project = head.owner_kind === 'project_metadata' ? head.owner_id : head.project_id
        if (!imported.some(record => record.project_id === project && record.bootstrap_id === head.bootstrap_id)) fail()
        for (const proof of head.history) {
          const row = rows.find(value => value.event_id === proof.event_id)
          if (!row || !replaySame(replayAppliedProof(row), proof) || !['create', 'update'].includes(proof.operation)
            || proof.protocol_version !== 3 || proof.source_device_id === s.deviceId) fail()
        }
        const genesis = head.history.find(proof => proof.operation === 'create')
        if (!genesis || genesis.source_device_id !== manifest!.anchor.registry.projects.find(record => record.project_id === project)?.origin_device_id) fail()
      }
      bytes += new TextEncoder().encode(JSON.stringify({ manifest, imported, heads, sources, notes, history })).length + REPLAY_LIMITS.controlReserveBytes
      if (manifest && (ciphertextBytes !== manifest.ciphertext_bytes || bytes > REPLAY_LIMITS.inventoryBytes)) fail()
      result({ manifest, rows, bindings: imported, owners: heads, sources, notes, noteHistory: history })
    } catch (error) { abort(error) }
  }
  for (const request of [identity, alias, binding, cursors, manifests, bindings, owners, projects, localNotes, noteHistory, ...counts]) request.onsuccess = finish
  const scan = tx.objectStore('inbox').openCursor(bound)
  scan.onsuccess = () => {
    try {
      check(); const cursor = scan.result
      if (!cursor) { finish(); return }
      if (rows.length >= REPLAY_LIMITS.events) fail()
      const row = validateBrowserInboxRecord(cursor.value, s)
      if (!(row.state === 'received' && row.record_version === 1 || version >= 2 && row.state === 'applied' && row.record_version === 2 || version >= 3 && (row.state === 'stage_proven' && row.record_version === 5 || row.state === 'stage_order_proven' && row.record_version === 6) || version >= 4 && row.state === 'note_proven' && row.record_version === 3 || version >= 5 && row.state === 'content_note_proven' && row.record_version === 7 || version >= 6 && row.state === 'content_note_conflict_preserved' && row.record_version === 8)
        || row.protocol_version !== 3 || !replaySame(cursor.primaryKey, [s.accountId, row.server_sequence])) fail()
      bytes += new TextEncoder().encode(JSON.stringify(row)).length
      if (bytes > REPLAY_LIMITS.inventoryBytes) fail()
      rows.push(row); cursor.continue()
    } catch (error) { abort(error) }
  }
}

/** Versionchange has no HTTP/crypto and never invents bindings, heads or applied outcomes. */
export function migrateReplay(tx: IDBTransaction, names: readonly string[], check: () => void, abort: (error: unknown) => void, version: 1 | 2 | 3 | 4 | 5): void {
  const all = new Map<string, unknown[]>(); let remaining = names.length, bytes = 0
  const validate = () => {
    try {
      check(); if (--remaining) return
      const identities = all.get('identities')!, bindings = all.get('accountBindings')!
      if (identities.length !== bindings.length) fail()
      const scopes = identities.map(value => {
        const identity = value as { canonical_user_id: string; local_account_id: string; device_id: string }
        if (![identity.canonical_user_id, identity.local_account_id, identity.device_id].every(replayUuid)) fail()
        return { canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id }
      })
      for (const [name, values] of all) for (const value of values) {
        const row = value as Record<string, unknown>
        if (!scopes.some(scope => name === 'identities' ? row.canonical_user_id === scope.canonicalUserId : name === 'accountBindings' ? row.local_account_id === scope.accountId : row.account_id === scope.accountId)) fail()
      }
      let pending = scopes.length
      const updates: ReplayManifest[] = []
      for (const scope of scopes) scheduleReplaySnapshot(tx, scope, check, snapshot => {
        try {
          if (snapshot.manifest) updates.push({ ...snapshot.manifest, record_version: 6, contract_version: 6, metadata_cursor: snapshot.manifest.metadata_cursor ?? 0,
            applied_cursor: snapshot.manifest.applied_cursor ?? snapshot.manifest.metadata_cursor ?? 0, source_seeds: snapshot.manifest.source_seeds ?? [] })
          if (--pending) return
          check()
          for (const manifest of updates) tx.objectStore('replayManifest').put(manifest)
        } catch (error) { abort(error) }
      }, abort, version)
    } catch (error) { abort(error) }
  }
  for (const name of names) {
    const rows: unknown[] = []; all.set(name, rows)
    const request = tx.objectStore(name).openCursor()
    request.onsuccess = () => {
      try {
        check(); const cursor = request.result
        if (!cursor) { validate(); return }
        if (rows.length >= 16384) fail()
        const row: unknown = cursor.value
        bytes += new TextEncoder().encode(JSON.stringify(row)).length
        if (bytes > REPLAY_LIMITS.inventoryBytes) fail()
        rows.push(row); cursor.continue()
      } catch (error) { abort(error) }
    }
  }
}
