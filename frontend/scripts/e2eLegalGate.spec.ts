// @vitest-environment node
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterAll, afterEach, describe, expect, it } from 'vitest'
import { enableE2eLegalGateSkip } from './e2eLegalGate'

describe('isolated E2E startup gate', () => {
  const profile = mkdtempSync(join(tmpdir(), 'worta-e2e-test-'))
  const env = { WORTA_E2E_SKIP_LEGAL_GATE: '1', NFPROGRESS_ENV: 'test',
    NFPROGRESS_BUILD_PROFILE: 'test', NFPROGRESS_DATA_DIR: profile }
  afterEach(() => rmSync(join(profile, '.worta-e2e-isolated'), { force: true }))
  afterAll(() => rmSync(profile, { recursive: true, force: true }))

  it('requires an explicitly marked temporary profile and test environment', () => {
    expect(enableE2eLegalGateSkip('serve', 'development', env)).toBe(false)
    writeFileSync(join(profile, '.worta-e2e-isolated'), '')
    expect(enableE2eLegalGateSkip('serve', 'development', env)).toBe(true)
    for (const key of ['WORTA_E2E_SKIP_LEGAL_GATE', 'NFPROGRESS_ENV', 'NFPROGRESS_BUILD_PROFILE', 'NFPROGRESS_DATA_DIR']) {
      expect(enableE2eLegalGateSkip('serve', 'development', { ...env, [key]: '' })).toBe(false)
    }
    expect(enableE2eLegalGateSkip('serve', 'development', { ...env, NFPROGRESS_DATA_DIR: process.cwd() })).toBe(false)
  })

  it('cannot enable the bypass in a build, production mode or production node environment', () => {
    writeFileSync(join(profile, '.worta-e2e-isolated'), '')
    expect(enableE2eLegalGateSkip('build', 'production', env)).toBe(false)
    expect(enableE2eLegalGateSkip('build', 'development', env)).toBe(false)
    expect(enableE2eLegalGateSkip('serve', 'production', env)).toBe(false)
    expect(enableE2eLegalGateSkip('serve', 'development', { ...env, NODE_ENV: 'production' })).toBe(false)
  })
})
