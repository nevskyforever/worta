<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { IonContent, IonIcon, IonPage, onIonViewWillEnter } from '@ionic/vue'
import { documentTextOutline, gitBranchOutline, openOutline, searchOutline } from 'ionicons/icons'

import StatePanel from '@/components/ui/StatePanel.vue'
import { openWorkspaceWindow } from '@/platform/workspaceWindows'
import { useLocaleStore } from '@/stores/locale'
import { useNotificationsStore } from '@/stores/notifications'
import { useProjectsStore } from '@/stores/projects'
import type { Project } from '@/types/api'

type ResourceKind = 'maps' | 'notes'

const route = useRoute()
const router = useRouter()
const store = useProjectsStore()
const locale = useLocaleStore()
const notifications = useNotificationsStore()
const t = locale.translate
const kind = computed<ResourceKind>(() => route.name === 'maps' ? 'maps' : 'notes')
const storageKey = computed(() => `nfprogress:${kind.value}:hub-state`)
const search = ref('')

function resourceTimestamp(project: Project): number {
  const value = kind.value === 'maps' ? project.mindmap_updated_at : project.notes_updated_at
  if (!value) return 0
  const timestamp = Date.parse(value)
  return Number.isFinite(timestamp) ? timestamp : 0
}

const visibleProjects = computed(() => {
  const query = search.value.trim().toLocaleLowerCase(locale.localeTag)
  return store.projects
    .filter((project) =>
      !query || project.name.toLocaleLowerCase(locale.localeTag).includes(query),
    )
    .sort((left, right) => resourceTimestamp(right) - resourceTimestamp(left))
})
const title = computed(() => kind.value === 'maps' ? t('Карты проектов') : t('Заметки проектов'))
const description = computed(() => kind.value === 'maps'
  ? t('Выберите проект, чтобы открыть и редактировать его карту.')
  : t('Выберите проект, чтобы открыть и редактировать его заметки.'))
const icon = computed(() => kind.value === 'maps' ? gitBranchOutline : documentTextOutline)

function target(project: Project) {
  return { name: kind.value === 'maps' ? 'global-project-map' : 'global-project-notes', params: { projectId: project.id } }
}

async function openHere(project: Project): Promise<void> {
  await router.push(target(project))
}

async function openSeparate(project: Project): Promise<void> {
  const resolved = router.resolve(target(project))
  try {
    await openWorkspaceWindow(resolved.href, `${project.name} — ${title.value}`)
  } catch {
    notifications.error(t('Не удалось открыть отдельное окно.'))
  }
}

function restoreState(): void {
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey.value) ?? '{}') as { search?: unknown }
    if (typeof saved.search === 'string') search.value = saved.search
  } catch { /* optional state */ }
}

watch(kind, () => restoreState())
watch(search, (value) => {
  try { localStorage.setItem(storageKey.value, JSON.stringify({ search: value })) } catch { /* optional */ }
})
function loadProjects(): void { void store.load({ sort: 'manual' }) }
onMounted(() => { restoreState(); loadProjects() })
onIonViewWillEnter(loadProjects)
</script>

<template>
  <IonPage>
    <IonContent :fullscreen="true" class="resource-hub-content">
      <main class="resource-hub">
        <header>
          <p>{{ t('Рабочее пространство') }}</p>
          <h1>{{ title }}</h1>
          <span>{{ description }}</span>
        </header>
        <label class="resource-search">
          <IonIcon :icon="searchOutline" aria-hidden="true" />
          <input v-model="search" type="search" :placeholder="t('Поиск проектов')" />
        </label>
        <StatePanel v-if="store.loading && !store.projects.length" :title="t('Загружаем проекты')" :message="description" loading />
        <StatePanel v-else-if="store.error" :title="t('Не удалось загрузить проекты')" :message="store.error" />
        <div v-else-if="visibleProjects.length" class="resource-grid">
          <article v-for="project in visibleProjects" :key="project.id" class="resource-tile">
            <img v-if="project.cover_image" :src="project.cover_image" alt="" />
            <div v-else class="resource-tile__placeholder"><IonIcon :icon="icon" aria-hidden="true" /></div>
            <div class="resource-tile__body">
              <h2>{{ project.name }}</h2>
              <p>{{ project.stages_enabled ? `${t(project.name === 'Общий проект' ? 'Источников' : 'Этапов')}: ${project.stages.length}` : t('Весь проект') }}</p>
              <div>
                <button class="nf-button" type="button" @click="openHere(project)">{{ t('Открыть здесь') }}</button>
                <button class="nf-button nf-button--secondary" type="button" @click="openSeparate(project)">
                  <IonIcon :icon="openOutline" aria-hidden="true" />{{ t('В отдельном окне') }}
                </button>
              </div>
            </div>
          </article>
        </div>
        <StatePanel v-else :title="t('Ничего не найдено')" :message="t('Создайте проект или измените поисковый запрос.')" :icon="icon" />
      </main>
    </IonContent>
  </IonPage>
</template>

<style scoped>
.resource-hub-content { --background: var(--nf-color-canvas); }
.resource-hub { width: min(100%, 91rem); min-height: 100%; margin: 0 auto; padding: calc(var(--nf-space-7) + env(safe-area-inset-top)) clamp(1rem, 3vw, 3.5rem) var(--nf-space-7); }
.resource-hub header p { margin: 0 0 var(--nf-space-2); color: var(--nf-color-accent); font-size: .75rem; font-weight: 800; letter-spacing: .1em; text-transform: uppercase; }
.resource-hub header h1 { margin: 0; font-family: var(--nf-font-serif); font-size: clamp(2rem, 4vw, 2.8rem); }
.resource-hub header span { display: block; margin-top: var(--nf-space-2); color: var(--nf-color-text-muted); }
.resource-search { display: grid; grid-template-columns: auto 1fr; gap: var(--nf-space-2); align-items: center; max-width: 38rem; min-height: 3rem; margin: var(--nf-space-6) 0 var(--nf-space-4); padding: 0 var(--nf-space-3); border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-sm); background: var(--nf-color-surface); }
.resource-search input { min-height: 2.8rem; border: 0; outline: 0; background: transparent; color: var(--nf-color-text); }
.resource-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 20rem), 1fr)); gap: var(--nf-space-4); }
.resource-tile { display: grid; grid-template-columns: 6.25rem minmax(0, 1fr); overflow: hidden; border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-md); background: var(--nf-color-surface); box-shadow: var(--nf-shadow-card); }
.resource-tile > img, .resource-tile__placeholder { width: 6.25rem; height: 100%; min-height: 10rem; object-fit: cover; background: var(--nf-color-primary-soft); }
.resource-tile__placeholder { display: grid; place-items: center; color: var(--nf-color-primary); font-size: 2rem; }
.resource-tile__body { display: flex; min-width: 0; padding: var(--nf-space-4); flex-direction: column; }
.resource-tile h2 { overflow-wrap: anywhere; margin: 0; font-family: var(--nf-font-serif); font-size: 1.25rem; }
.resource-tile p { margin: var(--nf-space-2) 0 var(--nf-space-4); color: var(--nf-color-text-muted); font-size: .8rem; }
.resource-tile__body > div { display: grid; gap: var(--nf-space-2); margin-top: auto; }
.resource-tile .nf-button { min-height: 2.45rem; padding: .45rem .65rem; font-size: .78rem; }
@media (max-width: 30rem) { .resource-tile { grid-template-columns: 1fr; } .resource-tile > img, .resource-tile__placeholder { width: 100%; height: 8rem; min-height: 0; } }
</style>
