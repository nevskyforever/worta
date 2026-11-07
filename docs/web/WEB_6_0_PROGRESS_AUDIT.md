# WORTA 6.0 — ревизия чтения Web и reader qualification, 06.11.2026

## 2026-11-06 — Safety verification and two-SQLite freeze acceptance (PARTIAL)

Starting/Final HEAD `7ba13869b2cd73fbd1b76182a7bcd4722aad9081`, branch6.0;
eight pre-existing modified files preserved. Native writer test fixtures now create
exclusive UUID directories before opening state.sqlite; peer.sqlite is created only
inside that owned directory. Cleanup unlinks exact created DB files only; no
recursive directory removal. The original erroneous operation was
remove_dir_all(path0.parent()) for a file directly under
/var/folders/nf/msldlpgd7hg0vxq35d5dhlj80000gn/T. Its Result was ignored. Surviving
/tmp compilation/test logs show execution continued to a fixture-ownership failure,
but contain no list of deleted files or cleanup return status. There is no prior
inventory to establish consequences; loss of unrelated temporary files cannot be
confirmed or excluded. No guessed recovery or extra cleanup was attempted. Current
temp root and workspace exist; source modifications remain present. No ongoing
recursive-delete call remains in game_writer.rs. Isolated fixtures were changed
before further mutating tests ran.

Extended the real freeze command test to TWO independent SQLite files: baseline
snapshot into peer.sqlite via VACUUM INTO before the command, then a distinct peer
device identity. Real source command captures the event; privileged Native apply
on B reproduces the account state. A B-ledger insert trigger aborts and proves
unchanged payload/tips; retry and redelivery yield one ledger outcome and one item
debit. Both files reopen/rebuild successfully. This is real Native owner integration
at the post-decryption boundary using existing test envelope fixtures, NOT fresh
E2EE transport/HTTP/Chromium acceptance. Corrected eligibility lookup to canonical
personal_goal and streak_enabled metadata keys. No crypto/gate/proof format change.

Still NOT COMPLETE: production streak-growth command/capture is missing, and full
engine.py day-plan/topology/group eligibility parity is not implemented. Owner's
choice of engine.py is settled. No assertion that these gaps are technically
impossible, and no new product permission is needed. Next work must implement
that runtime behavior and prove both scenarios, not re-audit the settled choice.
Do not ship or commit this incomplete Native feature as accepted.

Readiness2/10, seven Web reading groups, stage2/6 Slice4/5 PARTIAL unchanged.
ready=false, declarationAllowed=false, writerAuthorized=false. No main, VPS,
deployment, registration, browser capability PUT, ACK, extra worktree or CI access.
Eight files remain uncommitted; no commit/push before the full mandatory Native
acceptance. Full Web, all game mechanics, editing and bidirectional unified sync
remain required. Final test counts follow; prior checkpoint history is retained.

Final checks for this pass: fresh Cargo compilation and 5 game_writer tests,
15 game_sync tests, 4 game_codec tests PASS (24 total). Includes genuine two-file
freeze apply, receiver rollback/redelivery/reopen and loss-v2 regressions. Maintained
quick/diff checks PASS. Generated cleanup dry-run only; no --apply. No new tests or
results are claimed for growth or full Python-rule parity.

## 2026-11-06 — Native global freeze command/capture/apply implementation (PARTIAL)

Starting/Final HEAD `7ba13869b2cd73fbd1b76182a7bcd4722aad9081`, branch6.0.
The four prior uncommitted files are preserved. Owner has already authorized
engine.py as shared rule source; no further permission for that choice is needed.

Implemented in existing Native modules: global freeze command for enrolled Game
accounts invokes a real SQLite transaction; runtime ownership/game mode/day checks,
project-source eligibility rejection, captured account-parent equality, native
stable event ID bound to day/history/count, canonical freeze event, real inventory
and history projection, write intent and game_state commit. Existing game_sync
can rebuild/apply that standalone global freeze with exact before/after/count
checks and original schema. capture_loss shares its existing commit mechanics;
loss semantics and crypto/proof formats remain unchanged. Added an isolated bridge
step that calls the real command, not an artificial event generator. No Web work.

