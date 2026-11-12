<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch, watchEffect } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { IonIcon, IonRouterOutlet } from '@ionic/vue'
import {
  chevronBackOutline,
  chevronForwardOutline,
  cloudOfflineOutline,
  contrastOutline,
  codeSlashOutline,
  documentTextOutline,
  folderOpenOutline,
  gitBranchOutline,
  helpCircleOutline,
  languageOutline,
  settingsOutline,
  sparklesOutline,
} from 'ionicons/icons'

import { useNetworkStatus } from '@/composables/useNetworkStatus'
import { apiErrorMessage } from '@/api/client'
import { settingsApi } from '@/api/settings'
import DeveloperModeDialog from '@/components/developer/DeveloperModeDialog.vue'
import StreakBadge from '@/components/projects/StreakBadge.vue'
import { projectsApi } from '@/api/projects'
import { announceDataChange, onDataChange } from '@/services/dataChanges'
import {
  SUPPORTED_LANGUAGES,
  isSupportedLanguage,
  useLocaleStore,
} from '@/stores/locale'
import {
  isThemePreference,
  useThemeStore,
  type ThemePreference,
} from '@/stores/theme'
import type { GlobalStreakSummary, SupportedLanguage } from '@/types/api'
import type { GameState } from '@/types/game'

const props = defineProps<{ cloudWorkspace?: boolean; locked?: boolean }>()
const emit = defineEmits<{ projects: [] }>()
const { online } = useNetworkStatus()
const route = useRoute()
const router = useRouter()
const theme = useThemeStore()
const locale = useLocaleStore()
const t = locale.translate
const appIcon = '/icons/icon-192.webp'
const startupError = props.cloudWorkspace ? '' : window.__NFPROGRESS_RUNTIME__?.startupError ?? ''
const savingPreference = ref(false)
const preferenceError = ref<string | null>(null)
const developerAvailable = ref(false)
const developerDialogOpen = ref(false)
const globalStreak = ref<GlobalStreakSummary | null>(null)
let stopDataChanges: (() => void) | undefined
let writingDayTimer: number | undefined
let lastWritingDay: string | null = null
let writingDayRefreshInFlight = false
const WRITING_DAY_CHECK_INTERVAL_MS = 5 * 60 * 1000
const NOTES_WORKSPACE_ROUTES = new Set([
  'project-notes',
  'global-project-notes',
  'global-project-map',
])
const hasBanner = computed(() => !online.value || Boolean(startupError))
const routerOutletKey = computed(() => (
  NOTES_WORKSPACE_ROUTES.has(String(route.name)) ? route.path : 'app'
))
const lastProjectPath = ref('/projects')
const lastMapsPath = ref('/maps')
const lastNotesPath = ref('/notes')
const SIDEBAR_COLLAPSED_STORAGE_KEY = 'nfprogress.sidebar-collapsed'
const sidebarCollapsed = ref(!props.cloudWorkspace)
const appShell = ref<HTMLElement | null>(null)
let sidebarAnimationFrame: number | undefined
let sidebarAnimationToken = 0
try {
  const saved = sessionStorage.getItem('nfprogress:last-project-path')
  if (saved?.startsWith('/projects')) lastProjectPath.value = saved
  const savedMaps = localStorage.getItem('nfprogress:last-maps-path')
  const savedNotes = localStorage.getItem('nfprogress:last-notes-path')
  const savedSidebarState = localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY)
  if (savedSidebarState === 'true' || savedSidebarState === 'false') {
    sidebarCollapsed.value = savedSidebarState === 'true'
  }
  if (savedMaps?.startsWith('/maps')) lastMapsPath.value = savedMaps
  if (savedNotes?.startsWith('/notes')) lastNotesPath.value = savedNotes
} catch {
  // Session persistence is optional in restricted embedded webviews.
}
const navigationItems = computed(() => props.cloudWorkspace ? [
  { to: '/cloud', home: '/cloud', label: 'Проекты', mobileLabel: 'Проекты', icon: folderOpenOutline },
] : [
  { to: lastProjectPath.value, home: '/projects', label: 'Проекты', mobileLabel: 'Проекты', icon: folderOpenOutline },
  { to: lastMapsPath.value, home: '/maps', label: 'Карты', mobileLabel: 'Карты', icon: gitBranchOutline },
  { to: lastNotesPath.value, home: '/notes', label: 'Заметки', mobileLabel: 'Заметки', icon: documentTextOutline },
  { to: '/texts', home: '/texts', label: 'Тексты', mobileLabel: 'Тексты', icon: documentTextOutline },
  { to: '/game', home: '/game', label: 'Игра', mobileLabel: 'Игра', icon: sparklesOutline },
  { to: '/help', home: '/help', label: 'Помощь', mobileLabel: 'Помощь', icon: helpCircleOutline },
  { to: '/settings', home: '/settings', label: 'Настройки', mobileLabel: 'Ещё', icon: settingsOutline },
] as const)
const sidebarToggleLabel = computed(() => (
  sidebarCollapsed.value ? t('Развернуть меню') : t('Свернуть меню')
))
const showSidebarStreak = computed(() => (
  route.name !== 'projects' && globalStreak.value?.enabled === true
))

