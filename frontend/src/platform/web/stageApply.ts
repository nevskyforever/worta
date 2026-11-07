import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp';
import type { NormalUserAuthRuntime } from '@/auth/userAuth';
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding';
import type { RuntimeKeyContext } from '@/auth/keyContext';
import type { AccountMasterKey } from '@/crypto';
import type { StructuralEvent } from '@/cloud/stageCodec';
import { decodeBase64Url } from '@/api/base64url';
import { BrowserPersistenceError } from './persistence';
import { openBrowserCanonicalObject } from './canonicalReaders';
import { prepareMetadataPrefix, verifiedMetadataSequences, type VerifiedMetadataPrefix } from './metadataPrefixProof';
import { validateBrowserMetadataHead } from './metadataApplyRecords';
import { stageOwnerId, validateStageProof, validateStageHead, type BrowserStageProof, type BrowserStageHead } from './stageRecords';
import { validateBrowserProjectBinding } from './projectBindings';
import { immutableWireIdentity, snapshotBrowserSyncScope, validateBrowserInboxRecord, readBrowserSyncCursor, type BrowserSyncScope, type BrowserSyncTransaction, type BrowserInboxRecord } from './syncDurability';
import { PREFIX_STORES, type BrowserPrefixInventory } from './provenPrefixRecords';
const same = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b);
function fail(code: 'closed' | 'corrupt_record' | 'stale_authority' | 'ineligible_lineage' | 'missing_history' | 'metadata_conflict' | 'self_echo_unavailable' | 'unsupported_local_source' | 'stage_tombstone_child_manifest_incomplete' = 'corrupt_record'): never {
  throw new BrowserPersistenceError(code);
}
export function readStageInventory(tx: IDBTransaction, scope: BrowserSyncScope, done: (v: BrowserPrefixInventory) => void, abort: (e: unknown) => void) {
  const names = ['inbox', 'outbox', 'receipts', 'projectBindings', 'ownerHeads', 'legacyNoteHistory'], reads = names.map(n => tx.objectStore(n).getAll(IDBKeyRange.bound([scope.accountId], [scope.accountId, []])));
  let count = 0;
  reads.forEach(r => r.onsuccess = () => {
    if (++count !== reads.length)
      return;
    readBrowserSyncCursor(tx, scope, c => {
      try {
        done({ rows: reads[0]!.result, out: reads[1]!.result, receipts: reads[2]!.result, bindings: reads[3]!.result, owners: reads[4]!.result, history: reads[5]!.result, cursor: { pull_cursor: c.pull_cursor, ack_cursor: c.ack_cursor, proven_prefix: c.proven_prefix, next_local_ordinal: c.next_local_ordinal } });
      }
      catch (e) {
        abort(e);
      }
    }, abort);
  });
}
export async function structuralWireDigest(row: BrowserInboxRecord) {
  const w = JSON.parse(row.wire_item), b = new TextEncoder().encode(immutableWireIdentity({ protocol_version: row.protocol_version, event: w.event, object: w.object }));
  try {
    return Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', b)), x => x.toString(16).padStart(2, '0')).join('');
  }
  finally {
    b.fill(0);
  }
}
async function open(scope: BrowserSyncScope, row: BrowserInboxRecord, amk: AccountMasterKey) {
  const w = JSON.parse(row.wire_item), nonce = decodeBase64Url(w.object.nonce), ciphertext = decodeBase64Url(w.object.ciphertext);
  try {
    return await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId: w.event.project_id, entityId: w.event.entity_id, entityType: 'stage', eventId: row.event_id }, { ...w.object, nonce, ciphertext }) as StructuralEvent;
  }
  finally {
    nonce.fill(0);
    ciphertext.fill(0);
  }
}
async function proofFor(scope: BrowserSyncScope, row: BrowserInboxRecord, e: StructuralEvent): Promise<BrowserStageProof> {
  const h = e.header, w = JSON.parse(row.wire_item).event;
  if (h.entity_type !== 'stage' || !['create', 'update', 'delete'].includes(h.operation))
    fail('unsupported_local_source');
  // Native receiver validation: a tombstone carries no value and equals its header time.
  if (h.operation === 'delete' && (e.stage !== null || e.deleted_at !== h.updated_at))
    fail();
  if (h.device_id === scope.deviceId)
    fail('self_echo_unavailable');
  if (h.account_id !== scope.canonicalUserId || h.project_id !== w.project_id || h.entity_id !== w.entity_id || h.device_id !== w.device_id || h.event_id !== row.event_id || h.revision !== w.revision || h.updated_at !== canonicalizeSyncTimestamp(w.updated_at) || (h.operation === 'delete' ? e.deleted_at !== canonicalizeSyncTimestamp(w.deleted_at) : e.deleted_at !== w.deleted_at) || w.operation !== (h.operation === 'delete' ? 'delete' : 'upsert'))
    fail();
  return validateStageProof({ record_version: 1, owner_kind: 'stage', account_id: scope.accountId, project_id: h.project_id, stage_id: h.entity_id, bootstrap_id: h.bootstrap_id, event_id: h.event_id, source_device_id: h.device_id, server_sequence: row.server_sequence, operation: h.operation, revision: h.revision, generation: h.generation, parent_event_ids: h.parent_event_ids, metadata_event_id: h.metadata_event_id, codec_version: e.version, protocol_version: 3, wire_digest: await structuralWireDigest(row), reference: w }, scope);
}
export function structuralMetadataDependency(scope: BrowserSyncScope, v: BrowserPrefixInventory, p: Pick<BrowserStageProof,'project_id'|'bootstrap_id'|'metadata_event_id'> & {stage_id:string}, cert: VerifiedMetadataPrefix, active = true, entityType:'stage'|'stage_order'='stage') {
  const b = v.bindings.map(r => validateBrowserProjectBinding(r, scope)).find(b => b.project_id === p.project_id);
  if (!b || b.mode !== 'import_remote' || b.phase !== 'captured' || b.remote_state !== 'active' || b.bootstrap_id !== p.bootstrap_id)
    fail('ineligible_lineage');
  const raw = v.owners.find(h => (h as {
    owner_kind: string;
    owner_id: string;
  }).owner_kind === 'project_metadata' && (h as {
    owner_id: string;
  }).owner_id === p.project_id);
  if (!raw)
    fail('missing_history');
  const head = validateBrowserMetadataHead(raw, scope), ref = head.history.find(r => r.event_id === p.metadata_event_id), tip = head.history.find(r => r.event_id === head.tip_event_ids[0]);
  if (head.bootstrap_id !== p.bootstrap_id || !ref || ref.operation === 'delete' || !verifiedMetadataSequences(cert, scope, v).has(ref.server_sequence) || active && (!tip || head.tip_event_ids.length !== 1 || tip.operation === 'delete'))
    fail('missing_history');
  if (v.out.some(r => JSON.parse((r as {
    raw_body: string;
  }).raw_body).items.some((i: {
    event: {
      entity_type: string;
      project_id: string;
      entity_id: string;
    };
  }) => i.event.entity_type === entityType && i.event.project_id === p.project_id && i.event.entity_id === p.stage_id)))
    fail('metadata_conflict');
}
export interface VerifiedStagePrefix {
  readonly verifiedStagePrefix: unique symbol;
}
const certificates = new WeakMap<object, {
  scope: string;
  anchors: string;
  safe: Set<number>;
  current: () => boolean;
}>();
export function stageSequences(token: VerifiedStagePrefix | undefined, scope: BrowserSyncScope, v: BrowserPrefixInventory): ReadonlySet<number> {
  if (!token)
    return new Set();
  const p = certificates.get(token);
  if (!p || !p.current() || p.scope !== immutableWireIdentity(scope) || p.anchors !== immutableWireIdentity({ ...v, cursor: undefined }))
    fail('stale_authority');
  return new Set(p.safe);
}
/** Crypto happens outside IDB. Certificates carry only exact nonsecret anchors and safe sequences. */
export async function prepareStagePrefix(scope: BrowserSyncScope, v: BrowserPrefixInventory, amk: AccountMasterKey, current: () => boolean, metadata: VerifiedMetadataPrefix, force = false): Promise<VerifiedStagePrefix> {
  const safe = new Set<number>();
  for (const raw of v.owners) {
    if ((raw as {
      owner_kind: string;
    }).owner_kind !== 'stage')
      continue;
    const h = validateStageHead(raw, scope), decrypt = force || h.history.some(p => p.server_sequence > v.cursor.proven_prefix);
    const meta = decrypt ? await prepareMetadataPrefix(scope, v, amk, current, true) : metadata;
    let valid = true;
    try {
      for (const p of h.history) {
        structuralMetadataDependency(scope, v, p, meta, decrypt);
        const row = v.rows.map(r => validateBrowserInboxRecord(r, scope)).find(r => r.event_id === p.event_id);
        if (!row || row.state !== 'stage_proven' || !same(row.stage_proof, p) || p.source_device_id === scope.deviceId || await structuralWireDigest(row) !== p.wire_digest)
          fail();
        if (decrypt && !same(await proofFor(scope, row, await open(scope, row, amk)), p))
          fail();
      }
    }
    catch {
      valid = false;
    }
    if (!current())
      fail('closed');
    if (valid)
      h.history.forEach(p => safe.add(p.server_sequence));
  }
  if (!current())
    fail('closed');
  const token = Object.freeze({}) as VerifiedStagePrefix;
  certificates.set(token, { scope: immutableWireIdentity(scope), anchors: immutableWireIdentity({ ...v, cursor: undefined }), safe, current });
  return token;
}
export interface VerifiedStageApply {
  readonly verifiedStageApply: unique symbol;
}
interface Plan {
  owner: BrowserStageRuntime;
  scope: BrowserSyncScope;
  inventory: BrowserPrefixInventory;
  head: BrowserStageHead;
  row: BrowserInboxRecord;
  proof: BrowserStageProof;
  replay: boolean;
  current: () => boolean;
}
const plans = new WeakMap<object, Plan>();
export class BrowserStageApply {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly sessionCurrent: () => boolean) {
  }
  bind(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    return new BrowserStageRuntime(this.transaction, this.sessionCurrent, auth, bindings, keys);
  }
}
export class BrowserStageRuntime {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly sessionCurrent: () => boolean, private readonly auth: NormalUserAuthRuntime, private readonly bindings: AuthoritativeAccountBinding, private readonly keys: RuntimeKeyContext) {
  }
  private async guard(scope: BrowserSyncScope) {
    const { context } = await this.bindings.ensureForCurrentUser(scope.accountId), lease = this.keys.leaseForAccount(scope.accountId), current = () => this.sessionCurrent() && this.auth.isCurrent(context) && !!lease?.isCurrent();
    if (!lease || context.userId !== scope.canonicalUserId || lease.localAccountId !== scope.accountId || lease.canonicalUserId !== scope.canonicalUserId || lease.authEpoch !== context.authEpoch || !current())
      fail('closed');
    return { lease, current };
  }
  private inventory(scope: BrowserSyncScope) {
    return this.transaction<BrowserPrefixInventory>(scope, PREFIX_STORES, 'readonly', (tx, result, abort) => readStageInventory(tx, scope, result, abort));
  }
  /** The plan contains proof metadata, never the decrypted Stage value or a key. */
  async prepare(input: BrowserSyncScope, sequence: number): Promise<VerifiedStageApply> {
    const scope = snapshotBrowserSyncScope(input), { lease, current } = await this.guard(scope), v = await this.inventory(scope), row = v.rows.map(r => validateBrowserInboxRecord(r, scope)).find(r => r.server_sequence === sequence);
    if (!row || sequence > v.cursor.pull_cursor || !['received', 'stage_proven'].includes(row.state))
      fail('missing_history');
    const incoming = row!;
    return lease.use(async (amk) => {
      const proof = await proofFor(scope, incoming, await open(scope, incoming, amk)), meta = await prepareMetadataPrefix(scope, v, amk, current, true);
      structuralMetadataDependency(scope, v, proof, meta, incoming.state !== 'stage_proven');
      const raw = v.owners.find(h => (h as {
        owner_kind: string;
        owner_id: string;
      }).owner_kind === 'stage' && (h as {
        owner_id: string;
      }).owner_id === stageOwnerId(proof.project_id, proof.stage_id)), old = raw ? validateStageHead(raw, scope) : null;
      const cert = await prepareStagePrefix(scope, v, amk, current, meta, true);
      if (old && old.history.some(p => !stageSequences(cert, scope, v).has(p.server_sequence)))
        fail();
      let head: BrowserStageHead, replay = false;
      if (incoming.state === 'stage_proven') {
        if (!old || !same(incoming.stage_proof, proof) || !old.history.some(p => same(p, proof)))
          fail();
        head = old!;
        replay = true;
      }
      else {
        if (old && old.history.find(p => p.event_id === old.tip_event_ids[0])?.operation === 'delete')
          fail('stage_tombstone_child_manifest_incomplete');
        if (old ? !same(proof.parent_event_ids, old.tip_event_ids) || proof.revision !== old.history.length + 1 || proof.generation !== old.history.length + 1 : proof.operation !== 'create')
          fail('metadata_conflict');
        head = validateStageHead({ record_version: 1, owner_kind: 'stage', owner_id: stageOwnerId(proof.project_id, proof.stage_id), account_id: scope.accountId, device_id: scope.deviceId, project_id: proof.project_id, stage_id: proof.stage_id, bootstrap_id: proof.bootstrap_id, tip_event_ids: [proof.event_id], history: [...(old?.history ?? []), proof].sort((a, b) => a.event_id.localeCompare(b.event_id)) }, scope);
      }
      if (!current())
        fail('closed');
      const token = Object.freeze({}) as VerifiedStageApply;
      plans.set(token, { owner: this, scope, inventory: v, head, row: incoming, proof, replay, current });
      return token;
    });
  }
  /** Recheck every durable dependency, then atomically write the head and inbox proof. */
  async commit(token: VerifiedStageApply): Promise<'applied' | 'already_applied'> {
    const p = plans.get(token);
    if (!p || p.owner !== this)
      fail();
    const plan = p!;
    if (!plan.current())
      fail('closed');
    let unsub: (() => void) | undefined;
    return this.transaction<'applied' | 'already_applied'>(plan.scope, PREFIX_STORES, 'readwrite', (tx, result, abort) => {
      unsub = this.auth.onInvalidated(() => abort(new BrowserPersistenceError('closed')));
      readStageInventory(tx, plan.scope, v => {
        if (!plan.current())
          fail('closed');
        if (!same(v, plan.inventory))
          fail('stale_authority');
        if (plan.replay) {
          result('already_applied');
          return;
        }
        const writes = [tx.objectStore('ownerHeads').put(plan.head), tx.objectStore('inbox').put({ ...plan.row, record_version: 5, state: 'stage_proven', stage_proof: plan.proof })];
        writes.forEach(w => w.onsuccess = () => {
          if (!plan.current())
            abort(new BrowserPersistenceError('closed'));
        });
        result('applied');
      }, abort);
    }).finally(() => unsub?.());
  }
  async apply(scope: BrowserSyncScope, sequence: number) {
    return this.commit(await this.prepare(scope, sequence));
  }
  /** Recover the authenticated winning value in memory after unlock; no plaintext projection. */
  async read(input: BrowserSyncScope, project: string, stage: string) {
    const scope = snapshotBrowserSyncScope(input), { lease, current } = await this.guard(scope), v = await this.inventory(scope), raw = v.owners.find(h => (h as {
      owner_kind: string;
      owner_id: string;
    }).owner_kind === 'stage' && (h as {
      owner_id: string;
    }).owner_id === stageOwnerId(project, stage));
    if (!raw)
      return null;
    const h = validateStageHead(raw, scope);
    return lease.use(async (amk) => {
      const meta = await prepareMetadataPrefix(scope, v, amk, current, true), cert = await prepareStagePrefix(scope, v, amk, current, meta, true);
      if (h.history.some(p => !stageSequences(cert, scope, v).has(p.server_sequence)))
        fail();
      const row = v.rows.map(r => validateBrowserInboxRecord(r, scope)).find(r => r.event_id === h.tip_event_ids[0]);
      if (!row)
        fail();
      const event = await open(scope, row!, amk);
      if (!current())
        fail('closed');
      return event.stage;
    });
  }
}
