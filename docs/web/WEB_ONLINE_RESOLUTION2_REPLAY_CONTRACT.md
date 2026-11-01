# WEB.ONLINE.02B — Resolution2 replay compatibility contract

**DESIGN ONLY — no implementation, migration, capability or release grant.**
Baseline `1e589629a1bf37d7a053f552d06bef38949a3d0a`. This integrates the
preceding uncommitted STOP evidence in checkpoint/roadmap; its server/browser
acceptance remains NOT RUN. Server remains authoritative, clients do E2EE;
`ready=false`, `declarationAllowed=false`, `writerAuthorized=false`, Registration
CLOSED and PUBLIC RELEASE BLOCKED. Progress remains **89.36%**.

## 1. Exact first cohort and existing authority

One account and active connected project, one live Note with a genuine accepted
Legacy base, two concurrent ordinary Content Note updates from two distinct
peer devices to that same causal base, then one peer-authored codec8 Content
`choose_version` Resolution2 selecting one of those two verified tips. The
browser is a third distinct device; every event is already in the ordinary v3
account feed. All required Metadata/Stage/StageOrder events precede the Content
that depends on them and are qualified by their existing owners. Stage-less
Notes need no Stage proof. Full account history must be admitted, not filtered
to the selected project. Require an active bootstrap, exact original wire,
legitimate device association and supported compression/codec throughout.

The production choice path `cloudSession.chooseNoteVersion` → `NoteSyncRuntime`
→ `ContentNoteRuntime.choose` → `choose_content_note_version` → native
`content_note_writer::prepare_choice` verifies the current conflict and selected
version and constructs the canonical Resolution2 from the complete tips. This
shows the path exists; its full browser replay and fresh-device proof are **not
yet accepted**. The isolated implementation test must generate both branches
and the choice through genuine production writer/owners, rather than manually
splicing headers or declaring a browser capability it has not earned.

Exclude local browser author/receipts/outbox, self echo, tombstone/resurrection,
retain-both/clone, additional branches or resolutions, missing parents, other
families, encrypted covers not yet supported, later Metadata dependency,
conflicting owner tips and changes to the pinned account snapshot. Reject the
entire unsupported cohort before exposing a project. Existing earlier accepted
replay cohorts stay unchanged. Per-event proven prefixes may persist privately;
they are never an incomplete visible project. No content, Note or account
events may be silently dropped from the global cursor.

## 2. Source, sequential owners and durable proof

1. Authenticate account, legal acceptance, device identity and AMK lease. Use
   existing protected v3 pull and all device/family/compression gates; pin the
   active bootstrap registry, mode/epoch and high-water mark. Capture all pages
   densely with exact wire/nonce/ciphertext and account sequence. Enforce current
   bounds (100/page, 200/account, 32 MiB plus existing owner limits); a limit,
   missing/gap, duplicate mismatch, unsupported family or unstable source is a
   blocker, never a completeness claim. AEAD authenticates data, not a global
   server signature: server rollback/equivocation on a fresh profile is not
   detectable solely from hashes.
2. Reconcile the retained full prefix and registry/mode with fresh server
   responses. Reuse v5 Metadata, Stage/StageOrder, source-seed and Legacy
   owners in server order; exact already-committed records must be reauthenticated.
   Do not infer a Metadata/Stage proof from a decoded value, seed or cursor.
3. Invoke the existing Content owner for each original peer branch, in account
   sequence. Its actual applied/conflict-preserved outcome, encrypted source,
   candidate/head and conflict-group evidence must be materialized by the owner,
   not fabricated by the orchestrator. A conflicting branch is not an ordinary
   applied-content proof; define and validate its existing real conflict proof,
   exact candidate membership and group lifecycle in the new inventory. Abort the
   cohort if that owner cannot construct the peer-only conflict from these real
   parents without an invented local candidate or publication receipt. This is
   a concrete feasibility gate for the next implementation, not an assertion
   that an untested adapter already supports it.
