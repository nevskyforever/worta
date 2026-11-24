<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { IonContent, IonHeader, IonIcon, IonModal, IonSpinner } from '@ionic/vue'
import {
  checkmarkDoneOutline,
  closeOutline,
  notificationsOutline,
} from 'ionicons/icons'

import { apiErrorMessage } from '@/api/client'
import { gameApi } from '@/api/game'
import { useLocaleStore } from '@/stores/locale'
import { useNotificationsStore } from '@/stores/notifications'
import type { GameNotification } from '@/types/game'
import { gameHistoryMessage } from '@/utils/gameNotifications'

const locale = useLocaleStore()
const t = locale.translate
const notifications = useNotificationsStore()
const history = computed(() => notifications.gameHistory)
const open = ref(false)
const loading = ref(false)
const updating = ref(false)
const apiError = ref<string | null>(null)
let activeRequest: AbortController | null = null

const hasUnread = computed(() => history.value.unread_count > 0)

function formatCreatedAt(value: string | null): string | null {
  if (!value) return null
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return value
  return new Intl.DateTimeFormat(locale.localeTag, {
    day: 'numeric',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
  }).format(date)
}

async function refresh(showErrors = false): Promise<void> {
  activeRequest?.abort()
  const controller = new AbortController()
  activeRequest = controller
  loading.value = true
  if (showErrors) apiError.value = null
  try {
    const snapshot = await gameApi.notifications(controller.signal)
    if (!controller.signal.aborted) notifications.setGameHistory(snapshot)
  } catch (error) {
    if (controller.signal.aborted) return
    if (showErrors) apiError.value = t(apiErrorMessage(error))
  } finally {
    if (activeRequest === controller) {
      activeRequest = null
      loading.value = false
    }
  }
}

async function openCenter(): Promise<void> {
  open.value = true
  await refresh(true)
}

function closeCenter(): void {
  if (!updating.value) open.value = false
}

function handleDismiss(): void {
  open.value = false
}

async function markRead(notification: GameNotification): Promise<void> {
  if (updating.value) return
  activeRequest?.abort()
  updating.value = true
  apiError.value = null
  try {
    notifications.setGameHistory(await gameApi.markNotificationRead(notification.id))
  } catch (error) {
    apiError.value = t(apiErrorMessage(error))
  } finally {
    updating.value = false
  }
}

async function markAllRead(): Promise<void> {
  if (updating.value || !hasUnread.value) return
  activeRequest?.abort()
  updating.value = true
  apiError.value = null
  try {
    notifications.setGameHistory(await gameApi.markAllNotificationsRead())
  } catch (error) {
    apiError.value = t(apiErrorMessage(error))
  } finally {
    updating.value = false
  }
}

onMounted(() => {
  void refresh()
})

onBeforeUnmount(() => activeRequest?.abort())
</script>

