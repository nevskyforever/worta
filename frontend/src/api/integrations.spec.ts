import { invoke } from '@tauri-apps/api/core'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { currentPlatform } from '@/platform/runtime'
import { integrationsApi } from './integrations'
import { settingsApi } from './settings'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@/platform/runtime', async (importOriginal) => {
  const runtime = await importOriginal<typeof import('@/platform/runtime')>()
  return { ...runtime, currentPlatform: vi.fn(runtime.currentPlatform) }
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.mocked(currentPlatform).mockReset()
})

function jsonResponse(payload: unknown): Response {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  })
}

describe('settings and document integration API clients', () => {
  it('sends only the backend settings patch envelope', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        values: { language: 'fr' },
        platform: 'web',
        capabilities: {
          local_file_sync: false,
          background_file_sync: false,
          native_updates: false,
          remote_api: true,
        },
        editable_keys: ['language'],
      }),
    )
    vi.stubGlobal('fetch', fetchMock)

    await settingsApi.update({ language: 'fr' })

    const [url, options] = fetchMock.mock.calls[0] as [string, RequestInit]
    expect(url).toBe('/api/settings')
    expect(options.method).toBe('PATCH')
    expect(JSON.parse(String(options.body))).toEqual({ values: { language: 'fr' } })
  })

  it('encodes stage sync targets instead of concatenating raw identifiers', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        project_id: 'project/one',
        stage_id: 'stage & two',
        configured: false,
        type: null,
        path: null,
        item_id: null,
        last_synced_at: null,
        desktop_only: true,
      }),
    )
    vi.stubGlobal('fetch', fetchMock)

    await integrationsApi.getSync('project/one', 'stage & two')

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      '/api/projects/project%2Fone/sync?stage_id=stage+%26+two',
    )
  })

  it('uploads a selected Word file as multipart without overriding its boundary', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ symbols: 42 }))
    vi.stubGlobal('fetch', fetchMock)
    const file = new File(['document'], 'chapter.docx', {
      type: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    })

    await integrationsApi.countWord(file)

    const [, options] = fetchMock.mock.calls[0] as [string, RequestInit]
    expect(options.body).toBeInstanceOf(FormData)
    expect((options.body as FormData).get('file')).toBeInstanceOf(File)
    expect(new Headers(options.headers).has('Content-Type')).toBe(false)
  })

  it('targets an uploaded Word total at a stable project and stage identifier', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({ changed: false, symbols: 42, project: {}, progress: null }),
    )
    vi.stubGlobal('fetch', fetchMock)

    await integrationsApi.importWord('project/one', new File(['docx'], 'chapter.docx'), 'draft 1')

    const [url, options] = fetchMock.mock.calls[0] as [string, RequestInit]
    expect(url).toBe('/api/projects/project%2Fone/imports/word?stage_id=draft+1')
    expect(options.method).toBe('POST')
    expect(options.body).toBeInstanceOf(FormData)
  })

  it('runs the isolated desktop sync batch through one API command', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({ checked: 2, changed: 1, failed: 1, items: [] }),
    )
    vi.stubGlobal('fetch', fetchMock)

    const result = await integrationsApi.runAllSync()

    const [url, options] = fetchMock.mock.calls[0] as [string, RequestInit]
    expect(url).toBe('/api/integrations/sync/run-all')
    expect(options.method).toBe('POST')
    expect(result).toMatchObject({ checked: 2, changed: 1, failed: 1 })
  })

  it('lists and runs every existing binding of one staged project', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(jsonResponse({ project_id: 'project/one', syncs: [] }))
      .mockResolvedValueOnce(jsonResponse({ checked: 1, changed: 1, failed: 0, items: [] }))
    vi.stubGlobal('fetch', fetchMock)

    await integrationsApi.getProjectSyncs('project/one')
    await integrationsApi.runProjectSyncs('project/one')

    expect(fetchMock.mock.calls[0]?.[0]).toBe('/api/projects/project%2Fone/sync/all')
    expect(fetchMock.mock.calls[1]?.[0]).toBe('/api/projects/project%2Fone/sync/run-all')
    expect((fetchMock.mock.calls[1]?.[1] as RequestInit).method).toBe('POST')
  })
})

it('passes Scrivener inspection and source settings as native commands', async () => {
  vi.mocked(currentPlatform).mockReturnValue('tauri')
  const path = '/tmp/Book.scriv'
  const items = [{ id: 'draft', title: 'Draft', children: [{ id: 'chapter', title: 'Chapter', children: [] }] }]
  vi.mocked(invoke).mockResolvedValueOnce(items).mockResolvedValue({})

  expect(await integrationsApi.inspectScrivener(path)).toEqual(items)
  expect(invoke).toHaveBeenLastCalledWith('inspect_scrivener', { command: { path } })

  await integrationsApi.getSync('book', 'stage')
  expect(invoke).toHaveBeenLastCalledWith('get_document_sync', {
    command: { projectId: 'book', stageId: 'stage' },
  })

  await integrationsApi.configureSync('book', { type: 'scrivener', path, stage_id: 'stage', item_id: 'chapter' })
  expect(invoke).toHaveBeenLastCalledWith('configure_document_sync', {
    command: { projectId: 'book', stageId: 'stage', type: 'scrivener', path, itemId: 'chapter' },
  })

  await integrationsApi.removeSync('book', 'stage')
  expect(invoke).toHaveBeenLastCalledWith('remove_document_sync', {
    command: { projectId: 'book', stageId: 'stage' },
  })
})
