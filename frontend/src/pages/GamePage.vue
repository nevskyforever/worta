<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { IonContent, IonPage, onIonViewWillEnter } from '@ionic/vue'
import { alertCircleOutline, sparklesOutline } from 'ionicons/icons'

import { apiErrorMessage } from '@/api/client'
import { gameApi } from '@/api/game'
import { settingsApi } from '@/api/settings'
import AwardsBankPanel from '@/components/game/AwardsBankPanel.vue'
import CabinetPanel from '@/components/game/CabinetPanel.vue'
import CharacterQuickStats from '@/components/game/CharacterQuickStats.vue'
import ChallengesPanel from '@/components/game/ChallengesPanel.vue'
import CreditConfirmDialog from '@/components/game/CreditConfirmDialog.vue'
import GameOverview from '@/components/game/GameOverview.vue'
import GrowthPanel from '@/components/game/GrowthPanel.vue'
import InventoryShopPanel from '@/components/game/InventoryShopPanel.vue'
import LotteryTicketDialog, { type LotteryDraw } from '@/components/game/LotteryTicketDialog.vue'
import PurchaseConfirmDialog from '@/components/game/PurchaseConfirmDialog.vue'
import ShoppingCart, { type CartLine } from '@/components/game/ShoppingCart.vue'
import WritingSessionPanel from '@/components/game/WritingSessionPanel.vue'
import StatePanel from '@/components/ui/StatePanel.vue'
import { useLocaleStore } from '@/stores/locale'
import { useNotificationsStore } from '@/stores/notifications'
import { announceDataChange, onDataChange } from '@/services/dataChanges'
import { cartCapacity, clampCartCount, reconcileCart } from '@/utils/gameCart'
import { gameResponseMessages } from '@/utils/gameNotifications'
import type {
  BankProductRequest,
  GameCommandResponse,
  GameItem,
  GameState,
  InventoryCommand,
  WritingSessionStart,
} from '@/types/game'

type GameTab =
  | 'overview'
  | 'sessions'
  | 'challenges'
  | 'inventory'
  | 'shop'
  | 'growth'
  | 'cabinet'
  | 'awards'
  | 'bank'

const locale = useLocaleStore()
const notifications = useNotificationsStore()
const t = locale.translate
const state = ref<GameState | null>(null)
const loading = ref(true)
const busy = ref(false)
const error = ref('')
function readSavedGameTab(): string | null {
  try {
    return window.sessionStorage.getItem('nfprogress:game-tab')
  } catch {
    return null
  }
}

function saveGameTab(value: GameTab): void {
  try {
    window.sessionStorage.setItem('nfprogress:game-tab', value)
  } catch {
    // View state persistence is optional in restricted browser contexts.
  }
}

const savedGameTab = readSavedGameTab()
const tab = ref<GameTab>([
  'overview', 'sessions', 'challenges', 'inventory', 'shop', 'growth', 'cabinet', 'awards', 'bank',
].includes(savedGameTab ?? '') ? savedGameTab as GameTab : 'overview')
const bankPreview = ref<GameCommandResponse['result']>(null)
const inventoryCategory = ref('')
const lotteryDraws = ref<LotteryDraw[]>([])
const pendingPurchase = ref<InventoryCommand | null>(null)
const cartLines = ref<CartLine[]>([])
const pendingCredit = ref<{
  amount: number
  days: number
  preview: GameCommandResponse['result']
  source: 'cart' | 'purchase'
} | null>(null)
const cartTotal = computed(() => cartLines.value.reduce(
  (sum, line) => sum + (line.item.price ?? 0) * line.count,
  0,
))
const displayedCartLines = computed<CartLine[]>(() => cartLines.value.map((line) => ({
  ...line,
  maximumCount: state.value ? cartCapacity(line.item, state.value.inventory) : null,
})))
const cartCreditAllowed = computed(() => cartLines.value.every(
  (line) => line.item.credit_allowed !== false,
))
const pendingPurchaseItem = computed(() => {
  const payload = pendingPurchase.value
  if (!payload || !state.value) return null
  const findItem = (categories: GameState['shop']['categories']) =>
    categories.flatMap((category) => category.items)
      .find((item) => item.category === payload.category && item.key === payload.item_id)
  return findItem(state.value.shop.categories) ?? findItem(state.value.inventory.categories) ?? null
})
let stopDataChanges: (() => void) | undefined
let stateController: AbortController | undefined
let preferencesController: AbortController | undefined
let preferenceRequest = 0
let inventoryPreferenceSaveChain: Promise<void> = Promise.resolve()
let sessionCompletionTimer: ReturnType<typeof setTimeout> | undefined
let effectsRefreshTimer: ReturnType<typeof setTimeout> | undefined