async function toggleSidebar(): Promise<void> {
  const shell = appShell.value
  const sidebar = shell?.querySelector<HTMLElement>('.sidebar')
  const startWidth = sidebar?.getBoundingClientRect().width

  sidebarAnimationToken += 1
  const animationToken = sidebarAnimationToken
  if (sidebarAnimationFrame !== undefined) {
    window.cancelAnimationFrame(sidebarAnimationFrame)
    sidebarAnimationFrame = undefined
  }

  if (shell && sidebar && startWidth) {
    shell.style.gridTemplateColumns = `${startWidth}px minmax(0, 1fr)`
  }

  sidebarCollapsed.value = !sidebarCollapsed.value
  try {
    localStorage.setItem(SIDEBAR_COLLAPSED_STORAGE_KEY, String(sidebarCollapsed.value))
  } catch { /* optional */ }

  if (!shell || !sidebar || !startWidth) return

  await nextTick()
  if (animationToken !== sidebarAnimationToken) return
  shell.style.removeProperty('grid-template-columns')
  const endWidth = sidebar.getBoundingClientRect().width
  shell.style.gridTemplateColumns = `${startWidth}px minmax(0, 1fr)`
  void shell.offsetWidth

  // Keep the transition visible even when the system asks to reduce motion;
  // use a shorter duration instead of removing the feedback entirely.
  const duration = window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 160 : 220
  const startedAt = performance.now()
  const animateSidebar = (now: number): void => {
    if (animationToken !== sidebarAnimationToken) return
    const progress = Math.min((now - startedAt) / duration, 1)
    const easedProgress = progress < 0.5
      ? 2 * progress ** 2
      : 1 - ((-2 * progress + 2) ** 2) / 2
    const width = startWidth + (endWidth - startWidth) * easedProgress
    shell.style.gridTemplateColumns = `${width}px minmax(0, 1fr)`
    if (progress < 1) {
      sidebarAnimationFrame = window.requestAnimationFrame(animateSidebar)
      return
    }
    shell.style.removeProperty('grid-template-columns')
    sidebarAnimationFrame = undefined
  }
  sidebarAnimationFrame = window.requestAnimationFrame(animateSidebar)
}

watch(() => route.fullPath, (path) => {
  if (props.cloudWorkspace) return
  try {
    if (path.startsWith('/projects')) {
      lastProjectPath.value = path
      sessionStorage.setItem('nfprogress:last-project-path', path)
    } else if (path.startsWith('/maps')) {
      lastMapsPath.value = path
      localStorage.setItem('nfprogress:last-maps-path', path)
    } else if (path.startsWith('/notes')) {
      lastNotesPath.value = path
      localStorage.setItem('nfprogress:last-notes-path', path)
    }
  } catch { /* optional */ }
}, { immediate: true })

