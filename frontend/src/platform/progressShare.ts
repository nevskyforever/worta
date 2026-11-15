export const PROGRESS_SHARE_IMAGE_SIZE = 1080

const RING_CENTER_X = PROGRESS_SHARE_IMAGE_SIZE / 2
const RING_CENTER_Y = 435
const RING_OUTER_DIAMETER = 620
const RING_WIDTH = 24
const RING_RADIUS = RING_OUTER_DIAMETER / 2 - RING_WIDTH / 2
const TITLE_AREA = { x: 80, y: 805, width: PROGRESS_SHARE_IMAGE_SIZE - 160, height: 157 }
const BRAND_CENTER_Y = PROGRESS_SHARE_IMAGE_SIZE - 54
const BRAND_ICON_SIZE = 46
const BRAND_SPACING = 14
const BRAND_TEXT = 'worta'
const BRAND_ICON_URL = '/icons/icon-192.webp'

const START_COLOR = [169, 169, 169] as const
const END_COLOR = [37, 104, 172] as const

export interface ProgressSharePayload {
  title: string
  progress: number
  coverImage?: string | null
  statusLabel?: string
  progressText?: string
  footerLabel?: string
  footerDetail?: string
  theme?: 'light' | 'dark'
}

interface TextLayout {
  fontSize: number
  lineHeight: number
  lines: string[]
}

interface ClipboardItemConstructor {
  new (items: Record<string, Blob | Promise<Blob>>): ClipboardItem
}

function canvasContext(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  const context = canvas.getContext('2d')
  if (!context) throw new Error('Canvas rendering is unavailable.')
  return context
}

export function normalizeProgressSharePercent(value: number): number {
  if (!Number.isFinite(value)) return 0
  return Math.min(100, Math.max(0, Math.round(value)))
}

export function progressShareColor(progress: number): string {
  const ratio = normalizeProgressSharePercent(progress) / 100
  const channels = START_COLOR.map((start, index) => {
    const end = END_COLOR[index] ?? start
    return Math.trunc(start + ratio * (end - start))
  })
  return `rgb(${channels.join(', ')})`
}

export function progressShareTitle(name: string, parentName?: string): string {
  const normalizedName = name.trim() || BRAND_TEXT
  const normalizedParentName = parentName?.trim()
  return normalizedParentName ? `${normalizedParentName}: ${normalizedName}` : normalizedName
}

function titleMaxFontSize(title: string): number {
  const length = title.trim().length
  if (length <= 1) return 155
  if (length <= 4) return 139
  if (length <= 14) return 123
  if (length <= 28) return 104
  return 85
}

function splitLongWord(
  context: CanvasRenderingContext2D,
  word: string,
  maximumWidth: number,
): string[] {
  const chunks: string[] = []
  let chunk = ''

  for (const character of Array.from(word)) {
    const candidate = `${chunk}${character}`
    if (chunk && context.measureText(candidate).width > maximumWidth) {
      chunks.push(chunk)
      chunk = character
    } else {
      chunk = candidate
    }
  }
  if (chunk) chunks.push(chunk)
  return chunks
}

function wrapTitle(
  context: CanvasRenderingContext2D,
  title: string,
  maximumWidth: number,
): string[] {
  const words = title.trim().split(/\s+/).filter(Boolean)
  if (!words.length) return [BRAND_TEXT]

  const lines: string[] = []
  let line = ''
  for (const word of words) {
    const candidates = context.measureText(word).width > maximumWidth
      ? splitLongWord(context, word, maximumWidth)
      : [word]
    for (const candidate of candidates) {
      const combined = line ? `${line} ${candidate}` : candidate
      if (line && context.measureText(combined).width > maximumWidth) {
        lines.push(line)
        line = candidate
      } else {
        line = combined
      }
    }
  }
  if (line) lines.push(line)
  return lines
}

function ellipsizeLine(
  context: CanvasRenderingContext2D,
  line: string,
  maximumWidth: number,
): string {
  const suffix = '…'
  if (context.measureText(line).width <= maximumWidth) return line
  let shortened = line
  while (shortened && context.measureText(`${shortened}${suffix}`).width > maximumWidth) {
    shortened = Array.from(shortened).slice(0, -1).join('')
  }
  return shortened ? `${shortened}${suffix}` : suffix
}

