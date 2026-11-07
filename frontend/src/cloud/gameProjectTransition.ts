import { canonical } from './projectMetadataCodec'
import { progressMicros } from './progressCodec'
import { validateGameEvent, type GameEvent, type ProjectGameEvent, type AccountGameBase, type StreakBase, type GameReward } from './gameCodec'
export interface ProjectGameState { streak: StreakBase; completion_claimed: boolean }
export type GameState = ProjectGameState | AccountGameBase
function fail():never { throw new Error('game_invalid_rule') }
export const gameOwner = (e: GameEvent): string => e.header.scope === 'project' ? canonical([e.header.project_id,e.header.stage_id]) : 'account'
export const gameProject = (e: GameEvent): string => e.header.scope === 'project' ? e.header.project_id : 'account'
export async function gameActionId(account: string, kind: string, source: unknown) {
  const bytes = new TextEncoder().encode(canonical(['WORTA/C18/game/action/1',account,kind,source]))
  try {
    const hash = new Uint8Array(await crypto.subtle.digest('SHA-256',bytes)).slice(0,16)
    hash[6] = (hash[6]! & 15) | 80; hash[8] = (hash[8]! & 63) | 128
    const h=Array.from(hash,x=>x.toString(16).padStart(2,'0')).join('')
    return `${h.slice(0,8)}-${h.slice(8,12)}-${h.slice(12,16)}-${h.slice(16,20)}-${h.slice(20)}`
  } finally { bytes.fill(0) }
}
const amount = (value:string) => Number(progressMicros(value))/1e6
const decimal = (value:number):string => {
  if(!Number.isFinite(value)||value<0||value>1e12)fail()
  return value.toFixed(6)
}
const tenth = (value:number) => Math.max(0,Math.ceil((value-1e-9)*10)/10)
/** Frozen Native game_sync::writing_reward. No account balance is granted by a
 * project event: this merely verifies the immutable declared reward. */
export function verifyProjectReward(e:ProjectGameEvent):void {
  const a=e.action
  if(a.kind==='writing'){
    const multiplier=(1+amount(a.inspiration)/100*0.1)*(1+amount(a.writing_bonus))
    const expected={coins:decimal(tenth(amount(a.fact.delta)/100*10*amount(a.coin_coefficient)*multiplier)),
      experience:decimal(tenth(amount(a.fact.delta)/100*500*amount(a.experience_coefficient)*multiplier))}
    if(!['native-game-v1','python-game-v1'].includes(e.header.rule)||canonical(a.reward)!==canonical(expected))fail()
  }else if(a.kind==='completion'){
    if(e.header.rule!=='native-game-v1')fail() // Python ties-even completion is not yet qualified.
    const coins=Math.round(amount(a.total_symbols)/1000+0.5)*100*(e.header.stage_id===null?1:0.25)
    if(canonical(a.reward)!==canonical({coins:decimal(coins),experience:decimal(coins*100)}))fail()
  }
}
/** Exact admitted Native causal transitions. Deferred Native actions still fail
 * closed; Web reading never runs the Game event consumer or reissues rewards. */
