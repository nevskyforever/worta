import type { ProjectGameEvent } from '@/cloud/gameCodec'
import type { ProgressEvent } from '@/cloud/progressCodec'
import type { AccountMasterKey } from '@/crypto'
import type { MapEvent } from '@/cloud/mapCodec'
import type { DocumentEvent } from '@/cloud/documentCodec'
import type { ContentNoteEvent } from '@/cloud/contentNoteCodec'
import { BrowserPersistenceError } from './persistence'
import { prepareStagePrefix,stageSequences } from './stageApply'
import type { VerifiedMetadataPrefix } from './metadataPrefixProof'
import { proveStageReferenceCoverage } from './stageOrderRecords'
import { validateStageHead } from './stageRecords'
import { validateBrowserInboxRecord,type BrowserSyncScope,type BrowserInboxRecord } from './syncDurability'
import type { BrowserPrefixInventory } from './provenPrefixRecords'
/** Native content_reference_ready: one authenticated live projection/tip, bounded actual parent coverage. No Stage-order dependency. */
export async function requireContentStageDependency(scope:BrowserSyncScope,row:Pick<BrowserInboxRecord,'server_sequence'>,content:ContentNoteEvent|DocumentEvent|MapEvent|ProgressEvent|ProjectGameEvent,v:BrowserPrefixInventory,amk:AccountMasterKey,current:()=>boolean,metadata:VerifiedMetadataPrefix):Promise<void>{
 const isMap='header' in content&&('map' in content||'entries' in content||'action' in content)
 const document='header' in content
 if(!document&&content.event.version!==1)throw new BrowserPersistenceError('unsupported_local_source')
 const note=isMap?content.header:document?content.document:content.event.version===1?content.event.note:null,refs=document?content.header.stage_event_ids:content.dependencies.stage_event_ids
 if(!note)throw new BrowserPersistenceError('stage_dependency_missing')
 if(note.stage_id===null){if(refs.length)throw new BrowserPersistenceError('stage_dependency_missing');return}
 const raw=v.owners.find(r=>(r as {owner_kind:string;project_id:string;stage_id:string}).owner_kind==='stage'&&(r as {project_id:string}).project_id===note.project_id&&(r as {stage_id:string}).stage_id===note.stage_id)
 if(!raw)throw new BrowserPersistenceError('stage_dependency_missing')
 const head=validateStageHead(raw,scope)
 if(head.bootstrap_id!==(document?content.header.bootstrap_id:content.dependencies.bootstrap_id))throw new BrowserPersistenceError('stage_dependency_missing')
 const certificate=await prepareStagePrefix(scope,v,amk,current,metadata,true),safe=stageSequences(certificate,scope,v)
 if(head.history.some(p=>!safe.has(p.server_sequence))||!proveStageReferenceCoverage(head.tip_event_ids,refs,head.history))throw new BrowserPersistenceError('stage_dependency_missing')
 // Earlier retained unresolved Stage outcomes cannot authorize a newer Content event.
 // Later received Stage events remain their own barriers; safe applied advancement is covered above.
 for(const rawRow of v.rows){const r=validateBrowserInboxRecord(rawRow,scope),e=JSON.parse(r.wire_item).event
  if(r.server_sequence<row.server_sequence&&e.entity_type==='stage'&&e.project_id===note.project_id&&e.entity_id===note.stage_id&&(!safe.has(r.server_sequence)||r.state!=='stage_proven'))throw new BrowserPersistenceError(e.operation==='delete'?'stage_tombstone_child_manifest_incomplete':'stage_dependency_missing')
 }
 if(!current())throw new BrowserPersistenceError('closed')
}
