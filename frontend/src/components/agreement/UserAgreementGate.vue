<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'

import { apiErrorMessage } from '@/api/client'
import { contentApi } from '@/api/content'
import { settingsApi } from '@/api/settings'
import { requestApplicationClose } from '@/platform/runtime'
import {
  SUPPORTED_LANGUAGES,
  isSupportedLanguage,
  useLocaleStore,
} from '@/stores/locale'
import { useThemeStore } from '@/stores/theme'
import type { AgreementContent, SettingsResponse } from '@/types/content'

const emit = defineEmits<{
  accepted: [settings: SettingsResponse]
}>()

const locale = useLocaleStore()
const theme = useThemeStore()
const t = locale.translate
const agreement = ref<AgreementContent | null>(null)
const acceptedTerms = ref(false)
const loading = ref(true)
const savingLanguage = ref(false)
const accepting = ref(false)
const declining = ref(false)
const declined = ref(false)
const error = ref<string | null>(null)
const controller = new AbortController()

const agreementDocument = computed(() => {
  if (!agreement.value) return ''
  const dark = theme.resolved === 'dark'
  const bridgeStyles = `
    <style data-nfprogress-agreement-theme>
      :root { color-scheme: ${dark ? 'dark' : 'light'}; }
      html { background: ${dark ? '#1e2422' : '#fffdf8'} !important; }
      body {
        max-width: 72ch;
        margin: 0 auto !important;
        padding: 1.5rem !important;
        background: ${dark ? '#1e2422' : '#fffdf8'} !important;
        color: ${dark ? '#edf2ef' : '#1e2925'} !important;
        font-family: Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif !important;
        font-size: 1rem !important;
        line-height: 1.6 !important;
      }
      h1, h2 { color: inherit !important; line-height: 1.25 !important; }
      h1 { font-size: 1.65rem !important; }
      h2 { margin-top: 1.6rem !important; font-size: 1.25rem !important; }
      p, li { white-space: normal !important; }
    </style>
  `
  if (agreement.value.html.includes('</head>')) {
    return agreement.value.html.replace('</head>', `${bridgeStyles}</head>`)
  }
  return `${bridgeStyles}${agreement.value.html}`
})

async function loadAgreement(): Promise<void> {
  loading.value = true
  error.value = null
  agreement.value = null
  acceptedTerms.value = false
  try {
    agreement.value = await contentApi.agreement(locale.language, controller.signal)
  } catch (loadError) {
    if (loadError instanceof DOMException && loadError.name === 'AbortError') return
    error.value = t(apiErrorMessage(loadError))
  } finally {
    loading.value = false
  }
}

async function changeLanguage(event: Event): Promise<void> {
  if (savingLanguage.value) return
  const select = event.currentTarget as HTMLSelectElement
  const requestedLanguage = select.value
  if (!isSupportedLanguage(requestedLanguage) || requestedLanguage === locale.language) return

  savingLanguage.value = true
  error.value = null
  try {
    const settings = await settingsApi.update({ language: requestedLanguage })
    const storedLanguage = settings.values.language
    await locale.setLanguage(
      isSupportedLanguage(storedLanguage) ? storedLanguage : requestedLanguage,
    )
    await loadAgreement()
  } catch (updateError) {
    select.value = locale.language
    error.value = t(apiErrorMessage(updateError))
  } finally {
    savingLanguage.value = false
  }
}

async function acceptAgreement(): Promise<void> {
  if (!agreement.value || !acceptedTerms.value || accepting.value) return
  accepting.value = true
  error.value = null
  try {
    const settings = await settingsApi.acceptUserAgreement(agreement.value.id)
    if (settings.values.user_agreement !== true) {
      error.value = t('Не удалось сохранить принятие соглашения.')
      return
    }
    emit('accepted', settings)
  } catch (acceptError) {
    error.value = t(apiErrorMessage(acceptError))
  } finally {
    accepting.value = false
  }
}

