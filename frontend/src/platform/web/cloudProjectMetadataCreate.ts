import {
  sealProjectMetadataEvent,
  validateProjectMetadataEvent,
  type MetadataHeader,
  type ProjectMetadata,
  type ProjectMetadataEvent,
} from '@/cloud/projectMetadataCodec'
import type { AccountMasterKey } from '@/crypto'
import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp'

export interface CloudProjectCreateContext {
  /** Canonical user/account UUID from the authenticated session. */
  readonly accountId: string
  /** Canonical user UUID (same as accountId for project metadata). */
  readonly canonicalUserId: string
  /** Verified registered browser device UUID. */
  readonly deviceId: string
}

export interface CloudProjectMetadataCreateResult {
  readonly event: ProjectMetadataEvent
  readonly object: Awaited<ReturnType<typeof sealProjectMetadataEvent>>
}

function uuid(): string {
  const value = crypto.randomUUID()
  if (!value) throw new Error('random_uuid_unavailable')
  return value
}

function assertUuid(value: string, field: string): void {
  const re = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
  if (!re.test(value)) throw new Error(`invalid_${field}`)
}

/**
 * Build and seal the initial project Metadata Create event.
 * The plaintext name is used only inside the encrypted event; it is never
 * returned in any wire envelope. Only the ciphertext object is suitable for
 * an encrypted sync v3 push.
 */
export async function createCloudProjectMetadata(
  context: CloudProjectCreateContext,
  name: string,
  amk: AccountMasterKey,
): Promise<CloudProjectMetadataCreateResult> {
  assertUuid(context.accountId, 'account_id')
  assertUuid(context.canonicalUserId, 'canonical_user_id')
  assertUuid(context.deviceId, 'device_id')
  if (context.accountId !== context.canonicalUserId) throw new Error('account_mismatch')
  const trimmed = name.trim()
  if (!trimmed) throw new Error('invalid_project_name')
  if (new TextEncoder().encode(trimmed).length > 512) throw new Error('invalid_project_name')
  const now = canonicalizeSyncTimestamp(new Date().toISOString())
  const projectId = uuid()
  const bootstrapId = uuid()
  const eventId = uuid()
  const metadata: ProjectMetadata = {
    name: trimmed,
    goal: null,
    infinite: true,
    unit: 'symbols',
    deadline: null,
    status: 'активен',
    personal_goal: 0,
    auto_freeze: false,
    streak_enabled: true,
    work_method: 'по дням',
    stages_enabled: false,
    combine_stage_mindmaps: false,
  }
  const header: MetadataHeader = {
    account_id: context.accountId,
    bootstrap_id: bootstrapId,
    device_id: context.deviceId,
    entity_id: projectId,
    event_id: eventId,
    generation: 1,
    operation: 'create',
    parent_event_ids: [],
    project_id: projectId,
    revision: 1,
    updated_at: now,
  }
  const event: ProjectMetadataEvent = { version: 1, header, metadata, deleted_at: null }
  validateProjectMetadataEvent(event)
  const object = await sealProjectMetadataEvent(amk, event)
  return { event, object }
}
