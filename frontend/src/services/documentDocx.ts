import mammoth from 'mammoth'
import { AlignmentType, Document, HeadingLevel, HighlightColor, type IParagraphOptions, Packer, Paragraph, Tab, TextRun } from 'docx'
import type { TiptapDocument } from '@/types/documents'

export const WORD_FONT_FAMILIES = ['Arial', 'Georgia', 'Times New Roman', 'Courier New'] as const
export const WORD_FONT_SIZES = [9, 10, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 32, 36, 48, 72] as const

type TextMark = { type?: string; attrs?: { color?: string; fontFamily?: string; fontSize?: string; href?: string } }
type EditorNode = {
  type?: string
  text?: string
  marks?: TextMark[]
  content?: EditorNode[]
  attrs?: { level?: number; textAlign?: string; lineHeight?: string }
}

const HIGHLIGHT_COLORS: Record<string, (typeof HighlightColor)[keyof typeof HighlightColor]> = {
  '#ffff00': HighlightColor.YELLOW,
  '#00ff00': HighlightColor.GREEN,
  '#00ffff': HighlightColor.CYAN,
  '#ff00ff': HighlightColor.MAGENTA,
  '#0000ff': HighlightColor.BLUE,
  '#ff0000': HighlightColor.RED,
  '#808080': HighlightColor.DARK_GRAY,
}
const MAMMOTH_HIGHLIGHTS: Array<[string, string]> = [
  ['yellow', '#ffff00'], ['green', '#00ff00'], ['cyan', '#00ffff'], ['magenta', '#ff00ff'],
  ['blue', '#0000ff'], ['red', '#ff0000'], ['darkGray', '#808080'],
]

function supportedFont(font: unknown): string | undefined {
  if (typeof font !== 'string') return undefined
  return WORD_FONT_FAMILIES.find((candidate) => candidate.toLocaleLowerCase() === font.trim().toLocaleLowerCase())
}
function fontSizePoints(value: unknown): number | undefined {
  if (typeof value !== 'string') return undefined
  const match = /^(\d+(?:\.\d+)?)(pt|px)?$/i.exec(value.trim())
  if (!match) return undefined
  const points = Number(match[1]) * (match[2]?.toLowerCase() === 'px' ? 0.75 : 1)
  return Number.isFinite(points) ? points : undefined
}
function styleMap(): string[] {
  const textStyles = WORD_FONT_FAMILIES.flatMap((font) => WORD_FONT_SIZES.map((size) =>
    `r[style-name='NFProgress ${font} ${size}'] => span[style='font-family: ${font}; font-size: ${size}pt']`,
  ))
  return [
    ...textStyles,
    'u => u',
    ...MAMMOTH_HIGHLIGHTS.map(([wordColor, cssColor]) => `highlight[color='${wordColor}'] => mark[style='background-color: ${cssColor}']`),
  ]
}
function preserveWordTextFormatting(document: unknown): unknown {
  const visit = (value: unknown): unknown => {
    if (!value || typeof value !== 'object') return value
    const node = value as Record<string, unknown>
    const children = Array.isArray(node.children) ? node.children.map(visit) : node.children
    if (node.type !== 'run') return { ...node, children }
    const font = supportedFont(node.font)
    const size = typeof node.fontSize === 'number' && WORD_FONT_SIZES.includes(node.fontSize as never) ? node.fontSize : undefined
    return font || size
      ? { ...node, children, styleName: `NFProgress ${font ?? 'Arial'} ${size ?? 12}` }
      : { ...node, children }
  }
  return visit(document)
}

export async function importDocx(file: ArrayBuffer): Promise<string> {
  const result = await mammoth.convertToHtml({ arrayBuffer: file }, {
    styleMap: styleMap(),
    transformDocument: preserveWordTextFormatting,
    convertImage: mammoth.images.imgElement((image) => image.read('base64').then((value) => ({ src: `data:${image.contentType};base64,${value}` }))),
  })
  return result.value
}

