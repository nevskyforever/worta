import { describe, expect, it, vi } from 'vitest'
import { encodeV3DocumentPush, encodeV3MapPush, encodeV3MetadataPush, encryptedSyncV3Api, parseV3Pull } from './encryptedSyncV3'

const DEVICE = '123e4567-e89b-42d3-a456-426614174003'
const EVENT = '123e4567-e89b-42d3-a456-426614174010'
const NOW = '2026-09-21T00:00:00.000000Z'
const object = { crypto_version: 1 as const, aad_version: 1 as const,
  nonce: new Uint8Array(24), ciphertext: new Uint8Array(32) }
const descriptor = { event_id: EVENT, device_id: DEVICE, server_sequence: 1, project_id: 'project', entity_id: 'project',
  entity_type: 'project_metadata' as const, operation: 'upsert' as const, revision: 1, updated_at: NOW, deleted_at: null }

describe('mode-3 metadata transport boundary', () => {
  it('retries a recovery rate refusal with the same pinned snapshot and remains cancellable', async () => {
    vi.useFakeTimers()
    const page = { delivery_version: 1, current_cursor: 0, cutover_epoch: 2, writer_transport_version: 3,
      items: [], next_cursor: 0, has_more: false }
    const fetch = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ detail: { code: 'recovery_rate_limited' } }), { status: 429 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(page)))
    try {
      const reading = encryptedSyncV3Api.recoveryRaw('token', 0, 100, 0, 2)
      await vi.advanceTimersByTimeAsync(1000)
      expect(JSON.parse(await reading).items).toEqual([])
      expect(fetch.mock.calls[0]![0]).toBe(fetch.mock.calls[1]![0])
      const controller = new AbortController()
      fetch.mockResolvedValueOnce(new Response('{}', { status: 429 }))
      const cancelled = expect(encryptedSyncV3Api.recoveryRaw('token', 0, 100, 0, 2, controller.signal)).rejects.toMatchObject({ name: 'AbortError' })
      await vi.advanceTimersByTimeAsync(0)
      controller.abort()
      await cancelled
      await vi.advanceTimersByTimeAsync(1000)
      expect(fetch).toHaveBeenCalledTimes(3)
    } finally { fetch.mockRestore(); vi.useRealTimers() }
  })
  it('validates the account recovery snapshot and preserves the encrypted wire', async () => {
    const item = { event: descriptor, object: { crypto_version: 1, aad_version: 1,
      nonce: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', ciphertext: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' } }
    const page = { delivery_version: 1, current_cursor: 1, cutover_epoch: 2, writer_transport_version: 3,
      items: [item], next_cursor: 1, has_more: false }
    const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(JSON.stringify(page)))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ...page, current_cursor: 2 })))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ...page, items: [{ ...item, event: { ...descriptor, server_sequence: 2 } }] })))
    expect(JSON.parse(await encryptedSyncV3Api.recoveryRaw('token', 0, 1, 1, 2)).items).toEqual([item])
    await expect(encryptedSyncV3Api.recoveryRaw('token', 0, 1, 1, 2)).rejects.toThrow()
    await expect(encryptedSyncV3Api.recoveryRaw('token', 0, 1, 1, 2)).rejects.toThrow()
    fetch.mockRestore()
  })
  it('returns only validated original encrypted wire for replay and propagates cancellation', async () => {
    const page = { protocol_version: 3, encrypted_sync_version: 3, items: [{ event: descriptor, object: {
      crypto_version: 1, aad_version: 1, nonce: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', ciphertext: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    } }], next_cursor: 1, has_more: false }
    const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(JSON.stringify(page), { status: 200 }))
    const controller = new AbortController()
    expect(JSON.parse(await encryptedSyncV3Api.pullRaw('token', DEVICE, 0, 100, controller.signal))).toEqual(page)
    expect(fetch.mock.calls[0]![1]?.signal).toBe(controller.signal)
    fetch.mockResolvedValueOnce(new Response(JSON.stringify({ ...page, plaintext: 'DO_NOT_STORE' }), { status: 200 }))
    await expect(encryptedSyncV3Api.pullRaw('token', DEVICE, 0)).rejects.toThrow('invalid_encrypted_sync_v3')
  })
  it('document writer uses opaque stable identity and cannot mix Note/Map/metadata descriptors',()=>{const event={event_id:EVENT,project_id:'project',entity_id:'stable-document',entity_type:'document' as const,operation:'event' as const,revision:1,updated_at:NOW,deleted_at:null};const body=JSON.parse(encodeV3DocumentPush(DEVICE,[{event,object}]));expect(parseV3Pull({protocol_version:3,encrypted_sync_version:3,items:[{event:{...event,device_id:DEVICE,server_sequence:1},object:body.items[0].object}],next_cursor:1,has_more:false},0,8).items[0]!.event.entity_type).toBe('document');for(const change of [{entity_type:'map'},{entity_type:'note'},{operation:'upsert'},{deleted_at:NOW},{title:'plaintext'}])expect(()=>encodeV3DocumentPush(DEVICE,[{event:{...event,...change} as never,object}])).toThrow();expect(()=>encodeV3MapPush(DEVICE,[{event,object}])).toThrow();expect(()=>encodeV3MetadataPush(DEVICE,[{event,object}])).toThrow()})
  it('admits only opaque owning-map descriptors on the separate map writer', () => {
    for (const entity_id of ['project-map', 'stage-map-' + 'a'.repeat(64)]) {
      const event = {event_id:EVENT,project_id:'project',entity_id,entity_type:'map' as const,
        operation:'event' as const,revision:1,updated_at:NOW,deleted_at:null}
      const wire = JSON.parse(encodeV3MapPush(DEVICE,[{event,object}]))
      const page = {protocol_version:3,encrypted_sync_version:3,items:[{
        event:{...event,device_id:DEVICE,server_sequence:1},object:wire.items[0].object
      }],next_cursor:1,has_more:false}
      expect(parseV3Pull(page,0,10).items[0]!.event.entity_type).toBe('map')
      expect(() => encodeV3MetadataPush(DEVICE,[{event,object}])).toThrow()
      for (const change of [{entity_id:'combined-map'},{operation:'upsert'},
        {deleted_at:NOW},{entity_type:'note'},{entity_id:'stage-map-'+'A'.repeat(64)}]) {
        const invalid = {...event,...change}
        expect(() => encodeV3MapPush(DEVICE,[{event:invalid,object}] as never)).toThrow()
        if (change.entity_type !== 'note') expect(() => parseV3Pull({...page,items:[{
          event:{...invalid,device_id:DEVICE,server_sequence:1},object:wire.items[0].object
        }]},0,10)).toThrow()
      }
    }
  })
  it('retains metadata publication and exact project scope', () => {
    const event = { event_id: EVENT, project_id: 'project', entity_id: 'project', entity_type: 'project_metadata' as const,
      operation: 'upsert' as const, revision: 1, updated_at: NOW, deleted_at: null }
    const valid = { event, object }
    const encoded = JSON.parse(encodeV3MetadataPush(DEVICE, [valid]))
    expect(encoded.items[0].event.entity_type).toBe('project_metadata')
    expect(() => encodeV3MetadataPush(DEVICE, [{ ...valid, event: { ...event, entity_type: 'note' } }] as never)).toThrow()
    expect(() => encodeV3MetadataPush(DEVICE, [{ ...valid, event: { ...event, entity_id: 'wrong' } }])).toThrow()
    expect(() => encodeV3MetadataPush(DEVICE, [{ ...valid, event: { ...event, operation: 'delete', deleted_at: null } }] as never)).toThrow()
  })

  it('reads a bounded mixed Note/metadata page and rejects unsupported or mismatched descriptors', () => {
    const wireObject = { crypto_version: 1, aad_version: 1, nonce: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
      ciphertext: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' }
    const note = { ...descriptor, event_id: '123e4567-e89b-42d3-a456-426614174011', server_sequence: 2,
      entity_type: 'note', entity_id: 'note-1' }
    const page = { protocol_version: 3, encrypted_sync_version: 3, items: [
      { event: descriptor, object: wireObject }, { event: note, object: wireObject },
    ], next_cursor: 2, has_more: false }
    expect(parseV3Pull(page, 0, 200).items.map(item => item.event.entity_type)).toEqual(['project_metadata', 'note'])
    expect(() => parseV3Pull({ ...page, items: [{ event: { ...descriptor, entity_id: 'wrong' }, object: wireObject }], next_cursor: 1 }, 0, 200)).toThrow()
    expect(() => parseV3Pull({ ...page, items: [{ event: { ...descriptor, entity_type: 'future' }, object: wireObject }], next_cursor: 1 }, 0, 200)).toThrow()
    expect(() => parseV3Pull({ ...page, items: [{ event: { ...descriptor, server_sequence: 3 }, object: wireObject }], next_cursor: 3 }, 3, 200)).toThrow()
  })
  it('publishes and reads opaque structural descriptors with exact type/operation pairing', () => {
    for (const kind of ['stage', 'stage_order'] as const) {
      const event = {event_id:EVENT,project_id:'project',entity_id:kind==='stage'?'S1':'stage_order',entity_type:kind,operation:'upsert' as const,revision:1,updated_at:NOW,deleted_at:null}
      const wire = JSON.parse(encodeV3MetadataPush(DEVICE,[{event,object}]))
      const parsed = parseV3Pull({protocol_version:3,encrypted_sync_version:3,items:[{event:{...event,device_id:DEVICE,server_sequence:1},object:wire.items[0].object}],next_cursor:1,has_more:false},0,10)
      expect(parsed.items[0]!.event.entity_type).toBe(kind)
      expect(Object.keys(wire.items[0].event)).not.toContain('stage_ids')
      expect(() => encodeV3MetadataPush(DEVICE,[{event:{...event,operation:'resolution',revision:2},object}])).toThrow()
      if (kind==='stage_order') expect(() => encodeV3MetadataPush(DEVICE,[{event:{...event,entity_id:'other'},object}])).toThrow()
    }
  })

})

it('accepts exact mixed account descriptors and rejects cross-scope/version frames',()=>{
  const event={event_id:EVENT,device_id:DEVICE,server_sequence:1,canonical_user_id:DEVICE,scope:'account',entity_id:'каталог/📁',entity_type:'folder',operation:'upsert',revision:1,updated_at:NOW,deleted_at:null}
  const object={crypto_version:2,aad_version:2,nonce:'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',ciphertext:'AAAAAAAAAAAAAAAAAAAAAA'}
  const page={protocol_version:3,encrypted_sync_version:3,items:[{event,object}],next_cursor:1,has_more:false}
  expect(parseV3Pull(page,0,1).items[0]!.object.crypto_version).toBe(2)
  for(const change of [{scope:'project'},{project_id:'fake'},{entity_type:'note'},{canonical_user_id:'other'},{entity_id:'😀'.repeat(129)}]) expect(()=>parseV3Pull({...page,items:[{event:{...event,...change},object}]},0,1)).toThrow()
  for(const change of [{crypto_version:1},{aad_version:1}]) expect(()=>parseV3Pull({...page,items:[{event,object:{...object,...change}}]},0,1)).toThrow()
})

it('routes immutable project and account Game envelopes without accepting cross-domain descriptors',async()=>{
  const {encodeV3GamePush,encodeAccountPush}=await import('./encryptedSyncV3')
  for(const entity_id of [`game:project:${EVENT}`,`game:stage:S1:${EVENT}`]){
    const event={event_id:EVENT,project_id:'project',entity_id,entity_type:'project_game' as const,operation:'event' as const,revision:1,updated_at:NOW,deleted_at:null}
    const wire=JSON.parse(encodeV3GamePush(DEVICE,[{event,object}]))
    expect(parseV3Pull({protocol_version:3,encrypted_sync_version:3,items:[{event:{...event,device_id:DEVICE,server_sequence:1},object:wire.items[0].object}],next_cursor:1,has_more:false},0,8).items[0]!.event.entity_type).toBe('project_game')
    for(const change of [{entity_id:`game:project:${DEVICE}`},{operation:'upsert'},{operation:'delete',deleted_at:NOW},{entity_type:'progress'},{amount:999}])expect(()=>encodeV3GamePush(DEVICE,[{event:{...event,...change} as never,object}])).toThrow()
    expect(()=>encodeV3MetadataPush(DEVICE,[{event,object}])).toThrow()
  }
  const event={event_id:EVENT,canonical_user_id:DEVICE,scope:'account' as const,entity_id:`game:${EVENT}`,entity_type:'account_game' as const,operation:'upsert' as const,revision:1,updated_at:NOW,deleted_at:null}
  const envelope={...object,crypto_version:2 as const,aad_version:2 as const}
  const wire=JSON.parse(encodeAccountPush(DEVICE,[{event,object:envelope}]))
  const page={protocol_version:3,encrypted_sync_version:3,items:[{event:{...event,device_id:DEVICE,server_sequence:1},object:wire.items[0].object}],next_cursor:1,has_more:false}
  expect(parseV3Pull(page,0,8).items[0]!.event.entity_type).toBe('account_game')
  for(const change of [{entity_id:`game:${DEVICE}`},{operation:'delete',deleted_at:NOW},{scope:'project'},{coins:999}]){
    expect(()=>encodeAccountPush(DEVICE,[{event:{...event,...change} as never,object:envelope}])).toThrow()
    expect(()=>parseV3Pull({...page,items:[{...page.items[0],event:{...page.items[0]!.event,...change}}]},0,8)).toThrow()
  }
})
