import type { CloudProjectBootstrapListResponse } from '@/api/cloudProjects'
import { parseV2Capabilities, type V2Capabilities } from '@/api/encryptedSyncV2'
import { decodeBase64Url } from '@/api/base64url'
import { validateAccountRegistry } from './accountRegistry'
import { BrowserPersistenceError } from './persistence'
import { immutableWireIdentity, validateBrowserInboundPage, type BrowserSyncScope } from './syncDurability'

export const REPLAY_LIMITS = Object.freeze({ events: 200, ciphertextBytes: 32 * 1024 * 1024, inventoryBytes: 32 * 1024 * 1024, controlReserveBytes: 4096, page: 100 })
export class BrowserReplayError extends Error {
  constructor(readonly code: string) { super(`browser_replay_${code}`) }
}
export interface ReplayAnchor { registry: CloudProjectBootstrapListResponse; transport: V2Capabilities }
export interface ReplaySourceSeedReference {
  project_id: string; bootstrap_id: string; origin_device_id: string; genesis_event_id: string
  initial_event_count: number; initial_max_server_sequence: number
}
export interface ReplayManifest {
  record_version: 1 | 2 | 3 | 4 | 5 | 6; contract_version: 1 | 2 | 3 | 4 | 5 | 6; account_id: string; canonical_user_id: string; device_id: string
  generation: string; anchor: ReplayAnchor; receive_cursor: number; ciphertext_bytes: number
  phase: 'receiving' | 'received'; revision: number
  metadata_cursor?: number
  applied_cursor?: number
  source_seeds?: ReplaySourceSeedReference[]
}
export const replaySame = (a: unknown, b: unknown): boolean => immutableWireIdentity(a) === immutableWireIdentity(b)
export const replayExact = (v: unknown, keys: readonly string[]): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v)
  && Object.keys(v).length === keys.length && keys.every(key => Object.hasOwn(v, key))
export const replayUuid = (v: unknown): v is string => typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(v)
const safe = (v: unknown, min = 0): v is number => Number.isSafeInteger(v) && (v as number) >= min
const fail = (code: string): never => { throw new BrowserReplayError(code) }

export function replayAnchor(registry: unknown, transport: unknown): ReplayAnchor {
  const anchor = { registry: validateAccountRegistry(registry), transport: structuredClone(parseV2Capabilities(transport)) }
  if (anchor.transport.writer_transport_version !== 3) fail('transport_v3_required')
  if (anchor.registry.current_cursor > REPLAY_LIMITS.events) fail('history_limit')
  if (anchor.registry.projects.some(project => project.state !== 'active')) fail('unsupported_lineage')
  return anchor
}

export function validateReplayManifest(value: unknown, scope: BrowserSyncScope, version: 1 | 2 | 3 | 4 | 5 | 6 = 6): ReplayManifest {
  try {
    if (!replayExact(value, ['record_version', 'contract_version', 'account_id', 'canonical_user_id', 'device_id', 'generation', 'anchor', 'receive_cursor', 'ciphertext_bytes', 'phase', 'revision', ...(version >= 2 ? ['metadata_cursor'] : []), ...(version >= 3 ? ['applied_cursor'] : []), ...(version >= 4 ? ['source_seeds'] : [])])
      || value.record_version !== version || value.contract_version !== version || value.account_id !== scope.accountId
      || value.canonical_user_id !== scope.canonicalUserId || value.device_id !== scope.deviceId || !replayUuid(value.generation)
      || !safe(value.receive_cursor) || !safe(value.ciphertext_bytes) || value.ciphertext_bytes > REPLAY_LIMITS.ciphertextBytes
      || !safe(value.revision, 1) || !['receiving', 'received'].includes(value.phase as string)
      || !replayExact(value.anchor, ['registry', 'transport'])) throw new Error()
    const anchor = replayAnchor(value.anchor.registry, value.anchor.transport)
    if (value.receive_cursor > anchor.registry.current_cursor || value.phase === 'received' && value.receive_cursor !== anchor.registry.current_cursor) throw new Error()
    if (version >= 2 && (!safe(value.metadata_cursor) || value.metadata_cursor > value.receive_cursor)) throw new Error()
    if (version >= 3 && (!safe(value.applied_cursor) || value.applied_cursor > value.receive_cursor || (value.metadata_cursor as number) > value.applied_cursor)) throw new Error()
    if (version >= 4) {
      if (!Array.isArray(value.source_seeds) || value.source_seeds.length > 200) throw new Error()
      const projects = new Set<string>()
      for (const seed of value.source_seeds) {
        if (!replayExact(seed, ['project_id','bootstrap_id','origin_device_id','genesis_event_id','initial_event_count','initial_max_server_sequence'])
          || typeof seed.project_id !== 'string' || !seed.project_id.length || seed.project_id.length > 512
          || ![seed.bootstrap_id, seed.origin_device_id, seed.genesis_event_id].every(replayUuid)
          || !safe(seed.initial_event_count) || !safe(seed.initial_max_server_sequence)
          || (seed.initial_event_count === 0) !== (seed.initial_max_server_sequence === 0)
          || seed.initial_max_server_sequence > value.receive_cursor || projects.has(seed.project_id)) throw new Error()
        const remote = anchor.registry.projects.find(project => project.project_id === seed.project_id)
        if (!remote || remote.state !== 'active' || remote.bootstrap_id !== seed.bootstrap_id || remote.origin_device_id !== seed.origin_device_id
          || remote.initial_event_count !== seed.initial_event_count || remote.initial_max_server_sequence !== seed.initial_max_server_sequence) throw new Error()
        projects.add(seed.project_id)
      }
    }
    return structuredClone(value) as unknown as ReplayManifest
  } catch { throw new BrowserPersistenceError('corrupt_record') }
}

/** Transport staging only. No AEAD/owner outcome is inferred from this validation. */
export function prepareReplayPage(raw: string, since: number, scope: BrowserSyncScope, anchor: ReplayAnchor) {
  const page = validateBrowserInboundPage(raw, since, scope)
  if (page.protocol_version !== 3 || page.items.length > REPLAY_LIMITS.page) fail('invalid_page')
  const target = anchor.registry.current_cursor
  let bytes = 0
  for (const [index, item] of page.items.entries()) {
    if (item.event.server_sequence !== since + index + 1) fail('history_gap')
    if (item.event.server_sequence! > target) fail('server_changed')
    const event = item.event as unknown as { scope?: string; project_id?: string }
    if (event.scope !== 'account' && !anchor.registry.projects.some(project => project.project_id === event.project_id && project.state === 'active')) fail('unsupported_lineage')
    const ciphertext = decodeBase64Url((item.object as { ciphertext: string }).ciphertext)
    bytes += ciphertext.length; ciphertext.fill(0)
  }
  if (page.next_cursor > target || page.has_more && page.next_cursor >= target) fail('server_changed')
  if (!page.has_more && page.next_cursor !== target || !page.items.length && since < target) fail('history_incomplete')
  return { page, bytes }
}
