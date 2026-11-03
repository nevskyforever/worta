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
  it('shows only owned items, updates after the last item is removed and keeps the shop catalog', async () => {
    const data = structuredClone(inventory)
    data.categories[0]!.items[0]!.count = 0
    data.categories[0]!.items[1]!.count = 2
    data.categories[0]!.items[2]!.count = 0
    const catalog: ShopCatalog = { ...shop, categories: structuredClone(data.categories) }
    const wrapper = mount(InventoryShopPanel, {
      props: { inventory: data, shop: catalog, busy: false, view: 'inventory', canOpenCredit: false },
      global: { plugins: [createPinia()] },
    })
    expect(wrapper.findAll('.item-card')).toHaveLength(1)
    expect(wrapper.get('.count-badge').text()).toBe('×2')
    const depleted = structuredClone(data)
    depleted.categories[0]!.items[1]!.count = 0
    await wrapper.setProps({ inventory: depleted })
    expect(wrapper.findAll('.item-card')).toHaveLength(0)
    expect(wrapper.get('.empty-copy').text()).toContain('В этой категории пока нет предметов.')
    await wrapper.setProps({ view: 'shop' })
    expect(wrapper.findAll('.item-card')).toHaveLength(3)
    expect(data.categories[0]!.items).toHaveLength(3)
  })

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
