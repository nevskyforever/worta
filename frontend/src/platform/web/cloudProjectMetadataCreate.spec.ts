import { describe, expect, it } from 'vitest'
import { generateAccountMasterKey } from '@/crypto'
import { openProjectMetadataEvent, validateProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import { createCloudProjectMetadata } from './cloudProjectMetadataCreate'

describe('cloudProjectMetadataCreate', () => {
  const context = {
    accountId: '11111111-1111-4111-8111-111111111111',
    canonicalUserId: '11111111-1111-4111-8111-111111111111',
    deviceId: '22222222-2222-4222-8222-222222222222',
  }

  it('creates a valid Metadata Create event and seals it', async () => {
    const amk = await generateAccountMasterKey()
    const result = await createCloudProjectMetadata(context, 'Мой первый облачный проект', amk)
    expect(result.event.header.operation).toBe('create')
    expect(result.event.header.revision).toBe(1)
    expect(result.event.header.generation).toBe(1)
    expect(result.event.header.parent_event_ids).toEqual([])
    expect(result.event.metadata?.name).toBe('Мой первый облачный проект')
    expect(result.event.version).toBe(1)
    expect(result.object.crypto_version).toBe(1)
    expect(result.object.aad_version).toBe(1)
    expect(result.object.ciphertext.byteLength).toBeGreaterThan(0)
    validateProjectMetadataEvent(result.event)
  })

  it('round-trips through the existing codec and does not leak the plaintext name', async () => {
    const amk = await generateAccountMasterKey()
    const result = await createCloudProjectMetadata(context, 'SECRET_CLOUD_NAME', amk)
    const decrypted = await openProjectMetadataEvent(amk, {
      account_id: result.event.header.account_id,
      project_id: result.event.header.project_id,
      entity_id: result.event.header.entity_id,
      event_id: result.event.header.event_id,
    }, result.object)
    expect(decrypted.metadata?.name).toBe('SECRET_CLOUD_NAME')
    const text = new TextDecoder().decode(result.object.ciphertext)
    expect(text.includes('SECRET_CLOUD_NAME')).toBe(false)
  })

  it('rejects invalid inputs', async () => {
    const amk = await generateAccountMasterKey()
    await expect(createCloudProjectMetadata(context, '   ', amk)).rejects.toThrow('invalid_project_name')
    await expect(createCloudProjectMetadata({ ...context, accountId: 'not-a-uuid' }, 'ok', amk)).rejects.toThrow('invalid_account_id')
    await expect(createCloudProjectMetadata({ ...context, canonicalUserId: '33333333-3333-4333-8333-333333333333' }, 'ok', amk)).rejects.toThrow('account_mismatch')
  })
})
