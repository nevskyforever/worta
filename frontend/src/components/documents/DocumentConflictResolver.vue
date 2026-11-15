<script setup lang="ts">
import type { ConflictChoice } from '@/composables/useDocumentSync'
const emit = defineEmits<{ resolve: [choice: ConflictChoice] }>()
</script>
<template><div class="conflict" role="alertdialog" aria-label="Конфликт синхронизации"><strong>Обнаружен конфликт Word</strong><p>Текст был изменён и в worta, и в Word.</p><div><button class="nf-button" @click="emit('resolve', 'nfprogress')">Оставить worta</button><button class="nf-button nf-button--secondary" @click="emit('resolve', 'word')">Принять Word</button><button class="nf-button nf-button--secondary" @click="emit('resolve', 'both')">Сохранить обе</button></div></div></template>
<style scoped>.conflict{position:fixed;z-index:30;right:1rem;bottom:1rem;max-width:28rem;padding:1rem;border:1px solid var(--nf-color-border);border-radius:var(--nf-radius-md);background:var(--nf-color-surface);box-shadow:var(--nf-shadow-card)}.conflict p{color:var(--nf-color-text-muted)}.conflict div{display:flex;flex-wrap:wrap;gap:.5rem}</style>
