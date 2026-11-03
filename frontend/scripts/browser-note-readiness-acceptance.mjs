import assert from 'node:assert/strict'
import {runResolutionPrefixAcceptance} from './browser-resolution-prefix-acceptance.mjs'
let profiles=0,authenticated=0,revoked=0,writes=0
await runResolutionPrefixAcceptance(async c=>{
 const before=await c.snapshot(),traffic=[]
 const listener=r=>{if(new URL(r.url()).pathname.startsWith('/api/'))traffic.push(r.method()+':'+new URL(r.url()).pathname)}
 // External fixture registration only. The inspector must never register itself.
 await c.page.evaluate(async()=>{
  const device=await session.device.bind(meta.auth,meta.bindings).registerVerified(s)
  globalThis.readiness=c21.noteReaderReadinessFixture(session,meta,device)
 })
 c.page.on('request',listener)
 try{
  const result=await c.page.evaluate(async()=>{
   const tx=IDBDatabase.prototype.transaction,put=IDBObjectStore.prototype.put,add=IDBObjectStore.prototype.add,del=IDBObjectStore.prototype.delete,clear=IDBObjectStore.prototype.clear;let writes=0
   IDBDatabase.prototype.transaction=function(stores,mode,...args){if(mode==='readwrite'){writes++;throw Error('READINESS_WRITE')}return tx.call(this,stores,mode,...args)}
   for(const name of ['put','add','delete','clear'])IDBObjectStore.prototype[name]=()=>{writes++;throw Error('READINESS_WRITE')}
   try{return{diagnostic:await readiness.inspect(s),writes,methods:Object.keys(readiness)}}
   finally{IDBDatabase.prototype.transaction=tx;Object.assign(IDBObjectStore.prototype,{put,add,delete:del,clear})}
  })
  const d=result.diagnostic
  assert.equal(d.ready,false);assert.equal(d.declarationAllowed,false);assert.equal(d.writerAuthorized,false)
  assert.equal(d.localEvidence,'authenticated_bounded_inventory');assert.equal(d.serverGate,'not_checked')
  assert.equal(d.localPrefix.current,before.cursors[0].proven_prefix)
  assert.equal(d.blockers.includes('own_resolution_author_chain_not_accepted'),false)
  assert.equal(d.blockers.length,c.decisions.strategy==='manual_merge'?10:9)
  for(const code of ['production_v10_reader_dispatch_incomplete','transport_v10_page_replay_incomplete','server_note_gate_not_checked'])assert.ok(d.blockers.includes(code))
  assert.deepEqual(result.methods,['inspect']);assert.equal(result.writes,0);writes+=result.writes
  assert.doesNotMatch(JSON.stringify(d),/ciphertext|nonce|event_id|bootstrap_id|passphrase|token/)
  assert.deepEqual(await c.snapshot(),before);assert.deepEqual(traffic,[]);authenticated++
  // The just-restarted real disk profile has fresh normal auth/AMK/device proof.
  // Revoking any one lifetime must reject, rather than return a cached success.
  const failure=await c.page.evaluate(async strategy=>{
   if(strategy==='choose_version')await meta.keys.lock()
   else if(strategy==='manual_merge')await meta.auth.logout()
   else if(strategy==='delete')await session.close()
   else await meta.auth.login('fixture','fixture')
   try{await readiness.inspect(s);return false}catch{return true}
  },c.decisions.strategy)
  assert.equal(failure,true);assert.deepEqual(await c.snapshot(),before);assert.deepEqual(traffic,[]);revoked++
 }finally{c.page.off('request',listener)}
 await c.page.evaluate(async()=>{await meta.keys.dispose();await session.close()});await c.bind()
 assert.deepEqual(await c.snapshot(),before);profiles++
})
console.log(JSON.stringify({result:'PASS',I:'negative-readiness-preflight',profiles,authenticated,revoked,readOnlyWrites:writes,capabilityPUT:0,capabilityGET:0,ContentPush:0,ResolutionACK:0,pageErrors:0,positiveTokens:0,ready:false}))
