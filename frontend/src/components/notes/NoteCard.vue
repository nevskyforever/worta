<script setup lang="ts">
import { computed, ref } from 'vue'
import { IonIcon } from '@ionic/vue'
import {
  archiveOutline,
  arrowDownOutline,
  arrowUpOutline,
  createOutline,
  colorPaletteOutline,
  locateOutline,
  pinOutline,
  trashOutline,
} from 'ionicons/icons'

import { useLocaleStore } from '@/stores/locale'
import { NOTE_COLORS, type NoteColor } from './noteColors'
import type { ProjectNote } from '@/types/notes'

const props = withDefaults(
  defineProps<{
    note: ProjectNote
    disabled?: boolean
    canMoveUp?: boolean
    canMoveDown?: boolean
    reading?: boolean
  }>(),
  {
    disabled: false,
    canMoveUp: false,
    canMoveDown: false,
  },
)

const emit = defineEmits<{
  edit: [note: ProjectNote]
  togglePin: [note: ProjectNote]
  toggleArchive: [note: ProjectNote]
  delete: [note: ProjectNote]
  moveUp: [note: ProjectNote]
  moveDown: [note: ProjectNote]
  openMap: [note: ProjectNote]
  setColor: [note: ProjectNote, color: NoteColor]
}>()

const locale = useLocaleStore()
const t = locale.translate
const colorPaletteOpen = ref(false)

const preview = computed(() => {
  if (props.note.source_type === 'mindmap' || props.note.content_format === 'plain') {
    return props.note.content.trim()
  }
  const container = document.createElement('div')
  container.innerHTML = props.note.content
  return (container.textContent ?? '').trim()
})

const title = computed(
  () => props.note.display_title.trim() || props.note.title.trim() || t('Без названия'),
)

function selectColor(color: NoteColor): void {
  colorPaletteOpen.value = false
  if (color !== props.note.color) emit('setColor', props.note, color)
}
</script>

<template>
  <article
    class="note-card"
    :class="{ 'note-card--archived': note.archived, 'note-card--pinned': note.pinned }"
    :data-color="note.color"
  >
    <header class="note-card__header">
      <div class="note-card__title">
        <span v-if="note.pinned" class="note-card__pin-label">
          <IonIcon :icon="pinOutline" aria-hidden="true" />
          {{ t('Закреплено') }}
        </span>
        <span v-if="note.stage_name" class="note-card__stage">{{ note.stage_name }}</span>
        <h2>{{ title }}</h2>
      </div>

      <button
        v-if="!reading"
        class="icon-button"
        type="button"
        :aria-label="t('Редактировать заметку')"
        :disabled="disabled || note.read_only"
        @click="emit('edit', note)"
      >
        <IonIcon :icon="createOutline" aria-hidden="true" />
      </button>
    </header>

    <slot>
      <p v-if="preview" class="note-card__preview">{{ preview }}</p>
      <p v-else class="note-card__empty">{{ t('В заметке пока нет текста.') }}</p>
    </slot>

    <ul v-if="note.checklist.length" class="note-card__checklist" :aria-label="t('Чек-лист')">
      <li v-for="item in reading ? note.checklist : note.checklist.slice(0, 4)" :key="item.id">
        <span aria-hidden="true">{{ item.checked ? '✓' : '○' }}</span>
        <span :class="{ 'is-checked': item.checked }">{{ item.text }}</span>
      </li>
    </ul>

    <div v-if="note.tags.length || note.system_tags.length" class="note-card__tags">
      <span v-for="tag in note.system_tags" :key="`system-${tag}`">#{{ tag }}</span>
      <span v-for="tag in note.tags" :key="tag">#{{ tag }}</span>
    </div>

    <footer class="note-card__footer">
      <time :datetime="note.updated_at">
        {{ t('Изменено') }} {{ locale.formatDate(note.updated_at) }}
      </time>

      <div v-if="!reading" class="note-card__actions">
        <div class="note-card__color-control">
          <button
            class="icon-button note-card__color-toggle"
            type="button"
            :aria-label="t('Цвет заметки')"
            :aria-expanded="colorPaletteOpen"
            :disabled="disabled || note.read_only"
            @click="colorPaletteOpen = !colorPaletteOpen"
          >
            <IonIcon :icon="colorPaletteOutline" aria-hidden="true" />
            <span class="note-card__color-current" :data-color="note.color" aria-hidden="true" />
          </button>
          <div v-if="colorPaletteOpen" class="note-card__palette" role="group" :aria-label="t('Цвет заметки')">
            <button
              v-for="color in NOTE_COLORS"
              :key="color.value"
              class="note-card__swatch"
              type="button"
              :class="{ 'is-selected': note.color === color.value }"
              :data-color="color.value"
              :aria-label="t(color.label)"
              :aria-pressed="note.color === color.value"
              @click="selectColor(color.value)"
            >
              <span aria-hidden="true">✓</span>
            </button>
          </div>
        </div>
        <button
          v-if="note.source_type === 'mindmap'"
          class="icon-button"
          type="button"
          :aria-label="t('Показать заметку на карте')"
          @click="emit('openMap', note)"
        >
          <IonIcon :icon="locateOutline" aria-hidden="true" />
        </button>
        <button
          class="icon-button"
          type="button"
          :aria-label="note.pinned ? t('Открепить заметку') : t('Закрепить заметку')"
          :disabled="disabled || note.read_only"
          @click="emit('togglePin', note)"
        >
          <IonIcon :icon="pinOutline" aria-hidden="true" />
        </button>
        <button
          class="icon-button"
          type="button"
          :aria-label="t('Переместить заметку выше')"
          :disabled="disabled || note.read_only || !canMoveUp"
          @click="emit('moveUp', note)"
        >
          <IonIcon :icon="arrowUpOutline" aria-hidden="true" />
        </button>
        <button
          class="icon-button"
          type="button"
          :aria-label="t('Переместить заметку ниже')"
          :disabled="disabled || note.read_only || !canMoveDown"
          @click="emit('moveDown', note)"
        >
          <IonIcon :icon="arrowDownOutline" aria-hidden="true" />
        </button>
        <button
          class="icon-button"
          type="button"
          :aria-label="note.archived ? t('Вернуть заметку из архива') : t('Архивировать заметку')"
          :disabled="disabled || note.read_only"
          @click="emit('toggleArchive', note)"
        >
          <IonIcon :icon="archiveOutline" aria-hidden="true" />
        </button>
        <button
          class="icon-button icon-button--danger"
          type="button"
          :aria-label="t('Удалить заметку')"
          :disabled="disabled || note.read_only"
          @click="emit('delete', note)"
        >
          <IonIcon :icon="trashOutline" aria-hidden="true" />
        </button>
      </div>
    </footer>
  </article>
