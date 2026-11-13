<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  IonContent,
  IonIcon,
  IonPage,
  IonSpinner,
  onIonViewWillEnter,
  onIonViewWillLeave,
} from '@ionic/vue'
import {
  alertCircleOutline,
  archiveOutline,
  arrowBackOutline,
  addOutline,
  calendarClearOutline,
  checkmarkCircleOutline,
  createOutline,
  documentTextOutline,
  layersOutline,
  refreshOutline,
  trashOutline,
} from 'ionicons/icons'

import ProgressWorkspace from '@/components/projects/ProgressWorkspace.vue'
import ProjectEditDialog from '@/components/projects/ProjectEditDialog.vue'
import ProgressShareMenu from '@/components/projects/ProgressShareMenu.vue'
import StageDialog from '@/components/projects/StageDialog.vue'
import StreakBadge from '@/components/projects/StreakBadge.vue'
import StageWorkspace from '@/components/projects/StageWorkspace.vue'
import type { StageSort } from '@/components/projects/StageWorkspace.vue'
import StatisticsWorkspace from '@/components/projects/StatisticsWorkspace.vue'
import StatePanel from '@/components/ui/StatePanel.vue'
import ProgressBar from '@/components/ui/ProgressBar.vue'
import AnimatedNumber from '@/components/ui/AnimatedNumber.vue'
import ProgressRing from '@/components/ui/ProgressRing.vue'
import { apiErrorMessage } from '@/api/client'
import { gameApi } from '@/api/game'
import { integrationsApi } from '@/api/integrations'
import { documentsApi } from '@/api/documents'
import { settingsApi } from '@/api/settings'
import { announceDataChange, onDataChange } from '@/services/dataChanges'
import { useProjectPresentation } from '@/composables/useProjectPresentation'
import {
  copyProgressImage,
  downloadProgressImage,
  progressShareTitle,
} from '@/platform/progressShare'
import { useLocaleStore } from '@/stores/locale'
import { useNotificationsStore } from '@/stores/notifications'
import { progressGameMessages } from '@/utils/gameNotifications'
import { progressChangeNotification } from '@/utils/progressNotifications'
import { useProjectsStore } from '@/stores/projects'
import type {
  EntityUpdate,
  ProgressCreate,
  ProgressResult,
  Project,
  ProjectUpdate,
  StageCreate,
} from '@/types/api'
import type { SyncSummary } from '@/types/integrations'
import type { ProjectDocument } from '@/types/documents'

const route = useRoute()
const router = useRouter()
const store = useProjectsStore()
const locale = useLocaleStore()
const notifications = useNotificationsStore()
const t = locale.translate
const projectId = computed(() => String(route.params.projectId ?? ''))
const detailAnimationVersion = ref(0)
let detailViewActive = false
let pendingProjectRefresh = false
let stopDataChanges: (() => void) | undefined
const routeStageId = computed(() => String(route.params.stageId ?? ''))
const project = computed<Project>(() => store.currentProject as Project)
const openedStage = computed<Project | null>(() =>
  routeStageId.value
    ? project.value?.stages.find((stage) => stage.id === routeStageId.value) ?? null
    : null,
)
const detailEntity = computed<Project>(() => openedStage.value ?? project.value)
const isStageDetail = computed(() => openedStage.value !== null)
const presentation = useProjectPresentation(detailEntity)
const isSharedProject = computed(() => project.value.name === 'Общий проект')
const streaksEnabled = ref(false)
const gameModeEnabled = ref(false)
const stageSort = ref<StageSort>('manual')
let stageSortSaveChain: Promise<void> = Promise.resolve()
const displayedStreakEntity = computed<Project | null>(() => {
  if (!streaksEnabled.value || !store.currentProject) return null
  if (isStageDetail.value && project.value.deadline === null) {
    return detailEntity.value.deadline !== null && detailEntity.value.streak_enabled
      ? detailEntity.value
      : null
  }
  return project.value.deadline !== null && project.value.streak_enabled
    ? project.value
    : null
})
const displayedStreakScope = computed<'project' | 'stage'>(() =>
  displayedStreakEntity.value === openedStage.value ? 'stage' : 'project',
)
const editDialogOpen = ref(false)
const stageDialogOpen = ref(false)
const editingStage = ref<Project | null>(null)
const savedSelection = store.detailSelection(projectId.value)
const selectedEntityId = ref(savedSelection?.entityId ?? '')
const statisticsEntityId = ref(savedSelection?.statisticsEntityId ?? '')
const actionSuccess = ref<string | null>(null)
const feedbackArea = ref<'global' | 'progress'>('global')
const sharingProgress = ref(false)
const syncSummaries = ref<SyncSummary[]>([])
const syncLoading = ref(false)
const syncRunning = ref(false)
const documents = ref<ProjectDocument[]>([])
let syncRequestSequence = 0

