// @vitest-environment node
import { expect, it } from 'vitest'
import { routeSetup, contentRow } from './noteTransport.testFixture'
import { received } from './legacyNoteApply.testFixture'
import { scope, amk } from './bootstrap.testFixture'
import { BrowserContentNoteApplyRuntime } from './contentNoteApply'
import { assertReplayOwnerTransition, replayCoverage, type ReplaySnapshot } from './replayInventory'
import { verifyStoredContentNoteProof } from './contentNoteProof'
import { validateReplayManifest } from './replayRecords'

async function setup() {
  const f = await routeSetup()
  const root = await received(f, 2, 1, null, 'upsert', p => { p.note.metadata = {} }, 'content-note')
  await f.notes.apply(scope, 2)
  await contentRow(f, 3, { mutation: 'update', parent: root.event_id })
  const manifest = { record_version: 5 as const, contract_version: 5 as const, metadata_cursor: 1, applied_cursor: 2,
    source_seeds: [], account_id: scope.accountId, device_id: scope.deviceId, canonical_user_id: scope.canonicalUserId,
    generation: crypto.randomUUID(), anchor: { registry: { current_cursor: 3, projects: [] },
      transport: { supported_transport_version: 2 as const, writer_transport_version: 3 as const, cutover_epoch: 1 } },
    receive_cursor: 3, ciphertext_bytes: 0, phase: 'received' as const, revision: 1 }
  const snapshot = (): ReplaySnapshot => structuredClone({ manifest, rows: f.db.inbox, bindings: f.db.projectBindings,
    owners: f.db.ownerHeads, sources: f.db.localProjects, notes: f.db.localNotes, noteHistory: f.db.legacyNoteHistory }) as ReplaySnapshot
  const before = snapshot(), runtime = new BrowserContentNoteApplyRuntime(f.notes)
  await runtime.apply(scope, 3)
  const after = snapshot(); after.manifest = replayCoverage(manifest, after.rows[2]!)
  return { f, before, after, runtime }
}
it('genuine Legacy -> Content atomic owner records qualify the v5 transition and authenticate again', async () => {
  const { f, before, after, runtime } = await setup()
  assertReplayOwnerTransition(before, after, 3, 'applied')
  await verifyStoredContentNoteProof(scope, amk, after.rows[2]! as never, after.noteHistory! as never)
  const saved = structuredClone(f.db)
  expect(await runtime.apply(scope, 3)).toBe('already_applied'); expect(f.db).toEqual(saved)
  assertReplayOwnerTransition(after, structuredClone(after), 3, 'already_applied')
  expect(after.rows[2]).toMatchObject({ record_version: 7, state: 'content_note_proven' })
  expect(after.noteHistory!.find(r => r.record_kind === 'candidate' && r.event_id === after.rows[2]!.event_id)).toMatchObject({ record_version: 2 })
})
it.each(['source', 'projection', 'head', 'candidate', 'coverage', 'wire', 'proof', 'transport'] as const)('partial/corrupt %s transition is refused', async kind => {
  const { before, after } = await setup()
  if (kind === 'source') after.sources = before.sources
  if (kind === 'projection') after.notes = before.notes
  if (kind === 'coverage') after.manifest = before.manifest
  if (kind === 'wire') after.rows[2]!.wire_item += ' '
  if (kind === 'head') after.noteHistory = after.noteHistory!.filter(r => r.record_kind !== 'head')
  if (kind === 'candidate') after.noteHistory = before.noteHistory
  const row = after.rows[2]!
  if (row.state === 'content_note_proven' && kind === 'proof') row.content_note_proof.candidate_digest = '0'.repeat(64)
  if (kind === 'transport') {
    const c = after.noteHistory!.find(r => r.record_kind === 'candidate' && r.record_version === 2)
    if (c?.record_kind === 'candidate' && c.record_version === 2) c.transport_proof.proof_digest = '0'.repeat(64)
  }
  expect(() => assertReplayOwnerTransition(before, after, 3, 'applied')).toThrow()
})
it('v5 is explicit and historical v4 validation refuses new manifests', async () => {
  const { after } = await setup()
  expect(validateReplayManifest(after.manifest, scope, 5)).toEqual(after.manifest)
  expect(() => validateReplayManifest(after.manifest, scope, 4)).toThrow()
})
it('aborted real owner transaction preserves source, projection and inbox', async () => {
  const f = await routeSetup(), root = await received(f, 2, 1, null, 'upsert', p => { p.note.metadata = {} }, 'content-note')
  await f.notes.apply(scope, 2); await contentRow(f, 3, { mutation: 'update', parent: root.event_id })
  const runtime = new BrowserContentNoteApplyRuntime(f.notes), token = await runtime.prepare(scope, 3), before = structuredClone(f.db)
  f.abort(); await expect(runtime.commit(token)).rejects.toThrow(); expect(f.db).toEqual(before)
})

