<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { adminSession } from '@/api/admin'
import { useLocaleStore } from '@/stores/locale'

const router = useRouter()
const t = useLocaleStore().translate
const username = ref('')
const password = ref('')
const error = ref('')
const loading = ref(false)
async function submit(): Promise<void> {
  loading.value = true; error.value = ''
  try { await adminSession.login(username.value, password.value); await router.replace('/admin') }
  catch (reason) { error.value = reason instanceof Error ? reason.message : t('Не удалось войти.') }
  finally { loading.value = false }
}
</script>
<template><main class="admin-login"><form @submit.prevent="submit"><h1>{{t('Администрирование')}} · worta</h1><label>{{t('Имя пользователя')}}<input v-model="username" autocomplete="username" required /></label><label>{{t('Пароль')}}<input v-model="password" type="password" autocomplete="current-password" required /></label><p v-if="error" role="alert">{{ error }}</p><button class="nf-button" :disabled="loading">{{ loading ? t('Входим…') : t('Войти') }}</button></form></main></template>
<style scoped>
.admin-login { min-height: 100dvh; display: grid; place-items: center; padding: 1.5rem; color: var(--nf-color-text); background: var(--nf-color-surface-muted); }
.admin-login form { display: grid; gap: 1rem; width: min(28rem, 100%); padding: clamp(1.25rem, 4vw, 2rem); border: 1px solid var(--nf-color-border); border-radius: .75rem; background: var(--nf-color-surface); }
h1 { margin: 0 0 .5rem; font-size: 1.5rem; }
label { display: grid; gap: .4rem; font-weight: 600; }
input { width: 100%; min-height: 2.75rem; padding: .65rem; border: 1px solid var(--nf-color-border); border-radius: .4rem; color: var(--nf-color-text); background: var(--nf-color-surface); font-size: 1rem; }
button { min-height: 2.75rem; font-size: 1rem; }
p[role="alert"] { margin: 0; color: var(--nf-color-text); }
</style>
