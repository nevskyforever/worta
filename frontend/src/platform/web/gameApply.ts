import { gameOwner, gameProject, type GameState } from '@/cloud/gameProjectTransition'
import type { GameEvent } from '@/cloud/gameCodec'
import { encryptAccountObject, decryptAccountObject } from '@/crypto/accountObjectCrypto'
import { encryptObjectBytes, decryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { encodeBase64Url, decodeBase64Url } from '@/api/base64url'
import { canonical } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { prepareGameEvent, inspectPreparedGameEvent, type PreparedGameEvent } from './gamePrepare'
import type { ReplaySnapshot } from './replayInventory'
import { type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'

import { joinGameRecords, gameDigest, type GameInbox, type GameLedger as GameGenesisLedger, type GameProjection } from './gameRecords'
export type { GameLedger as GameGenesisLedger, GameProjection } from './gameRecords'
const fail = (): never => { throw new BrowserPersistenceError('corrupt_record') }

/** Verify encrypted projection against a freshly authenticated event token. */
export async function authenticateGameProjection(scope: BrowserSyncScope, token: PreparedGameEvent,
  inventory: ReplaySnapshot, ledger: GameGenesisLedger, projection: GameProjection, amk: AccountMasterKey) {
  const plan = inspectPreparedGameEvent(token, scope, inventory), h = plan.event.header
  if (ledger.account_id !== scope.accountId || ledger.device_id !== scope.deviceId || ledger.project_id !== gameProject(plan.event)
    || ledger.entity_id !== gameOwner(plan.event) || ledger.event_id !== h.event_id || ledger.server_sequence !== plan.serverSequence
    || ledger.outcome !== 'applied' || ledger.wire_item !== plan.wireItem || projection.account_id !== scope.accountId
    || projection.device_id !== scope.deviceId || projection.project_id !== gameProject(plan.event) || projection.entity_id !== gameOwner(plan.event) || projection.head_event_id !== h.event_id) fail()
  const nonce = decodeBase64Url(projection.envelope.nonce), ciphertext = decodeBase64Url(projection.envelope.ciphertext)
  let bytes: Uint8Array | undefined
  try {
    bytes = h.scope === 'account'
      ? await decryptAccountObject(amk, {userId:scope.canonicalUserId,scope:'account',entityId:gameOwner(plan.event),entityType:'account_game'}, {...projection.envelope,crypto_version:2,aad_version:2,nonce,ciphertext})
      : await decryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: gameOwner(plan.event), entityType: 'project_game' }, { ...projection.envelope,crypto_version:1,aad_version:1, nonce, ciphertext })
    if (new TextDecoder('utf-8', { fatal: true }).decode(bytes) !== canonical({ chain: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence })) fail()
    inspectPreparedGameEvent(token, scope, inventory)
    return structuredClone(plan.projection)
  } finally { nonce.fill(0); ciphertext.fill(0); bytes?.fill(0) }
}

/** Reauthenticate durable outcomes using the same source/ledger/projection checks
 * for recovery, source reimport and unlocked presentation. Digests alone never
 * authorize a row. The caller retains its full snapshot CAS/lifetime check. */
export async function authenticateReplayGames(scope: BrowserSyncScope,
  snapshot: import('./replayInventory').ReplaySnapshot, amk: AccountMasterKey, current: () => boolean) {
  if (!current()) throw new BrowserPersistenceError('closed')
  if (!snapshot.rows.some(row => row.state === 'game_proven')) return []
  const inventory = snapshot, values: BrowserProjectGame[] = [], events:GameEvent[] = [], verified = new Map<number, PreparedGameEvent>()
  for (const row of snapshot.rows) if (row.state === 'game_proven') {
    if (!current()) throw new BrowserPersistenceError('closed')
    const joined = joinGameRecords(scope, row, snapshot.games?.ledger ?? [], snapshot.games?.projections ?? [])
    if (await gameDigest(joined.ledger) !== row.game_proof.ledger_digest
      || await gameDigest(joined.projection) !== row.game_proof.projection_digest) fail()
    const token = await prepareGameEvent(scope, row, inventory, amk, current, verified)
    const chain = await authenticateGameProjection(scope, token, inventory, joined.ledger, joined.projection, amk)
    events.push(inspectPreparedGameEvent(token,scope,inventory).event)
    if (snapshot.games?.projections.some(value => (value as GameProjection).head_event_id === row.event_id)) values.push({ event:inspectPreparedGameEvent(token, scope, inventory).event, state:chain })
  }
  if (!current()) throw new BrowserPersistenceError('closed')
  return values.map(value=>({...value,history:events.filter(event=>gameOwner(event)===gameOwner(value.event))}))
}