Actual Native command test PASS: 2 items become1, history gains a frozen day,
canonical event captured and rebuilt state equals local account_base, repeated
command refuses, SQLite reopen retains head. A real BEFORE UPDATE trigger aborts
game_state and verifies unchanged payload/tips before successful retry. This is
NOT a second-device apply test. Streak growth command/capture is still missing.
Standalone eligibility is not yet complete parity for all engine.py topology,
daily-plan and group cases; non-enrolled legacy freeze still takes its old route.
Do not ship/declare full Native completion or enable capabilities from this work.
Remaining mandatory acceptance: growth from actual writing/day-plan with rewards,
complete grouped freeze eligibility/dependencies and apply on second isolated
SQLite, then reopen/redelivery/rollback parity. No HTTP/Vue/Chromium requested.

Safety incident in the first added test: cleanup erroneously attempted recursive
removal of the parent temporary directory and ignored its result. The test was
corrected to remove only its generated DB file; whether unrelated temporary files
were removed cannot be established without a prior inventory. Current temp root
exists. Workspace user/source files and Git changes are preserved. No further
recursive cleanup is used; maintenance cleanup is dry-run only.

Current uncommitted runtime files: game_codec.rs, game.rs, game_writer.rs,
game_sync.rs, c15_headless_test_bridge.rs; three documentation files also modified.
No commit/push until mandatory Native acceptance is complete. CI not started or
polled. Stage2/6, Slice4/5 PARTIAL, readiness2/10 and seven Web reading groups
unchanged. ready=false, declarationAllowed=false, writerAuthorized=false. No
browser capability PUT/ACK, VPS/deployment/registration/main/worktree change.
Full Web/game/editing/bidirectional unified sync remain product requirements.
Next: finish growth and grouped eligibility, then true second-Native acceptance.

Verification in this pass: fresh Cargo build PASS; game_writer5, game_sync15
and game_codec4 tests PASS (24 total, including the actual new command/rollback/
reopen test and existing loss regressions). Maintained quick and diff checks PASS.
Generated-output cleanup dry-run completed; no --apply cleanup. No second-device
Native apply, full growth scenario, HTTP or Web acceptance is claimed.

## 2026-11-06 — Owner resolved canonical rules: engine.py → Native

Owner explicitly answered: adopt existing engine.py streak/freeze rules as the
common Native/sync semantics, including daily plans, Project/Stage, eligibility,
history and rewards. This supersedes the pending-rule question below; DO NOT ask
for this decision again. Starting/Final HEAD remains
`7ba13869b2cd73fbd1b76182a7bcd4722aad9081`, branch6.0. Initial tree contained the
three documentation edits, all preserved.

Implemented an initial Native rule function in existing game_codec.rs:
`global_freeze_after` matches the history mutation in apply_global_streak_freeze:
requires enabled streak ending yesterday, appends one frozen day, derives count
from actual frozen entries, preserves maximum/reward/loss markers. A real Cargo
compile plus the new focused test PASS; all four game_codec module tests PASS
on the freshly built binary nfprogress_desktop_lib-fdb51ddf4f28ea15. This is NOT
Native command/capture/apply or end-to-end acceptance. The function is not yet
called by production command/apply; no item/history transaction or proof is
invented. No gameplay admission/capability was enabled. History growth, reward
port, grouping/eligibility and Native capture/apply remain unimplemented.

Four files currently uncommitted: game_codec.rs plus checkpoint/roadmap/audit.
No commit/push because the required real user scenarios remain unaccepted.
No new HTTP/Chromium/restart/rollback/typecheck/build claim. Generated-output
maintenance dry-run completed after Rust compilation; no cleanup applied, no user
files/data removed. Quick/diff checks recorded in the task result. CI untouched.
Stage2/6, Slice4/5 PARTIAL, readiness2/10 and seven remaining reading groups stay
unchanged. ready=false, declarationAllowed=false, writerAuthorized=false; no
browser capability PUT/ACK, main/VPS/deployment/registration/worktree changes.
Next implementation: use the approved engine.py rules to complete Native
eligibility, grouped item/history transaction and writer/apply dependencies;
then add Web processing and real PostgreSQL/FastAPI/Chromium acceptance. The
owner's full Web/game/bidirectional single sync engine requirements remain in force.

