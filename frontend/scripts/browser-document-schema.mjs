import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'
const source=await readFile('src/platform/web/replayStorage.ts','utf8')
const schema=Function(`return (${source.split('const previousSchema: readonly Schema[] = ')[1].split('\nconst documentSchema')[0].trim()})`)()
let browser,vite
try{
 vite=await createServer({logLevel:'error',server:{host:'127.0.0.1',port:0}});await vite.listen()
 browser=await chromium.launch({headless:true});const page=await browser.newPage();await page.goto(`http://127.0.0.1:${vite.httpServer.address().port}/scripts/browser-cloud-account-harness.html`)
 const r=await page.evaluate(async schema=>{
  const {openBrowserReplayStorage}=await import('/src/platform/web/replayStorage.ts')
  const s={accountId:'11111111-1111-4111-8111-111111111111',deviceId:'22222222-2222-4222-8222-222222222222',canonicalUserId:'33333333-3333-4333-8333-333333333333'}
  const request=q=>new Promise((ok,no)=>{q.onsuccess=()=>ok(q.result);q.onerror=()=>no(q.error)})
  const q=indexedDB.open('worta-web-replay',6);q.onupgradeneeded=()=>{for(const[n,keyPath,indexes]of schema){const st=q.result.createObjectStore(n,{keyPath});for(const[i,path,unique]of indexes)st.createIndex(i,path,{unique})}}
  const db=await request(q),tx=db.transaction(['identities','accountBindings'],'readwrite'),identity={record_version:1,canonical_user_id:s.canonicalUserId,local_account_id:s.accountId,device_id:s.deviceId}
  tx.objectStore('identities').put(identity);tx.objectStore('accountBindings').put({record_version:1,local_account_id:s.accountId,canonical_user_id:s.canonicalUserId});await new Promise((ok,no)=>{tx.oncomplete=ok;tx.onabort=no});db.close()
  const original=IDBDatabase.prototype.createObjectStore;let aborted=false,refused=false
  IDBDatabase.prototype.createObjectStore=function(n,...a){if(n==='documentProjection'){aborted=true;throw Error('migration abort')}return original.call(this,n,...a)}
  try{await openBrowserReplayStorage(s,new AbortController().signal,()=>true)}catch{refused=true}finally{IDBDatabase.prototype.createObjectStore=original}
  const old=await request(indexedDB.open('worta-web-replay'));const abortedVersion=old.version;old.close()
  const storage=await openBrowserReplayStorage(s,new AbortController().signal,()=>true),snapshot=await storage.read();await storage.close()
  const upgraded=await request(indexedDB.open('worta-web-replay')),version=upgraded.version,ids=await request(upgraded.transaction('identities').objectStore('identities').getAll());upgraded.close()
  let oldRejected=false;try{await request(indexedDB.open('worta-web-replay',6))}catch{oldRejected=true}
  const reopened=await openBrowserReplayStorage(s,new AbortController().signal,()=>true),after=await reopened.read();await reopened.close()
  return{aborted,refused,abortedVersion,version,identity,ids,snapshot,after,oldRejected}
 },schema)
 assert.equal(r.aborted,true);assert.equal(r.refused,true);assert.equal(r.abortedVersion,6);assert.equal(r.version,10);assert.equal(r.oldRejected,true);assert.deepEqual(r.ids,[r.identity]);assert.deepEqual(r.snapshot,r.after);assert.deepEqual(r.snapshot.documents,{ledger:[],projections:[]});console.log('Document schema migration/reopen/abort/old-reader: PASS')
}finally{await browser?.close();await vite?.close()}
