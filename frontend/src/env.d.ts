/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_API_BASE_URL?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}

interface NFProgressRuntimeBridge {
  version?: string
  nativeUpdates?: boolean
  architecture?: string
  development?: boolean
  testProfile?: boolean
  developerModeAvailable?: boolean
  startupError?: string
}

interface Window {
  __NFPROGRESS_RUNTIME__?: NFProgressRuntimeBridge
  __TAURI_INTERNALS__?: unknown
}
