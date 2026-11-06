// @vitest-environment node
import { expect,it } from 'vitest'
import { stageSetup,scope } from './stageApply.testFixture'
import { inventory } from './provenPrefix.testFixture'
import { amk } from './bootstrap.testFixture'
import golden from '@/cloud/__fixtures__/gameCodecV1.json'
import { frameGameEvent,type ProjectGameEvent } from '@/cloud/gameCodec'
import { encryptObjectBytes } from '@/crypto'
import { encodeBase64Url } from '@/api/base64url'
import { prepareGameEvent,inspectPreparedGameEvent } from './gamePrepare'
import type { ReplaySnapshot } from './replayInventory'
async function fixture(){
 const f=await stageSetup(),v=inventory(f),event=structuredClone(golden.examples[0]!.event) as ProjectGameEvent,id=crypto.randomUUID()
 Object.assign(event.header,{account_id:scope.canonicalUserId,device_id:'44444444-4444-4444-8444-444444444444',project_id:'p',stage_id:null,event_id:id,entity_id:`game:project:${id}`,bootstrap_id:f.metadataEvent.header.bootstrap_id,metadata_event_id:f.metadataEvent.header.event_id,stage_event_ids:[]})
 const frame=frameGameEvent(event),sealed=await encryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:'p',entityId:event.header.entity_id,entityType:'project_game'},frame)
 const input={server_sequence:2,wire_item:JSON.stringify({event:{event_id:id,device_id:event.header.device_id,project_id:'p',entity_id:event.header.entity_id,entity_type:'project_game',operation:'event',revision:1,updated_at:event.header.updated_at,deleted_at:null,server_sequence:2},object:{crypto_version:1,aad_version:1,nonce:encodeBase64Url(sealed.nonce),ciphertext:encodeBase64Url(sealed.ciphertext)}})}
 frame.fill(0);sealed.nonce.fill(0);sealed.ciphertext.fill(0)
 const snapshot={manifest:null,rows:v.rows,bindings:v.bindings,owners:v.owners,checkpoint:{record_version:1,account_id:scope.accountId,device_id:scope.deviceId,...v.cursor}} as ReplaySnapshot
 return {f,event,input,snapshot}
}
it('prepares encrypted Game genesis only against genuine Metadata/import evidence; stale cursor refuses',async()=>{
 const {f,event,input,snapshot}=await fixture(),before=structuredClone(f.db)
 const token=await prepareGameEvent(scope,input,snapshot,amk,()=>true)
 if(event.action.kind!=='genesis')throw Error('fixture')
 expect(inspectPreparedGameEvent(token,scope,snapshot).projection).toEqual({streak:event.action.base,completion_claimed:event.action.completion_claimed})
 const changed=structuredClone(snapshot);changed.checkpoint!.pull_cursor++
 expect(()=>inspectPreparedGameEvent(token,scope,changed)).toThrow('stale_source');expect(f.db).toEqual(before)
})
it('refuses wrong sequence, missing dependency, damaged ciphertext and logout without writes',async()=>{
 const {f,input,snapshot}=await fixture(),before=structuredClone(f.db)
 await expect(prepareGameEvent(scope,{...input,server_sequence:3},snapshot,amk,()=>true)).rejects.toThrow()
 await expect(prepareGameEvent(scope,input,{...snapshot,owners:[]},amk,()=>true)).rejects.toThrow()
 await expect(prepareGameEvent(scope,input,snapshot,amk,()=>false)).rejects.toThrow('closed')
 const wire=JSON.parse(input.wire_item);wire.object.ciphertext=(wire.object.ciphertext[0]==='A'?'B':'A')+wire.object.ciphertext.slice(1)
 await expect(prepareGameEvent(scope,{...input,wire_item:JSON.stringify(wire)},snapshot,amk,()=>true)).rejects.toThrow()
 expect(f.db).toEqual(before)
})
