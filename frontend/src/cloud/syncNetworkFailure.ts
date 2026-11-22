import { ApiError } from '@/api/client'

/** Only unambiguous transport failures qualify for automatic retry. */
export function isSyncNetworkFailure(error: unknown): boolean {
  return error instanceof ApiError && ((error.status === 0 && ['network_error', 'request_timeout'].includes(error.code))
    || ([502, 503, 504].includes(error.status) && error.code === 'api_error'))
}

/** Retain the network classification when a bounded cycle returns stage errors. */
export function syncStageErrorCode(error: unknown): string {
  if (isSyncNetworkFailure(error)) return error instanceof ApiError && error.code === 'request_timeout'
    ? 'request_timeout' : 'network_error'
  if (error instanceof Error && 'code' in error && typeof error.code === 'string') {
    // A server or local failure cannot opt into network retry by naming itself
    // network_error; only the transport status above can establish that.
    return ['network_error', 'request_timeout'].includes(error.code) ? 'unknown_error' : error.code
  }
  const name = error instanceof Error ? error.name : 'unknown_error'
  return ['network_error', 'request_timeout'].includes(name) ? 'unknown_error' : name
}
