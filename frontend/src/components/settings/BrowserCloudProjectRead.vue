<script setup lang="ts">
import BrowserAccountGameRead from './BrowserAccountGameRead.vue'
import BrowserProjectGameRead from './BrowserProjectGameRead.vue'
import BrowserProgressRead from './BrowserProgressRead.vue'
import MindMapEditor from '@/components/notes/MindMapEditor.vue'
import NoteCard from '@/components/notes/NoteCard.vue'
import StatePanel from '@/components/ui/StatePanel.vue'
import type { MindMapResponse, JsonObject } from '@/types/notes'
import { onBeforeUnmount, onMounted, ref, computed, defineAsyncComponent, watch } from 'vue'
import type { BrowserCloudAccountSession } from '@/platform/web/cloudAccountSession'
import { cloudDocumentForReading, cloudNoteCard, readCloudWorkspaceProject } from '@/platform/web/cloudWorkspaceReader'
import { BrowserCloudReadError, type BrowserCloudProjectRead } from '@/platform/web/cloudRead'
import { BrowserPersistenceError } from '@/platform/web/persistence'
import { BrowserReplayError } from '@/platform/web/replayRecords'
import { useLocaleStore } from '@/stores/locale'
import type { SupportedLanguage } from '@/types/api'
const props = defineProps<{ session: BrowserCloudAccountSession; projectId: string; autoOpenRecovery?: boolean; workspace?: boolean }>()
const emit = defineEmits<{ invalidated: [] }>()
const locale = useLocaleStore(), t = locale.translate
const NFDocumentEditor = defineAsyncComponent(() => import('@/components/documents/editor/NFDocumentEditor.vue'))
const copy: Record<SupportedLanguage, { open: string; partial: string; blocked: string; network: string; register: string }> = {
  ru: { open: 'Открыть облачный проект', partial: 'Проверенные настройки проекта с сервера. Тексты, этапы, заметки и другие данные ещё не открыты.', blocked: 'Чтение заблокировано', network: 'Не удалось подключиться к серверу. Повторите попытку.', register: 'Зарегистрировать браузер для доступа к облаку' },
  en: { open: 'Open cloud project', partial: 'Verified project settings from the server. Texts, stages, Notes and other data have not been opened yet.', blocked: 'Reading is blocked', network: 'Could not connect to the server. Try again.', register: 'Register this browser for cloud access' },
  es: { open: 'Abrir proyecto en la nube', partial: 'Ajustes verificados del servidor. Los textos, etapas, notas y otros datos aún no se han abierto.', blocked: 'Lectura bloqueada', network: 'No se pudo conectar al servidor. Inténtalo de nuevo.', register: 'Registrar este navegador para acceder a la nube' },
  de: { open: 'Cloudprojekt öffnen', partial: 'Geprüfte Projekteinstellungen vom Server. Texte, Etappen, Notizen und andere Daten sind noch nicht geöffnet.', blocked: 'Lesen ist gesperrt', network: 'Keine Verbindung zum Server. Erneut versuchen.', register: 'Browser für Cloudzugriff registrieren' },
  fr: { open: 'Ouvrir le projet cloud', partial: 'Paramètres vérifiés du serveur. Les textes, étapes, notes et autres données ne sont pas encore ouverts.', blocked: 'Lecture bloquée', network: 'Connexion au serveur impossible. Réessayez.', register: 'Enregistrer ce navigateur pour accéder au cloud' },
  pt_BR: { open: 'Abrir projeto na nuvem', partial: 'Configurações verificadas do servidor. Textos, etapas, notas e outros dados ainda não foram abertos.', blocked: 'Leitura bloqueada', network: 'Não foi possível conectar ao servidor. Tente novamente.', register: 'Registrar este navegador para acessar a nuvem' },
}
const words = computed(() => copy[locale.language])
const emptyProject: Record<SupportedLanguage, string> = {
  ru: 'В проекте пока нет содержимого', en: 'This project has no content yet',
  es: 'El proyecto aún no tiene contenido', de: 'Dieses Projekt hat noch keine Inhalte',
  fr: 'Ce projet ne contient encore aucun contenu', pt_BR: 'Este projeto ainda não tem conteúdo',
}
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
  ru: { open: 'Открыть проект для чтения', heading: 'Заметки', unavailable: 'Не удалось безопасно проверить историю проекта. Она может содержать пока не поддерживаемые данные.' },
  en: { open: 'Open project for reading', heading: 'Notes', unavailable: 'The project history could not be verified safely. It may contain data that is not supported yet.' },
  es: { open: 'Abrir proyecto para leer', heading: 'Notas', unavailable: 'No se pudo verificar el historial del proyecto de forma segura. Puede contener datos aún no compatibles.' },
  de: { open: 'Projekt zum Lesen öffnen', heading: 'Notizen', unavailable: 'Der Projektverlauf konnte nicht sicher geprüft werden. Er kann noch nicht unterstützte Daten enthalten.' },
  fr: { open: 'Ouvrir le projet en lecture', heading: 'Notes', unavailable: 'L’historique du projet n’a pas pu être vérifié en toute sécurité. Il peut contenir des données non encore prises en charge.' },
  pt_BR: { open: 'Abrir projeto para leitura', heading: 'Notas', unavailable: 'Não foi possível verificar o histórico do projeto com segurança. Ele pode conter dados ainda não compatíveis.' },
}
const result = ref<BrowserCloudProjectRead | null>(null), busy = ref(false), error = ref<string | null>(null)
const stageLabels: Record<SupportedLanguage, { stages: string; project: string }> = {
  ru: { stages: 'Этапы', project: 'Проект' }, en: { stages: 'Stages', project: 'Project' },
  es: { stages: 'Etapas', project: 'Proyecto' }, de: { stages: 'Etappen', project: 'Projekt' },
  fr: { stages: 'Étapes', project: 'Projet' }, pt_BR: { stages: 'Etapas', project: 'Projeto' },
}
const documentUnavailable: Record<SupportedLanguage, string> = {
  ru: 'Этот тип истории документов пока недоступен в Web. История проекта не открыта.',
  en: 'This document history is not supported on Web yet. The project history was not opened.',
  es: 'Este historial de documentos aún no es compatible con Web. No se abrió el historial.',
  de: 'Dieser Dokumentverlauf wird im Web noch nicht unterstützt. Der Verlauf wurde nicht geöffnet.',
  fr: 'Cet historique de documents n’est pas encore pris en charge sur le Web. L’historique n’a pas été ouvert.',
  pt_BR: 'Este histórico de documentos ainda não é compatível com a Web. O histórico não foi aberto.',
}
const documentLabels: Record<SupportedLanguage, string> = {
  ru: 'Документы', en: 'Documents', es: 'Documentos', de: 'Dokumente', fr: 'Documents', pt_BR: 'Documentos',
}
const readOnlyLabels: Record<SupportedLanguage, string> = {
  ru: 'Проект доступен для чтения. Редактирование в Web пока недоступно.',
  en: 'You can read this project. Editing in Web is not yet available.',
  es: 'Puedes leer este proyecto. La edición en la Web aún no está disponible.',
  de: 'Du kannst dieses Projekt lesen. Die Bearbeitung im Web ist noch nicht verfügbar.',
  fr: 'Vous pouvez consulter ce projet. La modification sur le Web est encore indisponible.',
  pt_BR: 'Você pode ler este projeto. A edição na Web ainda não está disponível.',
}
// Only authenticated Tiptap text is presented. No source HTML, URLs or mark
// attributes enter the DOM; Vue escapes even literal markup inside text nodes.
function documentText(node: Record<string, unknown>): string {
  if (node.type === 'text') return typeof node.text === 'string' ? node.text : ''
  if (node.type === 'hardBreak') return '\n'
  const content = Array.isArray(node.content) ? node.content.map(child => documentText(child as Record<string, unknown>)).join('') : ''
  return content + (['paragraph', 'heading', 'codeBlock'].includes(String(node.type)) ? '\n' : '')
}
const selectedDocument = ref<string | null>(null)
const stageDocuments = computed(() => recovery.value?.documents?.filter(document => document.stage_id === selectedStage.value) ?? [])
const selectedMap = computed(() => recovery.value?.maps?.find(map => map.project_id === props.projectId && map.stage_id === selectedStage.value))
const mapAnnotations = computed(() => Object.entries(selectedMap.value?.annotations ?? {}).map(([node, value]) => ({ node, ...(value as { title: string; tags: string[]; checklist: { id: string; text: string; checked: boolean }[] }) })))
const projectMap = computed<MindMapResponse | null>(() => {
  const value = selectedMap.value
  return value ? { project_id: props.projectId, stage_id: selectedStage.value, name: recovery.value!.project.name, data: value.data as JsonObject,
    combined: false, read_only: true, has_empty_completed_stage_map: false } : null
})
const refuseMapWrite = async (): Promise<never> => { throw new Error('read_only') }
const activeDocument = computed(() => stageDocuments.value.find(document => document.id === selectedDocument.value))
const documentForReading = computed(() => activeDocument.value && cloudDocumentForReading(activeDocument.value))
const stageNotes = computed(() => recovery.value?.notes.filter(note => note.stage_id === selectedStage.value) ?? [])
const selectedStage = ref<string | null>(null)
// UI preference only: opaque account/project/Stage IDs, never text or keys.
function selectionKey(): string | null {
  const account = props.session.currentIdentity?.local_account_id
  return account ? `worta:recovery-stage:${JSON.stringify([account, props.projectId])}` : null
}
function rememberedStage(): string | null | undefined {
  try {
    const key = selectionKey(), raw = key && window.localStorage.getItem(key)
    if (!raw) return undefined
    const value: unknown = JSON.parse(raw)
    return value === null || typeof value === 'string' ? value : undefined
  } catch { return undefined } // Optional view preference; it grants no authority.
}
function rememberStage(): void {
  try { const key = selectionKey(); if (key) window.localStorage.setItem(key, JSON.stringify(selectedStage.value)) }
  catch { /* Reading remains available when optional browser preferences are blocked. */ }
}
function selectStage(id: string | null): void {
  if (id !== selectedStage.value) selectedDocument.value = null
  selectedStage.value = id; rememberStage()
}
const recoveryError = ref(false)
const recovery = ref<Awaited<ReturnType<BrowserCloudAccountSession['readRecoveryProject']>> | null>(null)
let controller: AbortController | null = null
let disposed = false
const unsubscribe = props.session.onRevoked(() => {
  controller?.abort(); result.value = null; recovery.value = null; selectedDocument.value = null; error.value = null; recoveryError.value = false
  emit('invalidated')
})
async function open(): Promise<void> {
  if (disposed) return
  controller?.abort()
  const active = new AbortController(); controller = active
  result.value = null; recovery.value = null; selectedDocument.value = null; error.value = null; recoveryError.value = false; busy.value = true
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
  const priorDocument = selectedDocument.value, priorSelection = selectedStage.value, hadRecovery = recovery.value !== null
  controller?.abort(); const active = new AbortController(); controller = active
  result.value = null; recovery.value = null; selectedDocument.value = null; error.value = null; recoveryError.value = true; busy.value = true
  try {
    const opened = props.workspace
      ? await readCloudWorkspaceProject(props.session, props.projectId, active.signal)
      : { kind: 'project' as const, project: await props.session.readRecoveryProject(props.projectId, active.signal) }
    if (opened.kind === 'empty') {
      if (!active.signal.aborted && props.session.status === 'unlocked') result.value = opened.metadata
      return
    }
    const value = opened.project
    if (!active.signal.aborted && props.session.status === 'unlocked') {
      recovery.value = value
      const remembered = hadRecovery ? priorSelection : rememberedStage()
      selectedStage.value = remembered === null || value.stages.some(stage => stage.id === remembered)
        ? remembered! : value.games?.some(game=>game.event.header.stage_id===null) || value.progress?.some(progress => progress.stage_id === null) || value.notes.some(note => note.stage_id === null) || value.documents?.some(document => document.stage_id === null) || value.maps?.some(map => map.project_id === props.projectId && map.stage_id === null) ? null : value.stages[0]?.id ?? null
      selectedDocument.value = value.documents?.some(document => document.id === priorDocument && document.stage_id === selectedStage.value) ? priorDocument : null
      rememberStage()
    }
  } catch (cause) {
    if (active.signal.aborted) return
    if (props.session.status !== 'unlocked') { emit('invalidated'); return }
    error.value = cause instanceof BrowserCloudReadError || cause instanceof BrowserPersistenceError || cause instanceof BrowserReplayError ? cause.code : 'unsafe_history'
  } finally { if (controller === active) busy.value = false }
}
onMounted(() => { if (props.autoOpenRecovery) void openRecovery(); else void open() })
watch(() => props.projectId, () => { if (props.autoOpenRecovery) void openRecovery() })
onBeforeUnmount(() => { disposed = true; unsubscribe(); controller?.abort(); result.value = null; recovery.value = null; selectedDocument.value = null })
</script>

