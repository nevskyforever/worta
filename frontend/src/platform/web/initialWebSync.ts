import { ApiError } from '@/api/client'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encodeV3MetadataPush, encryptedSyncV3Api, isAccountItem, parseV3Pull, type V3PullItem } from '@/api/encryptedSyncV3'
import type { AuthoritativeKeyContextLease } from '@/auth/keyContext'
import { openProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import { validateAccountRegistry } from './accountRegistry'
import { createCloudProjectMetadata } from './cloudProjectMetadataCreate'
import type { BrowserSyncScope } from './syncDurability'

type AuthorizedRequest = <T>(send: (token: string) => Promise<T>) => Promise<T>

function nonempty(): never {
  throw new ApiError(409, 'initial_web_sync_requires_empty_account', 'Initial Web sync requires an empty account.')
}

/** Exercise the installed reader, including its v3 decoder and Metadata AEAD,
 * with an in-memory event. The probe is never sent or written to sync storage. */
async function verifyMetadataReader(scope: BrowserSyncScope, lease: AuthoritativeKeyContextLease): Promise<void> {
  await lease.use(async amk => {
    const probe = await createCloudProjectMetadata({ ...scope, accountId: scope.canonicalUserId }, 'Reader probe', amk)
    let decoded: V3PullItem | undefined
    try {
      const h = probe.event.header
      const raw = JSON.parse(encodeV3MetadataPush(scope.deviceId, [{
        event: { event_id: h.event_id, project_id: h.project_id, entity_id: h.entity_id, entity_type: 'project_metadata',
          operation: 'upsert', revision: 1, updated_at: h.updated_at, deleted_at: null },
        object: probe.object,
      }])).items[0]
      decoded = parseV3Pull({ protocol_version: 3, encrypted_sync_version: 3, next_cursor: 1, has_more: false,
        items: [{ ...raw, event: { ...raw.event, device_id: scope.deviceId, server_sequence: 1 } }] }, 0, 1).items[0]
      if (!decoded || isAccountItem(decoded)) throw new TypeError('web_reader_probe_missing')
      const opened = await openProjectMetadataEvent(amk, {
        account_id: scope.canonicalUserId, project_id: h.project_id, entity_id: h.entity_id, event_id: h.event_id,
      }, decoded.object)
      if (opened.header.device_id !== scope.deviceId || opened.header.bootstrap_id !== h.bootstrap_id
        || opened.metadata?.name !== probe.event.metadata?.name) throw new TypeError('web_reader_probe_failed')
    } finally {
      probe.object.nonce.fill(0); probe.object.ciphertext.fill(0)
      decoded?.object.nonce.fill(0); decoded?.object.ciphertext.fill(0)
    }
  })
}

/** Explicit first-use setup. The backend rechecks emptiness under its writer lock. */
export async function prepareInitialWebSync(
  scope: BrowserSyncScope, lease: AuthoritativeKeyContextLease, request: AuthorizedRequest,
  register: () => Promise<{ deviceId: string; serverAckCursor: number }>, signal: AbortSignal,
): Promise<void> {
  const capabilities = () => request(token => encryptedSyncV2Api.capabilities(token, signal))
  let mode = await capabilities()
  if (mode.writer_transport_version === 3) return
  const registry = validateAccountRegistry(await request(token => cloudProjectsApi.listBootstraps(token, signal)))
  if (registry.current_cursor !== 0 || registry.projects.length) nonempty()
  await verifyMetadataReader(scope, lease)
  if (mode.writer_transport_version === 1) {
    await request(token => encryptedSyncV2Api.cutover(token, mode.cutover_epoch, scope.deviceId, signal))
  }
  mode = await capabilities()
  if (mode.writer_transport_version !== 2) throw new TypeError('web_sync_mode_changed')
  const device = await register()
  if (device.deviceId !== scope.deviceId || device.serverAckCursor !== 0) nonempty()
  const history = await request(token => encryptedSyncV2Api.pull(token, scope.deviceId, 0, 1, signal))
  if (history.next_cursor !== 0 || history.items.length || history.has_more) nonempty()
  await request(token => encryptedSyncV3Api.readerReady(token, scope.deviceId, true, signal))
  mode = await capabilities()
  if (mode.writer_transport_version !== 2) throw new TypeError('web_sync_mode_changed')
  await request(token => encryptedSyncV3Api.cutover(token, mode.cutover_epoch, scope.deviceId, signal))
  if ((await capabilities()).writer_transport_version !== 3) throw new TypeError('web_sync_mode_changed')
}
