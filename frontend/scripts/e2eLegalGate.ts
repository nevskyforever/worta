import { existsSync, realpathSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { isAbsolute, relative, resolve, sep } from 'node:path'

// This only affects the local startup screen. Cloud legal enforcement is separate.
export function enableE2eLegalGateSkip(command: string, mode: string, env: NodeJS.ProcessEnv): boolean {
  if (command !== 'serve' || mode !== 'development' || env.NODE_ENV === 'production'
    || env.WORTA_E2E_SKIP_LEGAL_GATE !== '1' || env.NFPROGRESS_ENV !== 'test'
    || env.NFPROGRESS_BUILD_PROFILE !== 'test' || !env.NFPROGRESS_DATA_DIR) return false
  try {
    const root = realpathSync(tmpdir())
    const profile = realpathSync(resolve(env.NFPROGRESS_DATA_DIR))
    const path = relative(root, profile)
    return path !== '' && path !== '..' && !path.startsWith(`..${sep}`) && !isAbsolute(path)
      && existsSync(resolve(profile, '.worta-e2e-isolated'))
  } catch {
    return false
  }
}
