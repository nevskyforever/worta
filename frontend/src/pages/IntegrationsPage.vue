<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import { IonContent, IonIcon, IonPage, IonSpinner } from '@ionic/vue'
import {
  alertCircleOutline,
  checkmarkCircleOutline,
  cloudOfflineOutline,
  documentAttachOutline,
  folderOpenOutline,
  refreshOutline,
  unlinkOutline,
} from 'ionicons/icons'

import { apiErrorMessage } from '@/api/client'
import { integrationsApi } from '@/api/integrations'
import { projectsApi } from '@/api/projects'
import { settingsApi } from '@/api/settings'
import ScrivenerItemOptions from '@/components/integrations/ScrivenerItemOptions.vue'
import WordUploadCard from '@/components/integrations/WordUploadCard.vue'
import StatePanel from '@/components/ui/StatePanel.vue'
import {
  pickDesktopScrivenerProject,
  pickDesktopWordFile,
} from '@/platform/files'
import { currentPlatform } from '@/platform/runtime'
import { announceDataChange } from '@/services/dataChanges'
import { syncBatchNotification } from '@/utils/syncNotifications'
import { useLocaleStore } from '@/stores/locale'
import { useNotificationsStore } from '@/stores/notifications'
import { progressGameMessages } from '@/utils/gameNotifications'
import { progressChangeNotification } from '@/utils/progressNotifications'
import type { ProgressResult, Project } from '@/types/api'
import type { PlatformCapabilities } from '@/types/content'
import type {
  ScrivenerItem,
  SyncBatchItem,
  SyncBatchResult,
  SyncSummary,
  SyncType,
  WordImportResult,
} from '@/types/integrations'

const route = useRoute()
const locale = useLocaleStore()
const notifications = useNotificationsStore()
const t = locale.translate
const projects = ref<Project[]>([])
const capabilities = ref<PlatformCapabilities | null>(null)
const selectedProjectId = ref('')
const selectedStageId = ref('')
const sync = ref<SyncSummary | null>(null)
const syncType = ref<SyncType>('word')
const sourcePath = ref('')
const scrivenerItems = ref<ScrivenerItem[]>([])
const selectedScrivenerItem = ref('')
const loading = ref(true)
const syncLoading = ref(false)
const mutating = ref(false)
const batchMutating = ref(false)
const inspecting = ref(false)
const error = ref<string | null>(null)
const operationError = ref<string | null>(null)
const success = ref('')
const batchResult = ref<SyncBatchResult | null>(null)
let syncRequestSequence = 0

const selectedProject = computed(
  () => projects.value.find(({ id }) => id === selectedProjectId.value) ?? null,
)
const selectedEntity = computed(() => {
  if (!selectedProject.value) return null
  if (!selectedStageId.value) return selectedProject.value
  return selectedProject.value.stages.find(({ id }) => id === selectedStageId.value) ?? null
})
const localSyncAvailable = computed(() => capabilities.value?.local_file_sync === true)
const tauriPickerAvailable = computed(() => currentPlatform() === 'tauri')
const selectedProjectHasStages = computed(() => (selectedProject.value?.stages.length ?? 0) > 0)
const syncTargetReady = computed(
  () => Boolean(selectedProject.value) && (!selectedProjectHasStages.value || Boolean(selectedStageId.value)),
)
const entityWritable = computed(
  () => syncTargetReady.value && selectedEntity.value?.status === 'активен',
)
const syncMethodActive = computed(() => selectedEntity.value?.work_method === 'sync')
const busy = computed(() => mutating.value || batchMutating.value)
const flattenedScrivenerItems = computed(() => flattenScrivenerItems(scrivenerItems.value))
const selectedScrivenerTitle = computed(
  () =>
    flattenedScrivenerItems.value.find(({ id }) => id === selectedScrivenerItem.value)?.title ?? '',
)

function applyGameFeedback(progress: ProgressResult | null): void {
  const game = progress?.game
  if (!game) return
  notifications.setGameHistory(game.state.notifications)
  for (const message of progressGameMessages(game, t, locale.formatNumber)) notifications.success(message)
}