const selectedSyncStageId = computed(() => routeStageId.value || selectedEntityId.value || null)
const hasConfiguredSync = computed(() =>
  syncSummaries.value.some((summary) => summary.configured),
)
const selectedSyncConfigured = computed(() =>
  selectedSyncStageId.value
    ? syncSummaries.value.some((summary) =>
      summary.configured && summary.stage_id === selectedSyncStageId.value,
    )
    : hasConfiguredSync.value,
)
const selectedSyncEntity = computed<Project>(() => selectedSyncStageId.value
  ? project.value.stages.find((stage) => stage.id === selectedSyncStageId.value) ?? project.value
  : project.value)
const textSymbols = computed<Record<string, number>>(() => Object.fromEntries(
  documents.value
    .filter((document) => document.project_id === project.value.id && document.has_content)
    .map((document) => [document.stage_id ?? document.project_id, document.symbols]),
))
const canOpenText = computed(() => detailEntity.value.work_method !== 'sync' && (
  isStageDetail.value || project.value.stages.length === 0
))
const sharedSourceDefaultName = computed(() => {
  const hasLegacySource = isSharedProject.value
    && !project.value.stages.length
    && (
      project.value.project_notes.length > 0
      || syncSummaries.value.some((summary) => summary.configured && summary.stage_id === null)
    )
  const sourceNumber = project.value.stages.length + (hasLegacySource ? 2 : 1)
  return `Источник ${sourceNumber}`
})

const canCompleteProject = computed(() => {
  if (!store.currentProject || project.value.status === 'завершен') return false
  return detailEntity.value.status !== 'завершен'
    && !detailEntity.value.infinite
    && detailEntity.value.goal !== null
    && detailEntity.value.total >= detailEntity.value.goal
})

function numberForProject(value: number): string {
  return locale.formatNumber(value, project.value.unit === 'symbols' ? 0 : 2)
}

function chooseAvailableEntity(): void {
  if (!store.currentProject) return
  if (routeStageId.value) {
    const stageExists = project.value.stages.some((stage) => stage.id === routeStageId.value)
    selectedEntityId.value = stageExists ? routeStageId.value : ''
    statisticsEntityId.value = stageExists ? routeStageId.value : ''
    return
  }
  if (!project.value.stages.length) {
    selectedEntityId.value = ''
    statisticsEntityId.value = ''
    return
  }
  if (!project.value.stages.some((stage) => stage.id === selectedEntityId.value)) {
    selectedEntityId.value = project.value.stages.find((stage) => stage.status !== 'завершен')?.id
      ?? project.value.stages[0]?.id
      ?? ''
  }
  if (!project.value.stages.some((stage) => stage.id === statisticsEntityId.value)) {
    statisticsEntityId.value = ''
  }
}

async function loadProject(): Promise<void> {
  if (!projectId.value) return
  actionSuccess.value = null
  const selection = store.detailSelection(projectId.value)
  if (selection) {
    selectedEntityId.value = selection.entityId
    statisticsEntityId.value = selection.statisticsEntityId
  }
  if (!routeStageId.value) statisticsEntityId.value = ''
  await store.loadOne(projectId.value)
  chooseAvailableEntity()
  refreshStatistics()
  await Promise.all([loadSyncSummary(), loadStreakSummaries(), loadDocuments()])
  if (route.query?.edit === '1' && project.value.status !== 'завершен' && !isSharedProject.value) {
    openProjectEdit()
    const query = { ...(route.query ?? {}) }
    delete query.edit
    void router.replace({ name: route.name ?? undefined, params: route.params, query })
  }
}

async function loadDocuments(): Promise<void> {
  const entities = project.value.stages.length
    ? project.value.stages
    : [project.value]
  const scopes = entities
    .filter((entity) => entity.work_method === 'app')
    .map((entity) => ({
      projectId: project.value.id,
      ...(entity.id === project.value.id ? {} : { stageId: entity.id }),
    }))
  const results = await Promise.all(
    scopes.map(async (scope) => {
      try { return await documentsApi.get(scope) } catch { return null }
    }),
  )
  documents.value = results.filter((document): document is ProjectDocument => document !== null)
}

async function refreshChangedProject(): Promise<void> {
  const before = store.currentProject?.id === projectId.value
    ? JSON.stringify(store.currentProject)
    : ''
  await loadProject()
  const after = store.currentProject?.id === projectId.value
    ? JSON.stringify(store.currentProject)
    : ''
  if (before && before !== after) detailAnimationVersion.value += 1
}

function enabledSetting(value: unknown): boolean {
  return value === true || value === 1 || value === 'true' || value === 'True'
}

async function loadStreakSummaries(): Promise<void> {
  try {
    const settings = await settingsApi.get()
    streaksEnabled.value = enabledSetting(settings.values.global_streak)
    gameModeEnabled.value = enabledSetting(settings.values.game_mode)
    const savedStageSort = settings.values.frontend_stage_sort
    if (
      savedStageSort === 'manual'
      || savedStageSort === 'name'
      || savedStageSort === 'deadline'
      || savedStageSort === 'progress'
      || savedStageSort === 'updated'
    ) stageSort.value = savedStageSort
  } catch {
    streaksEnabled.value = false
    gameModeEnabled.value = false
  }
}

function saveStageSort(value: StageSort): void {
  stageSort.value = value
  stageSortSaveChain = stageSortSaveChain
    .then(() => settingsApi.update({ frontend_stage_sort: value }))
    .then(() => undefined)
    .catch(() => undefined)
}

