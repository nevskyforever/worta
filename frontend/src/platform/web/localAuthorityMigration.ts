import { BrowserPersistenceError, validateBrowserV4ProjectState } from './persistence'
import { validateBrowserMetadataHead } from './metadataApplyRecords'
import { immutableWireIdentity, validateBrowserInboxRecord, validateBrowserRetainedHistory, validateBrowserV3SyncState, type BrowserSyncScope, type BrowserAppliedInboxRecord } from './syncDurability'
import { validateBrowserProjectBinding } from './projectBindings'

/** Structural validation only; preserve original v5 rows, including authenticated apply proofs.
 * No decryption or asynchronous crypto in the versionchange transaction; no authority inferred.
 */
export function validateBrowserV5State(scopes: BrowserSyncScope[], stores: unknown[][]): void {
  const bad = (): never => { throw new BrowserPersistenceError('corrupt_record') }
  if (stores.length !== 6) bad()
  const [outbound, inbound, cursors, receipts, bindings, heads] = stores as [unknown[],unknown[],unknown[],unknown[],unknown[],unknown[]]
  const scopeFor = (value: unknown) => scopes.find(s => s.accountId === (value as { account_id?: string })?.account_id) ?? bad()
  const inbox = inbound.map(value => validateBrowserInboxRecord(value, scopeFor(value)))
  if(inbox.some(row=>['note_proven','note_conflict_preserved','stage_proven','stage_order_proven','content_note_proven','content_note_conflict_preserved','content_note_self_echo_proven'].includes(row.state)))bad() // v5 never contained Note or Stage runtime outcomes.
  // Detached received views validate legacy transport invariants without erasing applied state/proof.
  validateBrowserV3SyncState(scopes, outbound, inbox.map(row => {
    if (row.state === 'received') return structuredClone(row)
    if (row.state === 'note_proven'||row.state==='note_conflict_preserved'||row.state==='stage_proven'||row.state==='stage_order_proven'||row.state==='content_note_proven'||row.state==='content_note_conflict_preserved'||row.state==='content_note_self_echo_proven'||row.state==='document_proven'||row.state==='map_proven') return bad()
    const { applied_proof: _proof, ...received } = row
    return { ...received, record_version: 1, state: 'received' }
  }), cursors, receipts)
  validateBrowserV4ProjectState(scopes, bindings)
  const seen = new Set<string>(), linked = new Set<string>()
  for (const value of heads) {
    const scope = scopeFor(value), head = validateBrowserMetadataHead(value, scope)
    const key = JSON.stringify([head.account_id,head.owner_kind,head.owner_id])
    if (seen.has(key)) bad()
    seen.add(key)
    const b = bindings.find(v => (v as {account_id: string;project_id: string}).account_id === scope.accountId && (v as {project_id: string}).project_id === head.owner_id)
    const binding = validateBrowserProjectBinding(b, scope)
    if (binding.mode !== 'import_remote' || binding.phase !== 'captured' || binding.remote_state !== 'active' || binding.bootstrap_id !== head.bootstrap_id) bad()
    for (const proof of head.history) {
      const incoming = inbox.find(row => row.account_id === scope.accountId && row.event_id === proof.event_id)
      if (incoming?.state !== 'applied' || immutableWireIdentity(incoming.applied_proof) !== immutableWireIdentity(proof)) bad()
      validateBrowserRetainedHistory(scope, proof.reference,
        outbound.find(v => (v as {account_id: string;event_id: string}).account_id === scope.accountId && (v as {event_id: string}).event_id === proof.event_id),
        receipts.find(v => (v as {account_id: string;event_id: string}).account_id === scope.accountId && (v as {event_id: string}).event_id === proof.event_id), incoming)
      linked.add(JSON.stringify([scope.accountId,proof.event_id]))
    }
  }
  for (const row of inbox.filter((r): r is BrowserAppliedInboxRecord => r.state === 'applied')) if (!linked.has(JSON.stringify([row.account_id,row.event_id]))) bad()
}
