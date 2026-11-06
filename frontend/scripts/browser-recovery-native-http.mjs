import assert from 'node:assert/strict'
import {readFile,mkdtemp,rm} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {createServer} from 'vite'
import {chromium} from '@playwright/test'
const fixture=JSON.parse(await readFile(process.argv[2],'utf8')),backend=process.argv[3],profile=await mkdtemp(join(tmpdir(),'worta-http-native-')),realProfile=process.argv[4]??await mkdtemp(join(tmpdir(),'worta-http-real-'))
let vite,context,fault=null
const recoveryRequests=[]
const forbiddenRequests=[]
try{
 vite=await createServer({logLevel:'error',server:{host:'127.0.0.1',port:Number(process.argv[5]??0),strictPort:true}});await vite.listen()
 const origin=`http://127.0.0.1:${vite.httpServer.address().port}`
 const launch=async(directory=profile)=>{
  context=await chromium.launchPersistentContext(directory,{headless:true})
  await context.route(url=>url.pathname.startsWith('/api/'),async route=>{
   const url=new URL(route.request().url())
   if((route.request().method()==='PUT' && url.pathname.endsWith('/note-reader-capabilities')) || url.pathname==='/api/v3/sync/encrypted/ack')forbiddenRequests.push(`${route.request().method()} ${url.pathname}`)
   if(fixture.pagination&&url.pathname==='/api/v3/recovery/encrypted/history')url.searchParams.set('limit','1')
   const target=backend+url.pathname+url.search
   if(url.pathname==='/api/v3/recovery/encrypted/history')recoveryRequests.push(Number(url.searchParams.get('since')))
   if(fault&&url.pathname==='/api/v3/recovery/encrypted/history'&&url.searchParams.get('since')==='0'){
    const response=await route.fetch({url:target}),body=await response.json()
    if(fault==='old'&&body.items.length){const s=body.items[0].object.ciphertext;body.items[0].object.ciphertext=(s[0]==='A'?'B':'A')+s.slice(1)}
    if(fault==='gap'&&body.items.length)body.items[0].event.server_sequence++
    return route.fulfill({response,json:body})
   }
   return route.continue({url:target})
  })
  const page=await context.newPage();await page.goto(origin+'/scripts/browser-cloud-account-harness.html');return page
 }
 const execute=page=>page.evaluate(async f=>{
  const {NormalUserAuthRuntime}=await import('/src/auth/userAuth.ts'),{withReceivedBrowserAccountHistory}=await import('/src/platform/web/replayReceiver.ts')
  const auth=new NormalUserAuthRuntime();await auth.login('Arthur','correct horse battery staple');const ctx=auth.requireContext()
  const scope={accountId:f.accountId,deviceId:f.deviceId,canonicalUserId:ctx.userId},bindings={ensureForCurrentUser:async()=>({context:ctx})}
  const lease={localAccountId:scope.accountId,canonicalUserId:scope.canonicalUserId,authEpoch:ctx.authEpoch,isCurrent:()=>auth.isCurrent(ctx),use:async fn=>{const amk=Uint8Array.from(f.amk);try{return await fn(amk)}finally{amk.fill(0)}}}
  const keys={leaseForAccount:()=>lease,onInvalidated:()=>()=>{}}
  return await withReceivedBrowserAccountHistory({auth,lease,scope,signal:new AbortController().signal,current:()=>true,delivery:'recovery'},async store=>{
   await store.importRecoveryProject(f.projectId,auth,bindings,keys,'recovery')
   const outcomes=[]
   for(let i=0;i<f.sequence+1;i++){
    const before=await store.read()
    if(before.manifest.applied_cursor===f.sequence-1){
     const put=IDBObjectStore.prototype.put;let fired=false
     IDBObjectStore.prototype.put=function(value,...args){const r=put.call(this,value,...args);if(this.name==='inbox'&&value.state==='content_note_proven'){fired=true;r.addEventListener('success',()=>this.transaction.abort(),{once:true})}return r}
     try{await store.recoverV10(auth,bindings,keys,'recovery');throw Error('abort_not_rejected')}catch(error){if(!fired)throw error}finally{IDBObjectStore.prototype.put=put}
     if(JSON.stringify(await store.read())!==JSON.stringify(before))throw Error('abort_changed_snapshot')
    }
    const result=await store.recoverV10(auth,bindings,keys,'recovery');outcomes.push(result);if(!result.more)break;if(result.blocker&&!result.blocker.retryable)break
   }
   const state=await store.read()
   const local=store.contentAuthorOwner(auth,bindings,keys).local,source=await local.readSnapshot(scope,f.projectId)
   const notes=await local.withDecryptedSnapshot(scope,source.token,async value=>value.notes.map(n=>({id:n.note.id,content:n.note.content,stageId:n.note.stage_id,format:n.note.content_format})))
   return {outcomes,state,notes}
  })
 },fixture)
 let page=await launch(),first=await execute(page)
 assert.equal(first.state.manifest.applied_cursor,fixture.sequence,JSON.stringify(first.outcomes))
 assert.equal(first.state.rows.at(-1).state,'content_note_proven')
 assert.equal(first.state.rows.at(-1).wire_item,JSON.stringify(fixture.item))
 assert.equal(first.notes[0]?.id,'native-note')
 assert.equal(first.notes[0]?.content,fixture.expectedContent)
 if(fixture.contentFormat){assert.equal(first.notes[0]?.format,fixture.contentFormat);assert.equal(first.notes[0]?.stageId,fixture.stageId)}
 assert.equal(first.state.checkpoint.ack_cursor,0)
 if(fixture.items){
  assert.deepEqual(first.state.rows.map(row=>JSON.parse(row.wire_item)),fixture.items)
  assert.equal(first.state.rows.filter(row=>row.state==='content_note_proven').length,3)
  assert.equal(new Set(first.state.rows.map(row=>row.event_id)).size,fixture.sequence)
 }
 await context.close();page=await launch();const reopened=await execute(page);assert.deepEqual(reopened.state,first.state);assert.deepEqual(reopened.notes,first.notes)
 const original=first.state.rows.at(-1)
 await page.evaluate(async row=>{const q=indexedDB.open('worta-web-replay', 8);await new Promise((ok,no)=>{q.onsuccess=ok;q.onerror=no});const db=q.result;const damaged=structuredClone(row),wire=JSON.parse(damaged.wire_item);wire.object.ciphertext=(wire.object.ciphertext[0]==='A'?'B':'A')+wire.object.ciphertext.slice(1);damaged.wire_item=JSON.stringify(wire);await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite');tx.objectStore('inbox').put(damaged);tx.oncomplete=ok;tx.onabort=no});db.close()},original)
 await assert.rejects(()=>execute(page))
 await page.evaluate(async row=>{const q=indexedDB.open('worta-web-replay', 8);await new Promise((ok,no)=>{q.onsuccess=ok;q.onerror=no});const db=q.result;await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite');tx.objectStore('inbox').put(row);tx.oncomplete=ok;tx.onabort=no});db.close()},original)
 assert.deepEqual((await execute(page)).state,first.state)
 await page.evaluate(async row=>{const q=indexedDB.open('worta-web-replay', 8);await new Promise((ok,no)=>{q.onsuccess=ok;q.onerror=no});const db=q.result;const damaged=structuredClone(row);damaged.content_note_proof.outcome_digest='0'.repeat(64);await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite');tx.objectStore('inbox').put(damaged);tx.oncomplete=ok;tx.onabort=no});db.close()},original)
 await assert.rejects(()=>execute(page))
 await page.evaluate(async row=>{const q=indexedDB.open('worta-web-replay', 8);await new Promise((ok,no)=>{q.onsuccess=ok;q.onerror=no});const db=q.result;await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite');tx.objectStore('inbox').put(row);tx.oncomplete=ok;tx.onabort=no});db.close()},original)
 assert.deepEqual((await execute(page)).state,first.state)
 // Provision the isolated account with the Desktop's actual key through the
 // standard wrapped-key API, then exercise the real Web login/unlock runtime.
 if(!fixture.provisioned)await page.evaluate(async raw=>{
  const crypto=await import('/src/crypto/index.ts'),{encodeBase64Url}=await import('/src/api/base64url.ts'),{accountCryptoApi}=await import('/src/api/accountCrypto.ts')
  const auth=await (await fetch('/api/v1/auth/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({username:'Arthur',password:'correct horse battery staple'})})).json()
  const amk=Uint8Array.from(raw),recovery=await crypto.generateRecoveryKey()
  try{const password=await crypto.wrapAmkWithPassphrase(amk,'test-encryption-password'),wrapped=await crypto.wrapAmkWithRecoveryKey(amk,recovery)
   await accountCryptoApi.provision(auth.access_token,{password:{...password,kdf:{...password.kdf,salt:encodeBase64Url(password.kdf.salt)},nonce:encodeBase64Url(password.nonce),ciphertext:encodeBase64Url(password.ciphertext)},recovery:{...wrapped,nonce:encodeBase64Url(wrapped.nonce),ciphertext:encodeBase64Url(wrapped.ciphertext)}})
  }finally{amk.fill(0);recovery.fill(0)}
 },fixture.amk)
 await context.close();page=await launch(realProfile)
 await page.evaluate(async projectId=>{
  const {BrowserCloudAccountSession}=await import('/src/platform/web/cloudAccountSession.ts'),{mountRecoveryPanel}=await import('/scripts/browser-recovery-ui.ts')
  window.userSession=new BrowserCloudAccountSession();await userSession.login('Arthur','correct horse battery staple');await userSession.unlock('test-encryption-password')
  const root=document.createElement('div');document.body.append(root);window.testApp=mountRecoveryPanel(root,userSession,projectId)
 },fixture.projectId)
 const panel=page.locator('[data-testid="cloud-project-read"]')
 if(fixture.provisioned && !fixture.expectStale){
  await panel.locator('[data-testid="cloud-notes-open"]').waitFor({state:'visible'})
  await page.waitForFunction(()=>!document.querySelector('[data-testid="cloud-notes-open"]').disabled)
  await page.evaluate(async sequence=>{
   const snapshot=()=>new Promise((ok,no)=>{const q=indexedDB.open('worta-web-replay', 8);q.onerror=no;q.onsuccess=()=>{const db=q.result,tx=db.transaction([...db.objectStoreNames]),all={};for(const name of db.objectStoreNames){const r=tx.objectStore(name).getAll();r.onsuccess=()=>{all[name]=r.result}}tx.oncomplete=()=>{db.close();ok(all)};tx.onabort=no}})
   window.beforeExtension=await snapshot();window.readReplaySnapshot=snapshot;window.originalPut=IDBObjectStore.prototype.put;window.extensionAborted=false;window.extensionSequence=sequence
  },fixture.sequence)
  const initial=await page.evaluate(()=>window.beforeExtension)
  for(const kind of ['old','gap']){
   fault=kind;await panel.locator('[data-testid="cloud-notes-open"]').click();await panel.locator('[role="alert"]').waitFor();fault=null
   assert.deepEqual(await page.evaluate(()=>window.readReplaySnapshot()),initial)
  }
  await page.evaluate(()=>{IDBObjectStore.prototype.put=function(value,...args){const result=window.originalPut.call(this,value,...args);if(this.name==='replayManifest'&&value.anchor?.registry?.current_cursor===window.extensionSequence){window.extensionAborted=true;result.addEventListener('success',()=>this.transaction.abort(),{once:true})}return result}})
  await panel.locator('[data-testid="cloud-notes-open"]').click()
  await panel.locator('[role="alert"]').waitFor()
  const rollback=await page.evaluate(async()=>{IDBObjectStore.prototype.put=window.originalPut;return {fired:window.extensionAborted,before:window.beforeExtension,after:await window.readReplaySnapshot()}})
  assert.equal(rollback.fired,true);assert.deepEqual(rollback.after,rollback.before)
  const race=await page.evaluate(async()=>{
   const {openBrowserReplayStorage}=await import('/src/platform/web/replayStorage.ts'),{cloudProjectsApi}=await import('/src/api/cloudProjects.ts'),{encryptedSyncV2Api}=await import('/src/api/encryptedSyncV2.ts'),{encryptedSyncV3Api}=await import('/src/api/encryptedSyncV3.ts')
   const auth=userSession.auth,ctx=auth.requireContext(),identity=userSession.currentIdentity,scope={accountId:identity.local_account_id,deviceId:identity.device_id,canonicalUserId:ctx.userId},signal=new AbortController().signal
   const store=await openBrowserReplayStorage(scope,signal,()=>auth.isCurrent(ctx))
   try{
    const expected=await store.read(),registry=(await auth.authorized(t=>cloudProjectsApi.listBootstraps(t,signal))).value,transport=(await auth.authorized(t=>encryptedSyncV2Api.capabilities(t,signal))).value,anchor={registry,transport}
    const raw=(await auth.authorized(t=>encryptedSyncV3Api.recoveryRaw(t,expected.manifest.receive_cursor,100,registry.current_cursor,transport.cutover_epoch,signal))).value
    const server=async()=>{const fresh=(await auth.authorized(t=>encryptedSyncV3Api.recoveryRaw(t,expected.manifest.receive_cursor,100,registry.current_cursor,transport.cutover_epoch,signal))).value;if(fresh!==raw)throw Error('server_changed');await auth.authorized(t=>encryptedSyncV3Api.recoveryRaw(t,registry.current_cursor,1,registry.current_cursor,transport.cutover_epoch,signal))}
    let release,ready;const waiting=new Promise(ok=>release=ok),prepared=new Promise(ok=>ready=ok)
    const stale=store.stageRecoveryExtension(expected,anchor,raw,async()=>{await server();ready();await waiting}).then(()=>false,()=>true)
    await prepared;const winner=await store.stageRecoveryExtension(expected,anchor,raw,server);release();const refused=await stale
    const after=await store.read();return {refused,winner,after,expected}
   }finally{await store.close()}
  })
  assert.equal(race.refused,true);assert.deepEqual(race.after,race.winner)
  assert.deepEqual(race.after.bindings,race.expected.bindings)
  assert.equal(race.after.manifest.applied_cursor,race.expected.manifest.applied_cursor)
  assert.equal(race.after.rows.at(-1).state,'received')
 }
 if(fixture.pagination){for(let since=0;since<fixture.sequence;since++)assert.ok(recoveryRequests.includes(since),`missing real recovery page ${since}`);assert.ok(recoveryRequests.filter(since=>since===0).length>=2)}
 await panel.locator('[data-testid="cloud-notes-open"]').click()
 if(fixture.expectStale){
  await panel.locator('[role="alert"]').waitFor()
  assert.equal(await panel.locator('[role="alert"]').getAttribute('data-read-code'),'stale_authority')
  assert.equal(await panel.locator('[data-testid="cloud-project-notes"]').count(),0)
 }else{
  await page.locator('[data-testid="cloud-project-notes"]').waitFor()
  if(fixture.stages?.length){const stages=panel.locator('[data-stage-id]');assert.deepEqual(await stages.evaluateAll(nodes=>nodes.map(node=>node.getAttribute('data-stage-id'))),fixture.stages);assert.deepEqual(await stages.allTextContents(),fixture.stages.map(id=>`Stage ${id}`));await panel.locator('[data-stage-id="S2"]').click();assert.equal(await panel.locator('[data-testid="cloud-note-html"]').count(),0);await panel.locator('[data-stage-id="S1"]').click()}
  if(fixture.contentFormat==='html'){
   const frame=page.locator('[data-testid="cloud-note-html"]')
   assert.equal(await frame.getAttribute('sandbox'),'')
   const rendered=await page.frameLocator('[data-testid="cloud-note-html"]').locator('body').innerText()
   assert.ok(rendered.includes(fixture.expectedContent.replace(/<[^>]*>/g,'')))
   if(fixture.expectedContent.includes('<strong>'))assert.equal(await page.frameLocator('[data-testid="cloud-note-html"]').locator('strong').count(),1)
  }else assert.ok((await page.locator('[data-testid="cloud-project-notes"]').innerText()).includes(fixture.expectedContent))
 }
 await page.evaluate(async()=>{testApp.unmount();await userSession.dispose()})
 if(fixture.provisioned && !fixture.expectStale){
  await context.close();page=await launch(realProfile)
  const reopened=await page.evaluate(async projectId=>{const {BrowserCloudAccountSession}=await import('/src/platform/web/cloudAccountSession.ts');const reader=new BrowserCloudAccountSession();try{await reader.login('Arthur','correct horse battery staple');await reader.unlock('test-encryption-password');return await reader.readRecoveryProject(projectId,new AbortController().signal)}finally{await reader.dispose()}},fixture.projectId)
  assert.equal(reopened.notes[0]?.content,fixture.expectedContent)
 }
 assert.deepEqual(forbiddenRequests,[])
 console.log(JSON.stringify({pass:true,applied:fixture.sequence}))
}finally{await context?.close();await vite?.close();await rm(profile,{recursive:true,force:true});if(!process.argv[4])await rm(realProfile,{recursive:true,force:true})}
