<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue'
import type { Editor } from '@tiptap/core'
import { onBeforeRouteLeave, useRouter } from 'vue-router'
import { pickDesktopWordFile, pickDesktopWordSavePath } from '@/platform/files'
import { currentPlatform, isDeveloperModeAvailable } from '@/platform/runtime'
import { useDocumentSync, type ConflictChoice } from '@/composables/useDocumentSync'
import { exportDocx } from '@/services/documentDocx'
import type { DocumentScope, TiptapDocument } from '@/types/documents'
import { projectsApi } from '@/api/projects'
import type { Project } from '@/types/api'
import { convertProjectUnit } from '@/utils/projectPlanning'
import { UNIT_FACTORS } from '@/core/projects/units'
import { announceDataChange, onDataChange } from '@/services/dataChanges'
import { progressChangeNotification } from '@/utils/progressNotifications'
import { gameResponseMessages } from '@/utils/gameNotifications'
import LocalWordBindingPanel from './LocalWordBindingPanel.vue'
import DocumentConflictResolver from './DocumentConflictResolver.vue'
import NFDocumentEditor from './editor/NFDocumentEditor.vue'
import NFEditorStatusControls from './editor/NFEditorStatusControls.vue'
import { useProjectsStore } from '@/stores/projects'
import { useLocaleStore } from '@/stores/locale'
import { useNotificationsStore } from '@/stores/notifications'

