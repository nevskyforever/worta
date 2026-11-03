<script setup lang="ts">
import { computed } from 'vue'
import { IonContent, IonPage } from '@ionic/vue'
import { useRoute } from 'vue-router'
import DocumentEditor from '@/components/documents/DocumentEditor.vue'
import { useProjectsStore } from '@/stores/projects'
import { useLocaleStore } from '@/stores/locale'

const route = useRoute()
const locale = useLocaleStore()
const t = locale.translate
const projectStore = useProjectsStore()
const sharedSource = computed(() => {
  const project = projectStore.currentProject?.id === projectId.value ? projectStore.currentProject : projectStore.projects.find(item => item.id === projectId.value)
  return project?.name === 'Общий проект'
})
const projectId = computed(() => String(route.params.projectId))
const stageId = computed(() => route.name === 'stage-document' && typeof route.params.stageId === 'string'
  ? route.params.stageId
  : undefined)
const title = computed(() => typeof route.query.title === 'string' ? route.query.title : (stageId.value ? t(sharedSource.value ? 'Текст источника' : 'Текст этапа') : t('Текст проекта')))
</script>
<template><IonPage><IonContent :fullscreen="true"><DocumentEditor :scope="{ projectId, stageId }" :title="title" /></IonContent></IonPage></template>
