import { afterEach, describe, expect, it, vi } from 'vitest'
import { Capacitor } from '@capacitor/core'
import { currentPlatform, initializePlatformRuntime, isDesktopTestProfile, isDeveloperModeAvailable, openExternalUrl, requestApplicationClose, requireDesktopRuntime, supportsUpdateChecks, syncNativeSystemBars } from './runtime'
import { diagnostics } from '@/diagnostics/service'
import { pickDesktopWordFile } from './files'

// Throwing factories prove Web does not even import native plugins.
vi.mock('@tauri-apps/api/core', () => { throw new Error('Unexpected native import') })
vi.mock('@tauri-apps/plugin-dialog', () => { throw new Error('Unexpected dialog import') })
vi.mock('@tauri-apps/plugin-opener', () => { throw new Error('Unexpected opener import') })
vi.mock('@tauri-apps/api/window', () => { throw new Error('Unexpected window import') })
vi.mock('@capacitor/status-bar', () => { throw new Error('Unexpected mobile import') })

afterEach(() => vi.restoreAllMocks())
describe('plain browser boundary', () => {
  it('initializes Web without native globals, commands or background timers', async () => {
    expect(window.__TAURI_INTERNALS__).toBeUndefined()
    expect(Capacitor.isNativePlatform()).toBe(false)
    const timer = vi.spyOn(window, 'setInterval')
    await initializePlatformRuntime()
    expect(currentPlatform()).toBe('web')
    expect(isDeveloperModeAvailable()).toBe(false)
    window.__NFPROGRESS_RUNTIME__ = { testProfile: true }
    expect(isDesktopTestProfile()).toBe(false)
    delete window.__NFPROGRESS_RUNTIME__
    expect(document.documentElement.dataset.platform).toBe('web')
    expect(supportsUpdateChecks()).toBe(false)
    expect(timer).not.toHaveBeenCalled()
    await expect(requestApplicationClose()).resolves.toBe(false)
    await syncNativeSystemBars('dark')
    expect(() => requireDesktopRuntime()).toThrow('Доступно только в desktop-приложении.')
    await expect(pickDesktopWordFile('Word', 'Word')).rejects.toThrow('desktop')
  })

  it('rejects explicit native diagnostic operations without reporting success', async () => {
    diagnostics.record('application', 'load', 'succeeded')
    await diagnostics.flush()
    for (const operation of [() => diagnostics.stats(), () => diagnostics.copy(), () => diagnostics.export(), () => diagnostics.clear()]) {
      await expect(operation()).rejects.toThrow('Доступно только в desktop-приложении.')
    }
  })

  it('opens HTTP links through the browser and rejects other protocols', async () => {
    const open = vi.spyOn(window, 'open').mockReturnValue(null)
    await openExternalUrl('https://example.org/help')
    expect(open).toHaveBeenCalledWith('https://example.org/help', '_blank', 'noopener,noreferrer')
    await expect(openExternalUrl('file:///private/example')).rejects.toThrow()
    expect(open).toHaveBeenCalledTimes(1)
  })
})
