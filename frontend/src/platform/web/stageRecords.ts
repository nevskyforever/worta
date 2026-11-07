import {validateStageOrderHead} from './stageOrderRecords';
import type { V3Descriptor } from '@/api/encryptedSyncV3';
import { validateBrowserMetadataHead, wireTimestamp } from './metadataApplyRecords';
import { immutableWireIdentity, type BrowserSyncScope } from './syncDurability';
import { BrowserPersistenceError } from './persistence';
export interface BrowserStageProof {
  record_version: 1;
  owner_kind: 'stage';
  account_id: string;
  project_id: string;
  stage_id: string;
  bootstrap_id: string;
  event_id: string;
  source_device_id: string;
  server_sequence: number;
  operation: 'create' | 'update' | 'delete';
  revision: number;
  generation: number;
  parent_event_ids: string[];
  metadata_event_id: string;
  codec_version: 1 | 2;
  protocol_version: 3;
  wire_digest: string;
  reference: V3Descriptor;
}
export interface BrowserStageHead {
  record_version: 1;
  owner_kind: 'stage';
  owner_id: string;
  account_id: string;
  device_id: string;
  project_id: string;
  stage_id: string;
  bootstrap_id: string;
  tip_event_ids: string[];
  history: BrowserStageProof[];
}
const bad = (): never => {
  throw new BrowserPersistenceError('corrupt_record');
};
const exact = (v: unknown, k: string[]): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v) && Object.keys(v).length === k.length && k.every(s => Object.hasOwn(v, s));
const uuid = (v: unknown): v is string => typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(v);
const id = (v: unknown): v is string => typeof v === 'string' && v.length > 0 && !/[\uD800-\uDFFF]/u.test(v) && new TextEncoder().encode(v).length <= 512;
const positive = (v: unknown) => Number.isSafeInteger(v) && (v as number) > 0;
const same = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b);
/** Canonical tuple preserves project scope even when Stage IDs are reused. */
export function stageOwnerId(project: string, stage: string): string {
  if (!id(project) || !id(stage))
    bad();
  return JSON.stringify([project, stage]);
}
export function validateStageProof(value: unknown, scope: BrowserSyncScope): BrowserStageProof {
  if (!exact(value, ['record_version', 'owner_kind', 'account_id', 'project_id', 'stage_id', 'bootstrap_id', 'event_id', 'source_device_id', 'server_sequence', 'operation', 'revision', 'generation', 'parent_event_ids', 'metadata_event_id', 'codec_version', 'protocol_version', 'wire_digest', 'reference']))
    bad();
  const p = value as unknown as BrowserStageProof, d = p.reference;
  if (p.record_version !== 1 || p.owner_kind !== 'stage' || p.account_id !== scope.accountId || !id(p.project_id) || !id(p.stage_id) || ![p.bootstrap_id, p.event_id, p.source_device_id, p.metadata_event_id].every(uuid) || !positive(p.server_sequence) || !positive(p.revision) || p.generation !== p.revision || p.protocol_version !== 3 || ![1, 2].includes(p.codec_version) || !['create', 'update', 'delete'].includes(p.operation) || p.codec_version === 2 && p.operation === 'create' || !Array.isArray(p.parent_event_ids) || p.parent_event_ids.length !== (p.operation === 'create' ? 0 : 1) || !p.parent_event_ids.every(s => uuid(s) && s !== p.event_id) || p.operation === 'create' && p.revision !== 1 || p.operation !== 'create' && p.revision < 2 || typeof p.wire_digest !== 'string' || !/^[0-9a-f]{64}$/.test(p.wire_digest))
    bad();
  if (!exact(d, ['event_id', 'device_id', 'server_sequence', 'project_id', 'entity_id', 'entity_type', 'operation', 'revision', 'updated_at', 'deleted_at']) || d.event_id !== p.event_id || d.device_id !== p.source_device_id || d.server_sequence !== p.server_sequence || d.project_id !== p.project_id || d.entity_id !== p.stage_id || d.entity_type !== 'stage' || d.revision !== p.revision || !wireTimestamp(d.updated_at))
    bad();
  // A tombstone descriptor is the delete itself; every other proof is an upsert with no deletion.
  if (p.operation === 'delete' ? d.operation !== 'delete' || !wireTimestamp(d.deleted_at) : d.operation !== 'upsert' || d.deleted_at !== null)
    bad();
  return structuredClone(p);
}
export function validateStageHead(value: unknown, scope: BrowserSyncScope): BrowserStageHead {
  if (!exact(value, ['record_version', 'owner_kind', 'owner_id', 'account_id', 'device_id', 'project_id', 'stage_id', 'bootstrap_id', 'tip_event_ids', 'history']))
    bad();
  const h = value as unknown as BrowserStageHead;
  if (h.record_version !== 1 || h.owner_kind !== 'stage' || h.account_id !== scope.accountId || h.device_id !== scope.deviceId || h.owner_id !== stageOwnerId(h.project_id, h.stage_id) || !uuid(h.bootstrap_id) || !Array.isArray(h.history) || h.history.length < 1 || h.history.length > 200 || !Array.isArray(h.tip_event_ids) || h.tip_event_ids.length !== 1)
    bad();
  const history = h.history.map(p => validateStageProof(p, scope));
  if (history.some((p, i) => p.project_id !== h.project_id || p.stage_id !== h.stage_id || p.bootstrap_id !== h.bootstrap_id || i > 0 && history[i - 1]!.event_id >= p.event_id))
    bad();
  const causal = [...history].sort((a, b) => a.revision - b.revision);
  if (causal.some((p, i) => p.revision !== i + 1 || p.generation !== i + 1 || i > 0 && p.server_sequence <= causal[i - 1]!.server_sequence || (i === 0 ? p.operation !== 'create' || p.parent_event_ids.length !== 0 : !same(p.parent_event_ids, [causal[i - 1]!.event_id]))) || h.tip_event_ids[0] !== causal.at(-1)!.event_id)
    bad();
  return { ...structuredClone(h), history };
}
export function validateBrowserOwnerHead(value: unknown, scope: BrowserSyncScope) {
  if((value as {owner_kind?:string})?.owner_kind==='stage_order')return validateStageOrderHead(value,scope);
  return (value as {
    owner_kind?: string;
  })?.owner_kind === 'stage' ? validateStageHead(value, scope) : validateBrowserMetadataHead(value, scope);
}
