import { joinGameRecords, type GameLedger, type GameProjection } from './gameRecords'
import { joinProgressRecords } from './progressRecords'
import { joinMapRecords } from './mapRecords'
import { joinDocumentRecords } from './documentRecords'
import {inspectContentAuthorityInventory} from './contentAuthorityInventory'
import {validateContentSelfEchoProof2} from './contentAuthorityRecords'
import { readFutureInboxRecord, readFutureHistoryRecord, type FutureInbox, type FutureNoteHistory } from './resolutionRecords'
import { inspectResolutionDag } from './resolutionDag'
import { contentHistoryJoin } from './contentNoteProof'
import { contentConflictHistoryJoin } from './contentNoteConflictProof'
import { BrowserPersistenceError } from './persistence'
import { validateBrowserSyncCursor, type BrowserSyncScope } from './syncDurability'
import { validateBrowserProjectBinding, type BrowserProjectBinding } from './projectBindings'
import type { BrowserMetadataHead, BrowserMetadataProof } from './metadataApplyRecords'
import { validateBrowserOwnerHead, stageOwnerId, type BrowserStageHead, type BrowserStageProof } from './stageRecords'
import type { BrowserStageOrderHead, BrowserStageOrderProof } from './stageOrderRecords'
import { validateLocalProjectRecord, validateLocalNoteRecord, type LocalProjectRecord, type LocalNoteRecord } from './localAuthorityRecords'
import { validateNoteHistorySet, type BrowserNoteHistoryRecord, type BrowserNoteCandidate, type BrowserNoteHead } from './noteHistoryRecords'
import { REPLAY_LIMITS, replayExact, replaySame, replayUuid, validateReplayManifest, type ReplayManifest } from './replayRecords'

