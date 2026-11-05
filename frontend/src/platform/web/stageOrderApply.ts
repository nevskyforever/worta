import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp';
import type { NormalUserAuthRuntime } from '@/auth/userAuth';
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding';
import type { RuntimeKeyContext } from '@/auth/keyContext';
import type { AccountMasterKey } from '@/crypto';
import type { StructuralEvent } from '@/cloud/stageCodec';
import { decodeBase64Url } from '@/api/base64url';
import { BrowserPersistenceError, type BrowserPersistenceCode } from './persistence';
import { openBrowserCanonicalObject } from './canonicalReaders';
import { prepareMetadataPrefix, type VerifiedMetadataPrefix } from './metadataPrefixProof';
import { readStageInventory, structuralWireDigest, structuralMetadataDependency, prepareStagePrefix, stageSequences } from './stageApply';
import { validateStageHead } from './stageRecords';
import { validateStageOrderProof, validateStageOrderHead, proveStageReferenceCoverage, type BrowserStageOrderProof, type BrowserStageOrderHead } from './stageOrderRecords';
import { snapshotBrowserSyncScope, validateBrowserInboxRecord, immutableWireIdentity, type BrowserInboxRecord, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability';
import { PREFIX_STORES, type BrowserPrefixInventory } from './provenPrefixRecords';
type HeadIdentity = { owner_kind: string; owner_id: string; project_id: string; stage_id: string; history?: Array<{ event_id: string; operation: string }>; tip_event_ids?: string[] };
const same = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b);
function fail(code: BrowserPersistenceCode = 'corrupt_record'): never {
  throw new BrowserPersistenceError(code);
}
async function open(scope: BrowserSyncScope, row: BrowserInboxRecord, amk: AccountMasterKey) {
  const w = JSON.parse(row.wire_item), nonce = decodeBase64Url(w.object.nonce), ciphertext = decodeBase64Url(w.object.ciphertext);
  try {
    return await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId: w.event.project_id, entityId: 'stage_order', entityType: 'stage_order', eventId: row.event_id }, { ...w.object, nonce, ciphertext }) as StructuralEvent;
  }
  finally {
    nonce.fill(0);
    ciphertext.fill(0);
  }
}
async function proofFor(scope: BrowserSyncScope, row: BrowserInboxRecord, e: StructuralEvent): Promise<BrowserStageOrderProof> {
  const h = e.header, w = JSON.parse(row.wire_item).event;
  if (h.entity_type !== 'stage_order' || h.entity_id !== 'stage_order' || !['create', 'update'].includes(h.operation))
    fail('unsupported_local_source');
  if (h.device_id === scope.deviceId)
    fail('self_echo_unavailable');
  if (h.account_id !== scope.canonicalUserId || h.project_id !== w.project_id || h.entity_id !== w.entity_id || h.device_id !== w.device_id || h.event_id !== row.event_id || h.revision !== w.revision || h.updated_at !== canonicalizeSyncTimestamp(w.updated_at) || e.deleted_at !== w.deleted_at || w.operation !== 'upsert')
    fail();
  return validateStageOrderProof({ record_version: 1, owner_kind: 'stage_order', entity_id: 'stage_order', account_id: scope.accountId, project_id: h.project_id, bootstrap_id: h.bootstrap_id, event_id: h.event_id, source_device_id: h.device_id, server_sequence: row.server_sequence, operation: h.operation, revision: h.revision, generation: h.generation, parent_event_ids: h.parent_event_ids, metadata_event_id: h.metadata_event_id, codec_version: e.version, protocol_version: 3, wire_digest: await structuralWireDigest(row), reference: w, stage_dependencies: Object.keys(e.stage_heads!).sort().map(stage_id => ({ stage_id, event_ids: e.stage_heads![stage_id]! })) }, scope);
}
async function dependencies(scope: BrowserSyncScope, v: BrowserPrefixInventory, p: BrowserStageOrderProof, amk: AccountMasterKey, current: () => boolean, meta: VerifiedMetadataPrefix, fresh: boolean) {
  structuralMetadataDependency(scope, v, { ...p, stage_id: 'stage_order' }, meta, fresh, 'stage_order');
  // Historical orders cover the membership that existed at their server sequence.
  const raw = v.owners.filter(h => (h as HeadIdentity).owner_kind === 'stage' && (h as HeadIdentity).project_id === p.project_id)
    .filter(h => validateStageHead(h, scope).history.some(proof => proof.server_sequence <= p.server_sequence));
  for (const h of raw as HeadIdentity[])
    if (h.history?.some((r: { event_id: string; operation: string }) => h.tip_event_ids?.includes(r.event_id) && r.operation === 'delete'))
      fail('stage_tombstone_child_manifest_incomplete');
  const heads = raw.map(h => validateStageHead(h, scope));
  if (heads.some(h => h.bootstrap_id !== p.bootstrap_id))
    fail('ineligible_lineage');
  const stageCert = await prepareStagePrefix(scope, v, amk, current, meta, true), certified = stageSequences(stageCert, scope, v);
  for (const d of p.stage_dependencies) {
    const h = heads.find(h => h.stage_id === d.stage_id);
    if (!h) {
      if (v.owners.some(h => (h as HeadIdentity).owner_kind === 'stage' && (h as HeadIdentity).stage_id === d.stage_id))
        fail('stage_order_foreign_stage');
      fail('stage_dependency_missing');
    }
    if (h.history.some(r => !certified.has(r.server_sequence)))
      fail('stage_dependency_missing');
    if (!proveStageReferenceCoverage(h.tip_event_ids, d.event_ids, h.history))
      fail('stage_membership_head_changed');
  }
  if (fresh) {
    // Unapplied evidence cannot be silently omitted from authoritative membership.
    for (const raw of v.rows) {
      const r = validateBrowserInboxRecord(raw, scope), w = JSON.parse(r.wire_item).event;
      if (w.entity_type === 'stage' && w.project_id === p.project_id && r.server_sequence <= p.server_sequence && r.state !== 'stage_proven')
        fail(w.operation === 'delete' ? 'stage_tombstone_child_manifest_incomplete' : 'stage_dependency_missing');
    }
    if (!same(heads.map(h => h.stage_id).sort(), p.stage_dependencies.map(d => d.stage_id)))
      fail('stage_order_membership_mismatch');
  }
  if (!current())
    fail('closed');
}
export interface VerifiedStageOrderPrefix {
  readonly verifiedStageOrderPrefix: unique symbol;
}
const certificates = new WeakMap<object, {
  scope: string;
  anchors: string;
  safe: Set<number>;
  current: () => boolean;
}>();
export function stageOrderSequences(token: VerifiedStageOrderPrefix | undefined, scope: BrowserSyncScope, v: BrowserPrefixInventory): ReadonlySet<number> {
  if (!token)
    return new Set();
  const p = certificates.get(token);
  if (!p || !p.current() || p.scope !== immutableWireIdentity(scope) || p.anchors !== immutableWireIdentity({ ...v, cursor: undefined }))
    fail('stale_authority');
  return new Set(p.safe);
}
export async function prepareStageOrderPrefix(scope: BrowserSyncScope, v: BrowserPrefixInventory, amk: AccountMasterKey, current: () => boolean, meta: VerifiedMetadataPrefix, force = false, replayHistory = false): Promise<VerifiedStageOrderPrefix> {
  const safe = new Set<number>();
  for (const raw of v.owners) {
    if ((raw as HeadIdentity).owner_kind !== 'stage_order')
      continue;
    const h = validateStageOrderHead(raw, scope);
    for (const p of h.history) {
      let valid = true;
      try {
        const row = v.rows.map(r => validateBrowserInboxRecord(r, scope)).find(r => r.event_id === p.event_id);
        if (!row || row.state !== 'stage_order_proven' || !same(row.stage_order_proof, p) || p.source_device_id === scope.deviceId || await structuralWireDigest(row) !== p.wire_digest)
          fail();
        const fresh = !replayHistory && p.server_sequence > v.cursor.proven_prefix;
        if (force || fresh) {
          const verifiedMeta = await prepareMetadataPrefix(scope, v, amk, current, true);
          if (!same(await proofFor(scope, row, await open(scope, row, amk)), p))
            fail();
          await dependencies(scope, v, p, amk, current, verifiedMeta, fresh);
        }
        else
          structuralMetadataDependency(scope, v, { ...p, stage_id: 'stage_order' }, meta, false, 'stage_order');
      }
      catch {
        valid = false;
      }
      if (!current())
        fail('closed');
      if (valid)
        safe.add(p.server_sequence);
    }
  }
  if (!current())
    fail('closed');
  const token = Object.freeze({}) as VerifiedStageOrderPrefix;
  certificates.set(token, { scope: immutableWireIdentity(scope), anchors: immutableWireIdentity({ ...v, cursor: undefined }), safe, current });
  return token;
}
export interface VerifiedStageOrderApply {
  readonly verifiedStageOrderApply: unique symbol;
}
interface Plan {
  owner: BrowserStageOrderRuntime;
  scope: BrowserSyncScope;
  inventory: BrowserPrefixInventory;
  head: BrowserStageOrderHead;
  row: BrowserInboxRecord;
  proof: BrowserStageOrderProof;
  ordered: string[];
  replay: boolean;
  current: () => boolean;
}
const plans = new WeakMap<object, Plan>();
export class BrowserStageOrderApply {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly sessionCurrent: () => boolean) {
  }
  bind(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext) {
    return new BrowserStageOrderRuntime(this.transaction, this.sessionCurrent, auth, bindings, keys);
  }
}
export class BrowserStageOrderRuntime {
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
  async prepare(input: BrowserSyncScope, sequence: number): Promise<VerifiedStageOrderApply> {
    const scope = snapshotBrowserSyncScope(input), { lease, current } = await this.guard(scope), v = await this.inventory(scope), row = v.rows.map(r => validateBrowserInboxRecord(r, scope)).find(r => r.server_sequence === sequence);
    if (!row || sequence > v.cursor.pull_cursor || !['received', 'stage_order_proven'].includes(row.state))
      fail('missing_history');
    return lease.use(async (amk) => {
      const event = await open(scope, row, amk), proof = await proofFor(scope, row, event), meta = await prepareMetadataPrefix(scope, v, amk, current, true), replay = row.state === 'stage_order_proven';
      await dependencies(scope, v, proof, amk, current, meta, !replay);
      const raw = v.owners.find(h => (h as HeadIdentity).owner_kind === 'stage_order' && (h as HeadIdentity).owner_id === proof.project_id), old = raw ? validateStageOrderHead(raw, scope) : null;
      const cert = await prepareStageOrderPrefix(scope, v, amk, current, meta, true, true);
      if (old && old.history.some(p => !stageOrderSequences(cert, scope, v).has(p.server_sequence)))
        fail('missing_history');
      let head: BrowserStageOrderHead;
      if (replay) {
        if (!old || row.state !== 'stage_order_proven' || !same(row.stage_order_proof, proof) || !old.history.some(p => same(p, proof)))
          fail();
        head = old;
      }
      else {
        if (old ? !same(proof.parent_event_ids, old.tip_event_ids) || proof.revision !== old.history.length + 1 || proof.generation !== old.history.length + 1 : proof.operation !== 'create')
          fail('stage_order_conflict');
        head = validateStageOrderHead({ record_version: 1, owner_kind: 'stage_order', owner_id: proof.project_id, account_id: scope.accountId, device_id: scope.deviceId, project_id: proof.project_id, bootstrap_id: proof.bootstrap_id, tip_event_ids: [proof.event_id], history: [...(old?.history ?? []), proof].sort((a, b) => a.event_id.localeCompare(b.event_id)) }, scope);
      }
      if (!current())
        fail('closed');
      const token = Object.freeze({}) as VerifiedStageOrderApply;
      plans.set(token, { owner: this, scope, inventory: v, head, row, proof, ordered: [...event.stage_ids!], replay, current });
      return token;
    });
  }
  async commit(token: VerifiedStageOrderApply): Promise<'applied' | 'already_applied'> {
    const p = plans.get(token);
    if (!p || p.owner !== this)
      fail();
    if (!p.current())
      fail('closed');
    let unsub: (() => void) | undefined;
    return this.transaction<'applied' | 'already_applied'>(p.scope, PREFIX_STORES, 'readwrite', (tx, result, abort) => {
      unsub = this.auth.onInvalidated(() => abort(new BrowserPersistenceError('closed')));
      readStageInventory(tx, p.scope, v => {
        if (!p.current())
          fail('closed');
        if (!same(v, p.inventory))
          fail('stale_authority');
        if (p.replay) {
          result('already_applied');
          return;
        }
        const writes = [tx.objectStore('ownerHeads').put(p.head), tx.objectStore('inbox').put({ ...p.row, record_version: 6, state: 'stage_order_proven', stage_order_proof: p.proof })];
        writes.forEach(w => w.onsuccess = () => {
          if (!p.current())
            abort(new BrowserPersistenceError('closed'));
        });
        result('applied');
      }, abort);
    }).finally(() => unsub?.());
  }
  async apply(scope: BrowserSyncScope, sequence: number) {
    return this.commit(await this.prepare(scope, sequence));
  }
  async read(input: BrowserSyncScope, project: string): Promise<string[] | null> {
    const scope = snapshotBrowserSyncScope(input), { lease, current } = await this.guard(scope), v = await this.inventory(scope), raw = v.owners.find(h => (h as HeadIdentity).owner_kind === 'stage_order' && (h as HeadIdentity).owner_id === project);
    if (!raw)
      return null;
    const h = validateStageOrderHead(raw, scope);
    return lease.use(async (amk) => {
      const meta = await prepareMetadataPrefix(scope, v, amk, current, true), cert = await prepareStageOrderPrefix(scope, v, amk, current, meta, true);
      if (h.history.some(p => !stageOrderSequences(cert, scope, v).has(p.server_sequence)))
        fail();
      const row = v.rows.map(r => validateBrowserInboxRecord(r, scope)).find(r => r.event_id === h.tip_event_ids[0]);
      if (!row)
        fail();
      const event = await open(scope, row, amk);
      if (!current())
        fail('closed');
      return [...event.stage_ids!];
    });
  }
}
