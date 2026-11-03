import {requireOwnResolutionPublication} from './resolutionPublication'
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
  if(row.protocol_version!==3||e.entity_type!=='note'||e.operation!=='event')throw new BrowserReplayError('unsupported_resolution_cohort')
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
  if(descriptors[3].device_id===scope.deviceId)requireOwnResolutionPublication(scope,resolution,inventory)
  if (descriptors.some((e,index) => e.operation !== 'event' || index!==3&&e.device_id === scope.deviceId
    || e.project_id !== base.project_id || e.entity_id !== base.entity_id || e.deleted_at !== null)
    || descriptors[1].device_id === descriptors[2].device_id) reject()
  await lease!.use(async amk => {
    const opened = []
    for (const row of [root, first, second]) {
      opened.push(await openNoteTransport(scope, amk, 3, row.wire_item, 'content_note_v1')); check()
    }
    const tail=[]
    for(const row of rows.slice(sequence)){tail.push(await openReplayContent(scope,row,amk));check()}
    // Admission validates the repeating ordinary/fork/Resolution grammar only.
    // It never manufactures candidates, groups, source revisions or apply proofs.
    const nodes=new Map<string,{revision:number;deleted:boolean;parent:string|null;device:string;event:typeof tail[number]['event']|null}>()
    nodes.set(resolution.event_id,{revision:3,deleted:false,parent:null,device:'',event:null})
    let frontier=new Set([resolution.event_id])
    for(const content of tail){
      const event=content.event,h=event.header
      if(nodes.has(h.event_id))return reject()
      if(event.version===1){
        const parent=nodes.get(h.parent_event_id!)
        if(!parent||!['update','delete'].includes(event.mutation)||h.revision!==parent.revision+1
          ||parent.deleted&&event.mutation!=='delete'&&!(parent.event?.version===2&&parent.event.result.operation==='delete'&&event.mutation==='update'))return reject()
        if(frontier.size===1&&frontier.has(h.parent_event_id!))frontier=new Set([h.event_id])
        else{
          const sibling=nodes.get([...frontier][0]!)
          if(frontier.size!==1||!sibling||sibling.parent!==h.parent_event_id||sibling.device===content.device_id)return reject()
          frontier.add(h.event_id)
        }
        nodes.set(h.event_id,{revision:h.revision,deleted:event.mutation==='delete',parent:h.parent_event_id,device:content.device_id,event})
      }else{
        const decision=event.resolution
        if(decision.strategy!=='choose_version'||frontier.size!==2||!frontier.has(decision.selected_event_id)||!resolutionSame(decision.resolved_event_ids,[...frontier].sort()))return reject()
        const parents=decision.resolved_event_ids.map(id=>nodes.get(id)!)
        const selected=nodes.get(decision.selected_event_id)?.event
        if(h.revision!==Math.max(...parents.map(p=>p.revision))+1||!selected||selected.version!==1
          ||event.result.operation!==selected.header.operation||!resolutionSame(event.result.note,selected.note))return reject()
        nodes.set(h.event_id,{revision:h.revision,deleted:event.result.operation==='delete',parent:null,device:content.device_id,event})
        frontier=new Set([h.event_id])
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