async function declineAgreement(): Promise<void> {
  if (declining.value) return
  declining.value = true
  const closed = await requestApplicationClose()
  if (!closed) declined.value = true
  declining.value = false
}

function returnToAgreement(): void {
  declined.value = false
}

onMounted(loadAgreement)
onBeforeUnmount(() => controller.abort())
</script>

<template>
  <main class="agreement-gate">
    <section v-if="declined" class="agreement-card agreement-card--declined" aria-labelledby="declined-title">
      <span class="agreement-brand" aria-hidden="true">nf</span>
      <h1 id="declined-title">{{ t('Для работы с nfprogress необходимо принять пользовательское соглашение').replace(/nfprogress/gi, 'worta') }}</h1>
      <p>{{ t('Без принятия соглашения функции приложения остаются недоступны.') }}</p>
      <button class="nf-button" type="button" @click="returnToAgreement">
        {{ t('Вернуться к соглашению') }}
      </button>
    </section>

    <section v-else class="agreement-card" aria-labelledby="agreement-title">
      <header class="agreement-header">
        <div>
          <p class="agreement-eyebrow">worta</p>
          <h1 id="agreement-title">{{ t('Пользовательское соглашение') }}</h1>
          <p>{{ t('Ознакомьтесь с условиями перед началом работы.') }}</p>
        </div>
        <label class="agreement-language" for="agreement-language">
          <span>{{ t('Язык интерфейса') }}</span>
          <select
            id="agreement-language"
            :value="locale.language"
            :disabled="loading || savingLanguage || accepting"
            @change="changeLanguage"
          >
            <option
              v-for="language in SUPPORTED_LANGUAGES"
              :key="language.code"
              :value="language.code"
            >
              {{ language.displayName }}
            </option>
          </select>
        </label>
      </header>

      <div v-if="loading" class="agreement-state" role="status" aria-live="polite">
        <span class="agreement-loader" aria-hidden="true" />
        <p>{{ t('Загружаем пользовательское соглашение…') }}</p>
      </div>

      <div v-else-if="!agreement" class="agreement-state" role="alert">
        <p>{{ error ?? t('Не удалось загрузить пользовательское соглашение.') }}</p>
        <button class="nf-button" type="button" @click="loadAgreement">
          {{ t('Повторить') }}
        </button>
      </div>

      <template v-else>
        <iframe
          class="agreement-document"
          :srcdoc="agreementDocument"
          :title="t('Текст пользовательского соглашения')"
          sandbox=""
          referrerpolicy="no-referrer"
        />

        <label class="agreement-confirmation">
          <input v-model="acceptedTerms" type="checkbox" />
          <span>{{ t('Я ознакомился с условиями пользовательского соглашения и принимаю их.') }}</span>
        </label>

        <p v-if="error" class="agreement-error" role="alert">{{ error }}</p>

        <footer class="agreement-actions">
          <button
            class="nf-button nf-button--secondary"
            type="button"
            :disabled="accepting || declining"
            @click="declineAgreement"
          >
            {{ declining ? t('Закрываем…') : t('Не принимаю') }}
          </button>
          <button
            class="nf-button"
            type="button"
            :disabled="!acceptedTerms || accepting || declining"
            @click="acceptAgreement"
          >
            {{ accepting ? t('Сохраняем…') : t('Принять и продолжить') }}
          </button>
        </footer>
      </template>
    </section>
  </main>
</template>

<style scoped>
.agreement-gate {
  display: grid;
  min-height: 100dvh;
  padding: max(var(--nf-space-4), env(safe-area-inset-top))
    max(var(--nf-space-4), env(safe-area-inset-right))
    max(var(--nf-space-4), env(safe-area-inset-bottom))
    max(var(--nf-space-4), env(safe-area-inset-left));
  place-items: center;
  background:
    radial-gradient(circle at 10% 5%, var(--nf-color-primary-soft), transparent 36rem),
    var(--nf-color-canvas);
}

