<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { IonContent, IonIcon, IonPage, IonSpinner } from '@ionic/vue'
import {
  alertCircleOutline,
  checkmarkCircleOutline,
  cloudOutline,
  desktopOutline,
  logoGithub,
  refreshOutline,
  settingsOutline,
} from 'ionicons/icons'

import { apiErrorMessage } from '@/api/client'
import { settingsApi } from '@/api/settings'
import DeveloperModeDialog from '@/components/developer/DeveloperModeDialog.vue'
import CloudSyncSettingsCard from '@/components/settings/CloudSyncSettingsCard.vue'
import BrowserCloudAccountCard from '@/components/settings/BrowserCloudAccountCard.vue'
import DiagnosticsCard from '@/components/settings/DiagnosticsCard.vue'
import SettingToggle from '@/components/settings/SettingToggle.vue'
import StatePanel from '@/components/ui/StatePanel.vue'
import { SUPPORTED_LANGUAGES, useLocaleStore } from '@/stores/locale'
import { useMotionStore, type MotionPreference } from '@/stores/motion'
import { useNotificationsStore } from '@/stores/notifications'
import { useThemeStore, type ThemePreference } from '@/stores/theme'
import { useUpdaterStore } from '@/stores/updater'
import { currentPlatform, supportsUpdateChecks } from '@/platform/runtime'
import type {
  FrontendMotion,
  FrontendTheme,
  SettingKey,
  SettingsResponse,
  SettingsValues,
} from '@/types/content'
import type { SupportedLanguage } from '@/types/api'

interface SettingsForm {
  language: SupportedLanguage
  frontend_theme: FrontendTheme
  frontend_motion: FrontendMotion
  start_day_time: string
  notification_display_time: number
  game_mode: boolean
  inf_project: boolean
  global_streak: boolean
  show_written_today_in_all_projects: boolean
  background_synch: boolean
}

const GENERAL_KEYS: ReadonlyArray<keyof SettingsForm> = [
  'language',
  'frontend_theme',
  'frontend_motion',
  'start_day_time',
  'notification_display_time',
  'game_mode',
  'inf_project',
  'global_streak',
  'show_written_today_in_all_projects',
]
const DESKTOP_KEYS: ReadonlyArray<keyof SettingsForm> = ['background_synch']

const locale = useLocaleStore()
const theme = useThemeStore()
const motion = useMotionStore()
const notifications = useNotificationsStore()
const updater = useUpdaterStore()
const t = locale.translate
const response = ref<SettingsResponse | null>(null)
const loading = ref(true)
const saving = ref(false)
const error = ref<string | null>(null)
const savedMessage = ref('')
const developerDialogOpen = ref(false)
const originalValues = ref<Record<string, unknown>>({})
const controller = new AbortController()

const form = reactive<SettingsForm>({
  language: locale.language,
  frontend_theme: theme.preference,
  frontend_motion: motion.preference,
  start_day_time: '00:00',
  notification_display_time: 10,
  game_mode: false,
  inf_project: false,
  global_streak: false,
  show_written_today_in_all_projects: false,
  background_synch: false,
})

const editable = computed(() => new Set<SettingKey>(response.value?.editable_keys ?? []))
const isDesktop = computed(() => response.value?.platform === 'desktop')
const updaterAvailable = computed(() => isDesktop.value && supportsUpdateChecks())
const developerAvailable = computed(
  () => response.value?.capabilities.developer_mode_available === true,
)
const visibleKeys = computed<ReadonlyArray<keyof SettingsForm>>(() => [
  ...GENERAL_KEYS.filter((key) => editable.value.has(key)),
  ...(isDesktop.value ? DESKTOP_KEYS.filter((key) => editable.value.has(key)) : []),
])

const dirty = computed(() =>
  visibleKeys.value.some((key) => form[key] !== originalValues.value[key]),
)

const platformName = computed(() => {
  switch (response.value?.platform) {
    case 'desktop':
      return t('Настольное приложение')
    case 'ios':
      return 'iOS'
    case 'android':
      return 'Android'
    default:
      return t('Веб-приложение')
  }
})

function booleanValue(values: SettingsValues, key: keyof SettingsForm): boolean {
  return values[key] === true
}

function notificationDuration(values: SettingsValues): number {
  const value = values.notification_display_time
  return typeof value === 'number' && Number.isInteger(value) && value >= 1 && value <= 3600
    ? value
    : 10
}

