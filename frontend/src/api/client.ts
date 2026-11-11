interface DomainErrorDetail {
  code?: unknown
  message?: unknown
}

interface ErrorEnvelope {
  detail?: DomainErrorDetail | Array<{ msg?: unknown }>
}

export class ApiError extends Error {
  readonly status: number
  readonly code: string
  readonly details: unknown

  constructor(status: number, code: string, message: string, details?: unknown) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.details = details
  }
}

export class ApiResponseTooLargeError extends Error {
  readonly name = 'ApiResponseTooLargeError'
}

function connectionMessage(): string {
  return typeof document !== 'undefined' && document.documentElement.dataset.platform === 'web'
    ? 'Не удалось подключиться к серверу worta.'
    : 'Не удалось подключиться к nfprogress API.'
}

type ApiRequestOptions = Omit<RequestInit, 'body' | 'headers'> & {
  body?: unknown
  rawBody?: BodyInit
  headers?: HeadersInit
  /** Applies only to successful JSON responses, before JSON materialization. */
  maxResponseBytes?: number
}

async function readResponseBody<T>(read: () => Promise<T>): Promise<T> {
  try {
    return await read()
  } catch (error) {
    // Fetch can fail while consuming a response after the headers arrived.
    // JSON syntax, limits and aborts remain distinct non-network failures.
    if (error instanceof TypeError) {
      const cause = error.cause
      const code = cause && typeof cause === 'object' && 'code' in cause ? cause.code : undefined
      const disconnected = typeof code === 'string' && ['UND_ERR_SOCKET', 'ECONNRESET', 'ECONNABORTED', 'ETIMEDOUT', 'EPIPE'].includes(code)
        || /^(load failed|failed to fetch|networkerror.*|network request failed)$/i.test(error.message)
      if (disconnected) throw new ApiError(0, 'network_error', connectionMessage(), error)
    }
    throw error
  }
}

async function boundedJson(response: Response, maximumBytes: number): Promise<unknown> {
  if (!Number.isSafeInteger(maximumBytes) || maximumBytes < 1) throw new TypeError('Invalid API response size limit.')
  const contentLength = response.headers.get('Content-Length')
  if (contentLength !== null) {
    const declared = Number(contentLength)
    if (!Number.isSafeInteger(declared) || declared < 0 || declared > maximumBytes) {
      throw new ApiResponseTooLargeError('API response exceeds size limit.')
    }
  }
  if (!response.body) return JSON.parse(await readResponseBody(() => response.text()))
  const reader = response.body.getReader()
  const chunks: Uint8Array[] = []
  let received = 0
  try {
    while (true) {
      const { done, value } = await readResponseBody(() => reader.read())
      if (done) break
      received += value.byteLength
      if (received > maximumBytes) throw new ApiResponseTooLargeError('API response exceeds size limit.')
      chunks.push(value)
    }
  } catch (error) {
    await reader.cancel(error).catch(() => undefined)
    throw error
  } finally {
    reader.releaseLock()
  }
  const bytes = new Uint8Array(received)
  let offset = 0
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.byteLength
  }
  return JSON.parse(new TextDecoder().decode(bytes))
}

function configuredBaseUrl(): string {
  const value = import.meta.env.VITE_API_BASE_URL ?? ''
  return value.trim().replace(/\/+$/, '')
}

function requestUrl(path: string): string {
  const normalizedPath = path.startsWith('/') ? path : `/${path}`
  return `${configuredBaseUrl()}${normalizedPath}`
}

async function errorFromResponse(response: Response): Promise<ApiError> {
  let details: unknown
  try {
    details = await response.json()
  } catch {
    details = undefined
  }

  const envelope = details as ErrorEnvelope | undefined
  if (envelope?.detail && !Array.isArray(envelope.detail)) {
    const code = typeof envelope.detail.code === 'string' ? envelope.detail.code : 'api_error'
    const message =
      typeof envelope.detail.message === 'string'
        ? envelope.detail.message
        : `API вернул ошибку ${response.status}.`
    return new ApiError(response.status, code, message, details)
  }

  if (Array.isArray(envelope?.detail)) {
    const validationMessages = envelope.detail
      .map((item) => (typeof item.msg === 'string' ? item.msg : null))
      .filter((message): message is string => message !== null)
    const message = validationMessages.join(' ') || 'Проверьте введённые данные.'
    return new ApiError(response.status, 'request_validation', message, details)
  }

  return new ApiError(
    response.status,
    'api_error',
    `API вернул ошибку ${response.status}.`,
    details,
  )
}

export async function apiRequest<T>(path: string, options: ApiRequestOptions = {}): Promise<T> {
  const {
    body: jsonBody,
    rawBody,
    headers: suppliedHeaders,
    maxResponseBytes,
    ...requestOptions
  } = options
  const headers = new Headers(suppliedHeaders)
  headers.set('Accept', 'application/json')

  let body: BodyInit | undefined
  if (jsonBody !== undefined && rawBody !== undefined) {
    throw new TypeError('Укажите только JSON body или rawBody.')
  }
  if (rawBody !== undefined) {
    body = rawBody
  } else if (jsonBody !== undefined) {
    headers.set('Content-Type', 'application/json')
    body = JSON.stringify(jsonBody)
  }

  let response: Response
  try {
    response = await fetch(requestUrl(path), {
      ...requestOptions,
      headers,
      body,
    })
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') {
      throw error
    }
    throw new ApiError(
      0,
      'network_error',
      connectionMessage(),
      error,
    )
  }

  if (!response.ok) {
    throw await errorFromResponse(response)
  }
  if (response.status === 204) {
    return undefined as T
  }
  if (maxResponseBytes !== undefined) return await boundedJson(response, maxResponseBytes) as T
  return (await readResponseBody(() => response.json())) as T
}

export async function apiBinaryRequest(path: string, options: ApiRequestOptions = {}): Promise<{ bytes: Uint8Array; headers: Headers }> {
  const { body: jsonBody, rawBody, headers: suppliedHeaders, ...requestOptions } = options
  if (jsonBody !== undefined || rawBody !== undefined) throw new TypeError('Binary API request does not accept a request body.')
  const headers = new Headers(suppliedHeaders)
  headers.set('Accept', 'application/octet-stream')
  let response: Response
  try {
    response = await fetch(requestUrl(path), { ...requestOptions, headers })
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') throw error
    throw new ApiError(0, 'network_error', connectionMessage(), error)
  }
  if (!response.ok) throw await errorFromResponse(response)
  return { bytes: new Uint8Array(await readResponseBody(() => response.arrayBuffer())), headers: response.headers }
}

export function apiErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message
  }
  if (typeof error === 'string' && error.trim()) {
    return error
  }
  return 'Произошла непредвиденная ошибка.'
}
