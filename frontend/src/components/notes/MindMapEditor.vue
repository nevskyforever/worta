<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { IonIcon, IonSpinner } from '@ionic/vue'
import { locateOutline, saveOutline } from 'ionicons/icons'

import { apiErrorMessage } from '@/api/client'
import {presentStatus} from '@/diagnostics/presentation'
import {
  mindMapBridge,
  safeCloudMapDisplay,
  parseMindMapData,
  parseMindMapEvents,
  type MindMapInitialization,
} from '@/components/notes/mindMapBridge'
import { useLocaleStore } from '@/stores/locale'
import type { JsonObject, MindMapResponse, NotesScope, XMindImportResponse } from '@/types/notes'

const props = defineProps<{
  secureReadOnly?: boolean
  map: MindMapResponse
  persist: (data: JsonObject, scope: NotesScope, expectedHeads?: JsonObject) => Promise<MindMapResponse>
  importXMind: (file: File, scope: NotesScope) => Promise<XMindImportResponse>
  focusNodeId?: string | null
}>()

const emit = defineEmits<{
  saved: [payload: MindMapResponse]
  escape: []
}>()

const locale = useLocaleStore()
const t = locale.translate
const frame = ref<HTMLIFrameElement | null>(null)
const ready = ref(false)
const saving = ref(false)
const statusMessage = ref(t('Загрузка карты…'))
const errorMessage = ref<string | null>(null)
const fileInput = ref<HTMLInputElement | null>(null)
const importSheets = ref<Array<{ title: string; data: JsonObject }>>([])
const selectedImportSheet = ref(0)
const importing = ref(false)
let pollTimer: number | null = null
let pendingData: JsonObject | null = null
let saveLoopRunning = false
let initialized = false
let renderedHeads = props.map.expected_heads ? JSON.parse(JSON.stringify(props.map.expected_heads)) as JsonObject : undefined
let lastSavedSerialized = props.map.data ? JSON.stringify(props.map.data) : ''
const persistenceScope: NotesScope = {
  projectId: props.map.project_id,
  stageId: props.map.stage_id,
}

const editorSource = computed(
  () => `${import.meta.env.BASE_URL}mindmap-assets/index.html`,
)

function initialPayload(): MindMapInitialization {
  return {
    data: props.map.data
      ? (props.secureReadOnly ? safeCloudMapDisplay(props.map.data) : JSON.parse(JSON.stringify(props.map.data)) as JsonObject)
      : null,
    editorLabel: t('Редактор карты'),
    emptyStageMapText: t('Карта не была создана при работе над источником.'),
    floatingNodeName: t('Свободный узел'),
    floatingNoteName: t('Новая заметка'),
    addFloatingNodeLabel: t('Добавить свободный узел'),
    addFloatingNoteLabel: t('Добавить плавающую заметку'),
    searchMapLabel: t('Поиск по карте'),
    searchPlaceholder: t('Поиск узлов и заметок'),
    nothingFoundText: t('Ничего не найдено'),
    detachBranchLabel: t('Отсоединить ветвь'),
    attachBranchLabel: t('Прикрепить к узлу карты'),
    attachTargetPrompt: t('Выберите родительский узел карты'),
    locale: locale.language,
    loadingText: t('Загрузка карты…'),
    newTopicName: t('Новая тема'),
    readOnly: props.map.read_only,
    rootTopic: props.map.name,
  }
}

function stopPolling(): void {
  if (pollTimer !== null) window.clearInterval(pollTimer)
  pollTimer = null
}

function iframeLoaded(): void {
  stopPolling()
  ready.value = false
  initialized = false
  errorMessage.value = null
  statusMessage.value = t('Загрузка карты…')
  const bridge = mindMapBridge(frame.value)
  if (!bridge) {
    errorMessage.value = t('Не удалось загрузить редактор карты.')
    return
  }
  try {
    bridge.initialize(initialPayload())
    initialized = true
    frame.value?.contentWindow?.addEventListener('keydown', handleFrameKeydown, true)
    pollTimer = window.setInterval(pollEvents, 150)
    pollEvents()
  } catch (reason) {
    errorMessage.value = reason instanceof Error ? reason.message : t('Не удалось загрузить редактор карты.')
  }
}

function handleFrameKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape') return
  event.preventDefault()
  emit('escape')
}

function pollEvents(): void {
  const bridge = mindMapBridge(frame.value)
  if (!bridge || !initialized) return
  let payload: string
  try {
    payload = bridge.takeEvents()
  } catch {
    errorMessage.value = t('Связь с редактором карты прервана.')
    stopPolling()
    return
  }
  for (const event of parseMindMapEvents(payload)) {
    if (event.type === 'ready') {
      ready.value = true
      statusMessage.value = props.map.read_only
        ? t('Карта доступна только для просмотра.')
        : t('Карта готова.')
      if (props.focusNodeId) bridge.focusNode(props.focusNodeId)
    } else if (event.type === 'changed') {
      statusMessage.value = t('Есть несохранённые изменения.')
    } else if (event.type === 'save') {
      const data = parseMindMapData(event.payload)
      if (data) queueSave(data)
      else errorMessage.value = t('Редактор вернул повреждённые данные карты.')
    } else if (event.type === 'error' || event.type === 'exportError') {
      errorMessage.value = event.details || t('Не удалось обработать карту.')
    } else if (event.type === 'status' && event.message) {
      statusMessage.value = event.message
    }
  }
}

