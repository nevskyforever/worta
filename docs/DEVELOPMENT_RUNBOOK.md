# Запуск и сборка nfprogress

Этот документ описывает поддерживаемые варианты запуска из PyCharm и сборки
актуального Vue/Ionic-приложения поверх FastAPI. PySide6 больше не входит в
release-сборки и не поддерживается как пользовательская версия.

## Откуда запускать команды

Все команды ниже предполагают, что текущая папка терминала — **корень
репозитория**, то есть папка, где одновременно находятся `frontend/`,
`backend/` и `scripts/`. Если терминал открыт, например, в `docs/`, сначала
выполните:

```bash
cd "$(git rev-parse --show-toplevel)"
```

Проверка должна вывести путь, содержащий `frontend/package-lock.json` и
`scripts/`:

```bash
pwd
```

## Как устроено приложение

```text
Desktop: Vue/Ionic → Tauri/Rust → SQLite + filesystem
Web:     Vue/Ionic → FastAPI → services → Python Core
Mobile:  Vue/Ionic → Capacitor → remote HTTPS API
```

Новый desktop frontend не читает `.pkl` и не запускает Python: расчёты,
SQLite-операции и файловые границы выполняются native Rust-сервисами. Web и
мобильные клиенты подключаются к отдельно развёрнутому FastAPI по HTTP(S).
Python остаётся для Web, миграции/восстановления и oracle-тестов.

## Подготовка PyCharm

1. Откройте корень репозитория в PyCharm.
2. Создайте interpreter на Python 3.13 в `.venv`.
3. Установите Python-зависимости:

   ```bash
   python3 -m pip install -r requirements-backend.txt
   ```

4. Для Vue/Ionic установите Node.js 20.19 или новее, затем один раз выполните:

   ```bash
   cd frontend
   npm ci
   ```

## Исторический PySide6-код

`main_UI.py` сохранён только как источник для регрессионного сравнения и
совместимости старых `.pkl`. Он не собирается в Actions, не публикуется и не
считается поддерживаемым способом запуска. Для desktop-разработки используйте
Tauri ниже.

## Текст Desktop-релиза

Описание GitHub Release, описание обновления в манифесте и пост в Telegram
публикуются в одном формате:

```text
Обновление 5.3.12

Понятное для пользователя описание изменений.
```

В заголовке указывается фактическая версия выпуска. Основной текст кратко
описывает изменения понятным пользователю языком. Workflow добавляет заголовок,
если передан только текст описания; если переданный заголовок указывает другую
версию, публикация завершается ошибкой. Отдельный текст для Telegram также
нормализуется к этому формату. При повторном запуске уже опубликованное
сообщение не отправляется повторно.

## Новый Web-интерфейс

Для Web нужны два одновременно работающих процесса: FastAPI и Vite. В
обычном режиме разработки используйте тот же набор данных, что и
исторический Python UI: backend перед стартом копирует актуальные проекты,
документы, настройки и игровое состояние во временный Python-совместимый
профиль. Canonical `nfprogress/test_data` с SQLite-владельцем сохраняется для
Tauri и не используется Web-адаптером.

### 1. FastAPI

В PyCharm создайте конфигурацию **Python** с запуском по имени модуля:

- **Module name:** `backend.app`
- **Parameters:**

  ```text
  --host 127.0.0.1 --port 8000 --platform web --dev-data
  ```

- **Working directory:** `$PROJECT_DIR$`
- **Python interpreter:** `.venv`

`--dev-data` создаёт временный профиль и копирует в него актуальные legacy-
источники. Исходные рабочие данные и canonical Tauri-профиль не
перезаписываются. Для полностью пустых изолированных тестов по-прежнему можно
указать `--data-dir` вместо `--dev-data`. После запуска API доступен по адресам:

- `http://127.0.0.1:8000/health`
- `http://127.0.0.1:8000/docs`

### 2. Vue/Ionic

Создайте конфигурацию типа **npm**:

- **package.json:** `$PROJECT_DIR$/frontend/package.json`
- **Script:** `dev`

