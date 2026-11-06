# WORTA 6.0 — ревизия чтения Web и reader qualification, 06.11.2026

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
