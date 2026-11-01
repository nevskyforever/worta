declare const __WORTA_E2E_SKIP_LEGAL_GATE__: boolean

/** Development-only startup screen bypass; never changes recorded agreement state. */
export function skipE2eLegalGate(): boolean {
  return __WORTA_E2E_SKIP_LEGAL_GATE__
    && (window.location.hostname === '127.0.0.1' || window.location.hostname === 'localhost')
}
