import { createApp, h } from 'vue'
import { createPinia } from 'pinia'
import BrowserCloudProjectRead from '../src/components/settings/BrowserCloudProjectRead.vue'
import type { BrowserCloudAccountSession } from '../src/platform/web/cloudAccountSession'

export function mountRecoveryPanel(root: HTMLElement, session: BrowserCloudAccountSession, projectId: string) {
  const app = createApp({ render: () => h(BrowserCloudProjectRead, { session, projectId }) })
  app.use(createPinia())
  app.mount(root)
  return app
}
