<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { EditorContent, useEditor } from '@tiptap/vue-3'
import type { Editor } from '@tiptap/core'
import type { TiptapDocument } from '@/types/documents'
import { createDocumentEditorExtensions } from './editorExtensions'
import NFEditorToolbar from './NFEditorToolbar.vue'
import { calculateEditorPageGeometry, TYPEWRITER_RATIO } from './editorGeometry'

const props = defineProps<{
  initialContent: TiptapDocument
  translate?: (source: string) => string
  zoom?: number
  typewriterMode?: boolean
}>()
const emit = defineEmits<{
  update: [content: TiptapDocument]
  ready: [editor: Editor]
}>()
const viewport = ref<HTMLElement | null>(null)
const contentLayer = ref<HTMLElement | null>(null)
const viewportWidth = ref(0)
const viewportHeight = ref(0)
const viewportPaddingX = ref(0)
const viewportPaddingY = ref(0)
const contentHeight = ref(1)
const trackingStarted = ref(false)
let resizeObserver: ResizeObserver | undefined
let trackingFrame: number | undefined

const geometry = computed(() => calculateEditorPageGeometry({
  viewportWidth: viewportWidth.value,
  viewportHeight: viewportHeight.value,
  viewportPaddingX: viewportPaddingX.value,
  viewportPaddingY: viewportPaddingY.value,
  contentHeight: contentHeight.value,
  zoom: props.zoom ?? 100,
  typewriterMode: props.typewriterMode ?? false,
}))
const pageStyle = computed(() => ({
  width: `${geometry.value.pageWidth}px`,
  height: `${geometry.value.pageHeight}px`,
}))
const contentLayerStyle = computed(() => ({
  left: `${geometry.value.contentLeft}px`,
  top: `${geometry.value.contentTop}px`,
  width: `${geometry.value.contentLayoutWidth}px`,
  transform: `scale(${geometry.value.scale})`,
}))

const editor = useEditor({
  content: props.initialContent,
  extensions: createDocumentEditorExtensions(),
  editorProps: {
    attributes: {
      class: 'nf-editor-content',
      'aria-label': 'Текст документа',
    },
  },
  onCreate: ({ editor }) => emit('ready', editor),
  onUpdate: ({ editor }) => {
    emit('update', editor.getJSON() as TiptapDocument)
    void nextTick(() => {
      updateLayoutMetrics()
      trackTypewriterCaret(false)
    })
  },
})

function updateLayoutMetrics(): void {
  const container = viewport.value
  if (container) {
    const styles = getComputedStyle(container)
    viewportWidth.value = container.clientWidth
    viewportHeight.value = container.clientHeight
    viewportPaddingX.value = Number.parseFloat(styles.paddingLeft || '0')
    viewportPaddingY.value = Number.parseFloat(styles.paddingTop || '0')
  }
  if (contentLayer.value) contentHeight.value = Math.max(1, contentLayer.value.scrollHeight)
}

function caretOffsetFromTarget(): number | null {
  const instance = editor.value
  const container = viewport.value
  if (!instance || !container) return null
  try {
    const coords = instance.view.coordsAtPos(instance.state.selection.from)
    const caretCenter = (coords.top + coords.bottom) / 2
    const target = container.getBoundingClientRect().top + container.clientHeight * TYPEWRITER_RATIO
    return caretCenter - target
  } catch {
    return null
  }
}

function trackTypewriterCaret(activation: boolean): void {
  if (!props.typewriterMode || !viewport.value) return
  const offset = caretOffsetFromTarget()
  if (offset === null) return
  if (activation) trackingStarted.value = offset >= -2
  if (!trackingStarted.value && offset < -2) return
  trackingStarted.value = true
  viewport.value.scrollTop += offset
}

function scheduleTypewriterPositioning(activation = !trackingStarted.value): void {
  if (trackingFrame !== undefined) window.cancelAnimationFrame(trackingFrame)
  void nextTick(() => {
    updateLayoutMetrics()
    trackingFrame = window.requestAnimationFrame(() => {
      trackingFrame = undefined
      trackTypewriterCaret(activation)
    })
  })
}

