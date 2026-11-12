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
  if (fixture.amk) await page.evaluate(async raw => {
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

  const unlock = async (pageToUse, provision = false) => {
    const card = pageToUse.locator('[data-testid="browser-cloud-account"]')
    await card.locator('input[autocomplete="username"]').fill('Arthur')
    await card.locator('input[autocomplete="current-password"]').fill('correct horse battery staple')
    await card.locator('form button[type="submit"]').click()
    await card.locator('[data-status="authenticated"]').waitFor()
    if (provision) {
      for (const input of await card.locator('input[autocomplete="new-password"]').all()) {
        await input.fill('test-encryption-password')
      }
      await card.locator('form button[type="submit"]').click()
    }
    await card.locator('input[autocomplete="off"]').fill('test-encryption-password')
    await card.locator('form button[type="submit"]').click()
    await pageToUse.waitForURL('**/cloud')
    await card.locator('[data-status="unlocked"]').waitFor({ state: 'attached' })
    await card.locator('[data-testid="server-cloud-project-catalog"]').waitFor()
    return card
  }

  const open = async (card, projectId) => {
    await card.locator(`[data-cloud-project-id="${projectId}"]`).click()
    const panel = card.locator('[data-testid="cloud-project-read"]')
    await panel.waitFor()
    assert.equal(await card.locator('[data-testid="server-cloud-project-catalog"]').count(), 0)
    return panel
  }

  const verify = async (pageToUse) => {
    const panel = await open(await unlock(pageToUse), fixture.projectId)
    const notes = panel.locator('[data-testid="cloud-project-notes"]')
    await notes.waitFor({ timeout: 60000 })
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

  if (fixture.webCreate) {
    let card = await unlock(page, true)
    await card.getByTestId('cloud-sync-setup').click()
    const form = card.getByTestId('cloud-project-create')
    await form.locator('input').fill(fixture.expectedProjectName)
    await form.locator('button').click()
    const tile = card.locator('[data-cloud-project-id]')
    await tile.getByText(fixture.expectedProjectName, { exact: true }).waitFor()
    const id = await tile.getAttribute('data-cloud-project-id')
    const verifyEmpty = async () => {
      const panel = await open(card, id)
      const metadata = panel.getByTestId('cloud-project-metadata')
      await metadata.getByRole('heading', { name: fixture.expectedProjectName }).waitFor()
      await metadata.getByTestId('cloud-project-empty').waitFor()
      assert.equal(await panel.getByRole('alert').count(), 0)
      await card.getByTestId('cloud-project-back').click()
      assert.ok(!(await card.getByTestId('server-cloud-project-catalog').innerText()).includes(id))
    }
    await verifyEmpty()
    await context.close()
    page = await launch(`${profile}-fresh`)
    card = await unlock(page)
    await verifyEmpty()
  } else {
    await verify(page)
    // Read the same server history without the first browser's IndexedDB or device identity.
    await context.close()
    page = await launch(`${profile}-fresh`)
    await verify(page)
  }

  console.log(JSON.stringify({ pass: true, reopen: true }))
} finally {
  await context?.close()
  await vite?.close()
}