const props = defineProps<{ scope: DocumentScope; title: string }>()
const router = useRouter()
const locale = useLocaleStore()
const notifications = useNotificationsStore()
const t = locale.translate
const projectStore = useProjectsStore()
const sharedSource = computed(() => {
  const project = projectStore.currentProject?.id === props.scope.projectId ? projectStore.currentProject : projectStore.projects.find(item => item.id === props.scope.projectId)
  return project?.name === 'Общий проект'
})
type DocumentEditorExpose = {
  getEditor: () => Editor | null
  getJSON: () => TiptapDocument
  getScrollContainer: () => HTMLElement | null
  setContent: (content: TiptapDocument | string, emitUpdate?: boolean) => void
  setSelection: (position: number) => Promise<void>
}
const editorRef = shallowRef<DocumentEditorExpose | null>(null)
const editorShell = ref<HTMLElement | null>(null)
const showConflict = ref(false)
const pendingConflictResolve = ref<((choice: ConflictChoice) => void) | null>(null)
const editorContent = ref<TiptapDocument>({ type: 'doc', content: [{ type: 'paragraph' }] })
const projectEntity = ref<Project | null>(null)
type EditorViewState = {
  version: 1
  selection: number
  scrollTop: number
  zoom: number
  typewriterMode: boolean
}
function positionStorageKey(): string {
  return `nfprogress:document-position:${props.scope.projectId}:${props.scope.stageId ?? 'project'}`
}
function savedEditorViewState(): EditorViewState | null {
  try {
    const stored = JSON.parse(localStorage.getItem(positionStorageKey()) ?? '') as Partial<EditorViewState>
    if (
      typeof stored.selection !== 'number'
      || typeof stored.scrollTop !== 'number'
      || !Number.isFinite(stored.selection)
      || !Number.isFinite(stored.scrollTop)
    ) return null
    const zoom = typeof stored.zoom === 'number'
      && Number.isFinite(stored.zoom)
      && stored.zoom >= 70
      && stored.zoom <= 500
      ? stored.zoom
      : 100
    return {
      version: 1,
      selection: Math.max(1, Math.floor(stored.selection)),
      scrollTop: Math.max(0, stored.scrollTop),
      zoom,
      typewriterMode: typeof stored.typewriterMode === 'boolean' ? stored.typewriterMode : false,
    }
  } catch {
    return null
  }
}
const initialEditorViewState = savedEditorViewState()
const zoom = ref(initialEditorViewState?.zoom ?? 100)
const typewriterMode = ref(initialEditorViewState?.typewriterMode ?? false)
const canLinkWord = currentPlatform() === 'tauri'
const saving = ref(false)
const recording = ref(false)
const processing = computed(() => saving.value || recording.value)
const { content, documentState, status, save, saveAndRecord, setContent, scheduleSave, link, resolveExternal, checkExternal, acknowledgeExternal, downloadWordCopy } = useDocumentSync(props.scope)
let externalTimer: number | undefined
let stopCloseListener: (() => void) | undefined
let stopProjectDataChanges: (() => void) | undefined
let projectLoadSequence = 0
let closeInProgress = false
let externalImportInProgress = false
let positionSaveTimer: number | undefined
let positionRestoreTimer: number | undefined
let hasRestoredEditorPosition = false
const linked = computed(() => Boolean(documentState.value?.docx_path))
const showWordBindingPanel = computed(() => canLinkWord && (!linked.value || documentState.value?.sync_state !== 'synced'))
const textSymbols = computed(() => countTextSymbols(editorContent.value))
const textUnits = computed(() => {
  const entity = projectEntity.value
  if (!entity) return null
  // Desktop records fractional author sheets from the exact symbol count.
  if (canLinkWord && entity.unit === 'author_list') return textSymbols.value / UNIT_FACTORS.author_list
  return convertProjectUnit(textSymbols.value, 'symbols', entity.unit)
})
const hasRecordableDocument = computed(() => textSymbols.value > 0 || Boolean(canLinkWord && documentState.value?.exists))
const entityFractionDigits = computed(() => projectEntity.value?.unit === 'symbols' ? 0 : 2)
const entityProgressLabel = computed(() => {
  const entity = projectEntity.value
  if (!entity) return ''
  const goalLabel = entity.goal === null
    ? t('Без лимита')
    : locale.formatNumber(entity.goal, entityFractionDigits.value)
  const unitValue = entity.goal ?? entity.total
  const documentValue = textUnits.value ?? 0
  return `${locale.formatNumber(documentValue, entityFractionDigits.value)} / ${goalLabel} ${locale.formatUnit(entity.unit, unitValue)}`
})
const todayGoalCurrentValue = computed(() => {
  const entity = projectEntity.value
  if (!entity) return 0
  const unsavedChange = hasRecordableDocument.value
    ? (textUnits.value ?? entity.total) - entity.total
    : 0
  return Math.max(0, entity.added_today + unsavedChange)
})
const todayGoalRequired = computed(() => {
  const entity = projectEntity.value
  if (!entity) return null
  if (!canLinkWord) return entity.plan_daily_goal
  if (entity.today_goal === null) return null
  return entity.today_goal - Math.max(0, entity.total - entity.added_today)
})
const todayGoalCompleted = computed(() => {
  const target = todayGoalRequired.value
  if (target === null) return false
  if (canLinkWord) {
    const entity = projectEntity.value
    const projectedTotal = hasRecordableDocument.value ? textUnits.value : entity?.total
    return projectedTotal != null && projectedTotal + 1e-9 >= (entity?.today_goal ?? Infinity)
  }
  return todayGoalCurrentValue.value >= target
})
const todayGoalLabel = computed(() => {
  const entity = projectEntity.value
  if (!entity || entity.today_goal === null) return ''
  return `${locale.formatNumber(entity.today_goal, entityFractionDigits.value)} ${locale.formatUnit(entity.unit, entity.today_goal)}`
})
const todayGoalProgressPercent = computed(() => {
  const target = todayGoalRequired.value
  if (canLinkWord && todayGoalCompleted.value) return 100
  if (target === null || target <= 0) return 0
  return Math.min(100, Math.max(0, (todayGoalCurrentValue.value / target) * 100))
})
const canRecordText = computed(() => Boolean(
  projectEntity.value
  && hasRecordableDocument.value
  && textUnits.value !== null
  && Math.abs(textUnits.value - projectEntity.value.total) >= 0.009,
))

