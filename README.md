# nfprogress

nfprogress is a progress tracker for writers. It organizes writing projects,
stages, deadlines, notes, statistics, and optional game mechanics that turn
regular writing into challenges and rewards.

The current application uses Vue 3, TypeScript, Ionic, FastAPI, Tauri 2, and
Capacitor. PySide6 source remains in the repository only as a compatibility
reference for existing data and behavior; legacy packages are no longer built,
published, or supported.

## Architecture

```text
Desktop: Vue 3 + Ionic ── Tauri 2/Rust ── SQLite + filesystem
Web:     Vue 3 + Ionic ── FastAPI/Python ── server storage
Mobile:  Vue 3 + Ionic ── Capacitor ── remote HTTPS API
```

Web and legacy tooling retain the Python business rules. `Project`, `Stage`, `Gamer`,
and `Buff` retain their legacy module paths so existing pickle data stays
loadable, while the shared core and services import without Qt. The Vue
frontend never reads pickle files or imports Qt/Python objects directly. See the detailed
[architecture](docs/frontend-migration/ARCHITECTURE.md),
[migration plan](docs/frontend-migration/MIGRATION_PLAN.md), and
[live status](docs/frontend-migration/MIGRATION_STATUS.md).

## Features

- Track multiple projects, stages, states, goals, deadlines, and progress.
- Record characters, A4 pages, author's sheets, Ficbook pages, and the other
  units supported by the Python core.
- Review project statistics, productive days, and writing streaks.
- Create a 1080 × 1080 progress card for a finite project or stage; its share
  menu has separate Copy-to-clipboard and Save-PNG actions.
- Organize project-note cards and synchronize `#карта` notes with Mind Elixir.
- Count and apply progress from Word `.docx` documents and Scrivener projects.
- Use optional game mechanics: XP, levels, coins, inspiration, health, quests,
  items, buffs, writing sessions, challenges, specializations, cabinet relics,
  custom awards, bank deposits, and streak freezes.
- Use Russian, English, Spanish, German, French, or Brazilian Portuguese.
- Select light, dark, or system theme in the new frontend.
- Configure the writing-day boundary, notification duration, and optional
  all-project daily total; Vue remembers its own project-list and inventory
  views without changing historical desktop preference keys.
- Review persistent bank and streak events in the cross-platform notification
  center and mark them read without losing the legacy event history.

## Target status

| Target | Current state |
| --- | --- |
| Web | Production Vue build works with a configured FastAPI deployment; HTTPS, authentication/reverse-proxy policy, and SPA route fallback belong to the deployment |
| Tauri macOS Apple Silicon | Native Rust/Tauri app and ARM bundle path; production qualification remains F8 |
| Tauri macOS Intel | Native Rust/Tauri Intel target path is preserved; physical Intel UI/signing verification remains a release-host task |
| Tauri Windows | Native Rust/Tauri NSIS installer path; production qualification remains F8 |
| Capacitor iOS | Native project, plugins, branding, and `cap sync` are present; native compilation is blocked on this host because full Xcode and the iPhoneOS SDK are absent |
| Capacitor Android | Native project, plugins, branding, and `cap sync` are present; native compilation is blocked on this host by Java 8 and the absence of the Android SDK (`adb`/`sdkmanager`) |

The Capacitor applications do not embed CPython. Web, iOS, and Android require
a separately deployed FastAPI server. See
[MIGRATION_STATUS.md](docs/frontend-migration/MIGRATION_STATUS.md) for the
feature-level parity matrix and remaining platform-specific behavior.

## Download

