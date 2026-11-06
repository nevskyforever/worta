<script setup lang="ts">
import { computed } from 'vue'
import StreakBadge from '@/components/projects/StreakBadge.vue'
import { useLocaleStore } from '@/stores/locale'
import type { SupportedLanguage } from '@/types/api'
import type { ProjectGameState } from '@/cloud/gameProjectTransition'
import type { ProjectGameEvent } from '@/cloud/gameCodec'
import { progressMicros } from '@/cloud/progressCodec'
const props=defineProps<{value:{event:ProjectGameEvent;state:ProjectGameState;history:ProjectGameEvent[]}}>()
const locale=useLocaleStore(),t=locale.translate
const completionLabels: Record<SupportedLanguage,string>={ru:'Награда за завершение получена',en:'Completion reward claimed',es:'Recompensa por finalizar recibida',de:'Abschlussbelohnung erhalten',fr:'Récompense de fin reçue',pt_BR:'Recompensa de conclusão recebida'}
const streak=computed(()=>props.value.state.streak)
// Native game_projection::streak_fields; no clock-driven Game mutation on read.
const status=computed(()=>!streak.value.enabled?'Off':!streak.value.history.length&&streak.value.lost_day&&streak.value.lost_length>0?`Lose ${streak.value.lost_length}`:streak.value.history.length?'Active':'No')
const rewards=computed(()=>props.value.history.filter(event=>event.action.kind==='writing'||event.action.kind==='completion'))
const number=(amount:string)=>locale.formatNumber(Number(progressMicros(amount))/1e6,2)
</script>
<template>
  <section data-testid="cloud-project-game">
    <h4>{{ t('Игровой режим') }}</h4>
    <StreakBadge :length="streak.history.length" :max-length="streak.maximum" :status="status" :scope="value.event.header.stage_id===null?'project':'stage'" show-max />
    <dl><dt>{{ t('Использовано заморозок') }}</dt><dd data-testid="cloud-game-freezes">{{ streak.freezes }}</dd>
      <dt>{{ completionLabels[locale.language] }}</dt><dd data-testid="cloud-game-completion">{{ value.state.completion_claimed ? '☑' : '☐' }}</dd></dl>
    <h5>{{ t('Награды') }}</h5>
    <table data-testid="cloud-game-rewards"><thead><tr><th>{{ t('Дата') }}</th><th>{{ t('Монеты') }}</th><th>{{ t('Опыт') }}</th></tr></thead>
      <tbody><tr v-for="event in [...rewards].reverse()" :key="event.header.event_id" :data-game-event="event.header.event_id"><td>{{ locale.formatDate(event.header.updated_at) }}</td><template v-if="'reward' in event.action"><td>{{ number(event.action.reward.coins) }}</td><td>{{ number(event.action.reward.experience) }}</td></template></tr></tbody>
    </table>
  </section>
</template>
