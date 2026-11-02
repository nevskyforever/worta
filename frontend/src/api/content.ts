import { apiRequest } from './client'
import { currentPlatform } from '@/platform/runtime'
import { desktopCloudEnabled } from '@/platform/desktopCloud'
import { bundledAgreement, bundledHelp, bundledLanguages, bundledLocale } from '@/content/bundledContent'
import type { LanguageOption, SupportedLanguage } from '@/types/api'
import type { AgreementContent, HelpSection } from '@/types/content'

function desktopHelp(sections: HelpSection[]): HelpSection[] {
  if (desktopCloudEnabled()) return sections
  return sections
    .filter(section => !section.key.startsWith('cloud_') && section.key !== 'local_external_reattachment')
    .map(section => ({ ...section, children: desktopHelp(section.children) }))
}

export const contentApi = {
  languages(): Promise<LanguageOption[]> {
    if (currentPlatform() === 'tauri') return Promise.resolve(bundledLanguages())
    return apiRequest<LanguageOption[]>('/api/content/languages')
  },

  locale(language: SupportedLanguage): Promise<Record<string, string>> {
    if (currentPlatform() === 'tauri') return Promise.resolve(bundledLocale(language))
    return apiRequest<Record<string, string>>(
      `/api/content/locales/${encodeURIComponent(language)}`,
    )
  },

  help(language: SupportedLanguage, signal?: AbortSignal): Promise<HelpSection[]> {
    if (currentPlatform() === 'tauri') {
      if (signal?.aborted) return Promise.reject(new DOMException('Aborted', 'AbortError'))
      return Promise.resolve(desktopHelp(bundledHelp(language)))
    }
    const params = new URLSearchParams({ language })
    return apiRequest<HelpSection[]>(`/api/content/help?${params.toString()}`, { signal })
  },

  agreement(language: SupportedLanguage, signal?: AbortSignal): Promise<AgreementContent> {
    if (currentPlatform() === 'tauri') {
      if (signal?.aborted) return Promise.reject(new DOMException('Aborted', 'AbortError'))
      return Promise.resolve(bundledAgreement(language))
    }
    const params = new URLSearchParams({ language })
    return apiRequest<AgreementContent>(`/api/content/agreement?${params.toString()}`, { signal })
  },
}
