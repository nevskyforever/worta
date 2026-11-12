import { diagnostics } from './diagnostics/service'
import { safeError } from './diagnostics/events'
import { createApp } from 'vue'
import { createPinia } from 'pinia'
import { IonicVue } from '@ionic/vue'

import '@ionic/vue/css/core.css'
import '@ionic/vue/css/normalize.css'
import '@ionic/vue/css/structure.css'
import '@ionic/vue/css/typography.css'
import '@ionic/vue/css/padding.css'
import '@ionic/vue/css/display.css'

import App from './App.vue'
import { initializePlatformRuntime } from './platform/runtime'
import { currentPlatform } from './platform/runtime'
import router from './router'
import { useCloudSessionStore } from './stores/cloudSession'
import { useMotionStore } from './stores/motion'
import { useThemeStore } from './stores/theme'
import './theme/tokens.css'
import './theme/global.css'

async function bootstrap(): Promise<void> {
  await initializePlatformRuntime()

  const app = createApp(App)
  const pinia = createPinia()

  app.use(pinia)
  diagnostics.record('application', 'runtime_start', 'succeeded')
  app.config.errorHandler = error => { diagnostics.record('application', 'runtime_start', 'failed', undefined, safeError(error), 'error') }
  window.addEventListener('unhandledrejection', event => { diagnostics.record('application', 'load', 'failed', undefined, safeError(event.reason), 'error') })
  app.use(IonicVue)
  app.use(router)

  const theme = useThemeStore(pinia)
  theme.initialize()
  const motion = useMotionStore(pinia)
  motion.initialize()

  await router.isReady()
  // Notes cloud sessions are desktop-only and deliberately do not share the
  // administrator route or the legacy document-sync timer.
  if (currentPlatform() === 'tauri' && router.currentRoute.value.meta.admin !== true
      && router.currentRoute.value.meta.standalone !== true) {
    useCloudSessionStore(pinia).initialize()
  }
  app.mount('#app')
}

void bootstrap()