function applyDeveloperState(event: Event): void {
  const stateEvent = event as CustomEvent<GameState>
  if (stateEvent.detail) applyState(stateEvent.detail)
}

const tabs: ReadonlyArray<{ key: GameTab; label: string }> = [
  { key: 'overview', label: 'Обзор' },
  { key: 'sessions', label: 'Сессии' },
  { key: 'challenges', label: 'Испытания' },
  { key: 'inventory', label: 'Инвентарь' },
  { key: 'shop', label: 'Магазин' },
  { key: 'growth', label: 'Развитие' },
  { key: 'cabinet', label: 'Кабинет' },
  { key: 'awards', label: 'Награды' },
  { key: 'bank', label: 'Банк' },
]

function applyState(nextState: GameState): void {
  state.value = nextState
  cartLines.value = reconcileCart(cartLines.value, nextState.inventory)
  notifications.setGameHistory(nextState.notifications)
  scheduleSessionCompletion(nextState)
  scheduleEffectsRefresh(nextState)
}

function notifyGameResponses(
  responses: Array<Pick<GameCommandResponse, 'message' | 'messages'>>,
  fallbackMessage?: string,
): void {
  const messages = responses.flatMap((response) => gameResponseMessages(response))
  if (!messages.length && fallbackMessage) messages.push(fallbackMessage)
  for (const message of messages) notifications.success(t(message))
}

function notifyGameResponse(
  response: Pick<GameCommandResponse, 'message' | 'messages'>,
  fallbackMessage = 'Изменения сохранены.',
): void {
  notifyGameResponses([response], fallbackMessage)
}

function serverClockDelay(serverTime: string, endsAt: string): number | null {
  const server = Date.parse(serverTime)
  const end = Date.parse(endsAt)
  if (Number.isNaN(server) || Number.isNaN(end)) return null
  return Math.max(0, end - server)
}

function finishExpiredSession(): void {
  if (busy.value) {
    sessionCompletionTimer = setTimeout(finishExpiredSession, 250)
    return
  }
  void runCommand(() => gameApi.finishWritingSession())
}

function scheduleSessionCompletion(nextState: GameState): void {
  clearTimeout(sessionCompletionTimer)
  sessionCompletionTimer = undefined
  const session = nextState.writing_session.active
  if (!session) return
  const delay = session.ends_at
    ? serverClockDelay(nextState.writing_session.server_time, session.ends_at)
    : session.remaining_seconds * 1_000
  if (delay === null) return
  sessionCompletionTimer = setTimeout(finishExpiredSession, delay + 50)
}

function scheduleEffectsRefresh(nextState: GameState): void {
  clearTimeout(effectsRefreshTimer)
  effectsRefreshTimer = undefined
  const delays = [...nextState.buffs.positive, ...nextState.buffs.negative]
    .map((effect) => effect.expires_at
      ? serverClockDelay(nextState.buffs.server_time, effect.expires_at)
      : null)
    .filter((delay): delay is number => delay !== null)
  if (!delays.length) return
  effectsRefreshTimer = setTimeout(
    () => { void loadState() },
    Math.min(Math.min(...delays) + 50, 2_147_000_000),
  )
}

async function loadState(): Promise<void> {
  stateController?.abort()
  stateController = new AbortController()
  if (!state.value) loading.value = true
  error.value = ''
  try {
    applyState(await gameApi.state(stateController.signal))
  } catch (caught) {
    if (caught instanceof DOMException && caught.name === 'AbortError') return
    error.value = apiErrorMessage(caught)
  } finally {
    loading.value = false
  }
}

