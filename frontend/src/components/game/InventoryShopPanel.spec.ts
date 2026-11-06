import { createPinia } from 'pinia'
import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import type { GameInventory, ShopCatalog } from '@/types/game'

import InventoryShopPanel from './InventoryShopPanel.vue'

const inventory: GameInventory = {
  categories: [
    {
      key: 'Предметы',
      name: 'Предметы',
      items: [
        {
          id: 'Предметы:Лотерейный билет',
          key: 'Лотерейный билет',
          category: 'Предметы',
          name: '🎟️ Лотерейный билет',
          description: 'Лотерея',
          effect: 'Испытать удачу',
          count: 1,
          sellable: false,
          usable: true,
          buy: true,
        },
        {
          id: 'Предметы:Заморозка',
          key: 'Заморозка',
          category: 'Предметы',
          name: '❄️ Заморозка',
          description: 'Сохранить серию',
          count: 1,
          sellable: true,
          usable: true,
          buy: true,
        },
        {
          id: 'Предметы:Печатная машинка Хемингуэя',
          key: 'Печатная машинка Хемингуэя',
          category: 'Предметы',
          name: '📠 Печатная машинка Хемингуэя',
          description: 'Постоянный бонус',
          effect: 'Опыт +0,5',
          count: 1,
          sellable: true,
          usable: false,
          buy: true,
        },
      ],
    },
    { key: 'Зелья', name: 'Зелья', items: [] },
    { key: 'Награды', name: 'Награды', items: [] },
  ],
}

const shop: ShopCatalog = {
  categories: [{ key: 'Зелья', name: 'Зелья', items: [] }],
  custom_awards: { items: [] },
}

describe('InventoryShopPanel', () => {
  it('restores and emits the persisted inventory category without changing shop state', async () => {
    const wrapper = mount(InventoryShopPanel, {
      props: {
        inventory,
        shop,
        busy: false,
        view: 'inventory',
        canOpenCredit: false,
        initialInventoryCategory: 'Награды',
      },
      global: { plugins: [createPinia()] },
    })

    const category = wrapper.get('.item-toolbar select')
    expect((category.element as HTMLSelectElement).value).toBe('Награды')

    await category.setValue('Зелья')

    expect(wrapper.emitted('inventoryCategory')?.[0]).toEqual(['Зелья'])
  })

  it('uses the explicit usable flag and routes freeze to the streak selector', async () => {
    const wrapper = mount(InventoryShopPanel, {
      props: { inventory, shop, busy: false, view: 'inventory', canOpenCredit: false },
      global: { plugins: [createPinia()] },
    })

    expect(wrapper.findAll('.item-card')).toHaveLength(3)
    expect(wrapper.get('.item-card').text()).toContain('Использовать')
    const freezeCard = wrapper.findAll('.item-card')[1]
    expect(freezeCard?.text()).toContain('Выбрать серию')
    const permanentCard = wrapper.findAll('.item-card')[2]
    expect(permanentCard?.text()).not.toContain('Использовать')
    expect(permanentCard?.text()).toContain('Купить')
    const freezeButton = freezeCard?.findAll('button').find(
      (button) => button.text() === 'Выбрать серию',
    )
    await freezeButton?.trigger('click')
    expect(wrapper.emitted('freeze')).toHaveLength(1)
  })

  it('adds an item to the cart when Buy is clicked with Shift', async () => {
    const cartInventory: GameInventory = structuredClone(inventory)
    cartInventory.categories[0]!.items[0]!.can_buy = true
    const wrapper = mount(InventoryShopPanel, {
      props: { inventory: cartInventory, shop, busy: false, view: 'inventory', canOpenCredit: false },
      global: { plugins: [createPinia()] },
    })

    const buyButton = wrapper.findAll('.item-card')[0]?.find('.nf-button')
    await buyButton?.trigger('click', { shiftKey: true })

    expect(wrapper.emitted('addToCart')?.[0]?.[0]).toMatchObject({
      key: 'Лотерейный билет',
    })
  })

  it('keeps a credit-eligible item purchasable when coins are insufficient', async () => {
    const creditInventory: GameInventory = structuredClone(inventory)
    const item = creditInventory.categories[0]!.items[0]!
    item.can_buy = false
    item.available_for_level = true
    item.credit_allowed = true
    const wrapper = mount(InventoryShopPanel, {
      props: {
        inventory: creditInventory,
        shop,
        busy: false,
        view: 'inventory',
        canOpenCredit: true,
      },
      global: { plugins: [createPinia()] },
    })

    const buyButton = wrapper.findAll('.item-card')[0]?.find('.nf-button')
    expect(buyButton?.attributes('disabled')).toBeUndefined()
    await buyButton?.trigger('click')
    expect(wrapper.emitted('buy')?.[0]?.[0]).toMatchObject({
      item_id: 'Лотерейный билет',
    })
  })

  it('disables buying when inventory has reached the item maximum', () => {
    const fullInventory: GameInventory = structuredClone(inventory)
    const item = fullInventory.categories[0]!.items[0]!
    item.count = 1
    item.maximum_quantity = 1
    item.can_buy = true
    const wrapper = mount(InventoryShopPanel, {
      props: {
        inventory: fullInventory,
        shop,
        busy: false,
        view: 'inventory',
        canOpenCredit: true,
      },
      global: { plugins: [createPinia()] },
    })

    const buyButton = wrapper.findAll('.item-card')[0]?.find('.nf-button')
    expect(buyButton?.attributes('disabled')).toBeDefined()
  })
})

it('retains selectable authenticated inventory while hiding mutation controls in read-only mode', async () => {
  const wrapper = mount(InventoryShopPanel, {
    props: { inventory, shop, busy: false, view: 'inventory', canOpenCredit: false, readOnly: true },
    global: { plugins: [createPinia()] },
  })
  expect(wrapper.findAll('.item-card')).toHaveLength(3)
  expect(wrapper.findAll('button')).toHaveLength(0)
  expect(wrapper.find('input').exists()).toBe(false)
  await wrapper.get('select').setValue('Зелья')
  expect(wrapper.findAll('.item-card')).toHaveLength(0)
  expect(wrapper.emitted('buy')).toBeUndefined()
  expect(wrapper.emitted('use')).toBeUndefined()
  await wrapper.setProps({readOnly:false})
  await wrapper.get('select').setValue('Предметы')
  expect(wrapper.findAll('button').length).toBeGreaterThan(0)
})