## 2026-11-06 — Streak/freeze: canonical-rule decision pending

Starting/Final SHA `7ba13869b2cd73fbd1b76182a7bcd4722aad9081`, branch6.0,
initial tree clean. No Native/Web runtime or wire format changed in this pass.
Existing C18_GAME_MUTATION_AUDIT.md explicitly requires complete coupled freeze
semantics and states Native item-only consumption differs from Python. Evidence:
engine.py::apply_global_streak_freeze checks eligible yesterday and project/group
sources, decrements one item and appends the freeze history marker; Native
GameApplicationService::freeze currently just decrements the item. Growth also
uses Project.get_streak_status/get_today_goal_value and global source selection,
not any positive Progress delta. Publishing the Native debit alone would violate
the existing owner contract; adding a day on every positive delta changes rules.

An explicit product question is pending: adopt existing engine.py rules as the
shared Native semantics (including eligibility, grouping, daily goals and rewards),
or preserve the current Native behavior and keep unqualified actions blocked.
No response/approval is inferred. While waiting, 28 existing Python reference tests
passed (streak date repair, freeze purchase limit and price), in the isolated pytest
profile. These are reference-rule tests, NOT Native/Web/HTTP acceptance. Rust,
Vitest, HTTP/Chromium, restart and new rollback acceptance were not run; previous
loss-v2 acceptance is preserved and not re-claimed as growth/freeze acceptance.

Stage2/6, Slice4/5 PARTIAL, Note readiness2/10, seven reading groups unchanged.
ready=false, declarationAllowed=false, writerAuthorized=false; no capability PUT,
ACK, main/VPS/deployment/registration/worktree/CI changes. Three documentation files
are uncommitted; no new commit/push. Next: resolve the canonical-rule choice, then
implement it in the existing Native owner and capture/apply, extending the same
Web recovery only after true source/parent/dependency verification. Full Web,
all ordinary/game mechanisms and bidirectional unified sync remain mandatory.

## 2026-11-06 — Loss-v2: actual Native → HTTP → Web implementation

Starting SHA `b91d61d877f3e779022dbdc0210fcf95080567bd`, branch6.0; nine changed
files include the six preserved changes from the previous passes. Product stage2/6,
technical Slice4/5 PARTIAL. Final SHA is the commit containing this entry (reported
in the task result). No previous checkpoint history or owner product requirement
was removed. Full Web, all game mechanics and bidirectional unified sync remain goals.

The existing Game preparation now admits only actual version2 Native standalone
loss, authenticating the stable action identity against the parent streak. Existing
codec checks exact loss date/history/maximum/freeze reset and zero reward. The
existing transition reproduces Native health damage and single resurrection potion
consumption. Game events continue through recoverV10 and the SAME ownerPort/full
snapshot CAS, server checks, encrypted projection, source/ledger/proof/manifest
transaction and reauthentication. No new engine, proof format, store or schema.
Vue now exposes retained global length and freeze count from that verified state.

Real acceptance extends the existing Native Game HTTP fixture: retained3-day global
series, genuine Progress/writing reward, buy2 items, then completion/reward, sell1,
Native `loss` command and production push. A second loss command returns false.
History8→13, Game ledger5→9; account/project encrypted snapshots exactly match Native
SQLite. Both legal project/reward wire orders are tested. Existing login/passphrase
unlock, actual PostgreSQL16/FastAPI Recovery GET, redelivery, account ciphertext
refusal, last-account-projection IDB abort/full rollback, reopen/retry, final restart
and durable account-proof corruption checks are retained. Original wire/import seeds
and binding are preserved; ACK0. No new concurrent race is claimed: existing common
ownerPort CAS is retained. See the verification result below for final run counts.

