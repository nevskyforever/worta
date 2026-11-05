<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, computed } from 'vue'
import type { BrowserCloudAccountSession } from '@/platform/web/cloudAccountSession'
import { BrowserCloudReadError, type BrowserCloudProjectRead } from '@/platform/web/cloudRead'
import { BrowserPersistenceError } from '@/platform/web/persistence'
import { useLocaleStore } from '@/stores/locale'
import type { SupportedLanguage } from '@/types/api'
const props = defineProps<{ session: BrowserCloudAccountSession; projectId: string }>()
const emit = defineEmits<{ invalidated: [] }>()
const locale = useLocaleStore(), t = locale.translate
const copy: Record<SupportedLanguage, { open: string; partial: string; blocked: string; network: string; register: string }> = {
  ru: { open: 'Открыть облачный проект', partial: 'Проверенные настройки проекта с сервера. Тексты, этапы, заметки и другие данные ещё не открыты.', blocked: 'Чтение заблокировано', network: 'Не удалось подключиться к серверу. Повторите попытку.', register: 'Зарегистрировать браузер для доступа к облаку' },
  en: { open: 'Open cloud project', partial: 'Verified project settings from the server. Texts, stages, Notes and other data have not been opened yet.', blocked: 'Reading is blocked', network: 'Could not connect to the server. Try again.', register: 'Register this browser for cloud access' },
  es: { open: 'Abrir proyecto en la nube', partial: 'Ajustes verificados del servidor. Los textos, etapas, notas y otros datos aún no se han abierto.', blocked: 'Lectura bloqueada', network: 'No se pudo conectar al servidor. Inténtalo de nuevo.', register: 'Registrar este navegador para acceder a la nube' },
  de: { open: 'Cloudprojekt öffnen', partial: 'Geprüfte Projekteinstellungen vom Server. Texte, Etappen, Notizen und andere Daten sind noch nicht geöffnet.', blocked: 'Lesen ist gesperrt', network: 'Keine Verbindung zum Server. Erneut versuchen.', register: 'Browser für Cloudzugriff registrieren' },
  fr: { open: 'Ouvrir le projet cloud', partial: 'Paramètres vérifiés du serveur. Les textes, étapes, notes et autres données ne sont pas encore ouverts.', blocked: 'Lecture bloquée', network: 'Connexion au serveur impossible. Réessayez.', register: 'Enregistrer ce navigateur pour accéder au cloud' },
  pt_BR: { open: 'Abrir projeto na nuvem', partial: 'Configurações verificadas do servidor. Textos, etapas, notas e outros dados ainda não foram abertos.', blocked: 'Leitura bloqueada', network: 'Não foi possível conectar ao servidor. Tente novamente.', register: 'Registrar este navegador para acessar a nuvem' },
}
const words = computed(() => copy[locale.language])
const recoveryLabels: Record<SupportedLanguage, { open: string; heading: string }> = {
  ru: { open: 'Открыть заметки для чтения', heading: 'Заметки' },
  en: { open: 'Open notes for reading', heading: 'Notes' },
  es: { open: 'Abrir notas para leer', heading: 'Notas' },
  de: { open: 'Notizen zum Lesen öffnen', heading: 'Notizen' },
  fr: { open: 'Ouvrir les notes en lecture', heading: 'Notes' },
  pt_BR: { open: 'Abrir notas para leitura', heading: 'Notas' },
}
const result = ref<BrowserCloudProjectRead | null>(null), busy = ref(false), error = ref<string | null>(null)
const recovery = ref<Awaited<ReturnType<BrowserCloudAccountSession['readRecoveryProject']>> | null>(null)
let controller: AbortController | null = null
let disposed = false
const unsubscribe = props.session.onRevoked(() => {
  controller?.abort(); result.value = null; recovery.value = null; error.value = null
  emit('invalidated')
})
async function open(): Promise<void> {
  if (disposed) return
  controller?.abort()
  const active = new AbortController(); controller = active
  result.value = null; recovery.value = null; error.value = null; busy.value = true
  try {
    const value = await props.session.readCloudProject(props.projectId, active.signal)
    if (!active.signal.aborted && props.session.status === 'unlocked') result.value = value
  } catch (cause) {
    if (active.signal.aborted) return
    if (props.session.status !== 'unlocked') { emit('invalidated'); return }
    error.value = cause instanceof BrowserCloudReadError || cause instanceof BrowserPersistenceError ? cause.code : 'connection_failed'
  } finally { if (controller === active) busy.value = false }
}
async function register(): Promise<void> {
  busy.value = true; error.value = null
  try { await props.session.registerCloudReadDevice(); await open() }
  catch { if (props.session.status !== 'unlocked') emit('invalidated'); else error.value = 'connection_failed' }
  finally { busy.value = false }
}
async function openRecovery(): Promise<void> {
  if (disposed) return
  controller?.abort(); const active = new AbortController(); controller = active
  recovery.value = null; error.value = null; busy.value = true
  try {
    const value = await props.session.readRecoveryProject(props.projectId, active.signal)
    if (!active.signal.aborted && props.session.status === 'unlocked') recovery.value = value
  } catch (cause) {
    if (active.signal.aborted) return
    if (props.session.status !== 'unlocked') { emit('invalidated'); return }
    error.value = cause instanceof BrowserCloudReadError || cause instanceof BrowserPersistenceError ? cause.code : 'connection_failed'
  } finally { if (controller === active) busy.value = false }
}
onMounted(() => { void open() })
onBeforeUnmount(() => { disposed = true; unsubscribe(); controller?.abort(); result.value = null; recovery.value = null })
</script>

<template>
  <section data-testid="cloud-project-read" :data-project-id="projectId">
    <button class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="open">{{ busy ? t('Загружаем с сервера…') : words.open }}</button>
    <button data-testid="cloud-notes-open" class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="openRecovery">{{ recoveryLabels[locale.language].open }}</button>
    <p v-if="error" role="alert" :data-read-code="error">{{ error === 'connection_failed' ? words.network : `${words.blocked}: ${error}` }}</p>
    <button v-if="error === 'sync_device_not_registered'" class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="register">{{ words.register }}</button>
    <article v-if="result" data-testid="cloud-project-metadata">
      <h3>{{ result.metadata.name }}</h3>
      <p>{{ words.partial }}</p>
      <dl>
        <dt>{{ t('Цель') }}</dt><dd>{{ result.metadata.goal ?? t('Без лимита') }}</dd>
        <dt>{{ t('Статус') }}</dt><dd>{{ result.metadata.status }}</dd>
        <dt>{{ t('Дедлайн') }}</dt><dd>{{ result.metadata.deadline ?? '—' }}</dd>
      </dl>
    </article>
    <article v-if="recovery" data-testid="cloud-project-notes">
      <h3>{{ recovery.project.name }}</h3>
      <h4>{{ recoveryLabels[locale.language].heading }}</h4>
      <section v-for="note in recovery.notes" :key="note.id"><h5>{{ note.title }}</h5><p style="white-space: pre-wrap">{{ note.content }}</p></section>
    </article>
  </section>
</template>
