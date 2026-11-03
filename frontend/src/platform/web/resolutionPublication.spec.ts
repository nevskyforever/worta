// @vitest-environment node
import {it,expect} from 'vitest'
import {plannerFixture} from './resolutionPlanner.testFixture'
import {scope} from './bootstrap.testFixture'
import {requireOwnResolutionPublication} from './resolutionPublication'

it('a peer ciphertext cannot become own Resolution authorship by relabelling transport and fabricating publication records',async()=>{
 const f=await plannerFixture(),wire=JSON.parse(f.received.wire_item)
 wire.event.device_id=scope.deviceId
 f.received.wire_item=JSON.stringify(wire)
 const {device_id:_device,server_sequence:_seq,...event}=wire.event
 const raw=JSON.stringify({protocol_version:3,encrypted_sync_version:3,device_id:scope.deviceId,items:[{event,object:wire.object}]})
 const [queued]=await f.f.sync.enqueue(scope,[raw])
 await f.f.sync.commitUploadAcceptance(scope,queued!.item,{protocol_version:3,encrypted_sync_version:3,current_cursor:f.sequence,results:[{event_id:f.received.event_id,server_sequence:f.sequence,duplicate:false}]})
 const old=f.before();await expect(f.runtime.prepare(scope,f.sequence)).rejects.toThrow();expect(f.before()).toEqual(old)
})
it.each(['out','receipt','sequence','wire'] as const)('own publication refuses missing or mismatching %s evidence',async kind=>{
 const f=await plannerFixture(),wire=JSON.parse(f.received.wire_item);wire.event.device_id=scope.deviceId
 const {device_id:_device,server_sequence:_seq,...event}=wire.event,raw=JSON.stringify({protocol_version:3,encrypted_sync_version:3,device_id:scope.deviceId,items:[{event,object:wire.object}]})
 const out={record_version:1,account_id:scope.accountId,device_id:scope.deviceId,event_id:f.received.event_id,local_ordinal:1,protocol_version:3,state:'accepted',raw_body:raw},receipt={record_version:1,account_id:scope.accountId,device_id:scope.deviceId,event_id:f.received.event_id,protocol_version:3,server_sequence:f.sequence,duplicate:false}
 const row={...f.received,wire_item:JSON.stringify(wire)}
 if(kind==='sequence')receipt.server_sequence++
 if(kind==='wire'){const body=JSON.parse(out.raw_body);body.items[0].event.revision++;out.raw_body=JSON.stringify(body)}
 expect(()=>requireOwnResolutionPublication(scope,row,{out:kind==='out'?[]:[out],receipts:kind==='receipt'?[]:[receipt]})).toThrow()
})
