import { expect, it } from 'vitest'
import fixture from './__fixtures__/progressCodecV1.json'
import { frameProgressEvent, unframeProgressEvent, type ProgressEvent } from './progressCodec'
import { transitionProgress } from './progressTransition'
import { prepareProgressAppend, prepareProgressTombstone } from './progressAuthor'

it('prepares native-compatible additions and deletions without mutating the verified snapshot', async () => {
  const parent = structuredClone(fixture.examples[0]!.event) as ProgressEvent
  const chain = transitionProgress(parent), original = structuredClone(chain)
  const context = { parent, chain, deviceId: crypto.randomUUID(), metadataEventId: parent.header.metadata_event_id,
    stageEventIds: parent.header.stage_event_ids, now: '2026-10-03T00:00:00.000000Z', eventId: crypto.randomUUID() }
  const append = prepareProgressAppend(context, { entryId: crypto.randomUUID(), totalSymbols: '170.000000', unit: 'symbols', writingDay: '2026-10-03' })
  expect(append.entries[0]!.delta).toBe('50.000000')
  expect(await unframeProgressEvent(await frameProgressEvent(append))).toEqual(append)
  const next = transitionProgress(append, chain)
  const deletion = prepareProgressTombstone({ ...context, parent: append, chain: next, eventId: crypto.randomUUID() }, chain.entries[0]!.entry_id, [crypto.randomUUID()])
  expect(deletion.rebased_from).toEqual([append.entries[0]!.entry_id])
  expect(transitionProgress(deletion, next).entries[0]!.new_total).toBe('150.000000')
  expect(chain).toEqual(original)
  expect(() => prepareProgressTombstone({ ...context, parent: append, chain: next }, chain.entries[0]!.entry_id, [])).toThrow('progress_descendant_rebase_required')
})

it('rejects unchanged totals and refuses imprecise unit conversion', () => {
  const parent = structuredClone(fixture.examples[0]!.event) as ProgressEvent
  const chain = transitionProgress(parent)
  const context = { parent, chain, deviceId: crypto.randomUUID(), metadataEventId: parent.header.metadata_event_id,
    stageEventIds: parent.header.stage_event_ids, now: '2026-10-03T00:00:00.000000Z', eventId: crypto.randomUUID() }
  expect(() => prepareProgressAppend(context, { entryId: 'entry', totalSymbols: '120.000000', unit: 'symbols', writingDay: '2026-10-03' })).toThrow('progress_unchanged')
  const large = structuredClone(chain)
  large.entries[0]!.new_total = '10000000000.000000'
  expect(() => prepareProgressAppend({ ...context, chain: large }, { entryId: 'entry', totalSymbols: '10000001000.000000', unit: 'A4', writingDay: '2026-10-03' })).toThrow('progress_precision_unsupported')
})