function applyProgressFeedback(
  progress: ProgressResult | null,
  stageId: string | null = selectedStageId.value || null,
): void {
  if (!progress) return
  const entity = stageId
    ? progress.project.stages.find((stage) => stage.id === stageId) ?? selectedEntity.value ?? progress.project
    : progress.project
  const feedback = progressChangeNotification(
    progress,
    entity,
    t,
    locale.formatNumber,
    locale.formatUnit,
  )
  if (feedback) notifications.show(feedback.message, feedback.kind)
}
const canConfigure = computed(
  () =>
      localSyncAvailable.value &&
      entityWritable.value &&
      selectedEntity.value?.work_method !== 'app' &&
    sourcePath.value.trim().length > 0 &&
    (syncType.value === 'word' || selectedScrivenerItem.value.length > 0),
)

function routeProjectId(): string {
  const value = route.query.projectId
  return typeof value === 'string' ? value : ''
}

function routeStageId(): string {
  const value = route.query.stageId
  return typeof value === 'string' ? value : ''
}

function flattenScrivenerItems(items: ScrivenerItem[]): ScrivenerItem[] {
  return items.flatMap((item) => [item, ...flattenScrivenerItems(item.children)])
}

function applySyncSummary(summary: SyncSummary): void {
  sync.value = summary
  if (!summary.configured) return
  syncType.value = summary.type ?? 'word'
  sourcePath.value = summary.path ?? ''
  selectedScrivenerItem.value = summary.item_id ?? ''
}

function resetEditor(): void {
  sync.value = null
  syncType.value = 'word'
  sourcePath.value = ''
  scrivenerItems.value = []
  selectedScrivenerItem.value = ''
  operationError.value = null
  success.value = ''
}

async function loadPage(): Promise<void> {
  loading.value = true
  error.value = null
  try {
    const [settings, projectList] = await Promise.all([
      settingsApi.get(),
      projectsApi.list({ status: 'активен', sort: 'name' }),
    ])
    capabilities.value = settings.capabilities
    projects.value = projectList
    const requestedProject = routeProjectId()
    const nextProjectId = projectList.some(({ id }) => id === requestedProject)
      ? requestedProject
      : (projectList[0]?.id ?? '')
    const requestedStage = routeStageId()
    const nextProject = projectList.find(({ id }) => id === nextProjectId)
    let nextStageId = nextProject?.stages.some(({ id }) => id === requestedStage)
      ? requestedStage
      : ''
    if (!nextStageId && nextProject?.stages.length) {
      try {
        const bindings = await integrationsApi.getProjectSyncs(nextProjectId)
        nextStageId = bindings.syncs.find(
          (summary) => summary.configured && summary.stage_id,
        )?.stage_id ?? ''
      } catch {
        // Source setup remains available if optional binding discovery fails.
        nextStageId = ''
      }
    }
    if (selectedProjectId.value === nextProjectId) {
      selectedStageId.value = nextStageId
      await loadSync()
    } else {
      selectedStageId.value = nextStageId
      selectedProjectId.value = nextProjectId
    }
  } catch (loadError) {
    error.value = t(apiErrorMessage(loadError))
  } finally {
    loading.value = false
  }
}

async function loadSync(): Promise<void> {
  const sequence = ++syncRequestSequence
  resetEditor()
  if (!selectedProjectId.value || !syncTargetReady.value) {
    syncLoading.value = false
    return
  }
  syncLoading.value = true
  try {
    const result = await integrationsApi.getSync(
      selectedProjectId.value,
      selectedStageId.value || null,
    )
    if (sequence !== syncRequestSequence) return
    applySyncSummary(result)
    if (
      localSyncAvailable.value &&
      syncType.value === 'scrivener' &&
      sourcePath.value &&
      selectedScrivenerItem.value
    ) {
      await inspectScrivener(false)
    }
  } catch (loadError) {
    if (sequence === syncRequestSequence) operationError.value = t(apiErrorMessage(loadError))
  } finally {
    if (sequence === syncRequestSequence) syncLoading.value = false
  }
}