function editorScrollContainer(): HTMLElement | null {
  return editorRef.value?.getScrollContainer() ?? null
}
function toggleTypewriterMode(): void {
  typewriterMode.value = !typewriterMode.value
  schedulePositionSave()
}
function saveEditorPosition(): void {
  const editor = editorRef.value?.getEditor()
  if (!editor) return
  try {
    const selection = editor.state.selection.from
    const scrollTop = editorScrollContainer()?.scrollTop ?? 0
    localStorage.setItem(positionStorageKey(), JSON.stringify({
      version: 1,
      selection,
      scrollTop,
      zoom: zoom.value,
      typewriterMode: typewriterMode.value,
    } satisfies EditorViewState))
  } catch {
    // Position memory is optional in restricted embedded webviews.
  }
}
async function restoreEditorPosition(): Promise<boolean> {
  const saved = initialEditorViewState
  if (!saved) return true
  const editor = editorRef.value?.getEditor()
  if (!editor) return false

  await nextTick()
  const position = Math.min(saved.selection, Math.max(1, editor.state.doc.content.size))
  editor.commands.setTextSelection(position)
  editor.commands.focus()
  editor.commands.scrollIntoView()
  await nextTick()
  await new Promise<void>((resolve) => window.requestAnimationFrame(() => {
    window.requestAnimationFrame(() => resolve())
  }))
  const scrollContainer = editorScrollContainer()
  if (scrollContainer) scrollContainer.scrollTop = saved.scrollTop
  return true
}
function scheduleEditorPositionRestore(force = false): void {
  if (force) hasRestoredEditorPosition = false
  if (hasRestoredEditorPosition || positionRestoreTimer !== undefined) return
  let attempts = 0
  const attempt = async () => {
    positionRestoreTimer = undefined
    if (await restoreEditorPosition()) {
      hasRestoredEditorPosition = true
      return
    }
    attempts += 1
    if (attempts < 40) positionRestoreTimer = window.setTimeout(() => void attempt(), 50)
  }
  void attempt()
}
function schedulePositionSave(): void {
  if (positionSaveTimer !== undefined) return
  positionSaveTimer = window.setTimeout(() => {
    positionSaveTimer = undefined
    saveEditorPosition()
  }, 250)
}
function handleEditorSelectionChange(): void {
  const selection = window.getSelection()
  const anchor = selection?.anchorNode
  if (!anchor || !editorShell.value?.contains(anchor)) return
  schedulePositionSave()
}
function update(json: TiptapDocument) {
  // Never allow an asynchronous save/record cycle to replace a non-empty
  // draft with a transient empty snapshot.
  if (processing.value && countTextSymbols(json) === 0 && countTextSymbols(editorContent.value) > 0) return
  editorContent.value = json
  scheduleSave(json)
}
function captureEditorContent(): TiptapDocument {
  const latest = editorRef.value?.getJSON()
  if (!latest || typeof latest !== 'object') return content.value
  const json = latest as TiptapDocument
  if (countTextSymbols(json) === 0 && countTextSymbols(content.value) > 0) return content.value
  editorContent.value = json
  setContent(json)
  return json
}
function repairEditorSnapshot(snapshot: TiptapDocument): void {
  if (countTextSymbols(snapshot) === 0) return
  const editorApi = editorRef.value
  const editor = editorApi?.getEditor()
  if (editor && countTextSymbols(editor.getJSON()) > 0) return

  editorContent.value = snapshot
  setContent(snapshot)
  if (editorApi) {
    editorApi.setContent(snapshot, false)
    scheduleEditorPositionRestore(true)
  }
}
function countTextSymbols(value: unknown): number {
  if (!value || typeof value !== 'object') return 0
  const node = value as { text?: unknown; content?: unknown }
  return (typeof node.text === 'string' ? Array.from(node.text).length : 0)
    + (Array.isArray(node.content) ? node.content.reduce((total, child) => total + countTextSymbols(child), 0) : 0)
}
function setZoom(next: number) {
  zoom.value = Math.min(500, Math.max(70, next))
  schedulePositionSave()
}
async function loadProjectEntity() {
  const sequence = ++projectLoadSequence
  try {
    const project = await projectsApi.get(props.scope.projectId)
    if (sequence !== projectLoadSequence) return
    projectEntity.value = props.scope.stageId
      ? project.stages.find((stage) => stage.id === props.scope.stageId) ?? null
      : project
  } catch (error) {
    if (sequence === projectLoadSequence) projectEntity.value = null
    throw error
  }
}
function resolveConflict(choice: ConflictChoice) { showConflict.value = false; pendingConflictResolve.value?.(choice); pendingConflictResolve.value = null }
function chooseExternalVersion(): Promise<ConflictChoice> {
  showConflict.value = true
  return new Promise<ConflictChoice>((resolve) => { pendingConflictResolve.value = resolve })
}
async function recordTextProgress(force = false, snapshot = captureEditorContent()): Promise<boolean> {
  if (recording.value || !hasRecordableDocument.value || (!force && !canRecordText.value)) return false
  recording.value = true
  try {
    const result = await saveAndRecord(snapshot)
    if (!result.progress) {
      status.value = t('Документ не изменился. Текущий объём уже актуален.')
      return false
    }
    announceDataChange('projects')
    const progress = result.progress
    const entity = props.scope.stageId
      ? progress.project.stages.find((stage) => stage.id === props.scope.stageId) ?? projectEntity.value
      : progress.project
    projectEntity.value = entity ?? null
    if (entity) {
      const feedback = progressChangeNotification(progress, entity, t, locale.formatNumber, locale.formatUnit)
      if (feedback) notifications.show(feedback.message, feedback.kind)
    }
    if (progress.game) {
      notifications.setGameHistory(progress.game.state.notifications)
      for (const message of gameResponseMessages(progress.game)) notifications.success(t(message))
      announceDataChange('game')
    }
    if (progress.warning) notifications.warning(progress.warning)
    status.value = t('Запись прогресса добавлена.')
    return true
  } catch (error) {
    status.value = t(error instanceof Error ? error.message : 'Не удалось сохранить')
    return false
  } finally {
    recording.value = false
  }
}
let flushPromise: Promise<void> | null = null
async function flushAndRecord(recordProgress = false): Promise<void> {
  if (flushPromise) return flushPromise
  const operation = (async () => {
    const snapshot = captureEditorContent()
    try {
      if (recordProgress) {
        const recorded = await recordTextProgress(true, snapshot)
        // The document itself may have changed even when rounding/duplicate
        // protection correctly produced no progress entry. Keep project counters
        // and cached detail views in sync with that saved content as well.
        if (!recorded) announceDataChange('projects')
        return
      }
      saving.value = true
      try {
        await save(true, snapshot)
      } catch (error) {
        status.value = t(error instanceof Error ? error.message : 'Не удалось сохранить')
      } finally {
        saving.value = false
      }
    } finally {
      await nextTick()
      repairEditorSnapshot(snapshot)
    }
  })()
  flushPromise = operation
  try {
    await operation
  } finally {
    if (flushPromise === operation) flushPromise = null
  }
}
function onRecordClick(): void { void flushAndRecord(true) }
function closeEditor() {
  if (props.scope.stageId) {
    void router.push({ name: 'stage-detail', params: { projectId: props.scope.projectId, stageId: props.scope.stageId } })
    return
  }
  void router.push({ name: 'project-detail', params: { projectId: props.scope.projectId } })
}
function handleEscape(event: KeyboardEvent): void {
  if (event.key !== 'Escape' || showConflict.value || processing.value) return
  event.preventDefault()
  closeEditor()
}
async function importExternal() {
  if (externalImportInProgress) return
  externalImportInProgress = true
  try {
    const external = await checkExternal()
    const editorApi = editorRef.value
    if (!external || !editorApi) return
    const editor = editorApi.getEditor()
    if (!editor) return

    const current = editor.getJSON() as TiptapDocument
    if (external.state === 'conflict' || countTextSymbols(current) > 0) {
      const choice = await chooseExternalVersion()
      if (choice === 'nfprogress') {
        await resolveExternal('cloud')
        status.value = t('Сохранено')
        return
      }
      if (choice === 'both') await downloadWordCopy()
    }

    if (external.content) {
      editorApi.setContent(external.content, true)
    } else if (external.html) {
      editorApi.setContent(external.html, true)
    }
    const json = external.content ?? editor.getJSON() as TiptapDocument
    editorContent.value = json
    await acknowledgeExternal(json, external.hash)
  } finally {
    externalImportInProgress = false
  }
}
async function decideLocalFile(choice: 'compare' | 'cloud' | 'import' | 'unlink') {
  try { await resolveExternal(choice) }
  catch (error) {
    const code=String(error)
    status.value=t(code.includes('head_stale') ? 'Версия WORTA изменилась после сравнения' : code.includes('hash_stale') ? 'Word изменился после сравнения' : code.includes('source_missing') ? 'Локальный файл не найден. Переподключите его.' : 'Локальный источник требует повторной проверки')
  }
}
async function linkWord() {
  try {
    const path = await pickDesktopWordFile('Связать с файлом Word', 'Word')
    if (!path) return
    await link(path)
    status.value = t('Файл Word связан')
  } catch (error) {
    status.value = t(error instanceof Error ? error.message : 'Произошла непредвиденная ошибка.')
  }
}
async function exportWord() {
  if (currentPlatform() === 'tauri') {
    const targetPath = await pickDesktopWordSavePath('Экспортировать документ Word', `${props.title}.docx`)
    if (!targetPath) return
    const { invoke } = await import('@tauri-apps/api/core')
    await invoke('export_word_document', { command: { content: editorContent.value, targetPath } })
    return
  }
  const blob = await exportDocx(editorContent.value); const url = URL.createObjectURL(blob); const anchor = document.createElement('a')
  anchor.href = url; anchor.download = `${props.title}.docx`; anchor.click(); URL.revokeObjectURL(url)
}
function importWord() { const input = document.createElement('input'); input.type = 'file'; input.accept = '.docx'; input.onchange = async () => { const file = input.files?.[0]; if (!file || !editorRef.value) return; if (currentPlatform() === 'tauri') { const parsed = await (await import('@/api/documents')).documentsApi.parseWord(new Uint8Array(await file.arrayBuffer()), file.name); setContent(parsed.content); editorRef.value.setContent(parsed.content, true); return } const { importDocx } = await import('@/services/documentDocx'); const html = await importDocx(await file.arrayBuffer()); editorRef.value.setContent(html, true) }; input.click() }

