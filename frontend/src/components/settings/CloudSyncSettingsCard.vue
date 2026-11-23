<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import { IonIcon, IonSpinner } from '@ionic/vue'
import { cloudOutline, lockClosedOutline, syncOutline, warningOutline } from 'ionicons/icons'

import { MetadataImportContinuationError } from '@/cloud/projectMetadataMigrationRuntime'
import DiagnosticDetails from './DiagnosticDetails.vue'
import FriendlyStatus from './FriendlyStatus.vue'
import AccountCatalogAuthorityPanel from './AccountCatalogAuthorityPanel.vue'
import ContentNoteAuthorityPanel from './ContentNoteAuthorityPanel.vue'
import MapAuthorityPanel from './MapAuthorityPanel.vue'
import DocumentAuthorityPanel from './DocumentAuthorityPanel.vue'
import ProgressAuthorityPanel from './ProgressAuthorityPanel.vue'
import GameAuthorityPanel from './GameAuthorityPanel.vue'
import StageStructuralAuthorityPanel from './StageStructuralAuthorityPanel.vue'
import ProjectMetadataAuthorityPanel from './ProjectMetadataAuthorityPanel.vue'
import ProjectCoverAuthorityPanel from './ProjectCoverAuthorityPanel.vue'

import { encodeBase64Url } from '@/api/base64url'
import {
  useCloudSessionStore,
} from '@/stores/cloudSession'
import { useLocaleStore } from '@/stores/locale'

const cloud = useCloudSessionStore()
const locale = useLocaleStore()
const t = locale.translate

const accountUsername = ref('')
const accountPassword = ref('')
const encryptionPassword = ref('')
const encryptionPasswordConfirmation = ref('')
const unlockPassword = ref('')
const recoveryKey = ref<Uint8Array | null>(null)
const localError = ref<string | null>(null)
const pendingProjectId = ref<string | null>(null)
const pendingImportProjectId = ref<string | null>(null)
const importProjectName = ref('')
const needsLegacyImportName = ref(false)

const recoveryKeyText = computed(() => recoveryKey.value === null ? '' : encodeBase64Url(recoveryKey.value))
const hasPendingOnboarding = computed(() => recoveryKey.value !== null)


function clearRecoveryKey(): void {
  recoveryKey.value?.fill(0)
  recoveryKey.value = null
}

function clearProvisioningInputs(): void {
  encryptionPassword.value = ''
  encryptionPasswordConfirmation.value = ''
}

function clearProjectConfirmations(): void {
  pendingProjectId.value = null
  pendingImportProjectId.value = null
  importProjectName.value = ''
  needsLegacyImportName.value = false
}

async function signIn(): Promise<void> {
  if (cloud.busy) return
  localError.value = null
  clearProjectConfirmations()
  try {
    await cloud.login(accountUsername.value, accountPassword.value)
  } finally {
    accountPassword.value = ''
  }
}

async function prepareProvisioning(): Promise<void> {
  if (cloud.busy) return
  localError.value = null
  if (encryptionPassword.value.length === 0) {
    localError.value = t('Введите отдельный пароль шифрования.')
    return
  }
  if (encryptionPassword.value !== encryptionPasswordConfirmation.value) {
    localError.value = t('Пароли шифрования не совпадают.')
    return
  }
  try {
    recoveryKey.value = await cloud.prepareProvisioning(encryptionPassword.value)
  } finally {
    clearProvisioningInputs()
  }
}

async function confirmRecoveryKey(): Promise<void> {
  if (cloud.busy || !hasPendingOnboarding.value) return
  localError.value = null
  try {
    await cloud.submitProvisioning()
    clearRecoveryKey()
  } catch {
    // The retained immutable wrapper set and display copy allow a safe retry.
  }
}

async function reconcileProvisioning(): Promise<void> {
  if (cloud.busy || !hasPendingOnboarding.value) return
  try {
    if (await cloud.reconcileProvisioning()) clearRecoveryKey()
  } catch {
    // Store state provides a redacted error; never render request or key data.
  }
}

function cancelProvisioning(): void {
  cloud.cancelProvisioning()
  clearRecoveryKey()
  clearProvisioningInputs()
  localError.value = null
}

async function unlock(): Promise<void> {
  if (cloud.busy || unlockPassword.value.length === 0) return
  try {
    await cloud.unlock(unlockPassword.value)
  } catch {
    // The safe store status supplies the user-facing outcome.
  } finally {
    unlockPassword.value = ''
  }
}