function mark(node: EditorNode, type: string): TextMark | undefined { return node.marks?.find((item) => item.type === type) }
function run(node: EditorNode): TextRun[] {
  if (node.type === 'hardBreak') return [new TextRun({ break: 1 })]
  if (node.type !== 'text') return (node.content ?? []).flatMap(run)
  const textStyle = mark(node, 'textStyle')?.attrs
  const highlight = mark(node, 'highlight')?.attrs?.color?.toLocaleLowerCase()
  const options = {
    bold: Boolean(mark(node, 'bold')),
    italics: Boolean(mark(node, 'italic')),
    underline: mark(node, 'underline') ? {} : undefined,
    strike: Boolean(mark(node, 'strike')),
    subScript: Boolean(mark(node, 'subscript')),
    superScript: Boolean(mark(node, 'superscript')),
    font: supportedFont(textStyle?.fontFamily),
    size: fontSizePoints(textStyle?.fontSize) ? Math.round(fontSizePoints(textStyle?.fontSize)! * 2) : undefined,
    color: textStyle?.color?.replace('#', ''),
    highlight: highlight ? HIGHLIGHT_COLORS[highlight] : undefined,
  }
  const parts = (node.text ?? '').split('\t')
  return parts.flatMap((text, index) => [
    new TextRun({ ...options, text }),
    ...(index < parts.length - 1 ? [new TextRun({ ...options, children: [new Tab()] })] : []),
  ])
}
function alignment(value: unknown): (typeof AlignmentType)[keyof typeof AlignmentType] | undefined {
  return ({ left: AlignmentType.LEFT, center: AlignmentType.CENTER, right: AlignmentType.RIGHT, justify: AlignmentType.JUSTIFIED } as const)[String(value) as 'left']
}
function paragraph(node: EditorNode, options: IParagraphOptions = {}): Paragraph {
  const lineHeight = Number(node.attrs?.lineHeight)
  return new Paragraph({
    ...options,
    children: run(node),
    alignment: alignment(node.attrs?.textAlign),
    spacing: Number.isFinite(lineHeight) ? { line: Math.round(lineHeight * 240), lineRule: 'auto' } : undefined,
  })
}
function paragraphs(node: EditorNode, list?: { ordered: boolean; level: number }): Paragraph[] {
  if (node.type === 'bulletList' || node.type === 'orderedList') {
    return (node.content ?? []).flatMap((item) => paragraphs(item, { ordered: node.type === 'orderedList', level: list?.level ?? 0 }))
  }
  if (node.type === 'listItem') return (node.content ?? []).flatMap((item) => paragraphs(item, list))
  if (node.type === 'heading') {
    const levels = [HeadingLevel.HEADING_1, HeadingLevel.HEADING_2, HeadingLevel.HEADING_3, HeadingLevel.HEADING_4, HeadingLevel.HEADING_5, HeadingLevel.HEADING_6]
    return [paragraph(node, { heading: levels[Math.max(0, Math.min(5, (node.attrs?.level ?? 1) - 1))] })]
  }
  if (node.type === 'blockquote') return [paragraph(node, { indent: { left: 720 }, border: { left: { color: '888888', space: 6, style: 'single', size: 12 } } })]
  if (node.type === 'horizontalRule') return [new Paragraph({ border: { bottom: { color: '888888', style: 'single', size: 6 } } })]
  return [paragraph(node, list ? {
    bullet: list.ordered ? undefined : { level: list.level },
    numbering: list.ordered ? { reference: 'default-numbering', level: list.level } : undefined,
  } : {})]
}

export async function exportDocx(content: TiptapDocument): Promise<Blob> {
  const children = (content.content ?? []).flatMap((node) => paragraphs(node as EditorNode))
  return Packer.toBlob(new Document({
    styles: { default: { document: { run: { font: 'Arial', size: 24 } } } },
    sections: [{ children }],
    numbering: { config: [{ reference: 'default-numbering', levels: [{ level: 0, format: 'decimal', text: '%1.', alignment: 'left' }] }] },
  }))
}

export async function blobToBase64(blob: Blob): Promise<string> {
  const data = new Uint8Array(await blob.arrayBuffer()); let binary = ''
  for (const byte of data) binary += String.fromCharCode(byte)
  return btoa(binary)
}