async function inspectScrivener(showSuccess = true): Promise<void> {
  if (!sourcePath.value.trim() || !localSyncAvailable.value) return
  inspecting.value = true
  operationError.value = null
  success.value = ''
  try {
    scrivenerItems.value = await integrationsApi.inspectScrivener(sourcePath.value.trim())
    if (
      selectedScrivenerItem.value &&
      !flattenScrivenerItems(scrivenerItems.value).some(
        ({ id }) => id === selectedScrivenerItem.value,
      )
    ) {
      selectedScrivenerItem.value = ''
    }
    if (showSuccess) {
      success.value = t('Найдено документов Scrivener: {count}', {
        count: flattenScrivenerItems(scrivenerItems.value).length,
      })
    }
  } catch (inspectError) {
    scrivenerItems.value = []
    operationError.value = t(apiErrorMessage(inspectError))
  } finally {
    inspecting.value = false
  }
}

async function chooseSource(): Promise<void> {
  operationError.value = null
  success.value = ''
  try {
    const selected =
      syncType.value === 'word'
        ? await pickDesktopWordFile(t('Выберите документ Word'), t('Документ Word'))
        : await pickDesktopScrivenerProject(t('Выберите проект Scrivener'))
    if (!selected) return
    sourcePath.value = selected
    selectedScrivenerItem.value = ''
    scrivenerItems.value = []
    if (syncType.value === 'scrivener') await inspectScrivener(false)
  } catch (pickerError) {
    operationError.value = t(
      pickerError instanceof Error ? pickerError.message : String(pickerError),
    )
  }
}

function changeSyncType(): void {
  sourcePath.value = ''
  selectedScrivenerItem.value = ''
  scrivenerItems.value = []
  operationError.value = null
  success.value = ''
}

async function configureSync(): Promise<void> {
  if (!canConfigure.value || !selectedProjectId.value || busy.value) return
  if (selectedEntity.value?.work_method === 'manual') {
    if (!window.confirm(t('Синхронизация будет получать записи только из внешнего файла. Ручной ввод и текстовый редактор станут недоступны до следующей смены метода. Переключить метод и продолжить?'))) return
    const updated = selectedStageId.value
      ? await projectsApi.updateStage(selectedProjectId.value, selectedStageId.value, { work_method: 'sync' })
      : await projectsApi.update(selectedProjectId.value, { work_method: 'sync' })
    projects.value = projects.value.map((project) => project.id === updated.id ? updated : project)
  }
  mutating.value = true
  operationError.value = null
  success.value = ''
  try {
    applySyncSummary(
      await integrationsApi.configureSync(selectedProjectId.value, {
        type: syncType.value,
        path: sourcePath.value.trim(),
        stage_id: selectedStageId.value || null,
        item_id: syncType.value === 'scrivener' ? selectedScrivenerItem.value : null,
      }),
    )
    success.value = t('Источник синхронизации подключён.')
  } catch (configureError) {
    operationError.value = t(apiErrorMessage(configureError))
  } finally {
    mutating.value = false
  }
}

async function runSync(confirm = false): Promise<void> {
  if (
    !sync.value?.configured ||
    !selectedProjectId.value ||
    !entityWritable.value ||
    !syncMethodActive.value ||
    busy.value
  ) return
  mutating.value = true
  operationError.value = null
  success.value = ''
  try {
    const result = await (confirm ? integrationsApi.confirmSync : integrationsApi.runSync)(
      selectedProjectId.value,
      selectedStageId.value || null,
    )
    applySyncSummary(result.sync)
    applyProgressFeedback(result.progress)
    applyGameFeedback(result.progress)
    if (result.changed) announceDataChange('projects')
    success.value = result.sync.proposal_pending ? t('Внешний источник отличается. Подтвердите новый объём: {count}.', {count:locale.formatNumber(result.symbols,0)}) : result.changed
      ? t('Синхронизация завершена. Прочитано: {count} {unit}', {
          count: locale.formatNumber(result.symbols, 0),
          unit: locale.formatUnit('symbols', result.symbols),
        })
      : t('Документ не изменился. Текущий объём уже актуален.')
    if (!result.changed) notifications.show(success.value, 'info')
  } catch (runError) {
    const code=apiErrorMessage(runError)
    operationError.value = t(code==='sync_source_missing' ? 'Локальный файл не найден. Переподключите его.' : code.startsWith('external_') || code==='sync_source_stale' ? 'Локальный источник требует повторной проверки' : code)
  } finally {
    mutating.value = false
  }
}