Или выполните во встроенном терминале PyCharm:

```bash
cd frontend
npm run dev
```

Откройте `http://127.0.0.1:5173`. Vite перенаправляет `/api` и `/health` на
FastAPI по адресу `127.0.0.1:8000`.

Сначала запускайте FastAPI, затем Vite. Не запускайте одновременно
`main_UI.py` и backend, если оба используют одну и ту же папку данных.

### Ошибка «API вернул ошибку 502»

Она означает, что Vite работает, но не может достучаться до FastAPI на
`127.0.0.1:8000`. Проверьте API напрямую:

```bash
curl http://127.0.0.1:8000/health
```

Ожидаемый ответ содержит `"status":"ok"`. Если соединение отклонено,
запустите backend из корня репозитория:

```bash
python3 -m backend.app --host 127.0.0.1 --port 8000 --platform web --dev-data
```

В PyCharm backend нужно запускать именно как модуль `backend.app`, а не как
файл `backend/app/main.py`: последний создаёт FastAPI-приложение, но сам
Uvicorn-сервер не запускает. В активированном виртуальном окружении допустима
команда `python`, но в текущей macOS-среде доступна команда `python3`.

Для запуска Web без двух отдельных терминалов используйте из корня:

```bash
bash "Run Web.sh"
```

Скрипт запускает FastAPI с `--dev-data`, ждёт `/health`, затем запускает Vite.
При остановке Vite дочерний backend также завершается. Проверка зависимостей без
запуска выполняется командой `bash "Run Web.sh" --check`.

Vite автоматически подхватывает изменения Vue и TypeScript, но запущенный
Python backend сам не перезапускается. После изменений в `backend/`,
`nfprogress/` или `engine.py` полностью остановите `Run Web.sh` сочетанием
`Ctrl+C` и запустите его снова. Иначе браузер продолжит обращаться к старому
FastAPI-процессу, даже если новый интерфейс уже появился после hot reload.

## Проверки и Web-сборка

Python-проверки из корня репозитория:

```bash
python3 -m pytest -q
```

Frontend-проверки и production-сборка из `frontend/`:

```bash
npm run typecheck
npm run lint
npm run test
npm run build
```

Для browser-проверок установлен Playwright. Один раз на каждой машине загрузите
управляемый Chromium, затем запускайте будущие browser-сценарии из того же
каталога:

```bash
npx playwright install chromium
npx playwright test
```

`npm run test` проверяет Vue-компоненты, а Playwright предназначен для
сквозных сценариев в реальном Chromium. Перед такими сценариями должен быть
запущен FastAPI backend с изолированными developer-данными.

Готовый Web-frontend появится в `frontend/dist`. Для размещения в интернете
нужны отдельный FastAPI-сервер, HTTPS и настройка хоста, который отдаёт
`index.html` для маршрутов Vue Router.

## Новый desktop-вариант Tauri

Нужны только Rust/Cargo и Node.js:

```bash
cd "$(git rev-parse --show-toplevel)"
cd frontend
npm ci
NFPROGRESS_DATA_DIR=/absolute/path/to/nfprogress-profile npm run tauri:dev
```

`tauri:dev` самостоятельно запускает Tauri-окно и native SQLite/runtime.
Вручную запускать FastAPI для desktop не нужно. Для нового изолированного
root используйте `bash "Run Tauri.sh"` ниже.

Для обычного локального запуска нового desktop-приложения без production-пакета
используйте из корня репозитория:

```bash
bash "Run Tauri.sh"
```

Скрипт выбирает Rust architecture текущего Mac и запускает native runtime с
persistent-профилем `~/Documents/nfprogress/test_data`. Если профиль ещё не
создан или не прошёл проверку текущей SQLite schema, он один раз инициализируется из real
profile через Python migration pipeline. Существующий готовый test profile не
перезаписывается: его проекты, тексты, принятые Word hashes и игровые изменения
переживают повторный запуск. Путь к root печатается перед запуском. Для нового
пустого изолированного профиля используйте явный режим:

