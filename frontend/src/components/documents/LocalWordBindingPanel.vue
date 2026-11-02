<script setup lang="ts">
import {computed} from 'vue'
import {useLocaleStore} from '@/stores/locale'
import type {TiptapDocument} from '@/types/documents'
const props=defineProps<{attached:boolean;state?:string;externalContent?:TiptapDocument}>()
const emit=defineEmits<{reattach:[];cancel:[];decision:[choice:'compare'|'cloud'|'import'|'unlink']}>()
const t=useLocaleStore().translate
const label=computed(()=>t(!props.attached?'Файл Word не подключён на этом устройстве':props.state==='synced'?'Файл Word связан':props.state==='external_import_pending'?'Импорт Word ожидает подтверждения синхронизации':props.state==='missing_external'?'Локальный файл не найден. Переподключите его.':'Word отличается от WORTA. Выберите действие.'))
const showReattach = computed(() => !props.attached || props.state === 'missing_external')
const showResolutionActions = computed(() => props.attached && props.state !== 'synced' && props.state !== 'unlinked' && props.state !== 'missing_external')
function text(node:unknown):string{
 if(!node||typeof node!=='object')return ''
 const n=node as {text?:unknown;content?:unknown;type?:unknown}
 return (typeof n.text==='string'?n.text:'')+(Array.isArray(n.content)?n.content.map(text).join(''):'')+(['paragraph','heading','listItem'].includes(String(n.type))?'\n':'')
}
const preview=computed(()=>text(props.externalContent))
</script>
<template>
 <section class="local-word-binding" aria-live="polite">
  <p role="status">{{label}}</p>
  <p>{{t('Локальные файлы подключаются отдельно на каждом устройстве. Выбор существующего файла не перезаписывает его.')}}</p>
  <div v-if="showReattach" class="local-word-binding__actions">
   <button class="nf-button" type="button" data-action="reattach" @click="emit('reattach')">{{t('Переподключить существующий Word-файл')}}</button>
  </div>
  <details v-if="showResolutionActions" class="local-word-binding__decisions">
   <summary>{{t('Действия')}}</summary>
   <div class="local-word-binding__actions">
    <button class="nf-button" type="button" data-action="compare" @click="emit('decision','compare')">{{t('Повторить сравнение')}}</button>
    <button class="nf-button" type="button" data-action="cloud" @click="emit('decision','cloud')">{{t('Использовать версию WORTA')}}</button>
    <button class="nf-button" type="button" data-action="import" @click="emit('decision','import')">{{t('Импортировать версию Word')}}</button>
    <button class="nf-button" type="button" data-action="unlink" @click="emit('decision','unlink')">{{t('Отключить локальный файл')}}</button>
   </div>
  </details>
  <details v-if="preview"><summary>{{t('Предпросмотр локальной версии Word')}}</summary><p class="local-word-binding__preview">{{preview}}</p><button class="nf-button" type="button" @click="emit('cancel')">{{t('Отмена')}}</button></details>
 </section>
</template>
<style scoped>
.local-word-binding{padding:1rem;border:1px solid var(--border-color,#ddd);border-radius:.5rem;margin:.75rem 0}
.local-word-binding__actions{display:flex;flex-wrap:wrap;gap:.5rem}
.local-word-binding__preview{white-space:pre-wrap;max-height:18rem;overflow:auto}
</style>