async function loadInventoryPreference(): Promise<void> {
  preferencesController?.abort()
  preferencesController = new AbortController()
  const request = ++preferenceRequest
  try {
    const settings = await settingsApi.get(preferencesController.signal)
    const value = settings.values.inventory_filter
    if (request === preferenceRequest && typeof value === 'string') {
      inventoryCategory.value = value
    }
  } catch (caught) {
    if (caught instanceof DOMException && caught.name === 'AbortError') return
    // The game screen still works if a noncritical view preference is unavailable.
  }
}

function persistInventoryCategory(category: string): void {
  if (!category || category === inventoryCategory.value) return
  inventoryCategory.value = category
  const requestedCategory = category
  inventoryPreferenceSaveChain = inventoryPreferenceSaveChain
    .then(() => settingsApi.update({ inventory_filter: requestedCategory }))
    .then((settings) => {
      if (
        inventoryCategory.value === requestedCategory
        && typeof settings.values.inventory_filter === 'string'
      ) {
        inventoryCategory.value = settings.values.inventory_filter
      }
    })
    .catch((caught: unknown) => {
      error.value = apiErrorMessage(caught)
      notifications.error(error.value)
    })
}

async function runCommand(
  action: () => Promise<GameCommandResponse>,
  options: { capturePreview?: boolean; fallbackMessage?: string } = {},
): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    const response = await action()
    applyState(response.state)
    announceDataChange('game')
    bankPreview.value = options.capturePreview ? response.result : null
    notifyGameResponse(response, options.fallbackMessage ?? 'Изменения сохранены.')
  } catch (caught) {
    error.value = apiErrorMessage(caught)
    notifications.error(error.value)
  } finally {
    busy.value = false
  }
}

function applyFreeze(target: 'global' | 'project', projectId?: string): void {
  void runCommand(() => gameApi.applyStreakFreeze(target, projectId))
}

function openFreezeSelector(): void {
  tab.value = 'overview'
}

function startSession(payload: WritingSessionStart): void {
  void runCommand(() => gameApi.startWritingSession(payload))
}

function inventoryCommand(
  action: 'buy' | 'sell' | 'use',
  payload: InventoryCommand,
): void {
  const command = {
    buy: gameApi.buyItem,
    sell: gameApi.sellItem,
    use: gameApi.useItem,
  }[action]
  if (action === 'buy') {
    pendingPurchase.value = payload
    return
  }
  if (action === 'use' && payload.item_id === 'Лотерейный билет') {
    void useLotteryTicket(payload)
    return
  }
  void runCommand(() => command(payload))
}

function addToCart(item: GameItem, count: number): void {
  if (!state.value) return
  const existing = cartLines.value.find((line) => line.item.id === item.id)
  const nextCount = clampCartCount(
    item,
    state.value.inventory,
    (existing?.count ?? 0) + count,
  )
  if (existing) {
    if (nextCount > 0) existing.count = nextCount
    else removeCartLine(item.id)
  } else if (nextCount > 0) {
    cartLines.value.push({ item, count: nextCount })
  }
}

function changeCartLine(itemId: string, count: number): void {
  const line = cartLines.value.find((candidate) => candidate.item.id === itemId)
  if (!line || !state.value) return
  const nextCount = clampCartCount(line.item, state.value.inventory, count)
  if (nextCount > 0) line.count = nextCount
  else removeCartLine(itemId)
}

function removeCartLine(itemId: string): void {
  cartLines.value = cartLines.value.filter((line) => line.item.id !== itemId)
}

async function confirmPurchase(): Promise<void> {
  const payload = pendingPurchase.value
  const item = pendingPurchaseItem.value
  if (!payload || !item || !state.value || busy.value) return
  const total = (item.price ?? 0) * payload.count
  const shortfall = Math.max(0, total - state.value.profile.coins)
  if (shortfall > 0 && state.value.bank.can_open_credit && item.credit_allowed !== false) {
    busy.value = true
    error.value = ''
    try {
      const preview = await gameApi.previewBankProduct({
        product_type: 'credit', amount: shortfall, days: 30,
        allow_interest_withdrawal: false,
      })
      pendingCredit.value = { amount: shortfall, days: 30, preview: preview.result, source: 'purchase' }
    } catch (caught) {
      error.value = apiErrorMessage(caught)
      notifications.error(error.value)
    } finally {
      busy.value = false
    }
    return
  }
  pendingPurchase.value = null
  await runCommand(() => gameApi.buyItem(payload))
}

