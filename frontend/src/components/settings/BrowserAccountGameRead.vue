<script setup lang="ts">
import { computed } from 'vue'
import InventoryShopPanel from '@/components/game/InventoryShopPanel.vue'
import StreakBadge from '@/components/projects/StreakBadge.vue'
import type { AccountGameBase } from '@/cloud/gameCodec'
import { progressMicros } from '@/cloud/progressCodec'
import type { GameInventory } from '@/types/game'
import { useLocaleStore } from '@/stores/locale'
const props=defineProps<{value:AccountGameBase}>()
const locale=useLocaleStore(),t=locale.translate
const number=(amount:string)=>locale.formatNumber(Number(progressMicros(amount))/1e6,2)
const inventory=computed<GameInventory>(()=>{
 const categories=new Map<string,GameInventory['categories'][number]>()
 for(const item of props.value.inventory){
  let category=categories.get(item.category)
  if(!category){category={key:item.category,name:item.category,items:[]};categories.set(item.category,category)}
  category.items.push({id:JSON.stringify([item.category,item.item_id]),key:item.item_id,category:item.category,name:item.item_id,description:null,count:item.count,
    buy:false,sellable:false,usable:false,known:false})
 }
 return {categories:[...categories.values()]}
})
const skills=[['productivity','Продуктивность'],['profitability','Доходность'],['endurance','Выносливость']] as const
const coefficients=computed(()=>[['Монеты',props.value.coin_coefficient],['Опыт',props.value.experience_coefficient],['Восстановление',props.value.health_recovery_coefficient]] as const)
const status=computed(()=>!props.value.global_streak.enabled?'Off':props.value.global_streak.history.length?'Active':props.value.global_streak.lost_day&&props.value.global_streak.lost_length?`Lose ${props.value.global_streak.lost_length}`:'No')
</script>
<template>
  <section data-testid="cloud-account-game">
    <h4>{{ t('Параметры персонажа') }}</h4>
    <dl>
      <dt>{{ t('Монеты') }}</dt><dd data-testid="cloud-game-coins">{{ number(value.coins) }}</dd>
      <dt>{{ t('Опыт') }}</dt><dd data-testid="cloud-game-experience">{{ number(value.experience) }}</dd>
      <dt>{{ t('Уровень') }}</dt><dd data-testid="cloud-game-level">{{ value.level }}</dd>
      <dt>{{ t('Здоровье') }}</dt><dd>{{ number(value.health) }}/{{ number(value.max_health) }}</dd>
      <dt>{{ t('Вдохновение') }}</dt><dd>{{ number(value.inspiration) }}</dd>
      <dt>{{ t('Доступно очков навыков') }}</dt><dd>{{ value.available_skill_points }}</dd>
    </dl>
    <h5>{{ t('Навыки') }}</h5>
    <dl><template v-for="[key,label] in skills" :key="key"><dt>{{ t(label) }}</dt><dd>{{ value.skills[key] }}</dd></template></dl>
    <details><summary>{{ t('Подробные коэффициенты') }}</summary><dl><template v-for="[label,amount] in coefficients" :key="label"><dt>{{ t(label) }}</dt><dd>{{ number(amount) }}</dd></template></dl></details>
    <StreakBadge :length="value.global_streak.history.length" :max-length="value.global_streak.maximum" :status="status" scope="global" show-max />
    <div data-testid="cloud-game-inventory"><InventoryShopPanel :inventory="inventory" :shop="{categories:[],custom_awards:{items:[]}}" :busy="false" view="inventory" :can-open-credit="false" read-only /></div>
  </section>
</template>

<style scoped>
section { display: grid; gap: var(--nf-space-3); }
h4, h5, dl { margin: 0; }
dl { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: var(--nf-space-2) var(--nf-space-4); font-variant-numeric: tabular-nums; }
dd { margin: 0; font-weight: 650; }
dt { color: var(--nf-color-text-muted); }
summary { cursor: pointer; }
</style>
