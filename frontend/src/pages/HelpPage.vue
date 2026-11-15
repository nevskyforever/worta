<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { IonContent, IonIcon, IonPage } from '@ionic/vue'
import {
  alertCircleOutline,
  bookOutline,
  closeCircleOutline,
  searchOutline,
} from 'ionicons/icons'

import { apiErrorMessage } from '@/api/client'
import { contentApi } from '@/api/content'
import HelpTree from '@/components/help/HelpTree.vue'
import StatePanel from '@/components/ui/StatePanel.vue'
import { openExternalUrl } from '@/platform/runtime'
import { useLocaleStore } from '@/stores/locale'
import type { HelpSection } from '@/types/content'

const locale = useLocaleStore()
const t = locale.translate
const sections = ref<HelpSection[]>([])
const selectedKey = ref<string | null>(null)
const query = ref('')
const searchInput = ref<HTMLInputElement | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)
let controller: AbortController | null = null

function flatten(items: HelpSection[]): HelpSection[] {
  return items.flatMap((item) => [item, ...flatten(item.children)])
}

function searchableText(section: HelpSection): string {
  const document = new DOMParser().parseFromString(section.content, 'text/html')
  return `${section.title} ${document.body.textContent ?? ''}`
}

const allSections = computed(() => flatten(sections.value))
const searchResults = computed(() => {
  const normalized = query.value.trim().toLocaleLowerCase(locale.localeTag)
  if (!normalized) return []
  return allSections.value.filter((section) =>
    searchableText(section).toLocaleLowerCase(locale.localeTag).includes(normalized),
  )
})
const selected = computed(
  () => allSections.value.find(({ key }) => key === selectedKey.value) ?? null,
)
const articleHtml = computed(() => {
  if (!selected.value) return ''
  const document = new DOMParser().parseFromString(selected.value.content, 'text/html')
  return document.body.innerHTML
})

function selectSection(section: HelpSection): void {
  selectedKey.value = section.key
}

async function loadHelp(): Promise<void> {
  controller?.abort()
  const requestController = new AbortController()
  controller = requestController
  loading.value = true
  error.value = null
  try {
    const result = await contentApi.help(locale.language, requestController.signal)
    if (controller !== requestController) return
    sections.value = result
    const keys = new Set(flatten(result).map(({ key }) => key))
    if (!selectedKey.value || !keys.has(selectedKey.value)) {
      selectedKey.value = result[0]?.key ?? null
    }
  } catch (loadError) {
    if (loadError instanceof DOMException && loadError.name === 'AbortError') return
    sections.value = []
    if (controller === requestController) error.value = t(apiErrorMessage(loadError))
  } finally {
    if (controller === requestController) loading.value = false
  }
}

function handleArticleClick(event: MouseEvent): void {
  const target = event.target
  if (!(target instanceof Element)) return
  const link = target.closest<HTMLAnchorElement>('a[href]')
  if (!link) return
  const url = new URL(link.href, window.location.href)
  if (!['http:', 'https:'].includes(url.protocol)) return
  event.preventDefault()
  void openExternalUrl(url.toString())
}

function handleFindShortcut(event: KeyboardEvent): void {
  if (!(event.ctrlKey || event.metaKey) || event.key.toLocaleLowerCase() !== 'f') return
  event.preventDefault()
  searchInput.value?.focus()
  searchInput.value?.select()
}

watch(
  () => locale.language,
  loadHelp,
  { immediate: true },
)
onMounted(() => window.addEventListener('keydown', handleFindShortcut))
onBeforeUnmount(() => {
  controller?.abort()
  window.removeEventListener('keydown', handleFindShortcut)
})
</script>

<template>
  <IonPage>
    <IonContent :fullscreen="true" class="help-content">
      <main class="help-page">
        <header class="help-header">
          <div>
            <p>{{ t('Справочник nfprogress').replace(/nfprogress/gi, 'worta') }}</p>
            <h1>{{ t('Помощь') }}</h1>
            <span>{{ t('Справка всегда соответствует возможностям установленной версии приложения.') }}</span>
          </div>
          <IonIcon :icon="bookOutline" aria-hidden="true" />
        </header>

        <label class="help-search" for="help-search-input">
          <span class="visually-hidden">{{ t('Поиск в справке') }}</span>
          <IonIcon :icon="searchOutline" aria-hidden="true" />
          <input
            id="help-search-input"
            ref="searchInput"
            v-model="query"
            type="search"
            autocomplete="off"
            :placeholder="t('Найти ответ в справке')"
          />
          <button
            v-if="query"
            type="button"
            :aria-label="t('Очистить поиск')"
            @click="query = ''"
          >
            <IonIcon :icon="closeCircleOutline" aria-hidden="true" />
          </button>
        </label>

        <StatePanel
          v-if="loading"
          :title="t('Открываем справку')"
          :message="t('Загружаем статьи на выбранном языке.')"
          loading
        />
        <StatePanel
          v-else-if="error"
          :title="t('Не удалось загрузить справку')"
          :message="error"
          :icon="alertCircleOutline"
        >
          <button class="nf-button" type="button" @click="loadHelp">{{ t('Повторить') }}</button>
        </StatePanel>
        <StatePanel
          v-else-if="!sections.length"
          :title="t('Справка пока недоступна')"
          :message="t('В общем каталоге нет статей.')"
          :icon="bookOutline"
        />

        <div v-else class="help-workspace">
          <nav class="help-navigation" :aria-label="t('Разделы справки')">
            <div v-if="query" class="help-results" aria-live="polite">
              <p>{{ t('Результаты поиска: {count}', { count: searchResults.length }) }}</p>
              <ul v-if="searchResults.length">
                <li v-for="section in searchResults" :key="section.key">
                  <button
                    type="button"
                    :aria-current="section.key === selectedKey ? 'page' : undefined"
                    @click="selectSection(section)"
                  >
                    {{ section.title }}
                  </button>
                </li>
              </ul>
              <span v-else>{{ t('По вашему запросу ничего не найдено.') }}</span>
            </div>
            <HelpTree
              v-else
              :sections="sections"
              :selected-key="selectedKey"
              @select="selectSection"
            />
          </nav>

          <article v-if="selected" class="help-article" :aria-labelledby="`help-${selected.key}`">
            <h2 :id="`help-${selected.key}`">{{ selected.title }}</h2>
            <!-- HELP_SECTIONS is trusted application content; user-authored HTML never enters this boundary. -->
            <!-- eslint-disable-next-line vue/no-v-html -->
            <div class="help-article__body" @click="handleArticleClick" v-html="articleHtml"></div>
          </article>
        </div>
      </main>
    </IonContent>
  </IonPage>
