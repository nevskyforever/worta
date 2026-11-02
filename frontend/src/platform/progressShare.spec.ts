import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  drawProgressShareImage,
  createProgressShareImage,
  copyProgressImage,
  downloadProgressImage,
  normalizeProgressSharePercent,
  progressShareColor,
  progressShareTitle,
} from './progressShare'

const initialClipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, 'clipboard')

function drawingContext(): CanvasRenderingContext2D {
  return {
    arc: vi.fn(),
    beginPath: vi.fn(),
    closePath: vi.fn(),
    drawImage: vi.fn(),
    fill: vi.fn(),
    fillRect: vi.fn(),
    fillText: vi.fn(),
    lineTo: vi.fn(),
    measureText: vi.fn((text: string) => ({ width: text.length * 18 })),
    moveTo: vi.fn(),
    quadraticCurveTo: vi.fn(),
    clip: vi.fn(),
    restore: vi.fn(),
    save: vi.fn(),
    stroke: vi.fn(),
  } as unknown as CanvasRenderingContext2D
}

function installBrandImage(): void {
  class ImageMock {
    onload: (() => void) | null = null
    onerror: (() => void) | null = null
    naturalWidth = 600
    naturalHeight = 900
    width = 600
    height = 900

    set src(_value: string) {
      queueMicrotask(() => this.onload?.())
    }
  }
  vi.stubGlobal('Image', ImageMock)
}

describe('progressShare', () => {
  afterEach(() => {
    delete document.documentElement.dataset.platform
    if (initialClipboardDescriptor) {
      Object.defineProperty(navigator, 'clipboard', initialClipboardDescriptor)
    } else {
      Reflect.deleteProperty(navigator, 'clipboard')
    }
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('keeps the legacy rounded percentage and parent-stage title rules', () => {
    expect(normalizeProgressSharePercent(-1)).toBe(0)
    expect(normalizeProgressSharePercent(42.6)).toBe(43)
    expect(normalizeProgressSharePercent(101)).toBe(100)
    expect(progressShareColor(0)).toBe('rgb(169, 169, 169)')
    expect(progressShareColor(50)).toBe('rgb(103, 136, 170)')
    expect(progressShareColor(100)).toBe('rgb(37, 104, 172)')
    expect(progressShareTitle('Глава 3', 'Роман')).toBe('Роман: Глава 3')
  })

  it('draws the publication card with the ring, percentage, title, and brand', () => {
    const context = drawingContext()

    drawProgressShareImage(context, { title: 'Дом у моря', progress: 42.6 })

    expect(context.fillRect).toHaveBeenCalledWith(0, 0, 1080, 1080)
    expect(context.arc).toHaveBeenCalledTimes(2)
    expect(context.fillText).toHaveBeenCalledWith('43%', 540, 435)
    expect(context.fillText).toHaveBeenCalledWith('nf', expect.any(Number), 1027)
    expect(context.fillText).toHaveBeenCalledWith('worta', expect.any(Number), 1026)
  })

  it('starts the browser clipboard write before the PNG is generated', async () => {
    const context = drawingContext()
    const image = new Blob(['png'], { type: 'image/png' })
    const write = vi.fn().mockResolvedValue(undefined)
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(context)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(image))
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { write },
    })
    class ClipboardItemMock {
      constructor(readonly items: Record<string, Blob | Promise<Blob>>) {}
    }
    vi.stubGlobal('ClipboardItem', ClipboardItemMock)
    installBrandImage()

    const copying = copyProgressImage({ title: 'Дом у моря', progress: 25 })

    expect(write).toHaveBeenCalledTimes(1)
    expect(write.mock.calls[0]?.[0]).toHaveLength(1)
    const clipboardItem = write.mock.calls[0]?.[0]?.[0] as ClipboardItemMock
    expect(clipboardItem.items['image/png']).toBeInstanceOf(Promise)

    await expect(copying).resolves.toBeUndefined()
  })

  it('renders a covered project as a project card and retains its stage title', async () => {
    const context = drawingContext()
    const image = new Blob(['png'], { type: 'image/png' })
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(context)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(image))
    installBrandImage()

    await expect(createProgressShareImage({
      title: 'Роман: Глава 3',
      progress: 25,
      coverImage: 'data:image/png;base64,cover',
    })).resolves.toBe(image)

    expect(context.clip).toHaveBeenCalledTimes(1)
    expect(context.drawImage).toHaveBeenCalled()
    expect(context.fillText).toHaveBeenCalledWith('Роман: Глава 3', 370, expect.any(Number))
    expect(context.fillText).toHaveBeenCalledWith('worta', expect.any(Number), 1025)
  })

  it('uses the native clipboard in the desktop application', async () => {
    const context = drawingContext()
    const image = new Blob(['png'], { type: 'image/png' })
    const writeImage = vi.fn().mockResolvedValue(undefined)
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(context)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(image))
    document.documentElement.dataset.platform = 'tauri'
    vi.doMock('@tauri-apps/plugin-clipboard-manager', () => ({ writeImage }))
    installBrandImage()

    await expect(copyProgressImage({ title: 'Дом у моря', progress: 25 })).resolves.toBeUndefined()

    expect(writeImage).toHaveBeenCalledWith(expect.any(Uint8Array))
  })

  it('saves the PNG only after the explicit save action', async () => {
    const context = drawingContext()
    const image = new Blob(['png'], { type: 'image/png' })
    const createObjectURL = vi.fn(() => 'blob:nfprogress-progress')
    const revokeObjectURL = vi.fn()
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(context)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(image))
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: undefined,
    })
    vi.stubGlobal('URL', { createObjectURL, revokeObjectURL })
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {})
    installBrandImage()

    await expect(downloadProgressImage({ title: 'Дом у моря', progress: 25 })).resolves.toBeUndefined()

    expect(createObjectURL).toHaveBeenCalledWith(image)
    expect(click).toHaveBeenCalledTimes(1)
  })

  it('does not silently download when image clipboard access is unavailable', async () => {
    const context = drawingContext()
    const image = new Blob(['png'], { type: 'image/png' })
    const createObjectURL = vi.fn(() => 'blob:nfprogress-progress')
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(context)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(image))
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: undefined,
    })
    vi.stubGlobal('URL', { createObjectURL })
    installBrandImage()

    await expect(copyProgressImage({ title: 'Дом у моря', progress: 25 })).rejects.toThrow(
      'Image clipboard is unavailable.',
    )
    expect(createObjectURL).not.toHaveBeenCalled()
  })
})
