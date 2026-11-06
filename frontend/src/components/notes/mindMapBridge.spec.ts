import { describe, expect, it } from 'vitest'

import { parseMindMapData, parseMindMapEvents, safeCloudMapDisplay } from './mindMapBridge'

describe('Mind Elixir bridge adapter', () => {
  it('accepts map objects and rejects invalid or non-object payloads', () => {
    expect(parseMindMapData('{"nodeData":{"id":"root"}}')).toEqual({
      nodeData: { id: 'root' },
    })
    expect(parseMindMapData('[]')).toBeNull()
    expect(parseMindMapData('{broken')).toBeNull()
  })

  it('keeps only recognized, correctly typed editor events', () => {
    const events = parseMindMapEvents(
      JSON.stringify([
        { type: 'ready' },
        { type: 'save', payload: '{"nodeData":{}}' },
        { type: 'save', payload: 42 },
        { type: 'unknown' },
        null,
      ]),
    )

    expect(events).toEqual([
      { type: 'ready' },
      { type: 'save', payload: '{"nodeData":{}}' },
    ])
  })
})

it('isolates cloud Map labels/resources without rewriting the authenticated source', () => {
  const source = { nodeData: { id: 'root', topic: '<b>literal user text</b>', children: [], hyperLink: 'javascript:alert(1)', image: { url: 'https://foreign.invalid' }, style: { background: 'url(https://foreign.invalid)' } },
    arrows: [{ label: '<img src=x onerror=alert(1)>' }], theme: { cssVar: { background: 'url(https://foreign.invalid)' } } }
  const original = JSON.stringify(source), view = safeCloudMapDisplay(source)
  expect(JSON.stringify(source)).toBe(original)
  expect((view.nodeData as Record<string, unknown>).topic).toBe('<b>literal user text</b>')
  expect(JSON.stringify(view)).not.toMatch(/javascript:|foreign\.invalid|"style"|"theme"/)
  expect((view.arrows as { label: string }[])[0]!.label).toBe('&lt;img src=x onerror=alert(1)&gt;')
})
