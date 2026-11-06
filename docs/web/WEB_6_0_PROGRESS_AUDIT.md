# WORTA 6.0 — ревизия чтения Web и reader qualification, 06.11.2026

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