function returnToSectionHome(event: MouseEvent, home: string): void {
  if (props.cloudWorkspace) {
    event.preventDefault()
    emit('projects')
    return
  }
  const currentPath = route.fullPath.split(/[?#]/, 1)[0] ?? ''
  if (currentPath !== home && !currentPath.startsWith(`${home}/`)) return

  event.preventDefault()
  if (route.fullPath !== home) void router.push(home)
}

function isTypingTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLInputElement
    || target instanceof HTMLTextAreaElement
    || target instanceof HTMLSelectElement
    || (target instanceof HTMLElement && target.isContentEditable)
}

function handleShortcut(event: KeyboardEvent): void {
  if (isTypingTarget(event.target)) return
  const key = event.key.toLowerCase()
  const modified = event.ctrlKey || event.metaKey
  if (modified && key === 'd' && developerAvailable.value) {
    event.preventDefault()
    developerDialogOpen.value = true
    return
  }
  if (!modified && key !== 'delete') return
  if (modified && key === 'h' && !event.shiftKey) {
    event.preventDefault()
    void router.push({ name: 'help' })
    return
  }
  if (modified && key === 'n') {
    event.preventDefault()
    void router.push({ name: 'projects' }).then(() => window.dispatchEvent(new Event('nfprogress:new-project')))
    return
  }
  if (route.name === 'project-detail' || route.name === 'stage-detail') {
    const action = event.shiftKey && key === 'c' ? 'complete'
      : event.shiftKey && key === 's' ? 'statistics'
        : event.shiftKey && key === 'h' ? 'archive'
          : key === 's' ? 'sync'
      : key === 'e' ? 'edit'
        : key === 'delete' ? 'delete'
          : null
    if (action) {
      event.preventDefault()
      window.dispatchEvent(new CustomEvent<string>('nfprogress:project-shortcut', { detail: action }))
    }
  }
}

function publishDeveloperState(state: GameState): void {
  window.dispatchEvent(new CustomEvent<GameState>('nfprogress:game-state-updated', {
    detail: state,
  }))
  // A changed test clock recalculates project plans and streak statuses on the
  // backend. Refresh every active project view immediately.
  announceDataChange('projects')
}

async function refreshGlobalStreak(): Promise<void> {
  try {
    const settings = await settingsApi.get()
    if (settings.values.global_streak !== true) {
      globalStreak.value = null
      return
    }
    globalStreak.value = await projectsApi.globalStreak()
  } catch {
    globalStreak.value = null
  }
}

async function refreshWritingDay(): Promise<void> {
  if (writingDayRefreshInFlight) return
  writingDayRefreshInFlight = true
  try {
    // The backend owns the custom start-of-day rule. Polling this lightweight
    // summary keeps an already open workspace in sync even when no user
    // action happens at the writing-day boundary.
    const summary = await projectsApi.today()
    const previousDay = lastWritingDay
    lastWritingDay = summary.date
    if (previousDay !== null && previousDay !== summary.date) {
      announceDataChange('projects')
    }
  } catch {
    // The next interval retries; temporary offline state is shown separately.
  } finally {
    writingDayRefreshInFlight = false
  }
}

onMounted(() => {
  if (props.cloudWorkspace) return
  window.addEventListener('keydown', handleShortcut)
  void settingsApi.get().then((settings) => {
    developerAvailable.value = settings.capabilities.developer_mode_available === true
  })
  void refreshGlobalStreak()
  void refreshWritingDay()
  writingDayTimer = window.setInterval(() => { void refreshWritingDay() }, WRITING_DAY_CHECK_INTERVAL_MS)
  stopDataChanges = onDataChange((scope) => {
    if (scope === 'projects' || scope === 'game') void refreshGlobalStreak()
  })
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleShortcut)
  if (writingDayTimer !== undefined) window.clearInterval(writingDayTimer)
  sidebarAnimationToken += 1
  if (sidebarAnimationFrame !== undefined) window.cancelAnimationFrame(sidebarAnimationFrame)
  sidebarAnimationFrame = undefined
  appShell.value?.style.removeProperty('grid-template-columns')
  stopDataChanges?.()
})

const themeLabel = computed(() => {
  const labels: Record<ThemePreference, string> = {
    system: t('Системная тема'),
    light: t('Светлая тема'),
    dark: t('Тёмная тема'),
  }
  return labels[theme.preference]
})