async function disconnectSync(): Promise<void> {
  if (!sync.value?.configured || !selectedProjectId.value || busy.value) return
  const confirmed = window.confirm(
    t('Отключить источник? Файл и уже записанный прогресс останутся без изменений.'),
  )
  if (!confirmed) return
  mutating.value = true
  operationError.value = null
  success.value = ''
  try {
    applySyncSummary(
      await integrationsApi.removeSync(
        selectedProjectId.value,
        selectedStageId.value || null,
      ),
    )
    sourcePath.value = ''
    selectedScrivenerItem.value = ''
    scrivenerItems.value = []
    success.value = t('Источник синхронизации отключён.')
  } catch (removeError) {
    operationError.value = t(apiErrorMessage(removeError))
  } finally {
    mutating.value = false
  }
}

async function runAllSync(): Promise<void> {
  if (!localSyncAvailable.value || busy.value) return
  batchMutating.value = true
  batchResult.value = null
  operationError.value = null
  success.value = ''
  try {
    const result = await integrationsApi.runAllSync()
    const currentProjectId = selectedProjectId.value
    projects.value = await projectsApi.list({ status: 'активен', sort: 'name' })
    if (!projects.value.some(({ id }) => id === currentProjectId)) {
      selectedProjectId.value = projects.value[0]?.id ?? ''
    }
    await loadSync()
    const summary = syncBatchNotification(result, t, locale.formatNumber, locale.formatUnit)
    notifications.show(summary.message, summary.kind)
    const latestGame = [...result.items]
      .reverse()
      .find((item) => item.progress?.game)?.progress?.game
    if (latestGame) notifications.setGameHistory(latestGame.state.notifications)
    if (result.items.some((item) => item.changed)) announceDataChange('projects')
    if (latestGame) announceDataChange('game')
    batchResult.value = result
  } catch (batchError) {
    operationError.value = t(apiErrorMessage(batchError))
  } finally {
    batchMutating.value = false
  }
}

function batchItemLabel(item: SyncBatchItem): string {
  const project = projects.value.find(({ id }) => id === item.project_id)
  if (!project) return item.stage_id ?? item.project_id
  if (!item.stage_id) return project.name
  const stage = project.stages.find(({ id }) => id === item.stage_id)
  return `${project.name} · ${stage?.name ?? item.stage_id}`
}

function formatDateTime(value: string | null): string {
  if (!value) return t('Ещё не синхронизировалось')
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return value
  return new Intl.DateTimeFormat(locale.localeTag, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date)
}

function updateImportedProject(result: WordImportResult, stageId: string | null): void {
  const index = projects.value.findIndex(({ id }) => id === result.project.id)
  if (index >= 0) projects.value.splice(index, 1, result.project)
  applyProgressFeedback(result.progress, stageId)
  applyGameFeedback(result.progress)
  if (!result.changed) {
    notifications.show(t('Объём проекта уже совпадает с документом.'), 'info')
  }
  announceDataChange('projects')
}

watch(selectedProjectId, () => {
  const requestedStage = routeStageId()
  const stages = selectedProject.value?.stages ?? []
  if (stages.some(({ id }) => id === requestedStage)) {
    selectedStageId.value = requestedStage
  } else if (!stages.some(({ id }) => id === selectedStageId.value)) {
    selectedStageId.value = ''
  }
  void loadSync()
})
watch(selectedStageId, () => void loadSync())
onMounted(loadPage)
</script>

