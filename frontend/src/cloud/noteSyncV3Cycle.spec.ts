import { afterEach, describe, expect, it, vi } from 'vitest'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { ApiError } from '@/api/client'
import { NormalUserAuthRuntime } from '@/auth/userAuth'
import { NoteSyncV3Cycle } from './noteSyncV3Cycle'

const USER = '123e4567-e89b-42d3-a456-426614174099'
const DEVICE = '123e4567-e89b-42d3-a456-426614174003'
const PAGE = { committed_cursor: 1, new_events: 1, replayed_events: 0, has_more: false }

async function setup(structuralEnabled = false, accountEnabled = false, contentEnabled = false, mapEnabled = false, documentEnabled = false, actionsEnabled = false, contentWriters = false) {
  const auth = new NormalUserAuthRuntime({
    login: vi.fn().mockResolvedValue({ access_token: 'token', refresh_token: 'refresh', access_expires_in: 60 }),
    refresh: vi.fn(), logout: vi.fn(), me: vi.fn().mockResolvedValue({ id: USER, username: 'u',
      email: 'u@example.test', email_verified: true, role: 'user', status: 'active', created_at: 'now' }),
  })
  await auth.login('u', 'p')
  const context = auth.requireContext()
  let writerMode = 3
  const calls: string[] = []
  const capabilities = vi.spyOn(encryptedSyncV2Api, 'capabilities').mockImplementation(async () => ({
    supported_transport_version: 2, writer_transport_version: writerMode, cutover_epoch: 1,
  }) as never)
  const bindings = { ensureForCurrentUser: vi.fn(async () => ({ context })) }
  const identity = { read: vi.fn(async () => ({ local_account_id: 'local', device_id: DEVICE })) }
  const keys = { leaseForAccount: vi.fn(() => ({ canonicalUserId: USER, authEpoch: context.authEpoch,
    isCurrent: () => true })) }
  const device = { registerOnce: vi.fn(async () => { calls.push('register') }) }
  const intents = { list: vi.fn(async () => { calls.push('seal_notes'); return [] }),
    recordSealFailure: vi.fn(), commitSealedEvent: vi.fn() }
  const note = { uploadOnce: vi.fn(async () => { calls.push('upload_notes'); return { uploaded: 1 } }) }
  const resolution = { uploadOnce: vi.fn(async () => { calls.push('upload_resolutions'); return { uploaded: 1 } }) }
  const noteApply = { runMixedInboxOnce: vi.fn(async () => { calls.push('apply_notes'); return {
    blocked: [], errors: [], hasRemainingWork: false,
  } }) }
  const metadata = {
    sealOnce: vi.fn(async () => { calls.push('seal_metadata'); return 1 }),
    uploadOnce: vi.fn(async () => { calls.push('upload_metadata'); return 1 }),
    pullOnce: vi.fn(async () => { calls.push('pull'); return PAGE }),
    applyOnce: vi.fn(async () => { calls.push('apply_metadata'); return { applied: 1, conflicts: 0,
      orphans: 0, blocked: [], listed: 1 } }),
    ackOnce: vi.fn(async () => { calls.push('ack'); return { status: 'advanced', cursor: 1 } }),
  }
  const structural = {
    sealOnce: vi.fn(async () => { calls.push('seal_structure'); return 0 }),
    uploadOnce: vi.fn(async () => { calls.push('upload_structure'); return 0 }),
    applyOnce: vi.fn(async () => { calls.push('apply_structure'); return { applied: 0, conflicts: 0, orphans: 0, blocked: [], listed: 0 } }),
  }
  const account = { sealCatalog: vi.fn().mockResolvedValue(0), readOnce: vi.fn(async () => { calls.push('apply_account'); return { blocked: ['account_entity_codec_not_activated'], listed: 1, hasRemainingWork: false } }) }
  const content = { sealNotes: vi.fn().mockResolvedValue(0), readOnce: vi.fn(async () => { calls.push('apply_content'); return { blocked: ['orphan'], listed: 1, hasRemainingWork: true } }) }
  const maps = {
    declareSupport: vi.fn(async () => { calls.push('declare_maps') }),
    sealMaps: vi.fn(async () => { calls.push('seal_maps'); return 0 }),
    uploadMaps: vi.fn(async () => { calls.push('upload_maps'); return 0 }),
    readOnce: vi.fn(async () => { calls.push('apply_maps'); return { blocked: [], listed: 1,
      applied: 1, conflicts: 0, hasRemainingWork: false } }),
  }
  const documents={declareSupport:vi.fn(async()=>{calls.push('declare_documents')}),sealDocuments:vi.fn(async()=>{calls.push('seal_documents');return 0}),uploadDocuments:vi.fn(async()=>{calls.push('upload_documents');return 0}),readOnce:vi.fn(async()=>{calls.push('apply_documents');return {listed:1,applied:0,conflicts:0,blocked:['document_unsupported_extension'],hasRemainingWork:true}})}
  const progress={declareSupport:vi.fn(),beginProgress:vi.fn(),sealProgress:vi.fn().mockResolvedValue(0),uploadProgress:vi.fn().mockResolvedValue(0),readOnce:vi.fn(async()=>{calls.push('apply_progress');return {listed:2,applied:1,conflicts:0,blocked:['progress_parent_unknown'],hasRemainingWork:true}})}
  const game={gameReaderAvailable:vi.fn().mockResolvedValue(true),declareGameSupport:vi.fn(),beginGame:vi.fn(),sealGame:vi.fn().mockResolvedValue(0),uploadGame:vi.fn().mockResolvedValue(0),readGameOnce:vi.fn(async()=>{calls.push('apply_game');return {listed:2,applied:1,conflicts:0,blocked:['game_source_dependency_unknown'],hasRemainingWork:true}})}
  const cycle = new NoteSyncV3Cycle(auth, bindings as never, identity as never, keys as never,
    device as never, intents as never, note as never, resolution as never, noteApply as never, metadata as never, structuralEnabled ? structural as never : undefined, accountEnabled ? (contentWriters ? account : { readOnce: account.readOnce }) as never : undefined, contentEnabled ? (contentWriters ? content : { readOnce: content.readOnce }) as never : undefined, mapEnabled ? maps as never : undefined, documentEnabled ? documents as never : undefined, actionsEnabled ? progress as never : undefined, actionsEnabled ? game as never : undefined)
  return { progress, game, documents, account, content, structural, maps, cycle, calls, capabilities, intents, note, resolution, metadata,
    mode: (value: number) => { writerMode = value } }
}