export interface ReplaySnapshot {
  games?: { ledger: unknown[]; projections: unknown[] }
  progress?: { ledger: unknown[]; projections: unknown[] }
  maps?: { ledger: unknown[]; projections: unknown[] }
  documents?: { ledger: unknown[]; projections: unknown[] }
  checkpoint?: ReturnType<typeof validateBrowserSyncCursor>
  manifest: ReplayManifest | null; rows: FutureInbox[]; bindings: BrowserProjectBinding[]; owners: Array<BrowserMetadataHead | BrowserStageHead | BrowserStageOrderHead>
  author?: {out:unknown[];receipts:unknown[];intents:unknown[]}
  sources?: LocalProjectRecord[]
  notes?: LocalNoteRecord[]; noteHistory?: FutureNoteHistory[]
}
/** Derived coverage is never a qualified cursor or owner certificate. */
export function replayNoteOutcome(row: FutureInbox) {
  if (row.state === 'note_proven') return row.note_proof
  if (row.state === 'content_note_proven') return row.content_note_proof
  throw new BrowserPersistenceError('corrupt_record')
}
export function replayCoverage(manifest: ReplayManifest, row: FutureInbox) {
  const sequence = row.server_sequence
  return { ...manifest, applied_cursor: sequence, metadata_cursor: row.state === 'applied' && sequence === (manifest.metadata_cursor ?? 0) + 1 ? sequence : manifest.metadata_cursor,
    revision: manifest.revision + 1 }
}
export function replayAppliedProof(row: FutureInbox): BrowserMetadataProof | BrowserStageProof | BrowserStageOrderProof | undefined {
  return row.state === 'applied' ? row.applied_proof : row.state === 'stage_proven' ? row.stage_proof : row.state === 'stage_order_proven' ? row.stage_order_proof : undefined
}
function assertGameBatchTransition(before:ReplaySnapshot,after:ReplaySnapshot,sequence:number,value:{status:unknown;game_sequences:unknown}):void{
 const list=value.game_sequences
 if(value.status!=='applied'||!Array.isArray(list)||!list.length||list.length>REPLAY_LIMITS.events||sequence!==before.manifest!.applied_cursor!+1
   ||list.some((n,i)=>!Number.isSafeInteger(n)||n!==sequence+i)||before.rows.length!==after.rows.length)fail()
 const sequences=list as number[],old=before.games,next=after.games
 if(!old||!next||next.ledger.length!==old.ledger.length+sequences.length)fail()
 for(const [index,row] of after.rows.entries()){
  const previous=before.rows[index]!
  if(!sequences.includes(row.server_sequence)){if(!replaySame(row,previous))fail();continue}
  if(previous.state!=='received'||row.state!=='game_proven')fail()
  const {game_proof:_proof,...wire}=row
  if(!replaySame({...wire,record_version:1,state:'received'},previous))fail()
 }
 if(old!.ledger.some(row=>!next!.ledger.some(value=>replaySame(row,value))))fail()
 const added=next!.ledger.filter(value=>!old!.ledger.some(row=>replaySame(row,value))) as GameLedger[]
 if(added.length!==sequences.length||added.some(row=>!sequences.includes(row.server_sequence)||after.rows[row.server_sequence-1]?.event_id!==row.event_id))fail()
 const keys=new Set(added.map(row=>JSON.stringify([row.project_id,row.entity_id])))
 const key=(row:unknown)=>{const r=row as {project_id:string;entity_id:string};return JSON.stringify([r.project_id,r.entity_id])}
 if(!replaySame(old!.projections.filter(row=>!keys.has(key(row))),next!.projections.filter(row=>!keys.has(key(row)))))fail()
 for(const k of keys){
  const chain=added.filter(row=>key(row)===k).sort((a,b)=>JSON.parse(a.wire_item).event.revision-JSON.parse(b.wire_item).event.revision)
  let previous=old!.projections.find(row=>key(row)===k) as GameProjection|undefined
  for(const ledger of chain){
   if(!replaySame(ledger.previous_projection,previous))fail()
   const row=after.rows[ledger.server_sequence-1]!
   if(row.state!=='game_proven')fail()
   const matches=[...next!.projections,...(next!.ledger as GameLedger[]).flatMap(l=>l.previous_projection?[l.previous_projection]:[])].filter(p=>(p as GameProjection).head_event_id===ledger.event_id)
   if(matches.length!==1)fail()
   previous=matches[0] as GameProjection
  }
  const final=next!.projections.filter(row=>key(row)===k)
  if(final.length!==1||!replaySame(final[0],previous))fail()
 }
 if(next!.projections.length!==old!.projections.length+[...keys].filter(k=>!old!.projections.some(row=>key(row)===k)).length)fail()
 const last=after.rows[sequences.at(-1)!-1]!
 if(!replaySame(after.manifest,replayCoverage(before.manifest!,last))||!replaySame({...after,manifest:before.manifest,rows:before.rows,games:before.games},before))fail()
}
export function assertReplayOwnerTransition(before: ReplaySnapshot, after: ReplaySnapshot, sequence: number, status: unknown): void {
  if(status&&typeof status==='object'&&'game_sequences' in status){assertGameBatchTransition(before,after,sequence,status as {status:unknown;game_sequences:unknown});return}
  if (status === 'already_applied') { if (!replaySame(before, after)) fail(); return }
  if(after.rows[sequence-1]?.state!=='game_proven'&&!replaySame(before.games,after.games))fail()
  if(after.rows[sequence-1]?.state==='game_proven') {
    const row=after.rows[sequence-1]!,old=before.rows[sequence-1]!
    if(row.state!=='game_proven'||old.state!=='received'||status!=='applied'||sequence!==before.manifest!.applied_cursor!+1
      ||after.rows.length!==before.rows.length||before.rows.some((value,index)=>index!==sequence-1&&!replaySame(value,after.rows[index])))fail()
    const {game_proof:_proof,...wire}=row
    if(!replaySame({...wire,record_version:1,state:'received'},old)||!replaySame(after.manifest,replayCoverage(before.manifest!,row)))fail()
    const doc=after.games,prior=before.games
    const event=JSON.parse(row.wire_item).event
    const committed=doc?.ledger.find(value=>(value as {event_id:string}).event_id===row.event_id) as {project_id:string;entity_id:string}|undefined
    const sameGame=(value:unknown)=>{const p=value as {project_id:string;entity_id:string};return p.project_id===committed?.project_id&&p.entity_id===committed?.entity_id}
    const previous=prior?.projections.filter(sameGame)??[],next=doc?.projections.filter(sameGame)??[]
    const added=doc?.ledger.filter(value=>!prior?.ledger.some(old=>replaySame(old,value)))??[]
    if(!doc||!prior||doc.ledger.length!==prior.ledger.length+1||added.length!==1||next.length!==1
      ||(next[0] as {head_event_id:string}).head_event_id!==row.event_id
      ||(event.revision===1 ? previous.length!==0||doc.projections.length!==prior.projections.length+1
        : previous.length!==1||doc.projections.length!==prior.projections.length||!replaySame((added[0] as {previous_projection:unknown}).previous_projection,previous[0]))
      ||prior.ledger.some(value=>!doc.ledger.some(next=>replaySame(value,next)))
      ||!replaySame(prior.projections.filter(value=>!sameGame(value)),doc.projections.filter(value=>!sameGame(value)))
      ||!replaySame({...after,rows:before.rows,manifest:before.manifest,games:before.games},before))fail()
    return
  }
  if(after.rows[sequence-1]?.state!=='progress_proven'&&!replaySame(before.progress,after.progress))fail()
  if(after.rows[sequence-1]?.state==='progress_proven') {
    const row=after.rows[sequence-1]!,old=before.rows[sequence-1]!
    if(row.state!=='progress_proven'||old.state!=='received'||status!=='applied'||sequence!==before.manifest!.applied_cursor!+1
      ||after.rows.length!==before.rows.length||before.rows.some((value,index)=>index!==sequence-1&&!replaySame(value,after.rows[index])))fail()
    const {progress_proof:_proof,...wire}=row
    if(!replaySame({...wire,record_version:1,state:'received'},old)||!replaySame(after.manifest,replayCoverage(before.manifest!,row)))fail()
    const doc=after.progress,prior=before.progress
    const event=JSON.parse(row.wire_item).event
    const sameProgress=(value:unknown)=>{const p=value as {project_id:string;entity_id:string};return p.project_id===event.project_id&&p.entity_id===event.entity_id}
    const previous=prior?.projections.filter(sameProgress)??[],next=doc?.projections.filter(sameProgress)??[]
    const added=doc?.ledger.filter(value=>!prior?.ledger.some(old=>replaySame(old,value)))??[]
    if(!doc||!prior||doc.ledger.length!==prior.ledger.length+1||added.length!==1||next.length!==1
      ||(next[0] as {head_event_id:string}).head_event_id!==row.event_id
      ||(event.revision===1 ? previous.length!==0||doc.projections.length!==prior.projections.length+1
        : previous.length!==1||doc.projections.length!==prior.projections.length||!replaySame((added[0] as {previous_projection:unknown}).previous_projection,previous[0]))
      ||prior.ledger.some(value=>!doc.ledger.some(next=>replaySame(value,next)))
      ||!replaySame(prior.projections.filter(value=>!sameProgress(value)),doc.projections.filter(value=>!sameProgress(value)))
      ||!replaySame({...after,rows:before.rows,manifest:before.manifest,progress:before.progress},before))fail()
    return
  }
  if(after.rows[sequence-1]?.state!=='map_proven'&&!replaySame(before.maps,after.maps))fail()
  if(after.rows[sequence-1]?.state==='map_proven') {
    const row=after.rows[sequence-1]!,old=before.rows[sequence-1]!
    if(row.state!=='map_proven'||old.state!=='received'||status!=='applied'||sequence!==before.manifest!.applied_cursor!+1
      ||after.rows.length!==before.rows.length||before.rows.some((value,index)=>index!==sequence-1&&!replaySame(value,after.rows[index])))fail()
    const {map_proof:_proof,...wire}=row
    if(!replaySame({...wire,record_version:1,state:'received'},old)||!replaySame(after.manifest,replayCoverage(before.manifest!,row)))fail()
    const doc=after.maps,prior=before.maps
    const event=JSON.parse(row.wire_item).event
    const sameMap=(value:unknown)=>{const p=value as {project_id:string;entity_id:string};return p.project_id===event.project_id&&p.entity_id===event.entity_id}
    const previous=prior?.projections.filter(sameMap)??[],next=doc?.projections.filter(sameMap)??[]
    const added=doc?.ledger.filter(value=>!prior?.ledger.some(old=>replaySame(old,value)))??[]
    if(!doc||!prior||doc.ledger.length!==prior.ledger.length+1||added.length!==1||next.length!==1
      ||(next[0] as {head_event_id:string}).head_event_id!==row.event_id
      ||(event.revision===1 ? previous.length!==0||doc.projections.length!==prior.projections.length+1
        : previous.length!==1||doc.projections.length!==prior.projections.length||!replaySame((added[0] as {previous_projection:unknown}).previous_projection,previous[0]))
      ||prior.ledger.some(value=>!doc.ledger.some(next=>replaySame(value,next)))
      ||!replaySame(prior.projections.filter(value=>!sameMap(value)),doc.projections.filter(value=>!sameMap(value)))
      ||!replaySame({...after,rows:before.rows,manifest:before.manifest,maps:before.maps},before))fail()
    return
  }
  if(after.rows[sequence-1]?.state!=='document_proven'&&!replaySame(before.documents,after.documents))fail()
  if(after.rows[sequence-1]?.state==='document_proven') {
    const row=after.rows[sequence-1]!,old=before.rows[sequence-1]!
    if(row.state!=='document_proven'||old.state!=='received'||status!=='applied'||sequence!==before.manifest!.applied_cursor!+1
      ||after.rows.length!==before.rows.length||before.rows.some((value,index)=>index!==sequence-1&&!replaySame(value,after.rows[index])))fail()
    const {document_proof:_proof,...wire}=row
    if(!replaySame({...wire,record_version:1,state:'received'},old)||!replaySame(after.manifest,replayCoverage(before.manifest!,row)))fail()
    const doc=after.documents,prior=before.documents
    const event=JSON.parse(row.wire_item).event
    const sameDocument=(value:unknown)=>{const p=value as {project_id:string;entity_id:string};return p.project_id===event.project_id&&p.entity_id===event.entity_id}
    const previous=prior?.projections.filter(sameDocument)??[],next=doc?.projections.filter(sameDocument)??[]
    const added=doc?.ledger.filter(value=>!prior?.ledger.some(old=>replaySame(old,value)))??[]
    if(!doc||!prior||doc.ledger.length!==prior.ledger.length+1||added.length!==1||next.length!==1
      ||(next[0] as {head_event_id:string}).head_event_id!==row.event_id
      ||(event.revision===1 ? previous.length!==0||doc.projections.length!==prior.projections.length+1
        : previous.length!==1||doc.projections.length!==prior.projections.length||!replaySame((added[0] as {previous_projection:unknown}).previous_projection,previous[0]))
      ||prior.ledger.some(value=>!doc.ledger.some(next=>replaySame(value,next)))
      ||!replaySame(prior.projections.filter(value=>!sameDocument(value)),doc.projections.filter(value=>!sameDocument(value)))
      ||!replaySame({...after,rows:before.rows,manifest:before.manifest,documents:before.documents},before))fail()
    return
  }
  if(after.rows[sequence-1]?.state==='content_note_self_echo_proven'){
    const row=after.rows[sequence-1]!,old=before.rows[sequence-1]!;if(row.state!=='content_note_self_echo_proven'||old.state!=='received'||status!=='self_echo_applied'||sequence!==before.manifest!.applied_cursor!+1)fail()
    const {content_note_self_echo_proof:_p,...wire}=row
    if(!replaySame({...wire,record_version:1,state:'received'},old)||!replaySame({...after,manifest:before.manifest,rows:before.rows},before)||!replaySame(after.manifest,replayCoverage(before.manifest!,row)))fail();return
  }
  const manifest = before.manifest!, previous = before.rows[sequence - 1], row = after.rows[sequence - 1]
  if (row?.state === 'note_resolution_proven') {
    if (manifest.contract_version !== 6 || status !== 'applied' || previous?.state !== 'received'
      || sequence !== manifest.applied_cursor! + 1 || !replaySame(after.manifest, replayCoverage(manifest, row))
      || !replaySame(before.bindings, after.bindings) || !replaySame(before.owners, after.owners)
      || !replaySame(before.rows.filter(r => r.server_sequence !== sequence), after.rows.filter(r => r.server_sequence !== sequence))) fail()
    const { resolution_proof: proof, ...wire } = row
    if (!replaySame({ ...wire, record_version: 1, state: 'received' }, previous)) fail()
    const history = after.noteHistory ?? [], old = before.noteHistory ?? []
    const resolution = history.find(r => r.record_kind === 'resolution'&&r.event_id===row.event_id)
    const transition = history.find(r => r.record_kind === 'group_transition'&&r.resolution_event_id===row.event_id)
    if (!resolution || resolution.record_kind !== 'resolution' || resolution.event_id !== row.event_id
      || resolution.record_digest !== proof.resolution_digest || resolution.strategy !== 'choose_version'
      || !transition || transition.record_kind !== 'group_transition' || transition.record_digest !== proof.transition_digest
      || history.length !== old.length + 2
      || old.filter(r => !(r.record_kind === 'head' && r.project_id === resolution.project_id && r.note_id === resolution.note_id)).some(r => !history.some(v => replaySame(r, v)))) fail()
    const source = before.sources?.find(r => r.project_id === resolution.project_id)
    const next = after.sources?.find(r => r.project_id === resolution.project_id)
    if (!source || !next || next.source_revision !== source.source_revision + 1
      || !replaySame(before.sources?.filter(r => r !== source), after.sources?.filter(r => r !== next))
      || !replaySame(before.notes?.filter(r => r.note_id !== resolution.note_id), after.notes?.filter(r => r.note_id !== resolution.note_id))) fail()
    return
  }
  if(row?.state==='content_note_conflict_preserved'&&before.noteHistory?.some(r=>r.record_kind==='resolution')){
    const old=before.noteHistory??[],next=after.noteHistory??[],proof=row.content_note_conflict_proof
    const group=next.find(r=>r.record_kind==='group'&&r.group_id===proof.group_id)
    const candidate=next.find(r=>r.record_kind==='candidate'&&r.event_id===row.event_id)
    const head=old.find(r=>r.record_kind==='head'&&r.project_id===proof.header.project_id&&r.note_id===proof.header.entity_id)
    const newHead=next.find(r=>r.record_kind==='head'&&r.project_id===proof.header.project_id&&r.note_id===proof.header.entity_id)
    const {content_note_conflict_proof:_proof,...wire}=row
    if(manifest.contract_version!==6||status!=='conflict_preserved'||previous?.state!=='received'||sequence!==manifest.applied_cursor!+1
      ||!replaySame(after.manifest,replayCoverage(manifest,row))||!replaySame({...wire,record_version:1,state:'received'},previous)
      ||!replaySame(before.rows.filter(r=>r.server_sequence!==sequence),after.rows.filter(r=>r.server_sequence!==sequence))
      ||!replaySame(before.sources,after.sources)||!replaySame(before.notes,after.notes)||!replaySame(before.bindings,after.bindings)||!replaySame(before.owners,after.owners)
      ||!group||group.record_kind!=='group'||group.members.length!==2||group.conflict_generation!==1
      ||!candidate||candidate.record_kind!=='candidate'||candidate.projection!==null||candidate.record_digest!==proof.candidate_digest
      ||!old.some(r=>(r.record_kind==='resolution'||r.record_kind==='candidate')&&r.event_id===group.common_parent_event_id)
      ||!head||head.record_kind!=='head'||!newHead||newHead.record_kind!=='head'
      ||!replaySame(newHead,{...head,head_revision:head.head_revision+1,tip_event_ids:group.tip_event_ids})
      ||next.length!==old.length+2||old.filter(r=>r!==head).some(r=>!next.some(v=>replaySame(r,v))))fail()
    return
  }
  if (row?.state === 'content_note_conflict_preserved') {
    if (manifest.contract_version !== 6 || status !== 'conflict_preserved' || previous?.state !== 'received'
      || sequence !== (manifest.applied_cursor ?? 0) + 1 || !replaySame(after.manifest, replayCoverage(manifest, row))
      || !replaySame(before.bindings, after.bindings) || !replaySame(before.owners, after.owners)
      || !replaySame(before.sources, after.sources) || !replaySame(before.notes, after.notes)
      || !replaySame(before.rows.filter(value => value.server_sequence !== sequence), after.rows.filter(value => value.server_sequence !== sequence))) fail()
    const original = { ...row } as Record<string, unknown>; delete original.content_note_conflict_proof
    if (!replaySame({ ...original, record_version: 1, state: 'received' }, previous)) fail()
    const scope = { accountId: manifest.account_id, canonicalUserId: manifest.canonical_user_id, deviceId: manifest.device_id }
    const { candidate, group } = contentConflictHistoryJoin(scope, row, replayOrdinaryHistory(after.noteHistory ?? []))
    const old = before.noteHistory ?? [], next = replayOrdinaryHistory(after.noteHistory ?? [])
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
  const closed = before.noteHistory?.find(r => r.record_kind === 'resolution')
  if (closed?.record_kind === 'resolution' && row?.state === 'content_note_proven' && sequence > closed.server_sequence) {
    const proof = row.content_note_proof
    const { content_note_proof: _proof, ...wire } = row
    const old = before.noteHistory ?? [], next = after.noteHistory ?? []
    const candidate = next.find(r => r.record_kind === 'candidate' && r.event_id === row.event_id)
    if (status !== 'applied' || previous?.state !== 'received' || manifest.contract_version !== 6
      || sequence !== manifest.applied_cursor! + 1 || !replaySame(after.manifest, replayCoverage(manifest, row))
      || !replaySame({ ...wire, record_version: 1, state: 'received' }, previous)
      || !replaySame(before.rows.filter(r => r.server_sequence !== sequence), after.rows.filter(r => r.server_sequence !== sequence))
      || !replaySame(before.bindings, after.bindings) || !replaySame(before.owners, after.owners)
      || proof.header.parent_event_id !== before.rows[sequence - 2]?.event_id || !['update', 'delete'].includes(proof.mutation)
      || !candidate || candidate.record_kind !== 'candidate' || candidate.record_digest !== proof.candidate_digest
      || next.length !== old.length + 1
      || old.filter(r => !(r.record_kind === 'head' && r.project_id === closed.project_id && r.note_id === closed.note_id)).some(r => !next.some(v => replaySame(v, r)))) fail()
    const source = before.sources?.find(r => r.project_id === closed.project_id), updated = after.sources?.find(r => r.project_id === closed.project_id)
    if (!source || !updated || updated.source_revision !== source.source_revision + 1
      || !replaySame(before.sources?.filter(r => r !== source), after.sources?.filter(r => r !== updated))
      || !replaySame(before.notes?.filter(r => r.note_id !== closed.note_id), after.notes?.filter(r => r.note_id !== closed.note_id))) fail()
    return
  }
  if (row?.state === 'note_proven' || row?.state === 'content_note_proven') {
    if (row.state === 'content_note_proven') {
      if (row.content_note_proof.mutation !== 'update' && !(manifest.contract_version === 6 && row.content_note_proof.mutation === 'create')) fail()
      contentHistoryJoin({ accountId: manifest.account_id, canonicalUserId: manifest.canonical_user_id, deviceId: manifest.device_id }, row, replayOrdinaryHistory(after.noteHistory ?? []))
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
  const bound = range(s.accountId), rows: FutureInbox[] = []
  const identity = tx.objectStore('identities').get(s.canonicalUserId), alias = tx.objectStore('identities').index('local_account_id').get(s.accountId)
  const binding = tx.objectStore('accountBindings').get(s.accountId), cursors = tx.objectStore('cursors').getAll(bound)
  const manifests = tx.objectStore('replayManifest').getAll(bound), bindings = tx.objectStore('projectBindings').getAll(bound), owners = tx.objectStore('ownerHeads').getAll(bound)
  const projects = tx.objectStore('localProjects').getAll(bound)
  const localNotes = tx.objectStore('localNotes').getAll(bound), noteHistory = tx.objectStore('legacyNoteHistory').getAll(bound)
  const counts = replayEmptyStores.map(name => tx.objectStore(name).getAll(bound))
  const gameReads = ['gameLedger', 'gameProjection'].filter(name => tx.objectStoreNames.contains(name)).map(name => tx.objectStore(name).getAll(bound))
  const progressReads = ['progressLedger', 'progressProjection'].filter(name => tx.objectStoreNames.contains(name)).map(name => tx.objectStore(name).getAll(bound))
  const mapReads = ['mapLedger', 'mapProjection'].filter(name => tx.objectStoreNames.contains(name)).map(name => tx.objectStore(name).getAll(bound))
  const documentReads = ['documentLedger', 'documentProjection'].filter(name => tx.objectStoreNames.contains(name)).map(name => tx.objectStore(name).getAll(bound))
  let pending = 11 + counts.length + documentReads.length + mapReads.length + progressReads.length + gameReads.length, bytes = 0
  const finish = () => {
    try {
      check(); if (--pending) return
      const gameRows = rows.filter(row => row.state === 'game_proven')
      if (gameReads.length && (gameReads[0]!.result.length !== gameRows.length || gameReads[1]!.result.length !== new Set((gameReads[0]!.result as {project_id:string;entity_id:string}[]).map(row=>JSON.stringify([row.project_id,row.entity_id]))).size)) fail()
      if (!gameReads.length && gameRows.length) fail()
      for (const row of gameRows) {
        joinGameRecords(s, row, gameReads[0]!.result, gameReads[1]!.result)
      }
      const progressRows = rows.filter(row => row.state === 'progress_proven')
      if (progressReads.length && (progressReads[0]!.result.length !== progressRows.length || progressReads[1]!.result.length !== new Set(progressRows.map(row=>{const e=JSON.parse(row.wire_item).event;return JSON.stringify([e.project_id,e.entity_id])})).size)) fail()
      if (!progressReads.length && progressRows.length) fail()
      for (const row of progressRows) {
        joinProgressRecords(s, row, progressReads[0]!.result, progressReads[1]!.result)
      }
      const mapRows = rows.filter(row => row.state === 'map_proven')
      if (mapReads.length && (mapReads[0]!.result.length !== mapRows.length || mapReads[1]!.result.length !== new Set(mapRows.map(row=>{const e=JSON.parse(row.wire_item).event;return JSON.stringify([e.project_id,e.entity_id])})).size)) fail()
      if (!mapReads.length && mapRows.length) fail()
      for (const row of mapRows) {
        joinMapRecords(s, row, mapReads[0]!.result, mapReads[1]!.result)
      }
      const documentRows = rows.filter(row => row.state === 'document_proven')
      if (documentReads.length && (documentReads[0]!.result.length !== documentRows.length || documentReads[1]!.result.length !== new Set(documentRows.map(row=>{const e=JSON.parse(row.wire_item).event;return JSON.stringify([e.project_id,e.entity_id])})).size)) fail()
      if (!documentReads.length && documentRows.length) fail()
      for (const row of documentRows) {
        joinDocumentRecords(s, row, documentReads[0]!.result, documentReads[1]!.result)
      }
      if (version<6&&counts.some(request => request.result.length !== 0) || version < 4 && projects.result.length || cursors.result.length > 1 || manifests.result.length > 1
        || bindings.result.length > 200 || owners.result.length > 200 || version === 1 && (bindings.result.length || owners.result.length)) fail()
      const i: unknown = identity.result, b: unknown = binding.result
      if (i === undefined ? b !== undefined || alias.result !== undefined : !replayExact(i, ['record_version', 'canonical_user_id', 'local_account_id', 'device_id'])
        || i.record_version !== 1 || i.canonical_user_id !== s.canonicalUserId || i.local_account_id !== s.accountId || i.device_id !== s.deviceId
        || !replaySame(alias.result, i) || !replayExact(b, ['record_version', 'local_account_id', 'canonical_user_id'])
        || b.record_version !== 1 || b.local_account_id !== s.accountId || b.canonical_user_id !== s.canonicalUserId) fail()
      const manifest = manifests.result.length ? validateReplayManifest(manifests.result[0], s, version) : null
      const checkpoint = validateBrowserSyncCursor(cursors.result[0], s), metadata = manifest?.metadata_cursor ?? 0, applied = version >= 3 ? manifest?.applied_cursor ?? 0 : metadata
      if (checkpoint.ack_cursor !== 0 || (version < 6 ? checkpoint.proven_prefix !== 0 : checkpoint.proven_prefix > applied) || checkpoint.next_local_ordinal !== 1+counts[0]!.result.length+counts[2]!.result.length
        || checkpoint.pull_cursor !== rows.length || !manifest && (rows.length || cursors.result.length || bindings.result.length || owners.result.length || projects.result.length)
        || manifest && (i === undefined || !cursors.result.length || manifest.receive_cursor !== rows.length)) fail()
      const imported = bindings.result.map(raw => validateBrowserProjectBinding(raw, s)), heads = owners.result.map(raw => validateBrowserOwnerHead(raw, s))
      const sources = projects.result.map(raw => validateLocalProjectRecord(raw, s))
      const notes = localNotes.result.map(raw => validateLocalNoteRecord(raw, s, raw.project_id))
      const future = noteHistory.result.map(raw => readFutureHistoryRecord(raw, s))
      const resolutions = future.filter(r => r.record_kind === 'resolution').sort((a,b)=>a.server_sequence-b.server_sequence)
      const transitions = future.filter(r => r.record_kind === 'group_transition')
      if (resolutions.length !== transitions.length || version < 6 && resolutions.length) fail()
      const closed = resolutions[0]
      for (const resolution of resolutions) {
        const transition=transitions.find(t=>t.resolution_event_id===resolution.event_id)
        const row=rows.find(r=>r.server_sequence===resolution.server_sequence)
        if(resolution.strategy!=='choose_version'||resolution.transport_kind!=='content_note_resolution_v2'||resolution.retained_note_id!==null
          ||resolution.project_id!==closed!.project_id||resolution.note_id!==closed!.note_id
          ||!transition||row?.state!=='note_resolution_proven'||row.event_id!==resolution.event_id
          ||row.resolution_proof.resolution_digest!==resolution.record_digest||row.resolution_proof.transition_digest!==transition.record_digest)fail()
      }
      inspectResolutionDag(s, future)
      const authorActive=counts.some(r=>r.result.length),author={out:counts[0]!.result,receipts:counts[1]!.result,intents:counts[2]!.result}
      const authority=inspectContentAuthorityInventory(s,{...author,history:future,rows,bindings:imported,localNotes:notes,cursor:checkpoint})
      const authored=authority.candidates.sort((a,b)=>a.header.revision-b.header.revision),authorMutations=authored.length+authority.intents.filter(i=>i.record_version===2).length
      const resolutionsOut=authority.out.filter(o=>!authored.some(c=>c.event_id===o.event_id))
      for(const o of resolutionsOut){const e=JSON.parse(o.raw_body).items[0].event,root=resolutions.find(r=>r.event_id===o.event_id),group=future.find(g=>g.record_kind==='group'&&g.project_id===e.project_id&&g.note_id===e.entity_id&&e.revision===g.tip_revision+1)
        if(o.protocol_version!==3||e.entity_type!=='note'||e.operation!=='event'||!group||!root&&future.some(t=>t.record_kind==='group_transition'&&t.group_id===(group as any).group_id)||root&&root.source_device_id!==s.deviceId)fail()
        const row=rows.find(r=>r.event_id===o.event_id)
        if(row){const w=JSON.parse(row.wire_item),{device_id,server_sequence,...event}=w.event,receipt=author.receipts.find((r:any)=>r.event_id===o.event_id) as any;if(device_id!==s.deviceId||!receipt||receipt.record_version!==1||receipt.server_sequence!==server_sequence||!replaySame({event,object:w.object},JSON.parse(o.raw_body).items[0]))fail()}
      }
      if(authorActive&&(version!==6||authority.out.length!==authored.length+resolutionsOut.length||!closed&&(authored.length||author.intents.length)||authority.intents.some(i=>i.record_version!==2)||authored.some(c=>!closed||c.header.revision<=closed.header.revision||c.project_id!==closed.project_id||c.note_id!==closed.note_id||c.bootstrap_id!==closed.bootstrap_id)))fail()


      const successors = closed ? rows.filter(r => r.server_sequence > closed.server_sequence) : []
      for (const successor of successors) {
        const e = JSON.parse(successor.wire_item).event
        if (e.entity_type !== 'note' || e.operation !== 'event' || e.revision <= closed!.header.revision
          || e.project_id !== closed!.project_id || e.entity_id !== closed!.note_id || e.device_id === s.deviceId&&!authored.some(c=>c.event_id===successor.event_id)
          || !['received', 'content_note_proven', 'content_note_conflict_preserved', 'note_resolution_proven','content_note_self_echo_proven'].includes(successor.state)) fail()
      }
      for(const row of successors)if(JSON.parse(row.wire_item).event.device_id===s.deviceId){
        const c=authored.find(c=>c.event_id===row.event_id),r=authority.receipts.find(r=>r.event_id===row.event_id),w=JSON.parse(row.wire_item),{device_id:_device,server_sequence,...event}=w.event
        if(!c||!r||r.server_sequence!==server_sequence||server_sequence!==row.server_sequence||!replaySame({event,object:w.object},JSON.parse(c.wire_item)))fail()
      }
      const appliedSuccessors = successors.filter(r => r.state === 'content_note_proven')
      const successorCandidates=successors.filter(r=>r.state==='content_note_proven'||r.state==='content_note_conflict_preserved')
      const latestMaterial=rows.filter(r=>r.state==='content_note_proven'||r.state==='note_resolution_proven').at(-1)
      const history = replayOrdinaryHistory(future)
      if (closed) {
        const head = future.find(r => r.record_kind === 'head' && r.record_version === 2)
        const row = rows.find(r => r.server_sequence === closed.server_sequence)
        if (!head || head.record_kind !== 'head' || head.head_revision !== 3 + resolutions.length + successorCandidates.length+authored.length || head.linear_head_event_id !== (authored.at(-1)?.event_id??latestMaterial?.event_id)
          || row?.state !== 'note_resolution_proven' || row.event_id !== closed.event_id
          || row.resolution_proof.resolution_digest !== closed.record_digest
          || rows.filter(r => JSON.parse(r.wire_item).event.entity_type === 'note').length !== 4 + successors.length) fail()
      }
      if (future.length !== history.length + (closed ? 2 * resolutions.length + successorCandidates.length+authored.length + future.filter(r=>r.record_kind==='group'&&r.group_id!==closed.local_group_id).length : 0)) fail()
      validateNoteHistorySet(s, history)
      const noteProofs = rows.filter(row => (!closed || row.server_sequence < closed.server_sequence) && (row.state === 'note_proven' || row.state === 'content_note_proven'))
      const conflicts = rows.filter(row => (!closed||row.server_sequence<closed.server_sequence)&&row.state === 'content_note_conflict_preserved')
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
        if (source.source_revision !== appliedNotes.length + 1 + (closed?.project_id === source.project_id ? resolutions.length + appliedSuccessors.length+authorMutations : 0) || appliedNotes.some((row, index) => replayNoteOutcome(row).source_revision !== index + 2)) fail()
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
        if (row.server_sequence !== index + 1 || ids.has(row.event_id) || (row.server_sequence <= applied) !== (!!replayAppliedProof(row) || row.state === 'note_proven' || row.state === 'content_note_proven' || row.state === 'content_note_conflict_preserved' || row.state === 'note_resolution_proven'||row.state==='content_note_self_echo_proven'||row.state==='document_proven'||row.state==='map_proven'||row.state==='progress_proven'||row.state==='game_proven')) fail()
        ids.add(row.event_id)
        const wire = JSON.parse(row.wire_item) as { event: { project_id: string }; object: { ciphertext: string } }
        ciphertextBytes += Math.floor(wire.object.ciphertext.length * 3 / 4)
        const proof = replayAppliedProof(row)
        if(row.state==='content_note_self_echo_proven'){validateContentSelfEchoProof2(row.content_note_self_echo_proof,s);if(!authored.some(c=>c.event_id===row.event_id))fail()}
        if (closed && row.server_sequence > closed.server_sequence) {
          if (row.state === 'content_note_proven'||row.state==='content_note_conflict_preserved') {
            const candidate = future.find(r => r.record_kind === 'candidate' && r.event_id === row.event_id)
            if (!candidate || candidate.record_kind !== 'candidate' || candidate.record_version !== 2
              || candidate.header.revision !== JSON.parse(row.wire_item).event.revision
              || !['update', 'delete'].includes((row.state==='content_note_proven'?row.content_note_proof:row.content_note_conflict_proof).mutation) || candidate.wire_item !== row.wire_item
              || candidate.record_digest !== (row.state==='content_note_proven'?row.content_note_proof:row.content_note_conflict_proof).candidate_digest
              || (latestMaterial === row && !authorMutations && !replaySame(candidate.projection, notes.find(n => n.note_id === closed.note_id)))) fail()
          }
          continue // The continuation owner authenticates these proofs and sources.
        }
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
            || row.state === 'content_note_proven' && !contentGenesis && (p.mutation !== 'update' || !previous || !(previous.state === 'note_proven' && previous.note_proof.mutation !== 'delete' || version >= 6 && previous.state === 'content_note_proven' && ['create', 'update'].includes(previous.content_note_proof.mutation)))
            || candidate.projection.source_revision !== p.source_revision || candidate.projection.deleted !== (p.mutation === 'delete')
            || !replaySame(p.reference, retained.event)) fail()
          if (previous?.state === 'content_note_proven' && !(version >= 6 && ['create', 'update'].includes(previous.content_note_proof.mutation))) fail()
          if (isLatest && !closed && !replaySame(candidate.projection, note)) fail()
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
        const seed = manifest?.source_seeds?.find(value => value.project_id === record.project_id && value.bootstrap_id === record.bootstrap_id)
        const genesis = seed && rows.find(value => value.event_id === seed.genesis_event_id)
        // Preserve the exact original import boundary with its existing Content
        // owner outcome. The owner's complete history is reauthenticated before
        // any project is displayed, including its causal genesis and parents.
        // Import high-water is account-wide. A proven account Game outcome can
        // be its boundary; seed/bootstrap still identify this project. The Game
        // owner reauthenticates the exact account source/ledger before use/UI.
        const contentBoundary = rows.find(value => value.server_sequence === record.remote_high_water && (value.state === 'content_note_proven' && value.content_note_proof.header.project_id === record.project_id || value.state === 'applied' && value.applied_proof.project_id === record.project_id || (value.state === 'document_proven'||value.state === 'map_proven'||value.state === 'progress_proven'||value.state === 'game_proven') && JSON.parse(value.wire_item).event.project_id === record.project_id || value.state === 'game_proven' && JSON.parse(value.wire_item).event.scope === 'account' && JSON.parse(value.wire_item).event.entity_type === 'account_game' && gameReads.length === 2 && gameReads[0]!.result.some((ledger: {event_id:string;project_id:string;entity_id:string}) => ledger.event_id === value.event_id && ledger.project_id === 'account' && ledger.entity_id === 'account')))
        const retainedImport = manifest?.contract_version === 6 && seed && genesis && contentBoundary && record.remote_high_water !== null
          && contentBoundary.server_sequence === record.remote_high_water
          && genesis.server_sequence <= record.remote_high_water && seed.initial_max_server_sequence <= record.remote_high_water
          && record.remote_high_water <= (manifest.applied_cursor ?? 0) && record.remote_high_water < manifest.anchor.registry.current_cursor
        if (!remote || remote.bootstrap_id !== record.bootstrap_id || record.mode !== 'import_remote' || record.phase !== 'captured' || record.remote_state !== 'active' || record.remote_high_water!==manifest!.anchor.registry.current_cursor&&!retainedImport&&(!authorActive||record.remote_high_water===null||record.remote_high_water<(resolutionsOut.length?((closed?.server_sequence??rows.find(r=>resolutionsOut.some(o=>o.event_id===r.event_id))?.server_sequence??manifest!.anchor.registry.current_cursor)-1):(closed?.server_sequence??manifest!.anchor.registry.current_cursor))||record.remote_high_water>manifest!.anchor.registry.current_cursor)
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
      bytes += new TextEncoder().encode(JSON.stringify({ manifest, imported, heads, sources, notes, history: future,author,documents:documentReads.map(request=>request.result),games:gameReads.map(request=>request.result),progress:progressReads.map(request=>request.result),maps:mapReads.map(request=>request.result) })).length + REPLAY_LIMITS.controlReserveBytes
      if (manifest && (ciphertextBytes !== manifest.ciphertext_bytes || bytes > REPLAY_LIMITS.inventoryBytes)) fail()
      result({ manifest, rows, bindings: imported, owners: heads, sources, notes, noteHistory: future,...(gameReads.length ? { games: { ledger: gameReads[0]!.result, projections: gameReads[1]!.result } } : {}),...(progressReads.length ? { progress: { ledger: progressReads[0]!.result, projections: progressReads[1]!.result } } : {}),...(mapReads.length ? { maps: { ledger: mapReads[0]!.result, projections: mapReads[1]!.result } } : {}),...(documentReads.length ? { documents: { ledger: documentReads[0]!.result, projections: documentReads[1]!.result } } : {}),...(manifest?{checkpoint}:{}),...(authorActive?{author}:{}) })
    } catch (error) { abort(error) }
  }
  for (const request of [identity, alias, binding, cursors, manifests, bindings, owners, projects, localNotes, noteHistory, ...counts, ...documentReads, ...mapReads, ...progressReads, ...gameReads]) request.onsuccess = finish
  const scan = tx.objectStore('inbox').openCursor(bound)
  scan.onsuccess = () => {
    try {
      check(); const cursor = scan.result
      if (!cursor) { finish(); return }
      if (rows.length >= REPLAY_LIMITS.events) fail()
      const row = readFutureInboxRecord(cursor.value, s)
      if (!(row.state === 'received' && row.record_version === 1 || version >= 2 && row.state === 'applied' && row.record_version === 2 || version >= 3 && (row.state === 'stage_proven' && row.record_version === 5 || row.state === 'stage_order_proven' && row.record_version === 6) || version >= 4 && row.state === 'note_proven' && row.record_version === 3 || version >= 5 && row.state === 'content_note_proven' && row.record_version === 7 || version >= 6 && row.state === 'content_note_conflict_preserved' && row.record_version === 8)
        && !(documentReads.length === 2 && row.state === 'document_proven')
        && !(gameReads.length === 2 && row.state === 'game_proven')
        && !(progressReads.length === 2 && row.state === 'progress_proven')
        && !(mapReads.length === 2 && row.state === 'map_proven')
        && !(version >= 6 && (row.state === 'note_resolution_proven' && row.record_version === 10||row.state==='content_note_self_echo_proven'&&row.record_version===9))
        || row.protocol_version !== 3 || !replaySame(cursor.primaryKey, [s.accountId, row.server_sequence])) fail()
      bytes += new TextEncoder().encode(JSON.stringify(row)).length
      if (bytes > REPLAY_LIMITS.inventoryBytes) fail()
      rows.push(row); cursor.continue()
    } catch (error) { abort(error) }
  }
}

/** Versionchange has no HTTP/crypto and never invents bindings, heads or applied outcomes. */
export function migrateReplay(tx: IDBTransaction, names: readonly string[], check: () => void, abort: (error: unknown) => void, version: 1 | 2 | 3 | 4 | 5 | 6): void {
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
          if (snapshot.manifest && version < 6) updates.push({ ...snapshot.manifest, record_version: 6, contract_version: 6, metadata_cursor: snapshot.manifest.metadata_cursor ?? 0,
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

/** Historical ordinary proofs remain bound to the immutable pre-closure group.
 * This view is only for ordinary proof verification, never persisted or granted
 * Resolution authority; closed outputs require authenticateResolutionHistory.
 */
export function replayOrdinaryHistory(values: FutureNoteHistory[]): BrowserNoteHistoryRecord[] {
  const roots=values.filter(r=>r.record_kind==='resolution').sort((a,b)=>a.server_sequence-b.server_sequence)
  const root = roots[0]
  const descendants = values.filter(r => r.record_kind === 'candidate' && root?.record_kind === 'resolution' && r.header.revision > root.header.revision)
  const ordinary = values.filter((r): r is BrowserNoteHistoryRecord => r.record_kind !== 'resolution' && r.record_kind !== 'group_transition' && !(r.record_kind==='group'&&root&&r.group_id!==root.local_group_id) && !descendants.includes(r) && !(r.record_kind === 'head' && r.record_version === 2))
  for (const head of values) if (head.record_kind === 'head' && head.record_version === 2) {
    const group = ordinary.find(r => r.record_kind === 'group' && r.project_id === head.project_id && r.note_id === head.note_id)
    const winner = group?.record_kind === 'group' && group.members.find(m => m.source === 'remote_applied')
    if (!group || group.record_kind !== 'group' || !winner) fail()
    const { node_ref: _ref, ...identity } = head
    ordinary.push({ ...identity, record_id: 'head', record_version: 1, head_revision: head.head_revision - roots.length - descendants.length,
      linear_head_event_id: winner.event_id, tip_event_ids: group.tip_event_ids })
  }
  return ordinary
}

/** Local author lifecycle only; no receive/apply coverage or project-read grant. */
export function assertReplayAuthorTransition(before:ReplaySnapshot,after:ReplaySnapshot):void {
  if(!before.manifest||!before.checkpoint||!after.checkpoint||!replaySame({...after,checkpoint:before.checkpoint,manifest:before.manifest,author:before.author,sources:before.sources,notes:before.notes,noteHistory:before.noteHistory},{...before,author:before.author}))fail()
  if(!replaySame(after.manifest,{...before.manifest,revision:before.manifest.revision+1}))fail()
  const a=before.author??{out:[],receipts:[],intents:[]},b=after.author??{out:[],receipts:[],intents:[]}
  const ordinalDelta=b.out.length+b.intents.length-a.out.length-a.intents.length
  if(!replaySame(after.checkpoint,{...before.checkpoint,next_local_ordinal:before.checkpoint.next_local_ordinal+ordinalDelta}))fail()
  const old=before.noteHistory??[],next=after.noteHistory??[]
  if(b.intents.length===a.intents.length+1){
    const i=b.intents.find(i=>!a.intents.some(v=>replaySame(v,i))) as import('./contentAuthorityRecords').BrowserContentIntent2|undefined
    const source=before.sources?.find(p=>p.project_id===i?.project_id),updated=after.sources?.find(p=>p.project_id===i?.project_id)
    if(!i||i.record_version!==2||i.mutation_generation!==1||!replaySame(a.out,b.out)||!replaySame(a.receipts,b.receipts)||!replaySame(old,next)
      ||!source||!updated||updated.source_revision!==source.source_revision+1||!replaySame(after.notes?.find(n=>n.note_id===i.note_id),i.snapshot)
      ||!replaySame(before.sources?.filter(n=>n!==source),after.sources?.filter(n=>n!==updated))
      ||!replaySame(before.notes?.filter(n=>n.note_id!==i.note_id),after.notes?.filter(n=>n.note_id!==i.note_id)))fail()
  }else if(b.out.length===a.out.length+1){
    const candidates=next.filter(r=>r.record_kind==='candidate'&&r.record_version===3&&!old.some(v=>replaySame(v,r)))
    if(!candidates.length){
      if(!replaySame(old,next)||!replaySame(a.receipts,b.receipts)||!replaySame(a.intents,b.intents)||!replaySame(before.sources,after.sources)||!replaySame(before.notes,after.notes)||a.out.some(o=>!b.out.some(v=>replaySame(o,v))))fail()
      return
    }
    const c=candidates[0],head=old.find(r=>r.record_kind==='head'&&r.project_id===c?.project_id&&r.note_id===c?.note_id),h=next.find(r=>r.record_kind==='head'&&r.project_id===c?.project_id&&r.note_id===c?.note_id)
    if(candidates.length!==1||!c||c.record_kind!=='candidate'||c.record_version!==3||!head||head.record_kind!=='head'||!h||h.record_kind!=='head'||head.record_version!==2||h.record_version!==2
      ||!replaySame(h,{...head,head_revision:head.head_revision+1,linear_head_event_id:c.event_id,tip_event_ids:[c.event_id],node_ref:{kind:'candidate',event_id:c.event_id}})
      ||next.length!==old.length+1||old.filter(r=>r!==head).some(r=>!next.some(v=>replaySame(v,r)))
      ||!replaySame(b.intents,a.intents.filter((i:any)=>i.event_id!==c.event_id))||!a.intents.some(i=>replaySame(i,c.intent))||!replaySame(a.receipts,b.receipts)
      ||a.out.some(o=>!b.out.some(v=>replaySame(o,v)))||!replaySame(before.notes,after.notes)||!replaySame(before.sources,after.sources))fail()
  }else {
    if(!replaySame(a.intents,b.intents)||!replaySame(old,next)||!replaySame(before.sources,after.sources)||!replaySame(before.notes,after.notes)
      ||b.receipts.length!==a.receipts.length+1||a.receipts.some(r=>!b.receipts.some(v=>replaySame(r,v)))||a.out.length!==b.out.length)fail()
    const r=b.receipts.find(r=>!a.receipts.some(v=>replaySame(r,v))) as {event_id:string}
    if(!a.out.some((o:any)=>o.event_id===r.event_id&&o.state==='sealed')||!replaySame(b.out,a.out.map((o:any)=>o.event_id===r.event_id?{...o,state:'accepted'}:o)))fail()
  }
}