async function lock(): Promise<void> {
  clearProjectConfirmations()
  try {
    await cloud.lock()
  } catch {
    // Lock failure remains redacted; do not expose runtime details.
  } finally {
    unlockPassword.value = ''
  }
}

async function logout(): Promise<void> {
  cancelProvisioning()
  clearProjectConfirmations()
  try {
    await cloud.logout()
  } catch {
    // logout() already invalidates visible state in its finally branch.
  } finally {
    accountPassword.value = ''
    unlockPassword.value = ''
  }
}

async function retry(): Promise<void> {
  try {
    await cloud.retry()
  } catch {
    // The store reports only a redacted retryable state.
  }
}

async function requestConnection(projectId: string): Promise<void> {
  localError.value = null
  try {
    pendingProjectId.value = await cloud.prepareProjectConnection(projectId) ? projectId : null
  } catch {
    localError.value = t('Не удалось проверить проект. Серверная регистрация не выполнялась.')
  }
}

async function confirmConnection(projectId: string): Promise<void> {
  if (pendingProjectId.value !== projectId || cloud.busy) return
  pendingProjectId.value = null
  try {
    await cloud.bootstrapProject(projectId)
  } catch {
    // Durable state remains available for an exact resume.
  }
}

function requestImport(projectId: string): void {
  pendingImportProjectId.value = projectId
  importProjectName.value = ''
  needsLegacyImportName.value = false
  localError.value = null
}

async function confirmImport(projectId: string): Promise<void> {
  if (pendingImportProjectId.value !== projectId || cloud.busy) return
  pendingImportProjectId.value = null
  try {
    await cloud.importProject(projectId, importProjectName.value)
    importProjectName.value = ''
    needsLegacyImportName.value = false
  } catch (error) {
    if (error instanceof Error && error.message === 'legacy_import_name_required') {
      pendingImportProjectId.value = projectId
      needsLegacyImportName.value = true
    } else if (error instanceof MetadataImportContinuationError) localError.value = t('Проверенные страницы сохранены. Повторите импорт, чтобы продолжить чтение истории.')
    else if (error instanceof Error && error.message === 'metadata_import_resource_limit') localError.value = t('Не удалось обработать всю историю проекта. Проверенная часть сохранена. Экспортируйте журнал для проверки.')
    else localError.value = t('Не удалось завершить согласование. Проверьте состояние и безопасно повторите действие.')
  }
}

async function resumeProject(projectId: string): Promise<void> {
  try {
    await cloud.resumeProject(projectId)
  } catch {
    // Existing token, event IDs and durable progress remain unchanged.
  }
}

async function pauseProject(projectId: string): Promise<void> {
  try {
    await cloud.pauseProject(projectId)
  } catch {
    // Pause is fail-closed and never deletes local or remote data.
  }
}

async function refreshProjects(): Promise<void> {
  try {
    await cloud.refreshProjects()
  } catch {
    // The owner renders only the redacted failure state.
  }
}

onBeforeUnmount(() => {
  cancelProvisioning()
  clearProjectConfirmations()
})
</script>

