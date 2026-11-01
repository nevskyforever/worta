// @vitest-environment node
import { expect, it } from 'vitest'
import { contentConflictSetup } from './contentConflict.testFixture'
import { scope, amk } from './bootstrap.testFixture'
import { assertReplayOwnerTransition, replayCoverage, type ReplaySnapshot } from './replayInventory'
import { verifyStoredContentConflictProof } from './contentNoteConflictProof'

async function transition() {
  const f = await contentConflictSetup()
  const manifest = { record_version: 6 as const, contract_version: 6 as const, metadata_cursor: 1, applied_cursor: 3,
    source_seeds: [], account_id: scope.accountId, device_id: scope.deviceId, canonical_user_id: scope.canonicalUserId,
    generation: crypto.randomUUID(), anchor: { registry: { current_cursor: 4, projects: [] },
      transport: { supported_transport_version: 2 as const, writer_transport_version: 3 as const, cutover_epoch: 1 } },
    receive_cursor: 4, ciphertext_bytes: 0, phase: 'received' as const, revision: 1 }
  const snapshot = (): ReplaySnapshot => structuredClone({ manifest, rows: f.db.inbox, bindings: f.db.projectBindings,
    owners: f.db.ownerHeads, sources: f.db.localProjects, notes: f.db.localNotes, noteHistory: f.db.legacyNoteHistory }) as ReplaySnapshot
  const before = snapshot()
  expect(await f.content.apply(scope, 4)).toBe('conflict_preserved')
  const after = snapshot(); after.manifest = replayCoverage(manifest, after.rows[3]!)
  return { f, before, after }
}

it('checks the genuine peer conflict outcome and its retained owner proof', async () => {
  const { before, after } = await transition()
  assertReplayOwnerTransition(before, after, 4, 'conflict_preserved')
  await verifyStoredContentConflictProof(scope, amk, after.rows[3]!, after.noteHistory!, after.rows)
  assertReplayOwnerTransition(after, structuredClone(after), 4, 'already_applied')
})

it('does not admit the conflict transition under the frozen v5 contract', async () => {
  const { before, after } = await transition()
  before.manifest = { ...before.manifest!, record_version: 5, contract_version: 5 }
  after.manifest = replayCoverage(before.manifest, after.rows[3]!)
  expect(() => assertReplayOwnerTransition(before, after, 4, 'conflict_preserved')).toThrow()
})

it.each(['coverage', 'source', 'wire', 'proof', 'head', 'group', 'candidate', 'other-row'] as const)(
  'rejects a partial or altered %s transition', async kind => {
    const { before, after } = await transition()
    if (kind === 'coverage') after.manifest = before.manifest
    if (kind === 'source') after.sources = []
    if (kind === 'wire') after.rows[3]!.wire_item += ' '
    const row = after.rows[3]!
    if (kind === 'proof' && row.state === 'content_note_conflict_preserved') row.content_note_conflict_proof.candidate_digest = '0'.repeat(64)
    if (kind === 'head') after.noteHistory = after.noteHistory!.filter(value => value.record_kind !== 'head')
    if (kind === 'group') after.noteHistory = after.noteHistory!.filter(value => value.record_kind !== 'group')
    if (kind === 'candidate') after.noteHistory = after.noteHistory!.filter(value => value.record_kind !== 'candidate' || value.event_id !== row.event_id)
    if (kind === 'other-row') after.rows[0]!.wire_item += ' '
    expect(() => assertReplayOwnerTransition(before, after, 4, 'conflict_preserved')).toThrow()
  })