async function loadSyncSummary(): Promise<void> {
  if (!store.currentProject) return
  const sequence = ++syncRequestSequence
  syncLoading.value = true
  try {
    const summaries = await integrationsApi.getProjectSyncs(project.value.id)
    if (sequence === syncRequestSequence) syncSummaries.value = summaries.syncs
  } catch {
    if (sequence === syncRequestSequence) syncSummaries.value = []
  } finally {
    if (sequence === syncRequestSequence) syncLoading.value = false
  }
}

async function synchronizeProject(): Promise<void> {
  feedbackArea.value = 'global'
  actionSuccess.value = null
  if (selectedSyncEntity.value.work_method === 'manual') {
    if (!window.confirm(t('Синхронизация будет получать записи только из внешнего файла. Ручной ввод и текстовый редактор станут недоступны до следующей смены метода. Переключить метод и продолжить?'))) return
    const updated = selectedSyncStageId.value
      ? await store.updateStage(project.value.id, selectedSyncStageId.value, { work_method: 'sync' })
      : await store.updateCurrent(project.value.id, { work_method: 'sync' })
    if (!updated) return
  }
  if (!selectedSyncConfigured.value) {
    await router.push({
      name: 'integrations',
      query: {
        projectId: project.value.id,
        ...(selectedSyncStageId.value ? { stageId: selectedSyncStageId.value } : {}),
      },
    })
    return
  }
  syncRunning.value = true
  try {
    const result = await integrationsApi.runProjectSyncs(project.value.id)
    const failed = result.items.find((item) => !item.ok)
    if (failed && result.failed === result.checked) {
      store.detailActionError = t(failed.error?.message ?? t('Синхронизация не настроена.'))
      return
    }
    announceSuccess(t('Синхронизация завершена'))
    if (failed?.error?.message) notifications.warning(t(failed.error.message))
    for (const item of result.items) {
      applyProgressFeedback(item.progress, item.stage_id)
      applyGameFeedback(item.progress?.game ?? null)
    }
    await store.refreshCurrent(project.value.id)
    chooseAvailableEntity()
    refreshStatistics()
    await Promise.all([loadSyncSummary(), loadStreakSummaries()])
  } catch (error) {
    store.detailActionError = t(apiErrorMessage(error))
  } finally {
    syncRunning.value = false
  }
}

async function openTextEditor(): Promise<void> {
  if (detailEntity.value.work_method === 'manual') {
    if (!window.confirm(t('Встроенный редактор будет добавлять записи только из текста документа. Ручной ввод и синхронизация станут недоступны до следующей смены метода. Переключить метод и открыть текст?'))) return
    const updated = isStageDetail.value
      ? await store.updateStage(project.value.id, detailEntity.value.id, { work_method: 'app' })
      : await store.updateCurrent(project.value.id, { work_method: 'app' })
    if (!updated) return
  }
  await router.push({
    name: isStageDetail.value ? 'stage-document' : 'document',
    params: {
      projectId: project.value.id,
      ...(isStageDetail.value ? { stageId: detailEntity.value.id } : {}),
    },
    query: { title: detailEntity.value.name },
  })
}

function announceSuccess(message: string, area: 'global' | 'progress' = 'global'): void {
  feedbackArea.value = area
  actionSuccess.value = message
  notifications.success(message)
}

function applyGameFeedback(game: ProgressResult['game']): void {
  if (!game) return
  notifications.setGameHistory(game.state.notifications)
  for (const message of progressGameMessages(game, t, locale.formatNumber)) notifications.success(message)
  announceDataChange('game')
}

async function refreshGameHistory(): Promise<void> {
  const knownUnreadIds = new Set(
    notifications.gameHistory.unread.map((notification) => notification.id),
  )
  try {
    const history = await gameApi.notifications()
    notifications.setGameHistory(history)
    for (const notification of history.unread) {
      if (!knownUnreadIds.has(notification.id)) notifications.success(t(notification.text))
    }
  } catch {
    // Project completion remains successful when the optional game history refresh fails.
  }
}

function applyProgressFeedback(
  progress: ProgressResult | null | undefined,
  stageId?: string | null,
  area?: 'global' | 'progress',
): void {
  if (!progress) return
  const entity = stageId
    ? progress.project.stages.find((stage) => stage.id === stageId)
      ?? (isStageDetail.value ? detailEntity.value : project.value)
    : progress.project
  const feedback = progressChangeNotification(
    progress,
    entity,
    t,
    locale.formatNumber,
    locale.formatUnit,
  )
  if (!feedback) return

  const message = progress.warning
    ? `${feedback.message}. ${progress.warning}`
    : feedback.message
  if (area) {
    feedbackArea.value = area
    actionSuccess.value = message
  }
  notifications.show(message, progress.warning ? 'warning' : feedback.kind)
}

function refreshStatistics(): void {
  if (!store.currentProject) return
  void store.loadStatistics(project.value.id, statisticsEntityId.value || undefined)
}

function openProjectEdit(): void {
  feedbackArea.value = 'global'
  store.clearDetailActionError()
  actionSuccess.value = null
  editDialogOpen.value = true
}