it('single-event adapter reauthenticates real source and Content owner, then retries a lost response without writes', async () => {
  const { vi } = await import('vitest')
  const { applyReplayContentUpdate } = await import('./replayContentUpdate')
  const { cloudProjectsApi } = await import('@/api/cloudProjects')
  const { encryptedSyncV2Api } = await import('@/api/encryptedSyncV2')
  const { encryptedSyncV3Api } = await import('@/api/encryptedSyncV3')
  const { sealEmptyLocalProject } = await import('./localAuthority')
  const { metadataSetup, metadataRow, applyMetadata } = await import('./metadataPrefix.testFixture')
  const base = await metadataSetup(), m = await metadataRow(base, 1, { change: e => { e.metadata!.status = 'активен' } })
  await applyMetadata(base, m)
  const f = { ...base, m }, root = await received(f, 2, 1, null, 'upsert', p => { p.note.metadata = {} }, 'content-note')
  const wire = JSON.parse(root.wire_item); wire.event.device_id = f.m.event.header.device_id; root.wire_item = JSON.stringify(wire)
  const h = f.m.event.header, metadata = f.m.event.metadata!
  const sealed = await sealEmptyLocalProject(scope, { id: 'p', name: metadata.name, goal: metadata.goal,
    infinite: metadata.infinite, unit: metadata.unit, status: metadata.status,
    created_at: h.updated_at, updated_at: h.updated_at, payload: {} }, amk)
  f.db.localProjects![0] = sealed
  await f.notes.apply(scope, 2)
  await contentRow(f, 3, { mutation: 'update', parent: root.event_id })
  let manifest = { record_version: 5 as const, contract_version: 5 as const, metadata_cursor: 1, applied_cursor: 2,
    source_seeds: [{ project_id: 'p', bootstrap_id: f.b.bootstrap_id, origin_device_id: h.device_id,
      genesis_event_id: h.event_id, initial_event_count: 1, initial_max_server_sequence: 2 }],
    account_id: scope.accountId, device_id: scope.deviceId, canonical_user_id: scope.canonicalUserId,
    generation: crypto.randomUUID(), anchor: { registry: { current_cursor: 3, projects: [{ project_id: 'p',
      bootstrap_id: f.b.bootstrap_id, origin_device_id: h.device_id, state: 'active' as const,
      initial_event_count: 1, initial_max_server_sequence: 2 }] },
    transport: { supported_transport_version: 2 as const, writer_transport_version: 3 as const, cutover_epoch: 1 } },
    receive_cursor: 3, ciphertext_bytes: 0, phase: 'received' as const, revision: 1 }
  const snapshot = (): ReplaySnapshot => structuredClone({ manifest, rows: f.db.inbox, bindings: f.db.projectBindings,
    owners: f.db.ownerHeads, sources: f.db.localProjects, notes: f.db.localNotes, noteHistory: f.db.legacyNoteHistory }) as ReplaySnapshot
  vi.spyOn(cloudProjectsApi, 'listBootstraps').mockResolvedValue(manifest.anchor.registry)
  vi.spyOn(encryptedSyncV2Api, 'capabilities').mockResolvedValue(manifest.anchor.transport)
  vi.spyOn(encryptedSyncV3Api, 'pullRaw').mockImplementation(async (_token, _device, since) => JSON.stringify({
    protocol_version: 3, encrypted_sync_version: 3, items: f.db.inbox.slice(since).map(row => JSON.parse((row as { wire_item: string }).wire_item)),
    next_cursor: 3, has_more: false }))
  const port: import('./replayMetadata').MetadataPort = { read: async () => snapshot(),
    transaction: expected => async (s, stores, mode, schedule) => {
      expect(snapshot()).toEqual(expected)
      const result = await f.transaction(s, stores, mode, schedule)
      if (mode === 'readwrite' && result === 'applied') manifest = replayCoverage(manifest, f.db.inbox[2] as never) as typeof manifest
      return result
    } }
  try {
    const run = () => applyReplayContentUpdate(port, scope, 3, new AbortController().signal, () => true, f.auth, f.bindings as never, f.keys as never)
    const unchanged = snapshot()
    vi.mocked(cloudProjectsApi.listBootstraps).mockRejectedValueOnce(new Error('isolated network failure'))
    await expect(applyReplayContentUpdate(port, scope, 3, new AbortController().signal, () => true, f.auth, f.bindings as never, f.keys as never)).rejects.toThrow()
    expect(snapshot()).toEqual(unchanged)
    vi.mocked(cloudProjectsApi.listBootstraps).mockResolvedValueOnce({ ...manifest.anchor.registry, current_cursor: 4 })
    await expect(applyReplayContentUpdate(port, scope, 3, new AbortController().signal, () => true, f.auth, f.bindings as never, f.keys as never)).rejects.toThrow()
    expect(snapshot()).toEqual(unchanged)
    const cancelled = new AbortController(); cancelled.abort()
    await expect(applyReplayContentUpdate(port, scope, 3, cancelled.signal, () => true, f.auth, f.bindings as never, f.keys as never)).rejects.toThrow()
    expect(snapshot()).toEqual(unchanged)
    if (process.env.WORTA_CONTENT_CHROMIUM === '1') {
      const { mkdtemp, writeFile, rm } = await import('node:fs/promises')
      const { tmpdir } = await import('node:os')
      const { join } = await import('node:path')
      const { execFileSync } = await import('node:child_process')
      const directory = await mkdtemp(join(tmpdir(), 'content-owner-fixture-'))
      try {
        const path = join(directory, 'fixture.json')
        const rows = structuredClone(f.db)
        rows.projectBindings = rows.projectBindings.map(row => ({ ...row as object, remote_high_water: 3 }))
        rows.identities = [{ record_version: 1, canonical_user_id: scope.canonicalUserId, local_account_id: scope.accountId, device_id: scope.deviceId }]
        rows.accountBindings = [{ record_version: 1, local_account_id: scope.accountId, canonical_user_id: scope.canonicalUserId }]
        const wireBytes = rows.inbox.reduce<number>((sum, row) => sum + Math.floor(JSON.parse((row as { wire_item: string }).wire_item).object.ciphertext.length * 3 / 4), 0)
        rows.replayManifest = [{ ...manifest, record_version: 4, contract_version: 4, ciphertext_bytes: wireBytes }]
        await writeFile(path, JSON.stringify({ scope, key: [...amk], anchor: manifest.anchor, stores: rows }), { mode: 0o600 })
        const output = execFileSync(process.execPath, ['scripts/browser-content-replay-adapter-acceptance.mjs', path], { timeout: 60000, encoding: 'utf8' })
        expect(JSON.parse(output.trim().split('\n').at(-1)!)).toMatchObject({ pass: true, scenarios: 15 })
      } finally { await rm(directory, { recursive: true, force: true }) }
    }
    expect(await run()).toMatchObject({ status: 'applied', projectOpened: false, ready: false })
    const saved = snapshot()
    expect(await run()).toMatchObject({ status: 'already_applied' }); expect(snapshot()).toEqual(saved)
  } finally { vi.restoreAllMocks() }
}, 90000)

it.each(['parent', 'revision', 'ciphertext'] as const)('genuine producer invalid %s stays unproven with no source transition', async kind => {
  const f = await routeSetup(), root = await received(f, 2, 1, null, 'upsert', p => { p.note.metadata = {} }, 'content-note')
  await f.notes.apply(scope, 2)
  const child = await contentRow(f, 3, { mutation: 'update', parent: kind === 'parent' ? crypto.randomUUID() : root.event_id,
    revision: kind === 'revision' ? 4 : 2 })
  if (kind === 'ciphertext') { const wire = JSON.parse(child.row.wire_item); wire.object.ciphertext = 'AA'; child.row.wire_item = JSON.stringify(wire) }
  const before = structuredClone(f.db), runtime = new BrowserContentNoteApplyRuntime(f.notes)
  await expect(runtime.apply(scope, 3)).rejects.toThrow(); expect(f.db).toEqual(before)
})
