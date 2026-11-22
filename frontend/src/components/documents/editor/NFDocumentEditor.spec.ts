import { enableAutoUnmount, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { Editor } from '@tiptap/core'
import type { TiptapDocument } from '@/types/documents'
import NFDocumentEditor from './NFDocumentEditor.vue'

enableAutoUnmount(afterEach)

const formattedDocument: TiptapDocument = {
  type: 'doc',
  content: [{
    type: 'paragraph',
    attrs: { textAlign: 'center', lineHeight: '1.5' },
    content: [{
      type: 'text',
      text: 'Существующий текст',
      marks: [
        { type: 'bold' },
        { type: 'textStyle', attrs: { fontFamily: 'Georgia', fontSize: '18pt', color: '#336699' } },
      ],
    }],
  }],
}

type EditorExpose = {
  getEditor: () => Editor | null
  getJSON: () => TiptapDocument
  getScrollContainer: () => HTMLElement | null
  setContent: (content: TiptapDocument | string, emitUpdate?: boolean) => void
}

describe('NFDocumentEditor core', () => {
  it('preserves persisted tabs and paragraph line heights without Word sync', () => {
    const persistedDocument: TiptapDocument = {
      type: 'doc',
      content: [
        {
          type: 'paragraph',
          attrs: { lineHeight: '1.5' },
          content: [{ type: 'text', text: '\tПервый абзац' }],
        },
        {
          type: 'paragraph',
          attrs: { lineHeight: '2' },
          content: [{ type: 'text', text: '\tВторой абзац' }],
        },
      ],
    }
    const wrapper = mount(NFDocumentEditor, { props: { initialContent: persistedDocument } })
    const api = wrapper.vm as unknown as EditorExpose

    const json = api.getJSON()
    expect(json.content).toHaveLength(2)
    expect(json.content?.map((node) => (node.attrs as { lineHeight?: string })?.lineHeight)).toEqual(['1.5', '2'])
    expect(json.content?.map((node) => (node.content as Array<{ text?: string }>)?.[0]?.text)).toEqual([
      '\tПервый абзац',
      '\tВторой абзац',
    ])
  })

  it('loads existing Tiptap JSON without changing its supported schema', () => {
    const wrapper = mount(NFDocumentEditor, { props: { initialContent: formattedDocument } })
    const api = wrapper.vm as unknown as EditorExpose

    const json = api.getJSON()
    const paragraph = json.content?.[0] as { attrs?: Record<string, unknown>; content?: Array<{ marks?: Array<{ type?: string; attrs?: Record<string, unknown> }> }> }
    expect(paragraph.attrs).toMatchObject({ textAlign: 'center', lineHeight: '1.5' })
    expect(paragraph.content?.[0]?.marks).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'bold' }),
      expect.objectContaining({ type: 'textStyle', attrs: expect.objectContaining({ fontFamily: 'Georgia', fontSize: '18pt', color: '#336699' }) }),
    ]))
    expect(api.getScrollContainer()).toBe(wrapper.get('.nf-document-editor__viewport').element)
  })

  it('keeps initial content when parent persistence state changes and only replaces it explicitly', async () => {
    const wrapper = mount(NFDocumentEditor, { props: { initialContent: formattedDocument } })
    const api = wrapper.vm as unknown as EditorExpose
    const emptyDocument: TiptapDocument = { type: 'doc', content: [{ type: 'paragraph' }] }
    const replacement: TiptapDocument = {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Явная замена' }] }],
    }

    expect(api.getEditor()?.getText()).toContain('Существующий текст')
    await wrapper.setProps({ initialContent: emptyDocument })
    expect(api.getEditor()?.getText()).toContain('Существующий текст')
    expect(JSON.stringify(api.getJSON())).toContain('Существующий текст')

    api.setContent(replacement)
    expect(api.getEditor()?.getText()).toBe('Явная замена')
    expect(api.getJSON()).toMatchObject(replacement)
  })

  it('emits the current JSON when the document changes', () => {
    const wrapper = mount(NFDocumentEditor, { props: { initialContent: { type: 'doc', content: [{ type: 'paragraph' }] } } })
    const api = wrapper.vm as unknown as EditorExpose
    const next: TiptapDocument = { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Новый текст' }] }] }

    api.setContent(next, true)

    expect(wrapper.emitted('update')?.at(-1)?.[0]).toMatchObject(next)
  })

  it('inserts a tab character for plain Tab and leaves modified Tab untouched', async () => {
    const wrapper = mount(NFDocumentEditor, {
      props: { initialContent: { type: 'doc', content: [{ type: 'paragraph' }] } },
    })
    const api = wrapper.vm as unknown as EditorExpose
    await wrapper.vm.$nextTick()
    const editable = api.getEditor()!.view.dom

    const plainTab = new KeyboardEvent('keydown', {
      key: 'Tab',
      bubbles: true,
      cancelable: true,
    })
    editable.dispatchEvent(plainTab)
    expect(plainTab.defaultPrevented).toBe(true)
    const paragraph = api.getJSON().content?.[0] as {
      content?: Array<Record<string, unknown>>
    }
    expect(paragraph.content?.[0]).toMatchObject({
      type: 'text',
      text: '\t',
    })

    const modifiedTab = new KeyboardEvent('keydown', {
      key: 'Tab',
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    })
    editable.dispatchEvent(modifiedTab)
    expect(modifiedTab.defaultPrevented).toBe(false)
    const unchangedParagraph = api.getJSON().content?.[0] as {
      content?: Array<Record<string, unknown>>
    }
    expect(unchangedParagraph.content?.[0]).toMatchObject({ text: '\t' })
  })

  it('reflects the selection in the toolbar and applies formatting commands', async () => {
    const wrapper = mount(NFDocumentEditor, { props: { initialContent: formattedDocument } })
    const api = wrapper.vm as unknown as EditorExpose
    const editor = api.getEditor()
    expect(editor).not.toBeNull()

    editor!.commands.setTextSelection({ from: 1, to: 5 })
    await wrapper.vm.$nextTick()

    expect((wrapper.find('select[aria-label="Шрифт"]').element as HTMLSelectElement).value).toBe('Georgia')
    expect((wrapper.find('select[aria-label="Размер текста"]').element as HTMLSelectElement).value).toBe('18pt')
    expect(wrapper.find('button[aria-label="Полужирный"]').attributes('aria-pressed')).toBe('true')

    await wrapper.find('button[aria-label="Курсив"]').trigger('click')
    await wrapper.find('select[aria-label="Межстрочный интервал"]').setValue('2')
    await wrapper.find('button[aria-label="По правому краю"]').trigger('click')

    const json = api.getJSON()
    const paragraph = json.content?.[0] as { attrs?: Record<string, unknown>; content?: Array<{ marks?: Array<{ type?: string }> }> }
    expect(paragraph.attrs).toMatchObject({ textAlign: 'right', lineHeight: '2' })
    expect(paragraph.content?.[0]?.marks).toEqual(expect.arrayContaining([expect.objectContaining({ type: 'italic' })]))
  })

  it('shows effective defaults, explicit caret styles, and blank values only for mixed selections', async () => {
    const styleDocument: TiptapDocument = {
      type: 'doc',
      content: [
        { type: 'paragraph', content: [{ type: 'text', text: 'Обычный' }] },
        {
          type: 'paragraph',
          content: [{
            type: 'text',
            text: 'Стиль',
            marks: [{ type: 'textStyle', attrs: { fontFamily: 'Georgia', fontSize: '18pt' } }],
          }],
        },
      ],
    }
    const wrapper = mount(NFDocumentEditor, { props: { initialContent: styleDocument } })
    const editor = (wrapper.vm as unknown as EditorExpose).getEditor()!
    const textPositions = new Map<string, number>()
    editor.state.doc.descendants((node, position) => {
      if (node.isText && node.text) textPositions.set(node.text, position)
    })
    await wrapper.vm.$nextTick()
    const font = wrapper.find('select[aria-label="Шрифт"]')
    const size = wrapper.find('select[aria-label="Размер текста"]')
    const lineHeight = wrapper.find('select[aria-label="Межстрочный интервал"]')

    editor.commands.setTextSelection((textPositions.get('Обычный') ?? 0) + 1)
    await wrapper.vm.$nextTick()
    expect((font.element as HTMLSelectElement).value).toBe('Arial')
    expect((size.element as HTMLSelectElement).value).toBe('12pt')
    expect((lineHeight.element as HTMLSelectElement).value).toBe('1.5')

    editor.commands.setTextSelection((textPositions.get('Стиль') ?? 0) + 1)
    await wrapper.vm.$nextTick()
    expect((font.element as HTMLSelectElement).value).toBe('Georgia')
    expect((size.element as HTMLSelectElement).value).toBe('18pt')

    editor.commands.setTextSelection({
      from: textPositions.get('Обычный') ?? 1,
      to: (textPositions.get('Стиль') ?? 1) + 'Стиль'.length,
    })
    await wrapper.vm.$nextTick()
    expect((font.element as HTMLSelectElement).value).toBe('')
    expect((size.element as HTMLSelectElement).value).toBe('')
  })

  it('renders toolbar action buttons with consistently scalable icons', async () => {
    const wrapper = mount(NFDocumentEditor, { props: { initialContent: formattedDocument } })
    await wrapper.vm.$nextTick()

    const actionButtons = wrapper.findAll('.nf-editor-toolbar button')
    expect(actionButtons.length).toBeGreaterThan(0)
    expect(actionButtons.every((button) => button.find('svg').exists())).toBe(true)
  })

  it('converts imported Word HTML into DOCX-compatible JSON attributes', () => {
    const wrapper = mount(NFDocumentEditor, { props: { initialContent: { type: 'doc', content: [{ type: 'paragraph' }] } } })
    const api = wrapper.vm as unknown as EditorExpose

    api.setContent(
      '<h2 style="text-align: center; line-height: 2"><span style="font-family: Georgia; font-size: 18pt; color: #336699"><strong><u>Заголовок</u></strong></span></h2><p><mark style="background-color: #ffff00"><sup>Текст</sup></mark></p>',
      false,
    )

    const json = api.getJSON()
    expect(json.content?.[0]).toMatchObject({
      type: 'heading',
      attrs: { level: 2, textAlign: 'center', lineHeight: '2' },
      content: [{
        text: 'Заголовок',
        marks: expect.arrayContaining([
          expect.objectContaining({ type: 'bold' }),
          expect.objectContaining({ type: 'underline' }),
          expect.objectContaining({ type: 'textStyle', attrs: expect.objectContaining({ fontFamily: 'Georgia', fontSize: '18pt', color: '#336699' }) }),
        ]),
      }],
    })
    expect(json.content?.[1]).toMatchObject({
      type: 'paragraph',
      content: [{
        text: 'Текст',
        marks: expect.arrayContaining([
          expect.objectContaining({ type: 'highlight', attrs: expect.objectContaining({ color: '#ffff00' }) }),
          expect.objectContaining({ type: 'superscript' }),
        ]),
      }],
    })
  })

  it('does not center an early caret but positions a lower caret immediately on Typewriter activation', async () => {
    const wrapper = mount(NFDocumentEditor, {
      props: { initialContent: formattedDocument, zoom: 100, typewriterMode: false },
    })
    const api = wrapper.vm as unknown as EditorExpose
    const container = api.getScrollContainer()!
    let scrollTop = 0
    Object.defineProperties(container, {
      clientWidth: { configurable: true, value: 900 },
      clientHeight: { configurable: true, value: 600 },
      scrollTop: {
        configurable: true,
        get: () => scrollTop,
        set: (value: number) => { scrollTop = value },
      },
    })
    container.getBoundingClientRect = () => new DOMRect(0, 100, 900, 600)
    const coords = vi.spyOn(api.getEditor()!.view, 'coordsAtPos')
    coords.mockReturnValue({ top: 180, bottom: 200, left: 0, right: 1 })
    const animationFrames: FrameRequestCallback[] = []
    const requestFrame = vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
      animationFrames.push(callback)
      return animationFrames.length
    })

    await wrapper.setProps({ typewriterMode: true })
    await wrapper.vm.$nextTick()
    animationFrames.shift()?.(0)
    expect(scrollTop).toBe(0)

    await wrapper.setProps({ typewriterMode: false })
    coords.mockReturnValue({ top: 500, bottom: 520, left: 0, right: 1 })
    await wrapper.setProps({ typewriterMode: true })
    await wrapper.vm.$nextTick()
    animationFrames.shift()?.(0)
    expect(scrollTop).toBe(110)

    requestFrame.mockRestore()
    coords.mockRestore()
  })
})
