import type { LanguageOption, SupportedLanguage } from '@/types/api'
import type { AgreementContent, HelpSection } from '@/types/content'

type LocaleCatalog = Record<string, string>

const localeCatalogs = import.meta.glob<LocaleCatalog>(
  '../i18n/generated/locales/*.json',
  { import: 'default' },
)
const helpCatalogs = import.meta.glob<HelpSection[]>(
  '../i18n/generated/help/*.json',
  { import: 'default' },
)

const languages: LanguageOption[] = [
  { code: 'ru', display_name: 'Русский' },
  { code: 'en', display_name: 'English' },
  { code: 'es', display_name: 'Español' },
  { code: 'de', display_name: 'Deutsch' },
  { code: 'fr', display_name: 'Français' },
  { code: 'pt_BR', display_name: 'Português (Brasil)' },
]

export function bundledLanguages(): LanguageOption[] {
  return languages.map((language) => ({ ...language }))
}

export async function bundledLocale(language: SupportedLanguage): Promise<LocaleCatalog> {
  return await localeCatalogs[`../i18n/generated/locales/${language}.json`]?.() ?? {}
}

export async function bundledHelp(language: SupportedLanguage): Promise<HelpSection[]> {
  return await helpCatalogs[`../i18n/generated/help/${language}.json`]?.() ?? []
}

export async function bundledAgreement(language: SupportedLanguage): Promise<AgreementContent> {
  const values = await bundledLocale(language)
  const source = Object.keys(values).find((key) => key.startsWith('<!DOCTYPE HTML')) ?? ''
  return {
    id: 'd6157bbe87d88dc1',
    language,
    html: language === 'ru' ? source : values[source] ?? source,
  }
}