watch(content, (next) => { editorContent.value = next }, { deep: true })
watch(documentState, async (next) => {
  if (!next) return
  await nextTick()
  scheduleEditorPositionRestore()
})
onMounted(() => {
  window.addEventListener('keydown', handleEscape, true)
  window.addEventListener('pagehide', saveEditorPosition)
  document.addEventListener('selectionchange', handleEditorSelectionChange)
  editorShell.value?.addEventListener('scroll', schedulePositionSave, true)
  externalTimer = window.setInterval(() => void importExternal().catch(() => undefined), 5000)
  void loadProjectEntity().catch(() => undefined)
  stopProjectDataChanges = onDataChange((scope) => {
    if (scope === 'projects') void loadProjectEntity().catch(() => undefined)
  })
  if (window.__TAURI_INTERNALS__) {
    void import('@tauri-apps/api/window').then(async ({ getCurrentWindow }) => {
      stopCloseListener = await getCurrentWindow().onCloseRequested(async (event) => {
        event.preventDefault()
        if (closeInProgress) return
        closeInProgress = true
        try {
          saveEditorPosition()
          await flushAndRecord()
        } finally {
          const removeCloseListener = stopCloseListener
          stopCloseListener = undefined
          removeCloseListener?.()
          await getCurrentWindow().destroy()
        }
      })
    })
  }
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleEscape, true)
  window.removeEventListener('pagehide', saveEditorPosition)
  document.removeEventListener('selectionchange', handleEditorSelectionChange)
  editorShell.value?.removeEventListener('scroll', schedulePositionSave, true)
  if (positionSaveTimer !== undefined) window.clearTimeout(positionSaveTimer)
  if (positionRestoreTimer !== undefined) window.clearTimeout(positionRestoreTimer)
  saveEditorPosition()
  window.clearInterval(externalTimer)
  projectLoadSequence += 1
  stopProjectDataChanges?.()
  stopCloseListener?.()
})
onBeforeRouteLeave(async () => { saveEditorPosition(); await flushAndRecord() })
</script>