export interface BrowserProjectGame { event: GameEvent; state: GameState }

export interface GameCommitResult { status:'applied';game_sequences:number[] }
/** One Game-owner transaction for a dependency-closed received window. Source
 * proofs are written before dependants inside the SAME IDB commit. Nothing is
 * made applied, visible or ACK-qualified in an intermediate transaction. */
export async function commitGameBatch(transaction:BrowserSyncTransaction,scope:BrowserSyncScope,tokens:PreparedGameEvent[],expected:ReplaySnapshot,amk:AccountMasterKey,current:()=>boolean,
 readInventory:(tx:IDBTransaction,done:(v:ReplaySnapshot)=>void,abort:(error:unknown)=>void)=>void):Promise<GameCommitResult>{
 if(!tokens.length)fail()
 const plans=tokens.map(token=>inspectPreparedGameEvent(token,scope,expected))
 const sequences=plans.map(plan=>plan.serverSequence).sort((a,b)=>a-b)
 if(sequences.some((n,i)=>n!==expected.manifest!.applied_cursor!+i+1))fail()
 const projections=new Map((expected.games?.projections??[]).map(value=>{const p=value as GameProjection;return [JSON.stringify([p.project_id,p.entity_id]),p]}))
 const writes:{ledger:GameGenesisLedger;projection:GameProjection;proof:GameInbox['game_proof'];row:GameInbox}[]=[]
 for(const plan of plans){
  if(!current())throw new BrowserPersistenceError('closed')
  const h=plan.event.header,key=JSON.stringify([gameProject(plan.event),gameOwner(plan.event)]),previous=projections.get(key)
  if(plan.event.action.kind==='genesis'?!!previous:!previous||h.parents[0]!==previous.head_event_id)fail()
  const ledger:GameGenesisLedger={account_id:scope.accountId,device_id:scope.deviceId,project_id:gameProject(plan.event),entity_id:gameOwner(plan.event),event_id:h.event_id,server_sequence:plan.serverSequence,outcome:'applied',wire_item:plan.wireItem,...(previous?{previous_projection:structuredClone(previous)}:{})}
  const bytes=new TextEncoder().encode(canonical({chain:plan.projection,event_id:h.event_id,server_sequence:plan.serverSequence}))
  let projection:GameProjection
  try{
   const sealed=h.scope==='account'
    ?await encryptAccountObject(amk,{userId:scope.canonicalUserId,scope:'account',entityId:gameOwner(plan.event),entityType:'account_game'},bytes)
    :await encryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:h.project_id,entityId:gameOwner(plan.event),entityType:'project_game'},bytes)
   try{projection={account_id:scope.accountId,device_id:scope.deviceId,project_id:ledger.project_id,entity_id:ledger.entity_id,head_event_id:h.event_id,envelope:{crypto_version:sealed.crypto_version,aad_version:sealed.aad_version,nonce:encodeBase64Url(sealed.nonce),ciphertext:encodeBase64Url(sealed.ciphertext)}}}
   finally{sealed.nonce.fill(0);sealed.ciphertext.fill(0)}
  }finally{bytes.fill(0)}
  const proof:GameInbox['game_proof']={record_version:1,owner_kind:'game',outcome:'applied',ledger_digest:await gameDigest(ledger),projection_digest:await gameDigest(projection!)}
  const row=expected.rows.find(row=>row.server_sequence===plan.serverSequence)
  if(!row||row.state!=='received'||row.event_id!==h.event_id||row.wire_item!==plan.wireItem)fail()
  writes.push({ledger,projection:projection!,proof,row:{...row!,record_version:14,state:'game_proven',game_proof:proof} as GameInbox})
  projections.set(key,projection!)
 }
 return transaction(scope,['gameLedger','gameProjection','inbox','replayManifest'],'readwrite',(tx,result,abort)=>{
  readInventory(tx,inventory=>{try{
   for(const token of tokens)inspectPreparedGameEvent(token,scope,inventory)
   if(!current())throw new BrowserPersistenceError('closed')
   for(const write of writes){
    tx.objectStore('gameLedger').add(write.ledger)
    tx.objectStore('gameProjection').put(write.projection)
    tx.objectStore('inbox').put(write.row)
   }
   result({status:'applied',game_sequences:sequences})
  }catch(error){abort(error)}},abort)
 })
}
