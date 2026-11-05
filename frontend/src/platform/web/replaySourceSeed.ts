import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api, type V3Descriptor } from '@/api/encryptedSyncV3'
import { decodeBase64Url } from '@/api/base64url'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import { openProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import { openEmptyLocalProject, sealEmptyLocalProject } from './localAuthority'
import type { NativeLocalProject, LocalProjectRecord } from './localAuthorityRecords'
import { metadataProofFor } from './importedMetadataApply'
import { validateBrowserMetadataHead, type BrowserMetadataProof } from './metadataApplyRecords'
import { openNoteTransport } from './noteTransport'
import { reauthenticateReplayOwners } from './replayMetadata'
import { BrowserReplayError, prepareReplayPage, replayAnchor, replaySame, type ReplaySourceSeedReference } from './replayRecords'
import type { ReplaySnapshot } from './replayInventory'
import { reauthenticateReplayNotes } from './replayNoteProof'
import type { LocalSourceSnapshot } from './localAuthority'
import { immutableWireIdentity, type BrowserSyncScope } from './syncDurability'
import type { AccountMasterKey } from '@/crypto'

export interface ReplaySourceSeedResult {
  readonly kind: 'encrypted_note_source_seed'; readonly status: 'seeded' | 'already_seeded'
  readonly projectOpened: false; readonly ready: false; readonly declarationAllowed: false; readonly writerAuthorized: false
}
interface SeedPort {
  read(): Promise<ReplaySnapshot>
  readSource(expected: ReplaySnapshot, projectId: string): Promise<{ source: LocalSourceSnapshot; project: NativeLocalProject }>
  commit(expected: ReplaySnapshot, row: LocalProjectRecord, reference: ReplaySourceSeedReference, server: () => Promise<void>): Promise<void>
}
interface Evidence { project: NativeLocalProject; reference: ReplaySourceSeedReference }
const fail = (code: string): never => { throw new BrowserReplayError(code) }

/** Authenticates the server's actual bootstrap cohort; it never applies any Note. */
export async function authenticateReplaySourceSeed(scope: BrowserSyncScope, snapshot: ReplaySnapshot, projectId: string, amk: AccountMasterKey,
  current: () => boolean): Promise<Evidence> {
  const manifest = snapshot.manifest
  if (!manifest) throw new BrowserReplayError('source_seed_ineligible')
  if (manifest.phase !== 'received' || snapshot.rows.length !== manifest.receive_cursor
    || manifest.receive_cursor !== manifest.anchor.registry.current_cursor) fail('source_seed_ineligible')
  const remote = manifest.anchor.registry.projects.find(project => project.project_id === projectId)
  if (!remote || remote.state !== 'active' || !remote.bootstrap_id || !remote.origin_device_id
    || remote.origin_device_id === scope.deviceId || remote.initial_event_count === null
    || remote.initial_max_server_sequence === null) throw new BrowserReplayError('source_seed_ineligible')
  const noteOwners = new Map<string, string>(), metadataRows: ReplaySnapshot['rows'] = [], initial: ReplaySnapshot['rows'] = []
  for (const row of snapshot.rows) {
    if (!current()) fail('session_closed')
    const event = JSON.parse(row.wire_item).event as V3Descriptor
    if (event.entity_type === 'document') {
      // Seeding imports Metadata only. A later peer Document genesis remains
      // received until the Document owner authenticates and applies it.
      if (event.project_id !== projectId || row.server_sequence <= remote.initial_max_server_sequence
        || row.state !== 'received' || event.operation !== 'event' || event.revision !== 1
        || event.deleted_at !== null || event.device_id === scope.deviceId) fail('source_seed_unsupported_family')
      continue
    }
    if (!['note', 'project_metadata', 'stage', 'stage_order'].includes(event.entity_type)) fail('source_seed_unsupported_family')
    if (event.entity_type === 'note') {
      const owner = noteOwners.get(event.entity_id)
      if (owner && owner !== event.project_id) fail('source_seed_note_identity_collision')
      noteOwners.set(event.entity_id, event.project_id)
      if (event.project_id === projectId) {
        // The bootstrap pins only its original cohort. Later events are
        // admitted by the Note owner and its causal history, never by the seed.
        if (row.server_sequence <= remote.initial_max_server_sequence) initial.push(row)
      }
    }
    if (event.project_id === projectId && event.entity_type === 'project_metadata') metadataRows.push(row)
  }
  if (initial.length !== remote.initial_event_count || (initial.at(-1)?.server_sequence ?? 0) !== remote.initial_max_server_sequence) fail('source_seed_cohort_mismatch')
  if (!metadataRows.length) fail('source_seed_genesis_missing')
  let project: NativeLocalProject | undefined
  const chain: BrowserMetadataProof[] = []
  for (const [index, row] of metadataRows.entries()) {
    if (!current()) fail('session_closed')
    const wire = JSON.parse(row.wire_item), descriptor = wire.event as V3Descriptor
    if (descriptor.entity_id !== projectId || descriptor.project_id !== projectId || descriptor.device_id !== remote.origin_device_id
      || descriptor.operation !== 'upsert' || descriptor.deleted_at !== null || row.protocol_version !== 3) fail('source_seed_metadata_history_rejected')
    let nonce: Uint8Array | undefined, ciphertext: Uint8Array | undefined, identity: Uint8Array<ArrayBuffer> | undefined, digest: Uint8Array | undefined
    try {
      nonce = decodeBase64Url(wire.object.nonce); ciphertext = decodeBase64Url(wire.object.ciphertext)
      const event = await openProjectMetadataEvent(amk, { account_id: scope.canonicalUserId, project_id: projectId,
        entity_id: projectId, event_id: descriptor.event_id }, { ...wire.object, nonce, ciphertext })
      identity = new TextEncoder().encode(immutableWireIdentity({ protocol_version: row.protocol_version, event: descriptor, object: wire.object }))
      digest = new Uint8Array(await crypto.subtle.digest('SHA-256', identity))
      const hash = Array.from(digest, byte => byte.toString(16).padStart(2, '0')).join('')
      const verified = metadataProofFor(scope, descriptor, event, hash, row.protocol_version)
      const previous = chain.at(-1), metadata = event.metadata ?? fail('source_seed_metadata_history_rejected')
      if (verified.bootstrap_id !== remote.bootstrap_id || verified.source_device_id !== remote.origin_device_id
        || metadata.status !== 'активен'
        || (index === 0 ? verified.operation !== 'create' || verified.revision !== 1 || verified.generation !== 1 || verified.parent_event_ids.length
          : verified.operation !== 'update' || verified.revision !== previous!.revision + 1
            || verified.generation <= previous!.generation || !replaySame(verified.parent_event_ids, [previous!.event_id]))) fail('source_seed_metadata_history_rejected')
      if (index === 0) {
        if (row.state !== 'received' && (row.state !== 'applied'
          || !snapshot.owners.some(head => head.owner_kind === 'project_metadata' && head.owner_id === projectId
            && head.history.some(proof => proof.event_id === descriptor.event_id)))) fail('source_seed_genesis_rejected')
        project = { id: projectId, name: metadata.name, goal: metadata.goal,
          infinite: metadata.infinite, unit: metadata.unit, status: metadata.status,
          created_at: event.header.updated_at, updated_at: event.header.updated_at, payload: {} }
      }
      chain.push(verified)
    } catch (error) {
      if (error instanceof BrowserReplayError) throw error
      return fail('source_seed_metadata_history_rejected')
    } finally { nonce?.fill(0); ciphertext?.fill(0); identity?.fill(0); digest?.fill(0) }
  }
  validateBrowserMetadataHead({ record_version: 1, account_id: scope.accountId, device_id: scope.deviceId,
    owner_kind: 'project_metadata', owner_id: projectId, bootstrap_id: remote.bootstrap_id,
    tip_event_ids: [chain.at(-1)!.event_id], history: [...chain].sort((a, b) => a.event_id.localeCompare(b.event_id)) }, scope)
  const noteIds = new Set<string>()
  for (const row of initial) {
    if (!current()) fail('session_closed')
    const item = JSON.parse(row.wire_item), event = item.event as V3Descriptor
    if (row.protocol_version !== 3 || event.device_id !== remote.origin_device_id || event.project_id !== projectId
      || event.operation !== 'upsert' || event.revision !== 1 || event.deleted_at !== null
      || noteIds.has(event.entity_id) || !['received', 'note_proven'].includes(row.state)) fail('source_seed_cohort_mismatch')
    const opened = await openNoteTransport(scope, amk, row.protocol_version, row.wire_item, 'legacy')
    if (opened.plaintext.mutation !== 'create' || opened.plaintext.header.parent_event_id !== null
      || opened.plaintext.note.project_id !== projectId || opened.plaintext.note.id !== event.entity_id) fail('source_seed_cohort_mismatch')
    noteIds.add(event.entity_id)
  }
  if (!current()) fail('session_closed')
  return { project: project!, reference: { project_id: projectId, bootstrap_id: remote.bootstrap_id,
    origin_device_id: remote.origin_device_id, genesis_event_id: chain[0]!.event_id,
    initial_event_count: remote.initial_event_count, initial_max_server_sequence: remote.initial_max_server_sequence } }
}

/** Internal owner: full server reconciliation precedes this call; no caller supplies seed authority. */
export async function seedReplayNoteSource(port: SeedPort, scope: BrowserSyncScope, projectId: string, signal: AbortSignal,
  current: () => boolean, auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding,
  keys: RuntimeKeyContext, delivery: 'ordinary' | 'recovery' = 'ordinary'): Promise<ReplaySourceSeedResult> {
  const context = auth.requireContext(), lease = keys.leaseForAccount(scope.accountId)
  const check = () => {
    if (signal.aborted || !current() || !auth.isCurrent(context) || !lease?.isCurrent()
      || context.userId !== scope.canonicalUserId || lease.localAccountId !== scope.accountId
      || lease.canonicalUserId !== scope.canonicalUserId || lease.authEpoch !== context.authEpoch) fail('session_closed')
  }
  check()
  const account = await bindings.ensureForCurrentUser(scope.accountId); check()
  if (account.context.userId !== scope.canonicalUserId || account.context.authEpoch !== context.authEpoch) fail('session_closed')
  let snapshot = await port.read(); check()
  const manifest = snapshot.manifest
  if (!manifest) throw new BrowserReplayError('source_seed_ineligible')
  if (!manifest.anchor.registry.projects.some(project => project.project_id === projectId)) fail('source_seed_ineligible')
  const anchor = manifest.anchor
  const server = async () => {
    check()
    const registry = await auth.authorized(token => cloudProjectsApi.listBootstraps(token, signal)); check()
    const mode = await auth.authorized(token => encryptedSyncV2Api.capabilities(token, signal)); check()
    if (!replaySame(replayAnchor(registry.value, mode.value), anchor)) fail('server_changed')
    const tail = await auth.authorized(token => delivery === 'recovery'
      ? encryptedSyncV3Api.recoveryRaw(token, anchor.registry.current_cursor, 1, anchor.registry.current_cursor, anchor.transport.cutover_epoch, signal)
      : encryptedSyncV3Api.pullRaw(token, scope.deviceId, anchor.registry.current_cursor, 1, signal)); check()
    const { page } = prepareReplayPage(tail.value, anchor.registry.current_cursor, scope, anchor)
    if (page.items.length || page.has_more) fail('server_changed')
  }
  const prepared = await lease!.use(async amk => {
    const noteSequences = snapshot.rows.some(row => ['note_proven', 'content_note_proven', 'content_note_conflict_preserved', 'note_resolution_proven', 'content_note_self_echo_proven'].includes(row.state))
      ? await reauthenticateReplayNotes(scope, snapshot, amk, () => { try { check(); return true } catch { return false } }, id => port.readSource(snapshot, id))
      : new Set<number>()
    await reauthenticateReplayOwners(scope, snapshot, amk, () => { try { check(); return true } catch { return false } }, true, noteSequences)
    for (const previous of snapshot.manifest!.source_seeds ?? []) {
      const evidence = await authenticateReplaySourceSeed(scope, snapshot, previous.project_id, amk, () => { try { check(); return true } catch { return false } })
      const row = snapshot.sources?.find(source => source.project_id === previous.project_id)
      if (!row || !replaySame(previous, evidence.reference)) fail('source_seed_reauthentication_failed')
      const project = row!.source_revision === 1 ? await openEmptyLocalProject(scope, row, amk) : (await port.readSource(snapshot, previous.project_id)).project
      if (!replaySame(project, evidence.project)) fail('source_seed_reauthentication_failed')
    }
    const evidence = await authenticateReplaySourceSeed(scope, snapshot, projectId, amk, () => { try { check(); return true } catch { return false } })
    if (snapshot.manifest!.source_seeds?.some(seed => seed.project_id === projectId)) return { evidence, row: null }
    return { evidence, row: await sealEmptyLocalProject(scope, evidence.project, amk) }
  })
  check(); await server(); check()
  if (!replaySame(await port.read(), snapshot)) fail('source_seed_inventory_changed')
  if (prepared.row) {
    await port.commit(snapshot, prepared.row, prepared.evidence.reference, server); check()
    snapshot = await port.read(); check()
  }
  await lease!.use(async amk => {
    const noteSequences = snapshot.rows.some(row => ['note_proven', 'content_note_proven', 'content_note_conflict_preserved', 'note_resolution_proven', 'content_note_self_echo_proven'].includes(row.state))
      ? await reauthenticateReplayNotes(scope, snapshot, amk, () => { try { check(); return true } catch { return false } }, id => port.readSource(snapshot, id))
      : new Set<number>()
    await reauthenticateReplayOwners(scope, snapshot, amk, () => { try { check(); return true } catch { return false } }, true, noteSequences)
    const evidence = await authenticateReplaySourceSeed(scope, snapshot, projectId, amk, () => { try { check(); return true } catch { return false } })
    const seed = snapshot.manifest?.source_seeds?.find(value => value.project_id === projectId)
    const row = snapshot.sources?.find(value => value.project_id === projectId)
    if (!seed || !row || !replaySame(seed, evidence.reference)) fail('source_seed_reauthentication_failed')
    const project = row!.source_revision === 1 ? await openEmptyLocalProject(scope, row, amk) : (await port.readSource(snapshot, projectId)).project
    if (!replaySame(project, evidence.project)) fail('source_seed_reauthentication_failed')
  })
  await server(); check()
  return { kind: 'encrypted_note_source_seed', status: prepared.row ? 'seeded' : 'already_seeded',
    projectOpened: false, ready: false, declarationAllowed: false, writerAuthorized: false }
}
