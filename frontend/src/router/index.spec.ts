import { describe, expect, it, vi } from 'vitest'
import { adminSession } from '@/api/admin'

import router from './index'

describe('application routes', () => {
  it('routes administrators to the panel only while their session is active', async () => {
    const active = vi.spyOn(adminSession, 'active').mockReturnValue(false)
    await router.push('/admin')
    expect(router.currentRoute.value.name).toBe('admin-login')
    active.mockReturnValue(true)
    await router.push('/admin')
    await router.push('/admin/login')
    expect(router.currentRoute.value.name).toBe('admin')
    active.mockRestore()
  })

  it('exposes every migrated top-level workspace', () => {
    const routeNames = new Set(router.getRoutes().map(({ name }) => name))

    for (const routeName of [
      'projects', 'maps', 'global-project-map', 'notes', 'global-project-notes',
      'game', 'integrations', 'help', 'settings',
    ]) {
      expect(routeNames.has(routeName)).toBe(true)
    }
  })
})
