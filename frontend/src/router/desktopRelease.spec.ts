import { describe, expect, it, vi } from 'vitest'

vi.mock('@/platform/runtime', () => ({ currentPlatform: () => 'tauri' }))

import router from './index'

describe('local desktop routes', () => {
  it.each(['/admin/login', '/admin', '/invite'])('redirects %s to local projects', async path => {
    await router.push(path)
    expect(router.currentRoute.value.path).toBe('/projects')
  })
})
