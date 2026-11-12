<script setup lang="ts">
import { ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import AppShell from '@/layouts/AppShell.vue'
import BrowserCloudAccountCard from '@/components/settings/BrowserCloudAccountCard.vue'
import { webAccountSession } from '@/platform/web/webAccountSession'

const route = useRoute()
const router = useRouter()
const session = webAccountSession()
const accountCard = ref<InstanceType<typeof BrowserCloudAccountCard> | null>(null)

function syncRoute(status: typeof session.status): void {
  const target = status === 'unlocked' ? 'web-workspace' : 'web-login'
  if (route.name !== target && route.meta.webAccount) void router.replace({ name: target })
}
</script>

<template>
  <AppShell cloud-workspace :locked="route.name !== 'web-workspace'" @projects="accountCard?.showProjects()">
    <div class="web-account" :class="{ 'web-account--workspace': route.name === 'web-workspace' }">
      <h1 v-if="route.name !== 'web-workspace'">worta</h1>
      <BrowserCloudAccountCard ref="accountCard" :session="session" :workspace-page="route.name === 'web-workspace'" @status-change="syncRoute" />
    </div>
  </AppShell>
</template>

<style scoped>
.web-account { max-width: 58rem; width: 100%; margin: 0 auto; padding: clamp(1rem, 4vw, 3rem); }
.web-account h1 { font-family: var(--nf-font-serif); font-size: clamp(2rem, 6vw, 3.5rem); }
.web-account--workspace { max-width: 90rem; }
</style>
