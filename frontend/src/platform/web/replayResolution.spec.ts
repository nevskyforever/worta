// @vitest-environment node
import { afterEach, expect, it, vi } from 'vitest'
import { plannerFixture } from './resolutionPlanner.testFixture'
import { scope } from './bootstrap.testFixture'
import type { BrowserSyncTransaction } from './syncDurability'
import { applyReplayResolutionOwner } from './replayResolution'
afterEach(() => vi.restoreAllMocks())
async function setup() {
  const f = await plannerFixture('choose_version', 2, false, false, { format: 'plain', compressed: true })
  const run = (contractVersion = 6) => applyReplayResolutionOwner({ contractVersion, scope, sequence: f.sequence,
    transaction: f.f.transaction, current: () => true, auth: f.f.auth, bindings: f.f.bindings as never,
    keys: f.f.keys as never, device: f.device })
  return { f, run }
}
it('applies through the real owner, retains original wire and proofs, and reauthenticates without writes', async () => {
  const { f, run } = await setup(), wire = f.received.wire_item
  expect(await run()).toMatchObject({ status: 'applied', ready: false, declarationAllowed: false, writerAuthorized: false })
  expect(f.received.wire_item).toBe(wire)
  const saved = f.before()
  expect(await run()).toMatchObject({ status: 'already_applied' })
  expect(f.before()).toEqual(saved)
})
it('rejects an old replay contract before owner writes', async () => {
  const { f, run } = await setup(), saved = f.before()
  await expect(run(5)).rejects.toThrow('unsupported_resolution_cohort')
  expect(f.before()).toEqual(saved)
})
it.each(['ciphertext', 'parent', 'selected'] as const)('rejects invalid %s without writes', async kind => {
  const { f, run } = await setup()
  if (kind === 'ciphertext') { const w = JSON.parse(f.received.wire_item); w.object.ciphertext = 'AA'; f.received.wire_item = JSON.stringify(w) }
  if (kind === 'parent') { f.resolution.header.parent_event_id = crypto.randomUUID(); await f.reseal(true) }
  if (kind === 'selected') { if ('selected_event_id' in f.resolution.resolution) f.resolution.resolution.selected_event_id = crypto.randomUUID(); await f.reseal(true) }
  const saved = f.before()
  await expect(run()).rejects.toThrow()
  expect(f.before()).toEqual(saved)
})
it('rejects damaged persisted proofs on repeated delivery', async () => {
  const { f, run } = await setup(); await run()
  f.received.resolution_proof.outcome_digest = '0'.repeat(64)
  const saved = f.before(); await expect(run()).rejects.toThrow(); expect(f.before()).toEqual(saved)
})

it('rolls back the complete owner write when the transaction aborts', async () => {
  const { f } = await setup(), saved = f.before()
  const transaction: BrowserSyncTransaction = (scope, stores, mode, schedule) => f.f.transaction(scope, stores, mode,
    (tx: IDBTransaction, result: (value: unknown) => void, abort: (error: unknown) => void) => schedule(tx, value => mode === 'readwrite' ? abort(Error('injected_abort')) : result(value), abort))
  await expect(applyReplayResolutionOwner({ contractVersion: 6, scope, sequence: f.sequence, transaction,
    current: () => true, auth: f.f.auth, bindings: f.f.bindings as never, keys: f.f.keys as never,
    device: f.device })).rejects.toThrow('injected_abort')
  expect(f.before()).toEqual(saved)
})
