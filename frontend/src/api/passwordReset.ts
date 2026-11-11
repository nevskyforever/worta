import { apiRequest } from './client'

export const passwordResetApi = {
  request(email: string, signal?: AbortSignal): Promise<{ code: 'password_reset_request_accepted' }> {
    return apiRequest('/api/v1/auth/password-reset/request', { method: 'POST', body: { email }, signal })
  },
  confirm(token: string, password: string, signal?: AbortSignal): Promise<{ code: 'password_reset_complete' }> {
    return apiRequest('/api/v1/auth/password-reset/confirm', {
      method: 'POST', body: { token, new_password: password }, signal,
    })
  },
}