async function selectTheme(event: Event): Promise<void> {
  if (savingPreference.value) return
  const select = event.currentTarget as HTMLSelectElement
  const requestedTheme = select.value
  if (!isThemePreference(requestedTheme) || requestedTheme === theme.preference) return

  savingPreference.value = true
  preferenceError.value = null
  try {
    const settings = await settingsApi.update({ frontend_theme: requestedTheme })
    theme.setPreference(
      isThemePreference(settings.values.frontend_theme)
        ? settings.values.frontend_theme
        : requestedTheme,
    )
  } catch (error) {
    select.value = theme.preference
    preferenceError.value = t(apiErrorMessage(error))
  } finally {
    savingPreference.value = false
  }
}

async function selectLanguage(event: Event): Promise<void> {
  if (savingPreference.value) return
  const select = event.currentTarget as HTMLSelectElement
  const requestedLanguage = select.value as SupportedLanguage
  if (!isSupportedLanguage(requestedLanguage) || requestedLanguage === locale.language) return

  savingPreference.value = true
  preferenceError.value = null
  try {
    const settings = await settingsApi.update({ language: requestedLanguage })
    await locale.setLanguage(
      isSupportedLanguage(settings.values.language)
        ? settings.values.language
        : requestedLanguage,
    )
  } catch (error) {
    select.value = locale.language
    preferenceError.value = t(apiErrorMessage(error))
  } finally {
    savingPreference.value = false
  }
}

watchEffect(() => {
  if (props.cloudWorkspace) return
  const sourceTitle = typeof route.meta.title === 'string' ? route.meta.title : ''
  document.title = sourceTitle ? `${t(sourceTitle)} · nfprogress` : 'nfprogress'
})
</script>