function addPendingPurchaseToCart(): void {
  const payload = pendingPurchase.value
  const item = pendingPurchaseItem.value
  if (payload && item) addToCart(item, payload.count)
  pendingPurchase.value = null
}

async function checkoutCart(useCredit: boolean, days: number, approvedCredit = false): Promise<void> {
  if (busy.value || !cartLines.value.length || !state.value) return
  cartLines.value = reconcileCart(cartLines.value, state.value.inventory)
  if (!cartLines.value.length) return
  busy.value = true
  error.value = ''
  try {
    let currentState = {} as GameState
    const responses: GameCommandResponse[] = []
    const shortfall = Math.max(0, cartTotal.value - state.value.profile.coins)
    if (useCredit && shortfall > 0 && !approvedCredit) {
      const preview = await gameApi.previewBankProduct({
        product_type: 'credit', amount: shortfall, days: Math.floor(days),
        allow_interest_withdrawal: false,
      })
      pendingCredit.value = {
        amount: shortfall,
        days: Math.floor(days),
        preview: preview.result,
        source: 'cart',
      }
      return
    }
    if (useCredit && shortfall > 0) {
      const credit = await gameApi.openBankCredit(shortfall, Math.floor(days))
      responses.push(credit)
      currentState = credit.state
    }
    for (const line of cartLines.value) {
      const response = await gameApi.buyItem({
        category: line.item.category,
        item_id: line.item.key,
        count: line.count,
      })
      responses.push(response)
      currentState = response.state
    }
    applyState(currentState)
    cartLines.value = []
    announceDataChange('game')
    notifyGameResponses(responses, 'Покупки оформлены.')
  } catch (caught) {
    error.value = apiErrorMessage(caught)
    notifications.error(error.value)
  } finally {
    busy.value = false
  }
}

function cancelCreditPreview(): void {
  pendingCredit.value = null
}

function confirmCreditCheckout(): void {
  const credit = pendingCredit.value
  if (!credit) return
  pendingCredit.value = null
  if (credit.source === 'cart') {
    void checkoutCart(true, credit.days, true)
    return
  }
  void checkoutPurchaseWithCredit(credit.days)
}

async function checkoutPurchaseWithCredit(days: number): Promise<void> {
  const payload = pendingPurchase.value
  const item = pendingPurchaseItem.value
  if (!payload || !item || !state.value || busy.value) return
  busy.value = true
  error.value = ''
  try {
    const total = (item.price ?? 0) * payload.count
    const shortfall = Math.max(0, total - state.value.profile.coins)
    const credit = await gameApi.openBankCredit(shortfall, days)
    const purchase = await gameApi.buyItem(payload)
    pendingPurchase.value = null
    applyState(purchase.state ?? credit.state)
    announceDataChange('game')
    notifyGameResponses([credit, purchase], 'Покупка оформлена.')
  } catch (caught) {
    error.value = apiErrorMessage(caught)
    notifications.error(error.value)
  } finally {
    busy.value = false
  }
}

async function useLotteryTicket(payload: InventoryCommand): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    const response = await gameApi.useItem(payload)
    applyState(response.state)
    announceDataChange('game')
    const draws: unknown[] = Array.isArray(response.result?.lottery_draws)
      ? response.result.lottery_draws
      : []
    lotteryDraws.value = draws.filter(isLotteryDraw)
    notifyGameResponses(
      [response],
      lotteryDraws.value.length ? undefined : 'Изменения сохранены.',
    )
  } catch (caught) {
    error.value = apiErrorMessage(caught)
    notifications.error(error.value)
  } finally {
    busy.value = false
  }
}