function queueSave(data: JsonObject): void {
  const serialized = JSON.stringify(data)
  if (serialized === lastSavedSerialized && !pendingData) {
    statusMessage.value = t('Все изменения сохранены.')
    return
  }
  pendingData = data
  if (!saveLoopRunning) void flushSaves()
}

async function flushSaves(): Promise<void> {
  saveLoopRunning = true
  saving.value = true
  errorMessage.value = null
  try {
    while (pendingData) {
      const current = pendingData
      pendingData = null
      statusMessage.value = t('Сохраняем карту…')
      try {
        const result = await props.persist(current, persistenceScope, renderedHeads)
        lastSavedSerialized = JSON.stringify(result.data ?? current)
        renderedHeads = result.expected_heads ? JSON.parse(JSON.stringify(result.expected_heads)) as JsonObject : undefined
        emit('saved', result)
        statusMessage.value = pendingData
          ? t('Сохраняем карту…')
          : t('Все изменения сохранены.')
      } catch (reason) {
        const code=typeof reason==='string'?reason:reason instanceof Error?reason.message:''
        if(code.startsWith('map_')){const status=presentStatus('error',code);errorMessage.value=`${t(status.description)} ${t(status.action)}`}
        else errorMessage.value = apiErrorMessage(reason)
        statusMessage.value = t('Карта не сохранена.')
      }
    }
  } finally {
    saving.value = false
    saveLoopRunning = false
  }
}

function saveNow(): void {
  const bridge = mindMapBridge(frame.value)
  if (!bridge || props.map.read_only) return
  bridge.saveNow()
  pollEvents()
}

function hasUserNodes(): boolean {
  const root = props.map.data?.nodeData
  return isRecord(root) && Array.isArray(root.children) && root.children.length > 0
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function chooseImportFile(): void {
  fileInput.value?.click()
}

async function handleXMindImport(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file) return
  importing.value = true
  errorMessage.value = null
  try {
    const response = await props.importXMind(file, persistenceScope)
    if (!response.sheets.length) throw new Error(t('Карта XMind пуста.'))
    importSheets.value = response.sheets
    selectedImportSheet.value = 0
    if (response.sheets.length === 1) await applyImport()
  } catch (reason) {
    errorMessage.value = apiErrorMessage(reason)
  } finally {
    importing.value = false
  }
}

function cancelImport(): void {
  importSheets.value = []
}

async function applyImport(): Promise<void> {
  const selected = importSheets.value[selectedImportSheet.value]
  if (!selected || props.map.read_only) return
  if (hasUserNodes() && !window.confirm(t('Импорт XMind заменит текущую структуру карты. Продолжить?'))) return
  const bridge = mindMapBridge(frame.value)
  if (!bridge) return
  importSheets.value = []
  bridge.initialize({ ...initialPayload(), data: selected.data })
  lastSavedSerialized = ''
  queueSave(selected.data)
}

function centerMap(): void {
  mindMapBridge(frame.value)?.toCenter()
}

function focusNode(nodeId: string): boolean {
  return mindMapBridge(frame.value)?.focusNode(nodeId) ?? false
}

function updateNodeNote(nodeId: string, text: string): boolean {
  return mindMapBridge(frame.value)?.updateNodeNote(nodeId, text) ?? false
}

function removeNodeNote(nodeId: string): boolean {
  return mindMapBridge(frame.value)?.removeNodeNote(nodeId) ?? false
}

function flushCurrentMap(): void {
  const payload = mindMapBridge(frame.value)?.getDataString()
  const data = parseMindMapData(payload)
  if (data && !props.map.read_only) queueSave(data)
}

onBeforeUnmount(() => {
  flushCurrentMap()
  frame.value?.contentWindow?.removeEventListener('keydown', handleFrameKeydown, true)
  stopPolling()
})

watch(
  () => props.focusNodeId,
  (nodeId) => {
    if (nodeId && ready.value) focusNode(nodeId)
  },
)

defineExpose({ focusNode, updateNodeNote, removeNodeNote, saveNow })
</script>

