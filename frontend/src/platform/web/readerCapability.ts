import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import { encryptedSyncV2Api, parseV2Capabilities } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api } from '@/api/encryptedSyncV3'
import { BrowserPersistenceError } from './persistence'
import { snapshotBrowserSyncScope, immutableWireIdentity, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'
import { currentRegisteredBrowserDevice, type VerifiedBrowserDevice } from './deviceRegistration'
import { BROWSER_CANONICAL_READERS } from './canonicalReaders'
export interface VerifiedBrowserReader { readonly verifiedBrowserReader: unique symbol }
const readers = new WeakMap<object, { scope: BrowserSyncScope; auth: NormalUserAuthRuntime; current: () => boolean }>()
export function currentBrowserReader(token: VerifiedBrowserReader, scope: BrowserSyncScope, auth: NormalUserAuthRuntime): boolean {
  const p = readers.get(token)
  return !!p && p.auth === auth && p.current() && immutableWireIdentity(p.scope) === immutableWireIdentity(scope)
}
export class BrowserReaderCapability {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly sessionCurrent: () => boolean) {}
  bind(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding) { return new BrowserReaderCapabilityRuntime(this.transaction, this.sessionCurrent, auth, bindings) }
}
export class BrowserReaderCapabilityRuntime {
  constructor(private readonly transaction: BrowserSyncTransaction, private readonly sessionCurrent: () => boolean,
    private readonly auth: NormalUserAuthRuntime, private readonly bindings: AuthoritativeAccountBinding) {}
  async declare(input: BrowserSyncScope, device: VerifiedBrowserDevice): Promise<VerifiedBrowserReader> {
    const scope = snapshotBrowserSyncScope(input), { context } = await this.bindings.ensureForCurrentUser(scope.accountId)
    const current = () => this.sessionCurrent() && this.auth.isCurrent(context) && currentRegisteredBrowserDevice(device, scope, this.auth)
    const check = () => { if (!current() || context.userId !== scope.canonicalUserId) throw new BrowserPersistenceError('closed') }
    const ownership = () => this.transaction<void>(scope, [], 'readonly', (_tx, result, abort) => { try { check(); result(undefined) } catch(e) { abort(e) } })
    if (BROWSER_CANONICAL_READERS.length !== 13) throw new TypeError('browser_readers_incomplete')
    await ownership(); check()
    const mode = await this.auth.authorized(token => { check(); return encryptedSyncV2Api.capabilities(token) })
    check(); parseV2Capabilities(mode.value)
    const response = await this.auth.authorized(token => { check(); return encryptedSyncV3Api.compressionReaderCapabilities(token, scope.deviceId) })
    check(); if (response.value !== undefined) throw new TypeError('browser_reader_response_rejected')
    await ownership(); check()
    const token = Object.freeze({}) as VerifiedBrowserReader
    readers.set(token, { scope, auth: this.auth, current })
    return token
  }
}