function isLotteryDraw(value: unknown): value is LotteryDraw {
  if (!value || typeof value !== 'object') return false
  const draw = value as Partial<LotteryDraw>
  return Array.isArray(draw.player_numbers) && Array.isArray(draw.winning_numbers)
    && typeof draw.matches === 'number' && typeof draw.prize === 'number'
}

function previewBank(payload: BankProductRequest): void {
  void runCommand(() => gameApi.previewBankProduct(payload), {
    capturePreview: true,
    fallbackMessage: 'Расчёт обновлён.',
  })
}

onMounted(() => {
  window.addEventListener('nfprogress:game-state-updated', applyDeveloperState)
  void loadState()
  void loadInventoryPreference()
  stopDataChanges = onDataChange((scope) => {
    if (scope === 'projects' || scope === 'game-sync') void loadState()
  })
})
onIonViewWillEnter(() => {
  if (state.value) void loadState()
})
watch(tab, (value) => saveGameTab(value))
onBeforeUnmount(() => {
  window.removeEventListener('nfprogress:game-state-updated', applyDeveloperState)
  clearTimeout(sessionCompletionTimer)
  clearTimeout(effectsRefreshTimer)
  stateController?.abort()
  preferencesController?.abort()
  stopDataChanges?.()
})
</script>

