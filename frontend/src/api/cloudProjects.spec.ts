import { afterEach, describe, expect, it, vi } from 'vitest'

import { cloudProjectsApi } from './cloudProjects'

afterEach(() => vi.restoreAllMocks())

describe('cloud project bootstrap transport', () => {
  it('lists the server catalog with authenticated metadata-only GET and forwards cancellation', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(async () => new Response(JSON.stringify({
      cloud_project_ids: ['server-project'], cloud_project_count: 1, max_cloud_projects: 8,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    const controller = new AbortController()
    await expect(cloudProjectsApi.list('private-access-token', controller.signal)).resolves.toEqual({
      cloud_project_ids: ['server-project'], cloud_project_count: 1, max_cloud_projects: 8,
    })
    const [url, options] = fetchMock.mock.calls[0]!
    expect(url).toBe('/api/v1/cloud/projects')
    expect(options?.method ?? 'GET').toBe('GET')
    expect(new Headers(options?.headers).get('Authorization')).toBe('Bearer private-access-token')
    expect(options?.body).toBeUndefined()

    fetchMock.mockImplementationOnce((_url, requestOptions) => new Promise((_resolve, reject) => {
      const signal = requestOptions?.signal
      if (!signal) {
        reject(new Error('Fetch did not receive the cancellation signal.'))
        return
      }
      signal.addEventListener('abort', () => reject(signal.reason), { once: true })
    }))
    const pending = cloudProjectsApi.list('private-access-token', controller.signal)
    const reason = new DOMException('Catalog closed', 'AbortError')
    controller.abort(reason)
    await expect(pending).rejects.toBe(reason)
  })

  it('uses authenticated metadata-only registry routes and immutable lineage bodies', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(async () => new Response(JSON.stringify({
        project: {
          project_id: 'project', bootstrap_id: '123e4567-e89b-42d3-a456-426614174002',
          origin_device_id: '123e4567-e89b-42d3-a456-426614174001', state: 'initializing',
          initial_event_count: null, initial_max_server_sequence: null,
        },
        current_cursor: 0,
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    const lineage = {
      bootstrap_id: '123e4567-e89b-42d3-a456-426614174002',
      device_id: '123e4567-e89b-42d3-a456-426614174001',
    }

    await cloudProjectsApi.registerBootstrap('token', 'project', lineage)
    await cloudProjectsApi.completeBootstrap('token', 'project', {
      ...lineage, initial_event_count: 2, initial_max_server_sequence: 4,
    })

    const [registerUrl, registerOptions] = fetchMock.mock.calls[0]!
    const [completeUrl, completeOptions] = fetchMock.mock.calls[1]!
    expect(registerUrl).toBe('/api/v1/cloud/projects/project/bootstrap')
    expect(completeUrl).toBe('/api/v1/cloud/projects/project/bootstrap/complete')
    expect(new Headers(registerOptions?.headers).get('Authorization')).toBe('Bearer token')
    expect(JSON.parse(String(registerOptions?.body))).toEqual(lineage)
    expect(JSON.parse(String(completeOptions?.body))).toEqual({
      ...lineage, initial_event_count: 2, initial_max_server_sequence: 4,
    })
    expect(String(completeOptions?.body)).not.toContain('content')
  })
})
