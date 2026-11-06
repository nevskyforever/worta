import { progressMicros, validateProgressEvent, type ProgressEvent, type ProgressFact } from './progressCodec'
import { UNIT_FACTORS } from '@/core/projects/units'

/** Native progress_sync::Chain/transition, restricted to complete linear capture
 * and append. Conflict/rebase/correction and incomplete migration fail closed. */
export interface ProgressChain { base_total: string; entries: ProgressFact[] }
export function progressDisplay(symbols: bigint, unit: ProgressFact['unit'], recorded?: ProgressFact['unit']): number {
  const n = Number(symbols) / 1e6 / UNIT_FACTORS[unit]
  if (recorded === unit || unit === 'symbols') return n
  if (unit !== 'author_list') return Math.ceil(n)
  // Rust display(): (n * 10.0).round_ties_even(), distinct from Python's
  // round(n, 1) for binary-float halfway values such as 1.05.
  const scaled = n * 10, lower = Math.floor(scaled), fraction = scaled - lower
  return (fraction === 0.5 ? lower % 2 === 0 ? lower : lower + 1 : Math.round(scaled)) / 10
}
export function transitionProgress(event: ProgressEvent, parent?: ProgressChain): ProgressChain {
  validateProgressEvent(event)
  const op = event.header.operation
  if (op !== 'genesis' && op !== 'append') throw new Error('progress_operation_unsupported')
  if (op === 'genesis' ? !!parent : !parent || event.selected_event_id !== event.header.parents[0]) throw new Error('progress_parent_unknown')
  const chain: ProgressChain = parent ? structuredClone(parent) : { base_total: event.base_total!, entries: [] }
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
    if (difference > (op === 'genesis' ? 1n : 0n) || difference < (op === 'genesis' ? -1n : 0n)) throw new Error('progress_base_inconsistent')
    chain.entries.push(structuredClone(fact)); ids.add(fact.entry_id); total = next
  }
  if (op === 'genesis' && (event.migration!.entry_count !== chain.entries.length || progressMicros(event.migration!.final_total) !== total)) throw new Error('progress_migration_incomplete')
  return chain
}