<template>
  <IonPage>
    <IonContent :fullscreen="true" class="game-content">
      <main class="game-workspace">
        <LotteryTicketDialog :draws="lotteryDraws" @close="lotteryDraws = []" />
        <PurchaseConfirmDialog
          :item="pendingPurchaseItem"
          :count="pendingPurchase?.count ?? 1"
          :busy="busy"
          @confirm="confirmPurchase"
          @cancel="pendingPurchase = null"
          @add-to-cart="addPendingPurchaseToCart"
        />
        <CreditConfirmDialog
          :preview="pendingCredit?.preview ?? null"
          :amount="pendingCredit?.amount ?? 0"
          :days="pendingCredit?.days ?? 0"
          :busy="busy"
          @confirm="confirmCreditCheckout"
          @cancel="cancelCreditPreview"
        />
        <ShoppingCart
          :lines="displayedCartLines"
          :coins="state?.profile.coins ?? 0"
          :can-open-credit="state?.bank.can_open_credit ?? false"
          :credit-allowed="cartCreditAllowed"
          :busy="busy"
          @change="changeCartLine"
          @remove="removeCartLine"
          @clear="cartLines = []"
          @checkout="checkoutCart"
        />
        <header class="page-header">
          <div>
            <p class="page-eyebrow">{{ t('Творческая мотивация') }}</p>
            <h1>{{ t('Игровой режим') }}</h1>
            <p class="page-introduction">
              {{ t('Развивайте писательский ритм, а все награды доверяйте правилам nfprogress.').replace(/nfprogress/gi, 'worta') }}
            </p>
          </div>
        </header>

        <StatePanel
          v-if="loading && !state"
          :title="t('Загружаем игровой режим')"
          :message="t('Синхронизируем прогресс, награды и активные эффекты.')"
          loading
        />

        <StatePanel
          v-else-if="error && !state"
          :title="t('Не удалось загрузить игровой режим')"
          :message="error"
          :icon="alertCircleOutline"
        >
          <button class="nf-button" type="button" @click="loadState()">{{ t('Повторить') }}</button>
        </StatePanel>

        <StatePanel
          v-else-if="state && !state.enabled"
          :title="t('Игровой режим отключён')"
          :message="t('Включите игровой режим в настройках, чтобы получать опыт, монеты и испытания.')"
          :icon="sparklesOutline"
        />

        <template v-else-if="state">
          <p v-if="error" class="feedback feedback--error" role="alert">{{ error }}</p>

          <nav class="game-tabs" role="tablist" :aria-label="t('Разделы игрового режима')">
            <button
              v-for="item in tabs"
              :key="item.key"
              :id="`game-tab-${item.key}`"
              type="button"
              role="tab"
              :aria-selected="tab === item.key"
              :aria-controls="`game-panel-${item.key}`"
              :class="{ active: tab === item.key }"
              @click="tab = item.key"
            >
              {{ t(item.label) }}
            </button>
          </nav>

          <CharacterQuickStats v-if="tab !== 'overview'" :profile="state.profile" />

          <section
            :id="`game-panel-${tab}`"
            class="active-game-panel"
            role="tabpanel"
            :aria-labelledby="`game-tab-${tab}`"
            tabindex="0"
          >
            <GameOverview
              v-if="tab === 'overview'"
              :profile="state.profile"
              :bank="state.bank"
              :buffs="state.buffs"
              :streak-freezes="state.streak_freezes"
              :busy="busy"
              @apply-freeze="applyFreeze"
              @effects-expired="loadState"
            />

            <WritingSessionPanel
              v-else-if="tab === 'sessions'"
              :session="state.writing_session"
              :busy="busy"
              @start="startSession"
              @finish="runCommand(() => gameApi.finishWritingSession())"
              @cancel="runCommand(() => gameApi.cancelWritingSession())"
            />

            <ChallengesPanel
              v-else-if="tab === 'challenges'"
              :daily="state.daily_challenge"
              :weekly="state.weekly_challenge"
              :inspiration="state.profile.inspiration"
              :busy="busy"
              @select-daily="(id) => runCommand(() => gameApi.selectDailyChallenge(id))"
              @start-weekly="(id) => runCommand(() => gameApi.startWeeklyChallenge(id))"
            />

            <InventoryShopPanel
              v-else-if="tab === 'inventory' || tab === 'shop'"
              :inventory="state.inventory"
              :shop="state.shop"
              :busy="busy"
              :view="tab"
              :can-open-credit="state.bank.can_open_credit ?? false"
              :initial-inventory-category="inventoryCategory"
              @buy="(payload) => inventoryCommand('buy', payload)"
              @add-to-cart="addToCart"
              @sell="(payload) => inventoryCommand('sell', payload)"
              @use="(payload) => inventoryCommand('use', payload)"
              @freeze="openFreezeSelector"
              @inventory-category="persistInventoryCategory"
            />

            <GrowthPanel
              v-else-if="tab === 'growth'"
              :inspiration="state.inspiration"
              :inspiration-points="state.profile.inspiration"
              :specializations="state.specializations"
              :skills="state.skills"
              :quests="state.quests"
              :level="state.profile.level"
              :busy="busy"
              @activate-inspiration="(id) => runCommand(() => gameApi.activateInspirationAbility(id))"
              @resolve-creative-event="(choice) => runCommand(() => gameApi.resolveCreativeEvent(choice))"
              @select-specialization="(id) => runCommand(() => gameApi.selectSpecialization(id))"
              @activate-specialization="runCommand(() => gameApi.activateSpecializationAbility())"
              @increase-skill="(id) => runCommand(() => gameApi.increaseSkill(id))"
              @start-quest="(id) => runCommand(() => gameApi.startQuest(id))"
              @abandon-quest="(id) => runCommand(() => gameApi.abandonQuest(id))"
            />

            <CabinetPanel
              v-else-if="tab === 'cabinet'"
              :manuscripts="state.manuscripts"
            />

            <AwardsBankPanel
              v-else
              :awards="state.custom_awards"
              :bank="state.bank"
              :preview="bankPreview"
              :busy="busy"
              :view="tab === 'awards' ? 'awards' : 'bank'"
              @create-award="(name, price, applyInflation) => runCommand(() => gameApi.createCustomAward(name, price, applyInflation))"
              @update-award="(id, name, price, applyInflation) => runCommand(() => gameApi.updateCustomAward(id, { name, price, apply_inflation: applyInflation }))"
              @delete-award="(id) => runCommand(() => gameApi.deleteCustomAward(id))"
              @buy-award="(id, count) => runCommand(() => gameApi.buyCustomAward(id, count))"
              @sell-award="(id, count) => runCommand(() => gameApi.sellCustomAward(id, count))"
              @use-award="(id, count) => runCommand(() => gameApi.useCustomAward(id, count))"
              @preview-bank="previewBank"
              @open-credit="(amount, days) => runCommand(() => gameApi.openBankCredit(amount, days))"
              @open-deposit="(amount, days, interest) => runCommand(() => gameApi.openBankDeposit(amount, days, interest))"
              @process-bank="runCommand(() => gameApi.processBankEvents())"
              @pay-credit="runCommand(() => gameApi.makeBankLoanPayment())"
              @partial-repay="(amount) => runCommand(() => gameApi.partiallyRepayBankCredit(amount))"
              @repay-credit="runCommand(() => gameApi.repayBankCredit())"
              @top-up-deposit="(amount) => runCommand(() => gameApi.topUpBankDeposit(amount))"
              @withdraw-deposit="(early) => runCommand(() => gameApi.withdrawBankDeposit(early))"
              @withdraw-interest="runCommand(() => gameApi.withdrawBankDepositInterest())"
            />
          </section>
        </template>
      </main>
    </IonContent>
  </IonPage>
