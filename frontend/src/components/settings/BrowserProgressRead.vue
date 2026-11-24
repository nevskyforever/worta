<script setup lang="ts">
import { computed, ref } from 'vue'
import { IonIcon } from '@ionic/vue'
import { trashOutline } from 'ionicons/icons'
import ProgressBar from '@/components/ui/ProgressBar.vue'
import type { BrowserProgressView } from '@/platform/web/progressView'
import { useLocaleStore } from '@/stores/locale'
const props = defineProps<{ value: BrowserProgressView; writable?: boolean; busy?: boolean }>()
const emit = defineEmits<{ save: [action: { total: number } | { remove: string }] }>()
const locale = useLocaleStore(), t = locale.translate
const metrics = computed(() => props.value.statistics.metrics)
const newTotal = ref<number | ''>('')
function save() {
  if (!props.writable || props.busy || typeof newTotal.value !== 'number' || !Number.isFinite(newTotal.value) || newTotal.value < 0) return
  emit('save', { total: newTotal.value })
}
function remove(entryId: string) {
  if (props.writable && !props.busy && window.confirm(t('Удалить эту запись прогресса? Итоговое значение будет пересчитано.')))
    emit('save', { remove: entryId })
}
</script>
<template>
  <section class="cloud-progress" data-testid="cloud-progress-read">
    <h4>{{ t('Запись прогресса') }}</h4>
    <div class="cloud-progress__entry">
      <dl class="cloud-progress__summary">
        <div><dt>{{ t('Текущее значение') }}</dt><dd data-testid="cloud-progress-total">{{ locale.formatNumber(value.total, 2) }}</dd></div>
        <div><dt>{{ t('Цель') }}</dt><dd data-testid="cloud-progress-goal">{{ value.goal === null ? t('Без лимита') : locale.formatNumber(value.goal, 2) }}</dd></div>
        <div v-if="value.goal !== null"><dt>{{ t('Прогресс') }}</dt><dd data-testid="cloud-progress-percent">{{ locale.formatNumber(value.progress, 2) }}%</dd></div>
        <div v-if="value.remaining !== null"><dt>{{ t('Осталось') }}</dt><dd data-testid="cloud-progress-remaining">{{ locale.formatNumber(value.remaining, 2) }}</dd></div>
      </dl>
      <ProgressBar v-if="value.goal !== null" :value="value.progress" :label="t('Прогресс')" />
      <form v-if="writable" @submit.prevent="save">
        <label>{{ t('Новое общее значение') }} <input v-model.number="newTotal" type="number" min="0" step="any" required :disabled="busy" /></label>
        <button class="nf-button" type="submit" :disabled="busy">{{ busy ? t('Сохраняем…') : t('Записать') }}</button>
      </form>
    </div>
    <div class="cloud-progress__heading"><h5>{{ t('История') }}</h5><span>{{ t('Записей') }} · {{ metrics.entries_count }}</span></div>
    <div class="cloud-progress__table">
      <table data-testid="cloud-progress-history"><thead><tr><th scope="col">{{ t('Дата') }}</th><th scope="col">{{ t('Изменение') }}</th><th scope="col">{{ t('Итого') }}</th><th v-if="writable" scope="col"><span class="visually-hidden">{{ t('Удалить') }}</span></th></tr></thead>
        <tbody><tr v-for="entry in [...value.entries].reverse()" :key="entry.entry_id" :data-entry-id="entry.entry_id">
          <td><time :datetime="entry.writing_day">{{ locale.formatDate(entry.writing_day) }}</time></td>
          <td :class="{ 'cloud-progress__gain': entry.added > 0 }">{{ entry.added > 0 ? '+' : '' }}{{ locale.formatNumber(entry.added, 2) }}</td>
          <td>{{ locale.formatNumber(entry.total, 2) }}</td>
          <td v-if="writable"><button class="nf-button nf-button--quiet cloud-progress__remove" type="button" :aria-label="t('Удалить')" :disabled="busy" @click="remove(entry.entry_id)"><IonIcon :icon="trashOutline" aria-hidden="true" /></button></td>
        </tr></tbody>
      </table>
    </div>
    <h4>{{ t('Статистика') }}</h4>
    <dl class="cloud-progress__summary cloud-progress__entry" data-testid="cloud-progress-statistics">
      <div><dt>{{ t('Записей') }}</dt><dd data-testid="cloud-progress-count">{{ metrics.entries_count }}</dd></div>
      <div><dt>{{ t('Активных дней') }}</dt><dd data-testid="cloud-progress-active-days">{{ metrics.active_days }}</dd></div>
      <div><dt>{{ t('Среднее символов в записи') }}</dt><dd>{{ metrics.average_symbols_per_entry }}</dd></div>
      <div><dt>{{ t('Среднее символов за активный день') }}</dt><dd>{{ metrics.average_symbols_per_active_day }}</dd></div>
      <div><dt>{{ t('Среднее записей за активный день') }}</dt><dd>{{ metrics.average_entries_per_active_day }}</dd></div>
    </dl>
    <div v-if="value.statistics.timeline.length" class="cloud-progress__table">
      <table data-testid="cloud-progress-timeline"><tbody><tr v-for="day in value.statistics.timeline" :key="day.date"><td>{{ locale.formatDate(day.date) }}</td><td>{{ locale.formatNumber(day.value, 2) }}</td></tr></tbody></table>
    </div>
  </section>
</template>

<style scoped>
.cloud-progress { display: grid; gap: var(--nf-space-4); min-width: 0; margin-block: var(--nf-space-5); }
.cloud-progress h4 { font-size: clamp(1.4rem, 4vw, 2.15rem); font-weight: 650; }
.cloud-progress h4, .cloud-progress h5, .cloud-progress dl, .cloud-progress dd { margin: 0; }
.cloud-progress__entry, .cloud-progress__table { border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-md); background: var(--nf-color-surface); }
.cloud-progress__entry { display: grid; gap: var(--nf-space-4); padding: var(--nf-space-4); }
.cloud-progress__summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 10rem), 1fr)); gap: var(--nf-space-4); }
.cloud-progress__summary dt, .cloud-progress__heading span, .cloud-progress thead { color: var(--nf-color-text-muted); font-size: .8rem; }
.cloud-progress__summary dd { font-size: 1.15rem; font-weight: 700; }
.cloud-progress form { display: flex; flex-wrap: wrap; gap: var(--nf-space-3); align-items: end; }
.cloud-progress form label { display: grid; gap: var(--nf-space-1); max-width: 100%; }
.cloud-progress__heading { display: flex; justify-content: space-between; gap: var(--nf-space-3); align-items: center; }
.cloud-progress__table { overflow: auto; }
.cloud-progress table { width: 100%; border-collapse: collapse; text-align: start; }
.cloud-progress td, .cloud-progress th { border-bottom: 1px solid var(--nf-color-border); padding: var(--nf-space-3); text-align: start; }
.cloud-progress tbody tr:last-child td { border-bottom: 0; }
.cloud-progress__gain { color: var(--nf-color-success); font-weight: 650; }
.cloud-progress__remove { color: var(--ion-color-danger); }
</style>