```bash
bash "Run Tauri.sh" --fresh
```

`--legacy` остаётся compatibility alias persistent test-data profile и не
обновляет его скрыто. Для намеренной замены test profile актуальным снимком
real profile используйте явный режим:

```bash
bash "Run Tauri.sh" --refresh-test-data
```

Перед заменой migration pipeline создаёт backup, проверяет staging-базу и
активирует её атомарно. Те же операции доступны в модальном окне «Режим
разработчика»: запрос real → test или test → real выполняется только после
подтверждения и на следующем старте, до открытия active SQLite. При test → real
developer clock и test-only settings не переносятся.

Произвольный профиль задаётся явно через `--data-dir PATH` или
`NFPROGRESS_DATA_DIR=/absolute/path`. Production root при этом не меняется.

Production `.app`, DMG и ZIP при этом не создаются.
Проверить prerequisites без открытия окна можно так:

```bash
bash "Run Tauri.sh" --check
```

Перед запуском остановите отдельный npm run dev, если он уже занимает порт
5173. Первый Tauri dev-start может скомпилировать debug Rust-код, но не создаёт
production .app, DMG или ZIP. Чтобы debug-артефакты Cargo не накапливались в
репозитории, скрипт отключает incremental compilation и debug symbols для
обычного запуска, а Cargo target хранит в macOS-кэше
`~/Library/Caches/nfprogress/`. Перед стартом кэш и старый
`frontend/src-tauri/target` очищаются при размере больше 2 GiB. Принудительно
очистить эти артефакты, включая target в старых
`.tauri-build-workspaces/`, можно так:

```bash
bash "Run Tauri.sh" --clean
```

Порог изменяется через `NFPROGRESS_TAURI_TARGET_MAX_MB`. Удаляются только
сгенерированные Cargo-артефакты; исходники, данные проектов и пакеты сборки не
затрагиваются.

Если Web в браузере сообщает об ошибке API, это относится к отдельному Web
режиму и его FastAPI-процессу. Native Tauri не использует локальный FastAPI,
sidecar или session token; проверяйте desktop в открывшемся окне nfprogress.

Production-сборка:

```bash
cd frontend
npm run tauri:build
```

Для локального test/qualification macOS-архива доступны ARM/Intel/All-скрипты.
Они собирают release-компилированный app с
`NFPROGRESS_BUILD_PROFILE=test`, поэтому Finder launch использует canonical
`~/Documents/nfprogress/test_data`:

```bash
bash "Build Tauri ARM.sh"
bash "Build Tauri Intel.sh"
bash "Build Tauri All.sh"
```

Перед test-сборкой canonical `test_data` только инициализируется, если ещё не
готов. Сборка не уничтожает накопленные изменения persistent test profile.
Для явного обновления сначала выполните `Run Tauri.sh --refresh-test-data`, а
для намеренно пустого изолированного запуска используйте `Run Tauri.sh --fresh`.

`Build Tauri All.sh` параллельно выполняет ARM- и Intel test-сборку.
Initial release macOS — только Apple Silicon: `Release Tauri ARM.sh` является
основной точкой релиза, а `Release Tauri All.sh` по умолчанию запускает только
ARM. Intel остаётся доступен явно через `Release Tauri All.sh intel` или
`NFPROGRESS_TAURI_INCLUDE_INTEL=1` для отложенной квалификации.
Для каждой архитектуры создаётся отдельный игнорируемый frontend-workspace в
`.tauri-build-workspaces/`: в нём свои `node_modules`, Vite-вывод и Tauri target.
Обычные `Build Tauri` test-сборки удаляют workspace Cargo target после упаковки;
release qualification сохраняет его явно для проверки `.app`. Переменную
`NFPROGRESS_TAURI_KEEP_BUILD_CACHE=1` задавайте только если этот кэш нужно
оставить.

Перед сборкой нормализованная версия из `engine.py` синхронизируется с
`tauri.conf.json`, `Cargo.toml` и `Cargo.lock` внутри соответствующего
workspace, не затрагивая исходный frontend.