afterEach(() => vi.restoreAllMocks())

describe('mode-3 Note and metadata cycle', () => {
  it('retains a network failure separately from a capability blocker', async () => {
    const h = await setup()
    h.capabilities.mockRejectedValueOnce(new ApiError(0, 'network_error', 'offline'))
    expect((await h.cycle.runOnce('local', DEVICE)).errors).toEqual([{ stage: 'preflight', code: 'network_error' }])
    h.capabilities.mockRejectedValueOnce(new ApiError(503, 'note_readers_not_ready', 'blocked'))
    expect((await h.cycle.runOnce('local', DEVICE)).errors).toEqual([{ stage: 'preflight', code: 'note_readers_not_ready' }])
  })
  it('ends a timed-out upload without making more requests and can retry the cycle', async () => {
    const h = await setup()
    h.metadata.uploadOnce.mockRejectedValueOnce(new ApiError(0, 'request_timeout', 'timeout'))
    const timedOut = await h.cycle.runOnce('local', DEVICE)
    expect(timedOut.errors).toEqual([{ stage: 'upload_metadata', code: 'request_timeout' }])
    expect(timedOut.hasRemainingWork).toBe(true)
    expect(h.metadata.pullOnce).not.toHaveBeenCalled()
    expect(h.metadata.ackOnce).not.toHaveBeenCalled()

    const retried = await h.cycle.runOnce('local', DEVICE)
    expect(retried.errors).toEqual([])
    expect(h.metadata.ackOnce).toHaveBeenCalledOnce()
  })

  it('continues every family through several blockers without migration consent',async()=>{
    const h=await setup(true,true,true,true,true,true)
    for(let pass=0;pass<2;pass++){
      const start=h.calls.length
      const result=await h.cycle.runOnce('local',DEVICE)
      expect(result.blocked).toEqual(expect.arrayContaining(['orphan','document_unsupported_extension','progress_parent_unknown','game_source_dependency_unknown']))
      expect(result.hasRemainingWork).toBe(true)
      const calls=h.calls.slice(start)
      for(const family of ['apply_content','apply_maps','apply_documents','apply_progress','apply_game','ack'])expect(calls).toContain(family)
      expect(h.calls.indexOf('apply_progress')).toBeLessThan(h.calls.indexOf('apply_game'))
    }
    expect(h.progress.beginProgress).not.toHaveBeenCalled();expect(h.game.beginGame).not.toHaveBeenCalled()
  })

  it('runs the framed Note reader after dependencies and before common ACK without a content writer', async () => {
    const h=await setup(true,true,true),r=await h.cycle.runOnce('local',DEVICE)
    expect(h.calls.indexOf('apply_structure')).toBeLessThan(h.calls.indexOf('apply_content'))
    expect(h.calls.indexOf('apply_metadata')).toBeLessThan(h.calls.indexOf('apply_content'))
    expect(h.calls.indexOf('apply_content')).toBeLessThan(h.calls.indexOf('apply_account'))
    expect(h.calls.indexOf('apply_content')).toBeLessThan(h.calls.indexOf('ack'))
    expect(r.blocked).toContain('orphan');expect(r.hasRemainingWork).toBe(true)
    expect(r.stages.filter(s=>s.includes('content'))).toEqual(['apply_content_notes'])
  })
  it('documents apply after dependencies and before contiguous ACK even after lost upload',async()=>{const h=await setup(true,true,true,true,true);h.documents.uploadDocuments.mockRejectedValueOnce(new Error('lost_response'));const r=await h.cycle.runOnce('local',DEVICE);expect(h.calls.indexOf('apply_structure')).toBeLessThan(h.calls.indexOf('apply_documents'));expect(h.calls.indexOf('apply_maps')).toBeLessThan(h.calls.indexOf('apply_documents'));expect(h.calls.indexOf('apply_documents')).toBeLessThan(h.calls.indexOf('ack'));expect(r.documentApply?.blocked).toEqual(['document_unsupported_extension']);expect(r.hasRemainingWork).toBe(true)})
  it('runs one bounded mixed cycle and ACKs only after both appliers', async () => {
    const h = await setup()
    const result = await h.cycle.runOnce('local', DEVICE)
    expect(result.stages).toEqual(['preflight', 'register_device', 'seal_notes', 'seal_metadata',
      'upload_notes', 'upload_resolutions', 'upload_metadata', 'pull_v3', 'apply_notes', 'apply_metadata', 'ack_v3'])
    expect(h.calls).toEqual(['register', 'seal_notes', 'seal_metadata', 'upload_notes', 'upload_resolutions',
      'upload_metadata', 'pull', 'apply_notes', 'apply_metadata', 'ack'])
    expect(result).toMatchObject({ noteUploaded: 1, resolutionUploaded: 1, metadataUploaded: 1,
      errors: [], blocked: [], hasRemainingWork: false })
  })

  it('pulls self echo after a lost metadata upload response', async () => {
    const h = await setup()
    h.metadata.uploadOnce.mockRejectedValueOnce(new Error('lost_response'))
    const result = await h.cycle.runOnce('local', DEVICE)
    expect(result.errors).toEqual([{ stage: 'upload_metadata', code: 'Error' }])
    expect(h.metadata.pullOnce).toHaveBeenCalledOnce()
    expect(h.metadata.applyOnce).toHaveBeenCalledOnce()
    expect(h.metadata.ackOnce).toHaveBeenCalledOnce()
    expect(result.hasRemainingWork).toBe(true)
  })

  it('stops on mode loss and never publishes with a stale writer authority', async () => {
    const h = await setup()
    h.intents.list.mockImplementationOnce(async () => { h.mode(2); return [] })
    const result = await h.cycle.runOnce('local', DEVICE)
    expect(result.errors[0]?.stage).toBe('seal_metadata')
    expect(h.note.uploadOnce).not.toHaveBeenCalled()
    expect(h.resolution.uploadOnce).not.toHaveBeenCalled()
    expect(h.metadata.uploadOnce).not.toHaveBeenCalled()
  })
})

