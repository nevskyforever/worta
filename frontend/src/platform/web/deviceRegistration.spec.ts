import { describe, it, expect, vi, afterEach } from 'vitest'
import { syncApi } from '@/api/sync'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api } from '@/api/encryptedSyncV3'
import { NormalUserAuthRuntime } from '@/auth/userAuth'
import { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import { BrowserDeviceRegistration, currentBrowserDevice, currentRegisteredBrowserDevice, validateBrowserDeviceRegistration } from './deviceRegistration'
import { BrowserPersistenceError } from './persistence'
import type { BrowserSyncTransaction } from './syncDurability'
const scope = { canonicalUserId: '11111111-1111-4111-8111-111111111111', accountId: '22222222-2222-4222-8222-222222222222', deviceId: '33333333-3333-4333-8333-333333333333' }
const reply = () => ({ protocol_version: 1 as const, device_id: scope.deviceId, last_ack_cursor: 19 })
afterEach(() => vi.restoreAllMocks())
async function fixture() {
  let user = scope.canonicalUserId
  const auth = new NormalUserAuthRuntime({ login: async () => ({ access_token: 'fixture-access', refresh_token: 'fixture-refresh', access_expires_in: 60 }), logout: async () => {}, refresh: async () => { throw Error('fixture') }, me: async () => ({ id: user, username: 'fixture', email: 'fixture@example.test', email_verified: true, role: 'user', status: 'active', created_at: '2026-10-14T00:00:00Z' }) })
  await auth.login('fixture', 'fixture')
  let closed = false, corruptDevice = false
  const reads: unknown[] = []
  const transaction: BrowserSyncTransaction = async (s, stores, mode, schedule) => {
    // Scheduling contract only; real persistent identity checks are tested in Chromium.
    reads.push({ scope: structuredClone(s), stores, mode })
    if (closed) throw new BrowserPersistenceError('closed')
    if (corruptDevice || JSON.stringify(s) !== JSON.stringify(scope)) throw new BrowserPersistenceError('account_mismatch')
    return new Promise((resolve, reject) => schedule({} as IDBTransaction, resolve, reject))
  }
  const bindings = new AuthoritativeAccountBinding(auth, { ensure: async (account, user) => {
    if (account !== scope.accountId || user !== scope.canonicalUserId) throw new BrowserPersistenceError('account_mismatch')
    return 'validated'
  } })
  const runtime = new BrowserDeviceRegistration(transaction, () => !closed).bind(auth, bindings)
  const http = vi.spyOn(syncApi, 'registerDevice').mockResolvedValue(reply())
  return { runtime, auth, reads, http, close: () => { closed = true }, corrupt: () => { corruptDevice = true }, switchUser: async () => { await auth.logout(); user = '44444444-4444-4444-8444-444444444444'; await auth.login('fixture', 'fixture') } }
}
describe('C21.1.19 identity-only browser registration', () => {
  it('pins a recovery identity to the authenticated snapshot without registering or permitting declaration', async () => {
    const f = await fixture(), recovery = vi.spyOn(encryptedSyncV3Api, 'recoveryRaw').mockResolvedValue(JSON.stringify({ items: [], next_cursor: 4, has_more: false }))
    const token = await f.runtime.verifyRecovery(scope, 4, 2, new AbortController().signal)
    expect(recovery).toHaveBeenCalledOnce(); expect(f.http).not.toHaveBeenCalled()
    expect(currentBrowserDevice(token, scope, f.auth)).toBe(true)
    expect(currentRegisteredBrowserDevice(token, scope, f.auth)).toBe(false)
    await f.auth.logout(); expect(currentBrowserDevice(token, scope, f.auth)).toBe(false)
  })
  it('registers exact durable device and validates ownership twice without writes', async () => {
    const f = await fixture()
    expect(await f.runtime.registerOnce(scope)).toEqual({ deviceId: scope.deviceId, serverAckCursor: 19, ready: false })
    expect(f.http.mock.calls).toEqual([['fixture-access', scope.deviceId]])
    expect(f.reads).toEqual(Array.from({ length: 2 }, () => ({ scope, stores: [], mode: 'readonly' })))
  })
  it('replays exact UUID; server ACK metadata never becomes a local write', async () => {
    const f = await fixture(); await f.runtime.registerOnce(scope); await f.runtime.registerOnce(scope)
    expect(f.http.mock.calls).toEqual([['fixture-access', scope.deviceId], ['fixture-access', scope.deviceId]])
    expect(f.reads).toHaveLength(4)
  })
  it.each(['accountId', 'canonicalUserId', 'deviceId'] as const)('rejects wrong %s before HTTP', async key => {
    const f = await fixture()
    await expect(f.runtime.registerOnce({ ...scope, [key]: '44444444-4444-4444-8444-444444444444' })).rejects.toMatchObject({ code: 'account_mismatch' })
    expect(f.http).not.toHaveBeenCalled()
  })
  it('rejects closed session before binding/network', async () => {
    const f = await fixture(); f.close()
    await expect(f.runtime.registerOnce(scope)).rejects.toMatchObject({ code: 'closed' }); expect(f.http).not.toHaveBeenCalled()
  })
  it('rejects logged-out auth before network', async () => {
    const f = await fixture(); await f.auth.logout()
    await expect(f.runtime.registerOnce(scope)).rejects.toThrow(); expect(f.http).not.toHaveBeenCalled()
  })
  it('rejects logout during HTTP and requires explicit retry', async () => {
    const f = await fixture(); f.http.mockImplementationOnce(async () => { await f.auth.logout(); return reply() })
    await expect(f.runtime.registerOnce(scope)).rejects.toThrow(); expect(f.http).toHaveBeenCalledTimes(1); expect(f.reads).toHaveLength(1)
  })
  it('rejects account switch during response, even after login succeeds', async () => {
    const f = await fixture(); f.http.mockImplementationOnce(async () => { await f.switchUser(); return reply() })
    await expect(f.runtime.registerOnce(scope)).rejects.toThrow(); expect(f.reads).toHaveLength(1)
  })
  it('rejects close during HTTP', async () => {
    const f = await fixture(); f.http.mockImplementationOnce(async () => { f.close(); return reply() })
    await expect(f.runtime.registerOnce(scope)).rejects.toMatchObject({ code: 'closed' })
  })
  it('revalidates durable ownership after HTTP', async () => {
    const f = await fixture(); f.http.mockImplementationOnce(async () => { f.corrupt(); return reply() })
    await expect(f.runtime.registerOnce(scope)).rejects.toMatchObject({ code: 'account_mismatch' }); expect(f.reads).toHaveLength(2)
  })
  it('lost response retries exactly once per invocation, without local authority', async () => {
    const f = await fixture(); f.http.mockRejectedValueOnce(Error('lost_response'))
    await expect(f.runtime.registerOnce(scope)).rejects.toThrow('lost_response'); expect(f.reads).toHaveLength(1)
    await f.runtime.registerOnce(scope); expect(f.http).toHaveBeenCalledTimes(2)
  })
  it('snapshots caller scope before asynchronous work', async () => {
    const f = await fixture(), input = { ...scope }, flight = f.runtime.registerOnce(input)
    input.deviceId = '44444444-4444-4444-8444-444444444444'
    await flight; expect(f.http).toHaveBeenCalledWith('fixture-access', scope.deviceId)
  })
  it('does not advertise transport/family/compression capability or invoke ACK', async () => {
    const f = await fixture(), ack = vi.spyOn(syncApi, 'ack'), mode = vi.spyOn(encryptedSyncV2Api, 'capabilities'), cutover = vi.spyOn(encryptedSyncV2Api, 'cutover'), compression = vi.spyOn(encryptedSyncV3Api, 'compressionReaderCapabilities'), reader = vi.spyOn(encryptedSyncV3Api, 'noteReaderCapabilities')
    await f.runtime.registerOnce(scope)
    expect(f.http.mock.calls[0]).toHaveLength(2); expect(ack).not.toHaveBeenCalled(); expect(mode).not.toHaveBeenCalled(); expect(cutover).not.toHaveBeenCalled(); expect(compression).not.toHaveBeenCalled(); expect(reader).not.toHaveBeenCalled()
  })
  it.each([
    null, [], {}, { ...reply(), protocol_version: 2 }, { ...reply(), device_id: 'wrong' },
    { ...reply(), last_ack_cursor: -1 }, { ...reply(), last_ack_cursor: 0.5 },
    { ...reply(), last_ack_cursor: Number.MAX_SAFE_INTEGER + 1 }, { ...reply(), last_ack_cursor: '7' },
    { ...reply(), writer_transport_version: 3 }, { ...reply(), compression_id1: true },
    { ...reply(), compression_zero: true }, { ...reply(), reader_transport_version: 3 },
  ])('rejects malformed/future reply %#', value => {
    expect(() => validateBrowserDeviceRegistration(value, scope.deviceId)).toThrow('invalid_browser_device_registration')
  })
  it('accepts zero and maximum safe metadata cursors', () => {
    for (const cursor of [0, Number.MAX_SAFE_INTEGER]) expect(validateBrowserDeviceRegistration({ ...reply(), last_ack_cursor: cursor }, scope.deviceId).last_ack_cursor).toBe(cursor)
  })
})
