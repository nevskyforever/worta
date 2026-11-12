import { apiRequest } from './client'
import { currentPlatform } from '@/platform/runtime'
import { bundledAgreement, bundledHelp, bundledLanguages, bundledLocale } from '@/content/bundledContent'
import type { LanguageOption, SupportedLanguage } from '@/types/api'
import type { AgreementContent, HelpSection } from '@/types/content'

export const contentApi = {
  languages(): Promise<LanguageOption[]> {
    if (currentPlatform() === 'tauri') return Promise.resolve(bundledLanguages())
    return apiRequest<LanguageOption[]>('/api/content/languages')
  },

  locale(language: SupportedLanguage): Promise<Record<string, string>> {
    // The closed Web Beta serves the account API without legacy content routes.
    if (currentPlatform() === 'tauri' || currentPlatform() === 'web') return bundledLocale(language)
    return apiRequest<Record<string, string>>(
      `/api/content/locales/${encodeURIComponent(language)}`,
    )
  },

  help(language: SupportedLanguage, signal?: AbortSignal): Promise<HelpSection[]> {
    if (currentPlatform() === 'tauri') {
      if (signal?.aborted) return Promise.reject(new DOMException('Aborted', 'AbortError'))
      return bundledHelp(language)
    }
    const params = new URLSearchParams({ language })
    return apiRequest<HelpSection[]>(`/api/content/help?${params.toString()}`, { signal })
  },

  agreement(language: SupportedLanguage, signal?: AbortSignal): Promise<AgreementContent> {
    if (currentPlatform() === 'tauri') {
      if (signal?.aborted) return Promise.reject(new DOMException('Aborted', 'AbortError'))
      return bundledAgreement(language)
    }
    const params = new URLSearchParams({ language })
    return apiRequest<AgreementContent>(`/api/content/agreement?${params.toString()}`, { signal })
  },
}
