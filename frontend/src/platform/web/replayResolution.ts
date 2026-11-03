import { decryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { decodeBase64Url } from '@/api/base64url'
import { unframeContentNote } from '@/cloud/contentNoteCodec'
import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp'
import { resolutionSame, type FutureInbox } from './resolutionRecords'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import type { VerifiedBrowserDevice } from './deviceRegistration'
import { BrowserLocalAuthority } from './localAuthority'
import { BrowserResolutionPlanner, resolutionInventoryStores, scheduleResolutionInventory, type ResolutionInventorySnapshot } from './resolutionPlanner'
import { readFutureInboxRecord } from './resolutionRecords'
import { openNoteTransport } from './noteTransport'
import { BrowserReplayError } from './replayRecords'
import type { BrowserSyncScope, BrowserSyncTransaction } from './syncDurability'

/** Decode for route admission only. Owner tokens remain the sole apply authority. */
export async function openReplayContent(scope:BrowserSyncScope,row:FutureInbox,amk:AccountMasterKey){
  const wire=JSON.parse(row.wire_item),e=wire.event
  if(row.protocol_version!==3||e.entity_type!=='note'||e.operation!=='event'||e.device_id===scope.deviceId)throw new BrowserReplayError('unsupported_resolution_cohort')
  const nonce=decodeBase64Url(wire.object.nonce),ciphertext=decodeBase64Url(wire.object.ciphertext)
  let bytes:Uint8Array|undefined
  try{
    bytes=await decryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:e.project_id,entityId:e.entity_id,entityType:'note'},{...wire.object,nonce,ciphertext})
    const content=unframeContentNote(bytes),h=content.event.header
    if(content.account_id!==scope.canonicalUserId||content.device_id!==e.device_id||h.event_id!==row.event_id||h.project_id!==e.project_id||h.entity_id!==e.entity_id||h.revision!==e.revision||h.updated_at!==canonicalizeSyncTimestamp(e.updated_at))throw new BrowserReplayError('unsupported_resolution_cohort')
    return content
  }finally{bytes?.fill(0);nonce.fill(0);ciphertext.fill(0)}
}

/** Bounded v6 owner entry. The transaction provider owns atomic manifest coverage
 * alongside the inventory; no reader declaration, prefix or ACK is granted here. */
