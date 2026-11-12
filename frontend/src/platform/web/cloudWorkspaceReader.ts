import type { PortableDocument } from '@/cloud/documentCodec'
import type { TiptapDocument } from '@/types/documents'
import type { ProjectNote } from '@/types/notes'
import type { BrowserCloudAccountSession } from './cloudAccountSession'
import type { NativeLegacyNote } from './localAuthorityRecords'

type CloudReader = Pick<BrowserCloudAccountSession, 'readCloudProject' | 'readRecoveryProject'>

export async function readCloudWorkspaceProject(reader: CloudReader, projectId: string, signal: AbortSignal) {
  const metadata = await reader.readCloudProject(projectId, signal, 'recovery').catch(() => null)
  signal.throwIfAborted()
  if (metadata?.empty) return { kind: 'empty' as const, metadata }
  const project = await reader.readRecoveryProject(projectId, signal)
  return { kind: 'project' as const, project }
}

export function cloudNoteCard(note: NativeLegacyNote): ProjectNote {
  return {
    ...note,
    display_title: note.title,
    system_tags: [],
    owner_type: note.stage_id === null ? 'project' : 'stage',
    owner_id: note.stage_id ?? note.project_id,
    owner_order: note.sort_order,
    stage_name: null,
    read_only: true,
  }
}

// Tiptap extensions serialize these values into inline styles. Keep only
// formatting values that cannot add CSS declarations or fetch external content.
const formatting: Record<string, RegExp> = {
  color: /^(?:#[0-9a-f]{3,8}|[a-z]+|rgba?\([\d.,% /]+\))$/i,
  fontFamily: /^[\p{L}\d ,'"-]{1,128}$/u,
  fontSize: /^\d+(?:\.\d+)?(?:px|pt|em|rem|%)$/,
  lineHeight: /^(?:normal|\d+(?:\.\d+)?(?:px|pt|em|rem|%)?)$/,
  textAlign: /^(?:left|right|center|justify)$/,
}

export function cloudDocumentForReading(document: PortableDocument): TiptapDocument {
  // The recovery owner has already checked the portable document schema.
  // Construct a display copy without link actions or arbitrary DOM attributes.
  function nodeForReading(node: Record<string, unknown>): Record<string, unknown> {
    const result: Record<string, unknown> = { type: node.type }
    if (typeof node.text === 'string') result.text = node.text
    if (Array.isArray(node.content)) result.content = node.content.map(nodeForReading)
    if (Array.isArray(node.marks)) result.marks = node.marks.filter(mark => mark.type !== 'link').map(nodeForReading)
    if (node.attrs && typeof node.attrs === 'object') {
      result.attrs = Object.fromEntries(Object.entries(node.attrs).filter(([key, value]) =>
        (key === 'level' || key === 'start') && Number.isSafeInteger(value)
        || typeof value === 'string' && Object.hasOwn(formatting, key) && formatting[key]!.test(value)))
    }
    return result
  }
  return nodeForReading(document.content_json) as TiptapDocument
}
