import { Extension } from '@tiptap/core'
import Color from '@tiptap/extension-color'
import FontFamily from '@tiptap/extension-font-family'
import Highlight from '@tiptap/extension-highlight'
import Subscript from '@tiptap/extension-subscript'
import Superscript from '@tiptap/extension-superscript'
import TextAlign from '@tiptap/extension-text-align'
import { FontSize, LineHeight, TextStyle } from '@tiptap/extension-text-style'
import StarterKit from '@tiptap/starter-kit'

const InsertTab = Extension.create({
  name: 'nfInsertTab',
  addKeyboardShortcuts() {
    return {
      Tab: () => this.editor.commands.insertContent({ type: 'text', text: '\t' }),
    }
  },
})

export function createDocumentEditorExtensions(readOnly = false) {
  return [
    StarterKit.configure({
      heading: { levels: [1, 2, 3, 4, 5, 6] },
    }),
    TextStyle,
    FontFamily,
    FontSize,
    LineHeight.configure({ types: ['paragraph', 'heading'] }),
    Color,
    Highlight.configure({ multicolor: true }),
    Subscript,
    Superscript,
    TextAlign.configure({ types: ['heading', 'paragraph'] }),
    ...(readOnly ? [] : [InsertTab]),
  ]
}
