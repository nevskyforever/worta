import type { CloudProjectBootstrapRecord } from '@/infrastructure/sqlite/cloudProjectBootstrapRepository'
import { BrowserPersistenceError } from './persistence'
import { immutableWireIdentity, snapshotBrowserSyncScope, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'

export interface BrowserProjectBinding extends CloudProjectBootstrapRecord { record_version: 1; initial_metadata_event_id?: string }
export interface BrowserProjectPreparation { project_id: string; mode: 'upload_existing' }
export interface BrowserRemoteProjectImport { project_id: string; bootstrap_id: string; remote_state: 'active'; remote_high_water: number }
/** Record preparation only; remote provenance must still be established by the importing owner. */
export function prepareBrowserRemoteProjectImport(scope: BrowserSyncScope, descriptor: BrowserRemoteProjectImport): BrowserProjectBinding {
  scope = snapshotBrowserSyncScope(scope)
  if (!exact(descriptor, ['project_id','bootstrap_id','remote_state','remote_high_water']) || descriptor.remote_state !== 'active' || !safe(descriptor.remote_high_water)) fail()
  project(descriptor.project_id); bootstrap(descriptor.bootstrap_id)
  return validateBrowserProjectBinding({ record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, project_id: descriptor.project_id,
    bootstrap_id: descriptor.bootstrap_id, mode: 'import_remote', phase: 'captured', remote_state: 'active', initial_event_count: 0,
    initial_local_ordinal_hi: 0, remote_high_water: descriptor.remote_high_water, initial_max_server_sequence: null, blocked_reason: null }, scope)
}
export interface BrowserProjectRegistration { remote_state: 'initializing' | 'active'; remote_high_water: number }
export type BrowserProjectReadinessCode = 'missing_lineage' | 'registration_required' | 'capture_proof_unavailable' | 'ack_proof_unavailable' | 'bootstrap_completion_unavailable' | 'registry_reconciliation_required'
export interface BrowserProjectReadiness { ready: false; code: BrowserProjectReadinessCode; required_cursor: number | null }
const keys = ['record_version','account_id','project_id','device_id','bootstrap_id','mode','phase','remote_state','initial_event_count','initial_local_ordinal_hi','remote_high_water','initial_max_server_sequence','blocked_reason']
const exact = (v: unknown, fields: readonly string[]): boolean => !!v && typeof v === 'object' && !Array.isArray(v)
  && Object.keys(v).length === fields.length && fields.every(key => Object.hasOwn(v, key))
const safe = (n: unknown): n is number => Number.isSafeInteger(n) && (n as number) >= 0
const fail = (code: 'corrupt_record' | 'conflicting_record' | 'stale_lineage' | 'invalid_lineage_transition' = 'corrupt_record'): never => { throw new BrowserPersistenceError(code) }
function project(value: string): void {
  // Existing transport IDs are opaque, nonempty strings bounded to512; never derive from a display name/path.
  if (typeof value !== 'string' || value.length < 1 || value.length > 512) fail()
}
function bootstrap(value: string): void {
  if (typeof value !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value)) fail()
}
export function validateBrowserProjectBinding(value: unknown, scope: BrowserSyncScope): BrowserProjectBinding {
  scope = snapshotBrowserSyncScope(scope)
  const metadata = !!value && Object.hasOwn(value, 'initial_metadata_event_id')
  if (!exact(value, metadata ? [...keys, 'initial_metadata_event_id'] : keys)) fail()
  const row = value as BrowserProjectBinding
  project(row.project_id); bootstrap(row.bootstrap_id)
  if (row.record_version !== 1 || row.account_id !== scope.accountId || row.device_id !== scope.deviceId
    || row.blocked_reason !== null || !safe(row.initial_event_count) || !safe(row.initial_local_ordinal_hi) ) fail()
  if (metadata && (row.mode !== 'upload_existing'
    || typeof row.initial_metadata_event_id !== 'string'
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(row.initial_metadata_event_id)
    || (['captured', 'completing'].includes(row.phase) && row.initial_event_count !== 1)
    || (row.phase === 'registered' && row.remote_state !== 'initializing'))) fail()
  if (row.mode === 'upload_existing') {
    if (row.phase === 'prepared') { if (row.remote_state !== null || row.remote_high_water !== null) fail() }
    else if (row.phase === 'registered') { if (!['initializing','active'].includes(row.remote_state!) || !safe(row.remote_high_water)) fail() }
    else if (row.phase === 'captured') {
      if (row.remote_state !== 'initializing' || !safe(row.remote_high_water) || (row.initial_event_count === 0 ? row.initial_local_ordinal_hi !== 0 : row.initial_local_ordinal_hi < row.initial_event_count)) fail()
    } else if (row.phase === 'completing') {
      if (!['initializing','active'].includes(row.remote_state!) || !safe(row.remote_high_water) || !safe(row.initial_max_server_sequence)
        || (row.initial_event_count === 0 ? row.initial_local_ordinal_hi !== 0 || row.initial_max_server_sequence !== 0 : row.initial_local_ordinal_hi < row.initial_event_count || row.initial_max_server_sequence < row.initial_event_count)
        || row.remote_state === 'active' && row.remote_high_water < row.initial_max_server_sequence) fail()
    } else fail() // Ready/paused/blocked producers remain unavailable.
    if (row.phase !== 'completing' && row.initial_max_server_sequence !== null) fail()
    if (!['captured','completing'].includes(row.phase) && (row.initial_event_count !== 0 || row.initial_local_ordinal_hi !== 0)) fail()
  } else if (row.mode === 'import_remote') {
    if (row.phase !== 'captured' || row.remote_state !== 'active' || !safe(row.remote_high_water) || row.initial_event_count !== 0 || row.initial_local_ordinal_hi !== 0 || row.initial_max_server_sequence !== null) fail()
  } else fail()
  return Object.freeze({ ...row })
}
export function prepareBrowserProjectRegistration(scope: BrowserSyncScope, expected: BrowserProjectBinding, evidence: BrowserProjectRegistration): BrowserProjectBinding {
  const row = validateBrowserProjectBinding(expected, scope)
  if (row.phase === 'completing') fail('invalid_lineage_transition') // Only verified completion response may promote this phase.
  if (!exact(evidence, ['remote_state','remote_high_water']) || !['initializing','active'].includes(evidence.remote_state) || !safe(evidence.remote_high_water)) fail()
  if (row.remote_state === 'active' && evidence.remote_state !== 'active') fail('invalid_lineage_transition')
  return { ...row, phase: row.phase === 'prepared' ? 'registered' : row.phase, remote_state: evidence.remote_state,
    remote_high_water: Math.max(row.remote_high_water ?? 0, evidence.remote_high_water) }
}
/** Full-record expected-old CAS; exact retries only, never timestamp/LWW or arbitrary patch. */
export function compareBrowserProjectTransition(scope: BrowserSyncScope, current: unknown, expected: BrowserProjectBinding, target: BrowserProjectBinding): 'changed' | 'already_committed' {
  const row = validateBrowserProjectBinding(current, scope), old = validateBrowserProjectBinding(expected, scope), next = validateBrowserProjectBinding(target, scope)
  if (old.phase === 'completing' || next.phase === 'completing') fail('invalid_lineage_transition')
  for (const key of ['initial_event_count','initial_local_ordinal_hi','initial_max_server_sequence'] as const) if (old[key] !== next[key]) fail('invalid_lineage_transition')
  for (const key of ['account_id','project_id','device_id','bootstrap_id','mode','initial_metadata_event_id'] as const) if (row[key] !== old[key] || row[key] !== next[key]) fail('conflicting_record')
  if (next.phase !== (old.phase === 'prepared' ? 'registered' : old.phase) || next.remote_high_water === null
    || next.remote_high_water < (old.remote_high_water ?? 0) || old.remote_state === 'active' && next.remote_state !== 'active') fail('invalid_lineage_transition')
  if (immutableWireIdentity(row) === immutableWireIdentity(next)) return 'already_committed'
  if (immutableWireIdentity(row) !== immutableWireIdentity(old)) fail('stale_lineage')
  return 'changed'
}
export function browserProjectReadiness(row: BrowserProjectBinding | null): BrowserProjectReadiness {
  if (!row) return { ready: false, code: 'missing_lineage', required_cursor: null }
  const required_cursor = Math.max(row.remote_high_water ?? 0, row.initial_max_server_sequence ?? 0)
  return { ready: false, code: row.mode === 'import_remote' ? 'registry_reconciliation_required' : row.phase === 'prepared' ? 'registration_required' : row.phase === 'completing' && row.remote_state === 'active' ? 'registry_reconciliation_required' : ['captured','completing'].includes(row.phase) ? 'bootstrap_completion_unavailable' : 'capture_proof_unavailable', required_cursor }
}

