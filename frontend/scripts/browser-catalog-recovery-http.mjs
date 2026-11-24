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
  const launch = async (profilePath, viewport = { width: 1280, height: 900 }) => {
    context = await chromium.launchPersistentContext(profilePath, { headless: true, viewport })
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

  const screenshot = async (pageToUse, name) => {
    assert.ok(await pageToUse.locator('#main-content').evaluate(main => main.scrollWidth <= main.clientWidth))
    if (process.env.WORTA_WORKSPACE_SCREENSHOTS)
      await pageToUse.screenshot({ path: `${process.env.WORTA_WORKSPACE_SCREENSHOTS}/${name}.png` })
  }

  const open = async (card, projectId) => {
    await card.locator(`[data-cloud-project-id="${projectId}"]`).press('Enter')
    const panel = card.locator('[data-testid="cloud-project-read"]')
    await panel.waitFor()
    assert.equal(await card.locator('[data-testid="server-cloud-project-catalog"]').count(), 0)
    assert.equal(await card.getByTestId('cloud-project-back').evaluate(button => button === document.activeElement), true)
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
    const navigation = pageToUse.locator(pageToUse.viewportSize().width < 600 ? '.mobile-navigation' : '#primary-navigation')
    const chooseSection = async section => {
      const link = navigation.locator(`a[href="/cloud?section=${section}"]`)
      await link.click()
      await pageToUse.waitForURL(`**/cloud?section=${section}`)
      assert.equal(await link.getAttribute('aria-current'), 'page')
    }
    await chooseSection('notes')
    assert.equal(await panel.getByTestId('cloud-document-list').count(), 0)
    // Plain notes render directly; HTML notes render in the sandboxed iframe.
    if (fixture.contentFormat === 'html') {
      const rendered = await pageToUse.frameLocator('[data-testid="cloud-note-html"]').locator('body').innerText()
      assert.ok(rendered.includes(String(fixture.expectedContent).replace(/<[^>]*>/g, '')))
    } else {
      const text = await notes.innerText()
      assert.ok(text.includes(fixture.expectedContent), `note content missing: ${text}`)
    }
    if (fixture.manuscript) {
      await chooseSection('texts')
      assert.equal(await panel.locator('.cloud-reader__notes').count(), 0)
      assert.equal(await panel.getByTestId('cloud-document-read').count(), 1)
      const editor = panel.locator('.tiptap[contenteditable="false"]')
      await editor.locator('strong').getByText(fixture.manuscript, { exact: true }).waitFor()
      assert.equal(await editor.locator('a, img, iframe').count(), 0)
      if (fixture.reload) {
        await panel.getByTestId('cloud-project-refresh').click()
        await editor.locator('strong').getByText(fixture.manuscript, { exact: true }).waitFor()
      }
      await editor.press('Tab')
      assert.equal(await editor.evaluate(element => element === document.activeElement), false)
    }
    assert.equal(await panel.getByRole('alert').count(), 0)
    assert.equal(await pageToUse.locator('#main-content').evaluate(main => main.scrollWidth <= main.clientWidth), true)
    if (process.env.WORTA_WORKSPACE_SCREENSHOTS) {
      const width = pageToUse.viewportSize().width
      await pageToUse.screenshot({ path: `${process.env.WORTA_WORKSPACE_SCREENSHOTS}/workspace-${width}.png` })
    }
  }

  if (fixture.emptyOrder) {
    const card = await unlock(page)
    const panel = await open(card, fixture.projectId)
    await panel.getByTestId('cloud-project-details').waitFor()
    await panel.getByTestId('cloud-stage-create').waitFor()
    const api = async (path, body, token, method = 'POST') => {
      const response = await fetch(backend + path, { method, headers: { 'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}) }, body: JSON.stringify(body) })
      assert.ok(response.ok, `${path} · ${response.status}`)
      return response.status === 204 ? null : response.json()
    }
    const login = await api('/api/v1/auth/login', { username: 'Arthur', password: 'correct horse battery staple' })
    const token = login.access_token
    // Test the ordinary Desktop registration and StageOrder paths against the already-open browser.
    const bootstrap = `/api/v1/cloud/projects/${fixture.legacyProjectId}/bootstrap`
    await api(bootstrap, { device_id: fixture.desktop, bootstrap_id: fixture.legacyBootstrap }, token)
    await api(bootstrap + '/complete', { device_id: fixture.desktop, bootstrap_id: fixture.legacyBootstrap,
      initial_event_count: 0, initial_max_server_sequence: 0 }, token)
    const refresh = panel.getByTestId('cloud-project-refresh')
    await refresh.click()
    await panel.getByTestId('cloud-project-details').waitFor()
    assert.equal(await panel.getByRole('alert').count(), 0)
    await api('/api/v3/sync/encrypted/push', fixture.emptyOrder, token)
    await refresh.click()
    await panel.getByTestId('cloud-project-details').waitFor()
    assert.equal(await panel.getByRole('alert').count(), 0)
    await card.getByTestId('cloud-project-back').click()
    await card.getByRole('button', { name: /^(Refresh list|Обновить список)$/ }).click()
    await open(card, fixture.legacyProjectId)
    await panel.locator('[data-read-code="metadata_missing"]').waitFor()
    await card.getByTestId('cloud-project-back').click()
    await open(card, fixture.projectId)
    await panel.getByTestId('cloud-stage-create').waitFor()
  } else if (fixture.documentRoundTrip) {
    let card = await unlock(page)
    await card.locator('.project-card__status').first().waitFor()
    await screenshot(page, 'projects')
    let panel = await open(card, fixture.projectId)
    await panel.locator('.stage-card').first().waitFor()
    await screenshot(page, 'project')
    const stage = panel.locator('[data-stage-id="S1"]')
    await stage.click()
    await panel.getByTestId('cloud-project-details').getByRole('heading', { name: 'Stage S1' }).waitFor()
    await screenshot(page, 'stage')
    await page.locator('#primary-navigation a[href="/cloud?section=texts"]').click()
    await panel.locator('.tiptap[contenteditable="false"] strong').filter({ hasText: 'Desktop manuscript' }).waitFor({ timeout: 10000 })
      .catch(async cause => { throw new Error(await panel.innerText(), { cause }) })
    await panel.getByTestId('cloud-document-edit-action').click()
    const edit = panel.locator('.tiptap[contenteditable="true"]')
    await edit.click()
    await edit.press('End')
    await page.keyboard.insertText(' plus Web')
    await panel.getByTestId('cloud-document-save').click()
    await panel.locator('.tiptap[contenteditable="false"] strong').filter({ hasText: 'Desktop manuscript plus Web' }).waitFor({ timeout: 10000 })
      .catch(async cause => { throw new Error(await panel.innerText(), { cause }) })
    const projectsLink = page.locator('#primary-navigation a[href="/cloud"]')
    await projectsLink.click()
    panel = await open(card, fixture.projectId)
    await panel.getByTestId('cloud-stage-create').click()
    const nativeStageForm = panel.getByTestId('cloud-stage-create-form')
    await nativeStageForm.locator('input[required]').fill('Web stage in Desktop project')
    await nativeStageForm.locator('input[type="number"]').fill('4000')
    const stagePublish = url => url.searchParams.has('web_stage_version')
    let submittedStage
    await page.route(stagePublish, async route => {
      const url = new URL(route.request().url())
      submittedStage = route.request().postData()
      assert.equal(submittedStage.includes('Web stage in Desktop project'), false)
      const response = await route.fetch({ url: backend + url.pathname + url.search })
      assert.equal(response.status(), 200)
      await route.abort('failed')
    }, { times: 1 })
    const stageSubmit = nativeStageForm.locator('button[type="submit"]')
    await stageSubmit.click()
    await nativeStageForm.getByRole('alert').waitFor()
    const retry = page.waitForRequest(request => stagePublish(new URL(request.url())))
    await stageSubmit.click()
    assert.equal((await retry).postData(), submittedStage)
    await panel.getByTestId('cloud-project-details').getByText('Web stage in Desktop project', { exact: true })
      .waitFor({timeout: 8000}).catch(async cause => { throw new Error(await panel.innerText(), { cause }) })
    await card.getByTestId('cloud-project-back').click()
    await card.getByTestId('cloud-project-create-toggle').click()
    const form = card.getByTestId('cloud-project-create')
    await form.getByRole('textbox').fill('New Web manuscript project')
    await form.locator('button[type="submit"]').click()
    const tile = card.locator('[data-cloud-project-id]').filter({ hasText: 'New Web manuscript project' })
    await tile.waitFor()
    const createdId = await tile.getAttribute('data-cloud-project-id')
    assert.ok(createdId)
    panel = await open(card, createdId)
    await panel.getByTestId('cloud-project-details').locator('a[href="/cloud?section=texts"]').click()
    const create = panel.getByRole('button', { name: /^(Создать|Create)$/ })
    await create.click()
    await panel.locator('.tiptap[contenteditable="true"]').fill('First project text')
    await panel.getByTestId('cloud-document-save').click()
    await panel.locator('.tiptap[contenteditable="false"]').getByText('First project text').waitFor()
    await projectsLink.click()
    panel = await open(card, createdId)
    await panel.getByTestId('cloud-stage-create').click()
    const newStage = panel.getByTestId('cloud-stage-create-form')
    await newStage.locator('input[required]').fill('First Web stage')
    await newStage.locator('button[type="submit"]').click()
    await panel.getByTestId('cloud-project-details').getByText('First Web stage', { exact: true })
      .waitFor({timeout: 8000}).catch(async cause => { throw new Error(await panel.innerText(), { cause }) })
    await panel.getByTestId('cloud-project-details').locator('a[href="/cloud?section=texts"]').click()
    await panel.locator('[data-stage-id]').getByText('First Web stage', { exact: true }).waitFor()
    await create.click()
    await panel.locator('.tiptap[contenteditable="true"]').fill('First stage text')
    await panel.getByTestId('cloud-document-save').click()
    await panel.locator('.tiptap[contenteditable="false"]').getByText('First stage text').waitFor()
    await context.close()
    page = await launch(`${profile}-fresh`)
    card = await unlock(page)
    panel = await open(card, createdId)
    await page.locator('#primary-navigation a[href="/cloud?section=texts"]').click()
    await panel.locator('[data-stage-id]').getByText('First Web stage', { exact: true }).click()
    await panel.locator('.tiptap[contenteditable="false"]').getByText('First stage text').waitFor()
    await panel.getByTestId('cloud-project-notes-select').click()
    await panel.locator('.tiptap[contenteditable="false"]').getByText('First project text').waitFor()
  } else if (fixture.webCreate) {
    let card = await unlock(page, true)
    await card.getByTestId('cloud-sync-setup').click()
    await card.getByTestId('cloud-project-create-toggle').click()
    const form = card.getByTestId('cloud-project-create')
    await form.locator('input').fill(fixture.expectedProjectName)
    await form.locator('button[type="submit"]').click()
    const tile = card.locator('[data-cloud-project-id]').filter({ hasText: fixture.expectedProjectName })
    await tile.waitFor()
    const id = await tile.getAttribute('data-cloud-project-id')
    const secondName = 'Второй облачный черновик'
    await card.getByTestId('cloud-project-create-toggle').click()
    await form.locator('input').fill(secondName)
    await form.locator('button[type="submit"]').click()
    const secondTile = card.locator('[data-cloud-project-id]').filter({ hasText: secondName })
    await secondTile.waitFor().catch(async error => {
      throw new Error(`Second project did not appear. Form: ${await form.count() ? await form.innerText() : ''}; catalog: ${await card.getByTestId('server-cloud-project-catalog').innerText()}`, { cause: error })
    })
    const secondId = await secondTile.getAttribute('data-cloud-project-id')
    const verifyEmpty = async () => {
      const panel = await open(card, id)
      const metadata = panel.getByTestId('cloud-project-metadata')
      await metadata.getByRole('heading', { name: fixture.expectedProjectName }).waitFor()
      await metadata.getByTestId('cloud-project-empty').waitFor()
      assert.equal(await panel.getByRole('alert').count(), 0)
      await card.getByTestId('cloud-project-switch').selectOption(secondId)
      await card.getByTestId('cloud-project-metadata').getByRole('heading', { name: secondName }).waitFor()
      await card.getByTestId('cloud-project-back').click()
      assert.equal(await card.locator(`[data-cloud-project-id="${secondId}"]`).evaluate(button => button === document.activeElement), true)
      assert.ok(!(await card.getByTestId('server-cloud-project-catalog').innerText()).includes(id))
    }
    await verifyEmpty()
    await context.close()
    page = await launch(`${profile}-fresh`, { width: 390, height: 844 })
    card = await unlock(page)
    await verifyEmpty()
  } else {
    await verify(page)
    if (fixture.reload) {
      await page.reload()
      await page.waitForURL('**/login')
      await verify(page)
    }
    // Read the same server history without the first browser's IndexedDB or device identity.
    await context.close()
    page = await launch(`${profile}-fresh`, { width: 390, height: 844 })
    await verify(page)
  }

  console.log(JSON.stringify({ pass: true, reopen: true }))
} finally {
  await context?.close()
  await vite?.close()
}