async function saveProject(payload: ProjectUpdate): Promise<void> {
  feedbackArea.value = 'global'
  const updated = await store.updateCurrent(project.value.id, payload)
  if (!updated) return
  editDialogOpen.value = false
  announceSuccess(t('Изменения проекта сохранены.'))
  chooseAvailableEntity()
  refreshStatistics()
}

async function toggleArchive(): Promise<void> {
  feedbackArea.value = 'global'
  const archived = project.value.status !== 'в архиве'
  const updated = await store.setArchived(project.value.id, archived)
  if (!updated) return
  announceSuccess(archived ? t('Проект перемещён в архив.') : t('Проект снова активен.'))
}

async function completeProject(): Promise<void> {
  feedbackArea.value = 'global'
  if (!window.confirm(t('Завершить проект «{name}»? После этого он будет доступен только для просмотра.', { name: project.value.name }))) return
  const updated = isStageDetail.value
    ? await store.completeStage(project.value.id, detailEntity.value.id)
    : await store.completeCurrent(project.value.id)
  if (!updated) return
  announceSuccess(isStageDetail.value ? t(isSharedProject.value ? 'Источник завершён.' : 'Этап завершён.') : t('Проект завершён.'))
  await refreshGameHistory()
  refreshStatistics()
}

async function deleteProject(): Promise<void> {
  feedbackArea.value = 'global'
  const entity = detailEntity.value
  const confirmation = isStageDetail.value
    ? t(isSharedProject.value
      ? 'Удалить источник «{name}» и всю его историю прогресса? Это действие нельзя отменить.'
      : 'Удалить этап «{name}» и всю его историю прогресса? Это действие нельзя отменить.', { name: entity.name })
    : t('Удалить проект «{name}» и все связанные данные? Это действие нельзя отменить.', { name: entity.name })
  if (!window.confirm(confirmation)) return
  if (isStageDetail.value) {
    const updated = await store.removeStage(project.value.id, entity.id)
    if (updated) await router.replace({ name: 'project-detail', params: { projectId: project.value.id } })
    return
  }
  if (await store.removeCurrent(project.value.id)) {
    await router.replace({ name: 'projects' })
  }
}

type ProgressExportDestination = 'clipboard' | 'download'

async function exportProgress(
  entity: Project,
  destination: ProgressExportDestination,
  parentName?: string,
): Promise<void> {
  if (entity.infinite) {
    notifications.warning(t('Для проекта без цели нельзя создать картинку прогресса'))
    return
  }
  if (sharingProgress.value) return

  sharingProgress.value = true
  try {
    const fractionDigits = entity.unit === 'symbols' ? 0 : 2
    const goalLabel = entity.goal === null
      ? t('Без лимита')
      : locale.formatNumber(entity.goal, fractionDigits)
    const shareTheme: 'light' | 'dark' = document.documentElement.dataset.theme === 'dark'
      ? 'dark'
      : 'light'
    const payload = {
      title: progressShareTitle(entity.name, parentName),
      progress: entity.progress,
      coverImage: project.value.cover_image,
      statusLabel: t(entity.status === 'активен' ? 'Активен' : entity.status === 'в архиве' ? 'В архиве' : 'Завершён'),
      progressText: `${locale.formatNumber(entity.total, fractionDigits)} ${locale.formatUnit(entity.unit, entity.total)} / ${goalLabel} ${locale.formatUnit(entity.unit, entity.goal ?? 0)}`,
      footerLabel: entity.deadline ? locale.formatDate(entity.deadline) : t('Без срока'),
      footerDetail: parentName || !entity.stages_enabled
        ? undefined
        : `${t(isSharedProject.value ? 'Источников' : 'Этапов')}: ${locale.formatNumber(entity.stages.length, 0)}`,
      theme: shareTheme,
    }
    if (destination === 'clipboard') {
      await copyProgressImage(payload)
    } else {
      await downloadProgressImage(payload)
    }
    notifications.show(
      destination === 'clipboard'
        ? t('Картинка прогресса добавлена в буфер обмена')
        : t('Картинка прогресса скачана в формате PNG'),
      'info',
    )
  } catch {
    notifications.error(t('Не удалось создать картинку прогресса.'))
  } finally {
    sharingProgress.value = false
  }
}

function copyProjectProgress(): Promise<void> {
  return exportProgress(project.value, 'clipboard')
}

function downloadProjectProgress(): Promise<void> {
  return exportProgress(project.value, 'download')
}

function copyStageProgress(stage: Project): Promise<void> {
  return exportProgress(stage, 'clipboard', project.value.name)
}

function downloadStageProgress(stage: Project): Promise<void> {
  return exportProgress(stage, 'download', project.value.name)
}

function openStageCreate(): void {
  feedbackArea.value = 'global'
  store.clearDetailActionError()
  actionSuccess.value = null
  editingStage.value = null
  stageDialogOpen.value = true
}

function openStageEdit(stage: Project): void {
  feedbackArea.value = 'global'
  store.clearDetailActionError()
  actionSuccess.value = null
  editingStage.value = stage
  stageDialogOpen.value = true
}

