import { progressMicros, validateProgressEvent, type ProgressEvent, type ProgressFact } from './progressCodec'
import { UNIT_FACTORS } from '@/core/projects/units'

/** The optional migration evidence is present only until the final authenticated chunk. */
export interface ProgressChain {
  base_total: string
  entries: ProgressFact[]
  migration?: NonNullable<ProgressEvent['migration']>
}
export function progressDisplay(symbols: bigint, unit: ProgressFact['unit'], recorded?: ProgressFact['unit']): number {
  const n = Number(symbols) / 1e6 / UNIT_FACTORS[unit]
  if (recorded === unit || unit === 'symbols') return n
  if (unit !== 'author_list') return Math.ceil(n)
  // Rust display(): (n * 10.0).round_ties_even(), distinct from Python's
  // round(n, 1) for binary-float halfway values such as 1.05.
  const scaled = n * 10, lower = Math.floor(scaled), fraction = scaled - lower
  return (fraction === 0.5 ? lower % 2 === 0 ? lower : lower + 1 : Math.round(scaled)) / 10
}
function sameProgressAction(a: ProgressFact, b: ProgressFact): boolean {
  return a.unit === b.unit && a.writing_day === b.writing_day && a.writing_time === b.writing_time
    && a.occurred_at === b.occurred_at
}

export function transitionProgress(event: ProgressEvent, parent?: ProgressChain): ProgressChain {
  const id = event.header.parents[0]
  if (event.header.parents.length > 1 || (parent !== undefined) !== (id !== undefined))
    throw new Error('progress_parent_unknown')
  return transitionProgressBranches(event, parent && id ? new Map([[id, parent]]) : new Map())
}

export function transitionProgressBranches(event: ProgressEvent, parents: ReadonlyMap<string, ProgressChain>): ProgressChain {
  validateProgressEvent(event)
  const op = event.header.operation
  if (parents.size !== event.header.parents.length || event.header.parents.some(id => !parents.has(id)))
    throw new Error('progress_parent_unknown')
  if ([...parents.values()].reduce((count, branch) => count + branch.entries.length, 0) > 131072)
    throw new Error('progress_resource_limit')
  const capture = op === 'genesis' || op === 'adopt_local'
  const parent = event.selected_event_id === null ? undefined : parents.get(event.selected_event_id)
  const chain: ProgressChain = capture
    ? { base_total: event.base_total!, entries: [], migration: structuredClone(event.migration!) }
    : structuredClone(parent!)
  if (chain.migration && !capture && op !== 'migrate') throw new Error('progress_migration_incomplete')
  if (op === 'migrate' && !chain.migration) throw new Error('progress_migration_already_complete')
  if (op === 'tombstone' || op === 'correct') {
    const position = chain.entries.findIndex(f => f.entry_id === event.target_entry_id)
    if (position < 0) throw new Error('progress_target_unknown')
    const descendants = chain.entries.slice(position + (op === 'tombstone' ? 1 : 0))
    if (event.rebased_from.length !== descendants.length || event.rebased_from.some((id, i) => id !== descendants[i]!.entry_id))
      throw new Error('progress_descendant_rebase_required')
    for (const [i, fact] of event.entries.entries()) {
      const original = descendants[i]!
      if (fact.entry_id === original.entry_id || fact.occurred_at !== original.occurred_at || fact.writing_time !== original.writing_time
        || fact.writing_day !== original.writing_day || fact.unit !== original.unit
        || ((op === 'tombstone' || i > 0) && fact.delta !== original.delta))
        throw new Error('progress_rebase_invalid')
    }
    chain.entries.length = position
  } else if (op === 'rebase') {
    const other = [...parents].filter(([id]) => id !== event.selected_event_id).map(([, branch]) => branch)
    const selectedIds = new Set(chain.entries.map(fact => fact.entry_id))
    const requested = new Map(event.rebased_from.map((id, offset) => [id, offset]))
    const sources = new Map<string, ProgressFact>()
    for (const branch of other) {
      const ordered = branch.entries.filter(fact => requested.has(fact.entry_id))
      if (ordered.some((fact, i) => i > 0 && requested.get(ordered[i - 1]!.entry_id)! >= requested.get(fact.entry_id)!))
        throw new Error('progress_rebase_order_invalid')
      for (const fact of ordered) {
        const previous = sources.get(fact.entry_id)
        if (previous && (previous.delta !== fact.delta || !sameProgressAction(previous, fact)))
          throw new Error('progress_rebase_invalid')
        sources.set(fact.entry_id, fact)
      }
    }
    for (const [i, source] of event.rebased_from.entries()) {
      const original = sources.get(source), next = event.entries[i]!
      if (!original || selectedIds.has(source) || next.entry_id === source
        || original.delta !== next.delta || !sameProgressAction(original, next))
        throw new Error('progress_rebase_invalid')
    }
  }
  const ids = new Set(chain.entries.map(f => f.entry_id))
  let total = progressMicros(chain.entries.at(-1)?.new_total ?? chain.base_total)
  for (const fact of event.entries) {
    if (ids.has(fact.entry_id) || chain.entries.length >= 65536) throw new Error('progress_duplicate_entry')
    const previous = chain.entries.at(-1)
    // Native append_base uses the current unit's visible rounded preceding total.
    if (op === 'append' && previous && previous.unit !== fact.unit) {
      const factor = UNIT_FACTORS[fact.unit]
      total = BigInt(Math.round(progressDisplay(total, fact.unit) * factor * 1e6))
      if (total > 1000000000000000000n) throw new Error('progress_resource_limit')
    }
    const next = progressMicros(fact.new_total), difference = total + progressMicros(fact.delta) - next
    const tolerance = capture || op === 'migrate' ? 1n : 0n
    if (difference > tolerance || difference < -tolerance) throw new Error('progress_base_inconsistent')
    chain.entries.push(structuredClone(fact)); ids.add(fact.entry_id); total = next
  }
  if (chain.migration) {
    if (chain.entries.length > chain.migration.entry_count) throw new Error('progress_migration_count_invalid')
    if (chain.entries.length === chain.migration.entry_count) {
      if (progressMicros(chain.migration.final_total) !== total) throw new Error('progress_migration_total_invalid')
      delete chain.migration
    }
  }
  return chain
}