/** Dormant installation lineage only. Trusted future coordinator owns project preflight and remote provenance. */
export class BrowserProjectBindings {
  constructor(private readonly transaction: BrowserSyncTransaction) {}
  private create(scope: BrowserSyncScope, candidate: BrowserProjectBinding, replay: 'preparation' | 'import' | 'metadata_create'): Promise<BrowserProjectBinding> {
    candidate = validateBrowserProjectBinding(candidate, scope)
    return this.transaction(scope, ['projectBindings'], 'readwrite', (tx, result, abort) => {
      const store = tx.objectStore('projectBindings'), existing = store.get([scope.accountId, candidate.project_id])
      const lineage = store.index('bootstrap').get([scope.accountId, candidate.bootstrap_id])
      let ready = 0
      const finish = () => {
        if (++ready !== 2) return
        try {
          if (existing.result !== undefined) {
            const old = validateBrowserProjectBinding(existing.result, scope)
            if (old.mode !== candidate.mode || old.initial_metadata_event_id !== candidate.initial_metadata_event_id
              || replay !== 'preparation' && old.bootstrap_id !== candidate.bootstrap_id) fail('conflicting_record')
            result(old); return // Native prepare/import retry preserves original record; never resets phase/high-water.
          }
          if (lineage.result !== undefined && validateBrowserProjectBinding(lineage.result, scope).project_id !== candidate.project_id) fail('conflicting_record')
          store.add(candidate); result(candidate)
        } catch (error) { abort(error) }
      }
      existing.onsuccess = finish; lineage.onsuccess = finish
    })
  }
  prepare(scope: BrowserSyncScope, command: BrowserProjectPreparation): Promise<BrowserProjectBinding> {
    scope = snapshotBrowserSyncScope(scope)
    if (!exact(command, ['project_id','mode']) || command.mode !== 'upload_existing') fail()
    project(command.project_id)
    return this.create(scope, { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, project_id: command.project_id,
      bootstrap_id: crypto.randomUUID(), mode: 'upload_existing', phase: 'prepared', remote_state: null,
      initial_event_count: 0, initial_local_ordinal_hi: 0, remote_high_water: null, initial_max_server_sequence: null, blocked_reason: null }, 'preparation')
  }
  /** Retain the lineage already authenticated by the Metadata Create author. */
  prepareMetadataCreate(scope: BrowserSyncScope, projectId: string, bootstrapId: string, eventId: string): Promise<BrowserProjectBinding> {
    scope = snapshotBrowserSyncScope(scope)
    return this.create(scope, { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId,
      project_id: projectId, bootstrap_id: bootstrapId, initial_metadata_event_id: eventId, mode: 'upload_existing', phase: 'prepared',
      remote_state: null, initial_event_count: 0, initial_local_ordinal_hi: 0, remote_high_water: null,
      initial_max_server_sequence: null, blocked_reason: null }, 'metadata_create')
  }
  /** Explicit authenticated-registry import primitive, not authentication of a caller's DTO; no network/shell creation. */
  importRemote(scope: BrowserSyncScope, descriptor: BrowserRemoteProjectImport): Promise<BrowserProjectBinding> {
    scope = snapshotBrowserSyncScope(scope)
    return this.create(scope, prepareBrowserRemoteProjectImport(scope, descriptor), 'import')
  }
  read(scope: BrowserSyncScope, projectId: string): Promise<BrowserProjectBinding | null> {
    scope = snapshotBrowserSyncScope(scope); project(projectId)
    return this.transaction(scope, ['projectBindings'], 'readonly', (tx, result, abort) => {
      const request = tx.objectStore('projectBindings').get([scope.accountId, projectId])
      request.onsuccess = () => { try { result(request.result === undefined ? null : validateBrowserProjectBinding(request.result, scope)) } catch (error) { abort(error) } }
    })
  }
  list(scope: BrowserSyncScope): Promise<BrowserProjectBinding[]> {
    scope = snapshotBrowserSyncScope(scope)
    return this.transaction(scope, ['projectBindings'], 'readonly', (tx, result, abort) => {
      const request = tx.objectStore('projectBindings').getAll(IDBKeyRange.bound([scope.accountId], [scope.accountId, []]))
      request.onsuccess = () => { try { result(request.result.map(row => validateBrowserProjectBinding(row, scope))) } catch (error) { abort(error) } }
    })
  }
  confirmRegistration(scope: BrowserSyncScope, expected: BrowserProjectBinding, evidence: BrowserProjectRegistration): Promise<BrowserProjectBinding> {
    scope = snapshotBrowserSyncScope(scope); expected = validateBrowserProjectBinding(expected, scope)
    const target = prepareBrowserProjectRegistration(scope, expected, evidence)
    return this.transaction(scope, ['projectBindings'], 'readwrite', (tx, result, abort) => {
      const store = tx.objectStore('projectBindings'), request = store.get([scope.accountId, expected.project_id])
      request.onsuccess = () => {
        try {
          if (request.result === undefined) fail('conflicting_record')
          const status = compareBrowserProjectTransition(scope, request.result, expected, target)
          if (status === 'changed') store.put(target)
          result(status === 'changed' ? target : validateBrowserProjectBinding(request.result, scope))
        } catch (error) { abort(error) }
      }
    })
  }
  async evaluateReadiness(scope: BrowserSyncScope, projectId: string): Promise<BrowserProjectReadiness> { return browserProjectReadiness(await this.read(scope, projectId)) }
}