<template>
  <button
    class="notification-center__trigger"
    type="button"
    data-testid="notification-trigger"
    :aria-label="t('Уведомления')"
    aria-controls="notification-center-title"
    :aria-expanded="open"
    aria-haspopup="dialog"
    @click="openCenter"
  >
    <IonIcon :icon="notificationsOutline" aria-hidden="true" />
    <span v-if="hasUnread" class="notification-center__count" aria-hidden="true">
      {{ history.unread_count > 99 ? '99+' : history.unread_count }}
    </span>
    <span class="visually-hidden">
      {{ t('Непрочитанные уведомления') }}: {{ history.unread_count }}
    </span>
  </button>

  <IonModal
    :is-open="open"
    css-class="notification-center-modal"
    :backdrop-dismiss="!updating"
    :keyboard-close="!updating"
    @did-dismiss="handleDismiss"
  >
    <IonHeader class="notification-center__header ion-no-border">
      <div>
        <p>{{ t('История') }}</p>
        <h2 id="notification-center-title">{{ t('Уведомления') }}</h2>
      </div>
      <button
        class="notification-center__icon-button"
        type="button"
        :aria-label="t('Закрыть')"
        :disabled="updating"
        @click="closeCenter"
      >
        <IonIcon :icon="closeOutline" aria-hidden="true" />
      </button>
    </IonHeader>

    <IonContent class="notification-center__content">
      <div class="notification-center__body">
        <div v-if="apiError" class="notification-center__error" role="alert">
          {{ apiError }}
        </div>

        <div v-if="loading" class="notification-center__loading" role="status">
          <IonSpinner aria-hidden="true" />
          <span>{{ t('Загружаем уведомления…') }}</span>
        </div>

        <template v-else>
          <section aria-labelledby="unread-notifications-title">
            <div class="notification-center__section-heading">
              <div>
                <p>{{ t('Новые события') }}</p>
                <h3 id="unread-notifications-title">
                  {{ t('Непрочитанные') }}
                  <span v-if="hasUnread">({{ history.unread_count }})</span>
                </h3>
              </div>
              <button
                v-if="hasUnread"
                class="nf-button nf-button--secondary"
                type="button"
                data-testid="mark-all-notifications"
                :disabled="updating"
                @click="markAllRead"
              >
                <IonIcon :icon="checkmarkDoneOutline" aria-hidden="true" />
                {{ t('Прочитать все') }}
              </button>
            </div>

            <p v-if="!history.unread.length" class="notification-center__empty">
              {{ t('Новых уведомлений нет.') }}
            </p>
            <ol v-else class="notification-center__list">
              <li
                v-for="notification in history.unread"
                :key="notification.id"
                class="notification-center__item notification-center__item--unread"
              >
                <div>
                  <p>{{ gameHistoryMessage(notification, t, locale.formatNumber) }}</p>
                  <time
                    v-if="formatCreatedAt(notification.created_at)"
                    :datetime="notification.created_at ?? undefined"
                  >
                    {{ formatCreatedAt(notification.created_at) }}
                  </time>
                </div>
                <button
                  class="notification-center__icon-button"
                  type="button"
                  :aria-label="t('Отметить как прочитанное')"
                  :disabled="updating"
                  @click="markRead(notification)"
                >
                  <IonIcon :icon="checkmarkDoneOutline" aria-hidden="true" />
                </button>
              </li>
            </ol>
          </section>

          <section aria-labelledby="read-notifications-title">
            <div class="notification-center__section-heading">
              <div>
                <p>{{ t('Архив событий') }}</p>
                <h3 id="read-notifications-title">{{ t('Прочитанные') }}</h3>
              </div>
            </div>

            <p v-if="!history.read.length" class="notification-center__empty">
              {{ t('История уведомлений пока пуста.') }}
            </p>
            <ol v-else class="notification-center__list">
              <li
                v-for="notification in history.read"
                :key="notification.id"
                class="notification-center__item"
              >
                <div>
                  <p>{{ gameHistoryMessage(notification, t, locale.formatNumber) }}</p>
                  <time
                    v-if="formatCreatedAt(notification.created_at)"
                    :datetime="notification.created_at ?? undefined"
                  >
                    {{ formatCreatedAt(notification.created_at) }}
                  </time>
                </div>
              </li>
            </ol>
          </section>
        </template>
      </div>
    </IonContent>
  </IonModal>
</template>

<style>
.notification-center-modal {
  --width: min(38rem, calc(100vw - 2rem));
  --height: min(46rem, calc(100dvh - 2rem));
  --border-radius: var(--nf-radius-md);
}

.notification-center-modal::part(content) {
  border: 1px solid var(--nf-color-border);
  box-shadow: 0 28px 80px rgb(20 30 27 / 25%);
}

@media (max-width: 37.5rem) {
  .notification-center-modal {
    --width: 100%;
    --height: 100%;
    --border-radius: 0;
  }
}
</style>

<style scoped>
.notification-center__trigger {
  position: fixed;
  z-index: 31;
  top: max(var(--nf-space-4), env(safe-area-inset-top));
  right: max(var(--nf-space-4), env(safe-area-inset-right));
  display: grid;
  width: 2.9rem;
  height: 2.9rem;
  padding: 0;
  place-items: center;
  border: 1px solid var(--nf-color-border);
  border-radius: 999px;
  background: color-mix(in srgb, var(--nf-color-surface-raised) 92%, transparent);
  box-shadow: var(--nf-shadow-card);
  color: var(--nf-color-text);
  cursor: pointer;
}

.notification-center__trigger:hover,
.notification-center__trigger:focus-visible {
  border-color: var(--nf-color-primary);
  color: var(--nf-color-primary);
}