</template>

<style scoped>
.note-card {
  --note-paper: var(--nf-color-surface);
  position: relative;
  display: grid;
  gap: var(--nf-space-4);
  min-width: 0;
  min-height: 18rem;
  overflow: hidden;
  padding: var(--nf-space-5);
  border: 1px solid color-mix(in srgb, var(--nf-color-border) 85%, #0000);
  border-radius: var(--nf-radius-sm);
  background: var(--note-paper);
  box-shadow: var(--nf-shadow-card);
  transition: transform 160ms ease, box-shadow 160ms ease;
}

.note-card:hover { transform: translateY(-2px); box-shadow: 0 10px 24px rgb(0 0 0 / 24%); }

.note-card[data-color='coral'] { --note-paper: #f6c5bd; }
.note-card[data-color='orange'] { --note-paper: #f7d2a6; }
.note-card[data-color='yellow'] { --note-paper: #f7e6a4; }
.note-card[data-color='green'] { --note-paper: #cde7c6; }
.note-card[data-color='teal'] { --note-paper: #bde6df; }
.note-card[data-color='blue'] { --note-paper: #c6ddf2; }
.note-card[data-color='purple'] { --note-paper: #dccdf0; }
.note-card[data-color='pink'] { --note-paper: #efcadc; }
.note-card[data-color='brown'] { --note-paper: #dccdbf; }
.note-card[data-color='gray'] { --note-paper: #d7d9dd; }
.note-card:not([data-color='default']) { color: #202124; }

.note-card--archived {
  opacity: 0.78;
}

.note-card__header,
.note-card__footer {
  display: flex;
  gap: var(--nf-space-3);
  align-items: flex-start;
  justify-content: space-between;
}

.note-card__title {
  min-width: 0;
}

.note-card h2 {
  margin: var(--nf-space-1) 0 0;
  color: var(--nf-color-text);
  font-family: var(--nf-font-serif);
  font-size: 1.35rem;
  overflow-wrap: anywhere;
  white-space: pre-line;
}

.note-card__pin-label,
.note-card__stage {
  display: inline-flex;
  gap: var(--nf-space-1);
  align-items: center;
  margin-right: var(--nf-space-2);
  color: var(--nf-color-primary);
  font-size: 0.72rem;
  font-weight: 800;
  text-transform: uppercase;
}

.note-card__preview,
.note-card__empty {
  display: -webkit-box;
  margin: 0;
  overflow: hidden;
  color: var(--nf-color-text-muted);
  line-height: 1.55;
  overflow-wrap: anywhere;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 6;
}

.note-card__empty {
  font-style: italic;
}

.note-card__checklist {
  display: grid;
  gap: var(--nf-space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.note-card__checklist li {
  display: flex;
  gap: var(--nf-space-2);
  align-items: flex-start;
}

.is-checked {
  text-decoration: line-through;
  opacity: 0.7;
}

.note-card__tags {
  display: flex;
  flex-wrap: wrap;
  gap: var(--nf-space-2);
}

.note-card__tags span {
  padding: 0.2rem 0.55rem;
  border-radius: var(--nf-radius-pill);
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
  font-size: 0.75rem;
  font-weight: 700;
}

.note-card__footer {
  align-items: center;
  margin-top: auto;
  padding-top: var(--nf-space-3);
  border-top: 1px solid var(--nf-color-border);
}

.note-card__footer time {
  color: var(--nf-color-text-muted);
  font-size: 0.72rem;
}

.note-card__actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.note-card__color-control { position: relative; }
.note-card__color-toggle { position: relative; }
.note-card__color-current { position: absolute; right: 0.35rem; bottom: 0.35rem; width: 0.45rem; height: 0.45rem; border: 1px solid rgb(32 33 36 / 48%); border-radius: 50%; background: var(--nf-color-surface); }
.note-card__color-current[data-color='default'], .note-card__swatch[data-color='default'] { --note-color: var(--nf-color-surface); }
.note-card__color-current[data-color='coral'], .note-card__swatch[data-color='coral'] { --note-color: #f6c5bd; }
.note-card__color-current[data-color='orange'], .note-card__swatch[data-color='orange'] { --note-color: #f7d2a6; }
.note-card__color-current[data-color='yellow'], .note-card__swatch[data-color='yellow'] { --note-color: #f7e6a4; }
.note-card__color-current[data-color='green'], .note-card__swatch[data-color='green'] { --note-color: #cde7c6; }
.note-card__color-current[data-color='teal'], .note-card__swatch[data-color='teal'] { --note-color: #bde6df; }
.note-card__color-current[data-color='blue'], .note-card__swatch[data-color='blue'] { --note-color: #c6ddf2; }
.note-card__color-current[data-color='purple'], .note-card__swatch[data-color='purple'] { --note-color: #dccdf0; }
.note-card__color-current[data-color='pink'], .note-card__swatch[data-color='pink'] { --note-color: #efcadc; }
.note-card__color-current[data-color='brown'], .note-card__swatch[data-color='brown'] { --note-color: #dccdbf; }
.note-card__color-current[data-color='gray'], .note-card__swatch[data-color='gray'] { --note-color: #d7d9dd; }
.note-card__color-current, .note-card__swatch { background: var(--note-color); }
.note-card__palette { position: absolute; z-index: 2; right: 0; bottom: calc(100% + var(--nf-space-2)); display: grid; grid-template-columns: repeat(6, 2rem); gap: 0.4rem; padding: var(--nf-space-3); border: 1px solid rgb(50 50 50 / 12%); border-radius: var(--nf-radius-sm); background: color-mix(in srgb, var(--nf-color-surface) 94%, transparent); box-shadow: var(--nf-shadow-card); }
.note-card__swatch { display: grid; width: 2rem; height: 2rem; padding: 0; place-items: center; border: 1px solid rgb(32 33 36 / 48%); border-radius: 50%; color: #202124; cursor: pointer; }
.note-card__swatch span { opacity: 0; font-weight: 800; }
.note-card__swatch.is-selected { box-shadow: 0 0 0 2px var(--nf-color-focus); }
.note-card__swatch.is-selected span { opacity: 1; }

.icon-button {
  display: inline-grid;
  width: 2.75rem;
  min-width: 2.75rem;
  height: 2.75rem;
  padding: 0;
  place-items: center;
  border: 0;
  border-radius: var(--nf-radius-sm);
  background: transparent;
  color: var(--nf-color-text-muted);
  cursor: pointer;
}

.icon-button:hover:not(:disabled) {
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
}

.icon-button--danger:hover:not(:disabled) {
  background: color-mix(in srgb, var(--nf-color-danger) 12%, transparent);
  color: var(--nf-color-danger);
}

.icon-button:disabled {
  opacity: 0.35;
  cursor: not-allowed;
}

@media (max-width: 39.99rem) {
  .note-card__footer {
    align-items: stretch;
    flex-direction: column;
  }

  .note-card__actions {
    justify-content: flex-start;
  }
}
</style>
