import { afterEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'

import { initializePlatformRuntime, isDesktopTestProfile } from './runtime'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

afterEach(() => {
  delete window.__TAURI_INTERNALS__
  delete window.__NFPROGRESS_RUNTIME__
  vi.useRealTimers()
  vi.restoreAllMocks()
})

describe('desktop test profile boundary', () => {
  it.each([
    { testProfile: true, expected: true },
    { testProfile: false, expected: false },
    { testProfile: undefined, expected: false },
  ])('recognizes only the explicit native test profile: $testProfile', async ({ testProfile, expected }) => {
    window.__TAURI_INTERNALS__ = {}
    vi.useFakeTimers()
    vi.mocked(invoke).mockResolvedValue({
      version: '1.0.0',
      nativeUpdates: false,
      architecture: 'aarch64',
      development: true,
      developerModeAvailable: true,
      testProfile,
    })

    await initializePlatformRuntime()

    expect(isDesktopTestProfile()).toBe(expected)
  })
})
