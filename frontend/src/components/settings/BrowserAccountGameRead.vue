<script setup lang="ts">
import { computed } from 'vue'
import InventoryShopPanel from '@/components/game/InventoryShopPanel.vue'
import StreakBadge from '@/components/projects/StreakBadge.vue'
import type { AccountGameBase } from '@/cloud/gameCodec'
import { progressMicros } from '@/cloud/progressCodec'
import type { InventoryCommand } from '@/types/game'
import { presentGameShop, quoteGameShopAction, type GameShopCommand } from '@/cloud/gameShop'
import { useLocaleStore } from '@/stores/locale'
export type GameWorkspaceView = 'overview' | 'inventory' | 'shop'

const props=defineProps<{value:AccountGameBase; writable?:boolean; busy?:boolean; view?:GameWorkspaceView}>()
const emit=defineEmits<{ trade:[command:GameShopCommand] }>()
const locale=useLocaleStore(),t=locale.translate
const number=(amount:string)=>locale.formatNumber(Number(progressMicros(amount))/1e6,2)
const catalog=computed(()=>presentGameShop(props.value))
const trade=(operation:GameShopCommand['operation'], payload:InventoryCommand)=>emit('trade',{...payload,operation})
const canTrade=(operation:GameShopCommand['operation'], payload:InventoryCommand)=>
  quoteGameShopAction(props.value,{...payload,operation})!==null
const skills=[['productivity','Продуктивность'],['profitability','Доходность'],['endurance','Выносливость']] as const
const coefficients=computed(()=>[['Монеты',props.value.coin_coefficient],['Опыт',props.value.experience_coefficient],['Восстановление',props.value.health_recovery_coefficient]] as const)
const status=computed(()=>!props.value.global_streak.enabled?'Off':props.value.global_streak.history.length?'Active':props.value.global_streak.lost_day&&props.value.global_streak.lost_length?`Lose ${props.value.global_streak.lost_length}`:'No')
</script>
<template>
  <section data-testid="cloud-account-game" :class="{ 'game-workspace': view }">
    <h4 v-if="!view || view === 'overview'">{{ t('Параметры персонажа') }}</h4>
    <dl :class="{ 'game-workspace__stats': view }">
      <div><dt>{{ t('Монеты') }}</dt><dd data-testid="cloud-game-coins">{{ number(value.coins) }}</dd></div>
      <template v-if="!view || view === 'overview'">
        <div><dt>{{ t('Опыт') }}</dt><dd data-testid="cloud-game-experience">{{ number(value.experience) }}</dd></div>
        <div><dt>{{ t('Уровень') }}</dt><dd data-testid="cloud-game-level">{{ value.level }}</dd></div>
        <div><dt>{{ t('Здоровье') }}</dt><dd>{{ number(value.health) }}/{{ number(value.max_health) }}</dd></div>
        <div><dt>{{ t('Вдохновение') }}</dt><dd>{{ number(value.inspiration) }}</dd></div>
      </template>
    </dl>
    <div v-if="!view || view === 'overview'" class="game-workspace__details">
      <div>
        <h5>{{ t('Навыки') }}</h5>
        <dl><div><dt>{{ t('Доступно очков навыков') }}</dt><dd>{{ value.available_skill_points }}</dd></div>
          <div v-for="[key,label] in skills" :key="key"><dt>{{ t(label) }}</dt><dd>{{ value.skills[key] }}</dd></div>
        </dl>
        <details><summary>{{ t('Подробные коэффициенты') }}</summary><dl><div v-for="[label,amount] in coefficients" :key="label"><dt>{{ t(label) }}</dt><dd>{{ number(amount) }}</dd></div></dl></details>
      </div>
      <div>
        <StreakBadge data-testid="cloud-account-streak" :length="value.global_streak.history.length" :max-length="value.global_streak.maximum" :status="status" scope="global" show-max />
        <dl><div><dt>{{ t('Использовано заморозок') }}</dt><dd data-testid="cloud-account-freezes">{{ value.global_streak.freezes }}</dd></div></dl>
      </div>
    </div>
    <div v-if="!view || view === 'inventory'" data-testid="cloud-game-inventory">
      <InventoryShopPanel :inventory="catalog.inventory" :shop="catalog.shop" :busy="!!busy" view="inventory" :can-open-credit="false"
        :read-only="!writable" :can-trade="canTrade" trade-only @buy="trade('buy', $event)" @sell="trade('sell', $event)" />
    </div>
    <InventoryShopPanel v-if="writable && (!view || view === 'shop')" :inventory="catalog.inventory" :shop="catalog.shop" :busy="!!busy" view="shop"
      :can-open-credit="false" :can-trade="canTrade" trade-only @buy="trade('buy', $event)" />
  </section>
</template>

<style scoped>
section { display: grid; gap: var(--nf-space-3); }
h4, h5, dl { margin: 0; }
dl { display: grid; gap: var(--nf-space-2); font-variant-numeric: tabular-nums; }
dl > div { display: flex; justify-content: space-between; gap: var(--nf-space-3); }
.game-workspace__stats { grid-template-columns: repeat(auto-fit, minmax(min(100%, 8rem), 1fr)); }
.game-workspace__stats > div { display: grid; gap: var(--nf-space-1); padding: var(--nf-space-3); border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-md); background: var(--nf-color-surface); }
.game-workspace__stats dd { font-size: 1.25rem; }
.game-workspace__details { display: grid; gap: var(--nf-space-4); }
.game-workspace .game-workspace__details { grid-template-columns: repeat(auto-fit, minmax(min(100%, 17rem), 1fr)); }
.game-workspace__details > div { display: grid; align-content: start; justify-items: stretch; gap: var(--nf-space-2); }
dd { margin: 0; font-weight: 650; }
dt { color: var(--nf-color-text-muted); }
summary { cursor: pointer; }
</style>
