import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { expect,it,vi } from 'vitest'
import golden from '@/cloud/__fixtures__/gameCodecV1.json'
import type { AccountGameBase } from '@/cloud/gameCodec'
import { useLocaleStore } from '@/stores/locale'
import { contentApi } from '@/api/content'
import { bundledLocale } from '@/content/bundledContent'
import BrowserAccountGameRead from './BrowserAccountGameRead.vue'
it('shows real account balances and canonical inventory in six languages, with no resource mutation controls',async()=>{
 vi.spyOn(contentApi,'locale').mockImplementation(async language=>bundledLocale(language))
 const event=golden.examples.find(v=>v.name==='account-genesis')!.event
 if(event.action.kind!=='genesis'||!event.action.base||!('coins' in event.action.base))throw Error('fixture')
 const value=structuredClone(event.action.base) as AccountGameBase,pinia=createPinia(),locale=useLocaleStore(pinia)
 const wrapper=mount(BrowserAccountGameRead,{props:{value},global:{plugins:[pinia],stubs:{IonIcon:true}}})
 for(const language of ['ru','en','es','de','fr','pt_BR'] as const){
  await locale.setLanguage(language)
  expect(wrapper.text()).toContain(locale.translate('Параметры персонажа'))
  expect(wrapper.text()).toContain(locale.translate('Навыки'))
  expect(wrapper.get('[data-testid="cloud-game-coins"]').text()).toBe(locale.formatNumber(Number(value.coins),2))
  expect(wrapper.get('[data-testid="cloud-game-level"]').text()).toBe(String(value.level))
  expect(wrapper.findAll('button')).toHaveLength(0)
  expect(wrapper.findAll('input')).toHaveLength(0)
  expect(wrapper.get('.count-badge').text()).toBe('×2')
 }
 const next=structuredClone(value);next.coins='225.000000';next.inventory[0]!.count=3
 await wrapper.setProps({value:next})
 expect(wrapper.get('[data-testid="cloud-game-coins"]').text()).toBe(locale.formatNumber(225,2))
 expect(wrapper.get('.count-badge').text()).toBe('×3')
 expect(value.inventory[0]!.count).toBe(2)
})

it('offers a sale only when the requested quantity maps exactly to a Desktop event', async () => {
  const event = golden.examples.find(example => example.name === 'account-genesis')!.event
  if (event.action.kind !== 'genesis' || !event.action.base || !('coins' in event.action.base)) throw new Error('Expected account genesis')
  const value = structuredClone(event.action.base) as AccountGameBase
  value.inventory = [{ category: 'Зелья', item_id: 'Микро зелье здоровья', count: 4 }]
  const view = mount(BrowserAccountGameRead, {
    props: { value, writable: true, view: 'inventory' },
    global: { plugins: [createPinia()], stubs: { IonIcon: true } },
  })
  const sell = () => view.get('.button-row button:last-child')
  await view.get('input[type="number"]').setValue(3)
  expect(sell().attributes('disabled')).toBeDefined()
  await view.get('input[type="number"]').setValue(2)
  expect(sell().attributes('disabled')).toBeUndefined()
  await sell().trigger('click')
  expect(view.emitted('trade')).toEqual([[{
    category: 'Зелья', item_id: 'Микро зелье здоровья', operation: 'sell', count: 2,
  }]])
})
