// @vitest-environment node
import { expect, it } from 'vitest'
import fixture from '@/cloud/__fixtures__/progressCodecV1.json'
import { prepareProgressAppend } from '@/cloud/progressAuthor'
import { transitionProgress } from '@/cloud/progressTransition'
import { unframeProgressEvent, type ProgressEvent } from '@/cloud/progressCodec'
import { asAccountMasterKey, decryptObjectBytes } from '@/crypto'
import { decodeBase64Url } from '@/api/base64url'
import { validateBrowserSealedBody, prepareBrowserUploadAcceptance } from './syncDurability'
import { sealProgressForOutbox } from './progressOutbound'

it('seals a Progress append as an existing outbox body and checks the matching receipt', async () => {
  const parent = structuredClone(fixture.examples[0]!.event) as ProgressEvent
  const deviceId = crypto.randomUUID(), scope = { accountId: crypto.randomUUID(), canonicalUserId: parent.header.account_id, deviceId }
  const event = prepareProgressAppend({ parent, chain: transitionProgress(parent), deviceId, metadataEventId: parent.header.metadata_event_id,
    stageEventIds: parent.header.stage_event_ids, now: '2026-10-03T00:00:00.000000Z', eventId: crypto.randomUUID() },
  { entryId: crypto.randomUUID(), totalSymbols: '170.000000', unit: 'symbols', writingDay: '2026-10-03' })
  const key = asAccountMasterKey(new Uint8Array(32)), raw = await sealProgressForOutbox(scope, event, key)
  expect(validateBrowserSealedBody(raw, scope)).toMatchObject({ eventId: event.header.event_id, protocol: 3 })
  const item = JSON.parse(raw).items[0], envelope = { ...item.object, nonce: decodeBase64Url(item.object.nonce), ciphertext: decodeBase64Url(item.object.ciphertext) }
  const bytes = await decryptObjectBytes(key, { userId: scope.canonicalUserId, projectId: event.header.project_id, entityId: event.header.entity_id, entityType: 'progress' }, envelope)
  expect(await unframeProgressEvent(bytes)).toEqual(event)
  const out = { record_version: 1 as const, account_id: scope.accountId, device_id: deviceId, event_id: event.header.event_id, local_ordinal: 1, protocol_version: 3 as const, state: 'sealed' as const, raw_body: raw }
  const response = { protocol_version: 3, encrypted_sync_version: 3, results: [{ event_id: event.header.event_id, server_sequence: 4, duplicate: false }], current_cursor: 4 }
  expect(prepareBrowserUploadAcceptance(scope, out, response).server_sequence).toBe(4)
  expect(() => prepareBrowserUploadAcceptance(scope, out, { ...response, results: [{ ...response.results[0], event_id: crypto.randomUUID() }] })).toThrow()
  await expect(sealProgressForOutbox({ ...scope, deviceId: crypto.randomUUID() }, event, key)).rejects.toThrow('progress_scope_mismatch')
})
