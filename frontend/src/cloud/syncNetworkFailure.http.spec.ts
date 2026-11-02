// @vitest-environment node
import { spawn, type ChildProcess } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { createServer as createHttpServer } from 'node:http'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { afterEach, expect, it, vi } from 'vitest'
import { apiRequest } from '@/api/client'
import { isSyncNetworkFailure } from './syncNetworkFailure'

async function freePort(): Promise<number> {
  const server = createServer()
  await new Promise<void>((resolve, reject) => server.once('error', reject).listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('no loopback port')
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  return address.port
}

async function stop(child: ChildProcess): Promise<void> {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()))
  child.kill('SIGTERM')
  await exited
}

afterEach(() => vi.unstubAllEnvs())

it('classifies a real FastAPI connection loss and reaches it after restart', async () => {
  const port = await freePort()
  const url = `http://127.0.0.1:${port}`
  const dataDir = mkdtempSync(join(tmpdir(), 'worta-recovery-http-'))
  vi.stubEnv('VITE_API_BASE_URL', url)
  let child: ChildProcess | undefined
  async function start(): Promise<void> {
    const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('NFPROGRESS_')))
    let stderr = ''
    let spawnError: Error | undefined
    child = spawn(process.platform === 'win32' ? 'python' : 'python3', ['-m', 'uvicorn', 'backend.app.main:app', '--host', '127.0.0.1', '--port', String(port)], {
      cwd: '..', env: { ...env, NFPROGRESS_ENV: 'test', NFPROGRESS_PLATFORM: 'web',
        NFPROGRESS_DATA_DIR: dataDir, NFPROGRESS_TEST_DATA_DIR: dataDir,
        NFPROGRESS_CLOUD_BLOB_DIR: join(dataDir, 'blobs'), PYTHONDONTWRITEBYTECODE: '1' }, stdio: ['ignore', 'ignore', 'pipe'],
    })
    child.stderr?.on('data', chunk => { stderr += String(chunk) })
    child.once('error', error => { spawnError = error })
    for (let attempt = 0; attempt < 100; attempt++) {
      if (spawnError) throw new Error(`local FastAPI failed to start: ${spawnError.message}; stderr: ${stderr}`)
      if (child.exitCode !== null) throw new Error(`local FastAPI exited during startup (code ${child.exitCode}): ${stderr}`)
      try { await apiRequest('/health'); return } catch { await delay(100) }
    }
    throw new Error('local FastAPI health check timed out')
  }
  try {
    await start()
    await stop(child!)
    let failure: unknown
    try { await apiRequest('/health') } catch (error) { failure = error }
    expect(isSyncNetworkFailure(failure)).toBe(true)
    await start()
    await expect(apiRequest('/health')).resolves.toBeDefined()
  } finally {
    if (child) await stop(child)
    rmSync(dataDir, { recursive: true, force: true })
  }
}, 30_000)

it('classifies a real connection dropped after HTTP response headers', async () => {
  const server = createHttpServer((_request, response) => {
    response.writeHead(200, { 'Content-Type': 'application/json' })
    response.flushHeaders()
    response.write('{"partial":')
    setTimeout(() => response.destroy(), 25)
  })
  await new Promise<void>((resolve, reject) => server.once('error', reject).listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('no loopback port')
  vi.stubEnv('VITE_API_BASE_URL', `http://127.0.0.1:${address.port}`)
  try {
    for (const maxResponseBytes of [undefined, 1024]) {
      let failure: unknown
      try { await apiRequest('/interrupted', { maxResponseBytes }) } catch (error) { failure = error }
      expect(isSyncNetworkFailure(failure)).toBe(true)
    }
  } finally {
    server.closeAllConnections()
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  }
})