</template>

<style scoped>
.game-content {
  --background: var(--nf-color-canvas);
}

.game-workspace {
  width: min(100%, 94rem);
  min-height: 100%;
  margin: 0 auto;
  padding: calc(var(--nf-space-7) + env(safe-area-inset-top)) clamp(1rem, 3vw, 3.5rem)
    calc(var(--nf-space-7) + env(safe-area-inset-bottom));
}

.page-header {
  display: flex;
  gap: var(--nf-space-5);
  align-items: flex-end;
  justify-content: space-between;
}

.page-eyebrow {
  margin: 0 0 var(--nf-space-2);
  color: var(--nf-color-accent);
  font-size: 0.75rem;
  font-weight: 800;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}

.page-header h1 {
  margin: 0;
  color: var(--nf-color-text);
  font-family: var(--nf-font-serif);
  font-size: clamp(1.8rem, 3.5vw, 2.75rem);
  font-weight: 650;
  letter-spacing: -0.04em;
}

.page-introduction {
  max-width: 47rem;
  margin: var(--nf-space-3) 0 0;
  color: var(--nf-color-text-muted);
  font-size: clamp(1rem, 2vw, 1.1rem);
  line-height: 1.6;
}

.command-feedback {
  min-height: 3.25rem;
  padding-top: var(--nf-space-3);
}

.feedback {
  width: fit-content;
  max-width: 100%;
  margin: 0;
  padding: 0.6rem 0.85rem;
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-muted);
  color: var(--nf-color-text-muted);
  line-height: 1.4;
}

.feedback--success {
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-success);
}

.feedback--error {
  background: color-mix(in srgb, var(--nf-color-danger) 12%, var(--nf-color-surface));
  color: var(--nf-color-danger);
}

.game-tabs {
  position: sticky;
  z-index: 5;
  top: 0;
  display: flex;
  width: fit-content;
  max-width: 100%;
  gap: var(--nf-space-1);
  margin: 0 auto var(--nf-space-5);
  padding: var(--nf-space-2);
  overflow-x: auto;
  scrollbar-width: thin;
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-md);
  background: color-mix(in srgb, var(--nf-color-surface) 94%, transparent);
  backdrop-filter: blur(12px);
  box-shadow: 0 0.75rem 1.75rem color-mix(in srgb, var(--nf-color-canvas) 34%, transparent);
}

.game-tabs button {
  flex: 0 0 auto;
  min-height: 2.75rem;
  padding: 0.65rem 0.85rem;
  border: 0;
  border-radius: var(--nf-radius-sm);
  background: transparent;
  color: var(--nf-color-text-muted);
  cursor: pointer;
  white-space: nowrap;
}

.game-tabs button.active {
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
  font-weight: 750;
}

.active-game-panel {
  outline: none;
}

.active-game-panel:focus-visible {
  outline: 3px solid var(--nf-color-focus);
}

@media (max-width: 44rem) {
  .game-workspace {
    padding-top: calc(var(--nf-space-5) + env(safe-area-inset-top));
  }

  .page-header {
    align-items: stretch;
    flex-direction: column;
  }

  .page-header .nf-button {
    width: 100%;
  }

  .game-tabs {
    width: auto;
    max-width: none;
    margin-inline: calc(-1 * clamp(1rem, 3vw, 3.5rem));
    border-inline: 0;
    border-radius: 0;
  }
}
</style>
