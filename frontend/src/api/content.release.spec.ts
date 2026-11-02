import { describe, expect, it, vi } from 'vitest'

vi.mock('@/platform/runtime', () => ({ currentPlatform: () => 'tauri' }))

import { contentApi } from './content'
import type { SupportedLanguage } from '@/types/api'
import type { HelpSection } from '@/types/content'

function keys(sections: HelpSection[]): string[] {
  return sections.flatMap(section => [section.key, ...keys(section.children)])
}

describe('local desktop help', () => {
  it.each<SupportedLanguage>(['ru', 'en', 'es', 'de', 'fr', 'pt_BR'])(
    'hides cloud-only topics for %s', async language => {
      const result = keys(await contentApi.help(language))
      expect(result.length).toBeGreaterThan(0)
      expect(result.some(key => key.startsWith('cloud_'))).toBe(false)
      expect(result).not.toContain('local_external_reattachment')
    },
  )
})
