import type { JsonObject, JsonValue } from '@/types/notes'

/** Display copy only. Authenticated source/proofs retain the original map.
 * The bundled renderer uses HTML for link labels and allows external resources.
 * Cloud read-only maps expose neither active labels nor remote resources. */
export function safeCloudMapDisplay(data: JsonObject): JsonObject {
  const copy = JSON.parse(JSON.stringify(data)) as JsonObject
  const visit = (value: JsonValue): void => {
    if (Array.isArray(value)) { value.forEach(visit); return }
    if (!value || typeof value !== 'object') return
    for (const key of ['hyperLink', 'image', 'style', 'theme', 'branchColor']) delete value[key]
    if (typeof value.label === 'string') value.label = value.label.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;')
    Object.values(value).forEach(visit)
  }
  visit(copy)
  return copy
}

export interface MindMapInitialization {
  data: JsonObject | null
  editorLabel: string
  emptyStageMapText: string
  floatingNodeName: string
  floatingNoteName: string
  addFloatingNodeLabel: string
  addFloatingNoteLabel: string
  searchMapLabel: string
  searchPlaceholder: string
  nothingFoundText: string
  detachBranchLabel: string
  attachBranchLabel: string
  attachTargetPrompt: string
  locale: string
  loadingText: string
  newTopicName: string
  readOnly: boolean
  rootTopic: string
}

export interface MindMapBridgeApi {
  initialize(payload: MindMapInitialization): void
  getDataString(): string | null
  addFloatingItem(kind: 'node' | 'note'): JsonValue
  focusNode(nodeId: string): boolean
  updateNodeNote(nodeId: string, text: string): boolean
  removeNodeNote(nodeId: string): boolean
  saveNow(): void
  toCenter(): void
  takeEvents(): string
}

export type MindMapBridgeEvent =
  | { type: 'ready' | 'changed' }
  | { type: 'save'; payload: string }
  | { type: 'error' | 'status' | 'exportError'; details?: string; message?: string }
  | { type: 'export'; format?: string; data?: string }

interface MindMapFrameWindow extends Window {
  nfprogressMindMap?: MindMapBridgeApi
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function mindMapBridge(frame: HTMLIFrameElement | null): MindMapBridgeApi | null {
  return (frame?.contentWindow as MindMapFrameWindow | null)?.nfprogressMindMap ?? null
}

export function parseMindMapData(payload: unknown): JsonObject | null {
  if (typeof payload !== 'string') return null
  try {
    const parsed: unknown = JSON.parse(payload)
    return isRecord(parsed) ? (parsed as JsonObject) : null
  } catch {
    return null
  }
}

export function parseMindMapEvents(payload: unknown): MindMapBridgeEvent[] {
  if (typeof payload !== 'string') return []
  try {
    const parsed: unknown = JSON.parse(payload)
    if (!Array.isArray(parsed)) return []
    const events: MindMapBridgeEvent[] = []
    for (const value of parsed) {
      if (!isRecord(value) || typeof value.type !== 'string') continue
      if (value.type === 'ready' || value.type === 'changed') {
        events.push({ type: value.type })
      } else if (value.type === 'save' && typeof value.payload === 'string') {
        events.push({ type: 'save', payload: value.payload })
      } else if (value.type === 'error' || value.type === 'status' || value.type === 'exportError') {
        events.push({
          type: value.type,
          details: typeof value.details === 'string' ? value.details : undefined,
          message: typeof value.message === 'string' ? value.message : undefined,
        })
      } else if (value.type === 'export') {
        events.push({
          type: 'export',
          format: typeof value.format === 'string' ? value.format : undefined,
          data: typeof value.data === 'string' ? value.data : undefined,
        })
      }
    }
    return events
  } catch {
    return []
  }
}
