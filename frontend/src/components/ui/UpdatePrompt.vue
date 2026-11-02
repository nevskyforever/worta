<script setup lang="ts">
import { IonIcon, IonSpinner } from '@ionic/vue'
import { cloudDownloadOutline, closeOutline } from 'ionicons/icons'

import { useLocaleStore } from '@/stores/locale'
import { useUpdaterStore } from '@/stores/updater'

const locale = useLocaleStore()
const updater = useUpdaterStore()
const t = locale.translate
</script>

<template>
  <aside
    v-if="updater.promptVisible"
    class="update-prompt"
    role="dialog"
    aria-labelledby="update-prompt-title"
    aria-describedby="update-prompt-description"
  >
    <IonIcon class="update-prompt__icon" :icon="cloudDownloadOutline" aria-hidden="true" />
    <div class="update-prompt__body">
      <strong id="update-prompt-title">{{ t('Доступно обновление nfprogress').replace(/nfprogress/gi, 'worta') }}</strong>
      <p id="update-prompt-description">
        <template v-if="updater.status === 'available'">
          {{ t('Новая версия {version} готова к установке.', { version: updater.availableVersion }) }}
        </template>
        <template v-else-if="updater.status === 'downloading'">
          {{ t('Загружаем обновление…') }}
          <span v-if="updater.progressPercent !== null">{{ updater.progressPercent }}%</span>
        </template>
        <template v-else-if="updater.status === 'installing'">
          {{ t('Проверяем подпись и подготавливаем установку…') }}
        </template>
        <template v-else>{{ t('Перезапускаем приложение…') }}</template>
      </p>
      <p v-if="updater.status === 'available' && updater.releaseNotes" class="update-prompt__notes">
        {{ updater.releaseNotes }}
      </p>
      <div
        v-if="updater.status === 'downloading'"
        class="update-prompt__progress"
        role="progressbar"
        :aria-label="t('Загрузка обновления')"
        :aria-valuenow="updater.progressPercent ?? undefined"
        aria-valuemin="0"
        aria-valuemax="100"
      >
        <span :style="{ width: `${updater.progressPercent ?? 8}%` }" />
      </div>
      <div v-if="updater.status === 'available'" class="update-prompt__actions">
        <button class="nf-button nf-button--secondary" type="button" @click="updater.dismissPrompt">
          {{ t('Позже') }}
        </button>
        <button class="nf-button" type="button" @click="updater.installUpdate">
          {{ t('Установить и перезапустить') }}
        </button>
      </div>
      <div v-else class="update-prompt__working" role="status" aria-live="polite">
        <IonSpinner name="crescent" aria-hidden="true" />
      </div>
    </div>
    <button
      v-if="updater.status === 'available'"
      class="update-prompt__close"
      type="button"
      :aria-label="t('Отложить обновление')"
      @click="updater.dismissPrompt"
    >
      <IonIcon :icon="closeOutline" aria-hidden="true" />
    </button>
  </aside>
</template>

<style scoped>
.update-prompt {
  position: fixed;
  z-index: 10020;
  top: max(var(--nf-space-4), env(safe-area-inset-top));
  left: 50%;
  display: grid;
  width: min(38rem, calc(100vw - 2 * var(--nf-space-4)));
  grid-template-columns: auto minmax(0, 1fr) auto;
  gap: var(--nf-space-3);
  align-items: start;
  padding: var(--nf-space-4);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-surface-raised);
  box-shadow: var(--nf-shadow-card);
  color: var(--nf-color-text);
  transform: translateX(-50%);
}

.update-prompt__icon {
  padding: var(--nf-space-2);
  border-radius: 50%;
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
  font-size: 1.4rem;
}

.update-prompt__body {
  min-width: 0;
}

.update-prompt strong,
.update-prompt p {
  margin: 0;
}

.update-prompt p {
  margin-top: var(--nf-space-1);
  color: var(--nf-color-text-muted);
  font-size: 0.88rem;
  line-height: 1.45;
}

.update-prompt__notes {
  display: -webkit-box;
  overflow: hidden;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 3;
  white-space: pre-line;
}

.update-prompt__actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--nf-space-2);
  margin-top: var(--nf-space-3);
}

.update-prompt__progress {
  height: 0.35rem;
  overflow: hidden;
  margin-top: var(--nf-space-3);
  border-radius: 999px;
  background: var(--nf-color-surface-muted);
}

.update-prompt__progress span {
  display: block;
  min-width: 8%;
  height: 100%;
  border-radius: inherit;
  background: var(--nf-color-primary);
  transition: width 180ms ease-out;
}

.update-prompt__working {
  display: flex;
  margin-top: var(--nf-space-3);
  color: var(--nf-color-primary);
}

.update-prompt__close {
  display: grid;
  width: 2.25rem;
  height: 2.25rem;
  padding: 0;
  place-items: center;
  border: 0;
  border-radius: var(--nf-radius-sm);
  background: transparent;
  color: var(--nf-color-text-muted);
  cursor: pointer;
}

.update-prompt__close:hover,
.update-prompt__close:focus-visible {
  background: var(--nf-color-surface-muted);
  color: var(--nf-color-text);
}

@media (max-width: 34rem) {
  .update-prompt {
    top: max(var(--nf-space-2), env(safe-area-inset-top));
    width: calc(100vw - 2 * var(--nf-space-2));
  }

  .update-prompt__actions .nf-button {
    flex: 1 1 auto;
  }
}
</style>