function applySettings(settings: SettingsResponse): void {
  const values = settings.values
  form.language = typeof values.language === 'string' ? values.language : 'ru'
  form.frontend_theme =
    values.frontend_theme === 'light' || values.frontend_theme === 'dark'
      ? values.frontend_theme
      : 'system'
  form.frontend_motion =
    values.frontend_motion === 'system' || values.frontend_motion === 'reduced'
      ? values.frontend_motion
      : 'full'
  form.start_day_time =
    typeof values.start_day_time === 'string' ? values.start_day_time.slice(0, 5) : '00:00'
  form.notification_display_time = notificationDuration(values)
  form.game_mode = booleanValue(values, 'game_mode')
  form.inf_project = booleanValue(values, 'inf_project')
  form.global_streak = booleanValue(values, 'global_streak')
  form.show_written_today_in_all_projects = booleanValue(
    values,
    'show_written_today_in_all_projects',
  )
  form.background_synch = booleanValue(values, 'background_synch')
  response.value = settings
  originalValues.value = Object.fromEntries(
    [...GENERAL_KEYS, ...DESKTOP_KEYS].map((key) => [key, form[key]]),
  )
}

async function synchronizeFrontendPreferences(): Promise<void> {
  theme.setPreference(form.frontend_theme as ThemePreference)
  motion.setPreference(form.frontend_motion as MotionPreference)
  if (locale.language !== form.language) await locale.setLanguage(form.language)
}

async function loadSettings(): Promise<void> {
  loading.value = true
  error.value = null
  try {
    applySettings(await settingsApi.get(controller.signal))
    await synchronizeFrontendPreferences()
  } catch (loadError) {
    if (loadError instanceof DOMException && loadError.name === 'AbortError') return
    error.value = t(apiErrorMessage(loadError))
  } finally {
    loading.value = false
  }
}

function changedValues(): SettingsValues {
  const values: SettingsValues = {}
  const writable = values as Record<string, unknown>
  for (const key of visibleKeys.value) {
    if (form[key] !== originalValues.value[key]) writable[key] = form[key]
  }
  if ('start_day_time' in values) values.start_day_time = `${form.start_day_time}:00`
  return values
}

async function saveSettings(): Promise<void> {
  if (!dirty.value || saving.value) return
  saving.value = true
  error.value = null
  savedMessage.value = ''
  try {
    applySettings(await settingsApi.update(changedValues()))
    await synchronizeFrontendPreferences()
    savedMessage.value = t('Настройки сохранены.')
    notifications.setDurationSeconds(form.notification_display_time)
    notifications.success(savedMessage.value)
  } catch (saveError) {
    error.value = t(apiErrorMessage(saveError))
  } finally {
    saving.value = false
  }
}

async function checkForUpdates(): Promise<void> {
  await updater.checkForUpdates(true)
}

onMounted(loadSettings)
onBeforeUnmount(() => controller.abort())
</script>

