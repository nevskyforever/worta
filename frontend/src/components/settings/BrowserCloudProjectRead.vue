<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, computed } from 'vue'
import type { BrowserCloudAccountSession } from '@/platform/web/cloudAccountSession'
import { BrowserCloudReadError, type BrowserCloudProjectRead } from '@/platform/web/cloudRead'
import { BrowserPersistenceError } from '@/platform/web/persistence'
import { BrowserReplayError } from '@/platform/web/replayRecords'
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
// Keep only text and basic formatting; the opaque-origin sandbox is a second barrier
// against scripts, network access and access to the authenticated parent page.
function noteHtmlDocument(content: string): string {
  const input = document.createElement('template'), output = document.createElement('template')
  input.innerHTML = content
  const allowed = new Set(['p', 'br', 'div', 'span', 'strong', 'b', 'em', 'i', 'u', 's', 'ul', 'ol', 'li', 'blockquote', 'pre', 'code', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'hr'])
  const discarded = new Set(['script', 'style', 'iframe', 'object', 'embed', 'svg', 'math', 'template'])
  function copy(node: Node, parent: Node): void {
    if (node.nodeType === Node.TEXT_NODE) { parent.appendChild(document.createTextNode(node.textContent ?? '')); return }
    if (node.nodeType !== Node.ELEMENT_NODE) return
    const tag = (node as Element).localName
    if (discarded.has(tag)) return
    const target = allowed.has(tag) ? document.createElement(tag) : parent
    if (target !== parent) parent.appendChild(target)
    for (const child of node.childNodes) copy(child, target)
  }
  for (const child of input.content.childNodes) copy(child, output.content)
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'; base-uri 'none'; form-action 'none'"><meta name="referrer" content="no-referrer"></head><body>${output.innerHTML}</body></html>`
}
const recoveryLabels: Record<SupportedLanguage, { open: string; heading: string; unavailable: string }> = {
  ru: { open: 'Открыть заметки для чтения', heading: 'Заметки', unavailable: 'Не удалось безопасно проверить историю проекта. Она может содержать пока не поддерживаемые данные.' },
  en: { open: 'Open notes for reading', heading: 'Notes', unavailable: 'The project history could not be verified safely. It may contain data that is not supported yet.' },
  es: { open: 'Abrir notas para leer', heading: 'Notas', unavailable: 'No se pudo verificar el historial del proyecto de forma segura. Puede contener datos aún no compatibles.' },
  de: { open: 'Notizen zum Lesen öffnen', heading: 'Notizen', unavailable: 'Der Projektverlauf konnte nicht sicher geprüft werden. Er kann noch nicht unterstützte Daten enthalten.' },
  fr: { open: 'Ouvrir les notes en lecture', heading: 'Notes', unavailable: 'L’historique du projet n’a pas pu être vérifié en toute sécurité. Il peut contenir des données non encore prises en charge.' },
  pt_BR: { open: 'Abrir notas para leitura', heading: 'Notas', unavailable: 'Não foi possível verificar o histórico do projeto com segurança. Ele pode conter dados ainda não compatíveis.' },
}
const result = ref<BrowserCloudProjectRead | null>(null), busy = ref(false), error = ref<string | null>(null)
const stageLabels: Record<SupportedLanguage, { stages: string; project: string }> = {
  ru: { stages: 'Этапы', project: 'Заметки проекта' }, en: { stages: 'Stages', project: 'Project notes' },
  es: { stages: 'Etapas', project: 'Notas del proyecto' }, de: { stages: 'Etappen', project: 'Projektnotizen' },
  fr: { stages: 'Étapes', project: 'Notes du projet' }, pt_BR: { stages: 'Etapas', project: 'Notas do projeto' },
}
const selectedStage = ref<string | null>(null)
const recoveryError = ref(false)
const recovery = ref<Awaited<ReturnType<BrowserCloudAccountSession['readRecoveryProject']>> | null>(null)
let controller: AbortController | null = null
let disposed = false
const unsubscribe = props.session.onRevoked(() => {
  controller?.abort(); result.value = null; recovery.value = null; error.value = null; recoveryError.value = false
  emit('invalidated')
})
async function open(): Promise<void> {
  if (disposed) return
  controller?.abort()
  const active = new AbortController(); controller = active
  result.value = null; recovery.value = null; error.value = null; recoveryError.value = false; busy.value = true
  try {
    const value = await props.session.readCloudProject(props.projectId, active.signal)
    if (!active.signal.aborted && props.session.status === 'unlocked') result.value = value
  } catch (cause) {
    if (active.signal.aborted) return
    if (props.session.status !== 'unlocked') { emit('invalidated'); return }
    error.value = cause instanceof BrowserCloudReadError || cause instanceof BrowserPersistenceError ? cause.code : 'connection_failed'
  } finally { if (controller === active) busy.value = false }
}
async function openRecovery(): Promise<void> {
  if (disposed) return
  controller?.abort(); const active = new AbortController(); controller = active
  result.value = null; recovery.value = null; error.value = null; recoveryError.value = true; busy.value = true
  try {
    const value = await props.session.readRecoveryProject(props.projectId, active.signal)
    if (!active.signal.aborted && props.session.status === 'unlocked') {
      recovery.value = value
      selectedStage.value = value.notes.some(note => note.stage_id === null) ? null : value.stages[0]?.id ?? null
    }
  } catch (cause) {
    if (active.signal.aborted) return
    if (props.session.status !== 'unlocked') { emit('invalidated'); return }
    error.value = cause instanceof BrowserCloudReadError || cause instanceof BrowserPersistenceError || cause instanceof BrowserReplayError ? cause.code : 'unsafe_history'
  } finally { if (controller === active) busy.value = false }
}
onMounted(() => { void open() })
onBeforeUnmount(() => { disposed = true; unsubscribe(); controller?.abort(); result.value = null; recovery.value = null })
</script>

<template>
  <section data-testid="cloud-project-read" :data-project-id="projectId">
    <button class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="open">{{ busy ? t('Загружаем с сервера…') : words.open }}</button>
    <button data-testid="cloud-notes-open" class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="openRecovery">{{ recoveryLabels[locale.language].open }}</button>
    <p v-if="error" role="alert" :data-read-code="error">{{ error === 'connection_failed' ? words.network : recoveryError ? recoveryLabels[locale.language].unavailable : `${words.blocked}: ${error}` }}</p>
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
      <nav v-if="recovery.stages.length" :aria-label="stageLabels[locale.language].stages" data-testid="cloud-stage-list">
        <button type="button" :aria-pressed="selectedStage === null" data-testid="cloud-project-notes-select" @click="selectedStage = null">{{ stageLabels[locale.language].project }}</button>
        <button v-for="stage in recovery.stages" :key="stage.id" type="button" :data-stage-id="stage.id" :aria-pressed="selectedStage === stage.id" @click="selectedStage = stage.id">{{ stage.name }}</button>
      </nav>
      <h4>{{ recoveryLabels[locale.language].heading }}</h4>
      <section v-for="note in recovery.notes.filter(note => note.stage_id === selectedStage)" :key="note.id">
        <h5>{{ note.title }}</h5>
        <iframe v-if="note.content_format === 'html'" data-testid="cloud-note-html" :title="note.title" sandbox="" referrerpolicy="no-referrer" :srcdoc="noteHtmlDocument(note.content)" style="width: 100%; min-height: 12rem; border: 0" />
        <p v-else style="white-space: pre-wrap">{{ note.content }}</p>
      </section>
    </article>
  </section>
</template>