<template>
  <IonPage>
    <IonContent :fullscreen="true" class="integrations-content">
      <main class="integrations-page">
        <header class="integrations-header">
          <div>
            <p>{{ t('Документы') }}</p>
            <h1>{{ t('Word и Scrivener') }}</h1>
            <span>{{ t('Приложение вычисляет прогресс и не читает рукопись без явного действия.') }}</span>
          </div>
          <IonIcon :icon="documentAttachOutline" aria-hidden="true" />
        </header>

        <StatePanel
          v-if="loading"
          :title="t('Проверяем возможности платформы')"
          :message="t('Загружаем проекты и доступные способы работы с файлами.')"
          loading
        />
        <StatePanel
          v-else-if="error"
          :title="t('Не удалось открыть интеграции')"
          :message="error"
          :icon="alertCircleOutline"
        >
          <button class="nf-button" type="button" @click="loadPage">{{ t('Повторить') }}</button>
        </StatePanel>

        <div v-else class="integration-stack">
          <WordUploadCard :projects="projects" @imported="updateImportedProject" />

          <section
            v-if="!localSyncAvailable"
            class="platform-limitation"
            aria-labelledby="local-sync-unavailable-title"
          >
            <IonIcon :icon="cloudOfflineOutline" aria-hidden="true" />
            <div>
              <h2 id="local-sync-unavailable-title">{{ t('Постоянная синхронизация доступна только на desktop') }}</h2>
              <p>
                {{
                  t(
                    'Web, iOS и Android не получают произвольный фоновый доступ к Word или Scrivener. Используйте явный выбор .docx выше; загрузка проекта Scrivener сервером не поддерживается.',
                  )
                }}
              </p>
              <p>
                {{
                  t(
                    'Для автоматического отслеживания подключите локальный путь в настольной версии. В браузере и на мобильных устройствах используйте явный выбор файла.',
                  )
                }}
              </p>
            </div>
          </section>

          <section v-else class="local-sync-card" aria-labelledby="local-sync-title">
            <div class="local-sync-card__heading">
              <div>
                <p>{{ t('Синхронизация файлов') }}</p>
                <h2 id="local-sync-title">{{ t('Синхронизация проекта') }}</h2>
              </div>
              <div class="local-sync-card__heading-actions">
                <button
                  class="nf-button nf-button--secondary"
                  type="button"
                  :disabled="busy"
                  @click="runAllSync"
                >
                  <IonSpinner v-if="batchMutating" name="crescent" aria-hidden="true" />
                  <IonIcon v-else :icon="refreshOutline" aria-hidden="true" />
                  {{ t('Синхронизировать все') }}
                </button>
                <IonIcon :icon="folderOpenOutline" aria-hidden="true" />
              </div>
            </div>

            <div v-if="batchResult" class="batch-result" role="status">
              <strong>
                {{
                  t('Проверено: {checked}. Обновлено: {changed}. Ошибок: {failed}.', {
                    checked: batchResult.checked,
                    changed: batchResult.changed,
                    failed: batchResult.failed,
                  })
                }}
              </strong>
              <ul v-if="batchResult.failed">
                <li v-for="item in batchResult.items.filter(({ ok }) => !ok)" :key="`${item.project_id}:${item.stage_id}`">
                  {{ batchItemLabel(item) }} — {{ t(item.error?.message ?? 'Не удалось прочитать источник синхронизации.') }}
                </li>
              </ul>
            </div>

            <StatePanel
              v-if="!projects.length"
              :title="t('Нет активных проектов')"
              :message="t('Создайте или активируйте проект, чтобы подключить документ.')"
              :icon="folderOpenOutline"
            />

            <div v-else class="sync-form">
              <div class="sync-target-grid">
                <label for="sync-project">
                  <span>{{ t('Проект') }}</span>
                  <select id="sync-project" v-model="selectedProjectId" :disabled="busy">
                    <option v-for="project in projects" :key="project.id" :value="project.id">
                      {{ project.name }}
                    </option>
                  </select>
                </label>
                <label for="sync-stage">
                  <span>{{ t('Проект или источник') }}</span>
                  <select id="sync-stage" v-model="selectedStageId" :disabled="busy">
                    <option value="">
                      {{ selectedProjectHasStages ? t('Выберите источник') : t('Весь проект') }}
                    </option>
                    <option
                      v-for="stage in selectedProject?.stages ?? []"
                      :key="stage.id"
                      :value="stage.id"
                    >
                      {{ stage.name }}
                    </option>
                  </select>
                </label>
              </div>

              <div v-if="syncLoading" class="sync-loading" aria-live="polite">
                <IonSpinner name="crescent" aria-hidden="true" />
                {{ t('Проверяем подключение…') }}
              </div>

              <template v-else>
                <div v-if="sync?.configured" class="sync-status">
                  <IonIcon :icon="checkmarkCircleOutline" aria-hidden="true" />
                  <div>
                    <strong>
                      {{ sync.type === 'scrivener' ? 'Scrivener' : 'Word' }} ·
                      {{ selectedEntity?.name }}
                    </strong>
                    <span v-if="sync.path">{{ sync.path }}</span>
                    <small v-if="sync.type === 'scrivener' && selectedScrivenerTitle">
                      {{ t('Документ Scrivener') }}: {{ selectedScrivenerTitle }}
                    </small>
                    <small>{{ t('Последняя синхронизация') }}: {{ formatDateTime(sync.last_synced_at) }}</small>
                  </div>
                </div>

                <div v-if="selectedProjectHasStages && !selectedStageId" class="sync-warning" role="status">
                  {{ t('Выберите источник. Источник подключается отдельно к каждому источнику проекта.') }}
                </div>
                <div v-else-if="!entityWritable" class="sync-warning" role="status">
                  {{ t('Архивные и завершённые сущности доступны только для просмотра.') }}
                </div>

                <fieldset :disabled="busy || !entityWritable">
                  <legend>{{ sync?.configured ? t('Изменить источник') : t('Подключить источник') }}</legend>
                  <div class="source-type" role="radiogroup" :aria-label="t('Тип источника')">
                    <label>
                      <input v-model="syncType" type="radio" value="word" @change="changeSyncType" />
                      <span>Word (.docx)</span>
                    </label>
                    <label>
                      <input v-model="syncType" type="radio" value="scrivener" @change="changeSyncType" />
                      <span>Scrivener</span>
                    </label>
                  </div>

                  <label class="path-field" for="sync-path">
                    <span>{{ t('Локальный путь') }}</span>
                    <div>
                      <input
                        id="sync-path"
                        v-model="sourcePath"
                        type="text"
                        autocomplete="off"
                        spellcheck="false"
                        :placeholder="
                          syncType === 'word'
                            ? t('Путь к документу .docx')
                            : t('Путь к проекту Scrivener')
                        "
                      />
                      <button
                        v-if="tauriPickerAvailable"
                        class="nf-button nf-button--secondary"
                        type="button"
                        @click="chooseSource"
                      >
                        {{ t('Выбрать…') }}
                      </button>
                    </div>
                    <small v-if="!tauriPickerAvailable">
                      {{ t('В режиме разработки путь вводится вручную; готовая desktop-сборка открывает системный диалог.') }}
                    </small>
                  </label>

                  <div v-if="syncType === 'scrivener'" class="scrivener-picker">
                    <button
                      class="nf-button nf-button--secondary"
                      type="button"
                      :disabled="busy || !sourcePath.trim() || inspecting"
                      @click="inspectScrivener()"
                    >
                      <IonSpinner v-if="inspecting" name="crescent" aria-hidden="true" />
                      {{ inspecting ? t('Читаем проект…') : t('Показать документы Scrivener') }}
                    </button>
                    <label v-if="scrivenerItems.length" for="scrivener-item">
                      <span>{{ t('Документ Scrivener') }}</span>
                      <select id="scrivener-item" v-model="selectedScrivenerItem">
                        <option value="" disabled>{{ t('Выберите документ') }}</option>
                        <ScrivenerItemOptions :items="scrivenerItems" />
                      </select>
                    </label>
                  </div>

                  <button class="nf-button" type="button" :disabled="busy || !canConfigure" @click="configureSync">
                    {{ sync?.configured ? t('Сохранить источник') : t('Подключить источник') }}
                  </button>
                </fieldset>

                <div v-if="sync?.configured && syncMethodActive" class="sync-actions">
                  <button
                    class="nf-button"
                    type="button"
                    :disabled="busy || !entityWritable"
                    @click="runSync(false)"
                  >
                    <IonSpinner v-if="mutating" name="crescent" aria-hidden="true" />
                    <IonIcon v-else :icon="refreshOutline" aria-hidden="true" />
                    {{ t('Синхронизировать сейчас') }}
                  </button>
                  <button v-if="sync?.proposal_pending" class="nf-button" type="button" :disabled="busy" @click="runSync(true)">{{ t('Применить сравнение внешнего источника') }}</button>
                  <p>{{ t('Путь и выбранный источник остаются только на этом устройстве. Изменение прогресса требует подтверждения.') }}</p>
                  <button
                    class="nf-button nf-button--quiet"
                    type="button"
                    :disabled="busy"
                    @click="disconnectSync"
                  >
                    <IonIcon :icon="unlinkOutline" aria-hidden="true" />
                    {{ t('Отключить') }}
                  </button>
                </div>
              </template>

              <p v-if="operationError" class="operation-message operation-message--error" role="alert">
                <IonIcon :icon="alertCircleOutline" aria-hidden="true" />
                {{ operationError }}
              </p>
              <p v-if="success" class="operation-message" role="status">
                <IonIcon :icon="checkmarkCircleOutline" aria-hidden="true" />
                {{ success }}
              </p>
            </div>
          </section>
        </div>
      </main>
    </IonContent>
  </IonPage>