it('mode-3 production dispatch applies structural arrivals before contiguous ACK and pulls after a lost structural upload', async () => {
  const h = await setup(true)
  h.structural.uploadOnce.mockRejectedValueOnce(new Error('lost response'))
  const result = await h.cycle.runOnce('local', DEVICE)
  expect(h.calls.indexOf('apply_structure')).toBeGreaterThan(h.calls.indexOf('apply_metadata'))
  expect(h.calls.indexOf('ack')).toBeGreaterThan(h.calls.indexOf('apply_structure'))
  expect(h.metadata.pullOnce).toHaveBeenCalledOnce(); expect(h.structural.applyOnce).toHaveBeenCalledOnce()
  expect(result.errors).toEqual([{ stage: 'upload_structure', code: 'Error' }])
})

it('seals structural, catalog and content-note intents even when another writer has remaining work', async () => {
  const h = await setup(true, true, true, false, false, false, true)
  h.metadata.sealOnce.mockResolvedValueOnce(8)
  const result = await h.cycle.runOnce('local', DEVICE)
  expect(result.hasRemainingWork).toBe(true)
  expect(h.structural.sealOnce).toHaveBeenCalledOnce()
  expect(h.account.sealCatalog).toHaveBeenCalledOnce()
  expect(h.content.sealNotes).toHaveBeenCalledOnce()
})


