<script setup lang="ts">
import { useRoute, useRouter } from 'vue-router'
import BrowserCloudAccountCard from '@/components/settings/BrowserCloudAccountCard.vue'
import { webAccountSession } from '@/platform/web/webAccountSession'

const route = useRoute()
const router = useRouter()
const session = webAccountSession()

function syncRoute(status: typeof session.status): void {
  const target = status === 'unlocked' ? 'web-workspace' : 'web-login'
  if (route.name !== target && route.meta.webAccount) void router.replace({ name: target })
}
</script>

<template>
  <main class="web-account">
    <h1>worta</h1>
    <BrowserCloudAccountCard :session="session" :workspace-page="route.name === 'web-workspace'" @status-change="syncRoute" />
  </main>
</template>

<style scoped>
.web-account { max-width: 58rem; width: 100%; margin: 0 auto; padding: clamp(1rem, 4vw, 3rem); }
.web-account > h1 { font-family: var(--nf-font-serif); font-size: clamp(2rem, 6vw, 3.5rem); }
</style>