<template>
  <section data-testid="cloud-project-read" :data-project-id="projectId">
    <template v-if="!workspace">
      <button class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="open">{{ busy ? t('Загружаем с сервера…') : words.open }}</button>
      <button data-testid="cloud-notes-open" class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="openRecovery">{{ recoveryLabels[locale.language].open }}</button>
    </template>
    <StatePanel v-if="workspace && busy" :title="t('Загружаем с сервера…')" loading />
    <p v-if="error" role="alert" :data-read-code="error">{{ error === 'connection_failed' ? words.network : error === 'document_recovery_unavailable' ? documentUnavailable[locale.language] : recoveryError ? recoveryLabels[locale.language].unavailable : `${words.blocked}: ${error}` }}</p>
    <button v-if="workspace && error" class="nf-button nf-button--secondary" type="button" @click="openRecovery">{{ t('Повторить загрузку') }}</button>
    <article v-if="result" data-testid="cloud-project-metadata">
      <h3>{{ result.metadata.name }}</h3>
      <StatePanel v-if="result.empty" data-testid="cloud-project-empty" :title="emptyProject[locale.language]" />
      <p v-else>{{ words.partial }}</p>
      <dl v-if="!result.empty">
        <dt>{{ t('Цель') }}</dt><dd>{{ result.metadata.goal ?? t('Без лимита') }}</dd>
        <dt>{{ t('Статус') }}</dt><dd>{{ result.metadata.status }}</dd>
        <dt>{{ t('Дедлайн') }}</dt><dd>{{ result.metadata.deadline ?? '—' }}</dd>
      </dl>
    </article>
    <article v-if="recovery" :class="{ 'cloud-reader': workspace }" data-testid="cloud-project-notes">
      <h3>{{ recovery.project.name }}</h3>
      <p v-if="workspace" class="cloud-reader__notice">{{ readOnlyLabels[locale.language] }}</p>
      <nav v-if="recovery.stages.length" class="cloud-reader__stages" :aria-label="stageLabels[locale.language].stages" data-testid="cloud-stage-list">
        <button class="nf-button nf-button--secondary" type="button" :aria-pressed="selectedStage === null" data-testid="cloud-project-notes-select" @click="selectStage(null)">{{ stageLabels[locale.language].project }}</button>
        <button v-for="stage in recovery.stages" :key="stage.id" class="nf-button nf-button--secondary" type="button" :data-stage-id="stage.id" :aria-pressed="selectedStage === stage.id" @click="selectStage(stage.id)">{{ stage.name }}</button>
      </nav>
      <BrowserAccountGameRead v-if="recovery.accountGame" :value="recovery.accountGame" />
      <BrowserProjectGameRead v-for="value in recovery.games?.filter(value=>value.event.header.stage_id===selectedStage)" :key="value.event.header.event_id" :value="value" />
      <BrowserProgressRead v-for="value in recovery.progress?.filter(value => value.stage_id === selectedStage)" :key="value.event_id" :value="value" />
      <section v-if="stageDocuments.length" class="cloud-reader__panel" data-testid="cloud-document-list">
        <h4>{{ documentLabels[locale.language] }}</h4>
        <div class="cloud-reader__stages">
          <button v-for="document in stageDocuments" :key="document.id" class="nf-button nf-button--secondary" type="button" :data-document-id="document.id" :aria-pressed="selectedDocument === document.id" @click="selectedDocument = document.id">{{ document.title }}</button>
        </div>
      </section>
      <section v-if="activeDocument" class="cloud-reader__panel" data-testid="cloud-document-read">
        <h5>{{ activeDocument.title }}</h5>
        <NFDocumentEditor v-if="workspace && documentForReading" :key="activeDocument.id" :initial-content="documentForReading" :translate="t" read-only />
        <p v-else data-testid="cloud-document-text" style="white-space: pre-wrap">{{ documentText(activeDocument.content_json) }}</p>
      </section>
      <section v-if="projectMap" data-testid="cloud-map-read">
        <h4>{{ t('Карта') }}</h4>
        <MindMapEditor :key="recovery.maps?.find(map => map.stage_id === selectedStage)?.event_id" :map="projectMap" secure-read-only :persist="refuseMapWrite" :import-x-mind="refuseMapWrite" />
        <ul data-testid="cloud-map-annotations"><li v-for="annotation in mapAnnotations" :key="annotation.node">
          <strong>{{ annotation.title }}</strong><span v-for="tag in annotation.tags" :key="tag"> {{ tag }}</span>
          <ul><li v-for="item in annotation.checklist" :key="item.id">{{ item.checked ? '☑' : '☐' }} {{ item.text }}</li></ul>
        </li></ul>
      </section>
      <section v-if="workspace && stageNotes.length" class="cloud-reader__panel">
        <h4>{{ recoveryLabels[locale.language].heading }}</h4>
        <div class="cloud-reader__notes">
          <NoteCard v-for="note in stageNotes" :key="note.id" :note="cloudNoteCard(note)" reading>
            <iframe v-if="note.content_format === 'html'" data-testid="cloud-note-html" :title="note.title" sandbox="" referrerpolicy="no-referrer" :srcdoc="noteHtmlDocument(note.content)" />
            <p v-else class="cloud-reader__note-text">{{ note.content }}</p>
          </NoteCard>
        </div>
      </section>
      <template v-if="!workspace">
        <h4>{{ recoveryLabels[locale.language].heading }}</h4>
        <section v-for="note in stageNotes" :key="note.id">
          <h5>{{ note.title }}</h5>
          <iframe v-if="note.content_format === 'html'" data-testid="cloud-note-html" :title="note.title" sandbox="" referrerpolicy="no-referrer" :srcdoc="noteHtmlDocument(note.content)" style="width: 100%; min-height: 12rem; border: 0" />
          <p v-else style="white-space: pre-wrap">{{ note.content }}</p>
        </section>
      </template>
    </article>
  </section>
</template>

<style scoped>
.cloud-reader { display: grid; gap: var(--nf-space-4); min-width: 0; overflow-wrap: anywhere; }
.cloud-reader h3, .cloud-reader h4, .cloud-reader h5, .cloud-reader p { margin: 0; }
.cloud-reader h3 { font: clamp(1.5rem, 4vw, 2.4rem) var(--nf-font-serif); }
.cloud-reader__notice { color: var(--nf-color-text-muted); }
.cloud-reader__stages { display: flex; flex-wrap: wrap; gap: var(--nf-space-2); }
.cloud-reader__stages [aria-pressed='true'] { border-color: var(--nf-color-primary); color: var(--nf-color-primary); }
.cloud-reader__panel { display: grid; gap: var(--nf-space-3); min-width: 0; }
.cloud-reader__notes { display: grid; gap: var(--nf-space-3); grid-template-columns: repeat(auto-fit, minmax(min(100%, 19rem), 1fr)); }
.cloud-reader__notes iframe { width: 100%; min-height: 12rem; border: 0; }
.cloud-reader__note-text { white-space: pre-wrap; }
</style>