it('runs the account reader before shared ACK and preserves its typed blocker',async()=>{
  const h=await setup(false,true)
  h.metadata.ackOnce.mockImplementationOnce(async()=>{ expect(h.calls.at(-1)).toBe('apply_account'); return {status:'no_progress',cursor:0} })
  const result=await h.cycle.runOnce('local',DEVICE)
  expect(result.blocked).toContain('account_entity_codec_not_activated')
  expect(result.hasRemainingWork).toBe(true)
  expect(result.ack).toEqual({status:'no_progress',cursor:0})
})


it('applies maps after dependencies and before ACK, reports changes, and pulls after lost upload', async () => {
  const h = await setup(true, true, true, true)
  h.maps.uploadMaps.mockRejectedValueOnce(new Error('lost response'))
  const result = await h.cycle.runOnce('local', DEVICE)
  expect(h.maps.declareSupport).toHaveBeenCalledOnce()
  expect(h.calls.indexOf('apply_maps')).toBeGreaterThan(h.calls.indexOf('apply_metadata'))
  expect(h.calls.indexOf('apply_maps')).toBeGreaterThan(h.calls.indexOf('apply_structure'))
  expect(h.calls.indexOf('apply_maps')).toBeGreaterThan(h.calls.indexOf('apply_content'))
  expect(h.calls.indexOf('apply_maps')).toBeLessThan(h.calls.indexOf('ack'))
  expect(h.metadata.pullOnce).toHaveBeenCalledOnce()
  expect(result.mapApply).toMatchObject({ applied: 1, conflicts: 0, listed: 1 })
  expect(result.errors).toEqual([{ stage: 'upload_maps', code: 'Error' }])
})