<template>
  <section class="settings-card cloud-sync-card" aria-labelledby="cloud-sync-title">
    <AccountCatalogAuthorityPanel v-if="cloud.hasProvisionedKey && !cloud.busy" />
    <GameAuthorityPanel v-if="cloud.hasProvisionedKey && !cloud.busy" />
    <div class="settings-card__heading">
      <h2 id="cloud-sync-title">{{ t('Облачная синхронизация заметок') }}</h2>
      <p>{{ t('Отдельный защищённый сеанс для заметок. Он не связан с фоновой синхронизацией документов.') }}</p>
    </div>

    <div class="cloud-sync-card__status" :data-status="cloud.status" role="status">
      <IonIcon :icon="cloud.status === 'key_locked' ? lockClosedOutline : cloudOutline" aria-hidden="true" />
      <FriendlyStatus domain="session" :code="cloud.status" subsystem="sync" />
    </div>

    <p v-if="cloud.authenticated" class="cloud-sync-card__account">
      {{ t('Подключён аккаунт') }}: <strong>{{ cloud.username }}</strong>
    </p>
    <p v-if="cloud.errorMessage" class="cloud-sync-card__error" role="alert">{{ t(cloud.errorMessage) }}</p>
    <DiagnosticDetails v-if="cloud.errorMessage" :code="cloud.errorCode" subsystem="sync" />
    <p v-if="localError" class="cloud-sync-card__error" role="alert">{{ localError }}</p>

    <form v-if="cloud.status === 'logged_out'" class="cloud-sync-card__form" @submit.prevent="signIn">
      <label>
        <span>{{ t('Имя пользователя или e-mail') }}</span>
        <input v-model="accountUsername" autocomplete="username" required />
      </label>
      <label>
        <span>{{ t('Пароль аккаунта') }}</span>
        <input v-model="accountPassword" type="password" autocomplete="current-password" required />
      </label>
      <button class="nf-button" type="submit" :disabled="cloud.busy">
        <IonSpinner v-if="cloud.busy" name="crescent" aria-hidden="true" />
        {{ cloud.busy ? t('Входим…') : t('Войти в облачный аккаунт') }}
      </button>
    </form>

    <form v-else-if="cloud.status === 'provisioning' && !hasPendingOnboarding" class="cloud-sync-card__form" @submit.prevent="prepareProvisioning">
      <p class="cloud-sync-card__warning">
        <IonIcon :icon="warningOutline" aria-hidden="true" />
        {{ t('Пароль шифрования не отправляется на сервер. Если потерять и его, и ключ восстановления, доступ к зашифрованным данным будет утрачен.') }}
      </p>
      <label>
        <span>{{ t('Новый пароль шифрования') }}</span>
        <input v-model="encryptionPassword" type="password" autocomplete="new-password" required />
      </label>
      <label>
        <span>{{ t('Повторите пароль шифрования') }}</span>
        <input v-model="encryptionPasswordConfirmation" type="password" autocomplete="new-password" required />
      </label>
      <button class="nf-button" type="submit" :disabled="cloud.busy">
        <IonSpinner v-if="cloud.busy" name="crescent" aria-hidden="true" />
        {{ cloud.busy ? t('Готовим ключи…') : t('Создать ключ восстановления') }}
      </button>
    </form>

    <div v-else-if="hasPendingOnboarding" class="cloud-sync-card__recovery" role="region" :aria-label="t('ключ восстановления')">
      <p class="cloud-sync-card__warning">
        <IonIcon :icon="warningOutline" aria-hidden="true" />
        {{ t('Сохраните этот ключ восстановления самостоятельно до продолжения. Он не сохраняется приложением и не передаётся серверу.') }}
      </p>
      <output class="cloud-sync-card__recovery-key">{{ recoveryKeyText }}</output>
      <p>{{ t('Восстановление доступа через ключ восстановления пока не реализовано. Не удаляйте его и не рассчитывайте на автоматическое восстановление.') }}</p>
      <div class="cloud-sync-card__actions">
        <button class="nf-button" type="button" :disabled="cloud.busy" @click="confirmRecoveryKey">{{ t('Я сохранил(а) ключ восстановления') }}</button>
        <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="reconcileProvisioning">{{ t('Проверить статус на сервере') }}</button>
        <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="cancelProvisioning">{{ t('Отменить настройку') }}</button>
      </div>
    </div>

    <form v-else-if="cloud.status === 'key_locked'" class="cloud-sync-card__form" @submit.prevent="unlock">
      <label>
        <span>{{ t('Пароль шифрования') }}</span>
        <input v-model="unlockPassword" type="password" autocomplete="current-password" required />
      </label>
      <div class="cloud-sync-card__actions">
        <button class="nf-button" type="submit" :disabled="cloud.busy">
          <IonSpinner v-if="cloud.busy" name="crescent" aria-hidden="true" />
          {{ cloud.busy ? t('Разблокируем…') : t('Разблокировать') }}
        </button>
        <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="logout">{{ t('Выйти из аккаунта') }}</button>
      </div>
    </form>

    <template v-else-if="cloud.authenticated && cloud.hasProvisionedKey">
      <div class="cloud-sync-card__actions">
        <button v-if="cloud.canRunCycle && cloud.status !== 'syncing'" class="nf-button" type="button" :disabled="cloud.busy" @click="retry">
          <IonIcon :icon="syncOutline" aria-hidden="true" />
          {{ t('Синхронизировать заметки') }}
        </button>
        <IonSpinner v-else-if="cloud.status === 'syncing'" name="crescent" :aria-label="t('Синхронизация заметок')" />
        <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="refreshProjects">{{ t('Обновить состояние проектов') }}</button>
        <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="lock">{{ t('Заблокировать ключ') }}</button>
        <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="logout">{{ t('Выйти из аккаунта') }}</button>
      </div>

      <div class="cloud-sync-card__scope-note">
        <p>{{ t('Обмениваются поддерживаемые заметки проекта. Подключение настроек и этапов выполняется отдельно. Тексты, карты и остальные данные пока не передаются.') }}</p>
        <p>{{ t('Если один облачный проект приостановлен, требует проверки или ещё не импортирован, обмен изменениями аккаунта временно остановлен.') }}</p>
      </div>

      <section v-if="cloud.projectBootstrapEnabled" class="cloud-sync-card__projects" :aria-label="t('Проекты облачной синхронизации')">
        <h3>{{ t('Проекты') }}</h3>
        <p v-if="cloud.projects.length === 0">{{ t('Локальных и облачных проектов для подключения пока нет.') }}</p>
        <article v-for="project in cloud.projects" :key="project.projectId" class="cloud-project" :data-project-status="project.status">
          <div>
            <strong>{{ project.name ?? t('Облачный проект без названия на этом устройстве') }}</strong>
            <small v-if="project.origin === 'remote'">{{ t('Идентификатор') }}: {{ project.projectId }}</small>
          </div>
          <FriendlyStatus domain="project" :code="project.status" subsystem="projects" />
          <p v-if="project.status === 'local_only' && project.connectionAvailable" class="cloud-project__available">{{ t('Подключение доступно. Проект останется локальным, пока вы явно не подтвердите действие.') }}</p>
          <p v-if="project.reason">{{ t(project.reason) }}</p>
          <DiagnosticDetails v-if="project.reasonCode" :code="project.reasonCode" subsystem="projects" />

          <div v-if="project.status === 'local_only' && project.connectionAvailable" class="cloud-sync-card__actions">
            <button v-if="pendingProjectId !== project.projectId" class="nf-button" type="button" :disabled="cloud.busy" @click="requestConnection(project.projectId)">{{ t('Проверить и подключить') }}</button>
            <template v-else>
              <p class="cloud-sync-card__warning">{{ t('Будут подготовлены и отправлены только поддерживаемые заметки этого проекта. Остальные локальные проекты не затрагиваются.') }}</p>
              <button class="nf-button" type="button" :disabled="cloud.busy" @click="confirmConnection(project.projectId)">{{ t('Подтвердить первоначальное подключение') }}</button>
              <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="pendingProjectId = null">{{ t('Отмена') }}</button>
            </template>
          </div>

          <div v-else-if="project.status === 'import_available'" class="cloud-sync-card__actions">
            <button v-if="pendingImportProjectId !== project.projectId" class="nf-button" type="button" :disabled="cloud.busy" @click="requestImport(project.projectId)">{{ t('Импортировать на это устройство') }}</button>
            <form v-else class="cloud-sync-card__inline-form" @submit.prevent="confirmImport(project.projectId)">
              <label v-if="needsLegacyImportName">
                <span>{{ t('Название для проекта без облачных настроек') }}</span>
                <input v-model="importProjectName" required />
              </label>
              <p v-if="needsLegacyImportName">{{ t('Это название используется только на этом устройстве. Будет создан новый минимальный локальный проект; совпадающий ID или локальное имя блокирует импорт, объединение и перезапись запрещены.') }}</p>
              <div class="cloud-sync-card__actions">
                <button class="nf-button" type="submit" :disabled="cloud.busy">{{ t('Подтвердить импорт') }}</button>
                <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="clearProjectConfirmations">{{ t('Отмена') }}</button>
              </div>
            </form>
          </div>

          <div v-else-if="['registering', 'preparing_initial_notes', 'uploading_initial_notes', 'completing_registration', 'pulling_remote_notes', 'remaining_work'].includes(project.status)" class="cloud-sync-card__actions">
            <button class="nf-button" type="button" :disabled="cloud.busy" @click="resumeProject(project.projectId)">{{ t('Безопасно продолжить') }}</button>
          </div>
          <div v-else-if="project.status === 'initial_sync_completed'" class="cloud-sync-card__actions">
            <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="pauseProject(project.projectId)">{{ t('Приостановить облачный цикл') }}</button>
          </div>
          <div v-else-if="project.status === 'paused'" class="cloud-sync-card__actions">
            <button class="nf-button" type="button" :disabled="cloud.busy" @click="resumeProject(project.projectId)">{{ t('Возобновить') }}</button>
          </div>
          <MapAuthorityPanel v-if="project.origin !== 'remote' && project.status !== 'local_only'" :project-id="project.projectId" />
