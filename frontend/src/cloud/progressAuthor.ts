import { progressMicros, validateProgressEvent, type ProgressEvent, type ProgressFact } from './progressCodec'
import { progressDisplay, transitionProgress, type ProgressChain } from './progressTransition'
import { UNIT_FACTORS } from '@/core/projects/units'

/** Inputs must come from a freshly authenticated owner snapshot. This function
 * prepares a frame payload only: it does not authorize, persist or publish it. */
export interface ProgressAuthorContext {
  parent: ProgressEvent
  chain: ProgressChain
  deviceId: string
  metadataEventId: string
  stageEventIds: string[]
  now: string
  eventId: string
}

function decimal(value: bigint): string {
  const sign = value < 0n ? '-' : '', n = value < 0n ? -value : value
  const result = `${sign}${n / 1_000_000n}.${(n % 1_000_000n).toString().padStart(6, '0')}`
  progressMicros(result)
  return result
}

function event(context: ProgressAuthorContext, operation: 'append' | 'tombstone'): ProgressEvent {
  if (context.chain.migration) throw new Error('progress_migration_incomplete')
  const previous = context.parent.header
  return {
    version: 1, migration: null, base_total: null, entries: [],
    selected_event_id: previous.event_id, target_entry_id: null, rebased_from: [],
    header: { ...structuredClone(previous), device_id: context.deviceId, event_id: context.eventId,
      metadata_event_id: context.metadataEventId, stage_event_ids: [...context.stageEventIds],
      parents: [previous.event_id], revision: previous.revision + 1, generation: previous.generation + 1,
      operation, updated_at: context.now },
  }
}

/** Build the same absolute-total action as the Native Progress owner. */
export function prepareProgressAppend(context: ProgressAuthorContext, entry: {
  entryId: string; totalSymbols: string; unit: ProgressFact['unit']; writingDay: string
}): ProgressEvent {
  const next = progressMicros(entry.totalSymbols)
  if (next < 0n) throw new RangeError('invalid_progress_total')
  const prior = context.chain.entries.at(-1)
  let base = progressMicros(prior?.new_total ?? context.chain.base_total)
  if (prior && prior.unit !== entry.unit) {
    if (base > BigInt(Number.MAX_SAFE_INTEGER)) throw new RangeError('progress_precision_unsupported')
    base = BigInt(Math.round(progressDisplay(base, entry.unit) * UNIT_FACTORS[entry.unit] * 1_000_000))
    if (base > BigInt(Number.MAX_SAFE_INTEGER)) throw new RangeError('progress_precision_unsupported')
  }
  if (next === base) throw new RangeError('progress_unchanged')
  const result = event(context, 'append')
  result.entries = [{ entry_id: entry.entryId, new_total: entry.totalSymbols, delta: decimal(next - base),
    unit: entry.unit, occurred_at: context.now, writing_time: null, writing_day: entry.writingDay }]
  validateProgressEvent(result)
  transitionProgress(result, context.chain)
  return result
}

/** Delete one action and preserve each following action's original delta. */
export function prepareProgressTombstone(context: ProgressAuthorContext, targetId: string, newEntryIds: string[]): ProgressEvent {
  const index = context.chain.entries.findIndex(value => value.entry_id === targetId)
  if (index < 0) throw new Error('progress_target_unknown')
  const descendants = context.chain.entries.slice(index + 1)
  if (descendants.length !== newEntryIds.length) throw new Error('progress_descendant_rebase_required')
  let total = progressMicros(index ? context.chain.entries[index - 1]!.new_total : context.chain.base_total)
  const result = event(context, 'tombstone')
  result.target_entry_id = targetId
  result.rebased_from = descendants.map(value => value.entry_id)
  result.entries = descendants.map((value, i) => {
    total += progressMicros(value.delta)
    if (total < 0n) throw new RangeError('invalid_progress_total')
    return { ...value, entry_id: newEntryIds[i]!, new_total: decimal(total) }
  })
  validateProgressEvent(result)
  transitionProgress(result, context.chain)
  return result
}