Remaining requested work is NOT accepted: Native growth/global_streak-v1 and freeze
capture/apply were not implemented in this pass. `game.rs::apply_event(ProgressAdded)`
updates writing rewards/productive counters, not global history; current `freeze`
only decrements inventory and emits a command result. `game_writer::capture_domain`
queues the existing writing/reward pair and checks exact account post-state, while
`game_sync::transition_with_source` rejects the unimplemented action families.
Their codec shapes are not genuine producer semantics or successful owner proofs.
Implement and test the real Native streak growth/freeze state transition and its
capture/dependencies before claiming end-to-end freeze. Do not merely synthesize
an after-history from a local inventory debit. This is an implementation gap, not
a claim that the required feature is technically impossible.

Readiness2/10 closed and seven remaining Web reading groups unchanged. All three
security flags remain false; no browser capability PUT/ACK, main, VPS, deployment,
registration change, new worktree or CI start/poll. Synthetic Native capability PUT
is confined to the existing qualified isolated fixture. Next task remains Native
ordinary global-streak/freeze capture/apply and real HTTP/Web acceptance, retaining
the now-tested standalone loss path. Full account Game and Slice4 are incomplete.

Verification for this bounded loss-v2 change: 2 real Game HTTP/Chromium tests
PASS (73.22s), 57 focused Vitest PASS, 15 existing Native owner Rust tests PASS
(unchanged Native binary; no Native source changed), 30 help/localization tests
PASS; typecheck/build, generated-content check, changed Python compile and quick
PASS. Only one help source key was added per non-Russian language, with existing
translations preserved. The isolated task PostgreSQL16 container/anonymous volume
was verified, stopped and removed. Commit/push contains the tested loss-v2 subset,
including the preserved prior changes, not an assertion of complete streak/freeze.
Normal origin/6.0 push; no CI query. Final exact SHA/clean-tree result is in the
user-facing task result. Earlier PARTIAL entries below are historical evidence.

## 2026-11-06 — Native streak/freeze continuation (PARTIAL)

Starting/Final SHA `b91d61d877f3e779022dbdc0210fcf95080567bd`; branch 6.0.
The six prior uncommitted files and their existing loss-v2 work were preserved.
No new Native capture/apply route was implemented or accepted in this pass.
`game.rs::GameApplicationService::freeze` decrements the inventory item inside
`Self::mutate`; it does not itself write a synchronized freeze Game event or update
streak history. The portable writer `game_writer.rs::capture_domain` currently
queues a writing/completion Project Game event and a linked Account reward, then
requires the rebuilt account snapshot to equal the actual post-action account base;
a mismatch becomes `game_unsupported_local_mutation` and blocks the migration.
`game_sync.rs::transition_with_source` has no version-1 global_streak or freeze
branch. Codec declarations are not proof of capture, apply or dependency safety.
Do not equate a local item decrement with a synchronized freeze or fabricate an
after-state. The pre-existing browser loss-v2 adapter remains uncommitted and has
not passed real HTTP/Chromium/restart acceptance. No Native parity, new CAS race,
rollback, or freeze end-to-end PASS is claimed. See task result for test counts.

Next: trace the actual Desktop streak updater and freeze transaction to their
logical-day, inventory and Progress dependencies, implement a shared Native
capture/apply of its genuine committed outcomes with an account/project causal
proof, then extend the existing browser owner and test real SQLite → isolated
PostgreSQL16/FastAPI → Chromium with restart, redelivery and rollback. Preserve
all six changes and do not infer writer capability from codec IDs. Readiness stays
2/10 closed, seven Web reading groups remain, stage2/6 Slice4/5 PARTIAL. Security
gates stay ready=false, declarationAllowed=false, writerAuthorized=false; no
browser capability PUT, ACK, VPS, main or CI access. No commit or push pending
mandatory local acceptance. Full ordinary/game Web, all game mechanics, creation,
editing and bidirectional unified sync remain the owner's product objective.

## 06.11.2026 — Account Game streak/freeze PARTIAL

Browser standalone Native loss-v2 transition is implemented locally and has focused
unit coverage, but Native SQLite → HTTP → Chromium acceptance is not run. Native
`game_sync` lacks apply for ordinary global_streak-v1 and freeze; `game_writer`
currently captures standalone loss, not those requested event families. Complete
the authentic Native writer/apply contract and end-to-end acceptance next. Existing
readiness 2/10, 7 remaining Web reading groups, stage2/6, Slice4/5 and closed security
gates are unchanged. Full Web, all game mechanics and bidirectional sync remain goals.