4. Invoke the existing Resolution planner/apply owner with the real ordered
   inbox event and authenticated parent/group/source evidence. Its private
   single-use plan verifies bootstrap, all exact parents and their server
   publication, selected tip, conflict generation/closure, source revision,
   AEAD and codec. Use its actual `Resolution4`, `GroupTransition2`, `Head2`,
   `inbox10`/`ResolutionProof1`, encrypted output and source records. Keep raw
   Legacy-v2, Legacy-on-v3 and codec8 routes distinct; this cohort admits only
   codec8 Content-v3. No fabricated DTO, client ID, receipt or source revision
   may serve as authority. The server's immutable sequence must put every
   dependency before its dependent event; the rejected forward-Metadata audit
   remains unaffected.
5. Each event has one strict physical IndexedDB transaction: after crypto and
   planning outside the transaction, re-read the complete account inventory,
   identity, generation and exact server-pinned inputs; compare-and-swap them,
   check owner/lifecycle/key leases, then synchronously commit the owner's
   encrypted source/project/Note mutations, candidate/head/group transition,
   original inbox wire plus authentic outcome proof, and coverage journal in
   the same transaction. The adapter verifies the entire allowed before/after
   delta and leaves other projects/accounts unchanged. No network, decryption or
   awaited crypto inside the live transaction; success only on transaction
   completion. Existing `scheduleProtectedResolutionApply` and owner routines
   remain authority; an orchestrator never synthesizes records or rewards.
6. After the complete cohort, re-open and authenticate all real owner evidence,
   recompute the full inventory/dependency closure, recheck auth/lease/device,
   registry/mode and an empty server tail. Publish only a diagnostic with
   `projectOpened=false` and negative readiness flags. A separately authorized
   full-project publication contract must prove coverage for every encountered
   family; these three Note events alone never authorize a project view.

## 3. Proposed replay v6 and older clients

`worta-web-replay` v5 refuses conflict/Resolution evidence: its allowlist and
exact inventory joins admit only bounded `note_proven` and terminal
`content_note_proven`; inbox10 and Resolution4/Head2/GroupTransition2 fail.
Reusing v5 or weakening its validator in place is forbidden. Reserve native
IndexedDB database **version 6** and manifest `record_version=6`,
`contract_version=6`. This is a proposed, explicitly scoped **replay DB**
migration, not a migration of the separate developer `worta-web` v12, Desktop
SQLite or PostgreSQL. No server/wire/crypto version changes are implied.

Keep all thirteen v5 physical stores, key paths, indices and existing key
meanings. Use existing `inbox` for the actual conflict and inbox10 owner records,
`legacyNoteHistory` for the real candidate/group/Resolution4/Head2/
GroupTransition2 records, and existing encrypted `localProjects`/`localNotes`
and `ownerHeads`/bindings for owner state. Outbox, receipts and intents stay
empty in this peer-only cohort. **Do not assume** a group shape or key fits:
before implementation, prove each actual owner's records and keys fit these
stores and cannot collide with retained v5 records. If they do not, STOP and
revise this contract with a migration plan; do not add hidden stores or invent
alternate proof formats. The v6 manifest preserves all v5 fields; only the two
manifest versions change during migration. Existing `receive_cursor`,
`metadata_cursor`, `applied_cursor` and seed references retain their exact
meaning. Coverage may count a conflict outcome only after its actual persisted
owner proof is joined; it is never ACK or readiness.

On native versionchange v5→v6, acquire existing exclusive writer/lifecycle
guard, validate the entire v5 database for every account using the **frozen v5
validator**, identity and limits before any manifest write, and change only
manifest record/contract version to 6 in the same transaction. Preserve original
wire, ciphertext, keys, seeds, owners, proofs, cursors, accounts and ordering
byte-for-byte. Empty databases initialize v6; v1–v4 upgrades must validate each
actually supported legacy state using its frozen validator and perform the
existing proven conversion to v5 semantics followed by v6 within the same
versionchange, or fail closed without writes. Unknown/partial schemas, dirty
outbox, malformed records, failed validation, quota, logout, abort and blocked
upgrade preserve original bytes and original native version. Do not silently
delete/rebuild unknown owner-bearing data. A real pinned v5 client attempting
to open native version 5 must receive VersionError/refuse v6 without writing or
deleting; v6 likewise rejects unknown future versions. Test multiple accounts,
physical restart, aborted upgrade, old binary refusal and exact v5 comparisons.