<template>
  <section class="document-editor-view">
    <header class="document-editor-view__header">
      <div class="document-editor-view__title">
        <button class="document-editor-view__back" type="button" :title="scope.stageId ? t(sharedSource ? 'Вернуться к источнику' : 'Этап') : t('Вернуться к проекту')" :aria-label="scope.stageId ? t(sharedSource ? 'Вернуться к источнику' : 'Этап') : t('Вернуться к проекту')" @click="closeEditor">← {{ scope.stageId ? t(sharedSource ? 'К источнику' : 'Этап') : t('К проекту') }}</button>
        <div><p class="document-editor-view__eyebrow">Текст</p><h1>{{ title }}</h1></div>
      </div>
      <div class="document-editor-view__actions">
        <span aria-live="polite">{{ status }}</span>
        <button class="nf-button" type="button" :disabled="processing || !canRecordText" @click="onRecordClick">
          {{ processing ? t('Сохраняем…') : t('Добавить запись') }}
        </button>
        <button class="nf-button nf-button--secondary" type="button" title="Импортировать документ Word" aria-label="Импортировать документ Word" @click="importWord">Импорт DOCX</button>
        <button class="nf-button nf-button--secondary" type="button" title="Экспортировать документ Word" aria-label="Экспортировать документ Word" @click="exportWord">Экспорт DOCX</button>

      </div>

    </header>
    <LocalWordBindingPanel v-if="showWordBindingPanel" :attached="linked" :state="documentState?.sync_state" :external-content="documentState?.external_content" :show-conflict-actions="isDeveloperModeAvailable()" @reattach="linkWord" @decision="decideLocalFile" @cancel="status=''" />
    <div class="document-editor-view__workspace">
      <div ref="editorShell" class="document-editor-view__editor-shell">
        <NFDocumentEditor
          v-if="documentState"
          ref="editorRef"
          class="document-editor-view__custom-editor"
          :initial-content="content"
          :translate="t"
          :zoom="zoom"
          :typewriter-mode="typewriterMode"
          @update="update"
        />
      </div>
      <footer class="document-editor-view__statusbar" aria-label="Статус документа">
        <div v-if="projectEntity" class="document-editor-view__status-info">
          <span class="document-editor-view__unit-count" aria-live="polite">{{ t('Прогресс') }}: <strong>{{ entityProgressLabel }}</strong></span>
          <span
            v-if="projectEntity.today_goal !== null"
            class="document-editor-view__today-goal"
            :class="{ 'document-editor-view__today-goal--complete': todayGoalCompleted }"
            role="status"
          >
            <template v-if="todayGoalCompleted">{{ t('Цель на день выполнена!') }}</template>
            <template v-else>{{ t('Цель на сегодня') }}: <strong>{{ todayGoalLabel }}</strong></template>
            <span
              class="document-editor-view__today-goal-progress"
              role="progressbar"
              :aria-label="t('Цель на сегодня')"
              aria-valuemin="0"
              aria-valuemax="100"
              :aria-valuenow="Math.round(todayGoalProgressPercent)"
            >
              <span class="document-editor-view__today-goal-progress-fill" :style="{ width: `${todayGoalProgressPercent}%` }" />
            </span>
          </span>
        </div>
        <span v-else class="document-editor-view__unit-count">{{ t('Единицы проекта загружаются…') }}</span>
        <NFEditorStatusControls :zoom="zoom" :typewriter-mode="typewriterMode" :translate="t" @toggle-typewriter="toggleTypewriterMode" @zoom="setZoom" />
      </footer>
    </div>
    <DocumentConflictResolver v-if="showConflict" @resolve="resolveConflict" />
  </section>
