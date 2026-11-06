import { encryptedSyncV3Api } from '@/api/encryptedSyncV3'
import { BrowserPersistenceError } from './persistence'
import { currentBrowserDevice } from './deviceRegistration'
import type {NormalUserAuthRuntime} from '@/auth/userAuth'
import type {AuthoritativeAccountBinding} from '@/auth/accountBinding'
import type {RuntimeKeyContext} from '@/auth/keyContext'
import type {VerifiedBrowserDevice} from './deviceRegistration'
import type {BrowserSyncScope,BrowserSyncTransaction} from './syncDurability'
import {BrowserResolutionPrefix} from './resolutionPrefix'

// Audited gaps under the frozen ordinary1/Resolution2 claim, NOT runtime grants.
// Removing a blocker requires its own genuine producer/consumer acceptance.
// Eight remain from the original ten criteria (resurrection and own Resolution
// ancestry accepted). This is not a counter of completed Recovery GET/UI features.
// See docs/web/WEB_6_0_PROGRESS_AUDIT.md for the exact remaining scope per item.
// Own Resolution publication ancestry is covered by the production Native writer
// → sealed outbox/receipt → Chromium planner/apply/self-echo acceptance.
const blockers=Object.freeze([
 'production_v10_reader_dispatch_incomplete',
 'transport_v10_page_replay_incomplete',
 'orphan_dependency_recovery_not_accepted',
 'further_resolution_chains_not_accepted',
 'local_author_after_resolution_not_accepted',
 'own_content_self_echo_not_qualified',
 'multi_device_reader_orchestration_not_proven',
 'server_note_gate_not_checked',
] as const)
export interface BrowserNoteReadinessDiagnostic {
 readonly readOnly:true;readonly ready:false;readonly declarationAllowed:false;readonly writerAuthorized:false
 readonly localEvidence:'authenticated_bounded_inventory'
 readonly serverGate:'not_checked'|'checked_blocked'|'checked_account_ready'
 readonly serverMissingDevices?:number
 readonly blockers:readonly string[]
 readonly localPrefix:{current:number;candidate:number;firstBlockingSequence:number|null}
}
/** Dormant negative preflight only. Optional explicit gate GET only; no declaration HTTP, write method,
 * persistent flag or positive capability token. Replay storage exposes this
 * diagnostic explicitly; it is never an automatic reader declaration. */
export class BrowserNoteReaderReadiness {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly profileCurrent:()=>boolean){}
 bind(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext,device:VerifiedBrowserDevice){
  // Keep the actual G runtime and its tokens in this lexical closure, never in a
  // TypeScript-private property or a diagnostic DTO available to JS callers.
  const prefix=new BrowserResolutionPrefix(this.transaction,this.profileCurrent).bind(auth,bindings,keys,device)
  return Object.freeze({inspect:async(scope:BrowserSyncScope,options:{checkServerGate?:boolean}={}):Promise<BrowserNoteReadinessDiagnostic>=>{
   // Accepted G owns normal auth/device/AMK/profile checks, complete bounded
   // inventory authentication/currentness and fresh native snapshot comparison.
   // Unsupported/corrupt/stale local evidence rejects, never becomes readiness.
   const context=auth.requireContext(),lease=keys.leaseForAccount(scope.accountId)
   const check=()=>{if(!this.profileCurrent()||!auth.isCurrent(context)||!lease?.isCurrent()
     ||!currentBrowserDevice(device,scope,auth))throw new BrowserPersistenceError('closed')}
   check()
   const certificate=await prefix.prepare(scope)
   let result=await prefix.calculate(certificate)
   let gate:Awaited<ReturnType<typeof encryptedSyncV3Api.noteReaderGate>>|undefined
   if(options.checkServerGate){
    // GET is account-wide observation only: it never establishes this device's
    // eligibility, and cannot replace the remaining frozen reader requirements.
    gate=(await auth.authorized(token=>encryptedSyncV3Api.noteReaderGate(token))).value
    check()
    // HTTP may outlive a lock, source mutation or proof corruption. Authenticate
    // the complete current inventory again before returning any observation.
    result=await prefix.calculate(await prefix.prepare(scope));check()
   }
   return Object.freeze({readOnly:true,ready:false,declarationAllowed:false,writerAuthorized:false,
    localEvidence:'authenticated_bounded_inventory',serverGate:gate?(gate.ready?'checked_account_ready':'checked_blocked'):'not_checked',
    ...(gate?{serverMissingDevices:gate.missing_devices}:{}),
    // A gate GET is observation, not accepted full-capability recipient pull.
    // Preserve the frozen acceptance blockers even when the account GET succeeds.
    blockers:Object.freeze([...blockers,...(result.first_blocking_sequence===null?[]:['local_prefix_blocked'])]),
    localPrefix:Object.freeze({current:result.current_proven_prefix,candidate:result.candidate_proven_prefix,firstBlockingSequence:result.first_blocking_sequence})})
  }})
 }
}