<template>
  <IonPage>
    <IonContent :fullscreen="true" class="settings-content">
      <main class="settings-page">
        <header class="settings-header">
          <div>
            <p>{{ t('Приложение') }}</p>
            <h1>{{ t('Настройки') }}</h1>
            <span>{{ t('Здесь показаны только возможности, доступные на этом устройстве.') }}</span>
          </div>
          <IonIcon :icon="settingsOutline" aria-hidden="true" />
        </header>

        <StatePanel
          v-if="loading"
          :title="t('Загружаем настройки')"
          :message="t('Получаем доступные параметры для этой платформы.')"
          loading
        />
        <StatePanel
          v-else-if="!response"
          :title="t('Не удалось открыть настройки')"
          :message="error ?? t('Не удалось подключиться к приложению.')"
          :icon="alertCircleOutline"
        >
          <button class="nf-button" type="button" @click="loadSettings">{{ t('Повторить') }}</button>
        </StatePanel>

        <form v-else class="settings-layout" @submit.prevent="saveSettings">
          <aside class="platform-card" aria-labelledby="platform-title">
            <IonIcon :icon="isDesktop ? desktopOutline : cloudOutline" aria-hidden="true" />
            <div>
              <h2 id="platform-title">{{ platformName }}</h2>
              <p v-if="response.capabilities.local_file_sync">
                {{ t('Доступна прямая синхронизация с локальными файлами.') }}
              </p>
              <p v-else>
                {{ t('Локальные файлы доступны только после явного выбора или загрузки.') }}
              </p>
              <p v-if="response.capabilities.remote_api">
                {{ t('Адрес удалённого API задаётся конфигурацией приложения, а не хранится как пользовательский секрет.') }}
              </p>
              <p class="platform-card__author">{{ t('Автор') }}: nevskyforever / nfproject</p>
            </div>
          </aside>

          <div class="settings-groups">
            <BrowserCloudAccountCard v-if="currentPlatform() === 'web' && response?.platform !== 'desktop'" />
            <CloudSyncSettingsCard v-if="isDesktop" />
            <DiagnosticsCard v-if="isDesktop" />

            <section class="settings-card" aria-labelledby="appearance-settings-title">
              <div class="settings-card__heading">
                <h2 id="appearance-settings-title">{{ t('Интерфейс и время') }}</h2>
                <p>{{ t('Язык, тема и границы писательского дня.') }}</p>
              </div>
              <div class="select-grid">
                <label v-if="editable.has('language')" for="settings-language">
                  <span>{{ t('Язык интерфейса') }}</span>
                  <select id="settings-language" v-model="form.language">
                    <option
                      v-for="language in SUPPORTED_LANGUAGES"
                      :key="language.code"
                      :value="language.code"
                    >
                      {{ language.displayName }}
                    </option>
                  </select>
                </label>
                <label v-if="editable.has('frontend_theme')" for="settings-theme">
                  <span>{{ t('Тема') }}</span>
                  <select id="settings-theme" v-model="form.frontend_theme">
                    <option value="system">{{ t('Как в системе') }}</option>
                    <option value="light">{{ t('Светлая') }}</option>
                    <option value="dark">{{ t('Тёмная') }}</option>
                  </select>
                </label>
                <label v-if="editable.has('frontend_motion')" for="settings-motion">
                  <span>{{ t('Анимация интерфейса') }}</span>
                  <select id="settings-motion" v-model="form.frontend_motion">
                    <option value="full">{{ t('Полная') }}</option>
                    <option value="system">{{ t('Как в системе') }}</option>
                    <option value="reduced">{{ t('Уменьшенная') }}</option>
                  </select>
                </label>
                <label v-if="editable.has('start_day_time')" for="settings-day-start">
                  <span>{{ t('Начало писательского дня') }}</span>
                  <input id="settings-day-start" v-model="form.start_day_time" type="time" step="60" />
                </label>
                <label v-if="editable.has('notification_display_time')" for="settings-notification-time">
                  <span>{{ t('Время показа уведомлений, сек.') }}</span>
                  <input
                    id="settings-notification-time"
                    v-model.number="form.notification_display_time"
                    type="number"
                    min="1"
                    max="3600"
                    step="1"
                    inputmode="numeric"
                  />
                </label>
              </div>
            </section>

            <section class="settings-card" aria-labelledby="writing-settings-title">
              <div class="settings-card__heading">
                <h2 id="writing-settings-title">{{ t('Работа и мотивация') }}</h2>
                <p>{{ t('Общие правила проектов и необязательные игровые механики.') }}</p>
              </div>
              <SettingToggle
                v-if="editable.has('game_mode')"
                id="settings-game-mode"
                v-model="form.game_mode"
                :label="t('Игровой режим')"
                :description="t('Включает награды, магазин, испытания и творческий ритм.')"
              />
              <SettingToggle
                v-if="editable.has('inf_project')"
                id="settings-infinite-project"
                v-model="form.inf_project"
                :label="t('Бесконечный проект')"
                :description="t('Создаёт «Общий проект» без конечной цели. При отключении проект удаляется после резервного копирования данных.')"
              />
              <SettingToggle
                v-if="editable.has('global_streak')"
                id="settings-global-streak"
                v-model="form.global_streak"
                :label="t('Общий стрик')"
                :description="t('Учитывает продуктивные дни по всем активным проектам.')"
              />
              <SettingToggle
                v-if="editable.has('show_written_today_in_all_projects')"
                id="settings-written-today"
                v-model="form.show_written_today_in_all_projects"
                :label="t('Показывать написанное сегодня')"
                :description="t('Показывает суммарное число символов за текущий писательский день в рабочем пространстве проектов.')"
              />
            </section>

            <section
              v-if="isDesktop && (editable.has('background_synch') || updaterAvailable)"
              class="settings-card"
              aria-labelledby="desktop-settings-title"
            >
              <div class="settings-card__heading">
                <h2 id="desktop-settings-title">{{ t('Только desktop') }}</h2>
                <p>{{ t('Эти параметры скрыты в Web, iOS и Android.') }}</p>
              </div>
              <SettingToggle
                v-if="editable.has('background_synch')"
                id="settings-background-sync"
                v-model="form.background_synch"
                :label="t('Фоновая синхронизация документов')"
                :description="t('Проверяет активные подключённые источники при запуске и после смены писательского дня.')"
              />
              <div v-if="updaterAvailable" class="update-setting">
                <div>
                  <strong>{{ t('Обновления приложения') }}</strong>
                  <p>{{ t('Подписанные обновления проверяются автоматически при запуске и раз в час.') }}</p>
                  <p v-if="updater.status === 'available'" class="update-setting__status" role="status">
                    {{ t('Доступна версия {version}.', { version: updater.availableVersion }) }}
                  </p>
                  <p v-else-if="updater.status === 'error'" class="update-setting__error" role="alert">
                    {{ t('Не удалось проверить обновления.') }}
                  </p>
                </div>
                <button
                  id="settings-check-updates"
                  class="nf-button nf-button--secondary"
                  :class="{ 'update-setting__button--current': updater.status === 'current' }"
                  type="button"
                  :disabled="updater.busy"
                  @click="checkForUpdates"
                >
                  <IonSpinner v-if="updater.status === 'checking'" name="crescent" aria-hidden="true" />
                  <IonIcon
                    v-else
                    :icon="updater.status === 'current' ? checkmarkCircleOutline : refreshOutline"
                    aria-hidden="true"
                  />
                  {{
                    updater.status === 'checking'
                      ? t('Проверяем…')
                      : updater.status === 'current'
                        ? t('Установлена последняя версия')
                        : t('Проверить обновления')
                  }}
                </button>
              </div>
            </section>

            <section
              v-if="developerAvailable"
              class="settings-card"
              aria-labelledby="developer-settings-title"
            >
              <div class="settings-card__heading">
                <h2 id="developer-settings-title">{{ t('Режим разработчика') }}</h2>
                <p>{{ t('Тестовая дата, параметры персонажа, выдача предметов и управление тестовым профилем.') }}</p>
              </div>
              <button
                id="settings-open-developer-mode"
                class="nf-button"
                type="button"
                @click="developerDialogOpen = true"
              >
                {{ t('Открыть режим разработчика') }}
              </button>
            </section>

            <div v-if="error" class="settings-message settings-message--error" role="alert">
              <IonIcon :icon="alertCircleOutline" aria-hidden="true" />
              {{ error }}
            </div>
            <div v-if="savedMessage" class="settings-message" role="status">
              <IonIcon :icon="checkmarkCircleOutline" aria-hidden="true" />
              {{ savedMessage }}
            </div>

            <footer class="settings-actions">
              <p>{{ dirty ? t('Есть несохранённые изменения.') : t('Все изменения сохранены.') }}</p>
              <button class="nf-button" type="submit" :disabled="!dirty || saving">
                <IonSpinner v-if="saving" name="crescent" aria-hidden="true" />
                {{ saving ? t('Сохраняем…') : t('Сохранить настройки') }}
              </button>
            </footer>

            <div class="settings-app-meta">
              <span>{{ t('Версия nfprogress') }} {{ response?.version || '—' }}</span>
              <span>{{ t('Автор') }}: nevskyforever / nfproject</span>
              <a
                class="nf-button nf-button--secondary settings-repository-link"
                href="https://github.com/nevskyforever/worta"
                target="_blank"
                rel="noopener noreferrer"
                :aria-label="t('Открыть репозиторий nfprogress на GitHub')"
              >
                <IonIcon :icon="logoGithub" aria-hidden="true" />
              </a>
            </div>
          </div>
        </form>
      </main>
    </IonContent>
    <DeveloperModeDialog
      :open="developerDialogOpen"
      @close="developerDialogOpen = false"
    />
  </IonPage>