<ProgressAuthorityPanel v-if="project.status !== 'local_only'" :project-id="project.projectId" />
<DocumentAuthorityPanel v-if="project.origin !== 'remote' && project.status !== 'local_only'" :project-id="project.projectId" />
          <ContentNoteAuthorityPanel v-if="project.origin !== 'remote' && project.status !== 'local_only'" :project-id="project.projectId" />
          <StageStructuralAuthorityPanel v-if="project.origin !== 'remote' && project.status !== 'local_only'" :project-id="project.projectId" />
          <ProjectCoverAuthorityPanel v-if="project.origin !== 'remote' && project.status !== 'local_only'" :project-id="project.projectId" />
          <ProjectMetadataAuthorityPanel v-if="project.origin !== 'remote' && project.status !== 'local_only'" :project-id="project.projectId" />
        </article>
      </section>
    </template>
    <div v-else-if="cloud.authenticated" class="cloud-sync-card__actions">
      <button class="nf-button nf-button--secondary" type="button" :disabled="cloud.busy" @click="logout">{{ t('Выйти из аккаунта') }}</button>
    </div>

    <ul v-if="cloud.blockedEvents.length" class="cloud-sync-card__blockers" aria-live="polite">
      <li v-for="blocker in cloud.blockedEventCodes" :key="blocker"><FriendlyStatus :code="blocker" subsystem="sync" /></li>
    </ul>
    <p v-if="cloud.blockedEvents.length" class="cloud-sync-card__warning">{{ t('Обмен изменениями приостановлен до устранения этих причин. Варианты не выбираются автоматически.') }}</p>
  </section>