</template>

<style scoped>
.help-content {
  --background: var(--nf-color-canvas);
}

.help-page {
  width: min(100%, 86rem);
  min-height: 100%;
  margin: 0 auto;
  padding: calc(var(--nf-space-7) + env(safe-area-inset-top)) clamp(1rem, 3vw, 3.5rem)
    calc(var(--nf-space-7) + env(safe-area-inset-bottom));
}

.help-header {
  display: flex;
  gap: var(--nf-space-5);
  align-items: center;
  justify-content: space-between;
}

.help-header p {
  margin: 0 0 var(--nf-space-2);
  color: var(--nf-color-accent);
  font-size: 0.75rem;
  font-weight: 800;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}

.help-header h1 {
  margin: 0;
  font-family: var(--nf-font-serif);
  font-size: clamp(1.8rem, 3.5vw, 2.75rem);
  letter-spacing: -0.04em;
}

.help-header span {
  display: block;
  max-width: 44rem;
  margin-top: var(--nf-space-3);
  color: var(--nf-color-text-muted);
  line-height: 1.55;
}

.help-header > ion-icon {
  padding: var(--nf-space-4);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
  font-size: 2rem;
}

.help-search {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  gap: var(--nf-space-3);
  align-items: center;
  max-width: 48rem;
  min-height: 3.25rem;
  margin: var(--nf-space-6) 0;
  padding: 0 var(--nf-space-4);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-pill);
  background: var(--nf-color-surface);
}

.help-search > ion-icon {
  color: var(--nf-color-text-muted);
}

.help-search input {
  width: 100%;
  min-width: 0;
  min-height: 3rem;
  border: 0;
  outline: 0;
  background: transparent;
  color: var(--nf-color-text);
}

.help-search button {
  display: grid;
  width: 2.75rem;
  height: 2.75rem;
  padding: 0;
  place-items: center;
  border: 0;
  background: transparent;
  color: var(--nf-color-text-muted);
  cursor: pointer;
}

.help-workspace {
  display: grid;
  grid-template-columns: minmax(15rem, 20rem) minmax(0, 1fr);
  gap: var(--nf-space-6);
  align-items: start;
}

.help-navigation,
.help-article {
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-surface);
  box-shadow: var(--nf-shadow-card);
}

.help-navigation {
  position: sticky;
  top: var(--nf-space-5);
  max-height: calc(100dvh - 10rem);
  padding: var(--nf-space-3);
  overflow: auto;
}

.help-results > p,
.help-results > span {
  display: block;
  margin: var(--nf-space-2) var(--nf-space-3) var(--nf-space-3);
  color: var(--nf-color-text-muted);
  font-size: 0.82rem;
}

.help-results ul {
  display: grid;
  gap: var(--nf-space-1);
  padding: 0;
  margin: 0;
  list-style: none;
}

.help-results button {
  width: 100%;
  min-height: 2.65rem;
  padding: var(--nf-space-2) var(--nf-space-3);
  border: 0;
  border-radius: var(--nf-radius-sm);
  background: transparent;
  color: var(--nf-color-text-muted);
  text-align: left;
  cursor: pointer;
}

.help-results button:hover,
.help-results button[aria-current='page'] {
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
}

.help-article {
  min-width: 0;
  padding: clamp(1.25rem, 4vw, 3rem);
}

.help-article > h2 {
  margin: 0 0 var(--nf-space-5);
  font-family: var(--nf-font-serif);
  font-size: clamp(1.8rem, 4vw, 2.6rem);
  letter-spacing: -0.025em;
}

.help-article__body {
  color: var(--nf-color-text);
  font-size: 1rem;
  line-height: 1.7;
  overflow-wrap: anywhere;
}

.help-article__body :deep(h1),
.help-article__body :deep(h2) {
  display: none;
}

.help-article__body :deep(h3) {
  margin: var(--nf-space-6) 0 var(--nf-space-3);
  font-family: var(--nf-font-serif);
  font-size: 1.35rem;
}

.help-article__body :deep(p),
.help-article__body :deep(ul),
.help-article__body :deep(ol) {
  margin: 0 0 var(--nf-space-4);
}

.help-article__body :deep(a) {
  color: var(--nf-color-primary);
  font-weight: 700;
  text-underline-offset: 0.2em;
}

@media (max-width: 52rem) {
  .help-workspace {
    grid-template-columns: 1fr;
  }

  .help-navigation {
    position: static;
    max-height: 17rem;
  }
}

@media (max-width: 36rem) {
  .help-header > ion-icon {
    display: none;
  }
}
</style>
