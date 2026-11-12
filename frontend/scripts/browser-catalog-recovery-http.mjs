import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const backend = process.argv[3]
const profile = process.argv[4]
let vite, context
try {
  vite = await createServer({ logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: true } })
  await vite.listen()
  const origin = `http://127.0.0.1:${vite.httpServer.address().port}`
  const launch = async (profilePath) => {
    context = await chromium.launchPersistentContext(profilePath, { headless: true })
    await context.route(url => url.pathname.startsWith('/api/'), async route => {
      const request = route.request(), url = new URL(request.url())
      return route.continue({ url: backend + url.pathname + url.search })
    })
    const page = await context.newPage()
    await page.goto(origin)
    await page.waitForURL('**/login')
    return page
  }

  let page = await launch(profile)
  await page.evaluate(async raw => {
    const crypto = await import('/src/crypto/index.ts')
    const { encodeBase64Url } = await import('/src/api/base64url.ts')
    const { accountCryptoApi } = await import('/src/api/accountCrypto.ts')
    const auth = await (await fetch('/api/v1/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'Arthur', password: 'correct horse battery staple' }) })).json()
    const amk = Uint8Array.from(raw), recovery = await crypto.generateRecoveryKey()
    try {
      const password = await crypto.wrapAmkWithPassphrase(amk, 'test-encryption-password')
      const wrapped = await crypto.wrapAmkWithRecoveryKey(amk, recovery)
      await accountCryptoApi.provision(auth.access_token, {
        password: { ...password, kdf: { ...password.kdf, salt: encodeBase64Url(password.kdf.salt) }, nonce: encodeBase64Url(password.nonce), ciphertext: encodeBase64Url(password.ciphertext) },
        recovery: { ...wrapped, nonce: encodeBase64Url(wrapped.nonce), ciphertext: encodeBase64Url(wrapped.ciphertext) },
      })
    } finally { amk.fill(0); recovery.fill(0) }
  }, fixture.amk)

  const loginAndOpen = async (pageToUse) => {
    const card = pageToUse.locator('[data-testid="browser-cloud-account"]')
    await card.locator('input[autocomplete="username"]').fill('Arthur')
    await card.locator('input[autocomplete="current-password"]').fill('correct horse battery staple')
    await card.locator('form button[type="submit"]').click()
    await card.locator('[data-status="authenticated"]').waitFor()
    await card.locator('input[autocomplete="off"]').fill('test-encryption-password')
    await card.locator('form button[type="submit"]').click()
    await pageToUse.waitForURL('**/cloud')
    await card.locator('[data-status="unlocked"]').waitFor()
    await card.locator('[data-testid="server-cloud-project-catalog"]').waitFor()
    await card.locator(`[data-cloud-project-id="${fixture.projectId}"]`).waitFor()
    await card.locator(`[data-cloud-project-id="${fixture.projectId}"]`).click()
    const panel = card.locator('[data-testid="cloud-project-read"]')
    await panel.waitFor()
    await pageToUse.locator('[data-testid="cloud-project-notes"]').waitFor({ timeout: 60000 })
    return { card, panel }
  }

  const verify = async (pageToUse) => {
    const { panel } = await loginAndOpen(pageToUse)
    const notes = pageToUse.locator('[data-testid="cloud-project-notes"]')
    assert.equal(await notes.locator('h3').first().innerText(), fixture.expectedProjectName)
    const stages = panel.locator('[data-stage-id]')
    assert.equal(await stages.count(), fixture.stages?.length ?? 1)
    if (fixture.stages?.length) {
      const ids = await stages.evaluateAll(nodes => nodes.map(node => node.getAttribute('data-stage-id')))
      assert.deepEqual(ids, fixture.stages)
      await panel.locator(`[data-stage-id="${fixture.stageId ?? fixture.stages[0]}"]`).click()
    }
    // Plain notes render directly; HTML notes render in the sandboxed iframe.
    if (fixture.contentFormat === 'html') {
      const rendered = await pageToUse.frameLocator('[data-testid="cloud-note-html"]').locator('body').innerText()
      assert.ok(rendered.includes(String(fixture.expectedContent).replace(/<[^>]*>/g, '')))
    } else {
      const text = await notes.innerText()
      assert.ok(text.includes(fixture.expectedContent), `note content missing: ${text}`)
    }
  }

  await verify(page)
  // Read the same server history without the first browser's IndexedDB or device identity.
  await context.close()
  page = await launch(`${profile}-fresh`)
  await verify(page)

  console.log(JSON.stringify({ pass: true, catalogOpen: true, autoOpenRecovery: true, stage: true, note: true, reopen: true }))
} finally {
  await context?.close()
  await vite?.close()
}
