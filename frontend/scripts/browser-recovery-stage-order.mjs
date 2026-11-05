import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'

const fixture=JSON.parse(await readFile(process.argv[2],'utf8')), backend=process.argv[3], profile=process.argv[4]
let context, vite, corruptionChecked=false
try {
 vite=await createServer({logLevel:'error',server:{host:'127.0.0.1',port:Number(process.argv[5]),strictPort:true}});await vite.listen()
 const origin=`http://127.0.0.1:${vite.httpServer.address().port}`
 async function check(){
  context=await chromium.launchPersistentContext(profile,{headless:true})
  const forbidden=[]
  await context.route(url=>url.pathname.startsWith('/api/'),route=>{
   const request=route.request(), url=new URL(request.url())
   if((request.method()==='PUT'&&url.pathname.endsWith('/note-reader-capabilities'))||url.pathname==='/api/v3/sync/encrypted/ack')forbidden.push(url.pathname)
   return route.continue({url:backend+url.pathname+url.search})
  })
  const page=await context.newPage();await page.goto(origin+'/scripts/browser-cloud-account-harness.html')
  await page.evaluate(async projectId=>{
   const {BrowserCloudAccountSession}=await import('/src/platform/web/cloudAccountSession.ts'),{mountRecoveryPanel}=await import('/scripts/browser-recovery-ui.ts')
   window.userSession=new BrowserCloudAccountSession();await userSession.login('Arthur','correct horse battery staple');await userSession.unlock('test-encryption-password')
   const root=document.createElement('div');document.body.append(root);window.testApp=mountRecoveryPanel(root,userSession,projectId)
  },fixture.projectId)
  const panel=page.locator('[data-testid="cloud-project-read"]'),button=panel.locator('[data-testid="cloud-notes-open"]')
  await button.waitFor();await page.waitForFunction(()=>!document.querySelector('[data-testid="cloud-notes-open"]').disabled)
  await button.click();await panel.locator('[data-testid="cloud-stage-list"], [role="alert"]').first().waitFor();if(await panel.locator('[role="alert"]').count())throw Error(`recovery refused: ${await panel.locator('[role="alert"]').getAttribute('data-read-code')}`)
  const stages=panel.locator('[data-stage-id]')
  assert.deepEqual(await stages.evaluateAll(nodes=>nodes.map(n=>n.getAttribute('data-stage-id'))),fixture.stages)
  assert.deepEqual(await stages.allTextContents(),fixture.stages.map(id=>fixture.names?.[id]??`Stage ${id}`))
  await panel.locator('[data-stage-id="S1"]').click()
  if(fixture.firstHtml)assert.ok((await panel.frameLocator('[data-testid="cloud-note-html"]').locator('body').innerText()).includes(fixture.firstText))
  else assert.ok((await panel.innerText()).includes(fixture.firstText))
  await panel.locator('[data-stage-id="S2"]').click();assert.ok((await panel.innerText()).includes(fixture.secondText))
  if(fixture.names){await button.click();await panel.locator('[data-testid="cloud-stage-list"]').waitFor();assert.equal(await panel.locator('[data-stage-id="S2"]').getAttribute('aria-pressed'),'true');assert.ok((await panel.innerText()).includes(fixture.secondText))}
  if(!corruptionChecked){
   const row=await page.evaluate(async()=>{const q=indexedDB.open('worta-web-replay', 7);await new Promise((ok,no)=>{q.onsuccess=ok;q.onerror=no});const db=q.result;const result=await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite'),store=tx.objectStore('inbox'),all=store.getAll();let original;all.onsuccess=()=>{original=all.result.find(r=>r.state==='stage_proven');if(!original){tx.abort();return}const damaged=structuredClone(original);damaged.stage_proof.wire_digest='0'.repeat(64);store.put(damaged)};tx.oncomplete=()=>ok(original);tx.onabort=no});db.close();return result})
   await button.click();await panel.locator('[role="alert"]').waitFor();assert.equal(await panel.locator('[data-testid="cloud-stage-list"]').count(),0)
   await page.evaluate(async original=>{const q=indexedDB.open('worta-web-replay', 7);await new Promise((ok,no)=>{q.onsuccess=ok;q.onerror=no});const db=q.result;await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite');tx.objectStore('inbox').put(original);tx.oncomplete=ok;tx.onabort=no});db.close()},row)
   corruptionChecked=true
  }
  await page.evaluate(async()=>{testApp.unmount();await userSession.dispose()})
  assert.deepEqual(forbidden,[]);await context.close();context=undefined
 }
 await check();await check();console.log(JSON.stringify({pass:true}))
} finally {await context?.close();await vite?.close()}
