<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { IonApp, IonRouterOutlet } from '@ionic/vue'

import { apiErrorMessage } from '@/api/client'
import { settingsApi } from '@/api/settings'
import UserAgreementGate from '@/components/agreement/UserAgreementGate.vue'
import NotificationCenter from '@/components/ui/NotificationCenter.vue'
import NotificationStack from '@/components/ui/NotificationStack.vue'
import UpdatePrompt from '@/components/ui/UpdatePrompt.vue'
import AppShell from '@/layouts/AppShell.vue'
import { isWorkspaceWindow } from '@/platform/workspaceWindows'
import { skipE2eLegalGate } from '@/platform/e2eLegalGate'
import { currentPlatform, isDesktopTestProfile, supportsUpdateChecks } from '@/platform/runtime'
import { useCloudSessionStore } from '@/stores/cloudSession'
import { isSupportedLanguage, useLocaleStore } from '@/stores/locale'
import { isMotionPreference, useMotionStore } from '@/stores/motion'
import { useNotificationsStore } from '@/stores/notifications'
import { isThemePreference, useThemeStore } from '@/stores/theme'
import { useUpdaterStore } from '@/stores/updater'
import router from '@/router'
import { onAdminSessionInvalidated } from '@/api/admin'
import type { SettingsResponse } from '@/types/content'

type BootstrapState = 'loading' | 'agreement' | 'ready' | 'error'

const locale = useLocaleStore()
const theme = useThemeStore()
const motion = useMotionStore()
const notifications = useNotificationsStore()
const updater = useUpdaterStore()
const cloudSession = useCloudSessionStore()
const t = locale.translate
const appIcon = '/icons/icon-192.webp'
const bootstrapState = ref<BootstrapState>('loading')
const bootstrapError = ref<string | null>(null)
let updateTimer: number | null = null
const workspaceWindow = isWorkspaceWindow()
const isStandaloneRoute = computed(() => router.currentRoute.value.meta.admin === true
  || router.currentRoute.value.meta.standalone === true)

function startAutomaticUpdateChecks(): void {
  if (!supportsUpdateChecks() || updateTimer !== null) return
  void updater.checkForUpdates()
  updateTimer = window.setInterval(() => {
    void updater.checkForUpdates()
  }, 60 * 60 * 1000)
}

async function applyBackendPreferences(settings: SettingsResponse): Promise<void> {
  if (isThemePreference(settings.values.frontend_theme)) {
    theme.setPreference(settings.values.frontend_theme)
  }
  motion.setPreference(
    isMotionPreference(settings.values.frontend_motion)
      ? settings.values.frontend_motion
      : 'full',
  )
  const language = isSupportedLanguage(settings.values.language)
    ? settings.values.language
    : locale.language
  notifications.setDurationSeconds(settings.values.notification_display_time)
  await locale.setLanguage(language)
}

async function bootstrapApplication(): Promise<void> {
  bootstrapState.value = 'loading'
  bootstrapError.value = null
  try {
    const settings = await settingsApi.get()
    await applyBackendPreferences(settings)
    bootstrapState.value = settings.values.user_agreement === true || isDesktopTestProfile() || skipE2eLegalGate() ? 'ready' : 'agreement'
    if (bootstrapState.value === 'ready') startAutomaticUpdateChecks()
  } catch (error) {
    await locale.initialize()
    bootstrapError.value = t(apiErrorMessage(error))
    bootstrapState.value = 'error'
  }
}

async function handleAgreementAccepted(settings: SettingsResponse): Promise<void> {
  await applyBackendPreferences(settings)
  bootstrapState.value = 'ready'
  startAutomaticUpdateChecks()
}

watch(isStandaloneRoute, (standalone) => {
  if (standalone) {
    void locale.initialize()
  } else if (bootstrapState.value === 'loading') {
    void bootstrapApplication()
  }
  if (!standalone && currentPlatform() === 'tauri') cloudSession.initialize()
}, { immediate: true })
onAdminSessionInvalidated(() => {
  if (router.currentRoute.value.meta.admin === true) void router.replace('/admin/login')
})
onBeforeUnmount(() => {
  if (updateTimer !== null) window.clearInterval(updateTimer)
  void cloudSession.dispose()
  onAdminSessionInvalidated(null)
})
</script>

<template>
  <IonApp>
    <IonRouterOutlet v-if="isStandaloneRoute" />
    <div
      v-else-if="bootstrapState === 'loading'"
      class="application-bootstrap"
      role="status"
      aria-live="polite"
    >
      <img class="application-bootstrap__mark" :src="appIcon" alt="" />
      <p>{{ t('Запускаем nfprogress…').replace(/nfprogress/gi, 'worta') }}</p>
    </div>

    <div
      v-else-if="bootstrapState === 'error'"
      class="application-bootstrap"
      role="alert"
    >
      <img class="application-bootstrap__mark" :src="appIcon" alt="" />
      <h1>{{ t('Не удалось открыть nfprogress').replace(/nfprogress/gi, 'worta') }}</h1>
      <p>{{ bootstrapError ?? t('Не удалось подключиться к приложению.') }}</p>
      <button class="nf-button" type="button" @click="bootstrapApplication">
        {{ t('Повторить') }}
      </button>
    </div>

    <UserAgreementGate
      v-else-if="bootstrapState === 'agreement'"
      @accepted="handleAgreementAccepted"
    />
    <template v-else>
      <div v-if="workspaceWindow" class="workspace-window">
        <IonRouterOutlet />
      </div>
      <template v-else>
        <AppShell />
        <NotificationCenter />
        <NotificationStack />
        <UpdatePrompt />
      </template>
    </template>
  </IonApp>
</template>

<style scoped>
.application-bootstrap {
  display: grid;
  min-height: 100dvh;
  gap: var(--nf-space-3);
  align-content: center;
  justify-items: center;
  padding: var(--nf-space-5);
  background: var(--nf-color-canvas);
  color: var(--nf-color-text);
  text-align: center;
}

.application-bootstrap h1,
.application-bootstrap p {
  max-width: 38rem;
  margin: 0;
}

.application-bootstrap h1 {
  font-family: var(--nf-font-serif);
  font-size: clamp(1.8rem, 5vw, 2.7rem);
}

.application-bootstrap p {
  color: var(--nf-color-text-muted);
  line-height: 1.5;
}

.application-bootstrap__mark {
  width: 4rem;
  height: 4rem;
  object-fit: contain;
}

.workspace-window {
  width: 100%;
  height: 100dvh;
  min-height: 100dvh;
  background: var(--nf-color-canvas);
  color: var(--nf-color-text);
}

.workspace-window :deep(ion-router-outlet) {
  display: block;
  width: 100%;
  height: 100%;
}
</style>