export async function applyReplayResolutionOwner(input: {
  contractVersion: number; scope: BrowserSyncScope; sequence: number
  transaction: BrowserSyncTransaction; current: () => boolean
  auth: NormalUserAuthRuntime; bindings: AuthoritativeAccountBinding; keys: RuntimeKeyContext
  device: VerifiedBrowserDevice
}) {
  const { scope, sequence, transaction, current, auth, bindings, keys, device } = input
  const reject = (): never => { throw new BrowserReplayError('unsupported_resolution_cohort') }
  if (input.contractVersion !== 6 || !Number.isSafeInteger(sequence) || sequence < 5) reject()
  const context = auth.requireContext(), lease = keys.leaseForAccount(scope.accountId)
  const check = () => {
    if (!current() || !auth.isCurrent(context) || !lease?.isCurrent()
      || lease.localAccountId !== scope.accountId || lease.canonicalUserId !== scope.canonicalUserId
      || context.userId !== scope.canonicalUserId || lease.authEpoch !== context.authEpoch)
      throw new BrowserReplayError('session_closed')
  }
  check()
  const inventory = await transaction<ResolutionInventorySnapshot>(scope, resolutionInventoryStores, 'readonly', (tx, result, abort) =>
    scheduleResolutionInventory(tx, scope, current, result, abort))
  check()
  const rows = inventory.rows.map(row => readFutureInboxRecord(row, scope))
  if (rows.length < sequence || inventory.cursor.pull_cursor !== rows.length
    || rows.some((row, index) => row.server_sequence !== index + 1)) reject()
  const notes = rows.slice(0, sequence).filter(row => JSON.parse(row.wire_item).event.entity_type === 'note')
  if (notes.length !== 4 || notes.some((row, index) => row.server_sequence !== sequence - 3 + index
    || row.protocol_version !== 3)) reject()
  const root = notes[0]!, first = notes[1]!, second = notes[2]!, resolution = notes[3]!
  for (const tail of rows.slice(sequence)) {
    const e = JSON.parse(tail.wire_item).event, parent = JSON.parse(resolution.wire_item).event
    if (tail.protocol_version !== 3 || tail.state !== 'received' || e.entity_type !== 'note' || e.operation !== 'event'
      || e.project_id !== parent.project_id || e.entity_id !== parent.entity_id || e.revision <= parent.revision
      || e.deleted_at !== null || e.device_id === scope.deviceId) reject()
  }
  if (root.state !== 'content_note_proven' || first.state !== 'content_note_proven'
    || second.state !== 'content_note_conflict_preserved'
    || !['received', 'note_resolution_proven'].includes(resolution.state)) reject()
  if (rows.slice(0, sequence - 4).some(row => row.state !== 'applied'
    || JSON.parse(row.wire_item).event.entity_type !== 'project_metadata')) reject()
  const descriptors = notes.map(row => JSON.parse(row.wire_item).event)
  const base = descriptors[0]
  if (descriptors.some(e => e.operation !== 'event' || e.device_id === scope.deviceId
    || e.project_id !== base.project_id || e.entity_id !== base.entity_id || e.deleted_at !== null)
    || descriptors[1].device_id === descriptors[2].device_id) reject()
  await lease!.use(async amk => {
    const opened = []
    for (const row of [root, first, second]) {
      opened.push(await openNoteTransport(scope, amk, 3, row.wire_item, 'content_note_v1')); check()
    }
    const tail=[]
    for(const row of rows.slice(sequence)){tail.push(await openReplayContent(scope,row,amk));check()}
    if(tail.some(c=>c.event.version===2)){
      const a=tail[0],b=tail[1],r=tail[2]
      if(tail.length!==3||!a||!b||!r)return reject()
      const first=a.event,second=b.event,result=r.event
      if(first.version!==1||second.version!==1||result.version!==2)return reject()
      if(first.mutation!=='update'||second.mutation!=='update'||a.device_id===b.device_id
        ||first.header.parent_event_id!==resolution.event_id||second.header.parent_event_id!==resolution.event_id
        ||first.header.revision!==4||second.header.revision!==4||result.header.revision!==5
        ||result.resolution.strategy!=='choose_version'
        ||!resolutionSame(result.resolution.resolved_event_ids,[first.header.event_id,second.header.event_id].sort()))return reject()
      const selected=result.resolution.selected_event_id===first.header.event_id?first:result.resolution.selected_event_id===second.header.event_id?second:undefined
      if(!selected||result.result.operation!=='upsert'||!resolutionSame(result.result.note,selected.note))return reject()
    }else{
      let parent=resolution.event_id,revision=3,deleted=false
      for(const content of tail){
        const event=content.event
        if(event.version!==1||!['update','delete'].includes(event.mutation)||event.header.parent_event_id!==parent
          ||event.header.revision!==revision+1||deleted&&event.mutation!=='delete')reject()
        parent=event.header.event_id;revision=event.header.revision;deleted=event.mutation==='delete'
      }
    }
    const genesis = opened[0]!, one = opened[1]!, two = opened[2]!
    if (genesis.content?.event.mutation !== 'create'
      || genesis.plaintext.header.revision !== 1 || genesis.plaintext.header.parent_event_id !== null
      || [one, two].some(value => value.content?.event.mutation !== 'update'
        || value.plaintext.header.revision !== 2
        || value.plaintext.header.parent_event_id !== genesis.plaintext.header.event_id)) reject()
  })
  check()
  const planner = new BrowserResolutionPlanner(transaction, new BrowserLocalAuthority(transaction), current)
    .bind(auth, bindings, keys, device)
  const owner = planner.atomicApply()
  // The existing owner authenticates the selected version, group and all parent
  // proofs, and compares the full inventory inside its atomic write transaction.
  let result: { status: 'applied' | 'already_applied' }
  if (resolution.state === 'note_resolution_proven') {
    const record = inventory.history.find((value: any) => value.record_kind === 'resolution' && value.event_id === resolution.event_id) as { strategy?: string } | undefined
    if (record?.strategy !== 'choose_version') reject()
    result = await owner.reconcile(scope, sequence)
  } else {
    const plan = await planner.prepare(scope, sequence); check()
    if (plan.status.strategy !== 'choose_version' || plan.status.parentCount !== 2) reject()
    result = await owner.commit(await owner.prepare(plan.token))
  }
  check()
  await owner.reconcile(scope, sequence); check()
  return { kind: 'bounded_resolution_owner_applied' as const, status: result.status,
    ready: false as const, declarationAllowed: false as const, writerAuthorized: false as const }
}
