# WORTA 6.0 — Web Online-Only roadmap

Актуальный продуктовый порядок из шести пользовательских этапов утверждён в
[checkpoint](../WORTA_CHECKPOINT.md#worta-60--утверждённый-упрощённый-план-завершения-mvp).
Ниже сохранены технические срезы и их исторические статусы. Матрица функций и
старый critical path описывают состояние на момент их составления: уже
реализованные receipt, Metadata и Structural replay не являются новой задачей.
Полное открытие проекта, запись Web и сквозная приёмка пока не подтверждены.

**Owner decision, 2026-10-27: WEB ONLINE-ONLY / SERVER-AUTHORITATIVE.** Desktop
remains offline-first on SQLite with optional cloud sync. Web requires server
connectivity. E2EE remains client-side; the server is authoritative for
persistent user data and stores opaque ciphertext plus required protocol
metadata. LOCAL-FIRST is still mandatory for Codex development and acceptance;
it does not require offline-first runtime behavior in Web.


## 2026-11-01 — Resolution2 replay compatibility design

[Design contract](WEB_ONLINE_RESOLUTION2_REPLAY_CONTRACT.md): propose replay v6
with atomic validation/preservation of v5 and honest old-client refusal, then
separate bounded peer conflict, Resolution owner and full capability slices.
Key/store compatibility and genuine peer-only conflict provenance remain
implementation acceptance gates. No capability, reader readiness, schema or
production implementation is activated. Progress **89.36%**, `ready=false`,
`declarationAllowed=false`, `writerAuthorized=false`, Registration CLOSED and
PUBLIC RELEASE BLOCKED remain.

## 2026-11-01 — valid Resolution2 composition STOP

One legitimate production choice of a version after concurrent Content Note
edits needs the existing Resolution2 owner. Replay v5 admits neither that conflict
history nor the owner's inbox10/ResolutionProof1 and corresponding Resolution4,
Head2 and GroupTransition2 durable evidence. A dispatcher-only change cannot
establish safe restart. Reader implementation is STOP pending a separate scoped
replay-record/atomic-proof/upgrade and admission contract; no new capability or
schema was authorized here. Protected-server browser/restart acceptance for
this cohort is NOT RUN, since the current recipient gate cannot be truthfully
declared ready and test-only capability substitution is prohibited. See the
checkpoint for the precise evidence and limitations. All negative readiness
flags, closed registration/release and **89.36%** remain.

## 2026-10-31 — forward dependency audit outcome A

The [production writer audit](C21_FORWARD_METADATA_DEPENDENCY_AUDIT.md)
establishes that Stage cannot reference an unsent Metadata revision: the exact
dependency must already have server-derived applied authority before Stage is
queued. This gate predates the integrated writer and persists across audited
versions. The earlier late-Metadata fixture uses real codecs but manually
bypasses the production writer ordering; it is a negative semantic-history
test. Keep replay v5 fail-closed without forward-proof/schema expansion. The
conditional forward-dependency design suggestion below is superseded for this
cohort. Other reader blockers remain open; no next slice is started. Progress
**89.36%**, all negative readiness flags and closed release/registration remain.

## 2026-10-31 — one dependency reproduction; implementation STOP

The protected HTTP/PostgreSQL/Chromium reader now has a regression for Stage2
depending on real Metadata3 after Metadata genesis1. Existing owners preserve
all ciphertext and only the genuine prefix proof across restart and interrupted
server reconciliation, returning `missing_history`. No readiness is substituted.
Replay v5 explicitly requires earlier Metadata proof sequences; changing only
dispatch would produce a durable state rejected on reopen. Reader implementation
is STOP pending an explicit proof-ordering and schema compatibility decision
for this single cohort. No migration, production fix or other recovery scenario
is introduced. Keep the current rejection or separately authorize that contract;
do not repeat completed page-download recovery or bypass server gates.
Official progress **89.36%** and all negative capability/release flags remain.

## 2026-10-31 — WEB.ONLINE.02B baseline reconciliation

Baseline `96ada25b270a47bff8b56d5f244927a337d904d6` matches `origin/6.0`.
The original source/proof design is already complete; receipt, Metadata,
Stage/StageOrder, private source seed, bounded Legacy replay and the v5 terminal
Content-update/restart path are implemented. Do not repeat those slices.
See the [current contract status](WEB_ONLINE_02B_C21_SOURCE_PROOF_CONTRACT.md#baseline-reconciliation--96ada25b-2026-10-31).

Next bounded implementation: specify one remaining reader dependency/orphan
cohort, reuse existing ordinary receiver/owners and prove protected-server and
restart behavior before revisiting its readiness blocker. This review neither
authorizes that implementation nor removes any blocker. Resolution and native
family browser composition, owned-generation advancement and full-project
publication remain incomplete. Account Game now requires the actual account-v2
reader predicate; legal-acceptance and VPS database rights do not qualify Web
owners. No migration is needed for this documentation reconciliation.

`ready=false`, `declarationAllowed=false`, `writerAuthorized=false`,
Registration CLOSED, PUBLIC RELEASE BLOCKED and progress **89.36%** remain.

## 2026-10-30 — WEB.ONLINE.02B.03K.02A — multipage recovery

Real v3 pull of 107 events exercises interruption after the first 100, Chromium
restart, full wire reconciliation, ordered owner application and proof replay.
This bounded acceptance does not establish full C21 transport readiness.

## 2026-10-29 — WEB.ONLINE.02B.03K.01B — real server acceptance

The bounded ordered Content route now has an isolated real FastAPI/PostgreSQL 16
and Chromium acceptance through the ordinary protected v3 pull. A registered
recipient without Note readiness is rejected by the server. Test-only explicit
qualification permits one Metadata/Structural/Legacy/Content history, whose
owner proofs survive browser restart and fresh server verification. This does
not qualify production Web reader capabilities or open a complete project.
Next: a separate reader-readiness slice covering remaining C21 blockers.

## 2026-10-29 — WEB.ONLINE.02B.03K.01A2 — ordered owner composition

One bounded terminal peer Content update can now follow the existing ordered
Metadata/Structural/Legacy owner prefix inside replay v5. Full-history A1
admission runs before any commit; the real Content adapter owns final proof and
full server-wire recheck. Chromium restart with isolated v3 responses and
genuine owner fixtures cover retry and proof reauthentication. Real server gate
integration and browser reader readiness remain later slices.

## 2026-10-29 — WEB.ONLINE.02B.03K.01A1 — bounded full-history admission

The account-wide validator now admits exactly one terminal peer-origin Content
update after supported Legacy history, with canonical encrypted payload and
parent checks. This creates no owner proof and does not dispatch or open a
project. Next: sequential owner composition through the existing v5 adapter.

## 2026-10-29 — WEB.ONLINE.02B.03J — replay v5 Content adapter

Baseline `b7e1731a5b1479c2fbd9f19b30e890b9263ec537`. Explicit single-event
adapter reuses the real Content/Legacy owner for one peer-origin update of a
live Legacy Note. Inbox7/candidate2/transport proof, source/projection/head and
coverage commit atomically; restart reauthenticates the proofs and freshly
compares original server wire. Replay v1–v4 migration preserves existing records;
only the replay schema/manifest advances to v5. Developer v12/SQLite/PostgreSQL
are unchanged. See [contract and acceptance scope](WEB_ONLINE_02B_C21_SOURCE_PROOF_CONTRACT.md#2026-10-29--webonline02b03j--versioned-content-replay-adapter).

The new native Chromium test uses real producer/owner records and a real profile
restart with mocked ordinary v3 responses; it does not qualify server delivery
or capabilities. **LOCAL COMPLETE / INDEPENDENT CI PENDING (adapter scope only).** Final prepush
PASS: 831 Vitest / 30 files; additional native adapter run 15 focused tests / 14
browser checks; seven frontend Chromium scripts; 269 PostgreSQL16 regressions
and one backup/restore test; typecheck/build, changed-file ESLint, workflow/shell,
links and diff checks PASS. Earlier stale v4 fixture expectations were corrected
and the full prepush rerun passed. Six CI jobs and their limits are retained.
Option A approved / B rejected, all ten readiness blockers, negative flags,
projectOpened=false, closed registration/release and C21/C22/C23/PF6.0/RC remain.
Active product stage 2/6; historical progress 89,36%. Production ordered dispatch
is the next separate slice; no automatic Content/Resolution/writer activation.

## 2026-10-29 — WEB.ONLINE.02B.03I — STOP: replay Content compatibility

Baseline `eec525001e3894a86076b390ec9cdce005cc3dd4`; no production dispatcher
was implemented. Option A remains approved; Option B remains rejected. 03E is
accepted (SHA `737a8dfd4cca3d34f299a3549d96f45388619ff2`, run 40844792258,
6/6 SUCCESS). Replay v4 admits Legacy inbox3/candidate1 and authenticates only
Legacy Note proofs. The real Content owner commits inbox7/candidate2 plus a
transport proof; v4 transition, coverage and restart validators cannot accept
that complete commit. J current-page replay also cannot replace zero-origin
server reconciliation. See the [compatibility evidence and next slice](WEB_ONLINE_02B_C21_SOURCE_PROOF_CONTRACT.md#2026-10-29--webonline02b03i--stop-replay-content-record-composition).

Next implementation: versioned Content proof/source adapter with atomic CAS,
coverage, migration preservation and restart reauthentication, followed by its
ordered production dispatch. No migration was implemented. Both target blockers
and all readiness flags remain negative. Focused existing Vitest regressions:
**300 passed / 5 files** (Content owner, v10 transport/recovery, receiver and
Structural replay). These establish existing component behavior, not new
production acceptance. Chromium/PostgreSQL integration, backup/restore,
typecheck/build, ESLint and prepush NOT RUN: no runtime implementation to accept.
Documentation diff check PASS. Developer v12, SQLite/PostgreSQL and all
C21/C22/C23/PF6.0/RC/security/release gates remain unchanged; projectOpened=false,
product stage 2/6 and historical progress 89,36% remain.

## 2026-10-29 — WEB.ONLINE.02B.03H: unified sync owner decision

**Option A approved; Option B rejected.** Desktop and Web use the existing C21
server sync protocol; Desktop is offline-first and Web online-only with an
independent auth session. Auth is not a sync device or family capability. No
separate recovery/history endpoint or fallback may be implemented. Ordinary
device/capability gates remain authoritative. The 03G endpoint design is kept
as historical research only. 03E remains accepted (run 40844792258, 6/6
SUCCESS); 03F remains STOP at `content_note_reader_required`.

Readiness still has ten explicit blockers; existing Content ordinary owner,
Resolution continuation/prefix and mixed-recovery code are not full production
dispatch or readiness evidence. The next implementation slice is ordinary v10
production dispatch plus account-wide ordered page replay through protected
pull, proven with genuine mixed history/restart and server gate parity. It must
leave capability PUT/readiness activation disabled and preserve refusal for
unready recipients. Subsequent slices address dependency recovery, remaining
Resolution lineage, tombstone/resurrection, local author/self-echo receipts and
multi-device orchestration. A capability declaration is a later separate task,
only after all prerequisites have accepted coverage. Full project opening and
stage 2 remain incomplete; all negative gates and 89,36% remain unchanged.

## 2026-10-29 — WEB.ONLINE.02B.03F: STOP before Content composition

03E SHA `737a8dfd4cca3d34f299a3549d96f45388619ff2` is independently accepted
by owner-reported run **40844792258 — 6/6 SUCCESS**. 03F is **NOT IMPLEMENTED**:
a newly registered Web recipient cannot pull an account containing codec8 Note
history without the full ordinary1/Resolution2 capability claim. Identity
registration does not grant it; current readiness is negative and 03F forbids
capability PUT or gate changes. Section 9 of the source/proof contract explicitly
retains this barrier. No predeclared fixture recipient may stand in for the
missing production contract. Replay stays v4 and no schema or application changes
were made. Four existing gate tests passed on isolated Docker PostgreSQL 16;
Content Chromium acceptance and full prepush were NOT RUN at this STOP.

03H resolves the architecture decision: implement genuine reader prerequisites
through the unified ordinary protected sync path (Option A); the separate
delivery proposal is rejected. Continue with the production dispatch/page-replay
slice above. Do not alter gates or declare readiness. Full project opening and
all security/release gates remain blocked; official progress stays **89,36%**.

## 2026-10-29 — WEB.ONLINE.02B.03E: Legacy Note deletion replay

**LOCAL COMPLETE; INDEPENDENT CI PENDING; FULL PROJECT OPEN BLOCKED.** This
bounded slice reuses the real Legacy Note owner for supported peer-origin
tombstones of Notes already established by the accepted initial cohort and
sequential-update path. Deletes are bound to the exact preceding live event;
duplicate deletes and later events after a tombstone fail closed. The owner's
atomic commit and durable proof formats are unchanged. Replay storage remains
v4 without a migration. Real Chromium/FastAPI/PostgreSQL 16 acceptance passed
23 scenarios with genuine encrypted delete events, restart, two projects and
mixed live/deleted Notes; invalid delete parent, revision gap, unknown Note ID
and repeated deletion fail without an applied proof. Full local prepush passed
816 Vitest, 269 PostgreSQL tests, six Chromium scripts, typecheck/build and
backup/restore.
No complete project opening, writer, ACK, capability or release readiness is
claimed. Independent CI for this SHA remains pending; C21/C22/C23/PF6.0/RC and
all release gates remain mandatory. The next slice requires acceptance of its
own bounded owner contract (Content Notes or Resolution); do not infer broad
delete or full-project support from this case.

## 2026-10-29 — WEB.ONLINE.02B.03D: sequential Legacy Note updates

**LOCAL COMPLETE; INDEPENDENT CI PENDING; FULL PROJECT OPEN BLOCKED.** Owner-reported
run **40820462050 — 6/6 SUCCESS** accepts 03C SHA `7f29c55b9b943a9087eafbe81a8b05836a8e5ced`.
03D is bounded internal work in MVP product stage 2 of 6. Initial bootstrap Note
membership remains fixed; later updates qualify only through the genuine Legacy
Note owner, parent/revision causality, AEAD, source and durable history. Replay v4
holds one candidate per proven event and the latest head/projection per Note.
Recovery rechecks server wire and all owner proofs after restart. The real
Chromium/FastAPI/PostgreSQL acceptance passed 19 scenarios, including multiple
updates in two projects, interruption, restart, corruption, wrong parent,
revision gap, fork, AMK lock and logout refusal. Full local prepush passed
816 Vitest, typecheck/build, six Chromium scripts, 268 PostgreSQL tests and
backup/restore. New independent CI remains pending. No ACK, capability, push, writer or full project
opening is enabled. Content Notes, Resolution, deletion and other project
families remain blocked; C21/C22/C23/PF6.0/RC and all negative gates remain.
The next internal slice needs a separate accepted owner composition for one
remaining Note family or deletion cohort.

## 2026-10-28 — WEB.ONLINE.02B.03C: initial Legacy Note owner replay

**LOCAL COMPLETE; INDEPENDENT CI PENDING; FULL PROJECT OPEN BLOCKED.**
The 03B SHA `7fdcde1515a3eb777c0eef27a7b97e03a8bc95ac` is independently
accepted by owner-reported run **40734155479 — 6/6 SUCCESS**. The accepted six
MVP product stages in the checkpoint are unchanged. This is a bounded internal
step in stage 2.

The complete server account feed now admits the genuine initial Legacy Note
create cohort through `BrowserLegacyNoteApplyRuntime`, in exact server order
before later Metadata genesis where necessary. A server-authenticated seed and
fresh registry permit a technical `import_remote` binding before the first
Note; it does not stand in for a Metadata proof. The existing owner commits
encrypted source/Note, candidate/head history, inbox `note_proven` and derived
coverage atomically under full inventory/generation CAS. Imported Metadata and
Stage still use their own owners and proofs. Restart rechecks server wire,
source AEAD, genesis/cohort and each genuine Note/history proof; exact retry
performs no write. The physical replay schema remains **v4** with no new store
or migration; developer v12, Desktop SQLite and PostgreSQL are untouched.

Only ordinary initial Legacy creates are supported. Subsequent Legacy updates
or deletes, Content Notes, Resolution, forks, conflicts, other data families,
changed server generations and full project UI stay blocked. No capability,
ACK, push or writer activation occurs. `projectOpened=false`, all readiness
and writer flags false, Registration CLOSED, PUBLIC RELEASE BLOCKED and
C21/C22/C23/PF6.0/RC remain mandatory. Official progress **89,36%**.

Local prepush passed **816 Vitest / 29 files**, typecheck/build, six frontend
Chromium scripts, **267 PostgreSQL backend tests** and one real backup/restore.
The new isolated FastAPI/PostgreSQL/Chromium acceptance passed **23 scenarios**
with three genuine Notes, Metadata, Stage and StageOrder, two complete browser
restarts, partial-cohort continuation, quota/key rollback, corruption and
server-drift refusals, exact readonly source text and zero reader writes.
Focused final **91 Vitest / 4 files**, changed ESLint, syntax and diff checks
passed. Linux x86-64 remains unverified locally; new independent CI is pending.

Next minimum within stage 2: specify and implement one supported continuation
cohort for subsequent Legacy/Content/Resolution Note history with genuine
source, dependency and conflict proofs; keep unsupported account history
blocked. Recommended **GPT-6 Sol / High**.



## 2026-10-28 — WEB.ONLINE.02B.03B: private encrypted Note source seed

**LOCAL COMPLETE; INDEPENDENT CI 40734155479 — 6/6 SUCCESS; FULL PROJECT OPEN BLOCKED.**
Baseline `57abfbc785c9e8c849e2b6fbc9f78208ab7b47a8` and owner-reported
**40675867782 — 6/6 SUCCESS** accept 03A. The separate published documentation
commit `77a8d349` establishes the six user-facing MVP stages in the checkpoint;
03B is an internal part of stage 2, not a new product milestone.

The existing account receiver plus real Metadata/Structural certificate checks
now permit one private, server-derived Note source seed. Genuine Metadata genesis
and the initial Legacy Note cohort are authenticated against active bootstrap,
origin device, project, account, count/max sequence and full retained history.
The shared Metadata head validator rejects fork/delete, foreign origin and
nonlinear later Metadata; additional Notes outside the cohort also reject.
Initial Notes before Metadata are supported for seed verification only. Existing
AMK local-source helpers encrypt the empty source; its technical seed reference
and source commit atomically under fresh server checks and full inventory CAS.
Restart reauthenticates server wire, source AEAD and owner proofs without a write.
Replay **v4** migrates v1/v2/v3 manifests while preserving wire/proofs; pinned
v3 refuses v4 and failed migrations roll back. Developer v12, SQLite and
PostgreSQL are unchanged. No Note event is applied, no cursor/ACK advances,
and no source is exposed through the local workspace or project UI.

Local prepush passed **816 Vitest / 29 files**, typecheck/build, six frontend
Chromium scripts, **266 isolated PostgreSQL backend tests** and backup/restore.
Focused seed Vitest passed **18** cases; the
real API/Chromium test passed **19 scenarios** with 104 events/two projects,
rollback, restart, idempotence, corruption and no plaintext/HTTP writes.
ESLint, static/workflow/six-job checks and diff check pass; Linux/x86-64 remains
unverified locally. Official progress **89,36%**; all readiness/writer/open
flags false, Registration CLOSED, PUBLIC RELEASE BLOCKED, C21/C22/C23/PF6.0/RC
mandatory. Next internal slice of user stage 2: genuine Legacy Note owner for the
initial seed cohort, including events before Metadata, without fake dependency
proofs or premature project opening. Recommended: **GPT-6 Sol / High**.

## 2026-10-28 — WEB.ONLINE.02B.03A: genuine Structural replay

**LOCAL COMPLETE; FULL PROJECT OPEN BLOCKED; NEW CI PENDING.**
Baseline `3cb31ff63546d8be388013184a168c2babe88678`, clean `6.0` and matching
origin. Independent acceptance: slice 1 run **40601063694**, slice 2 run
**40610547517**, both **6/6 SUCCESS**. New commit CI remains pending.

The existing account receiver now has one ordered Metadata/Stage/StageOrder
dispatcher, calling the actual owners' opaque prepare/commit paths. Real earlier
Metadata proofs qualify Stage; authenticated Stage heads, exact membership and
reference coverage qualify StageOrder. No new reducer, editor or sync engine.
Strict atomic head/inbox/journal writes use complete inventory/generation CAS;
restart performs fresh server reconciliation and force-reauthenticates every
owner family, without repeated writes. No plaintext payload projection persists.

Only isolated replay **v3** is introduced: validate/preserve genuine v2 Metadata
state (and v1 receipt), change manifests only, add derived `applied_cursor`.
`metadata_cursor` remains the initial Metadata-only prefix. Actual Structural
proofs stay inbox5/6 and their original heads, without schema/proof substitution.
Migration rollback/old client refusal preserve bytes. V12, SQLite and PostgreSQL
remain unchanged.

Ordinary create/update, two stages, exact order/reorder, projects/pages and
restart are supported for the peer-origin cohort. Later new Stage membership
after ordering, delete/fork, missing dependencies, omitted Stage, stale context/
server or bad proofs reject. Existing owned-generation growth refusal remains.
Full prepush **PASS**: **798 Vitest / 28 files**, typecheck/build, all six
frontend Chromium scripts, **265 PostgreSQL/backend tests** and one real
backup/restore. Focused owners: **197 tests / six files**. New real Chromium/
FastAPI/PostgreSQL acceptance: **43 scenarios**, including genuine v2 Metadata
migration, real Structural proofs, native rollback/restart and no repeat writes;
existing 47 Metadata, 17 storage and 10 receiver scenarios remain. Final focused
repeat after prepush also reads exact edited Stage winners and `b → a` order
through actual readonly owners. ESLint/static/six-job/diff checks pass. Final
focused arm64 duration 34.73s; Linux/x86-64 is NOT RUN locally. Existing six jobs,
40-minute budgets and Metadata/browser checks are retained; foundation unchanged.

Official progress **89.36%**; no complete-project criterion is closed. All
readiness/writer/open flags remain false, Registration CLOSED, PUBLIC RELEASE
BLOCKED and C21/C22/C23/PF6.0/RC mandatory. Next minimum: **03B private encrypted
source seed**, fed by original history, fresh registry/generation and real
Metadata/Structural proofs, with atomic cohort/mapping/source validation.
No local CRUD initializer or synthetic revision is a substitute. Recommended:
**GPT-6 Sol / High**. No next task started; new independent CI still required.

## 2026-10-28 — WEB.ONLINE.02B.02: actual Imported Metadata owner

**METADATA-ONLY LOCAL COMPLETE;
FULL PROJECT OPEN BLOCKED; NEW CI PENDING.** Clean starting SHA
`bdfb5601d2d4b512fc76e424c5b0db1ae87dc67c`, branch `6.0`, independently
accepted by owner-reported **40601063694 — 6/6 SUCCESS**. No Actions polling.

Slice 2 now binds `BrowserImportedMetadataApply` and real prefix proof
reauthentication to isolated `worta-web-replay` **schema v2**. The atomic v1→v2
migration changes manifests only and preserves all original encrypted wire and
technical state. Old native v1 clients refuse v2. Corrupt/aborted/quota migrations
roll back; v12/SQLite/PostgreSQL are unchanged.

The supported path authenticates peer-origin, active-bootstrap, coverless
Metadata genesis and ordinary linear updates across projects/pages. Existing
owner tokens/transition/proof formats are reused. Import binding, real owner
head, applied inbox proof and derived replay journal commit atomically with
full account inventory/generation CAS and fresh server registry/mode/device
checks. Durable proofs are cryptographically reauthenticated after complete
server-prefix reconciliation on every restart, without duplicate application.
The shared timestamp contract handles exact server timestamp representation
without changing raw wire or accepting any actual time mismatch.

This is an explicit restricted diagnostic, not a new project UI or sync engine.
`projectOpened=false`; receipt-only still means zero proofs. Unsupported family,
self-echo/source, fork, missing parent, cover/delete/generation, stale server,
key/session/context loss and bad proofs refuse. Owned-generation snapshot growth
is blocked and preserves proofs, pending a separate refresh contract. No source
seed, structural/Note/native-family composition, ACK/push/capability/writer.

See the [contract implementation record](WEB_ONLINE_02B_C21_SOURCE_PROOF_CONTRACT.md#slice-2-implementation-record--webonline02b02-2026-10-28)
and checkpoint for exact local evidence. Full prepush **PASS**: 631 Vitest /
23 files, typecheck/build, six frontend Chromium scripts, 264 Docker PostgreSQL
backend tests and one real encrypted-blob/PostgreSQL restore test. New owner API
acceptance: **47** real Chromium scenarios, including native v1→v2 migration,
full restart and no repeat writes; existing 17 native replay and 10 receiver API
scenarios remain. Changed ESLint/static/diff checks pass. Focused arm64 API run
measured 28.73s; Linux/x86-64 compatibility/timing is NOT RUN locally. No work
is added to foundation; existing content-action carries the bounded real API
acceptance. All six jobs and 40-minute timeouts remain.

Progress stays **89.36%** until a formal reassessment. `ready=false`,
`declarationAllowed=false`, `writerAuthorized=false`, `projectOpened=false`,
Registration CLOSED, PUBLIC RELEASE BLOCKED, C21/C22/C23/PF6.0/RC unchanged.
Next minimum: slice 3 structural/source foundation, separately bounded, with
owned-generation advancement explicitly specified before changed snapshots.
Recommended model: **GPT-6 Sol / High**. No later slice started.

## 2026-10-28 — 02B implementation slice 1: isolated ciphertext receipt

**LOCAL COMPLETE — CIPHERTEXT RECEIPT; INDEPENDENT CI PENDING;
FULL PROJECT OPEN BLOCKED.** Starting
SHA `a092ed2e84ceb18836d83dbec7118e060d8d07f4`. With the owner's explicit
storage authorization, a separate `worta-web-replay` initial schema v1 and an
account-wide paged receiver now implement slice 1 of the
[02B contract](WEB_ONLINE_02B_C21_SOURCE_PROOF_CONTRACT.md). Existing v12,
SQLite and PostgreSQL schemas/data remain unchanged.

The explicit unlocked-session API returns only `received_ciphertext` after
complete dense sequence/identity checks and final fresh server snapshot checks.
The database stores original ciphertext and technical receipt/generation state;
all owner/source/outbound compatibility stores stay empty. Atomic page commit
reuses existing durability and includes inbox/cursor/manifest CAS. Restart
revalidates all retained wire against server history from zero, then resumes;
exact page replay does not duplicate rows. Network/server/lifecycle errors never
return cached success. Unsupported schemas, source/lineage rollback, unknown
descriptors, gaps and duplicate identities reject; only the current account's
known owner-free cache can be rebuilt for a newer server snapshot.

No owner apply/source seed, project opening, decryption guarantee, capability
declaration, ACK, push, new project UI or background sync was added. Receipt is
not application evidence. `projectOpened=false`, `ownerProofs=0`, `ready=false`,
`declarationAllowed=false`, `writerAuthorized=false`; Registration CLOSED,
PUBLIC RELEASE BLOCKED and C21/C22/C23/PF6.0/RC stay mandatory. Progress remains
**89.36%** pending formal reassessment; no full-project criterion is closed.

Focused checks passed: 67 Vitest, typecheck/changed ESLint, 17 actual Chromium
storage scenarios and 10 real Chromium/FastAPI/PostgreSQL receipt scenarios.
Local arm64 timings were 3.42s for final storage and 5.83s for the new backend test.
Existing Frontend admin/content-action jobs carry these bounded checks;
foundation load and all six mandatory names/timeouts are unchanged. Linux/remote
acceptance remains independent. Final prepush PASS: 501 Vitest/18 files,
typecheck/build, all six frontend Chromium scripts, 263 backend tests and one
real PostgreSQL/encrypted-blob backup/restore. This includes the final persisted
row budget guard and its native before-write refusal regression. YAML/shell/
Python syntax, changed ESLint, six-job configuration and diff checks passed.

Next: compose imported Metadata's real owner through the replay transaction and
generation adapter, including schema/record compatibility before permitting any
applied state. Keep Metadata-only preview truthful. Subsequent structural/source,
Note/Resolution and native-family slices remain gated and were not started.
Recommended model for the next slice: GPT-6 Sol / High.

## 2026-10-28 — WEB.ONLINE.02B source/proof contract — DESIGN COMPLETE

Reviewed against clean baseline `57abb27ed712378a836ba654bc3c1c107ac804d5`.
The [C21 Online-Only source/proof contract](WEB_ONLINE_02B_C21_SOURCE_PROOF_CONTRACT.md)
resolves the storage design raised by .02A through a proposed isolated encrypted
replay cache. Server history remains authoritative; browser proofs record real
owner transactions over a reconstructible projection. Cache presence cannot
authorize offline opening or replace a fresh server check. Existing developer
`worta-web` v12 content/proofs are neither imported nor promoted.

The contract includes the complete owner matrix, replay diagram, account-wide
cursor/registry inventory, proof atomicity, initial-source and mixed transport
barriers, failure/restart rules and ordered acceptance slices. The audit confirms
that Maps/Documents/Progress/Game and catalog owners currently execute native
Rust/SQLite transactions; their shared owner logic needs browser adapters before
full reading. Codec support and Metadata preview do not close those gaps.

No fundamental conflict exists between Online-Only and durable reconstructible
evidence under this proposed boundary. Implementation remains gated: a new
`worta-web-replay` initial schema needs a subsequent explicit task; no SQLite,
PostgreSQL or existing IDB v12 migration is required for the bounded design.
All code, schemas, capability gates and the restricted Metadata preview remain
unchanged. This is a design proposal, not FULL PROJECT OPEN or C21 closure.

Next minimum implementation: isolated replay schema/transaction adapter and
paged full-account receiver. It must retain only original ciphertext/technical
receive state, mint no applied proofs, expose no opened project, preserve v12,
and prove restart/replay/isolation/rollback with real producer and Chromium/API
tests. Then compose Metadata, structural/bootstrap source and bounded Note/
Resolution owners; native families and complete publication follow only after
their own acceptance. Recommended model for slice 1: GPT-6 Sol / High.

No migrations, runtime tests or implementation were performed here; documentation
links/paths and `git diff --check` are the local checks. No CI was polled.
Progress stays **89.36%**, `ready=false`, `declarationAllowed=false`,
`writerAuthorized=false`, Registration CLOSED, PUBLIC RELEASE BLOCKED;
C21/C22/C23/PF6.0/RC stay mandatory. .02A below is the historical implementation
STOP; .02B supplies its source/proof design, without claiming production readiness.

## 2026-10-28 — WEB.ONLINE.02A production reader audit / STOP

Baseline `3ef704c5f4353fcfdf4d0921739ce2213fd052de` was independently accepted
by Cloud run `40578946929` (**6/6 SUCCESS**, owner-reported). WEB.CLOUD.READ.01
is present and accepted as a bounded **Metadata-only preview**: it validates the
whole bounded account v3 sequence, canonical AEAD, bootstrap/descriptor joins,
Metadata history and registry/transport stability. This is not full project
opening, project-content restoration, or C21 apply/recovery.

WEB.ONLINE.02A stopped before implementation because the existing C21 owners
are not composable with live server pull as a read-only Online-Only projection.
`BrowserV10Recovery`/`BrowserV10Transport`, Stage/StageOrder apply and Note
recovery are bound to a complete durable local inbox/source inventory and
atomically record owner/applied proofs. The Online-Only catalog/read path must
not promote the v12 developer workspace to project authority, nor invent
ephemeral proofs in place of owner evidence. Adding codec dispatch alone would
not authenticate dependencies, history, conflicts or applied state.

| Scope | Existing support | Web production-read gap |
| --- | --- | --- |
| Metadata | Shared codec/reducer, AEAD, account-wide cursor and active bootstrap validation; bounded preview only | Not a complete project. Cover material, non-Metadata families and unresolved/deleted heads fail closed. |
| Stage / StageOrder | Codecs and durable local apply owners check Metadata/Stage dependencies, history, tombstones, self-echo and owner-head CAS | Owners require IDB inbox and durable proofs; no online-only server projection/recovery path. |
| Legacy/Content Notes and Resolution | Codecs, causal apply, mixed-source/recovery owners and historical transport join exist | Production server-page dispatch/replay, complete dependency/source coverage and truthful readiness are unfinished; Note readiness remains negative-only. |
| Maps / Documents / Progress / Game | Canonical codecs and protected sync runtimes exist | Runtimes use durable protocol state and capability gates; not connected as production read-only owners. |
| Folder/catalog account events | Canonical account crypto decoder exists | No composed account-history owner; account events cannot be removed before validating the shared cursor. |
| Mixed history / transport | v3 pull, account registry and server-side ID0/ID1, family/device gates exist | Existing v10 recovery consumes persisted inbox; all-device readiness is not proved by Metadata preview. No capability PUT or ACK is permitted here. |

Specific boundaries: Legacy-v2 and Content-v3 Note histories have distinct
authenticated owners; mixed replay is not a production dispatcher. A Legacy
Resolution carried by v3 has an exact retained-identity join only against
persisted inbox evidence. Missing/dependent/orphan events are owner barriers,
not skippable pages. Multi-project account ordering and sequence continuity are
checked by Metadata preview, but no project can be projected past an unsupported
account event. Source device/header and AEAD identities are checked by existing
owners; this does not establish all-device readiness. ID0/ID1 support and
compression-reader gates remain server-controlled. Unknown versions/families,
deletions, conflicts and unsupported histories fail closed in their respective
owners; there is no uniform production project reducer. Read cancellation,
logout and AMK lock clear/abort the current Metadata preview, while the dormant
v10 lifecycle applies to persisted inbox work, not a live cloud session.

**Next minimum step:** settle a C21-compatible online source/proof contract for
replaying complete server history through the existing owners, including restart,
all-device/capability gates, mixed account ordering and safe refusal. Then
implement only the bounded production dispatcher/page replay slice authorized
by that contract (recommended model: GPT-6 Sol / High). No project-open UI or
readiness claim until complete family and recovery requirements are met.
Desktop offline-first/SQLite and Web
online-only/server-authoritative remain unchanged. `ready=false`,
`declarationAllowed=false`, `writerAuthorized=false`, Registration CLOSED,
PUBLIC RELEASE BLOCKED, C21/C22/C23/PF6.0/RC mandatory; Web progress remains
**89.36%**.

**MVP target: full Desktop feature parity in Web is mandatory.** Reuse the
existing shared Vue/TypeScript screens, Tiptap and Mind Elixir, business rules,
models, codecs and E2EE cloud protocol. The required user actions include
projects/folders, project organization and archive, ordered/completable stages,
project/stage text editing, Notes and their properties, mind maps and Note links,
writing goals, progress/statistics/history, the full Game Mode, settings and
personalization, and browser-supported import/export. Only genuine native-API
differences may use browser equivalents; Web Lite is not an MVP substitute.

**SUPERSEDED BY WEB ONLINE-ONLY DECISION:** the earlier plan for full offline Web
parity, durable IndexedDB copies of all user data, offline queues/retry, and
browser-only manuscript recovery. Historical acceptance records remain valid as
evidence of the existing developer-only workspace; they are not the target
product contract. IndexedDB v12 and its data are retained without automatic
cleanup or import. The local workspace remains a clearly labeled legacy
developer/test tool and is never merged into the cloud catalog.

Previous baseline SHA `6011d0d04cb1e7b75372c778de4e5cc2e615e931` was independently
accepted by run `40541798752` (**6/6 SUCCESS**). LEGAL.ACCEPTANCE.02 and M1.02
remain historically accepted. MVP6.0.M1.03 was cancelled by owner decision before
commit: **CANCELLED / OBSOLETE BY OWNER DECISION**; its partial uncommitted
v13/local map experiment was discarded explicitly and no v13 schema was
published.

## Feature matrix

Web status dimensions are separate: **user action**, **server path**, **server
storage**, **E2EE**, **Desktop format compatibility**, and **gate status**.
`PARTIAL` means only a bounded component exists; it is not an accepted user flow.

| Capability | User action in Web | Server path | Server stores data | E2EE verified | Desktop format compatible | Status / next work |
| --- | --- | --- | --- | --- | --- | --- |
| Cloud project catalog | IMPLEMENTED in developer account flow; IDs/count/limit, empty state, refresh, network error | `GET /api/v1/cloud/projects`, authenticated ordinary-user token via session | Server-owned catalog; no catalog persistence in IndexedDB | N/A: IDs and limits only | IDs are not names/content | Local fixture acceptance added; verify backend regression in prepush |
| Ciphertext account receipt | Explicit unlocked-session method; no project UI/opening result | Existing authenticated v3 account pull, registry and mode GETs | Server remains authoritative; isolated reconstructible replay IDB v1 | Original AEAD ciphertext retained; content/owner authentication deferred | Original receive protocol, descriptor, nonce and ciphertext preserved | Slice 1 LOCAL COMPLETE/prepush PASS; no proofs or source initialization; independent CI pending |
| Project names/content | Bounded Metadata-only preview accepted at `3ef704c5`; full project/content opening remains blocked | Real authenticated account-wide v3 pull plus shared Metadata decoder/reducer | Existing ciphertext protocol; preview remains in memory | Bounded Metadata read accepted; other families uncomposed | Shared formats retained | .02B defines source/proof design; next is isolated replay receiver, then accepted owners |
| Stages and structure | NOT IMPLEMENTED in target online flow; legacy local CRUD remains dev-only | No Web cloud product flow | Not confirmed by this slice | Not verified in Web flow | Existing codecs/owners only | ONLINE.04 after accepted read path |
| Project/stage documents | Editor component exists; server flow NOT IMPLEMENTED | No writer connected | No Web document write in this slice | Existing local M1.02 is not cloud E2EE acceptance | Desktop codec exists; cross-client Web flow unverified | ONLINE.03 after writer gates |
| Notes and maps | Legacy/local developer experiences only | No cloud Note/Map writer connected | No Web write in this slice | Not verified as server flow | Existing codecs exist; Web compatibility unaccepted | ONLINE.04; keep C21 gates frozen |
| Progress and Game Mode | UI paths exist but are not target server-authoritative flow | Legacy plaintext routes are not permitted for E2EE data | Not verified | Not verified | Desktop models/codecs exist | ONLINE.05; no plaintext fallback |
| Account and legal access | IMPLEMENTED bounded login, legal acceptance and account key setup in developer flow | Auth/legal APIs | Server-owned identity/legal records | Key remains in client; no key sent with catalog request | N/A | Keep public registration CLOSED |

## Online critical path

### FASTTRACK.01 — next user action: open a server project

**Status: BLOCKED; no project-open code or mock success was added.** The
existing `GET /api/v3/sync/encrypted/pull` is the correct opaque transport, but
requires a registered device and negotiated v3 transport; if retained history
requires compressed ID1 frames, compression-reader support is also required.
The server also rejects pull when the account history includes
families for which that device lacks the corresponding accepted capability.
Because the feed has one account-wide sequence, filtering to a selected project
before validating earlier events and dependencies would violate the existing
source/history contract. `BrowserV10Recovery` is an isolated manual consumer of
already persisted inbox pages, not an online Web reader; the normal session does
not compose the production v10 dispatcher, page replay, dependency fetching,
family readers or lifecycle orchestration. `BrowserNoteReaderReadiness` remains
negative-only. The next concrete prerequisite is truthful C21.2.08 production
reader/recovery orchestration with complete required-family coverage and
accepted all-device readiness. Until that contract passes, do not declare reader
capability, ACK, or show a project ID as an opened project.

The backend's real API and security gates were exercised locally by the isolated
PostgreSQL 16 backend acceptance (261 tests plus encrypted-blob backup/restore);
those tests do not establish a browser-to-server project-open E2E.

1. **WEB.ONLINE.01 — architecture and server catalog.** Bind the developer-only
   Web account flow to the authenticated server catalog. Keep local test projects
   visibly separate. No content read or write is implied by an ID.
2. **WEB.ONLINE.02 — encrypted server read.** Retrieve opaque E2EE events and
   restore supported content in browser memory only after accepted reader,
   security and recovery gates. Reject unsupported histories without mutation.
3. **WEB.ONLINE.03 — encrypted document save.** Reuse Tiptap and existing codecs;
   send ciphertext through an accepted writer protocol. Show saved only after
   verified server confirmation and revision/protocol checks.
4. **WEB.ONLINE.04 — Notes, Maps and Stages.** Compose existing UI/codecs with
   accepted cloud owners. Preserve map-note links and stage dependencies; no
   plaintext API fallback.
5. **WEB.ONLINE.05 — Progress, Game Mode and Settings.** Add only supported
   server-authoritative encrypted flows with safe rejection of unsupported data.
6. **WEB.ONLINE.06 — Desktop ↔ Web acceptance.** Verify format compatibility,
   conflicts, recovery, authorization and cross-client preservation against
   isolated backend and PostgreSQL fixtures.

These stages may be refined after checking frozen C21/C22/C23 contracts. They
must not activate reader declaration, writer authorization, ACK, auto-bootstrap
or background synchronization before their exact gates pass.

## Safety and release boundary

- Desktop offline-first and SQLite behavior remain unchanged.
- The ordinary plaintext compatibility `/api` routes are not an E2EE fallback.
- `localProjects`, `localStages`, `localNotes`, `localDocuments` and the v12
  schema remain for existing data and developer tests. No automatic import,
  promotion, cleanup or cloud-catalog substitution is allowed.
- On network loss, show an error and retain current unsaved input in memory where
  possible. Do not claim persistence or restart recovery.
- LEGAL.ACCEPTANCE.02 remains required; legal text approval is separate.
- Official Web progress remains **89.36%** pending formal reassessment.
  `ready=false`, `declarationAllowed=false`, `writerAuthorized=false`,
  Registration CLOSED, PUBLIC RELEASE BLOCKED. C21/C22/C23/PF6.0/RC remain
  mandatory. No production VPS/database changes are authorized by this roadmap.

See [WEB ONLINE-ONLY architecture](WEB_ONLINE_ONLY_ARCHITECTURE.md) and the
current [checkpoint](../WORTA_CHECKPOINT.md).