<template>
  <a v-if="!locked" class="skip-link" href="#main-content">{{ t('Перейти к содержимому') }}</a>

  <div ref="appShell" class="app-shell" :class="{ 'app-shell--sidebar-collapsed': sidebarCollapsed, 'app-shell--cloud': cloudWorkspace, 'app-shell--locked': locked }">
    <aside v-if="!locked" class="sidebar" :aria-label="t('Основная навигация')">
      <div class="sidebar-header">
        <RouterLink class="brand" :to="cloudWorkspace ? '/cloud' : '/projects'" :aria-label="`worta · ${t('Проекты')}`" @click="cloudWorkspace && returnToSectionHome($event, '/cloud')">
          <img class="brand-mark" :src="appIcon" alt="" />
          <span>
            <strong>worta</strong>
            <small>{{ t('Пространство писателя') }}</small>
          </span>
        </RouterLink>
        <button
          class="sidebar-toggle"
          type="button"
          :aria-label="sidebarToggleLabel"
          :title="sidebarToggleLabel"
          :aria-expanded="!sidebarCollapsed"
          aria-controls="primary-navigation"
          @click="toggleSidebar"
        >
          <IonIcon
            :icon="sidebarCollapsed ? chevronForwardOutline : chevronBackOutline"
            aria-hidden="true"
          />
        </button>
      </div>

      <StreakBadge
        v-if="showSidebarStreak && globalStreak"
        class="sidebar-global-streak"
        :length="globalStreak.length"
        :max-length="globalStreak.max_length"
        :status="globalStreak.status"
        scope="global"
        :compact="sidebarCollapsed"
        show-max
      />

      <nav id="primary-navigation" class="primary-navigation" :aria-label="t('Разделы приложения')">
        <RouterLink
          v-for="item in navigationItems"
          :key="item.to"
          class="navigation-link"
          :to="item.to"
          :aria-label="t(item.label)"
          :title="sidebarCollapsed ? t(item.label) : undefined"
          @click="returnToSectionHome($event, item.home)"
        >
          <IonIcon :icon="item.icon" aria-hidden="true" />
          <span>{{ t(item.label) }}</span>
        </RouterLink>
      </nav>

      <div v-if="!cloudWorkspace" class="sidebar-controls">
        <button
          v-if="developerAvailable"
          class="nf-button nf-button--quiet"
          type="button"
          @click="developerDialogOpen = true"
        >
          <IonIcon :icon="codeSlashOutline" aria-hidden="true" />
          {{ t('Режим разработчика') }}
        </button>
        <label class="compact-select">
          <IonIcon :icon="languageOutline" aria-hidden="true" />
          <span class="visually-hidden">{{ t('Язык интерфейса') }}</span>
          <select
            :value="locale.language"
            :aria-label="t('Язык интерфейса')"
            :disabled="savingPreference"
            :aria-busy="savingPreference"
            @change="selectLanguage"
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

        <label class="compact-select">
          <IonIcon :icon="contrastOutline" aria-hidden="true" />
          <span class="visually-hidden">{{ t('Тема') }}</span>
          <select
            :value="theme.preference"
            :aria-label="t('Тема')"
            :disabled="savingPreference"
            :aria-busy="savingPreference"
            @change="selectTheme"
          >
            <option value="system">{{ t('Системная') }}</option>
            <option value="light">{{ t('Светлая') }}</option>
            <option value="dark">{{ t('Тёмная') }}</option>
          </select>
        </label>
        <p class="theme-summary">{{ themeLabel }}</p>
        <p v-if="preferenceError" class="preference-error" role="alert">
          {{ preferenceError }}
        </p>
      </div>
    </aside>

    <div
      v-if="startupError"
      class="network-banner network-banner--error"
      role="alert"
    >
      <IonIcon :icon="cloudOfflineOutline" aria-hidden="true" />
      {{ t('Не удалось запустить приложение. Попробуйте повторить запуск.') }}
    </div>
    <div
      v-else-if="!online && !locked"
      class="network-banner"
      role="status"
      aria-live="polite"
    >
      <IonIcon :icon="cloudOfflineOutline" aria-hidden="true" />
      {{ t('Нет сети. Изменения станут доступны после восстановления подключения.') }}
    </div>

    <main
      id="main-content"
      class="app-main"
      :class="{ 'app-main--with-banner': hasBanner }"
      tabindex="-1"
    >
      <slot><IonRouterOutlet :key="routerOutletKey" /></slot>
    </main>

    <nav v-if="!locked" class="mobile-navigation" :aria-label="t('Основная навигация')">
      <RouterLink
        v-for="item in navigationItems"
        :key="item.to"
        class="mobile-navigation-link"
        :to="item.to"
        :aria-label="t(item.label)"
        @click="cloudWorkspace && returnToSectionHome($event, item.home)"
      >
        <IonIcon :icon="item.icon" aria-hidden="true" />
        <span>{{ t(item.mobileLabel) }}</span>
      </RouterLink>
    </nav>
    <DeveloperModeDialog
      v-if="!cloudWorkspace"
      :open="developerDialogOpen"
      @close="developerDialogOpen = false"
      @updated="publishDeveloperState"
    />
  </div>
</template>

<style scoped>
.app-shell--cloud .app-main { overflow: auto; }
.app-shell--locked { display: block; }
.app-shell--locked .app-main { height: 100dvh; }
.app-shell--cloud .mobile-navigation { grid-template-columns: minmax(0, 1fr); }
.preference-error {
  margin: var(--nf-space-1) var(--nf-space-2) 0;
  color: var(--nf-color-danger);
  font-size: 0.75rem;
  line-height: 1.35;
}

:deep(.sidebar-global-streak) {
  width: 100%;
  margin-top: var(--nf-space-4);
  border-radius: var(--nf-radius-md);
}

:deep(.sidebar-global-streak.streak-badge--compact) {
  display: grid;
  min-width: max-content;
  grid-template-columns: minmax(0, 1fr);
  grid-template-rows: auto auto;
  justify-items: center;
  align-items: center;
}

:deep(.sidebar-global-streak.streak-badge--compact .streak-badge__copy) {
  grid-column: 1;
  min-width: 0;
  justify-content: center;
  flex-wrap: wrap;
}

:deep(.sidebar-global-streak.streak-badge--compact > ion-icon) {
  grid-column: 1;
  grid-row: auto;
}
</style>
