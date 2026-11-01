// @vitest-environment node
import { afterEach, expect, it, vi } from 'vitest'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api } from '@/api/encryptedSyncV3'
import { metadataSetup, metadataRow, applyMetadata } from './metadataPrefix.testFixture'
import { received } from './legacyNoteApply.testFixture'
import { contentRow } from './noteTransport.testFixture'
import { sealEmptyLocalProject } from './localAuthority'
import { scope, amk } from './bootstrap.testFixture'
import { composeReplayOrderedContent, composeReplayMetadata, type MetadataPort } from './replayMetadata'
import { replayCoverage, type ReplaySnapshot } from './replayInventory'
import { BrowserContentNoteApplyRuntime } from './contentNoteApply'
import { stageRow } from './stageApply.testFixture'
import { orderRow } from './stageOrder.testFixture'

afterEach(() => vi.restoreAllMocks())

async function setup(contentOptions: Parameters<typeof contentRow>[2] = {}, withStructure = false, conflict = false, contentGenesis = false) {
  const base = await metadataSetup(), m = await metadataRow(base, 1, { change: e => { e.metadata!.status = 'активен' } })
  await applyMetadata(base, m)
  const f = { ...base, m }
  if (withStructure) {
    const structural = { ...f, metadataEvent: m.event }
    const stage = await stageRow(structural as never, 2, { change: e => { e.header.entity_id = 's0' } })
    await orderRow({ ...structural, stageEvents: [stage.event] } as never, 3)
  }
  const noteSequence = withStructure ? 4 : 2, contentSequence = noteSequence + 1
  const root = contentGenesis
    ? (await contentRow(f, noteSequence, { change: e => { if ('note' in e.event) e.event.note.content_format = 'plain' } })).row
    : await received(f, noteSequence, 1, null, 'upsert', p => { p.note.metadata = {} }, 'content-note')
  if (!contentGenesis) { const wire = JSON.parse(root.wire_item); wire.event.device_id = m.event.header.device_id; root.wire_item = JSON.stringify(wire) }
  const h = m.event.header, metadata = m.event.metadata!
  f.db.localProjects![0] = await sealEmptyLocalProject(scope, { id: 'p', name: metadata.name, goal: metadata.goal,
    infinite: metadata.infinite, unit: metadata.unit, status: metadata.status,
    created_at: h.updated_at, updated_at: h.updated_at, payload: {} }, amk)
  await contentRow(f, contentSequence, { mutation: 'update', parent: root.event_id, ...contentOptions,
    change: e => { if (contentGenesis && 'note' in e.event) e.event.note.content_format = 'plain'; contentOptions?.change?.(e) } })
  if (conflict) await contentRow(f, contentSequence + 1, { mutation: 'update', parent: root.event_id,
    change: event => { event.device_id = '55555555-5555-4555-8555-555555555555'; if (contentGenesis && 'note' in event.event) event.event.note.content_format = 'plain' } })
  const total = contentSequence + (conflict ? 1 : 0)
  let manifest = { record_version: (conflict ? 6 : 5) as 5 | 6, contract_version: (conflict ? 6 : 5) as 5 | 6, metadata_cursor: 1, applied_cursor: 1,
    source_seeds: [{ project_id: 'p', bootstrap_id: f.b.bootstrap_id, origin_device_id: h.device_id,
      genesis_event_id: h.event_id, initial_event_count: contentGenesis ? 0 : 1, initial_max_server_sequence: contentGenesis ? 0 : noteSequence }],
    account_id: scope.accountId, device_id: scope.deviceId, canonical_user_id: scope.canonicalUserId,
    generation: crypto.randomUUID(), anchor: { registry: { current_cursor: total, projects: [{ project_id: 'p',
      bootstrap_id: f.b.bootstrap_id, origin_device_id: h.device_id, state: 'active' as const,
      initial_event_count: contentGenesis ? 0 : 1, initial_max_server_sequence: contentGenesis ? 0 : noteSequence }] },
    transport: { supported_transport_version: 2 as const, writer_transport_version: 3 as const, cutover_epoch: 1 } },
    receive_cursor: total, ciphertext_bytes: 0, phase: 'received' as const, revision: 1 }
  const snapshot = (): ReplaySnapshot => structuredClone({ manifest, rows: f.db.inbox, bindings: f.db.projectBindings,
    owners: f.db.ownerHeads, sources: f.db.localProjects, notes: f.db.localNotes, noteHistory: f.db.legacyNoteHistory }) as ReplaySnapshot
  vi.spyOn(cloudProjectsApi, 'listBootstraps').mockImplementation(async () => structuredClone(manifest.anchor.registry))
  vi.spyOn(encryptedSyncV2Api, 'capabilities').mockImplementation(async () => structuredClone(manifest.anchor.transport))
  vi.spyOn(encryptedSyncV3Api, 'pullRaw').mockImplementation(async (_token, _device, since) => JSON.stringify({
    protocol_version: 3, encrypted_sync_version: 3, items: (f.db.inbox.slice(since) as Array<{ wire_item: string }>).map(row => JSON.parse(row.wire_item)),
    next_cursor: total, has_more: false }))
  const port: MetadataPort = { read: async () => snapshot(), transaction: expected => async (s, stores, mode, schedule) => {
    expect(snapshot()).toEqual(expected)
    const result = await f.transaction(s, stores, mode, schedule)
    if (mode === 'readwrite' && (result === 'applied' || result === 'conflict_preserved')) {
      const sequence = manifest.applied_cursor + 1
      manifest = replayCoverage(manifest, f.db.inbox[sequence - 1] as never) as typeof manifest
    }
    return result
  } }
  const run = () => composeReplayOrderedContent(port, scope, new AbortController().signal, () => true,
    f.auth, f.bindings as never, f.keys as never)
  return { f, snapshot, run, port }
}