</template>

<style scoped>
.settings-content {
  --background: var(--nf-color-canvas);
}

.settings-page {
  width: min(100%, 78rem);
  min-height: 100%;
  margin: 0 auto;
  padding: calc(var(--nf-space-7) + env(safe-area-inset-top)) clamp(1rem, 3vw, 3.5rem)
    calc(var(--nf-space-7) + env(safe-area-inset-bottom));
}

.settings-header {
  display: flex;
  gap: var(--nf-space-5);
  align-items: center;
  justify-content: space-between;
  margin-bottom: var(--nf-space-7);
}

.settings-header p {
  margin: 0 0 var(--nf-space-2);
  color: var(--nf-color-accent);
  font-size: 0.75rem;
  font-weight: 800;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}

.settings-header h1 {
  margin: 0;
  font-family: var(--nf-font-serif);
  font-size: clamp(1.8rem, 3.5vw, 2.75rem);
  letter-spacing: -0.04em;
}

.settings-header span {
  display: block;
  max-width: 42rem;
  margin-top: var(--nf-space-3);
  color: var(--nf-color-text-muted);
  line-height: 1.55;
}

.settings-header > ion-icon {
  flex: 0 0 auto;
  padding: var(--nf-space-4);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
  font-size: 2rem;
}

.settings-layout {
  display: grid;
  grid-template-columns: minmax(14rem, 18rem) minmax(0, 1fr);
  gap: var(--nf-space-6);
  align-items: start;
}

