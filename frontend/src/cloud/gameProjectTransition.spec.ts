import { expect, it } from 'vitest'
import golden from './__fixtures__/gameCodecV1.json'
import type { ProjectGameEvent, AccountGameEvent, AccountGameBase } from './gameCodec'
import { transitionGame, verifyProjectReward, verifyGlobalStreakGrowth, gameOwner, gameActionId, type ProjectGameState } from './gameProjectTransition'
const event=(name:string)=>structuredClone(golden.examples.find(e=>e.name===name)!.event)
it('retains Native project streak/capture and validates writing reward without granting account resources',()=>{
 const genesis=event('project-genesis') as ProjectGameEvent,before=structuredClone(genesis)
 const state=transitionGame(genesis) as ProjectGameState
 expect(state.streak.history).toHaveLength(1);expect(state.completion_claimed).toBe(true)
 const writing=event('progress-reward-source') as ProjectGameEvent
 expect(transitionGame(writing,state)).toEqual(state);expect(genesis).toEqual(before)
 if(writing.action.kind!=='writing')throw Error('fixture')
 writing.action.reward.coins='11.000000'
 expect(()=>verifyProjectReward(writing)).toThrow('game_invalid_rule')
})
it('matches Native completion and linked account level/coins/skills transition; no duplicate completion',()=>{
 const project=event('project-genesis') as ProjectGameEvent
 if(project.action.kind!=='genesis')throw Error('fixture')
 project.action.completion_claimed=false
 const state=transitionGame(project),completion=event('project-completion') as ProjectGameEvent
 if(completion.action.kind!=='completion')throw Error('fixture')
 completion.action.reward={coins:'200.000000',experience:'20000.000000'}
 const completed=transitionGame(completion,state) as ProjectGameState
 expect(completed.completion_claimed).toBe(true)
 expect(()=>transitionGame(completion,completed)).toThrow('game_invalid_rule')
 const account=event('account-genesis') as AccountGameEvent
 const reward=event('linked-reward') as AccountGameEvent
 if(reward.action.kind!=='reward')throw Error('fixture')
 reward.action.project_action_id=completion.header.event_id;reward.action.reward_id=`reward:${completion.header.event_id}`;reward.action.reward=completion.action.reward
 const next=transitionGame(reward,transitionGame(account),completion) as AccountGameBase
 expect(next).toMatchObject({coins:'700.000000',experience:'12500.000000',level:2,available_skill_points:2,health:'100.000000'})
 expect(next.completion_claims).toContain('project:p')
 expect(()=>transitionGame(reward,transitionGame(account))).toThrow('game_invalid_rule')
})
it('keeps Stage owner identity and stable action IDs scoped by actual source',async()=>{
 const p=event('project-genesis') as ProjectGameEvent,s=event('stage-genesis') as ProjectGameEvent
 expect(gameOwner(p)).not.toBe(gameOwner(s))
 const a=await gameActionId(p.header.account_id,'writing',['p',null,'event','entry'])
 expect(a).toMatch(/^[a-f0-9-]{14}5/)
 expect(await gameActionId(p.header.account_id,'writing',['p',null,'event','entry'])).toBe(a)
 expect(await gameActionId(p.header.account_id,'writing',['p','stage','event','entry'])).not.toBe(a)
})
it('uses Native inventory before/after counts, signed price and canonical post-state; never replays a debit',()=>{
 const account=event('account-genesis') as AccountGameEvent
 const before=transitionGame(account) as AccountGameBase
 const buy=event('inventory-buy') as AccountGameEvent
 if(buy.action.kind!=='inventory')throw Error('fixture')
 const action=buy.action
 action.before_count=before.inventory.find(i=>i.category===action.category&&i.item_id===action.item_id)?.count??0
 buy.action.after_count=buy.action.before_count+buy.action.count
 const next=transitionGame(buy,before) as AccountGameBase
 expect(next.coins).toBe('225.000000')
 expect(next.inventory.find(i=>i.item_id===action.item_id)?.count).toBe(buy.action.after_count)
 expect(()=>transitionGame(buy,next)).toThrow('game_invalid_rule')
 const sell=structuredClone(buy)
 if(sell.action.kind!=='inventory')throw Error('fixture')
 sell.action.operation='sell';sell.action.before_count=buy.action.after_count;sell.action.after_count=buy.action.before_count;sell.action.coins_delta='25.000000'
 expect(transitionGame(sell,next)).toEqual(before)
 const wrong=structuredClone(buy)
 if(wrong.action.kind!=='inventory')throw Error('fixture')
 wrong.action.coins_delta='-24.000000'
 expect(()=>transitionGame(wrong,before)).toThrow('game_invalid_rule')
 expect(()=>transitionGame(buy,{...before,coins:'0.000000'})).toThrow('game_invalid_rule')
 expect(transitionGame(account)).toEqual(before)
})
it('applies Native standalone streak loss once, including health and resurrection inventory',()=>{
 const genesis=event('account-genesis') as AccountGameEvent
 const base=transitionGame(genesis) as AccountGameBase
 const loss=event('global-streak') as AccountGameEvent
 if(loss.action.kind!=='global_streak')throw Error('fixture')
 const before=structuredClone(base.global_streak)
 loss.version=2;loss.action.before=before;loss.action.writing_day='2026-10-03';loss.action.project_action_ids=[]
 loss.action.after={...before,history:[],freezes:0,lost_day:'2026-10-03',lost_length:1,maximum:Math.max(before.maximum,1)}
 loss.action.reward={coins:'0.000000',experience:'0.000000'}
 const after=transitionGame(loss,base) as AccountGameBase
 expect(after.health).toBe('95.000000');expect(after.global_streak).toEqual(loss.action.after)
 expect(()=>transitionGame(loss,after)).toThrow('game_invalid_rule')
 const low={...structuredClone(base),health:'3.000000',inventory:[{category:'Зелья',item_id:'Зелье воскрешения',count:1}]}
 const revived=transitionGame(loss,low) as AccountGameBase
 expect(revived.health).toBe(low.max_health);expect(revived.inventory).toEqual([])
})

it('recomputes streak growth from the recorded progress and the authenticated goal', () => {
 const account=transitionGame(event('account-genesis') as AccountGameEvent) as AccountGameBase
 const source=event('progress-reward-source') as ProjectGameEvent
 if(source.action.kind!=='writing')throw Error('fixture')
 const streak={...account.global_streak,history:[],maximum:0,freezes:0,enabled:true,last_reward_day:null,lost_day:null,lost_length:0}
 account.global_streak=streak
 const day=source.action.fact.writing_day
 const project={goal:1000,unit:'symbols' as const,infinite:false,status:'active',personal_goal:100,deadline:null,streak_enabled:true,stages_enabled:false}
 const progress={base_total:'0.000000',entries:[source.action.fact]}
 const action={kind:'global_streak' as const,writing_day:day,project_action_ids:[source.header.event_id],before:streak,
  after:{...streak,history:[{day,frozen:false}],maximum:1,last_reward_day:day},reward:{coins:'50.000000',experience:'0.000000'}}
 const state={streak,completion_claimed:false}
 expect(()=>verifyGlobalStreakGrowth(action,account,source,state,progress,project)).not.toThrow()
 expect(()=>verifyGlobalStreakGrowth(action,account,source,state,progress,{...project,personal_goal:1000})).toThrow('game_invalid_rule')
 expect(()=>verifyGlobalStreakGrowth({...action,reward:{...action.reward,coins:'500.000000'}},account,source,state,progress,project)).toThrow('game_invalid_rule')
})