it('composes two distinct peer Content branches and reauthenticates the conflict on retry', async () => {
  const { snapshot, run } = await setup({}, false, true)
  expect(await run()).toMatchObject({ kind: 'bounded_peer_content_conflict_preserved', contentProofs: 2, ready: false })
  const saved = snapshot()
  expect(saved.rows.map(row => row.state)).toEqual(['applied', 'note_proven', 'content_note_proven', 'content_note_conflict_preserved'])
  expect(await run()).toMatchObject({ kind: 'bounded_peer_content_conflict_preserved' })
  expect(snapshot()).toEqual(saved)
})

it('applies a Content genesis and two peer branches with owner proofs', async () => {
  const { snapshot, run } = await setup({}, false, true, true)
  expect(await run()).toMatchObject({ kind: 'bounded_peer_content_conflict_preserved', contentProofs: 3, ready: false })
  const saved = snapshot()
  expect(saved.rows.map(row => row.state)).toEqual(['applied', 'content_note_proven', 'content_note_proven', 'content_note_conflict_preserved'])
  expect(await run()).toMatchObject({ contentProofs: 3 })
  expect(snapshot()).toEqual(saved)
})

it('rejects damaged Content genesis before any Note owner commit', async () => {
  const { f, snapshot, run } = await setup({}, false, true, true)
  const row = f.db.inbox[1]! as { wire_item: string }, wire = JSON.parse(row.wire_item)
  wire.object.ciphertext = 'AA'; row.wire_item = JSON.stringify(wire)
  const before = snapshot()
  await expect(run()).rejects.toThrow()
  expect(snapshot()).toEqual(before)
})

it('rejects a peer branch whose parent is not the authenticated Content genesis', async () => {
  const { snapshot, run } = await setup({ parent: crypto.randomUUID() }, false, true, true)
  const before = snapshot()
  await expect(run()).rejects.toThrow('unsupported_content_cohort')
  expect(snapshot()).toEqual(before)
})

it('does not qualify a Content branch with a missing Metadata dependency', async () => {
  const { snapshot, run } = await setup({ change: e => { e.dependencies.metadata_event_id = crypto.randomUUID() } }, false, true, true)
  await expect(run()).rejects.toThrow()
  const after = snapshot()
  expect(after.rows.at(-1)?.state).toBe('received')
  expect(after.noteHistory?.filter(row => row.record_kind === 'group')).toEqual([])
})

it('rejects a Content branch with a route different from its Content genesis', async () => {
  const { snapshot, run } = await setup({ change: e => { if ('note' in e.event) e.event.note.content_format = 'html' } }, false, true, true)
  await expect(run()).rejects.toThrow()
  expect(snapshot().rows.at(-1)?.state).toBe('received')
  expect(snapshot().noteHistory?.filter(row => row.record_kind === 'group')).toEqual([])
})

it('rejects a damaged second branch before applying the Legacy prefix', async () => {
  const { f, snapshot, run } = await setup({}, false, true)
  const row = f.db.inbox[3]! as { wire_item: string }, wire = JSON.parse(row.wire_item)
  wire.object.ciphertext = 'AA'; row.wire_item = JSON.stringify(wire)
  const before = snapshot()
  await expect(run()).rejects.toThrow()
  expect(snapshot()).toEqual(before)
})

it('rejects sibling branches attributed to the same peer before applying the prefix', async () => {
  const { snapshot, run } = await setup({ change: event => { event.device_id = '55555555-5555-4555-8555-555555555555' } }, false, true)
  const before = snapshot()
  await expect(run()).rejects.toThrow('unsupported_content_cohort')
  expect(snapshot()).toEqual(before)
})

it('does not join a plain Content branch to an HTML Legacy parent', async () => {
  const { snapshot, run } = await setup({ change: event => {
    if (!('note' in event.event)) throw new Error('expected ordinary Content Note')
    event.event.note.content_format = 'plain'
  } }, false, true)
  await expect(run()).rejects.toThrow()
  const after = snapshot()
  expect(after.rows.slice(-2).map(row => row.state)).toEqual(['received', 'received'])
  expect(after.noteHistory?.filter(row => row.record_kind === 'group')).toEqual([])
})