</template>

<style scoped>
.integrations-content {
  --background: var(--nf-color-canvas);
}

.integrations-page {
  width: min(100%, 82rem);
  min-height: 100%;
  margin: 0 auto;
  padding: calc(var(--nf-space-7) + env(safe-area-inset-top)) clamp(1rem, 3vw, 3.5rem)
    calc(var(--nf-space-7) + env(safe-area-inset-bottom));
}

.integrations-header {
  display: flex;
  gap: var(--nf-space-5);
  align-items: center;
  justify-content: space-between;
  margin-bottom: var(--nf-space-7);
}

.integrations-header p,
.local-sync-card__heading p {
  margin: 0 0 var(--nf-space-2);
  color: var(--nf-color-accent);
  font-size: 0.75rem;
  font-weight: 800;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}

.integrations-header h1 {
  margin: 0;
  font-family: var(--nf-font-serif);
  font-size: clamp(1.8rem, 3.5vw, 2.75rem);
  letter-spacing: -0.04em;
}

.integrations-header span {
  display: block;
  max-width: 44rem;
  margin-top: var(--nf-space-3);
  color: var(--nf-color-text-muted);
  line-height: 1.55;
}

.integrations-header > ion-icon {
  padding: var(--nf-space-4);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
  font-size: 2rem;
}

