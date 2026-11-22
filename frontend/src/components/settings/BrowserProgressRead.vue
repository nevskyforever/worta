<script setup lang="ts">
import { computed, ref } from 'vue'
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
  <section data-testid="cloud-progress-read">
    <h4>{{ t('Прогресс') }}</h4>
    <dl>
      <dt>{{ t('Всего') }}</dt><dd data-testid="cloud-progress-total">{{ locale.formatNumber(value.total, 2) }}</dd>
      <dt>{{ t('Цель') }}</dt><dd data-testid="cloud-progress-goal">{{ value.goal === null ? t('Без лимита') : locale.formatNumber(value.goal, 2) }}</dd>
      <dt>{{ t('Прогресс') }}</dt><dd data-testid="cloud-progress-percent">{{ locale.formatNumber(value.progress, 2) }}%</dd>
      <dt>{{ t('Осталось') }}</dt><dd data-testid="cloud-progress-remaining">{{ value.remaining === null ? '—' : locale.formatNumber(value.remaining, 2) }}</dd>
    </dl>
    <form v-if="writable" @submit.prevent="save">
      <label>{{ t('Новое общее значение') }} <input v-model.number="newTotal" type="number" min="0" step="any" required :disabled="busy" /></label>
      <button class="nf-button" type="submit" :disabled="busy">{{ busy ? t('Сохраняем…') : t('Записать') }}</button>
    </form>
    <h4>{{ t('История') }}</h4>
    <table data-testid="cloud-progress-history"><thead><tr><th>{{ t('Дата') }}</th><th>{{ t('Всего') }}</th><th>{{ t('Добавлено') }}</th></tr></thead>
      <tbody><tr v-for="entry in [...value.entries].reverse()" :key="entry.entry_id" :data-entry-id="entry.entry_id">
        <td>{{ entry.writing_day }}<br><time :datetime="entry.occurred_at ?? entry.writing_time ?? undefined">{{ entry.occurred_at ?? entry.writing_time }}</time></td><td>{{ locale.formatNumber(entry.total, 2) }}</td><td>{{ locale.formatNumber(entry.added, 2) }}</td>
        <td v-if="writable"><button class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="remove(entry.entry_id)">{{ t('Удалить') }}</button></td>
      </tr></tbody>
    </table>
    <h4>{{ t('Статистика') }}</h4>
    <dl data-testid="cloud-progress-statistics">
      <dt>{{ t('Записей') }}</dt><dd data-testid="cloud-progress-count">{{ metrics.entries_count }}</dd>
      <dt>{{ t('Активных дней') }}</dt><dd data-testid="cloud-progress-active-days">{{ metrics.active_days }}</dd>
      <dt>{{ t('Среднее символов в записи') }}</dt><dd>{{ metrics.average_symbols_per_entry }}</dd>
      <dt>{{ t('Среднее символов за активный день') }}</dt><dd>{{ metrics.average_symbols_per_active_day }}</dd>
      <dt>{{ t('Среднее записей за активный день') }}</dt><dd>{{ metrics.average_entries_per_active_day }}</dd>
    </dl>
    <table data-testid="cloud-progress-timeline"><tbody><tr v-for="day in value.statistics.timeline" :key="day.date"><td>{{ day.date }}</td><td>{{ locale.formatNumber(day.value, 2) }}</td></tr></tbody></table>
  </section>
</template>