Основание: утверждённые владельцем шесть продуктовых этапов в
[checkpoint](../WORTA_CHECKPOINT.md#worta-60--утверждённый-упрощённый-план-завершения-mvp),
[пять технических Slice](WEB_ONLINE_RESOLUTION2_REPLAY_CONTRACT.md#5-sequential-implementation-slices-and-acceptance),
[исходный source/proof contract](WEB_ONLINE_02B_C21_SOURCE_PROOF_CONTRACT.md),
[текущий inspector](../../frontend/src/platform/web/noteReaderReadiness.ts)
и реальные тесты/ограничения production-кода. Новые этапы, release policy,
capabilities или security gates эта ревизия не вводит.

## 06.11.2026 — Account Game: balances/inventory LOCAL COMPLETE (bounded)

Native SQLite → production push → PostgreSQL16/FastAPI → Chromium login/unlock
подтвердил balances, inventory buy2 → sell1, linked writing/completion rewards,
точное равенство обеих зашифрованных projections с Native, redelivery и restart.
Оба законных порядка project action/account reward приняты: received Game window
проверяется в порядке зависимостей и атомарно применяется тем же ownerPort/CAS.
Нет временных proofs, отдельного Game storage/engine, ACK или browser PUT.
Текущая schema v10 и proof format сохраняются. Проектная награда и account-баланс
остаются разными областями. Web не запускает reward consumer при чтении.

**Осталось 7 групп**, а не6: группа5 включает все реальные account-механики,
а не только принятые balances/inventory. Stage Game HTTP, конфликтные/расширенные
histories и ожидание source через промежуточный owner другой семьи не приняты.
Это прежние границы существующей матрицы, не новые release blockers. Note2/10,
этап2/6, Slice4/5 PARTIAL, все три security gates false. Полный Web/все игровые
механики/создание-редактирование/двусторонний единый sync остаются целью владельца.

## Почему число 2/10 не отражает количество сделанных функций

Это **десять исходных критериев полного Note ordinary1/Resolution2 reader claim**,
а не десять функций интерфейса и не процент готовности всей WORTA. Два пункта
уже удалены из активного списка; inspector сейчас содержит **восемь** постоянных
blockers. Дополнительный `local_prefix_blocked` зависит от проверяемой истории
и не является одиннадцатым постоянным критерием.

Recovery GET, Content/Stage/Document/Map/линейный Progress чтение, пагинация и UI — принятые функции
продукта. Их успех не объявляет recovery-only профиль обычным sync-устройством
и не доказывает полный Note claim. Старые формулировки «нет HTTP recovery»,
«не применяется successor», «нет нескольких страниц» и «нет документов» устарели.
В истории checkpoint они сохранены; актуальный статус приведён ниже.

## Все десять критериев

В таблице «закрыть» означает исполнить существующий контракт, а не добавить
новый необязательный тест. Открытый пункт может иметь принятые подмножества.

| Критерий | Статус и уже принятые результаты | Конкретный остаток / что подтвердить для закрытия | Этап и актуальность |
| --- | --- | --- | --- |
| `production_v10_reader_dispatch_incomplete` | OPEN. recoverV10/ownerPort применяет Metadata, Stage/Order, Content, Resolution, own echo; добавлены peer Document и project Map. | Полная заявляемая ordinary1/Resolution2 матрица, а не только choose_version. `resolutionPlanner.prepareSuccessor` и `resolutionHistorical` ограничивают повторные поколения codec8 choose_version; Map/Document route отказывает при `hasResolution(v)`. Эти допустимые смешанные истории ещё не открываются полностью. | Peer-чтение необходимо этапу 2; собственные Web-author части — этапу 3. Критерий не устарел, старое описание отсутствующего dispatch устарело. |
| `transport_v10_page_replay_incomplete` | OPEN для полного claim. Recovery GET принимает две реальные страницы, 104/108 событий, append/re-pin, resume, duplicate и restart; это LOCAL COMPLETE, не повторять как недостающую пагинацию. | Приёмка **обычного защищённого pull** с действительно допустимым browser recipient, полной поддерживаемой смешанной историей и reader/compression gate parity. Recovery-only GET не является доказательством такого pull. | Recovery-пагинация этапа 2 выполнена. Обычная recipient qualification нужна до включения sync в этапе 3. Не требовать её для уже принятого recovery-чтения. |
| `orphan_dependency_recovery_not_accepted` | OPEN. Реальные поздние importRemote/StageOrder, два проекта, Stage/Content prefix, первые и повторные Resolution, rollback/reopen приняты. Native codec8 Stage dependency не может иметь более поздний sequence, чем Content: этот невозможный сценарий исключён контрактом. | Замкнуть реестр достижимых зависимостей полного claim. Конкретный незавершённый путь: own Content P2 после Resolution остаётся за границей подтверждаемого prefix; дальнейший recovery за этим событием не квалифицирован. Для невозможных порядков использовать доказанные writer/server инварианты, не новые искусственные orphan fixtures. | Peer dependency безопасность нужна этапу 2; own P2 — этапу 3. Общая формулировка требует этого уточнения, а не удаления проверки. |
| `tombstone_resurrection_not_accepted` | **CLOSED** ранее. Native project/plain, Stage/plain/html, повторные Resolution/delete/resurrection, proofs/rollback/reopen приняты. Обычный недопустимый tombstone→upsert продолжает отклоняться. | Нет нового остатка в закрытом критерии. | Этап 2. Исходное «вся resurrection отклоняется» устарело; код/учёт уже исправлены. |
| `further_resolution_chains_not_accepted` | OPEN. Два и повторные choose_version поколения с continuation и persisted prefix уже приняты. | Повторные допустимые поколения остальных стратегий frozen claim, включая manual_merge/delete/keep_both и retained branch. Сейчас successor planner явно допускает только codec8 choose_version; отсутствие поддержки не исправляется количеством успешных choose_version циклов. | Чтение Desktop-конфликтов — этап 2. Не устарел; старое «второе поколение всегда unsupported» устарело. |
| `local_author_after_resolution_not_accepted` | OPEN. Реальный browser author intent/seal/receipt/echo после Native choose_version Resolution, несколько правок, delete и ordinals приняты в изолированном транспорте. | Stage-scoped и остальные strategy/source author chains полного claim; дальнейшее продолжение после собственного P2 с аутентифицированным prefix. Принятый project/choose_version случай не заменяет эту матрицу. | Прежде всего создание/редактирование Web, **этап 3**, а не новая read-only функция этапа 2. Не устарел. |
| `own_content_self_echo_not_qualified` | OPEN. Own Content через recoverV10, receipts, ordinals, повреждения, logout, abort и restart приняты. | Реальная независимая Native own-publication ancestry и P2 в Prefix owner: после собственного события sequence6 applied coverage=6, proven_prefix остаётся5; дальнейший replay/qualification за ним не принят. Не приравнивать applied_cursor к ACK prefix. | Этап 3 / полный reader claim. Не блокировать уже безопасное peer recovery-чтение ради этого будущего writer пути. |
| `own_resolution_author_chain_not_accepted` | **CLOSED** ранее. Genuine Native author/publication/receipt ancestry, browser planner/apply/self-echo. Затем own recoverV10 positive, abort и logout дополнительно приняты. | Отдельный own-specific concurrent тест не заявляется: независимая конкурирующая операция в той fixture не создана. Общий ownerPort CAS тот же, что прошёл настоящую конкурирующую приёмку. Это не отменяет уже закрытый author-chain критерий. | Author-компонент этапа 3; безопасность чтения own истории также проверена. Уже корректно снят из inspector. |
| `multi_device_reader_orchestration_not_proven` | OPEN. Desktop→HTTP→Web обновления, account isolation, reopen и несколько локальных Native SQLite histories приняты. Recovery-only Web не блокирует Desktop writer. | Обычные два допустимых sync recipient: полный mixed feed, независимые identity/receipt/ordinal/cursor, отзыв/повторный вход и восстановление после передачи между ними. Это отличается от одного recovery-only профиля и от двух аккаунтов. | Desktop→Web чтение — этап 2; полноценный Web↔Desktop sync и own-author обмен — этап 3. Не устарел. |
| `server_note_gate_not_checked` | OPEN. Реальные unready отказы и Native gate/push приняты; inspector умеет явный read-only gate GET. Recovery GET проверяет auth/account/snapshot без browser declaration. | Реальный eligible browser recipient в **обычном защищённом feed**, актуальный Note/ID1 gate и отрицательный unready recipient. Account-wide gate GET или разрешение Native Desktop не квалифицируют браузер. | Перед обычной sync capability — этап 3 / Slice 4. Recovery GET этапа 2 разрешён отдельно. Browser capability PUT в текущем задании запрещён и не выполнялся. |

Доказательства авторских границ: `own Content self-echo recovery (partial)` в
checkpoint, `test_native_resolution_browser_replay_author_self_echo`,
`test_native_own_resolution_author_receipt_self_echo` и текущие
`resolutionPlanner.ts`, `resolutionHistorical.ts`, `v10Recovery.ts`,
`provenPrefixRecords.ts`. Диагностические тесты в `noteReaderReadiness.spec.ts`
явно сохраняют эти восемь gaps даже при успешном bounded prefix/gate GET.
В этой задаче **нет основания снять ещё один полный критерий**. Результат 2/10
сохраняется по доказательствам, не по инерции и не потому, что Map «не считается».

## Можно ли закрыть Slice 4 и перейти к Slice 5

**Нет**, по действующему frozen контракту Slice 4 — reader qualification,
а не «появилась ещё одна кнопка чтения». Необходимые условия перечислены выше:
полная strategy/source dispatch матрица; own P2 prefix и продолжение; замкнутые
production dependency cases; eligible ordinary multi-page feed на двух
полноценных получателях; gate parity; затем отдельно разрешённый capability
lifecycle с отзывом. Пока unsupported ветви существуют, PUT не является способом
закрыть критерий. Writer/public-release gates от этого автоматически не меняются.

Slice 5 — полная публикация чтения всех account/project семейств и стабильный UI
в двухклиентной приёмке. **Часть его пользовательских функций уже реализована**:
Content, структура, Document и теперь project Map UI. Старое «Slice 5 вообще не
начинался» не описывает фактические функции, но формальный переход 4→5 и завершение
Slice 5 не заявляются. Технические Slice и шесть продуктовых этапов не взаимно
однозначны. Нельзя скрывать прогресс UI неизменным числом Note blockers.

## Что осталось до конца продуктового этапа 2

Авторизованный Recovery GET остаётся допустимым путём чтения. Продуктовый этап 2
не требует Web editing, browser capability PUT или выдачи ACK: это не следует
добавлять в его критерии автоматически. Важны полное безопасное чтение
поддерживаемых Native данных, новые Desktop revisions и reopen.

Осталось **7 явно перечисленных функциональных групп чтения**. Это единицы
учёта roadmap, а не 7 новых технических этапов и не оценка процента готовности.
Для каждой требуется свой обычный Native→HTTP→Vue→restart сценарий.

| № | Незавершённая функция | Наблюдаемый критерий завершения |
| --- | --- | --- |
| 1 | Оставшиеся изменения структуры | Удаление Stage через Native owner корректно убирает этап в Web; выбор и зависимые данные сохраняют действующий контракт. CREATE/rename/order уже приняты. |
| 2 | Полное представление Document | Форматирование действующего Tiptap JSON видно в Web; поздний новый Document после unlock и допустимые delete/Resolution histories применяются owner, а не блокируют проект. Текущий continuation preflight требует revision≥2, поэтому поздний genesis также остаётся конкретным пробелом. Plain text/create/linear updates обоих scopes уже приняты. |
| 3 | Оставшиеся Map histories | Stage Map create/update, annotations, Stage proof и restart теперь приняты вместе с project Map. Остаются ранее перечисленные delete/Resolution Native Map histories; группа пока не закрыта. |
| 4 | Свойства и связи Notes | Проверенные checklist/tags/annotations и Map-derived Notes доступны пользователю. Сейчас основной recovery UI показывает заголовок и body. |
| 5 | Состояние игры аккаунта | Частично принято: отдельные account-балансы/уровень/здоровье/вдохновение/навыки/коэффициенты/global streak и read-only inventory; Native buy/sell + связанные награды обновляются через HTTP, оба порядка G/R приняты. Остаются global_streak/freeze action updates, compensation/Resolution/adoption, cabinet/rhythm/баффы и другие реальные панели/механики. Игровые действия/запись Web относятся к этапам 3–4. |
| 6 | Полный облачный каталог | Canonical folders/order/membership и project order показываются через владельцев account history. Bootstrap project list не заменяет эти данные. |
| 7 | Зашифрованные обложки | Metadata2/cover references и encrypted blob проверяются, отображаются и обновляются без раскрытия ключа серверу. |

Дополнительно остаются **сквозные условия чтения**, а не новые UI функции:
совместное открытие полного mixed account history (сейчас Map/Document вместе с
Note Resolution блокируются `hasResolution`); автоматическое согласование двух
существующих проектов через пользовательский маршрут; реалистичный объём сверх
текущих 200 событий с сохранением byte/resource limits либо обоснованный и принятый
продуктовый предел. Без этих проверок нельзя объявлять «полное чтение» всего
аккаунта. Безопасные ограничения не удалять ради счётчика.

Stage Map и базовое чтение Progress/history/stats приняты. Для Progress проверены
project/Stage complete genesis + append, реальные HTTP updates, цели/история/
статистика, rollback/proofs и restart; счётчик уменьшен9→8. Partial migration и
конфликтные/correct/tombstone/select/rebase истории не объявлены поддерживаемыми:
это остаток существующей полной source/strategy матрицы и mixed-history приёмки,
а не новая функциональная группа или новый Slice.

Project Game basic route принят: Native genesis, writing + точный Progress fact,
completion + Metadata status и связанная account reward проверены через HTTP/
Chromium/restart; project streak/claim/reward UI совпадает с Native. Счётчик7
отражает этот пользовательский маршрут, а не полную Game qualification. Account
UI не принят. Обратный допустимый порядок R-before-G пока не проходит строгую
последовательную ownerPort coverage; Stage Game HTTP и advanced Game histories
также не заявлены PASS. Это конкретный остаток existing dependency/source matrix,
не новый Slice и не разрешение обходить префикс/proofs. Все игровые механики
остаются обязательной целью полного Web.

Следующая продуктовая задача: **Account Game global_streak/freeze updates** через существующий owner, затем остальные группы
чтения. Rich Document и оставшиеся строки не исключены
из этапа 2. После полного чтения следующий продуктовый этап — **3/6, создание,
редактирование и Web→Server→Desktop**, с проверенным server confirmation,
собственным echo и обоими направлениями единого sync engine. Полная WORTA Web,
обычные проекты, все игровые механики и двусторонний sync остаются конечной целью.

## Native global streak growth

Starting SHA `7ba13869`. OpenCode completed verification; Rust tests pass; parity test passes.

## 2026-11-07 — Web Stage structure update (reorder) verification

Starting HEAD `a1b2ebbe`. No production code change required; existing browser Stage
owner already supports Stage structure update (`BrowserStageOrderApply`). Real Chromium
IndexedDB acceptance passed; tombstone semantics and `guard_delete` unchanged.
Product reading groups: 4 of 9 closed, 5 remain. Note reader readiness remains 2/10.

## 2026-11-07 — Web project catalog and cover metadata read

Starting HEAD `297904b4`. Read-only project metadata now supports a non-null
cover reference in `readBrowserCloudProject`; encrypted cover blob material
remains separate. `cloudRead.spec.ts`: 18 passed. Security gates unchanged.

## 2026-11-07 — Web cloud project catalog metadata display

Starting HEAD `625933f2`. Cloud project catalog now shows authenticated project
names and cover references where available, with per-project safe failure and no
cover blob fetch. Tests: account card 2, project read 8, cloudRead 18; build pass.
Security gates unchanged.

## 2026-11-07 — Web cloud project opening from catalog

Starting HEAD `7f9239aa`. Cloud catalog selection now opens the existing
recovery reader for the selected project ID. Card 2, project read 8, build pass.
Security gates unchanged.