.integration-stack {
  display: grid;
  gap: var(--nf-space-6);
}

.platform-limitation,
.local-sync-card {
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-surface);
}

.platform-limitation {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: var(--nf-space-4);
  padding: var(--nf-space-5);
}

.platform-limitation > ion-icon {
  color: var(--nf-color-warning);
  font-size: 1.7rem;
}

.platform-limitation h2,
.platform-limitation p {
  margin: 0;
}

.platform-limitation h2 {
  font-family: var(--nf-font-serif);
  font-size: 1.3rem;
}

.platform-limitation p {
  margin-top: var(--nf-space-2);
  color: var(--nf-color-text-muted);
  line-height: 1.55;
}

.local-sync-card {
  padding: clamp(1.25rem, 3vw, 2rem);
  box-shadow: var(--nf-shadow-card);
}

.local-sync-card__heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-bottom: var(--nf-space-5);
  border-bottom: 1px solid var(--nf-color-border);
}

.local-sync-card__heading h2 {
  margin: 0;
  font-family: var(--nf-font-serif);
  font-size: 1.55rem;
}

.local-sync-card__heading-actions {
  display: flex;
  gap: var(--nf-space-3);
  align-items: center;
}

.local-sync-card__heading-actions > ion-icon {
  color: var(--nf-color-primary);
  font-size: 1.7rem;
}