watch(() => props.typewriterMode, (enabled) => {
  trackingStarted.value = false
  if (enabled) scheduleTypewriterPositioning(true)
})
watch(() => props.zoom, () => {
  void nextTick(() => {
    updateLayoutMetrics()
    if (props.typewriterMode) scheduleTypewriterPositioning()
  })
})

onMounted(() => {
  void nextTick(() => {
    updateLayoutMetrics()
    if (typeof ResizeObserver !== 'undefined') {
      resizeObserver = new ResizeObserver(() => {
        updateLayoutMetrics()
        if (props.typewriterMode) scheduleTypewriterPositioning()
      })
      if (viewport.value) resizeObserver.observe(viewport.value)
      if (contentLayer.value) resizeObserver.observe(contentLayer.value)
    }
    if (props.typewriterMode) scheduleTypewriterPositioning(true)
  })
})
onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  if (trackingFrame !== undefined) window.cancelAnimationFrame(trackingFrame)
})

function getEditor(): Editor | null {
  return editor.value ?? null
}

function getScrollContainer(): HTMLElement | null {
  return viewport.value
}

function focus(): void {
  editor.value?.commands.focus()
}

function getJSON(): TiptapDocument {
  return (editor.value?.getJSON() ?? props.initialContent) as TiptapDocument
}

function setContent(content: TiptapDocument | string, emitUpdate = false): void {
  editor.value?.commands.setContent(content, { emitUpdate })
}

function getSelection(): number | null {
  return editor.value?.state.selection.from ?? null
}

async function setSelection(position: number): Promise<void> {
  if (!editor.value) return
  editor.value.commands.setTextSelection(Math.min(position, editor.value.state.doc.content.size))
  await nextTick()
}

defineExpose({ focus, getEditor, getJSON, getScrollContainer, getSelection, setContent, setSelection })
</script>

<template>
  <section class="nf-document-editor">
    <NFEditorToolbar v-if="editor" :editor="editor" :translate="translate" />
    <div ref="viewport" class="nf-document-editor__viewport">
      <div class="nf-document-editor__page" :style="pageStyle">
        <div ref="contentLayer" class="nf-document-editor__content-layer" :style="contentLayerStyle">
          <EditorContent v-if="editor" :editor="editor" class="nf-document-editor__content" />
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.nf-document-editor {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  flex: 1;
  background: var(--nf-color-canvas);
}

.nf-document-editor__viewport {
  box-sizing: border-box;
  flex: 1 1 auto;
  width: 100%;
  min-width: 0;
  min-height: 0;
  padding: var(--nf-space-5);
  overflow: auto;
  scrollbar-gutter: stable;
}

.nf-document-editor__page {
  position: relative;
  box-sizing: border-box;
  margin: 0 auto;
  overflow: hidden;
  color: var(--nf-color-text);
  background: color-mix(in srgb, var(--nf-color-surface) 82%, white);
  box-shadow: var(--nf-shadow-card);
}

.nf-document-editor__content-layer {
  position: absolute;
  transform-origin: top left;
}

.nf-document-editor__content :deep(.nf-editor-content) {
  box-sizing: border-box;
  width: 100%;
  min-height: 0;
  color: inherit;
  font-family: Arial, sans-serif;
  font-size: 12pt;
  line-height: 1.5;
  background: transparent;
  outline: none;
  white-space: pre-wrap;
}

.nf-document-editor__content :deep(.nf-editor-content > :first-child) { margin-top: 0; }
.nf-document-editor__content :deep(.nf-editor-content > :last-child) { margin-bottom: 0; }

@media (max-width: 44rem) {
  .nf-document-editor__viewport { padding: var(--nf-space-2); }
  .nf-document-editor__page {
    box-shadow: none;
  }
}
</style>
