import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api } from '@/api/encryptedSyncV3'
import type { BrowserSyncScope } from './syncDurability'
import { BrowserContentNoteApply } from './contentNoteApply'
import { BrowserLegacyNoteApply } from './legacyNoteApply'
import { BrowserLocalAuthority } from './localAuthority'
import { reauthenticateReplayNotes } from './replayNoteProof'
import { reauthenticateReplayOwners, type MetadataPort } from './replayMetadata'
import { BrowserReplayError, prepareReplayPage, replayAnchor, replaySame } from './replayRecords'

/** Explicit single-event adapter; server receipt/reconciliation remains owned by the receiver.
 * No discovery, dispatch loop, capability, qualified prefix or ACK surface. */
export async function applyReplayContentUpdate(port: MetadataPort, scope: BrowserSyncScope, sequence: number,
  signal: AbortSignal, current: () => boolean, auth: NormalUserAuthRuntime,
  bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
  const context = auth.requireContext(), lease = keys.leaseForAccount(scope.accountId)
  const check = () => {
    if (signal.aborted || !current() || !auth.isCurrent(context) || !lease?.isCurrent()
      || context.userId !== scope.canonicalUserId || lease.localAccountId !== scope.accountId
      || lease.canonicalUserId !== scope.canonicalUserId || lease.authEpoch !== context.authEpoch)
      throw new BrowserReplayError('session_closed')
  }
  check()
  let snapshot = await port.read(); check()
  const manifest = snapshot.manifest, row = snapshot.rows[sequence - 1]
  if (!Number.isSafeInteger(sequence) || !manifest || (manifest.contract_version !== 5 && manifest.contract_version !== 6) || manifest.phase !== 'received'
    || !row || row.server_sequence !== sequence || row.protocol_version !== 3
    || !['received', 'content_note_proven'].includes(row.state)
    || (row.state === 'received' ? manifest.applied_cursor !== sequence - 1 : manifest.applied_cursor !== sequence))
    throw new BrowserReplayError('unsupported_content_cohort')
  const event = JSON.parse(row.wire_item).event
  const previous = snapshot.rows.slice(0, sequence - 1).filter(value => {
    const e = JSON.parse(value.wire_item).event
    return e.project_id === event.project_id && e.entity_type === 'note' && e.entity_id === event.entity_id
  }).at(-1)
  if (event.entity_type !== 'note' || event.operation !== 'event' || event.device_id === scope.deviceId
    || !previous || previous.state !== 'note_proven' || previous.note_proof.mutation === 'delete')
    throw new BrowserReplayError('unsupported_content_cohort')
  const anchor = manifest.anchor
  const server = async () => {
    check()
    const registry = await auth.authorized(token => cloudProjectsApi.listBootstraps(token, signal)); check()
    const mode = await auth.authorized(token => encryptedSyncV2Api.capabilities(token, signal)); check()
    if (!replaySame(replayAnchor(registry.value, mode.value), anchor)) throw new BrowserReplayError('server_changed')
    let since = 0
    while (since < anchor.registry.current_cursor) {
      const response = await auth.authorized(token => encryptedSyncV3Api.pullRaw(token, scope.deviceId, since, 100, signal)); check()
      const { page: received } = prepareReplayPage(response.value, since, scope, anchor)
      for (const item of received.items) {
        if (!replaySame(JSON.parse(snapshot.rows[item.event.server_sequence! - 1]!.wire_item), item))
          throw new BrowserReplayError('retained_wire_mismatch')
      }
      if (received.next_cursor <= since) throw new BrowserReplayError('history_incomplete')
      since = received.next_cursor
    }
    const raw = await auth.authorized(token => encryptedSyncV3Api.pullRaw(token, scope.deviceId, anchor.registry.current_cursor, 1, signal)); check()
    const { page } = prepareReplayPage(raw.value, anchor.registry.current_cursor, scope, anchor)
    if (page.items.length || page.has_more) throw new BrowserReplayError('server_changed')
  }
  const transaction = () => port.transaction(snapshot, sequence, server)
  const owner = () => {
    const tx = transaction()
    return new BrowserContentNoteApply(new BrowserLegacyNoteApply(tx, new BrowserLocalAuthority(tx), current)).bind(auth, bindings, keys)
  }
  const authenticate = async () => {
    await lease!.use(async amk => {
      const verified = await reauthenticateReplayNotes(scope, snapshot, amk, current, async project => {
        const local = new BrowserLocalAuthority(transaction()).bind(auth, bindings, keys)
        const source = await local.readSnapshot(scope, project)
        const value = await local.withDecryptedSnapshot(scope, source.token, async opened => structuredClone(opened.project))
        return { source, project: value }
      })
      await reauthenticateReplayOwners(scope, snapshot, amk, current, true, verified)
    }); check()
    // Existing owner also verifies fresh Metadata/Stage dependency certificates.
    for (const applied of snapshot.rows.filter(value => value.state === 'content_note_proven')) {
      await owner().prepare(scope, applied.server_sequence); check()
    }
  }
  await server(); await authenticate()
  const runtime = owner(), token = await runtime.prepare(scope, sequence); check()
  const status = await runtime.commit(token); check()
  if (!['applied', 'already_applied'].includes(status)) throw new BrowserReplayError('content_not_applied')
  snapshot = await port.read(); check()
  await authenticate(); await server()
  if (!replaySame(await port.read(), snapshot)) throw new BrowserReplayError('metadata_inventory_changed')
  check()
  return { kind: 'bounded_content_update_applied' as const, sequence, status,
    ready: false as const, declarationAllowed: false as const, writerAuthorized: false as const, projectOpened: false as const }
}
