import { apiRequest } from './client'
import {
  SYNC_PROTOCOL_VERSION,
  isSyncCursor,
  isSyncEventEnvelope,
  type SyncAckRequest,
  type SyncPullResponse,
  type SyncPushRequest,
  type SyncPushResponse,
} from '@/cloud/syncProtocol'

function authorization(accessToken: string): Headers {
  const headers = new Headers()
  headers.set('Authorization', `Bearer ${accessToken}`)
  return headers
}

export interface SyncDeviceResponse {
  protocol_version: typeof SYNC_PROTOCOL_VERSION
  device_id: string
  last_ack_cursor: number
}

/** Typed C9 client only. Production UI deliberately does not call this yet. */
export const syncApi = {
  registerDevice(accessToken: string, deviceId: string, initialWebSetup?: boolean): Promise<SyncDeviceResponse> {
    return apiRequest(`/api/v1/sync/devices/${encodeURIComponent(deviceId)}${initialWebSetup ? '?initial_web_setup=true' : ''}`, {
      method: 'PUT', headers: authorization(accessToken),
    })
  },
  push(accessToken: string, request: SyncPushRequest): Promise<SyncPushResponse> {
    if (request.events.length > 100 || !request.events.every(isSyncEventEnvelope)) {
      throw new RangeError('Invalid C9 sync event envelope.')
    }
    return apiRequest('/api/v1/sync/push', { method: 'POST', headers: authorization(accessToken), body: request })
  },
  pull(accessToken: string, deviceId: string, since: number, limit?: number): Promise<SyncPullResponse> {
    if (!isSyncCursor(since)) throw new RangeError('Invalid C9 sync cursor.')
    const query = new URLSearchParams({ device_id: deviceId, since: String(since), protocol_version: String(SYNC_PROTOCOL_VERSION) })
    if (limit !== undefined) query.set('limit', String(limit))
    return apiRequest(`/api/v1/sync/pull?${query}`, { headers: authorization(accessToken) })
  },
  ack(accessToken: string, request: SyncAckRequest): Promise<void> {
    if (!isSyncCursor(request.cursor)) throw new RangeError('Invalid C9 sync cursor.')
    return apiRequest('/api/v1/sync/ack', { method: 'POST', headers: authorization(accessToken), body: request })
  },
}