.notification-center__trigger ion-icon {
  font-size: 1.35rem;
}

.notification-center__count {
  position: absolute;
  top: -0.3rem;
  right: -0.2rem;
  min-width: 1.2rem;
  padding: 0.08rem 0.25rem;
  border: 2px solid var(--nf-color-canvas);
  border-radius: 999px;
  background: var(--nf-color-danger);
  color: #fff;
  font-size: 0.65rem;
  font-weight: 800;
  line-height: 1.2;
}

.notification-center__header {
  display: flex;
  gap: var(--nf-space-4);
  align-items: flex-start;
  justify-content: space-between;
  padding: var(--nf-space-5);
  border-bottom: 1px solid var(--nf-color-border);
  background: var(--nf-color-surface-raised);
}

.notification-center__header p,
.notification-center__header h2,
.notification-center__section-heading p,
.notification-center__section-heading h3,
.notification-center__item p,
.notification-center__item time {
  margin: 0;
}

.notification-center__header p,
.notification-center__section-heading p {
  color: var(--nf-color-primary);
  font-size: 0.75rem;
  font-weight: 750;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

.notification-center__header h2 {
  margin-top: var(--nf-space-1);
  font-family: var(--nf-font-serif);
  font-size: 1.55rem;
}

.notification-center__icon-button {
  display: inline-grid;
  width: 2.75rem;
  height: 2.75rem;
  flex: 0 0 auto;
  padding: 0;
  place-items: center;
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface);
  color: var(--nf-color-text-muted);
  cursor: pointer;
}

.notification-center__icon-button:hover:not(:disabled),
.notification-center__icon-button:focus-visible:not(:disabled) {
  border-color: var(--nf-color-primary);
  color: var(--nf-color-primary);
}

.notification-center__icon-button:disabled {
  cursor: wait;
  opacity: 0.6;
}

.notification-center__content {
  --background: var(--nf-color-surface);
}

.notification-center__body {
  display: grid;
  gap: var(--nf-space-6);
  padding: var(--nf-space-5);
}

.notification-center__error {
  padding: var(--nf-space-3);
  border: 1px solid color-mix(in srgb, var(--nf-color-danger) 48%, transparent);
  border-radius: var(--nf-radius-sm);
  background: color-mix(in srgb, var(--nf-color-danger) 10%, transparent);
  color: var(--nf-color-danger);
}

.notification-center__loading {
  display: flex;
  min-height: 12rem;
  gap: var(--nf-space-3);
  align-items: center;
  justify-content: center;
  color: var(--nf-color-text-muted);
}

.notification-center__section-heading {
  display: flex;
  gap: var(--nf-space-4);
  align-items: center;
  justify-content: space-between;
  margin-bottom: var(--nf-space-3);
}

.notification-center__section-heading h3 {
  margin-top: var(--nf-space-1);
  font-family: var(--nf-font-serif);
  font-size: 1.2rem;
}

.notification-center__section-heading .nf-button {
  min-height: 2.75rem;
  font-size: 0.85rem;
}

.notification-center__empty {
  margin: 0;
  padding: var(--nf-space-4);
  border: 1px dashed var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  color: var(--nf-color-text-muted);
  line-height: 1.45;
}

.notification-center__list {
  display: grid;
  gap: var(--nf-space-3);
  margin: 0;
  padding: 0;
  list-style: none;
}

.notification-center__item {
  display: flex;
  gap: var(--nf-space-3);
  align-items: flex-start;
  justify-content: space-between;
  padding: var(--nf-space-4);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
}

.notification-center__item--unread {
  border-left: 0.3rem solid var(--nf-color-primary);
}

.notification-center__item p {
  color: var(--nf-color-text);
  line-height: 1.45;
  white-space: pre-wrap;
}

.notification-center__item time {
  display: block;
  margin-top: var(--nf-space-2);
  color: var(--nf-color-text-muted);
  font-size: 0.8rem;
}

@media (max-width: 37.5rem) {
  .notification-center__trigger {
    top: max(var(--nf-space-3), env(safe-area-inset-top));
    right: max(var(--nf-space-3), env(safe-area-inset-right));
  }

  .notification-center__header,
  .notification-center__body {
    padding: var(--nf-space-4);
  }

  .notification-center__section-heading {
    align-items: flex-end;
  }
}
</style>
