import { diagnostics } from '@/diagnostics/service'
import { safeError } from '@/diagnostics/events'
import { Capacitor } from '@capacitor/core'

interface DesktopRuntimeInfo {
  version: string
  nativeUpdates: boolean
  architecture: string
  development: boolean
  testProfile: boolean
  developerModeAvailable: boolean
  startupError?: string
}

export type RuntimePlatform = 'web' | 'tauri' | 'ios' | 'android'

let runtimePlatform: RuntimePlatform = 'web'

function isTauriRuntime(): boolean {
  return typeof window !== 'undefined' && window.__TAURI_INTERNALS__ !== undefined
}

export function currentPlatform(): RuntimePlatform {
  return runtimePlatform
}

/** Reject unsupported native operations before importing a Tauri plugin. */
export function requireDesktopRuntime(): void {
  if (currentPlatform() !== 'tauri') {
    throw new Error('Доступно только в desktop-приложении.')
  }
}

export function isNativeMobile(): boolean {
  return runtimePlatform === 'ios' || runtimePlatform === 'android'
}

export function supportsNativeUpdates(): boolean {
  return runtimePlatform === 'tauri' && window.__NFPROGRESS_RUNTIME__?.nativeUpdates === true
}

export function isTauriDevelopment(): boolean {
  return runtimePlatform === 'tauri' && window.__NFPROGRESS_RUNTIME__?.development === true
}

/** The native build profile is compiled into the desktop application. */
export function isDesktopTestProfile(): boolean {
  return runtimePlatform === 'tauri' && window.__NFPROGRESS_RUNTIME__?.testProfile === true
}

export function isDeveloperModeAvailable(): boolean {
  return runtimePlatform === 'tauri' && window.__NFPROGRESS_RUNTIME__?.developerModeAvailable === true
}

export function supportsMacUpdateChecks(): boolean {
  return runtimePlatform === 'tauri'
    && !supportsNativeUpdates()
    && typeof navigator !== 'undefined'
    && /Macintosh|Mac OS X/i.test(navigator.userAgent)
}

export function supportsUpdateChecks(): boolean {
  return supportsNativeUpdates() || supportsMacUpdateChecks()
}

async function initializeTauriRuntime(): Promise<void> {
  const { invoke } = await import('@tauri-apps/api/core')
  try {
    const runtime = await invoke<DesktopRuntimeInfo>('runtime_info')
    window.__NFPROGRESS_RUNTIME__ = {
      version: runtime.version,
      nativeUpdates: runtime.nativeUpdates,
      architecture: runtime.architecture,
      development: runtime.development,
      testProfile: runtime.testProfile,
      developerModeAvailable: runtime.developerModeAvailable,
      startupError: runtime.startupError,
    }
  } catch (error) {
    window.__NFPROGRESS_RUNTIME__ = {
      startupError:
        error instanceof Error ? error.message : String(error || 'Не удалось запустить приложение.'),
    }
  }
  diagnostics.record('migrations', 'runtime_start', window.__NFPROGRESS_RUNTIME__?.startupError ? 'failed' : 'succeeded', undefined, {}, window.__NFPROGRESS_RUNTIME__?.startupError ? 'error' : 'info')
  // Native document synchronization is deliberately independent of the
  // legacy localhost service.  The command is idempotent and hashes/coalesces
  // unchanged sources on the Rust side.
  window.setInterval(() => {
    void invoke('run_all_document_sync').catch(error => { diagnostics.record('documents', 'documents_sync', 'failed', undefined, safeError(error), 'error') })
  }, 60_000)
}

async function initializeCapacitorRuntime(): Promise<void> {
  const nativePlatform = Capacitor.getPlatform()
  runtimePlatform = nativePlatform === 'ios' ? 'ios' : 'android'
  document.documentElement.dataset.platform = runtimePlatform

  const [{ App }, { Keyboard, KeyboardResize }, { StatusBar, Style }] = await Promise.all([
    import('@capacitor/app'),
    import('@capacitor/keyboard'),
    import('@capacitor/status-bar'),
  ])
  await Promise.allSettled([
    Keyboard.setResizeMode({ mode: KeyboardResize.Native }),
    StatusBar.setOverlaysWebView({ overlay: false }),
    StatusBar.setStyle({ style: Style.Default }),
  ])
  await App.addListener('backButton', ({ canGoBack }) => {
    if (canGoBack) {
      window.history.back()
    } else {
      void App.minimizeApp()
    }
  })
}

export async function syncNativeSystemBars(theme: 'light' | 'dark'): Promise<void> {
  if (!isNativeMobile()) return
  const { StatusBar, Style } = await import('@capacitor/status-bar')
  await Promise.allSettled([
    StatusBar.setStyle({ style: theme === 'dark' ? Style.Dark : Style.Light }),
    StatusBar.setBackgroundColor({ color: theme === 'dark' ? '#171b1a' : '#f7f5ef' }),
  ])
}

export async function initializePlatformRuntime(): Promise<void> {
  if (isTauriRuntime()) {
    runtimePlatform = 'tauri'
    document.documentElement.dataset.platform = runtimePlatform
    await initializeTauriRuntime()
    return
  }
  if (Capacitor.isNativePlatform()) {
    await initializeCapacitorRuntime()
    return
  }
  document.documentElement.dataset.platform = runtimePlatform
}

export async function openExternalUrl(url: string): Promise<void> {
  const parsedUrl = new URL(url)
  if (!['https:', 'http:'].includes(parsedUrl.protocol)) {
    throw new Error('Разрешены только HTTP- и HTTPS-ссылки.')
  }
  if (runtimePlatform === 'tauri') {
    const { openUrl } = await import('@tauri-apps/plugin-opener')
    await openUrl(parsedUrl.toString())
    return
  }
  if (isNativeMobile()) {
    const { Browser } = await import('@capacitor/browser')
    await Browser.open({ url: parsedUrl.toString() })
    return
  }
  window.open(parsedUrl.toString(), '_blank', 'noopener,noreferrer')
}

export async function requestApplicationClose(): Promise<boolean> {
  if (runtimePlatform !== 'tauri') return false
  try {
    const { getCurrentWindow } = await import('@tauri-apps/api/window')
    await getCurrentWindow().close()
    return true
  } catch {
    // A restricted desktop capability must leave the agreement gate in place.
    return false
  }
}
