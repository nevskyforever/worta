import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { expect, it } from 'vitest'
import Panel from './LocalWordBindingPanel.vue'

function panel(attached = false, state = attached ? 'external_proposal' : 'unlinked') {
  return mount(Panel, {
    props: {
      attached,
      state,
      externalContent: {
        type: 'doc',
        content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Local external manuscript' }] }],
      },
    },
    global: { plugins: [createPinia()] },
  })
}

it('shows only reattach for an unlinked local file and no duplicate export action', () => {
  const wrapper = panel()
  expect(wrapper.emitted()).toEqual({})
  expect(wrapper.find('[data-action="reattach"]').exists()).toBe(true)
  expect(wrapper.find('[data-action="copy"]').exists()).toBe(false)
  expect(wrapper.find('[data-action="cloud"]').exists()).toBe(false)
  expect(wrapper.find('[data-action="import"]').exists()).toBe(false)
  expect(wrapper.text()).toContain('не подключён')
  wrapper.unmount()
})

it('hides conflict actions while the Word binding is synchronized', () => {
  const wrapper = panel(true, 'synced')
  expect(wrapper.text()).toContain('Файл Word связан')
  expect(wrapper.find('.local-word-binding__actions').exists()).toBe(false)
  wrapper.unmount()
})

it('offers conflict decisions only when a linked file needs resolution', async () => {
  const wrapper = panel(true, 'external_proposal')
  expect(wrapper.find('details.local-word-binding__decisions').attributes('open')).toBeUndefined()
  expect(wrapper.find('[data-action="reattach"]').exists()).toBe(false)
  expect(wrapper.find('[data-action="copy"]').exists()).toBe(false)
  for (const choice of ['compare', 'cloud', 'import', 'unlink'] as const) {
    await wrapper.get(`[data-action="${choice}"]`).trigger('click')
  }
  expect(wrapper.emitted('decision')).toEqual([['compare'], ['cloud'], ['import'], ['unlink']])
  wrapper.unmount()
})

it('allows reattachment when an existing local file is missing', () => {
  const wrapper = panel(true, 'missing_external')
  expect(wrapper.find('[data-action="reattach"]').exists()).toBe(true)
  expect(wrapper.find('[data-action="compare"]').exists()).toBe(false)
  expect(wrapper.find('[data-action="unlink"]').exists()).toBe(false)
  wrapper.unmount()
})