.platform-card,
.settings-card {
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-surface);
  box-shadow: var(--nf-shadow-card);
}

.platform-card {
  position: sticky;
  top: var(--nf-space-5);
  display: grid;
  gap: var(--nf-space-3);
  padding: var(--nf-space-5);
}

.platform-card > ion-icon {
  color: var(--nf-color-primary);
  font-size: 1.7rem;
}

.platform-card h2,
.platform-card p {
  margin: 0;
}

.platform-card .platform-card__author {
  margin-top: var(--nf-space-5);
  font-weight: 700;
}

.platform-card h2 {
  font-family: var(--nf-font-serif);
  font-size: 1.25rem;
}

.platform-card p {
  margin-top: var(--nf-space-3);
  color: var(--nf-color-text-muted);
  font-size: 0.85rem;
  line-height: 1.5;
}

.settings-groups {
  display: grid;
  gap: var(--nf-space-5);
}

.settings-card {
  padding: var(--nf-space-5);
}

.settings-card__heading {
  padding-bottom: var(--nf-space-4);
  border-bottom: 1px solid var(--nf-color-border);
}

.settings-card__heading h2,
.settings-card__heading p {
  margin: 0;
}

.update-setting {
  display: flex;
  gap: var(--nf-space-4);
  align-items: center;
  justify-content: space-between;
  padding-top: var(--nf-space-4);
  border-top: 1px solid var(--nf-color-border);
}

.update-setting strong,
.update-setting p {
  margin: 0;
}

.update-setting p {
  margin-top: var(--nf-space-1);
  color: var(--nf-color-text-muted);
  font-size: 0.85rem;
  line-height: 1.45;
}

.update-setting .update-setting__status {
  color: var(--nf-color-success);
  font-weight: 700;
}

.update-setting .update-setting__error {
  color: var(--nf-color-danger);
}

.update-setting .nf-button {
  flex: 0 0 auto;
}

.update-setting .update-setting__button--current {
  border-color: color-mix(in srgb, var(--nf-color-success), var(--nf-color-border) 35%);
  color: var(--nf-color-success);
}

.settings-card__heading h2 {
  font-family: var(--nf-font-serif);
  font-size: 1.4rem;
}

.settings-card__heading p {
  margin-top: var(--nf-space-1);
  color: var(--nf-color-text-muted);
  font-size: 0.87rem;
}

.select-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--nf-space-4);
  padding-top: var(--nf-space-4);
}

.select-grid label {
  display: grid;
  gap: var(--nf-space-2);
  color: var(--nf-color-text-muted);
  font-size: 0.82rem;
  font-weight: 700;
}

.select-grid select,
.select-grid input {
  width: 100%;
  min-height: 2.8rem;
  padding: 0 var(--nf-space-3);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
  color: var(--nf-color-text);
}

.settings-message {
  display: flex;
  gap: var(--nf-space-2);
  align-items: center;
  padding: var(--nf-space-3) var(--nf-space-4);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-success);
  font-weight: 700;
}

.settings-message--error {
  background: color-mix(in srgb, var(--nf-color-danger), transparent 88%);
  color: var(--nf-color-danger);
}

.settings-actions {
  display: flex;
  gap: var(--nf-space-4);
  align-items: center;
  justify-content: space-between;
}

.settings-actions p {
  margin: 0;
  color: var(--nf-color-text-muted);
  font-size: 0.85rem;
}

.settings-app-meta {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: var(--nf-space-3);
  margin-top: var(--nf-space-6);
  color: var(--nf-color-text-muted);
  font-size: 0.8rem;
}

.settings-repository-link {
  min-width: 2.25rem;
  padding: var(--nf-space-2);
}

@media (max-width: 52rem) {
  .settings-layout {
    grid-template-columns: 1fr;
  }

  .platform-card {
    position: static;
    grid-template-columns: auto 1fr;
  }
}

@media (max-width: 36rem) {
  .settings-header > ion-icon {
    display: none;
  }

  .select-grid {
    grid-template-columns: 1fr;
  }

  .settings-actions {
    align-items: stretch;
    flex-direction: column;
  }

  .update-setting {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
