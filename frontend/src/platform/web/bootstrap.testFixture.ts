import { BrowserProvenPrefix } from './provenPrefix'
import { BrowserNoteIntents } from './noteIntents'
import { BrowserLegacyNoteHistory } from './noteHistory'
import { BrowserLegacyNoteApply } from './legacyNoteApply'
// Shared scheduling-contract fixture only. Real IDB/HTTP is validated in Chromium.
import { afterAll, beforeAll, vi } from 'vitest'
import { generateAccountMasterKey, type AccountMasterKey } from '@/crypto'
import { NormalUserAuthRuntime } from '@/auth/userAuth'
import { BrowserPersistenceError } from './persistence'
import { BrowserLocalAuthority } from './localAuthority'
import { BrowserProjectBindings } from './projectBindings'
import { BrowserSyncDurability, type BrowserSyncTransaction } from './syncDurability'
import { BrowserInitialNoteCapture } from './initialNoteCapture'
import { BrowserAccountRegistry } from './accountRegistry'
import { BrowserBootstrapCompletion } from './bootstrapCompletion'
import type { NativeLegacyNote, NativeLocalProject } from './localAuthorityRecords'
export const scope = { canonicalUserId: '11111111-1111-4111-8111-111111111111', accountId: '22222222-2222-4222-8222-222222222222', deviceId: '33333333-3333-4333-8333-333333333333' }
export const at = '2026-10-13T00:00:00.000000Z'
export const project = (id = 'p'): NativeLocalProject => ({ id, name: 'PRIVATE_CAPTURE_PROJECT', goal: null, infinite: true, unit: 'symbols', status: 'active', created_at: null, updated_at: null, payload: {} })
export const note = (id: string): NativeLegacyNote => ({ id, project_id: 'p', stage_id: null, source_type: 'project', source_map_id: null, source_node_id: null, content_format: 'html', title: 'PRIVATE_CAPTURE_TITLE', content: '<b>PRIVATE_CAPTURE_BODY</b>', checklist: [], color: 'default', pinned: false, archived: false, sort_order: 0, tags: [], created_at: at, updated_at: at, metadata: { retain: [null, true] }, revision: 5 })
export let amk: AccountMasterKey
beforeAll(async () => { amk = await generateAccountMasterKey(); vi.stubGlobal('IDBKeyRange', { bound: (lower: unknown[]) => ({ lower }) }) })
afterAll(() => { amk.fill(0); vi.unstubAllGlobals() })
export async function fixture(ids = ['z','a']) {
  const auth = new NormalUserAuthRuntime({ login: async () => ({ access_token: 'fixture-token', refresh_token: 'fixture-refresh', access_expires_in: 60 }), logout: async () => {}, refresh: vi.fn(), me: async () => ({ id: scope.canonicalUserId, username: 'fixture', email: 'fixture@example.test', email_verified: true, role: 'user', status: 'active', created_at: at }) }); await auth.login('fixture','fixture')
  const context = auth.requireContext(); let missingKey=false; let current = true, abortWrite = false, closed = false
  const db: Record<string, unknown[]> & {outbox: unknown[];inbox: unknown[];receipts: unknown[];projectBindings: unknown[];cursors: unknown[]} = { localProjects: [], localNotes: [], outbox: [], inbox: [], receipts: [], projectBindings: [], cursors: [], ownerHeads: [], legacyNoteHistory: [], legacyNoteIntents: [] }
  const calls: { mode: string; stores: string[] }[] = [], copies: Uint8Array[] = []
  const key = (name: string, value: unknown) => { const r = value as Record<string,unknown>; return name === 'ownerHeads' ? [r.account_id,r.owner_kind,r.owner_id] : name === 'legacyNoteHistory' ? [r.account_id,r.project_id,r.note_id,r.record_kind,r.record_id] : name === 'localNotes' ? [r.account_id,r.project_id,r.note_id] : ['outbox','receipts','legacyNoteIntents'].includes(name) ? [r.account_id,r.event_id] : name === 'inbox' ? [r.account_id,r.server_sequence] : name === 'cursors' ? [r.account_id,r.device_id] : [r.account_id,r.project_id] }
  const eq = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b)
  const transaction: BrowserSyncTransaction = (s, stores, mode, schedule) => new Promise((resolve,reject) => {
    calls.push({ mode, stores }); if (closed) { reject(new BrowserPersistenceError('closed')); return }; if (s.accountId !== scope.accountId || s.canonicalUserId !== scope.canonicalUserId || s.deviceId !== scope.deviceId) { reject(new BrowserPersistenceError('account_mismatch')); return }
    const pending = structuredClone(db), requests: {result: unknown;onsuccess: (() => void) | null}[] = []; let result: unknown, error: unknown
    const read = (value: unknown) => { const r = { result: structuredClone(value), onsuccess: null as (() => void) | null }; requests.push(r); return r }
    const select = (name: string, range: {lower: unknown[]}) => pending[name]!.filter(v => range.lower.every((part,i) => key(name,v)[i] === part))
    const store = (name: string) => {
      const write = (row: unknown, add: boolean) => { const index = pending[name]!.findIndex(v => eq(key(name,v),key(name,row))); if (index >= 0 && add) { error = new BrowserPersistenceError('conflicting_record'); return read(undefined) }; if (index >= 0) pending[name]![index] = structuredClone(row); else pending[name]!.push(structuredClone(row)); return read(undefined) }
      const openCursor = (range: {lower: unknown[]}) => {
        const values=select(name,range);let position=0
        const request=read(undefined)
        const load=()=>{request.result=position<values.length?{value:structuredClone(values[position]),continue:()=>{position++;requests.push({result:undefined,onsuccess:()=>{load();request.onsuccess?.()}})}}:null}
        load();return request
      }
      return { openCursor, delete: (k: unknown[]) => {pending[name]=pending[name]!.filter(v=>!eq(key(name,v),k));return read(undefined)}, get: (k: unknown[]) => read(pending[name]!.find(v => eq(key(name,v),k))), getAll: (r: {lower: unknown[]}) => read(select(name,r)), count: (r: {lower: unknown[]}) => read(select(name,r).length), add: (r: unknown) => write(r,true), put: (r: unknown) => write(r,false), index: (index: string) => ({ get: (k: unknown[]) => read(pending[name]!.find(v => { const r = v as Record<string,unknown>; return eq([r.account_id,index === 'identity' ? r.note_id : index === 'bootstrap' ? r.bootstrap_id : r.event_id],k) })) }) }
    }
    // Scheduling-contract driver only; native IDB isolation/constraint/rollback is proven in Chromium.
    schedule({ objectStore: store } as unknown as IDBTransaction, value => { result = value }, e => { error = e })
    for (let i = 0; i < requests.length && !error; i++) requests[i]!.onsuccess?.()
    if (abortWrite && mode === 'readwrite') error = new BrowserPersistenceError('storage_failed')
    if (error) reject(error); else { Object.assign(db,pending); resolve(result as never) }
  })
  const lease = { localAccountId: scope.accountId, canonicalUserId: scope.canonicalUserId, authEpoch: context.authEpoch, isCurrent: () => current && auth.isCurrent(context), use: async <T>(action: (key: AccountMasterKey) => Promise<T>) => { const key = Uint8Array.from(amk) as AccountMasterKey; copies.push(key); try { return await action(key) } finally { key.fill(0) } } }
  const invalidations=new Set<()=>void>()
  const bindings = { ensureForCurrentUser: async () => ({ context }) }, keys = { leaseForAccount: () => missingKey?null:lease, onInvalidated: (listener:()=>void) => { invalidations.add(listener);return ()=>{invalidations.delete(listener)} } }
  const localOwner = new BrowserLocalAuthority(transaction), local = localOwner.bind(auth,bindings as never,keys as never), projects = new BrowserProjectBindings(transaction), sync = new BrowserSyncDurability(transaction)
  const capture = new BrowserInitialNoteCapture(transaction,localOwner,projects).bind(auth,bindings as never,keys as never)
  await local.initializeProject(scope,{ project: project(), notes: ids.map(id => ({ state: 'live', note: note(id) })) })
  const prepared = await projects.prepare(scope,{ project_id: 'p', mode: 'upload_existing' }), registered = await projects.confirmRegistration(scope,prepared,{ remote_state: 'initializing', remote_high_water: 0 })
  const noteHistory = new BrowserLegacyNoteHistory(transaction,()=>!closed).bind(auth,bindings as never,keys as never)
  const notes = new BrowserLegacyNoteApply(transaction,localOwner,()=>!closed).bind(auth,bindings as never,keys as never)
  const registry = new BrowserAccountRegistry(transaction, () => !closed).bind(auth,bindings as never,keys as never)
  const completion = new BrowserBootstrapCompletion(transaction, () => !closed).bind(auth,bindings as never,keys as never)
  const proven=new BrowserProvenPrefix(transaction,()=>!closed).bind(auth,bindings as never,keys as never)
  const writer=new BrowserNoteIntents(transaction,localOwner,()=>!closed).bind(auth,bindings as never,keys as never)
  const mutate = async () => local.updateNote(scope,(await local.readSnapshot(scope,'p')).token,'a',{ title: 'LATER_PRIVATE' },at)
  const accept = async (row: unknown, sequence: number) => sync.commitUploadAcceptance(scope,row as never,{ protocol_version: 2, encrypted_sync_version: 2, current_cursor: sequence, results: [{ event_id: (row as {event_id: string}).event_id, server_sequence: sequence, duplicate: false }] })
  return { bindings, keys, proven,writer,noteHistory,notes,registry,completion,db,local,localOwner,projects,sync,capture,prepared,registered,mutate,accept,calls,copies,auth, transaction, unavailable: () => {missingKey=true}, lock: () => { current = false;for(const listener of invalidations)listener() }, abort: (value = true) => { abortWrite = value }, close: () => { closed = true } }
}