.batch-result {
  padding: var(--nf-space-4);
  margin-top: var(--nf-space-5);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-muted);
  color: var(--nf-color-text);
}

.batch-result ul {
  display: grid;
  gap: var(--nf-space-2);
  padding-left: var(--nf-space-5);
  margin: var(--nf-space-3) 0 0;
  color: var(--nf-color-danger);
  font-size: 0.88rem;
}

.sync-form {
  display: grid;
  gap: var(--nf-space-5);
  padding-top: var(--nf-space-5);
}

.sync-target-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--nf-space-4);
}

.sync-target-grid label,
.path-field,
.scrivener-picker label {
  display: grid;
  gap: var(--nf-space-2);
  color: var(--nf-color-text-muted);
  font-size: 0.82rem;
  font-weight: 700;
}

.sync-target-grid select,
.path-field input,
.scrivener-picker select {
  width: 100%;
  min-height: 2.8rem;
  padding: 0 var(--nf-space-3);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
  color: var(--nf-color-text);
}

.sync-loading,
.sync-status,
.sync-warning,
.operation-message {
  display: flex;
  gap: var(--nf-space-3);
  align-items: center;
  padding: var(--nf-space-3) var(--nf-space-4);
  border-radius: var(--nf-radius-sm);
}

.sync-loading {
  color: var(--nf-color-text-muted);
}

.sync-status {
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-success);
}

.sync-status > ion-icon {
  flex: 0 0 auto;
  font-size: 1.4rem;
}

.sync-status strong,
.sync-status span,
.sync-status small {
  display: block;
}

.sync-status span,
.sync-status small {
  margin-top: var(--nf-space-1);
  color: var(--nf-color-text-muted);
  overflow-wrap: anywhere;
}

.sync-warning {
  background: color-mix(in srgb, var(--nf-color-warning), transparent 88%);
  color: var(--nf-color-warning);
}

fieldset {
  display: grid;
  gap: var(--nf-space-4);
  padding: var(--nf-space-5);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-md);
}

legend {
  padding: 0 var(--nf-space-2);
  color: var(--nf-color-text);
  font-family: var(--nf-font-serif);
  font-size: 1.15rem;
  font-weight: 700;
}

.source-type {
  display: flex;
  gap: var(--nf-space-3);
  flex-wrap: wrap;
}

.source-type label {
  display: flex;
  gap: var(--nf-space-2);
  align-items: center;
  min-height: 2.75rem;
  padding: 0 var(--nf-space-4);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-pill);
  cursor: pointer;
}

.source-type label:has(input:checked) {
  border-color: var(--nf-color-primary);
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
}

.source-type input {
  width: 1.15rem;
  height: 1.15rem;
}

.path-field > div {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: var(--nf-space-3);
}

.path-field small {
  font-weight: 500;
  line-height: 1.45;
}

.scrivener-picker {
  display: flex;
  gap: var(--nf-space-4);
  align-items: end;
  flex-wrap: wrap;
}

.scrivener-picker label {
  flex: 1 1 20rem;
}

.sync-actions {
  display: flex;
  gap: var(--nf-space-3);
  align-items: center;
  flex-wrap: wrap;
}

.operation-message {
  margin: 0;
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-success);
  font-weight: 700;
}

.operation-message--error {
  background: color-mix(in srgb, var(--nf-color-danger), transparent 88%);
  color: var(--nf-color-danger);
}

@media (max-width: 42rem) {
  .integrations-header > ion-icon {
    display: none;
  }

  .sync-target-grid {
    grid-template-columns: 1fr;
  }

  .path-field > div {
    grid-template-columns: 1fr;
  }

  .local-sync-card__heading {
    align-items: flex-start;
    flex-direction: column;
    gap: var(--nf-space-4);
  }

  .local-sync-card__heading-actions {
    width: 100%;
  }

  .local-sync-card__heading-actions .nf-button {
    flex: 1;
  }
}

@media (max-width: 35rem) {
  .platform-limitation {
    grid-template-columns: 1fr;
  }
}
</style>