function fitTitle(context: CanvasRenderingContext2D, title: string): TextLayout {
  const maximumFontSize = titleMaxFontSize(title)
  for (let fontSize = maximumFontSize; fontSize >= 27; fontSize -= 2) {
    const lineHeight = Math.ceil(fontSize * 1.16)
    context.font = `700 ${fontSize}px Arial, sans-serif`
    const lines = wrapTitle(context, title, TITLE_AREA.width)
    if (lines.length * lineHeight <= TITLE_AREA.height) {
      return { fontSize, lineHeight, lines }
    }
  }

  const fontSize = 27
  const lineHeight = Math.ceil(fontSize * 1.16)
  const maximumLines = Math.max(1, Math.floor(TITLE_AREA.height / lineHeight))
  context.font = `700 ${fontSize}px Arial, sans-serif`
  const lines = wrapTitle(context, title, TITLE_AREA.width)
  if (lines.length <= maximumLines) return { fontSize, lineHeight, lines }

  const visibleLines = lines.slice(0, maximumLines)
  const lastIndex = visibleLines.length - 1
  const lastLine = visibleLines[lastIndex]
  if (lastLine !== undefined) {
    visibleLines[lastIndex] = ellipsizeLine(context, `${lastLine} ${lines.slice(maximumLines).join(' ')}`, TITLE_AREA.width)
  }
  return { fontSize, lineHeight, lines: visibleLines }
}

function drawProgressRing(context: CanvasRenderingContext2D, progress: number): void {
  context.lineWidth = RING_WIDTH
  context.lineCap = 'round'
  context.strokeStyle = '#E6EBEF'
  context.beginPath()
  context.arc(RING_CENTER_X, RING_CENTER_Y, RING_RADIUS, 0, Math.PI * 2)
  context.stroke()

  if (!progress) return
  context.strokeStyle = progressShareColor(progress)
  context.beginPath()
  context.arc(
    RING_CENTER_X,
    RING_CENTER_Y,
    RING_RADIUS,
    -Math.PI / 2,
    -Math.PI / 2 + (progress / 100) * Math.PI * 2,
  )
  context.stroke()
}

function drawTitle(context: CanvasRenderingContext2D, title: string): void {
  const layout = fitTitle(context, title)
  const totalHeight = layout.lines.length * layout.lineHeight
  let centerY = TITLE_AREA.y + (TITLE_AREA.height - totalHeight) / 2 + layout.lineHeight / 2
  context.fillStyle = '#000000'
  context.font = `700 ${layout.fontSize}px Arial, sans-serif`
  context.textAlign = 'center'
  context.textBaseline = 'middle'
  for (const line of layout.lines) {
    context.fillText(line, PROGRESS_SHARE_IMAGE_SIZE / 2, centerY)
    centerY += layout.lineHeight
  }
}

function roundedRectangle(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
): void {
  const boundedRadius = Math.min(radius, width / 2, height / 2)
  context.moveTo(x + boundedRadius, y)
  context.lineTo(x + width - boundedRadius, y)
  context.quadraticCurveTo(x + width, y, x + width, y + boundedRadius)
  context.lineTo(x + width, y + height - boundedRadius)
  context.quadraticCurveTo(x + width, y + height, x + width - boundedRadius, y + height)
  context.lineTo(x + boundedRadius, y + height)
  context.quadraticCurveTo(x, y + height, x, y + height - boundedRadius)
  context.lineTo(x, y + boundedRadius)
  context.quadraticCurveTo(x, y, x + boundedRadius, y)
  context.closePath()
}

