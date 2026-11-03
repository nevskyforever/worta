<script setup lang="ts">
import { computed, ref, watch } from 'vue'

import { useLocaleStore } from '@/stores/locale'
import type { GameInventory, GameItem, InventoryCommand, ShopCatalog } from '@/types/game'
import { cartCapacity } from '@/utils/gameCart'

const props = defineProps<{
  inventory: GameInventory
  shop: ShopCatalog
  busy: boolean
  view: 'inventory' | 'shop'
  canOpenCredit: boolean
  initialInventoryCategory?: string
}>()

const emit = defineEmits<{
  buy: [payload: InventoryCommand]
  addToCart: [item: GameItem, count: number]
  sell: [payload: InventoryCommand]
  use: [payload: InventoryCommand]
  freeze: []
  inventoryCategory: [category: string]
}>()

const locale = useLocaleStore()
const t = locale.translate
const category = ref('')
const count = ref(1)

const categories = computed(() =>
  props.view === 'inventory'
    ? props.inventory.categories.map((category) => ({
      ...category,
      items: category.items.filter((item) => item.count > 0),
    }))
    : props.shop.categories,
)

const selectedCategory = computed(() =>
  categories.value.find((item) => item.key === category.value) ?? categories.value[0],
)

watch(
  categories,
  (items) => {
    if (!items.some((item) => item.key === category.value)) {
      category.value = items[0]?.key ?? ''
    }
  },
  { immediate: true },
)

function applyPreferredInventoryCategory(): void {
  const preferred = props.initialInventoryCategory
  if (
    props.view === 'inventory'
    && preferred
    && props.inventory.categories.some((item) => item.key === preferred)
  ) {
    category.value = preferred
  }
}

watch(
  [() => props.initialInventoryCategory, () => props.inventory.categories],
  applyPreferredInventoryCategory,
  { immediate: true },
)

function onCategoryChange(event: Event): void {
  const target = event.target
  if (!(target instanceof HTMLSelectElement)) return
  category.value = target.value
  if (props.view === 'inventory') emit('inventoryCategory', category.value)
}

function itemName(item: GameItem): string {
  return item.known === false ? item.name : t(item.name)
}

function payload(item: GameItem): InventoryCommand {
  return {
    category: item.category,
    item_id: item.key,
    count: Math.max(1, Math.min(1_000, Math.floor(count.value || 1))),
  }
}

function buy(item: GameItem, event: MouseEvent): void {
  const quantity = payload(item).count
  if (event.shiftKey) {
    emit('addToCart', item, quantity)
    return
  }
  emit('buy', payload(item))
}

function isFreeze(item: GameItem): boolean {
  return item.key === 'Заморозка' || item.name.includes('Заморозка')
}

function isUsable(item: GameItem): boolean {
  return item.usable
}

function canPurchase(item: GameItem): boolean {
  if (cartCapacity(item, props.inventory) === 0) return false
  if (item.can_buy) return true
  if (!props.canOpenCredit || item.credit_allowed === false || !item.available_for_level) return false
  return true
}
</script>

<template>
  <section class="game-panel" :aria-labelledby="'inventory-title'">
    <header class="panel-heading">
      <div>
        <p>{{ t('Предметы и эффекты') }}</p>
        <h2 id="inventory-title">{{ view === 'inventory' ? t('Инвентарь') : t('Магазин') }}</h2>
      </div>
    </header>

    <div class="item-toolbar">
      <label>
        <span>{{ t('Категория') }}</span>
        <select :value="category" @change="onCategoryChange">
          <option v-for="item in categories" :key="item.key" :value="item.key">
            {{ t(item.name) }}
          </option>
        </select>
      </label>
      <label>
        <span>{{ t('Количество') }}</span>
        <input v-model.number="count" type="number" min="1" max="1000" step="1" />
      </label>
    </div>

    <p v-if="!selectedCategory || selectedCategory.items.length === 0" class="empty-copy">
      {{ view === 'inventory' ? t('В этой категории пока нет предметов.') : t('Каталог пуст.') }}
    </p>

    <div v-else class="item-grid">
      <article v-for="item in selectedCategory.items" :key="item.id" class="item-card">
        <header>
          <div>
            <h3>{{ itemName(item) }}</h3>
            <span v-if="view === 'inventory'" class="count-badge">×{{ item.count }}</span>
          </div>
          <span v-if="view === 'shop' && item.price !== undefined" class="price">
            {{ locale.formatNumber(item.price) }} {{ t('монет') }}
          </span>
        </header>
        <p v-if="item.description">{{ t(item.description) }}</p>
        <p v-if="item.effect" class="effect">{{ t(item.effect) }}</p>
        <p v-if="item.level && item.level > 1" class="meta">
          {{ t('Требуемый уровень') }}: {{ item.level }}
        </p>
        <div class="button-row">
          <button
            v-if="item.buy && (view === 'shop' || view === 'inventory')"
            class="nf-button"
            type="button"
            :disabled="busy || !canPurchase(item)"
            @click="buy(item, $event)"
          >
            {{ t('Купить') }}
          </button>
          <button
            v-if="view === 'inventory' && isUsable(item) && !isFreeze(item)"
            class="nf-button"
            type="button"
            :disabled="busy || count > item.count"
            @click="emit('use', payload(item))"
          >
            {{ t('Использовать') }}
          </button>
          <button
            v-if="view === 'inventory' && isFreeze(item)"
            class="nf-button"
            type="button"
            :disabled="busy || item.count < 1"
            @click="emit('freeze')"
          >
            {{ t('Выбрать серию') }}
          </button>
          <button
            v-if="view === 'inventory' && item.sellable"
            class="nf-button nf-button--secondary"
            type="button"
            :disabled="busy || count > item.count"
            @click="emit('sell', payload(item))"
          >
            {{ t('Продать') }}
            <span v-if="item.sell_price !== undefined">· {{ locale.formatNumber(item.sell_price) }}</span>
          </button>
        </div>
      </article>
    </div>
  </section>