## 4. Recovery, capability and security boundaries

At interruption before an atomic commit, retain received ciphertext and prior
proof prefix without a partial outcome. After commit with lost response,
reopen exact durable records, reauthenticate the existing owner's proofs and
original wire, and return already-applied only for the exact committed join;
never reproduce randomized ciphertext or advance a cursor on guessed success.
On restart reacquire the account lock, auth/key/device leases, reconcile the
whole source from sequence zero, authenticate parent/group/proof/source chains
and do a fresh gated server-tail check before any result. Network failure,
cache eviction, quota, revocation, malformed wire, unknown format, replay drift,
wrong key, missing dependency or conflicting tip yield a visible fail-closed
blocker. Unknown data is not dropped to make the project open. A server with
new history does not silently clear owner-bearing cache; owned-generation
advancement requires its own accepted contract.

Current Note capability declares the complete ordinary1 **and** Resolution2
reader contract, including all ten readiness blockers, device/compression gates,
not merely the first cohort above. Today `BrowserNoteReaderReadiness` is a
negative diagnostic and production capability PUT is not authorized. An
unqualified recipient receives server 409; it must show the real blocker. A
truthful future declaration requires independently accepted production reader
dispatch, full supported Resolution/ordinary coverage and negative scenarios,
multi-device orchestration, server Note gate verification, restart and old
client compatibility, followed by an explicitly authorized capability lifecycle
with revocation semantics. If the frozen server contract remains broader than
implemented browser capability, the only honest result is blocked; a different
server contract requires separate owner decision. No fabricated declaration,
relabelled codec, prequalified identity, new endpoint or gate bypass is a
production path. Test-only capability setup, if separately allowed in an
isolated server test, proves gate mechanics only and never positive readiness;
it does not replace an actual full-capability recipient acceptance.

## 5. Sequential implementation slices and acceptance

| Slice | Minimum change | Required acceptance and boundary |
| --- | --- | --- |
| 1. v6 migration only | Frozen v5 validator and atomic versionchange; no new owner records or dispatch | Real Chromium v1–v5 profiles, multiple accounts, byte preservation, abort/quota/restart, pinned old-client refusal, malformed/unknown refusal; no ACK, PUT, project result or data migration outside replay DB |
| 2. Conflict owner bridge | One verified peer/peer branch cohort using actual Content owner and actual conflict outcome, exact pre/post account inventory | Genuine native production writer branches through encrypted server; source/CAS, original wire, restart, missing parent/metadata, forks and mixed-family refusal; if existing owner requires fabricated local-author evidence, STOP and revise contract |
| 3. Resolution owner bridge | One peer `choose_version` only, real Resolution planner/apply and v6 transaction adapter | Native writer builds choice after two genuine peer edits; PostgreSQL16 + real protected HTTP + Chromium, actual recipient eligibility, full atomic output/inbox/proofs, interruption before/after commit, duplicate/restart, lost response, altered wire/key/group/parent, revocation/quota and no visible project on failure; verify all existing reader/gate negative cases |
| 4. Reader qualification | Independently close each applicable readiness blocker using actual production owners and all required histories, then separately review lifecycle | Eligible real browser declares only after demonstrated full frozen ordinary1/Resolution2 contract and real server gate; unready recipients remain rejected; declaration is never writer permission |
| 5. Full read publication | Separately cover all account/project families, stable source and UI lifecycle | Real two-client end-to-end/restart and negative validation before complete project opening; no writer or public release implied |

Each slice needs focused local tests first, relevant negative/backward-compatibility
tests, typecheck/build and browser checks; server behavior requires disposable
PostgreSQL 16 and real FastAPI HTTP; independent CI acceptance follows commit
without polling. A later schema slice must not be started as part of this design.
Production storage and protocol remain unchanged here.