function drawBrand(context: CanvasRenderingContext2D, icon: CanvasImageSource | null = null): void {
  context.font = '700 37px Arial, sans-serif'
  const textWidth = context.measureText(BRAND_TEXT).width
  const groupWidth = BRAND_ICON_SIZE + BRAND_SPACING + textWidth
  const groupX = (PROGRESS_SHARE_IMAGE_SIZE - groupWidth) / 2
  const iconY = BRAND_CENTER_Y - BRAND_ICON_SIZE / 2

  if (icon) {
    context.drawImage(icon, groupX, iconY, BRAND_ICON_SIZE, BRAND_ICON_SIZE)
  } else {
    context.fillStyle = '#2568AC'
    context.beginPath()
    roundedRectangle(context, groupX, iconY, BRAND_ICON_SIZE, BRAND_ICON_SIZE, 12)
    context.fill()
    context.fillStyle = '#FFFFFF'
    context.font = '700 24px Arial, sans-serif'
    context.textAlign = 'center'
    context.textBaseline = 'middle'
    context.fillText('nf', groupX + BRAND_ICON_SIZE / 2, BRAND_CENTER_Y + 1)
  }

  context.fillStyle = '#2E78B8'
  context.font = '700 37px Arial, sans-serif'
  context.textAlign = 'left'
  context.fillText(BRAND_TEXT, groupX + BRAND_ICON_SIZE + BRAND_SPACING, BRAND_CENTER_Y)
}

function drawCoverImage(
  context: CanvasRenderingContext2D,
  cover: HTMLImageElement,
  x: number,
  y: number,
  width: number,
  height: number,
): void {
  const sourceWidth = cover.naturalWidth || cover.width
  const sourceHeight = cover.naturalHeight || cover.height
  if (!sourceWidth || !sourceHeight) return

  const sourceRatio = sourceWidth / sourceHeight
  const targetRatio = width / height
  let sourceX = 0
  let sourceY = 0
  let croppedWidth = sourceWidth
  let croppedHeight = sourceHeight
  if (sourceRatio > targetRatio) {
    croppedWidth = sourceHeight * targetRatio
    sourceX = (sourceWidth - croppedWidth) / 2
  } else {
    croppedHeight = sourceWidth / targetRatio
    sourceY = (sourceHeight - croppedHeight) / 2
  }
  context.drawImage(cover, sourceX, sourceY, croppedWidth, croppedHeight, x, y, width, height)
}

function drawCoveredProjectCard(
  context: CanvasRenderingContext2D,
  payload: ProgressSharePayload,
  cover: HTMLImageElement,
  icon: CanvasImageSource | null,
): void {
  const colors = projectCardColors(payload.theme)
  const progress = normalizeProgressSharePercent(payload.progress)
  const card = { x: 340, y: 30, width: 400, height: 900, radius: 32 }
  const coverHeight = 600
  const contentY = card.y + coverHeight
  const title = progressShareTitle(payload.title)

  context.fillStyle = colors.canvas
  context.fillRect(0, 0, PROGRESS_SHARE_IMAGE_SIZE, PROGRESS_SHARE_IMAGE_SIZE)
  context.save()
  context.shadowColor = colors.shadow
  context.shadowBlur = 28
  context.shadowOffsetY = 12
  context.fillStyle = colors.surface
  context.beginPath()
  roundedRectangle(context, card.x, card.y, card.width, card.height, card.radius)
  context.fill()
  context.restore()

  context.save()
  context.beginPath()
  roundedRectangle(context, card.x, card.y, card.width, card.height, card.radius)
  context.clip()
  context.save()
  drawCoverImage(context, cover, card.x, card.y, card.width, coverHeight)
  context.restore()

  context.fillStyle = colors.surface
  context.fillRect(card.x, contentY, card.width, card.height - coverHeight)

  context.fillStyle = colors.primarySoft
  context.beginPath()
  roundedRectangle(context, card.x + 30, contentY + 28, 98, 34, 17)
  context.fill()
  context.fillStyle = colors.primary
  context.font = '700 17px Arial, sans-serif'
  context.textBaseline = 'middle'
  context.fillText(payload.statusLabel || 'Активен', card.x + 43, contentY + 45)

  context.fillStyle = colors.text
  context.font = '500 31px Arial, sans-serif'
  context.textAlign = 'left'
  context.textBaseline = 'alphabetic'
  const layout = fitTitleInArea(context, title, { x: card.x + 30, y: contentY + 102, width: 330, height: 70 }, 31)
  let titleY = contentY + 102 + layout.lineHeight
  for (const line of layout.lines) {
    context.fillText(line, card.x + 30, titleY)
    titleY += layout.lineHeight
  }

  context.fillStyle = colors.textMuted
  context.font = '500 17px Arial, sans-serif'
  context.fillText(payload.progressText || `${progress}%`, card.x + 30, contentY + 198)

  const bar = { x: card.x + 30, y: contentY + 218, width: card.width - 60, height: 14 }
  context.fillStyle = colors.surfaceMuted
  context.beginPath()
  roundedRectangle(context, bar.x, bar.y, bar.width, bar.height, bar.height / 2)
  context.fill()
  if (progress) {
    context.fillStyle = colors.accent
    context.beginPath()
    roundedRectangle(context, bar.x, bar.y, Math.max(bar.height, bar.width * progress / 100), bar.height, bar.height / 2)
    context.fill()
  }

  context.fillStyle = colors.textMuted
  context.font = '500 16px Arial, sans-serif'
  context.fillText(payload.footerLabel || 'Без срока', card.x + 30, contentY + 276)
  if (payload.footerDetail) {
    context.textAlign = 'right'
    context.fillText(payload.footerDetail, card.x + card.width - 30, contentY + 276)
  }
  context.restore()

  context.strokeStyle = colors.border
  context.lineWidth = 1
  context.beginPath()
  roundedRectangle(context, card.x + .5, card.y + .5, card.width - 1, card.height - 1, card.radius)
  context.stroke()

  drawCoveredBrand(context, icon, colors.brand)
}