.agreement-card {
  display: grid;
  width: min(100%, 58rem);
  max-height: calc(100dvh - 2rem);
  gap: var(--nf-space-4);
  padding: clamp(1rem, 3vw, 2rem);
  overflow: auto;
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-lg);
  background: var(--nf-color-surface);
  box-shadow: var(--nf-shadow-card);
}

.agreement-header {
  display: flex;
  gap: var(--nf-space-5);
  align-items: end;
  justify-content: space-between;
}

.agreement-header h1,
.agreement-header p,
.agreement-card--declined h1,
.agreement-card--declined p {
  margin: 0;
}

.agreement-header h1,
.agreement-card--declined h1 {
  font-family: var(--nf-font-serif);
  font-size: clamp(1.8rem, 5vw, 2.7rem);
  line-height: 1.08;
}

.agreement-header h1 + p,
.agreement-card--declined p {
  margin-top: var(--nf-space-2);
  color: var(--nf-color-text-muted);
  line-height: 1.5;
}

.agreement-eyebrow {
  margin-bottom: var(--nf-space-2) !important;
  color: var(--nf-color-accent);
  font-size: 0.75rem;
  font-weight: 800;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}

.agreement-language {
  display: grid;
  flex: 0 0 min(15rem, 38%);
  gap: var(--nf-space-2);
  color: var(--nf-color-text-muted);
  font-size: 0.8rem;
  font-weight: 700;
}

.agreement-language select {
  min-height: 2.75rem;
  padding: 0 var(--nf-space-3);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
  color: var(--nf-color-text);
}

.agreement-document {
  width: 100%;
  min-height: min(52vh, 31rem);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
}

.agreement-confirmation {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: var(--nf-space-3);
  align-items: start;
  padding: var(--nf-space-3);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-primary-soft);
  line-height: 1.45;
  cursor: pointer;
}

.agreement-confirmation input {
  width: 1.25rem;
  height: 1.25rem;
  margin: 0.1rem 0 0;
  accent-color: var(--nf-color-primary);
}

.agreement-actions {
  display: flex;
  gap: var(--nf-space-3);
  justify-content: flex-end;
}

.agreement-error {
  margin: 0;
  padding: var(--nf-space-3);
  border-radius: var(--nf-radius-sm);
  background: color-mix(in srgb, var(--nf-color-danger), transparent 88%);
  color: var(--nf-color-danger);
  font-weight: 700;
}

.agreement-state,
.agreement-card--declined {
  justify-items: center;
  text-align: center;
}

.agreement-state {
  display: grid;
  min-height: 20rem;
  gap: var(--nf-space-3);
  align-content: center;
}

.agreement-loader {
  width: 2rem;
  height: 2rem;
  border: 3px solid var(--nf-color-border);
  border-top-color: var(--nf-color-primary);
  border-radius: 50%;
  animation: agreement-spin 0.8s linear infinite;
}

.agreement-brand {
  display: grid;
  width: 4rem;
  height: 4rem;
  place-items: center;
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-primary);
  color: #fff;
  font-family: var(--nf-font-serif);
  font-size: 1.7rem;
  font-weight: 800;
}

@keyframes agreement-spin {
  to { transform: rotate(360deg); }
}

@media (max-width: 40rem) {
  .agreement-gate {
    display: block;
    padding: env(safe-area-inset-top) 0 env(safe-area-inset-bottom);
  }

  .agreement-card {
    min-height: 100dvh;
    max-height: none;
    border: 0;
    border-radius: 0;
  }

  .agreement-header {
    align-items: stretch;
    flex-direction: column;
  }

  .agreement-language {
    flex-basis: auto;
  }

  .agreement-document {
    min-height: 48vh;
  }

  .agreement-actions {
    align-items: stretch;
    flex-direction: column-reverse;
  }
}

:global(html[data-motion='reduced']) .agreement-loader { animation: none; }
</style>
