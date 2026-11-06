import { expect, it } from 'vitest'
import golden from './__fixtures__/gameCodecV1.json'
import type { ProjectGameEvent, AccountGameEvent, AccountGameBase } from './gameCodec'
import { transitionGame, verifyProjectReward, gameOwner, gameActionId, type ProjectGameState } from './gameProjectTransition'
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