</template>

<style scoped>
.cloud-sync-card { display: grid; gap: var(--nf-space-3); }
.cloud-sync-card__status, .cloud-sync-card__warning { display: flex; gap: var(--nf-space-2); align-items: flex-start; }
.cloud-sync-card__status p, .cloud-sync-card__warning, .cloud-sync-card__account, .cloud-sync-card__recovery p, .cloud-sync-card__scope-note p, .cloud-project p { margin: 0; }
.cloud-sync-card__status { color: var(--nf-color-text-muted); }
.cloud-sync-card__status svg, .cloud-sync-card__warning svg { flex: 0 0 auto; margin-top: .15rem; }
.cloud-sync-card__warning { color: var(--nf-color-warning, #9a6500); line-height: 1.45; }
.cloud-sync-card__form, .cloud-sync-card__inline-form { display: grid; gap: var(--nf-space-3); max-width: 34rem; }
.cloud-sync-card__form label, .cloud-sync-card__inline-form label { display: grid; gap: var(--nf-space-1); color: var(--nf-color-text-muted); }
.cloud-sync-card__form input, .cloud-sync-card__inline-form input { width: 100%; }
.cloud-sync-card__actions { display: flex; flex-wrap: wrap; gap: var(--nf-space-2); align-items: center; }
.cloud-sync-card__error { margin: 0; color: var(--nf-color-danger, #b42318); }
.cloud-sync-card__recovery { display: grid; gap: var(--nf-space-3); }
.cloud-sync-card__recovery-key { overflow-wrap: anywhere; padding: var(--nf-space-3); border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-2); background: var(--nf-color-canvas-muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.cloud-sync-card__scope-note { display: grid; gap: var(--nf-space-2); padding: var(--nf-space-3); border-left: 3px solid var(--nf-color-warning, #9a6500); background: var(--nf-color-canvas-muted); }
.cloud-sync-card__projects { display: grid; gap: var(--nf-space-3); }
.cloud-sync-card__projects h3 { margin: 0; }
.cloud-project { display: grid; gap: var(--nf-space-2); padding: var(--nf-space-3); border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-2); }
.cloud-project > div:first-child { display: grid; gap: var(--nf-space-1); }
.cloud-project small { overflow-wrap: anywhere; color: var(--nf-color-text-muted); }
.cloud-project__available { color: var(--nf-color-text-muted); }
.cloud-sync-card__blockers { margin: 0; padding-left: 1.25rem; color: var(--nf-color-danger, #b42318); }
</style>