</template>

<style scoped>
.game-panel {
  padding: var(--nf-space-5);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-lg);
  background: var(--nf-color-surface);
  box-shadow: var(--nf-shadow-card);
}

.panel-heading,
.item-card header,
.item-card header > div,
.button-row,
.item-toolbar {
  display: flex;
  gap: var(--nf-space-3);
  align-items: center;
}

.panel-heading,
.item-card header {
  justify-content: space-between;
}

.panel-heading p,
.panel-heading h2,
.item-card h3,
.item-card p,
.empty-copy {
  margin: 0;
}

.panel-heading p {
  color: var(--nf-color-accent);
  font-size: 0.75rem;
  font-weight: 800;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

.panel-heading h2,
.item-card h3 {
  font-family: var(--nf-font-serif);
}

.view-switch {
  display: inline-flex;
  padding: var(--nf-space-1);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-pill);
  background: var(--nf-color-surface-muted);
}

.view-switch button {
  min-height: 2.4rem;
  padding: 0.45rem 0.8rem;
  border: 0;
  border-radius: var(--nf-radius-pill);
  background: transparent;
  color: var(--nf-color-text-muted);
  cursor: pointer;
}

.view-switch button.active {
  background: var(--nf-color-surface-raised);
  color: var(--nf-color-primary);
  box-shadow: 0 2px 8px rgb(0 0 0 / 8%);
  font-weight: 750;
}

.item-toolbar {
  margin: var(--nf-space-5) 0;
}

.item-toolbar label {
  display: grid;
  gap: var(--nf-space-1);
  color: var(--nf-color-text-muted);
  font-size: 0.8rem;
  font-weight: 700;
}

.item-toolbar select,
.item-toolbar input {
  min-height: 2.7rem;
  padding: 0.55rem 0.7rem;
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
  color: var(--nf-color-text);
}

.item-toolbar select {
  min-width: 13rem;
}

.item-toolbar input {
  width: 7rem;
}

.item-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--nf-space-3);
}

.item-card {
  display: grid;
  gap: var(--nf-space-3);
  min-width: 0;
  padding: var(--nf-space-4);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-surface-raised);
}

.item-card header > div {
  min-width: 0;
}

.item-card h3 {
  overflow-wrap: anywhere;
  font-size: 1.05rem;
}

.item-card p,
.empty-copy {
  color: var(--nf-color-text-muted);
  line-height: 1.45;
}

.item-card .effect {
  color: var(--nf-color-primary);
}

.item-card .meta {
  font-size: 0.8rem;
}

.count-badge,
.price {
  flex: 0 0 auto;
  font-size: 0.85rem;
  font-weight: 750;
}

.count-badge {
  color: var(--nf-color-primary);
}

.price {
  color: var(--nf-color-warning);
}

.button-row {
  align-self: end;
  flex-wrap: wrap;
}

@media (max-width: 72rem) {
  .item-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 44rem) {
  .panel-heading,
  .item-toolbar {
    align-items: stretch;
    flex-direction: column;
  }

  .view-switch,
  .item-toolbar select,
  .item-toolbar input {
    width: 100%;
  }

  .view-switch button {
    flex: 1;
  }

  .item-grid {
    grid-template-columns: 1fr;
  }
}
</style>