async function openStage(stage: Project): Promise<void> {
  await router.push({
    name: 'stage-detail',
    params: { projectId: project.value.id, stageId: stage.id },
  })
}

async function saveStage(payload: StageCreate | EntityUpdate): Promise<void> {
  feedbackArea.value = 'global'
  const creatingSharedSource = isSharedProject.value && !editingStage.value
  const updated = editingStage.value
    ? await store.updateStage(project.value.id, editingStage.value.id, payload as EntityUpdate)
    : await store.createStage(project.value.id, payload as StageCreate)
  if (!updated) return
  stageDialogOpen.value = false
  announceSuccess(editingStage.value ? t(isSharedProject.value ? 'Источник сохранён.' : 'Этап сохранён.') : t(isSharedProject.value ? 'Источник создан.' : 'Этап создан.'))
  if (creatingSharedSource) {
    const sourceName = (payload as StageCreate).name
    const source = updated.stages.find((stage) => stage.name === sourceName)
    if (source) {
      selectedEntityId.value = source.id
      statisticsEntityId.value = source.id
    }
  }
  editingStage.value = null
  chooseAvailableEntity()
  refreshStatistics()
}

async function removeStage(stage: Project): Promise<void> {
  feedbackArea.value = 'global'
  const updated = await store.removeStage(project.value.id, stage.id)
  if (!updated) return
  announceSuccess(t(isSharedProject.value ? 'Источник удалён.' : 'Этап удалён.'))
  chooseAvailableEntity()
  refreshStatistics()
}

async function completeStage(stage: Project): Promise<void> {
  feedbackArea.value = 'global'
  const updated = await store.completeStage(project.value.id, stage.id)
  if (!updated) return
  announceSuccess(t(isSharedProject.value ? 'Источник завершён.' : 'Этап завершён.'))
  await refreshGameHistory()
  refreshStatistics()
}

async function reorderStages(stageIds: string[]): Promise<void> {
  feedbackArea.value = 'global'
  const updated = await store.reorderStages(project.value.id, stageIds)
  if (updated) announceSuccess(t(isSharedProject.value ? 'Порядок источников сохранён.' : 'Порядок этапов сохранён.'))
}

async function synchronizeStage(stage: Project): Promise<void> {
  if (stage.work_method !== 'sync') {
    if (!window.confirm(t('Синхронизация будет получать записи только из внешнего файла. Ручной ввод и текстовый редактор станут недоступны до следующей смены метода. Переключить метод и продолжить?'))) return
    const updated = await store.updateStage(project.value.id, stage.id, { work_method: 'sync' })
    if (!updated) return
  }
  feedbackArea.value = 'global'
  syncRunning.value = true
  try {
    const result = await integrationsApi.runSync(project.value.id, stage.id)
    applyProgressFeedback(result.progress, stage.id)
    applyGameFeedback(result.progress?.game ?? null)
    announceSuccess(t(result.changed ? 'Синхронизация завершена' : 'Документ не изменился. Текущий объём уже актуален.'))
    await store.refreshCurrent(project.value.id)
    refreshStatistics()
  } catch (error) {
    store.detailActionError = t(apiErrorMessage(error))
  } finally {
    syncRunning.value = false
  }
}

async function recordProgress(payload: ProgressCreate): Promise<void> {
  const result = await store.recordProgress(project.value.id, payload)
  if (!result) return
  applyProgressFeedback(result, payload.stage_id, 'progress')
  applyGameFeedback(result.game)
  await loadStreakSummaries()
  refreshStatistics()
}

async function deleteProgress(entryId: string, stageId?: string): Promise<void> {
  feedbackArea.value = 'progress'
  const updated = await store.deleteProgress(project.value.id, entryId, stageId)
  if (!updated) return
  announceSuccess(t('Запись прогресса удалена, итог пересчитан.'), 'progress')
  refreshStatistics()
}

watch([projectId, routeStageId], () => { void loadProject() }, { immediate: true })
watch(statisticsEntityId, () => {
  actionSuccess.value = null
  refreshStatistics()
})
watch([selectedEntityId, statisticsEntityId, projectId], ([entityId, statisticsId, id]) => {
  if (id) store.saveDetailSelection(id, entityId, statisticsId)
})

function handleProjectShortcut(event: Event): void {
  const action = (event as CustomEvent<string>).detail
  if (action === 'sync') void synchronizeProject()
  if (action === 'edit' && detailEntity.value.status !== 'завершен') {
    if (isStageDetail.value) openStageEdit(detailEntity.value)
    else openProjectEdit()
  }
  if (action === 'delete') void deleteProject()
  if (action === 'complete') void completeProject()
  if (action === 'archive' && !isStageDetail.value) void toggleArchive()
  if (action === 'statistics') document.querySelector('#statistics-heading')?.scrollIntoView({ behavior: 'smooth' })
}

onMounted(() => {
  window.addEventListener('nfprogress:project-shortcut', handleProjectShortcut)
  stopDataChanges = onDataChange((scope) => {
    if (scope !== 'projects' || store.detailBusy) return
    if (detailViewActive) void refreshChangedProject()
    else pendingProjectRefresh = true
  })
})

