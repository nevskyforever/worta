import { beforeEach, describe, expect, it, vi } from 'vitest'
import { adminApi, adminSession, onAdminSessionInvalidated } from './admin'

function inMemoryStorageStub() {
  return {
    getItem: vi.fn(() => null),
    setItem: vi.fn(),
    removeItem: vi.fn(),
    clear: vi.fn(),
  }
}

describe('admin API memory session', () => {
  beforeEach(() => { vi.unstubAllGlobals(); adminSession.clear(); vi.restoreAllMocks() })

  it('uses bearer authentication without browser token storage', async () => {
    const localStorage = inMemoryStorageStub()
    const sessionStorage = inMemoryStorageStub()
    vi.stubGlobal('localStorage', localStorage)
    vi.stubGlobal('sessionStorage', sessionStorage)
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(JSON.stringify({
      access_token: 'access-one', refresh_token: 'refresh-one', access_expires_in: 60,
    }), { status: 200 })).mockResolvedValueOnce(new Response(JSON.stringify({
      id: '1', username: 'Admin', email: 'admin@example.test', email_verified: true,
      role: 'admin', status: 'active', created_at: '2026-01-01T00:00:00Z',
    }), { status: 200 })).mockResolvedValueOnce(new Response(JSON.stringify({ users: [], total: 0, limit: 50, offset: 0 }), { status: 200 }))
    await adminSession.login('Admin', 'password')
    await adminApi.users()
    expect(fetchMock.mock.calls[2]?.[1]).toMatchObject({ headers: expect.any(Headers) })
    expect((fetchMock.mock.calls[2]?.[1] as RequestInit).headers as Headers).toHaveProperty('get')
    expect(((fetchMock.mock.calls[2]?.[1] as RequestInit).headers as Headers).get('Authorization')).toBe('Bearer access-one')
    expect(localStorage.getItem).not.toHaveBeenCalled()
    expect(localStorage.setItem).not.toHaveBeenCalled()
    expect(sessionStorage.getItem).not.toHaveBeenCalled()
    expect(sessionStorage.setItem).not.toHaveBeenCalled()
  })

  it('replaces both runtime tokens during refresh and clears on logout', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(JSON.stringify({ access_token: 'a', refresh_token: 'r', access_expires_in: 60 }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ id: '1', username: 'Admin', email: 'a@b.test', email_verified: true, role: 'admin', status: 'active', created_at: '2026-01-01T00:00:00Z' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ access_token: 'new-a', refresh_token: 'new-r', access_expires_in: 60 }), { status: 200 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
    await adminSession.login('Admin', 'password'); await adminSession.refresh(); await adminSession.logout()
    expect(adminSession.active()).toBe(false)
  })

  it('refreshes once and retries an invalid access request with rotated tokens', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ access_token: 'old-a', refresh_token: 'old-r', access_expires_in: 60 }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ id: '1', username: 'Admin', email: 'a@b.test', email_verified: true, role: 'admin', status: 'active', created_at: '2026-01-01T00:00:00Z' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ detail: { code: 'invalid_token', message: 'Invalid authentication token.' } }), { status: 401 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ access_token: 'new-a', refresh_token: 'new-r', access_expires_in: 60 }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ users: [], total: 0, limit: 50, offset: 0 }), { status: 200 }))
    await adminSession.login('Admin', 'password'); await adminApi.users()
    expect(fetchMock).toHaveBeenCalledTimes(5)
    expect(((fetchMock.mock.calls[4]?.[1] as RequestInit).headers as Headers).get('Authorization')).toBe('Bearer new-a')
  })

  it('clears and signals an invalidated session when refresh fails', async () => {
    const invalidated = vi.fn(); onAdminSessionInvalidated(invalidated)
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(JSON.stringify({ access_token: 'a', refresh_token: 'r', access_expires_in: 60 }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ id: '1', username: 'Admin', email: 'a@b.test', email_verified: true, role: 'admin', status: 'active', created_at: '2026-01-01T00:00:00Z' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ detail: { code: 'invalid_token', message: 'Invalid authentication token.' } }), { status: 401 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ detail: { code: 'invalid_credentials', message: 'Invalid credentials.' } }), { status: 401 }))
    await adminSession.login('Admin', 'password')
    await expect(adminApi.users()).rejects.toBeInstanceOf(Error)
    expect(adminSession.active()).toBe(false); expect(invalidated).toHaveBeenCalledOnce()
  })

  it('clears tokens when account inspection denies a normal user', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(JSON.stringify({ access_token: 'a', refresh_token: 'r', access_expires_in: 60 }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ id: '1', username: 'User', email: 'u@b.test', email_verified: true, role: 'user', status: 'active', created_at: '2026-01-01T00:00:00Z' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
    await expect(adminSession.login('User', 'password')).rejects.toThrow('нет доступа')
    expect(adminSession.active()).toBe(false)
  })
})
