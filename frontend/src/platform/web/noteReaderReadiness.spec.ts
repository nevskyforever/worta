// @vitest-environment node
import {afterEach,it,expect,vi} from 'vitest'
import {continuationFixture} from './resolutionContinuation.testFixture'
import {BrowserResolutionPrefix} from './resolutionPrefix'
import {BrowserNoteReaderReadiness} from './noteReaderReadiness'
import {scope} from './bootstrap.testFixture'
import {encryptedSyncV3Api} from '@/api/encryptedSyncV3'
import {encryptedSyncV2Api} from '@/api/encryptedSyncV2'
import {syncApi} from '@/api/sync'
import type {BrowserSyncTransaction} from './syncDurability'
afterEach(()=>vi.restoreAllMocks())
async function fixture(...args:Parameters<typeof continuationFixture>){
 const f=await continuationFixture(...args);let active=true
 const g=new BrowserResolutionPrefix(f.f.transaction,()=>active).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
 const create=(device=f.device,tx:BrowserSyncTransaction=f.f.transaction)=>new BrowserNoteReaderReadiness(tx,()=>active).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,device)
 return{...f,g,inspector:create(),create,close:()=>{active=false;f.close()}}
}
function noNetwork(){return [vi.spyOn(encryptedSyncV3Api,'noteReaderCapabilities'),vi.spyOn(encryptedSyncV3Api,'noteReaderGate'),vi.spyOn(encryptedSyncV3Api,'compressionReaderCapabilities'),vi.spyOn(globalThis,'fetch'),vi.spyOn(encryptedSyncV3Api,'ack'),vi.spyOn(encryptedSyncV2Api,'ack'),vi.spyOn(syncApi,'ack')]}
for(const legacy of [true,false])for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)it(`genuine F/H/G protocol${legacy?2:3} ${strategy} is evidence, never complete Note readiness`,async()=>{
 const f=await fixture(strategy,legacy),child=await f.receive({deleted:strategy==='delete',compressed:true});await f.runtime.apply(scope,child.sequence)
 if(strategy==='keep_both'){const retained=await f.receive({id:'retained-note',compressed:true});await f.runtime.apply(scope,retained.sequence)}
 const gToken=await f.g.prepare(scope);await f.g.commit(await f.g.prepareCommit(gToken))
 const spies=noNetwork(),before=f.before(),d=await f.inspector.inspect(scope)
 expect(d).toMatchObject({readOnly:true,ready:false,declarationAllowed:false,writerAuthorized:false,localEvidence:'authenticated_bounded_inventory',serverGate:'not_checked',localPrefix:{current:(before.cursors[0] as any).proven_prefix,candidate:(before.cursors[0] as any).pull_cursor,firstBlockingSequence:null}})
 expect(d.blockers).toHaveLength(8);expect(d.blockers).toContain('production_v10_reader_dispatch_incomplete');expect(d.blockers).not.toContain('own_resolution_author_chain_not_accepted')
 expect(f.before()).toEqual(before);spies.forEach(s=>expect(s).not.toHaveBeenCalled())
 expect(Object.keys(f.inspector)).toEqual(['inspect']);expect(Object.isFrozen(f.inspector)).toBe(true);expect(Object.isFrozen(d.blockers)).toBe(true)
 expect(JSON.stringify(d)).not.toMatch(/PRIVATE_|AUTHENTIC_SUCCESSOR|ciphertext|nonce|event_id|bootstrap_id|passphrase|token/)
 await expect(f.f.proven.ackOnce(d as never)).rejects.toThrow();expect((f.f.db.cursors[0] as any).ack_cursor).toBe(0)
})
for(const route of [{compressed:false},{compressed:true},{stage:true,format:'plain' as const,compressed:true,order:true,mixedInventory:true}])it(`canonical ID1 and complete bounded prefix do not imply Note support ${JSON.stringify(route)}`,async()=>{
 const f=await fixture('keep_both',false,route),d=await f.inspector.inspect(scope)
 expect(d.ready).toBe(false);expect(d.declarationAllowed).toBe(false);expect(d.serverGate).toBe('not_checked');expect(d.blockers).not.toContain('tombstone_resurrection_not_accepted')
})
for(const kind of ['key','logout','profile','epoch'] as const)it(`revoked ${kind} cannot inspect fresh accepted evidence`,async()=>{
 const f=await fixture();await f.inspector.inspect(scope)
 if(kind==='key')f.f.lock();else if(kind==='logout')await f.f.auth.logout();else if(kind==='profile')f.close();else await f.f.auth.login('fixture','fixture')
 const before=f.before();await expect(f.inspector.inspect(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const key of ['accountId','canonicalUserId','deviceId'] as const)it(`wrong ${key} rejected`,async()=>{
 const f=await fixture(),before=f.before();await expect(f.inspector.inspect({...scope,[key]:crypto.randomUUID()})).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('forged registered-device token is not accepted as owned profile authority',async()=>{
 const f=await fixture(),before=f.before();await expect(f.create({} as never).inspect(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const kind of ['parent','variant','source','metadata','own'] as const)it(`invalid ${kind} evidence never returns a readiness diagnostic`,async()=>{
 const f=await fixture(),d=f.f.db
 if(kind==='parent')d.inbox=d.inbox.filter((r:any)=>r.event_id!==f.parents[0]!.event_id)
 if(kind==='variant')d.legacyNoteHistory.push({...d.legacyNoteHistory[0],record_version:99})
 if(kind==='source')d.localNotes[0].envelope.ciphertext='AA'
 if(kind==='metadata')d.ownerHeads=[]
 if(kind==='own')d.legacyNoteHistory.find((r:any)=>r.record_kind==='resolution').source_device_id=scope.deviceId
 const before=f.before();await expect(f.inspector.inspect(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('new unresolved independent conflict is authentic but stays blocked',async()=>{
 const f=await fixture(),child=await f.receive();await f.runtime.apply(scope,child.sequence)
 const fork=await f.receive();await f.runtime.apply(scope,fork.sequence)
 const before=f.before(),d=await f.inspector.inspect(scope)
 expect(d.localPrefix.firstBlockingSequence).toBe(fork.sequence);expect(d.blockers).toContain('local_prefix_blocked');expect(d.ready).toBe(false);expect(f.before()).toEqual(before)
})
it('accepted deleted Resolution continuation no longer reports the resurrection blocker',async()=>{
 const f=await fixture('delete'),upsert=await f.receive()
 expect(await f.runtime.apply(scope,upsert.sequence)).toBe('applied')
 const before=f.before(),d=await f.inspector.inspect(scope)
 expect(d.blockers).not.toContain('tombstone_resurrection_not_accepted');expect(d.blockers).toContain('orphan_dependency_recovery_not_accepted');expect(d.ready).toBe(false);expect(f.before()).toEqual(before)
})
it('missing sequence cannot be covered by an existing receipt or diagnostic',async()=>{
 const f=await fixture('choose_version',true);f.f.db.inbox=f.f.db.inbox.filter((r:any)=>r.server_sequence!==1)
 const before=f.before(),d=await f.inspector.inspect(scope);expect(d.localPrefix).toMatchObject({candidate:0,firstBlockingSequence:1});expect(d.ready).toBe(false);expect(f.before()).toEqual(before)
})
it('all inspector transactions are readonly, also when prefix advancement is available',async()=>{
 const f=await fixture(),modes:string[]=[],tx:BrowserSyncTransaction=(s,stores,mode,schedule)=>{modes.push(mode);if(mode!=='readonly')throw Error('READINESS_WRITE');return f.f.transaction(s,stores,mode,schedule)}
 const before=f.before();expect((await f.create(f.device,tx).inspect(scope)).localPrefix.candidate).toBe(f.sequence);expect(modes.length).toBeGreaterThan(0);expect(new Set(modes)).toEqual(new Set(['readonly']));expect(f.before()).toEqual(before)
})
it('late old-group sibling and local outbound remain unaccepted despite good historical diagnostics',async()=>{
 const f=await fixture(),late=await f.receive({parent:f.parents[0]!.header.parent_event_id!,revision:2}),before=f.before()
 await expect(f.runtime.apply(scope,late.sequence)).rejects.toThrow();expect((await f.inspector.inspect(scope)).blockers).toContain('local_author_after_resolution_not_accepted')
 const source=await f.f.local.readSnapshot(scope,'p');await expect(f.f.local.updateNote(scope,source.token,'content-note',{content:'UNPUBLISHED'},'2026-10-20T00:00:00.000000Z')).rejects.toThrow();expect(f.before()).toEqual(before)
})

for(const ready of [false,true])it(`explicit gate GET ready=${ready} never grants reader or writer permission`,async()=>{
 const f=await fixture(),before=f.before(),put=vi.spyOn(encryptedSyncV3Api,'noteReaderCapabilities')
 const get=vi.spyOn(encryptedSyncV3Api,'noteReaderGate').mockResolvedValue({ready,missing_devices:ready?0:1})
 const d=await f.inspector.inspect(scope,{checkServerGate:true})
 expect(d).toMatchObject({ready:false,declarationAllowed:false,writerAuthorized:false,serverGate:ready?'checked_account_ready':'checked_blocked',serverMissingDevices:ready?0:1})
 expect(d.blockers).toHaveLength(8);expect(d.blockers).toContain('server_note_gate_not_checked')
 expect(get).toHaveBeenCalledTimes(1);expect(put).not.toHaveBeenCalled();expect(f.before()).toEqual(before)
})
for(const kind of ['network','revocation','proof'] as const)it(`gate observation fails closed on ${kind}`,async()=>{
 const f=await fixture()
 vi.spyOn(encryptedSyncV3Api,'noteReaderGate').mockImplementation(async()=>{
  if(kind==='network')throw Error('offline')
  if(kind==='revocation')f.f.lock()
  if(kind==='proof')f.f.db.legacyNoteHistory.find((r:any)=>r.record_kind==='resolution').record_digest='0'.repeat(64)
  return{ready:true,missing_devices:0}
 })
 await expect(f.inspector.inspect(scope,{checkServerGate:true})).rejects.toThrow()
})
