import { apiRequest } from './client'

export interface CloudProjectsResponse {
  cloud_project_ids: string[]
  cloud_project_count: number
  max_cloud_projects: number
}

export type CloudProjectBootstrapState = 'legacy' | 'initializing' | 'active'

export interface CloudProjectBootstrapDescriptor {
  project_id: string
  bootstrap_id: string | null
  origin_device_id: string | null
  state: CloudProjectBootstrapState
  initial_event_count: number | null
  initial_max_server_sequence: number | null
}

export interface CloudProjectBootstrapListResponse {
  projects: CloudProjectBootstrapDescriptor[]
  current_cursor: number
}

export interface CloudProjectBootstrapResponse {
  project: CloudProjectBootstrapDescriptor
  current_cursor: number
}

export interface CloudProjectBootstrapRegistration {
  bootstrap_id: string
  device_id: string
}

export interface CloudProjectBootstrapCompletion extends CloudProjectBootstrapRegistration {
  initial_event_count: number
  initial_max_server_sequence: number
}

function authorization(accessToken: string): Headers {
  const headers = new Headers()
  headers.set('Authorization', `Bearer ${accessToken}`)
  return headers
}

/** Metadata-only C8 API: it receives a project ID in the path and no content. */
export const cloudProjectsApi = {
  list(accessToken: string, signal?: AbortSignal): Promise<CloudProjectsResponse> {
    return apiRequest<CloudProjectsResponse>('/api/v1/cloud/projects', {
      headers: authorization(accessToken), signal,
    })
  },

  enable(accessToken: string, projectId: string): Promise<CloudProjectsResponse> {
    return apiRequest<CloudProjectsResponse>(`/api/v1/cloud/projects/${encodeURIComponent(projectId)}`, {
      method: 'POST',
      headers: authorization(accessToken),
    })
  },

  disable(accessToken: string, projectId: string): Promise<void> {
    return apiRequest<void>(`/api/v1/cloud/projects/${encodeURIComponent(projectId)}`, {
      method: 'DELETE',
      headers: authorization(accessToken),
    })
  },

  listBootstraps(accessToken: string, signal?: AbortSignal): Promise<CloudProjectBootstrapListResponse> {
    return apiRequest<CloudProjectBootstrapListResponse>('/api/v1/cloud/projects/bootstrap', {
      headers: authorization(accessToken), signal, maxResponseBytes: 2 * 1024 * 1024,
    })
  },

  registerBootstrap(accessToken: string, projectId: string, body: CloudProjectBootstrapRegistration, signal?: AbortSignal): Promise<CloudProjectBootstrapResponse> {
    return apiRequest<CloudProjectBootstrapResponse>(`/api/v1/cloud/projects/${encodeURIComponent(projectId)}/bootstrap`, {
      method: 'POST', headers: authorization(accessToken), body, signal,
    })
  },

  completeBootstrap(accessToken: string, projectId: string, body: CloudProjectBootstrapCompletion, signal?: AbortSignal): Promise<CloudProjectBootstrapResponse> {
    return apiRequest<CloudProjectBootstrapResponse>(`/api/v1/cloud/projects/${encodeURIComponent(projectId)}/bootstrap/complete`, {
      method: 'POST', headers: authorization(accessToken), body, signal, maxResponseBytes: 16 * 1024,
    })
  },
}