onIonViewWillEnter(() => {
  detailViewActive = true
  if (pendingProjectRefresh || store.currentProject?.id === projectId.value) {
    pendingProjectRefresh = false
    void refreshChangedProject()
  }
})
onIonViewWillLeave(() => { detailViewActive = false })

onBeforeUnmount(() => {
  store.cancelDetail()
  stopDataChanges?.()
  window.removeEventListener('nfprogress:project-shortcut', handleProjectShortcut)
})
</script>

<template>
  <IonPage>
    <IonContent :fullscreen="true" class="detail-content">
      <div :key="detailAnimationVersion" class="detail-workspace">
        <RouterLink
          class="back-link"
          :to="isStageDetail ? { name: 'project-detail', params: { projectId } } : { name: 'projects' }"
        >
          <IonIcon :icon="arrowBackOutline" aria-hidden="true" />
          {{ isStageDetail ? t('Вернуться к проекту') : t('Все проекты') }}
        </RouterLink>

        <StatePanel
          v-if="store.detailLoading"
          :title="t('Открываем проект')"
          :message="t('Загружаем актуальные цели и этапы.')"
          loading
        />
        <StatePanel
          v-else-if="store.detailError"
          :title="t('Не удалось открыть проект')"
          :message="store.detailError"
          :icon="alertCircleOutline"
        >
          <button class="nf-button" type="button" @click="loadProject">{{ t('Повторить') }}</button>
        </StatePanel>

        <article v-else-if="store.currentProject" class="project-detail">
          <header class="detail-header">
            <div>
              <p class="detail-status">{{ presentation.statusLabel }}</p>
              <p v-if="isStageDetail" class="detail-parent">{{ project.name }}</p>
              <h1>{{ detailEntity.name }}</h1>
            </div>
            <ProgressRing
              size="large"
              :value="isSharedProject && detailEntity.infinite ? 100 : presentation.progress"
              :infinite="detailEntity.infinite"
              :full="isSharedProject && detailEntity.infinite"
              :label="`${t('Прогресс')}: ${isSharedProject && detailEntity.infinite ? '100%' : presentation.progressLabel}`"
            />
          </header>

          <nav class="project-actions" :aria-label="t('Действия проекта')">
            <button
              v-if="detailEntity.status !== 'завершен' && selectedSyncEntity.work_method !== 'app'"
              class="nf-button project-sync-button"
              type="button"
              :disabled="syncLoading || syncRunning"
              @click="synchronizeProject"
            >
              <IonSpinner v-if="syncLoading || syncRunning" name="crescent" aria-hidden="true" />
              <IonIcon v-else :icon="refreshOutline" aria-hidden="true" />
              {{ isStageDetail && !isSharedProject ? t('Синхронизировать') : selectedSyncConfigured ? t('Синхронизировать сейчас') : t('Подключить источник') }}
            </button>
            <button
              v-if="isSharedProject && !isStageDetail && project.status !== 'завершен'"
              class="nf-button nf-button--secondary project-add-source-button"
              type="button"
              :disabled="store.detailBusy"
              @click="openStageCreate"
            >
              <IonIcon :icon="addOutline" aria-hidden="true" />{{ t('Добавить источник') }}
            </button>
            <RouterLink
              v-if="isStageDetail || !project.stages.length"
              class="nf-button nf-button--secondary project-notes-button"
              :to="{
                name: 'project-notes',
                params: { projectId: project.id },
                ...(isStageDetail ? { query: { stageId: detailEntity.id } } : {}),
              }"
            >
              <IonIcon :icon="documentTextOutline" aria-hidden="true" />{{ t('Заметки и карта') }}
            </RouterLink>
            <button
              v-if="canOpenText"
              class="nf-button nf-button--secondary project-notes-button"
              type="button"
              @click="openTextEditor"
            >
              <IonIcon :icon="documentTextOutline" aria-hidden="true" />{{ t('Текст') }}
            </button>
            <ProgressShareMenu
              :label="t('Поделиться прогрессом «{name}»', { name: detailEntity.name })"
              :title="detailEntity.infinite ? t('Для проекта без цели нельзя создать картинку прогресса') : undefined"
              :busy="sharingProgress"
              :disabled="detailEntity.infinite"
              @copy="isStageDetail ? copyStageProgress(detailEntity) : copyProjectProgress()"
              @save="isStageDetail ? downloadStageProgress(detailEntity) : downloadProjectProgress()"
            />
            <button
              v-if="detailEntity.status !== 'завершен' && !isSharedProject"
              class="nf-button nf-button--secondary"
              type="button"
              :disabled="store.detailBusy"
              @click="isStageDetail ? openStageEdit(detailEntity) : openProjectEdit()"
            >
              <IonIcon :icon="createOutline" aria-hidden="true" />{{ t('Изменить') }}
            </button>
            <button
              v-if="!isStageDetail && project.status !== 'завершен' && !isSharedProject"
              class="nf-button nf-button--secondary"
              type="button"
              :disabled="store.detailBusy"
              @click="toggleArchive"
            >
              <IonIcon :icon="project.status === 'в архиве' ? refreshOutline : archiveOutline" aria-hidden="true" />
              {{ project.status === 'в архиве' ? t('Вернуть в активные') : t('В архив') }}
            </button>
            <button
              v-if="detailEntity.status !== 'завершен' && !isSharedProject"
              class="nf-button"
              type="button"
              :title="!canCompleteProject ? t('Чтобы завершить проект, сначала достигните его цели.') : undefined"
              :disabled="store.detailBusy || !canCompleteProject"
              @click="completeProject"
            >
              <IonIcon :icon="checkmarkCircleOutline" aria-hidden="true" />{{ t('Завершить') }}
            </button>
            <button
              v-if="!isSharedProject || isStageDetail"
              class="nf-button project-delete-button"
              type="button"
              :disabled="store.detailBusy"
              @click="deleteProject"
            >
              <IonIcon :icon="trashOutline" aria-hidden="true" />{{ isSharedProject && isStageDetail ? t('Удалить источник') : t('Удалить проект') }}
            </button>
          </nav>

          <div v-if="feedbackArea === 'global'" class="action-announcements" aria-live="polite">
            <p v-if="store.detailActionError" class="action-error" role="alert">
              {{ store.detailActionError }}
            </p>
            <p v-else-if="actionSuccess" class="action-success">{{ actionSuccess }}</p>
          </div>

          <section class="progress-hero" :aria-label="t('Прогресс проекта')">
            <div><span>{{ t('Написано') }}</span><strong><AnimatedNumber :value="detailEntity.total" :digits="detailEntity.unit === 'symbols' ? 0 : 2" /> {{ locale.formatUnit(detailEntity.unit, detailEntity.total) }}</strong></div>
            <div><span>{{ t('Цель') }}</span><strong>{{ presentation.goalLabel }}<template v-if="!detailEntity.infinite && detailEntity.goal !== null">&nbsp;{{ locale.formatUnit(detailEntity.unit, detailEntity.goal) }}</template></strong></div>
            <ProgressBar
              v-if="!detailEntity.infinite"
              :value="presentation.progress"
              :label="`${t('Прогресс')}: ${presentation.progressLabel}`"
            />
          </section>

          <section class="detail-facts" :aria-label="t('Сведения о проекте')">
            <div class="fact-card"><IonIcon :icon="calendarClearOutline" aria-hidden="true" /><span>{{ t('Срок') }}</span><strong>{{ locale.formatDate(detailEntity.deadline) }}</strong></div>
            <div class="fact-card"><IonIcon :icon="documentTextOutline" aria-hidden="true" /><span>{{ t('Записей прогресса') }}</span><strong>{{ locale.formatNumber(detailEntity.progress_entries.length, 0) }}</strong></div>
            <div v-if="detailEntity.today_goal !== null" class="fact-card"><IonIcon :icon="layersOutline" aria-hidden="true" /><span>{{ t('Цель на сегодня') }}</span><strong>{{ numberForProject(detailEntity.today_goal) }} {{ locale.formatUnit(detailEntity.unit, detailEntity.today_goal) }}</strong></div>
            <StreakBadge
              v-if="displayedStreakEntity"
              class="detail-streak detail-streak--entity"
              :length="displayedStreakEntity.streak_length"
              :max-length="displayedStreakEntity.max_streak"
              :status="displayedStreakEntity.streak_status"
              :scope="displayedStreakScope"
              show-max
            />
          </section>

          <StageWorkspace
            v-if="!isStageDetail"
            :project="project"
            :busy="store.detailBusy"
            :sharing="sharingProgress"
            :streaks-enabled="streaksEnabled"
            :stage-sort="stageSort"
            @add="openStageCreate"
            @edit="openStageEdit"
            @remove="removeStage"
            @complete="completeStage"
            @reorder="reorderStages"
            @copy="copyStageProgress"
            @save="downloadStageProgress"
            @open="openStage"
            @sync="synchronizeStage"
            @sort="saveStageSort"
          />
            <ProgressWorkspace
            v-model="selectedEntityId"
            :project="project"
            :busy="store.detailBusy"
            :submitting="store.detailOperation === 'record-progress'"
            :syncs="syncSummaries"
            :text-symbols="textSymbols"
            :error="feedbackArea === 'progress' ? store.detailActionError : null"
            :success="feedbackArea === 'progress' ? actionSuccess : null"
            :fixed-stage-id="isStageDetail ? detailEntity.id : null"
            @record="recordProgress"
            @remove="deleteProgress"
          />
          <StatisticsWorkspace
            v-model:entity-id="statisticsEntityId"
            :statistics="store.statistics"
            :project="project"
            :loading="store.statisticsLoading"
            :error="store.statisticsError"
            :fixed-stage-id="isStageDetail ? detailEntity.id : null"
            @retry="refreshStatistics"
          />
        </article>
      </div>
    </IonContent>

    <ProjectEditDialog
      v-if="store.currentProject"
      :open="editDialogOpen"
      :project="project"
      :submitting="store.detailOperation === 'update-project'"
      :api-error="store.detailActionError"
      :global-streak-enabled="streaksEnabled"
      :game-mode-enabled="gameModeEnabled"
      @close="editDialogOpen = false"
      @submit="saveProject"
    />
    <StageDialog
      v-if="store.currentProject"
      :open="stageDialogOpen"
      :project-unit="project.unit"
      :planning-date="project.planning_date"
      :stage="editingStage"
      :shared-source="isSharedProject && !editingStage"
      :default-name="isSharedProject && !editingStage ? sharedSourceDefaultName : ''"
      :submitting="Boolean(store.detailOperation?.includes('stage'))"
      :api-error="store.detailActionError"
      :global-streak-enabled="streaksEnabled"
      :game-mode-enabled="gameModeEnabled"
      @close="stageDialogOpen = false"
      @submit="saveStage"
    />
  </IonPage>
