import { execFileSync } from 'node:child_process'
import { mkdtemp, writeFile, rm } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

// Frozen real slice-1 receiver/storage. Its native IDB version check is the old-client proof.
async function replayClientFixture(revision, modules, entry) {
  const root = dirname(fileURLToPath(import.meta.url)), directory = await mkdtemp(join(root, 'replay-v1-fixture-'))
  try {
    for (const name of modules) {
      const original = execFileSync('git', ['show', revision + ':frontend/src/platform/web/' + name], { cwd: root, encoding: 'utf8' })
      const source = original.replace(/(from |import\()'\.\/([^']+)'/g, (match, prefix, module) => modules.includes(module + '.ts') ? match : `${prefix}'../../src/platform/web/${module}'`)
      await writeFile(join(directory, name), source)
    }
    return { url: `/scripts/${directory.slice(root.length + 1)}/${entry}`, storage: `/scripts/${directory.slice(root.length + 1)}/replayStorage.ts`, close: () => rm(directory, { recursive: true, force: true }) }
  } catch (error) { await rm(directory, { recursive: true, force: true }); throw error }
}

export const replayV1Fixture = () => replayClientFixture('bdfb5601d2d4b512fc76e424c5b0db1ae87dc67c', ['replayRecords.ts', 'replayStorage.ts'], 'replayStorage.ts')
export const replayV2Fixture = () => replayClientFixture('3cb31ff63546d8be388013184a168c2babe88678',
  ['cloudAccountSession.ts', 'replayReceiver.ts', 'replayStorage.ts', 'replayRecords.ts', 'replayInventory.ts', 'replayMetadata.ts'], 'cloudAccountSession.ts')
export const replayV3Fixture = () => replayClientFixture('57abfbc785c9e8c849e2b6fbc9f78208ab7b47a8',
  ['replayStorage.ts', 'replayRecords.ts', 'replayInventory.ts', 'replayMetadata.ts'], 'replayStorage.ts')
export const replayV4Fixture = () => replayClientFixture('7f29c55b9b943a9087eafbe81a8b05836a8e5ced',
  ['replayStorage.ts', 'replayRecords.ts', 'replayInventory.ts', 'replayMetadata.ts'], 'replayStorage.ts')
export const replayV5Fixture = () => replayClientFixture('0b66eb877d17eaf5b2c276201a5d44042d199acd',
  ['replayStorage.ts', 'replayRecords.ts', 'replayInventory.ts', 'replayMetadata.ts'], 'replayStorage.ts')
export const replayV5SessionFixture = () => replayClientFixture('0b66eb877d17eaf5b2c276201a5d44042d199acd',
  ['cloudAccountSession.ts', 'replayReceiver.ts', 'replayStorage.ts', 'replayRecords.ts', 'replayInventory.ts', 'replayMetadata.ts', 'replaySourceSeed.ts', 'replayContentUpdate.ts'], 'cloudAccountSession.ts')