</template>

<style scoped>
.document-editor-view {
  width: min(100%, 88rem);
  margin: 0 auto;
  padding: var(--nf-space-5);
  color: var(--nf-color-text);
}
.document-editor-view__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: .75rem;
}
.document-editor-view__title { display: grid; gap: .35rem; }
.document-editor-view__back {
  display: inline-flex;
  align-items: center;
  width: max-content;
  min-height: 2rem;
  padding: 0 .45rem;
  color: var(--nf-color-text-muted);
  font: inherit;
  font-size: .85rem;
  font-weight: 700;
  cursor: pointer;
  background: transparent;
  border: 0;
  border-radius: var(--nf-radius-sm);
}
.document-editor-view__back:hover,
.document-editor-view__back:focus-visible {
  color: var(--nf-color-text);
  background: color-mix(in srgb, var(--nf-color-primary) 10%, transparent);
  outline: none;
}
.document-editor-view__header h1,
.document-editor-view__eyebrow { margin: 0; }
.document-editor-view__eyebrow {
  color: var(--nf-color-text-muted);
  font-size: .78rem;
  font-weight: 800;
  text-transform: uppercase;
}
.document-editor-view__actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: flex-end;
  gap: .5rem;
}
.document-editor-view__actions span { color: var(--nf-color-text-muted); font-size: .82rem; }
.document-editor-view__workspace {
  display: flex;
  height: calc(100dvh - 12rem);
  min-height: 30rem;
  flex-direction: column;
}
.document-editor-view__editor-shell {
  display: flex;
  flex: 1;
  min-height: 0;
  overflow: hidden;
  background: var(--nf-color-canvas);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-lg) var(--nf-radius-lg) 0 0;
  box-shadow: var(--nf-shadow-card);
}
.document-editor-view__custom-editor { flex: 1; min-height: 0; }
.document-editor-view__statusbar {
  position: relative;
  z-index: 2;
  display: flex;
  flex-shrink: 0;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  min-height: 2.8rem;
  padding: .35rem .65rem;
  color: var(--nf-color-text-muted);
  font-size: .82rem;
  background: var(--nf-color-surface);
  border: 1px solid var(--nf-color-border);
  border-top: 0;
  border-radius: 0 0 var(--nf-radius-lg) var(--nf-radius-lg);
}
.document-editor-view__unit-count strong,
.document-editor-view__today-goal strong { color: var(--nf-color-text); }
.document-editor-view__status-info {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  min-width: 0;
  gap: 1rem;
}
.document-editor-view__today-goal { display: inline-flex; align-items: center; gap: .45rem; }
.document-editor-view__today-goal--complete { color: var(--nf-color-success); font-weight: 700; }
.document-editor-view__today-goal-progress {
  display: block;
  width: 4.5rem;
  height: .36rem;
  overflow: hidden;
  background: color-mix(in srgb, var(--nf-color-primary) 18%, var(--nf-color-canvas));
  border-radius: var(--nf-radius-pill);
}
.document-editor-view__today-goal-progress-fill {
  display: block;
  height: 100%;
  background: var(--nf-color-primary);
  border-radius: inherit;
  transition: width .4s ease-out;
}
.document-editor-view__today-goal--complete .document-editor-view__today-goal-progress-fill {
  background: var(--nf-color-success);
}
@media (max-width: 44rem) {
  .document-editor-view { padding: var(--nf-space-3); }
  .document-editor-view__header { align-items: flex-start; flex-direction: column; }
  .document-editor-view__actions { justify-content: flex-start; }
  .document-editor-view__workspace { height: calc(100dvh - 16rem); min-height: 24rem; }
  .document-editor-view__statusbar { align-items: flex-start; flex-direction: column; }
  .document-editor-view__custom-editor { min-width: 0; }
}
</style>