</template>

<style scoped>
.detail-content { --background: var(--nf-color-canvas); }
.detail-workspace { width: min(100%, 76rem); min-height: 100%; margin: 0 auto; padding: calc(var(--nf-space-6) + env(safe-area-inset-top)) clamp(1rem, 4vw, 4rem) var(--nf-space-7); }
.back-link { display: inline-flex; gap: var(--nf-space-2); align-items: center; min-height: 2.75rem; margin-bottom: var(--nf-space-5); padding: 0 var(--nf-space-2); border-radius: var(--nf-radius-sm); color: var(--nf-color-primary); font-weight: 700; text-decoration: none; }
.detail-header { display: flex; gap: var(--nf-space-5); align-items: flex-start; justify-content: space-between; }
.detail-status { display: inline-flex; margin: 0 0 var(--nf-space-3); padding: 0.35rem 0.7rem; border-radius: var(--nf-radius-pill); background: var(--nf-color-primary-soft); color: var(--nf-color-primary); font-size: 0.75rem; font-weight: 800; }
.detail-parent { margin: 0 0 var(--nf-space-2); color: var(--nf-color-text-muted); font-size: 0.85rem; font-weight: 700; }
.detail-header h1 { max-width: 50rem; margin: 0; overflow-wrap: anywhere; color: var(--nf-color-text); font-family: var(--nf-font-serif); font-size: clamp(1.8rem, 3.5vw, 2.75rem); letter-spacing: -0.025em; line-height: 1.15; }
.project-actions { display: flex; flex-wrap: wrap; gap: var(--nf-space-2); margin-top: var(--nf-space-5); }
.project-sync-button { box-shadow: 0 8px 20px color-mix(in srgb, var(--nf-color-primary) 20%, transparent); }
.project-notes-button { color: var(--nf-color-text); text-decoration: none; }
.project-delete-button { margin-left: auto; border-color: color-mix(in srgb, var(--nf-color-danger) 35%, transparent); background: transparent; color: var(--nf-color-danger); }
.project-delete-button:hover:not(:disabled) { background: color-mix(in srgb, var(--nf-color-danger) 10%, transparent); }
.action-announcements { min-height: 1.25rem; margin-top: var(--nf-space-3); }
.action-announcements p { margin: 0; padding: var(--nf-space-3); border-radius: var(--nf-radius-sm); font-size: 0.85rem; }
.action-error { background: color-mix(in srgb, var(--nf-color-danger) 10%, var(--nf-color-surface)); color: var(--nf-color-danger); }
.action-success { background: color-mix(in srgb, var(--nf-color-success) 10%, var(--nf-color-surface)); color: var(--nf-color-success); }
.progress-hero { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--nf-space-5); margin-top: var(--nf-space-5); padding: var(--nf-space-5); border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-lg); background: var(--nf-color-surface); box-shadow: var(--nf-shadow-card); }
.progress-hero > div { display: grid; gap: var(--nf-space-1); }
.progress-hero > div > span { color: var(--nf-color-text-muted); font-size: 0.78rem; font-weight: 700; text-transform: uppercase; }
.progress-hero strong { color: var(--nf-color-text); font-size: clamp(1.1rem, 2.5vw, 1.35rem); }
.progress-hero :deep(.progress-bar) { grid-column: 1 / -1; width: 100%; }
.detail-facts { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--nf-space-3); margin-top: var(--nf-space-4); }
.fact-card { display: grid; grid-template-columns: auto 1fr; gap: var(--nf-space-1) var(--nf-space-3); padding: var(--nf-space-4); border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-md); background: var(--nf-color-surface); }
.fact-card ion-icon { grid-row: 1 / 3; align-self: center; color: var(--nf-color-accent); font-size: 1.35rem; }
.fact-card span { color: var(--nf-color-text-muted); font-size: 0.75rem; }
.fact-card strong { overflow: hidden; color: var(--nf-color-text); font-size: 0.92rem; text-overflow: ellipsis; white-space: nowrap; }
.detail-streak { width: 100%; border-radius: var(--nf-radius-md); }

@media (max-width: 42rem) {
  .detail-header { display: grid; }
  .detail-header :deep(.progress-ring) { justify-self: start; }
  .project-actions { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .project-delete-button { margin-left: 0; }
  .detail-facts { grid-template-columns: 1fr; }
}
@media (max-width: 25rem) {
  .project-actions { grid-template-columns: 1fr; }
}
</style>
