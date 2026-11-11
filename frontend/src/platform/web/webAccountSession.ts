import { BrowserCloudAccountSession } from './cloudAccountSession'

// The Web application owns this in-memory session. Navigation leaves its key
// lease intact. The session itself revokes on logout, lock and pagehide.
let session: BrowserCloudAccountSession | undefined

export function webAccountSession(): BrowserCloudAccountSession {
  return session ??= new BrowserCloudAccountSession()
}
