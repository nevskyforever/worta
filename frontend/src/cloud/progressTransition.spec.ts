import { expect, it } from 'vitest'
import golden from './__fixtures__/progressCodecV1.json'
import { transitionProgress, progressDisplay } from './progressTransition'
import type { ProgressEvent } from './progressCodec'
import { presentProgress } from '@/platform/web/progressView'
it('interprets Native fixed-symbol genesis and linear append without changing original facts', () => {
  const genesis = structuredClone(golden.examples[0]!.event) as ProgressEvent
  const before = structuredClone(genesis), parent = transitionProgress(genesis)
  const append = structuredClone(genesis)
  Object.assign(append.header, { operation:'append', event_id:crypto.randomUUID(), parents:[genesis.header.event_id], revision:2, generation:2 })
  Object.assign(append, { base_total:null, migration:null, selected_event_id:genesis.header.event_id, entries:[{...genesis.entries[0]!,entry_id:'next',new_total:'170.000000',delta:'50.000000'}] })
  const chain = transitionProgress(append,parent)
  expect(genesis).toEqual(before);expect(parent.entries).toHaveLength(1)
  const view=presentProgress({...chain,project_id:'P1',stage_id:null,event_id:append.header.event_id}, {unit:'symbols',goal:1000,infinite:false,created_at:'2026-10-01'},'2026-10-03')
  expect(view).toMatchObject({total:170,progress:17,remaining:830,goal:1000})
  expect(view.statistics.metrics).toMatchObject({entries_count:2,active_days:1,average_symbols_per_entry:35})
  expect(view.statistics.timeline).toEqual([{date:'2026-10-02',symbols:70,value:70}])
  append.entries[0]!.delta='49.000000'
  expect(()=>transitionProgress(append,parent)).toThrow('progress_base_inconsistent')
})
it('preserves Native same-unit fractions and rounds only conversions',()=>{
 expect(progressDisplay(2400000000n,'A4','A4')).toBe(2400/1800)
 expect(progressDisplay(2400000000n,'A4','symbols')).toBe(2)
 expect(progressDisplay(42000000000n,'author_list','symbols')).toBe(1)
})
it('does not manufacture a completed migration or accept duplicate facts',()=>{
 const e=structuredClone(golden.examples[0]!.event) as ProgressEvent
 e.migration!.entry_count++
 expect(()=>transitionProgress(e)).toThrow('progress_migration_incomplete')
 e.migration!.entry_count=1;e.entries[0]!.delta='19.000000'
 expect(()=>transitionProgress(e)).toThrow('progress_base_inconsistent')
})
