// @vitest-environment node
import { describe, expect, it } from 'vitest'
import { scope } from './bootstrap.testFixture'
import { imported, received } from './legacyNoteApply.testFixture'
import { localCanonicalBytes, MAX_LOCAL_SOURCE_BYTES } from './localAuthorityRecords'

const opened = async (f: Awaited<ReturnType<typeof imported>>) =>
  f.local.withDecryptedSnapshot(scope, (await f.local.readSnapshot(scope, 'p')).token, async source => structuredClone(source))

describe('C21.2.08Z2B1 shared local source transition', () => {
  it('derives the same encrypted project membership and Note state from the same authenticated transitions', async () => {
    const states: Awaited<ReturnType<typeof opened>>[] = []
    for (let run = 0; run < 2; run++) {
      const f = await imported(), create = await received(f, 1)
      await f.notes.apply(scope, 1)
      const update = await received(f, 2, 2, create.event_id)
      await f.notes.apply(scope, 2)
      await received(f, 3, 3, update.event_id, 'delete')
      await f.notes.apply(scope, 3)
      states.push(await opened(f))
      expect(JSON.stringify(f.db.localProjects)).not.toContain('PRIVATE_CAPTURE_PROJECT')
      expect(JSON.stringify(f.db.localNotes)).not.toContain('PRIVATE_CAPTURE_BODY')
    }
    expect(states[0]).toEqual(states[1])
    expect(states[0]).toMatchObject({ source_revision: 4, notes: [{ state: 'tombstone', revision: 0, note: { id: 'remote-note' } }] })
  })

  it('rejects source growth beyond the existing byte limit without publishing a Note or proof', async () => {
    const f = await imported(), snapshot = await f.local.readSnapshot(scope, 'p'), source = await opened(f)
    const template = { local_format: 1, kind: 'project', account_id: scope.accountId, device_id: scope.deviceId,
      project_id: 'p', source_revision: 2, deleted_at: null, project: { ...source.project, payload: { pad: '' } }, member_ids: [] }
    const bytes = localCanonicalBytes(template)
    const pad = 'x'.repeat(MAX_LOCAL_SOURCE_BYTES - bytes.length)
    bytes.fill(0)
    await f.local.replaceProject(scope, snapshot.token, { ...source.project, payload: { pad } })
    await received(f, 1)
    const saved = structuredClone(f.db)
    await expect(f.notes.prepare(scope, 1)).rejects.toMatchObject({ code: 'source_limit' })
    expect(f.db).toEqual(saved)
  }, 20_000)

  it.each(['lock', 'logout'] as const)('%s during preparation leaves no durable write', async kind => {
    const f = await imported()
    await received(f, 1)
    const saved = structuredClone(f.db), preparing = expect(f.notes.prepare(scope, 1)).rejects.toThrow()
    if (kind === 'lock') f.lock(); else await f.auth.logout()
    await preparing
    expect(f.db).toEqual(saved)
  })

  it('an unverified caller cannot use the protected source path to write', async () => {
    const f = await imported(), saved = structuredClone(f.db)
    await expect(f.local.prepareProtectedNoteApply(scope, {} as never, {} as never)).rejects.toMatchObject({ code: 'stale_source' })
    expect(f.db).toEqual(saved)
  })
})