SQLite хранит версию nfprogress отдельно от `schema_version` в таблице
`application_metadata`. `data_created_by_version` устанавливается для новой
БД и больше не меняется; у обновлённой старой БД без исторической версии оно
остаётся `NULL`. `data_last_written_by_version` меняется на текущую версию
после успешной записи пользовательских данных. Другая сохранённая версия пока
носит только информационный характер и не включает compatibility policy.

Они собирают native Tauri `.app`, проверенный DMG и ZIP с DMG, лицензией и
сведениями об исходном коде. Результаты лежат в
`build-tauri-arm/` или `build-tauri-intel/`. На Apple Silicon Intel-скрипт
использует native Rust target без отдельного Python-окружения.

Скрипты `Release Tauri ARM.sh`, `Release Tauri Intel.sh` и
`Release Tauri All.sh` разделяют build и qualification: проверяют frontend
typecheck/tests,
Rust, архитектуру app, отсутствие Python/FastAPI/Nuitka/backend sidecar,
provenance и SHA-256, но ничего не публикуют по умолчанию. Старый hosting
handoff можно включить только явно через
`NFPROGRESS_TAURI_RELEASE_UPLOAD=1`; это переходный legacy-путь, а не Tauri
updater.

На macOS для обычного DMG без Finder/AppleScript-оформления используйте:

```bash
scripts/build-tauri-dmg.sh aarch64-apple-darwin
```

## Официальная Windows-сборка и обновления

`.github/workflows/build.yml` собирает только новую native Tauri-версию на
`x86_64-pc-windows-msvc`: executable `nfprogress-desktop.exe`, current-user
NSIS `*-setup.exe`, Tauri updater artifact с обязательной отдельной подписью,
`latest.json`, SHA-256 и commit provenance. Build job загружает Actions artifact;
GitHub Release запускается только явным `workflow_dispatch` с
`publish_production=true`. Authenticode включается отдельно через
`sign_windows=true` и fail-closed при отсутствии credentials.

Перед первым запуском workflow настройте GitHub Actions:

- secrets `TAURI_SIGNING_PRIVATE_KEY` и, если задан,
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`;
- repository variable `TAURI_UPDATER_PUBLIC_KEY`.

Пара updater-ключей создаётся один раз командой из `frontend/`:

```powershell
npm run tauri signer generate -- -w "$env:USERPROFILE\.tauri\nfprogress-updater.key"
```

Это бесплатная пара ключей Tauri, а не сертификат Authenticode. Приватный ключ
нельзя коммитить или терять. Release-приложение проверяет
`latest.json` в GitHub Releases после запуска и раз в час; найденное обновление
устанавливается штатным Tauri updater только после проверки подписи. В
`tauri:dev` updater намеренно выключен.

`update_manifest.json` и `update_manifest_legacy.json` не являются normal Tauri
updater manifest. Они сохраняются только для отдельного legacy handoff Python
5.x → Tauri 6.x; обычный Tauri updater использует GitHub `latest.json` и его
криптографическую подпись. Standalone migration helper распространяется рядом
с transition installer только когда это отдельно квалифицировано и не входит
в normal Tauri app/runtime.

## iOS и Android

Capacitor не запускает Python внутри приложения: ему нужен опубликованный
FastAPI по HTTPS. Сначала задайте URL backend и синхронизируйте Web-ресурсы:

```bash
cd frontend
VITE_API_BASE_URL=https://api.example.com npm run cap:sync
```

Затем открывайте `frontend/ios` в Xcode либо `frontend/android` в Android
Studio. Для нативной сборки нужны полный Xcode с iPhoneOS SDK либо поддерживаемые
JDK и Android SDK соответственно.

## Полезные адреса и документы

- API documentation: `http://127.0.0.1:8000/docs`.
- Vue development server: `http://127.0.0.1:5173`.
- Архитектура: `docs/frontend-migration/ARCHITECTURE.md`.
- Текущий статус и platform-specific ограничения:
  `docs/frontend-migration/MIGRATION_STATUS.md`.
