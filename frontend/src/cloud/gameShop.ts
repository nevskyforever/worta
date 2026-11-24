import type { GameInventory, GameItem, InventoryCommand, ShopCatalog } from '@/types/game'
import type { AccountGameAction, AccountGameBase, AccountGameEvent } from './gameCodec'
import { transitionGame } from './gameProjectTransition'
import { progressMicros } from './progressCodec'
import { canonicalizeSyncTimestamp } from './syncTimestamp'
import catalog from './gameShopCatalog.json'

const roundMoney = (amount: number) => Math.round(amount * 10) / 10
const fromAmount = (value: string) => Number(progressMicros(value)) / 1e6
const findItem = (category: string, key: string) => catalog.find(item => item.category === category && item.key === key)
const buyPrice = (item: typeof catalog[number], level: number) =>
  item.apply_inflation ? roundMoney(item.price * (1 + (level - 1) * 0.15)) : item.price
const getCount = (base: AccountGameBase, category: string, key: string) =>
  base.inventory.find(item => item.category === category && item.item_id === key)?.count ?? 0

export type GameShopCommand = InventoryCommand & { operation: 'buy' | 'sell' }

export function quoteGameShopAction(base: AccountGameBase, command: GameShopCommand): Extract<AccountGameAction, { kind: 'inventory' }> | null {
  const { operation, category, item_id, count } = command
  const item = findItem(category, item_id)
  if (!item || !Number.isInteger(count) || count < 1 || count > 10_000
    || (operation === 'buy' ? !item.buyable || base.level < item.level : operation !== 'sell' || !item.sellable)) {
    return null
  }
  const beforeCount = getCount(base, category, item_id)
  const afterCount = beforeCount + (operation === 'buy' ? count : -count)
  if (afterCount < 0 || afterCount > 10_000 || (operation === 'buy' && afterCount > (item.maximum ?? 10_000))) {
    return null
  }
  const unitPrice = operation === 'buy' ? buyPrice(item, base.level) : roundMoney(item.price * 0.75)
  const totalPrice = operation === 'buy' ? roundMoney(unitPrice * count) : roundMoney(item.price * 0.75 * count)
  const coins = fromAmount(base.coins)
  if (operation === 'buy' && coins < totalPrice) return null
  const nextCoins = roundMoney(coins + (operation === 'buy' ? -totalPrice : totalPrice))
  const delta = (nextCoins - coins).toFixed(6)
  const micros = progressMicros(delta), quantity = BigInt(count)
  const frozenUnitPrice = operation === 'sell' && micros % quantity === 0n
    ? (Number(micros / quantity) / 1e6).toFixed(6)
    : unitPrice.toFixed(6)
  const signedPrice = progressMicros(frozenUnitPrice) * quantity * (operation === 'buy' ? -1n : 1n)
  if (micros !== signedPrice || nextCoins > 1e12) return null
  return { kind: 'inventory', operation, category, item_id, count, before_count: beforeCount, after_count: afterCount,
    unit_price: frozenUnitPrice, coins_delta: delta }
}

export function prepareGameShopAction(base: AccountGameBase, head: AccountGameEvent, deviceId: string,
  command: GameShopCommand, now: string, eventId: string): AccountGameEvent {
  const action = quoteGameShopAction(base, command)
  if (!action) throw new Error('game_invalid_rule')
  const event: AccountGameEvent = {
    version: 1,
    header: { ...head.header, event_id: eventId, entity_id: 'game:' + eventId, device_id: deviceId,
      parents: [head.header.event_id], revision: head.header.revision + 1, rule: 'native-game-v1',
      updated_at: canonicalizeSyncTimestamp(now) },
    action,
  }
  transitionGame(event, base)
  return event
}

export function presentGameShop(base: AccountGameBase): { inventory: GameInventory; shop: ShopCatalog } {
  const shop = new Map<string, GameItem[]>()
  const inventory = new Map<string, GameItem[]>()
  const add = (target: Map<string, GameItem[]>, item: GameItem) => {
    const items = target.get(item.category) ?? []
    items.push(item)
    target.set(item.category, items)
  }
  for (const item of catalog) {
    const price = buyPrice(item, base.level)
    const count = getCount(base, item.category, item.key)
    const view: GameItem = {
      id: JSON.stringify([item.category, item.key]), category: item.category, key: item.key, name: item.name,
      description: item.description, effect: item.effect, price, sell_price: roundMoney(item.price * 0.75), count,
      level: item.level, maximum_quantity: item.maximum, available_for_level: base.level >= item.level,
      buy: item.buyable, can_buy: item.buyable && fromAmount(base.coins) >= price && base.level >= item.level
        && count < (item.maximum ?? 10_000), credit_allowed: false, known: true, usable: false, sellable: item.sellable,
    }
    if (view.buy) add(shop, view)
    if (count > 0) add(inventory, view)
  }
  for (const item of base.inventory) {
    if (findItem(item.category, item.item_id)) continue
    add(inventory, { id: JSON.stringify([item.category, item.item_id]), category: item.category, key: item.item_id,
      name: item.item_id, count: item.count, description: null, buy: false, sellable: false, usable: false, known: false })
  }
  const categories = (target: Map<string, GameItem[]>) => Array.from(target, ([key, items]) => ({ key, name: key, items }))
  return { inventory: { categories: categories(inventory) }, shop: { categories: categories(shop), custom_awards: { items: [] } } }
}