interface ProjectCardColors {
  canvas: string
  surface: string
  surfaceMuted: string
  text: string
  textMuted: string
  border: string
  primary: string
  primarySoft: string
  accent: string
  brand: string
  shadow: string
}

function projectCardColors(theme: ProgressSharePayload['theme']): ProjectCardColors {
  return theme === 'dark'
    ? {
        canvas: '#202124', surface: '#292B30', surfaceMuted: '#383B42', text: '#F1F3F4',
        textMuted: '#BDC1C6', border: '#5F6368', primary: '#8DA2FB', primarySoft: '#303D7D',
        accent: '#AAB8FF', brand: '#F1F3F4', shadow: 'rgb(0 0 0 / 20%)',
      }
    : {
        canvas: '#F5F5F7', surface: '#FFFFFF', surfaceMuted: '#F0F1F4', text: '#202124',
        textMuted: '#687078', border: '#C7C9CE', primary: '#4263EB', primarySoft: '#E8ECFF',
        accent: '#4263EB', brand: '#2E78B8', shadow: 'rgb(0 0 0 / 15%)',
      }
}

function drawCoveredBrand(
  context: CanvasRenderingContext2D,
  icon: CanvasImageSource | null,
  color: string,
  brandY = 1025,
): void {
  context.font = '700 37px Arial, sans-serif'
  const textWidth = context.measureText(BRAND_TEXT).width
  const groupWidth = BRAND_ICON_SIZE + BRAND_SPACING + textWidth
  const groupX = (PROGRESS_SHARE_IMAGE_SIZE - groupWidth) / 2
  if (icon) context.drawImage(icon, groupX, brandY - BRAND_ICON_SIZE / 2, BRAND_ICON_SIZE, BRAND_ICON_SIZE)
  context.fillStyle = color
  context.textAlign = 'left'
  context.textBaseline = 'middle'
  context.fillText(BRAND_TEXT, groupX + BRAND_ICON_SIZE + BRAND_SPACING, brandY)
}