export function transitionGame(e:GameEvent,parent?:GameState,source?:ProjectGameEvent):GameState {
  validateGameEvent(e)
  const a=e.action
  if(a.kind==='genesis'){
    if(parent)fail()
    return e.header.scope==='project'
      ? {streak:structuredClone((a as {base:StreakBase}).base),completion_claimed:(a as {completion_claimed:boolean}).completion_claimed}
      : structuredClone((a as {base:AccountGameBase}).base)
  }
  if(!parent)fail()
  const state=structuredClone(parent)
  if(e.header.scope==='project'){
    verifyProjectReward(e as ProjectGameEvent)
    if(a.kind==='writing')return state
    if(a.kind==='completion'){
      if((state as ProjectGameState).completion_claimed!==false)fail()
      ;(state as ProjectGameState).completion_claimed=true;return state
    }
    return fail()
  }
  if(a.kind==='global_streak'){
    if(e.header.rule!=='native-game-v1')fail()
    const account=state as AccountGameBase
    if(canonical(account.global_streak)!==canonical(a.before))fail()
    account.global_streak=structuredClone(a.after)
    if(e.version===1){
      if(!source||source.header.scope!=='project'||source.action.kind!=='writing')fail()
      const reward=a.reward
      const change=(key:'coins'|'experience',delta:bigint)=>{const n=progressMicros(account[key])+delta;if(n<0n||n>1000000000000000000n)fail();account[key]=`${n/1000000n}.${(n%1000000n).toString().padStart(6,'0')}`}
      change('coins',progressMicros(reward.coins))
      change('experience',progressMicros(reward.experience))
      return account
    }
    if(e.version!==2)fail()
    const damage=BigInt(a.after.lost_length)*5000000n
    const health=progressMicros(account.health), max=progressMicros(account.max_health)
    const remaining=(health<max?health:max)-damage
    const safe=remaining>0n?remaining:0n
    account.health=`${safe/1000000n}.${(safe%1000000n).toString().padStart(6,'0')}`
    if(safe===0n){
      const index=account.inventory.findIndex(item=>item.category==='Зелья'&&item.item_id==='Зелье воскрешения'&&item.count>0)
      if(index>=0){if(account.inventory[index]!.count===1)account.inventory.splice(index,1);else account.inventory[index]!.count--;account.health=account.max_health}
    }
    return account
  }
  if(a.kind==='freeze'){
    if(e.header.rule!=='native-game-v1'||a.project_id!==null||a.before===null||a.after===null)fail()
    const account=state as AccountGameBase
    if(canonical(account.global_streak)!==canonical(a.before))fail()
    const index=account.inventory.findIndex(item=>item.category==='Предметы'&&item.item_id==='Заморозка')
    if(index<0||account.inventory[index]!.count!==a.before_count)fail()
    if(a.after_count===0)account.inventory.splice(index,1);else account.inventory[index]!.count=a.after_count
    account.global_streak=structuredClone(a.after)
    return account
  }
  if(a.kind==='inventory'){
    if(e.header.rule!=='native-game-v1'||!['buy','sell'].includes(a.operation))fail()
    const account=state as AccountGameBase
    const delta=progressMicros(a.unit_price)*BigInt(a.count)*(a.operation==='buy'?-1n:1n)
    if(delta!==progressMicros(a.coins_delta))fail()
    const position=account.inventory.findIndex(item=>item.category===a.category&&item.item_id===a.item_id)
    if((position<0?0:account.inventory[position]!.count)!==a.before_count)fail()
    if(position>=0)account.inventory.splice(position,1)
    if(a.after_count!==0)account.inventory.push({category:a.category,item_id:a.item_id,count:a.after_count})
    account.inventory.sort((left,right)=>{const a=`${left.category}\0${left.item_id}`,b=`${right.category}\0${right.item_id}`;return a<b?-1:a>b?1:0})
    const coins=progressMicros(account.coins)+delta
    if(coins<0n||coins>1000000000000000000n)fail()
    account.coins=`${coins/1000000n}.${(coins%1000000n).toString().padStart(6,'0')}`
    // Native revalidates every changed account snapshot as a typed genesis.
    validateGameEvent({...e,version:1,header:{...e.header,parents:[],revision:1,rule:'legacy-game-v1'},action:{kind:'genesis',base:account}})
    return account
  }
  if(a.kind!=='reward'||!source||source.header.scope!=='project'||source.header.account_id!==e.header.account_id
    ||source.header.event_id!==a.project_action_id||source.header.project_id!==a.project_id||source.header.rule!==e.header.rule
    ||!('reward' in source.action)||canonical(source.action.reward)!==canonical(a.reward))fail()
  verifyProjectReward(source)
  const account=state as AccountGameBase, reward=(a as {reward:GameReward}).reward
  const change=(key:'coins'|'experience',delta:bigint)=>{const n=progressMicros(account[key])+delta;if(n<0n||n>1000000000000000000n)fail();account[key]=`${n/1000000n}.${(n%1000000n).toString().padStart(6,'0')}`}
  if(source.action.kind==='writing'){
    for(const key of ['inspiration','writing_bonus','coin_coefficient','experience_coefficient'] as const)if(source.action[key]!==account[key])fail()
    account.coins=decimal(tenth(amount(account.coins)+amount(reward.coins)))
    account.writing_bonus='0.000000'
    if(e.header.rule==='native-game-v1'){
      const n=account.productive_actions+1
      if(n>=3&&account.creative_event_pending==='absent'){account.productive_actions=0;account.creative_event_pending='unexpected_idea'}else account.productive_actions=n
    }
  }else if(source.action.kind==='completion'){
    change('coins',progressMicros(reward.coins))
    const claim=source.header.stage_id===null?`project:${source.header.project_id}`:`stage:${source.header.project_id}:${source.header.stage_id}`
    if(account.completion_claims.includes(claim))fail()
    account.completion_claims.push(claim);account.completion_claims.sort()
  }else fail()
  if(e.header.rule==='python-game-v1'){change('experience',progressMicros(reward.experience));return account}
  if(e.header.rule!=='native-game-v1')fail()
  change('experience',progressMicros(decimal(tenth(amount(reward.experience)))))
  while(account.level<99&&progressMicros(account.experience)>=BigInt(8000*account.level*account.level)*1000000n){
    const level=account.level
    change('experience',-BigInt(8000*level*level)*1000000n)
    change('coins',BigInt(level*250*(100+(level-1)*15))*10000n)
    account.level++;account.available_skill_points+=2
    account.max_health=decimal(100+Math.floor(level/5)*10);account.health=account.max_health
  }
  return account
}