it.runIf(process.env.WORTA_CONFLICT_CHROMIUM === '1').each([false, true])('persists the peer conflict with Content genesis=%s in Chromium IndexedDB and reopens', async contentGenesis => {
  const { f, snapshot } = await setup({}, false, true, contentGenesis)
  const { mkdtemp, writeFile, rm } = await import('node:fs/promises')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  const { execFileSync } = await import('node:child_process')
  const directory = await mkdtemp(join(tmpdir(), 'worta-peer-fixture-'))
  try {
    const data = snapshot(), rows = structuredClone(f.db)
    rows.projectBindings = rows.projectBindings.map(row => ({ ...row as object, remote_high_water: 4 }))
    rows.identities = [{ record_version: 1, canonical_user_id: scope.canonicalUserId, local_account_id: scope.accountId, device_id: scope.deviceId }]
    rows.accountBindings = [{ record_version: 1, local_account_id: scope.accountId, canonical_user_id: scope.canonicalUserId }]
    const bytes = rows.inbox.reduce<number>((sum, row) => sum + Math.floor(JSON.parse((row as { wire_item: string }).wire_item).object.ciphertext.length * 3 / 4), 0)
    rows.replayManifest = [{ ...data.manifest!, ciphertext_bytes: bytes }]
    const path = join(directory, 'fixture.json')
    await writeFile(path, JSON.stringify({ scope, key: [...amk], anchor: data.manifest!.anchor, stores: rows }), { mode: 0o600 })
    const output = execFileSync(process.execPath, ['scripts/browser-replay-conflict-local.mjs', path], { timeout: 90000, encoding: 'utf8' })
    expect(JSON.parse(output.trim().split('\n').at(-1)!)).toMatchObject({ pass: true })
  } finally { await rm(directory, { recursive: true, force: true }) }
}, 120000)

it('commits the Legacy prefix before one real Content owner update and reopens without writes', async () => {
  const { f, snapshot, run, port } = await setup()
  await expect(composeReplayMetadata(port, scope, new AbortController().signal, () => true,
    f.auth, f.bindings as never, f.keys as never, true, true)).rejects.toThrow()
  expect(snapshot().manifest!.applied_cursor).toBe(1)
  expect(await run()).toMatchObject({ kind: 'bounded_ordered_content_update_applied', contentProofs: 1,
    noteProofs: 1, projectOpened: false, ready: false })
  const applied = snapshot()
  expect(applied.rows.map(row => row.state)).toEqual(['applied', 'note_proven', 'content_note_proven'])
  expect(applied.manifest!.applied_cursor).toBe(3)
  expect(await run()).toMatchObject({ contentProofs: 1 })
  expect(snapshot()).toEqual(applied)
})

it('commits Metadata, Stage, StageOrder, Legacy and Content in account sequence', async () => {
  const { snapshot, run } = await setup({}, true)
  expect(await run()).toMatchObject({ metadataProofs: 1, stageProofs: 1, stageOrderProofs: 1,
    noteProofs: 1, contentProofs: 1, ownerProofs: 5 })
  expect(snapshot().rows.map(row => row.state)).toEqual([
    'applied', 'stage_proven', 'stage_order_proven', 'note_proven', 'content_note_proven',
  ])
})

it('rejects corrupt terminal wire before committing the valid prefix', async () => {
  const { snapshot, run, f } = await setup()
  const row = f.db.inbox[2]! as { wire_item: string }, wire = JSON.parse(row.wire_item)
  wire.object.ciphertext = 'AA'; row.wire_item = JSON.stringify(wire)
  const before = snapshot()
  await expect(run()).rejects.toThrow()
  expect(snapshot()).toEqual(before)
})

it('retains only the proven prefix when Content dependencies fail', async () => {
  const { snapshot, run } = await setup({ change: event => { event.dependencies.metadata_event_id = crypto.randomUUID() } })
  await expect(run()).rejects.toThrow()
  const after = snapshot()
  expect(after.rows.map(row => row.state)).toEqual(['applied', 'note_proven', 'received'])
  expect(after.manifest!.applied_cursor).toBe(2)
  await expect(run()).rejects.toThrow()
  expect(snapshot()).toEqual(after)
})

it('recovers a lost Content commit response from genuine durable proofs', async () => {
  const { snapshot, run } = await setup()
  const original = BrowserContentNoteApplyRuntime.prototype.commit
  vi.spyOn(BrowserContentNoteApplyRuntime.prototype, 'commit').mockImplementationOnce(async function(this: BrowserContentNoteApplyRuntime, token) {
    await original.call(this, token)
    throw Error('isolated_lost_response')
  })
  await expect(run()).rejects.toThrow('isolated_lost_response')
  const applied = snapshot()
  expect(applied.manifest!.applied_cursor).toBe(3)
  expect(applied.rows[2]!.state).toBe('content_note_proven')
  expect(await run()).toMatchObject({ contentProofs: 1, projectOpened: false })
  expect(snapshot()).toEqual(applied)
})