function drawUncoveredProjectCard(
  context: CanvasRenderingContext2D,
  payload: ProgressSharePayload,
  icon: CanvasImageSource | null,
): void {
  const colors = projectCardColors(payload.theme)
  const progress = normalizeProgressSharePercent(payload.progress)
  const card = { x: 120, y: 270, width: 840, height: 390, radius: 34 }
  const title = progressShareTitle(payload.title)

  context.fillStyle = colors.canvas
  context.fillRect(0, 0, PROGRESS_SHARE_IMAGE_SIZE, PROGRESS_SHARE_IMAGE_SIZE)
  context.save()
  context.shadowColor = colors.shadow
  context.shadowBlur = 28
  context.shadowOffsetY = 12
  context.fillStyle = colors.surface
  context.beginPath()
  roundedRectangle(context, card.x, card.y, card.width, card.height, card.radius)
  context.fill()
  context.restore()

  context.save()
  context.beginPath()
  roundedRectangle(context, card.x, card.y, card.width, card.height, card.radius)
  context.clip()

  context.fillStyle = colors.primarySoft
  context.beginPath()
  roundedRectangle(context, card.x + 34, card.y + 30, 108, 36, 18)
  context.fill()
  context.fillStyle = colors.primary
  context.font = '700 18px Arial, sans-serif'
  context.textAlign = 'left'
  context.textBaseline = 'middle'
  context.fillText(payload.statusLabel || 'Активен', card.x + 48, card.y + 48)

  const ringX = card.x + 150
  const ringY = card.y + 195
  const ringRadius = 77
  context.lineWidth = 16
  context.lineCap = 'round'
  context.strokeStyle = colors.surfaceMuted
  context.beginPath()
  context.arc(ringX, ringY, ringRadius, 0, Math.PI * 2)
  context.stroke()
  if (progress) {
    context.strokeStyle = colors.accent
    context.beginPath()
    context.arc(ringX, ringY, ringRadius, -Math.PI / 2, -Math.PI / 2 + progress / 100 * Math.PI * 2)
    context.stroke()
  }
  context.fillStyle = colors.text
  context.font = '700 28px Arial, sans-serif'
  context.textAlign = 'center'
  context.fillText(`${progress}%`, ringX, ringY + 1)

  const contentX = card.x + 285
  context.textAlign = 'left'
  context.textBaseline = 'alphabetic'
  context.fillStyle = colors.text
  const layout = fitTitleInArea(context, title, { x: contentX, y: card.y + 100, width: 475, height: 76 }, 38)
  let titleY = card.y + 100 + layout.lineHeight
  for (const line of layout.lines) {
    context.fillText(line, contentX, titleY)
    titleY += layout.lineHeight
  }
  context.fillStyle = colors.textMuted
  context.font = '500 19px Arial, sans-serif'
  context.fillText(payload.progressText || `${progress}%`, contentX, card.y + 210)
  const bar = { x: contentX, y: card.y + 230, width: 475, height: 14 }
  context.fillStyle = colors.surfaceMuted
  context.beginPath()
  roundedRectangle(context, bar.x, bar.y, bar.width, bar.height, bar.height / 2)
  context.fill()
  if (progress) {
    context.fillStyle = colors.accent
    context.beginPath()
    roundedRectangle(context, bar.x, bar.y, Math.max(bar.height, bar.width * progress / 100), bar.height, bar.height / 2)
    context.fill()
  }

  context.fillStyle = colors.textMuted
  context.font = '500 16px Arial, sans-serif'
  context.fillText(payload.footerLabel || 'Без срока', card.x + 34, card.y + 340)
  if (payload.footerDetail) {
    context.textAlign = 'right'
    context.fillText(payload.footerDetail, card.x + card.width - 34, card.y + 340)
  }
  context.restore()

  context.strokeStyle = colors.border
  context.lineWidth = 1
  context.beginPath()
  roundedRectangle(context, card.x + .5, card.y + .5, card.width - 1, card.height - 1, card.radius)
  context.stroke()
  drawCoveredBrand(context, icon, colors.brand, 770)
}

function fitTitleInArea(
  context: CanvasRenderingContext2D,
  title: string,
  area: { x: number, y: number, width: number, height: number },
  maximumFontSize: number,
): TextLayout {
  for (let fontSize = maximumFontSize; fontSize >= 26; fontSize -= 2) {
    const lineHeight = Math.ceil(fontSize * 1.16)
    context.font = `500 ${fontSize}px Arial, sans-serif`
    const lines = wrapTitle(context, title, area.width)
    if (lines.length * lineHeight <= area.height) return { fontSize, lineHeight, lines }
  }
  context.font = '500 26px Arial, sans-serif'
  return { fontSize: 26, lineHeight: 31, lines: [ellipsizeLine(context, title, area.width)] }
}

export function drawProgressShareImage(
  context: CanvasRenderingContext2D,
  payload: ProgressSharePayload,
  icon: CanvasImageSource | null = null,
): void {
  const progress = normalizeProgressSharePercent(payload.progress)
  const title = progressShareTitle(payload.title)

  context.fillStyle = '#FFFFFF'
  context.fillRect(0, 0, PROGRESS_SHARE_IMAGE_SIZE, PROGRESS_SHARE_IMAGE_SIZE)
  drawProgressRing(context, progress)

  context.fillStyle = progressShareColor(progress)
  context.font = '700 210px Arial, sans-serif'
  context.textAlign = 'center'
  context.textBaseline = 'middle'
  context.fillText(`${progress}%`, RING_CENTER_X, RING_CENTER_Y)

  drawTitle(context, title)
  drawBrand(context, icon)
}

