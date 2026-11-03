// @vitest-environment node
import {it,expect} from 'vitest'
import {continuationFixture} from './resolutionContinuation.testFixture'
import {BrowserContentNoteIntents} from './contentNoteIntents'
import {BrowserContentNoteApply} from './contentNoteApply'
import {BrowserLegacyNoteApply} from './legacyNoteApply'
import {scope,at} from './bootstrap.testFixture'
async function setup(format:'plain'|'html'='plain'){
 const f=await continuationFixture('choose_version',false,{format}),base=f.f
 const writer=new BrowserContentNoteIntents(base.transaction,base.localOwner,()=>true).bind(base.auth,base.bindings as never,base.keys as never)
 const content=new BrowserContentNoteApply(new BrowserLegacyNoteApply(base.transaction,base.localOwner,()=>true)).bind(base.auth,base.bindings as never,base.keys as never)
 const publish=async(deleted=false)=>{
  const token=(await writer.local.readSnapshot(scope,'p')).token
  if(deleted)await writer.local.tombstoneNote(scope,token,'content-note',at)
  else await writer.local.updateNote(scope,token,'content-note',{content:'AUTHOR_AFTER_RESOLUTION'},at)
  const i=(await writer.list(scope,'p'))[0]!,old=structuredClone(base.db)
  await writer.authority.seal(scope,i.event_id)
  const out=base.db.outbox.at(-1) as any,w=JSON.parse(out.raw_body).items[0],sequence=(await base.sync.readCheckpoint(scope)).pull_cursor+1
  await writer.authority.accept(scope,i.event_id,{protocol_version:3,encrypted_sync_version:3,current_cursor:sequence,results:[{event_id:i.event_id,server_sequence:sequence,duplicate:false}]})
  await base.sync.commitInboundPage(scope,sequence-1,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[{event:{...w.event,device_id:scope.deviceId,server_sequence:sequence},object:w.object}],next_cursor:sequence,has_more:false}))
  const source=structuredClone(base.db.localNotes)
  expect(await content.apply(scope,sequence)).toBe('self_echo_applied');expect(base.db.localNotes).toEqual(source)
  const accepted=structuredClone(base.db);expect(await content.apply(scope,sequence)).toBe('already_applied');expect(base.db).toEqual(accepted)
  await writer.authority.authenticate(scope)
  return {i,out,sequence,old}
 }
 return{...f,writer,content,publish}
}
it.each(['plain','html'] as const)('repeating genuine author updates after Resolution2 (%s), then delete, without source reapply',async format=>{
 const f=await setup(format);let parent=f.resolution.header.event_id
 for(let n=0;n<3;n++){const {i}=await f.publish();expect(i.parent_event_id).toBe(parent);parent=i.event_id}
 const {i}=await f.publish(true);expect(i.parent_event_id).toBe(parent);expect(i.operation).toBe('delete')
 const old=structuredClone(f.f.db);await expect(f.writer.local.updateNote(scope,(await f.writer.local.readSnapshot(scope,'p')).token,'content-note',{content:'RESURRECTION'},at)).rejects.toThrow();expect(f.f.db).toEqual(old)
})
it('author handover requires closed conflict, authenticated source and accepted prior echo',async()=>{
 const f=await setup();const source=await f.writer.local.readSnapshot(scope,'p');await f.writer.local.updateNote(scope,source.token,'content-note',{content:'PENDING'},at)
 const i=(await f.writer.list(scope,'p'))[0]!;await f.writer.authority.seal(scope,i.event_id)
 const old=structuredClone(f.f.db);await expect(f.writer.local.updateNote(scope,(await f.writer.local.readSnapshot(scope,'p')).token,'content-note',{content:'UNPUBLISHED_CHILD'},at)).rejects.toThrow();expect(f.f.db).toEqual(old)
})

it('a retained pending peer event cannot be skipped to create an author branch',async()=>{const f=await setup();await f.receive({content:'PENDING_PEER'});const source=await f.writer.local.readSnapshot(scope,'p'),old=structuredClone(f.f.db);await expect(f.writer.local.updateNote(scope,source.token,'content-note',{content:'SKIPPED_PEER'},at)).rejects.toMatchObject({code:'note_conflict_unproven'});expect(f.f.db).toEqual(old)})
