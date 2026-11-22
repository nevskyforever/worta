// Exercise the production Web transition against authenticated server events.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createServer } from 'vite'
const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const vite = await createServer({ logLevel: 'error', server: { middlewareMode: true } })
try {
  const { transitionProgress } = await vite.ssrLoadModule('/src/cloud/progressTransition.ts')
  let chain
  for (const event of fixture.events) chain = transitionProgress(event, chain)
  assert.deepEqual(chain, fixture.expected)
} finally {
  await vite.close()
}