function loadImage(source: string): Promise<HTMLImageElement | null> {
  if (typeof Image === 'undefined') return Promise.resolve(null)
  return new Promise((resolve) => {
    const image = new Image()
    image.onload = () => resolve(image)
    image.onerror = () => resolve(null)
    image.src = source
  })
}

function canvasBlob(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob) {
        resolve(blob)
      } else {
        reject(new Error('PNG generation failed.'))
      }
    }, 'image/png')
  })
}

export async function createProgressShareImage(payload: ProgressSharePayload): Promise<Blob> {
  if (typeof document === 'undefined') throw new Error('Canvas rendering is unavailable.')
  const canvas = document.createElement('canvas')
  canvas.width = PROGRESS_SHARE_IMAGE_SIZE
  canvas.height = PROGRESS_SHARE_IMAGE_SIZE
  const [icon, cover] = await Promise.all([
    loadImage(BRAND_ICON_URL),
    payload.coverImage ? loadImage(payload.coverImage) : Promise.resolve(null),
  ])
  const context = canvasContext(canvas)
  if (cover) drawCoveredProjectCard(context, payload, cover, icon)
  else drawUncoveredProjectCard(context, payload, icon)
  return canvasBlob(canvas)
}

function clipboardItemConstructor(): ClipboardItemConstructor | undefined {
  return (globalThis as typeof globalThis & { ClipboardItem?: ClipboardItemConstructor }).ClipboardItem
}

async function copyImageToClipboard(blob: Blob): Promise<boolean> {
  if (document.documentElement.dataset.platform === 'tauri') {
    try {
      const { writeImage } = await import('@tauri-apps/plugin-clipboard-manager')
      await writeImage(new Uint8Array(await blob.arrayBuffer()))
      return true
    } catch {
      return false
    }
  }
  const ClipboardImageItem = clipboardItemConstructor()
  if (
    typeof navigator === 'undefined'
    || !navigator.clipboard
    || typeof navigator.clipboard.write !== 'function'
    || !ClipboardImageItem
  ) {
    return false
  }
  try {
    await navigator.clipboard.write([
      new ClipboardImageItem({ [blob.type || 'image/png']: blob }),
    ])
    return true
  } catch {
    return false
  }
}

function downloadImage(blob: Blob): void {
  if (typeof document === 'undefined' || typeof URL.createObjectURL !== 'function') {
    throw new Error('PNG download is unavailable.')
  }
  const url = URL.createObjectURL(blob)
  const revokeObjectURL = typeof URL.revokeObjectURL === 'function'
    ? URL.revokeObjectURL
    : undefined
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = 'worta-progress.png'
  anchor.rel = 'noopener'
  anchor.style.display = 'none'
  document.body.append(anchor)
  anchor.click()
  anchor.remove()
  if (revokeObjectURL) window.setTimeout(() => revokeObjectURL(url), 0)
}

export async function copyProgressImage(payload: ProgressSharePayload): Promise<void> {
  if (document.documentElement.dataset.platform === 'tauri') {
    const image = await createProgressShareImage(payload)
    if (!await copyImageToClipboard(image)) {
      throw new Error('Image clipboard is unavailable.')
    }
    return
  }

  const ClipboardImageItem = clipboardItemConstructor()
  if (
    typeof navigator === 'undefined'
    || !navigator.clipboard
    || typeof navigator.clipboard.write !== 'function'
    || !ClipboardImageItem
  ) {
    throw new Error('Image clipboard is unavailable.')
  }

  try {
    await navigator.clipboard.write([
      new ClipboardImageItem({ 'image/png': createProgressShareImage(payload) }),
    ])
  } catch {
    throw new Error('Image clipboard is unavailable.')
  }
}

export async function downloadProgressImage(payload: ProgressSharePayload): Promise<void> {
  downloadImage(await createProgressShareImage(payload))
}
