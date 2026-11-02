import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'

vi.mock('@/platform/runtime', () => ({ currentPlatform: () => 'tauri' }))

import { desktopCloudEnabled } from '@/platform/desktopCloud'
import { configureCloudSessionRuntimeFactoryForTests, useCloudSessionStore } from './cloudSession'

describe('local desktop release', () => {
  beforeEach(() => setActivePinia(createPinia()))

  it('does not initialize a cloud runtime or allow account login', async () => {
    const factory = vi.fn()
    configureCloudSessionRuntimeFactoryForTests(factory)
    const session = useCloudSessionStore()
    expect(desktopCloudEnabled()).toBe(false)
    session.initialize()
    expect(factory).not.toHaveBeenCalled()
    expect(session.supported).toBe(false)
    await expect(session.login('user', 'password')).rejects.toThrow('Desktop cloud session is unavailable.')
    expect(factory).not.toHaveBeenCalled()
    configureCloudSessionRuntimeFactoryForTests(null)
  })
})