<template>
  <section class="mindmap-editor" :aria-label="t('Карта проекта')">
    <header class="mindmap-editor__toolbar">
      <div class="mindmap-editor__status" aria-live="polite">
        <IonSpinner v-if="saving" name="dots" aria-hidden="true" />
        <span>{{ statusMessage }}</span>
      </div>
      <div class="mindmap-editor__actions">
        <input
          ref="fileInput"
          class="mindmap-editor__file-input"
          type="file"
          accept=".xmind,application/zip"
          @change="handleXMindImport"
        />
        <button
          class="nf-button nf-button--secondary"
          type="button"
          :disabled="!ready || saving || importing || map.read_only"
          @click="chooseImportFile"
        >
          {{ t('Импорт из XMind…') }}
        </button>
        <button class="nf-button nf-button--secondary" type="button" :disabled="!ready" @click="centerMap">
          <IonIcon :icon="locateOutline" aria-hidden="true" />
          {{ t('По центру') }}
        </button>
        <button
          class="nf-button"
          type="button"
          :disabled="!ready || saving || map.read_only"
          @click="saveNow"
        >
          <IonIcon :icon="saveOutline" aria-hidden="true" />
          {{ t('Сохранить карту') }}
        </button>
      </div>
    </header>

    <div v-if="importSheets.length" class="mindmap-editor__import-dialog" role="dialog" aria-modal="true">
      <div class="mindmap-editor__import-card">
        <h2>{{ t('Выберите лист XMind') }}</h2>
        <label>
          {{ t('Лист карты') }}
          <select v-model.number="selectedImportSheet">
            <option v-for="(sheet, index) in importSheets" :key="`${sheet.title}-${index}`" :value="index">
              {{ sheet.title }}
            </option>
          </select>
        </label>
        <div class="mindmap-editor__import-actions">
          <button class="nf-button nf-button--secondary" type="button" @click="cancelImport">{{ t('Отмена') }}</button>
          <button class="nf-button" type="button" @click="applyImport">{{ t('Импортировать') }}</button>
        </div>
      </div>
    </div>

    <p v-if="map.combined" class="mindmap-editor__notice">
      {{ t('Показана объединённая карта проекта и источников.') }}
    </p>
    <p v-if="map.has_empty_completed_stage_map" class="mindmap-editor__notice">
      {{ t('У завершённого источника нет сохранённой карты; его ветвь доступна только для просмотра.') }}
    </p>
    <p v-if="errorMessage" class="mindmap-editor__error" role="alert">{{ errorMessage }}</p>

    <iframe
      ref="frame"
      class="mindmap-editor__frame"
      :src="editorSource"
      :title="t('Редактор карты')"
      sandbox="allow-scripts allow-same-origin allow-downloads"
      referrerpolicy="no-referrer"
      @load="iframeLoaded"
    />
  </section>
</template>

<style scoped>
.mindmap-editor {
  display: grid;
  min-height: min(70dvh, 54rem);
  overflow: hidden;
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-lg);
  background: var(--nf-color-surface);
  box-shadow: var(--nf-shadow-card);
}

.mindmap-editor__toolbar {
  display: flex;
  gap: var(--nf-space-3);
  align-items: center;
  justify-content: space-between;
  padding: var(--nf-space-3) var(--nf-space-4);
  border-bottom: 1px solid var(--nf-color-border);
}

.mindmap-editor__status,
.mindmap-editor__actions {
  display: flex;
  gap: var(--nf-space-2);
  align-items: center;
}

.mindmap-editor__file-input { display: none; }
.mindmap-editor__import-dialog { position: fixed; inset: 0; z-index: 10; display: grid; place-items: center; padding: 1rem; background: rgb(0 0 0 / 35%); }
.mindmap-editor__import-card { display: grid; gap: 1rem; width: min(28rem, 100%); padding: 1.25rem; border-radius: var(--nf-radius-lg); background: var(--nf-color-surface); box-shadow: var(--nf-shadow-card); }
.mindmap-editor__import-card h2 { margin: 0; font-family: var(--nf-font-serif); }
.mindmap-editor__import-card label { display: grid; gap: .4rem; font-weight: 700; }
.mindmap-editor__import-card select { min-height: 2.5rem; padding: .4rem; }
.mindmap-editor__import-actions { display: flex; justify-content: flex-end; gap: var(--nf-space-2); }

.mindmap-editor__status {
  color: var(--nf-color-text-muted);
  font-size: 0.85rem;
}

.mindmap-editor__notice,
.mindmap-editor__error {
  margin: 0;
  padding: var(--nf-space-2) var(--nf-space-4);
  font-size: 0.82rem;
}

.mindmap-editor__notice {
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
}

.mindmap-editor__error {
  background: color-mix(in srgb, var(--nf-color-danger) 12%, transparent);
  color: var(--nf-color-danger);
}

.mindmap-editor__frame {
  width: 100%;
  min-height: 36rem;
  border: 0;
  background: #f6f6f6;
}

@media (max-width: 39.99rem) {
  .mindmap-editor {
    min-height: calc(100dvh - 13rem);
    border-radius: var(--nf-radius-md);
  }

  .mindmap-editor__toolbar {
    align-items: stretch;
    flex-direction: column;
  }

  .mindmap-editor__actions {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .mindmap-editor__frame {
    min-height: calc(100dvh - 20rem);
  }
}
</style>