Tauri desktop builds are published through
[GitHub Releases](https://github.com/nevskyforever/worta/releases).
Windows releases use the bundled NSIS installer; automatic updates are delivered
from the same GitHub release channel.

## Development prerequisites

- Python 3.13 and `requirements-backend.txt` for Web, migration and oracle work.
- Node.js 20.19 or newer for `frontend/`.
- Rust and Cargo for Tauri.
- Full Xcode with the iPhoneOS SDK for an iOS build.
- A supported JDK and Android SDK for an Android build.
- Windows with the MSVC toolchain for the Tauri bundle.

Clone the repository and create a Python environment:

```bash
git clone https://github.com/nevskyforever/worta.git
cd worta
python3 -m venv .venv
source .venv/bin/activate
python -m pip install -r requirements-backend.txt
```

On Windows, activate it with PowerShell:

```powershell
py -3.13 -m venv .venv
.venv\Scripts\Activate.ps1
python -m pip install -r requirements-backend.txt
```

## Historical PySide6 source

`main_UI.py` and the old updater remain for regression comparison and pickle
compatibility only. They are not part of the release workflow and are not a
supported application target. Use Web or Tauri for development and releases.

## Run FastAPI and the Vue frontend

Start the API from the repository root:

```bash
python -m backend.app --host 127.0.0.1 --port 8000 --platform web --dev-data
```

`/health` reports readiness and `/docs` exposes OpenAPI. The `--dev-data` flag
copies the current real projects, documents, settings and game state into a
temporary Python-compatible Web profile before starting. The canonical
SQLite-authoritative `test_data` profile remains reserved for Tauri. To isolate
a test completely, replace it with `--data-dir /absolute/path/to/test-data`.

In a second terminal:

```bash
cd frontend
npm ci
npm run dev
```

For a one-command local Web run from the repository root, use:

```bash
bash "Run Web.sh"
```

It starts the Python backend with `--dev-data`, waits for `/health`, and then
starts Vite. Stopping Vite also stops that backend.

Vite listens on `127.0.0.1:5173` and proxies `/api` and `/health` to the local
backend. For a remote Web or Capacitor build, set the public API origin before
building:

```bash
cd frontend
VITE_API_BASE_URL=https://api.example.com npm run build
```

`VITE_API_BASE_URL` is public bundle configuration, not a place for secrets.
Configure `NFPROGRESS_ALLOWED_ORIGINS` on the server for the deployed client.
A non-loopback backend bind additionally requires `--allow-remote`; that mode
is intentionally unauthenticated and must sit behind an external HTTPS and
authentication layer. A Web host must route unknown application paths to
`index.html` so direct Vue Router URLs work.

Useful frontend commands:

```bash
cd frontend
npm run typecheck
npm run lint
npm run test
npm run build
npm run preview
```

## Export localization and help content

Russian strings and `HELP_SECTIONS` remain canonical in Python. Regenerate the
deterministic frontend artifacts from the repository root:

```bash
python scripts/export_frontend_content.py
python scripts/export_frontend_content.py --check
```

When canonical source strings change, follow the localization workflow in
`AGENTS.md` and regenerate `translations_catalog.py` first.

## Tauri desktop

The packaged desktop app is native Rust/Tauri. It opens and migrates SQLite
directly, uses native filesystem services, and does not start Python, FastAPI,
or a localhost sidecar.

```bash
cd frontend
npm ci
cd ..
bash "Run Tauri.sh"
```

The repository script refreshes the canonical `~/Documents/nfprogress/test_data`
profile through the Python migration pipeline before starting Tauri. The root
is printed before Tauri starts. To use a new empty isolated SQLite root, pass
`--fresh`:

```bash
bash "Run Tauri.sh" --fresh
```

`--legacy` remains an explicit compatibility alias for the canonical profile;
the refresh keeps its legacy files as migration fixtures while marking the
SQLite database ready for Tauri.

For a specific profile, pass an explicit directory (the Rust runtime also
accepts `NFPROGRESS_DATA_DIR` directly):

```bash
bash "Run Tauri.sh" --data-dir /absolute/path/to/nfprogress-profile
NFPROGRESS_DATA_DIR=/absolute/path/to/nfprogress-profile bash "Run Tauri.sh"
```

The script selects the host target and starts the native application. Use
`bash "Run Tauri.sh" --check` to validate prerequisites without opening a
window. `npm run tauri:dev` remains available for advanced use, but should be
paired with an explicit `NFPROGRESS_DATA_DIR` when a non-canonical profile is
needed. The script disables Cargo incremental compilation and debug symbols for
the normal dev run. Its Cargo target is kept in the macOS cache at
`~/Library/Caches/nfprogress/` and is automatically cleaned before launch when
it exceeds 2 GiB; an old `frontend/src-tauri/target` is handled the same way.
`--clean` also removes Cargo targets inside existing
`.tauri-build-workspaces/` directories. To clean those generated artifacts
manually, use:

```bash
bash "Run Tauri.sh" --clean
```

The limit can be changed with `NFPROGRESS_TAURI_TARGET_MAX_MB`; only generated
Cargo artifacts are affected, never source code or project data.
Stop a separately running npm run dev first, because Tauri dev uses port 5173.

Checks and a production bundle:

```bash
cd frontend/src-tauri
cargo check
cd ..
npm run tauri:build
```

`Build Tauri ARM.sh`, `Build Tauri Intel.sh`, and `Build Tauri All.sh`
provide local test/qualification build entry points. They compile a release
native Tauri app with `NFPROGRESS_BUILD_PROFILE=test`, use
`~/Documents/nfprogress/test_data` when opened from Finder, and produce a
test-prefixed verified DMG plus a local ZIP containing the DMG,
license, and source-code notice:

```bash
bash "Build Tauri ARM.sh"
bash "Build Tauri Intel.sh"
bash "Build Tauri All.sh"
```

Before a test build, the current real application data is refreshed into the
canonical `test_data` profile through the migration pipeline. The test `.app`
therefore starts with the latest projects, texts, settings, and game state;
`--fresh` remains available for an intentionally empty isolated Tauri run.

`Build Tauri All.sh` remains a parallel ARM/Intel test build. The initial
release policy is ARM64 only: `Release Tauri ARM.sh` is the primary macOS
release entry point, and `Release Tauri All.sh` runs only that target by default.
Use `Release Tauri All.sh intel` (or set
`NFPROGRESS_TAURI_INCLUDE_INTEL=1`) for explicit Intel qualification. Each
architecture uses its own ignored frontend workspace under
`.tauri-build-workspaces/`, including `node_modules`, Vite output and Tauri
target files. Ordinary `Build Tauri` test builds remove the workspace Cargo
target after packaging; release qualification retains it explicitly for app
inspection. Set `NFPROGRESS_TAURI_KEEP_BUILD_CACHE=1` only when retaining that
cache is intentional.

The build workspace synchronizes the normalized three-component version from
`engine.py` into the Tauri and Cargo metadata before building, without racing
over the source frontend files.

The artifacts are written to `build-tauri-arm/` and `build-tauri-intel/`.
Release scripts perform build and qualification only by default: they run
frontend typecheck/tests, Rust fmt/check/tests, verify the app architecture and
reject Python/FastAPI/Nuitka/backend payloads, then print sizes, SHA-256,
provenance, signing status and updater status. Set
`NFPROGRESS_TAURI_RELEASE_UPLOAD=1` only for the separately maintained legacy
hosting handoff; it is not the Tauri updater and is never implicit.
Release scripts force `NFPROGRESS_BUILD_PROFILE=production` and retain the
production artifact names. The lower-level `scripts/build-tauri-local.sh` is
production-default unless this variable is set explicitly.

### Windows release and automatic updates

`.github/workflows/build.yml` is the supported Windows release path. It targets
`x86_64-pc-windows-msvc`, runs frontend and Rust checks, builds only the native
Tauri executable plus current-user NSIS `*-setup.exe`, audits the package for
legacy runtime files, records commit provenance/SHA-256, and creates the Tauri
updater artifact and signed `latest.json` contract. The GitHub Release job is
protected behind an explicit `workflow_dispatch` with `publish_production=true`;
ordinary builds upload Actions artifacts only.

Configure these GitHub Actions secrets before the first release:

- `TAURI_SIGNING_PRIVATE_KEY`: the private updater key generated once with
  `npm run tauri signer generate -- -w
  "$env:USERPROFILE\\.tauri\\nfprogress-updater.key"` from PowerShell;
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: updater-key password, when set.

`sign_windows=true` is a separate optional dispatch input for fail-closed
Authenticode. Unsigned initial distribution is allowed by policy; missing Tauri
updater signing credentials are not allowed when the updater is enabled.

Set `TAURI_UPDATER_PUBLIC_KEY` as a repository variable (a secret with the same
name is also accepted). This is a free Tauri update key, not an Authenticode
publisher certificate. Keep its private half backed up securely: installed
clients cannot trust releases signed by a replacement key.

Official release builds check GitHub Releases after startup and once per hour.
When a version is available, the app shows its release notes and installs it
through Tauri after cryptographic verification. Local `tauri:dev` and ordinary
unsigned local bundles do not enable this channel.

`update_manifest_legacy.json` and the legacy flat `update_manifest.json` remain
only for the explicit Python 5.x → Tauri 6.x transition boundary. They are not
normal Tauri updater manifests, and this workflow does not generate or publish
them. The standalone migration helper is delivered separately when a transition
release needs it; it is never packed into the normal Tauri app or installer.

`npm run tauri:build` also creates a Finder-styled DMG. In a headless macOS
environment, use `npx tauri build --bundles app` to verify the production app
bundle without requiring Finder AppleScript automation. To produce a normal
drag-to-Applications DMG without Finder styling, use:

```bash
scripts/build-tauri-dmg.sh aarch64-apple-darwin
scripts/build-tauri-dmg.sh x86_64-apple-darwin
```

The script builds the matching native `.app`, then creates and verifies a UDZO
DMG with `hdiutil`; it never invokes `osascript`.

The Windows target `x86_64-pc-windows-msvc` must be built on Windows. Intel
macOS uses the existing Rust target without a Python compatibility environment.

## Capacitor iOS and Android

Set `VITE_API_BASE_URL` to the remote HTTPS API and synchronize the generated
web output with both native projects:

```bash
cd frontend
npm ci
VITE_API_BASE_URL=https://api.example.com npm run cap:sync
```

Open the platform project when its SDK is installed:

```bash
npm run cap:open:ios
npm run cap:open:android
```

An Android debug build can then be run from `frontend/android` with
`./gradlew assembleDebug` (`gradlew.bat assembleDebug` on Windows). iOS builds
are run from the generated Xcode workspace. `cap sync` does not prove that a
native SDK build succeeds.

## Word and Scrivener workflows

- Tauri desktop may select local `.docx` files and Scrivener projects, inspect
  the Scrivener binder hierarchy, configure project/stage synchronization, and
  run it manually.
- When desktop background synchronization is enabled, native Rust performs
  configured file reads outside the API event loop on enable/start and when
  the effective writing day changes. It checks the setting once per minute and
  uses the existing one-minute schedule.
- Web, iOS, and Android cannot obtain arbitrary filesystem or background
  access. They can explicitly upload a selected `.docx`; the server counts it
  and applies the total through normal progress and reward rules.
- Scrivener project upload is not supported by remote clients. The shared
  integration accepts `.docx` only and asks the user to save the obsolete
  `.doc` format as `.docx`.

## Data safety

- SQLite is authoritative for migrated desktop domains. `data.pkl`,
  `settings.pkl`, `gamer.pkl` and `documents.json` remain untouched recovery
  artifacts and are not desktop runtime sources.
- Tests and smoke checks use explicit temporary data directories; they must not
  migrate real user files.
- No automatic JSON/SQLite replacement or destructive pickle migration occurs.
- Keep backups before testing migration builds with valuable data.

## Tests

Run the Python suite from the repository root:

```bash
python -m pytest -q
```

The focused migration checks can be run independently:

```bash
python -m pytest -q \
  tests/test_core_storage.py \
  tests/test_project_service.py \
  tests/test_core_notes_service.py \
  tests/test_core_game_service.py \
  tests/test_document_integrations.py \
  tests/test_api.py \
  tests/test_content_services.py \
  tests/test_frontend_content_export.py
```

Run frontend gates from `frontend/`:

```bash
npm run typecheck
npm run lint
npm run test
npm run build
```

Native checks additionally require the matching platform SDK. Current blockers
and the latest verified matrix are recorded in
[MIGRATION_STATUS.md](docs/frontend-migration/MIGRATION_STATUS.md).

## Uninstallation

### Windows

Open **Settings > Apps > Installed apps**, find **nfprogress**, and select
**Uninstall**. The Tauri NSIS application is installed for the current user.

Uninstalling the application does not remove user data. It is stored in
`%APPDATA%\nfprogress`. Data created by older versions may also remain in
`%USERPROFILE%\Documents\nfprogress`. Delete these directories manually only
if you no longer need your projects, settings, and progress history.

### macOS

Move `nfprogress.app` from the Applications folder to the Trash. User data in
`~/Documents/nfprogress` is not removed automatically; delete that directory
manually only if you no longer need it.

### Project roles

- Committer: nevskyforever
- Reviewer: nevskyforever
- Approver: nevskyforever

### Privacy policy

This program will not transfer any information to other networked systems
unless specifically requested by the user or the person installing or
operating it.

## License

nfprogress is free and open-source software licensed under the
[GNU General Public License version 3](LICENSE).
