// @vitest-environment node
import { it, expect } from 'vitest'
import { contentWriterSetup, sourceToken, fresh, update, scope } from './contentNoteIntents.testFixture'
import { BrowserContentAuthorityOwner } from './contentAuthorityOwner'
import { authenticateContentCandidate3, authenticateContentReceipt2 } from './contentAuthorityAuthentication'
import { amk } from './bootstrap.testFixture'
const setup = async (peer = true) => {
 const f = await contentWriterSetup(peer)
 if (peer) await update(f); else await f.cw.local.createNote(scope, await sourceToken(f), fresh())
 const i = (await f.cw.list(scope, 'p'))[0]!
 const owner = f.cw.authority
 await owner.seal(scope, i.event_id)
 const out = f.db.outbox[0] as any, wire = JSON.parse(out.raw_body).items[0], sequence = peer ? 3 : 2
 const response = { protocol_version:3, encrypted_sync_version:3, current_cursor:sequence, results:[{event_id:i.event_id,server_sequence:sequence,duplicate:false}] }
 const receive = () => f.sync.commitInboundPage(scope, sequence - 1, JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[{event:{...wire.event,device_id:scope.deviceId,server_sequence:sequence},object:wire.object}],next_cursor:sequence,has_more:false}))
 return { ...f, i, owner, out, wire, sequence, response, receive }
}
it.each([false,true])('real mutation → seal → receipt → exact Content self echo peer=%s', async peer => {
 const f = await setup(peer), source = structuredClone(f.db.localNotes), history = structuredClone(f.db.legacyNoteHistory)
 const candidate = history!.find((r:any)=>r.event_id===f.i.event_id) as any
 await authenticateContentCandidate3(scope,candidate,amk,()=>true)
 expect(await f.owner.seal(scope,f.i.event_id)).toBe('already_sealed')
 expect(await f.owner.accept(scope,f.i.event_id,f.response)).toBe('accepted')
 await authenticateContentReceipt2(scope,f.db.receipts[0],candidate,f.db.outbox[0] as any,amk,()=>true)
 await f.receive(); expect(await f.content.apply(scope,f.sequence)).toBe('self_echo_applied')
 expect(f.db.inbox.at(-1)).toMatchObject({record_version:9,state:'content_note_self_echo_proven'})
 expect(f.db.localNotes).toEqual(source); expect(f.db.legacyNoteHistory).toEqual(history)
 const retained=structuredClone(f.db)
 expect(await f.content.apply(scope,f.sequence)).toBe('already_applied')
 expect(await f.owner.accept(scope,f.i.event_id,{...f.response,results:[{...f.response.results[0],duplicate:true}]})).toBe('already_accepted')
 const reopened=new BrowserContentAuthorityOwner(f.transaction,()=>true,f.auth,f.bindings as never,f.keys as never)
 await reopened.authenticate(scope); expect(f.db).toEqual(retained)
 expect((f.db.outbox[0] as any).raw_body).toBe(f.out.raw_body)
})
it('received own event without acceptance cannot be acknowledged',async()=>{const f=await setup();await f.receive();const old=structuredClone(f.db);await expect(f.content.apply(scope,f.sequence)).rejects.toMatchObject({code:'self_echo_unavailable'});expect(f.db).toEqual(old);await f.owner.accept(scope,f.i.event_id,f.response);expect(await f.content.apply(scope,f.sequence)).toBe('self_echo_applied')})
it.each(['ciphertext','candidate','receipt','echo','sequence','metadata','parent'] as const)('corrupt %s fails without changing source/history',async kind=>{
 const f=await setup();await f.owner.accept(scope,f.i.event_id,f.response);await f.receive();await f.content.apply(scope,f.sequence)
 const c=f.db.legacyNoteHistory!.find((r:any)=>r.event_id===f.i.event_id) as any,r=f.db.receipts[0] as any,row=f.db.inbox.at(-1) as any
 if(kind==='ciphertext'){const w=JSON.parse(row.wire_item);w.object.ciphertext='A'+w.object.ciphertext.slice(1);row.wire_item=JSON.stringify(w)}
 if(kind==='candidate')c.record_digest='0'.repeat(64)
 if(kind==='receipt')r.receipt_digest='0'.repeat(64)
 if(kind==='echo')row.content_note_self_echo_proof.outcome_digest='0'.repeat(64)
 if(kind==='sequence')r.server_sequence++
 if(kind==='metadata')f.db.ownerHeads=[]
 if(kind==='parent')c.header.parent_event_id=crypto.randomUUID()
 const old=structuredClone(f.db);await expect(f.content.apply(scope,f.sequence)).rejects.toThrow();expect(f.db).toEqual(old)
})
it.each(['seal','accept','echo'])('%s transaction abort preserves every store',async phase=>{
 const f=await contentWriterSetup(true);await update(f);const i=(await f.cw.list(scope,'p'))[0]!,owner=f.cw.authority
 if(phase!=='seal')await owner.seal(scope,i.event_id)
 const out=f.db.outbox[0] as any,response={protocol_version:3,encrypted_sync_version:3,current_cursor:3,results:[{event_id:i.event_id,server_sequence:3,duplicate:false}]}
 if(phase==='echo'){await owner.accept(scope,i.event_id,response);const w=JSON.parse(out.raw_body).items[0];await f.sync.commitInboundPage(scope,2,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[{event:{...w.event,device_id:scope.deviceId,server_sequence:3},object:w.object}],next_cursor:3,has_more:false}))}
 const old=structuredClone(f.db);f.abort();await expect(phase==='seal'?owner.seal(scope,i.event_id):phase==='accept'?owner.accept(scope,i.event_id,response):f.content.apply(scope,3)).rejects.toThrow();expect(f.db).toEqual(old);f.abort(false)
 if(phase==='seal')await owner.seal(scope,i.event_id);else if(phase==='accept')await owner.accept(scope,i.event_id,response);else expect(await f.content.apply(scope,3)).toBe('self_echo_applied')
})
it.each(['lock','logout','session'])('%s invalidation rejects prepared echo',async kind=>{const f=await setup();await f.owner.accept(scope,f.i.event_id,f.response);await f.receive();const p=await f.owner.prepareEcho(scope,f.sequence);const old=structuredClone(f.db);if(kind==='lock')f.lock();else if(kind==='logout')await f.auth.logout();else f.invalidate();await expect(f.owner.commitEcho(p!)).rejects.toThrow();expect(f.db).toEqual(old)})
it('wrong response event or sequence cannot replace receipt',async()=>{const f=await setup();const old=structuredClone(f.db);await expect(f.owner.accept(scope,f.i.event_id,{...f.response,results:[{...f.response.results[0],event_id:crypto.randomUUID()}]})).rejects.toThrow();expect(f.db).toEqual(old);await f.owner.accept(scope,f.i.event_id,f.response);const accepted=structuredClone(f.db);await expect(f.owner.accept(scope,f.i.event_id,{...f.response,results:[{...f.response.results[0],server_sequence:4}],current_cursor:4})).rejects.toThrow();expect(f.db).toEqual(accepted)})
it.each(['ciphertext','header','device','sequence'])('returned %s mismatch cannot acquire self-echo authority',async field=>{
 const f=await setup();await f.owner.accept(scope,f.i.event_id,f.response)
 const wire=structuredClone(f.wire),event={...wire.event,device_id:scope.deviceId,server_sequence:f.sequence}
 if(field==='ciphertext')wire.object.ciphertext=(wire.object.ciphertext[0]==='A'?'B':'A')+wire.object.ciphertext.slice(1)
 if(field==='header')event.revision++
 if(field==='device')event.device_id=crypto.randomUUID()
 if(field==='sequence')event.server_sequence++
 const old=structuredClone(f.db)
 await expect(f.sync.commitInboundPage(scope,f.sequence-1,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[{event,object:wire.object}],next_cursor:event.server_sequence,has_more:false}))).rejects.toThrow()
 expect(f.db).toEqual(old)
})
it('missing Metadata authority prevents sealing a genuine intent',async()=>{const f=await contentWriterSetup(true);await update(f);const i=(await f.cw.list(scope,'p'))[0]!;f.db.ownerHeads=[];const old=structuredClone(f.db);await expect(f.cw.authority.seal(scope,i.event_id)).rejects.toThrow();expect(f.db).toEqual(old)})
it('prepared echo cannot cross a source CAS change or replay through another owner',async()=>{const f=await setup();await f.owner.accept(scope,f.i.event_id,f.response);await f.receive();const p=await f.owner.prepareEcho(scope,f.sequence);const other=new BrowserContentAuthorityOwner(f.transaction,()=>true,f.auth,f.bindings as never,f.keys as never);await expect(other.commitEcho(p!)).rejects.toThrow();(f.db.localNotes![0] as any).source_revision++;const old=structuredClone(f.db);await expect(f.owner.commitEcho(p!)).rejects.toMatchObject({code:'stale_source'});expect(f.db).toEqual(old)})

it('missing own head cannot acknowledge an otherwise genuine echo',async()=>{const f=await setup();await f.owner.accept(scope,f.i.event_id,f.response);await f.receive();f.db.legacyNoteHistory=f.db.legacyNoteHistory!.filter((r:any)=>r.record_kind!=='head');const old=structuredClone(f.db);await expect(f.content.apply(scope,f.sequence)).rejects.toThrow();expect(f.db).toEqual(old)})
