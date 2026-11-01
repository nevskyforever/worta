## About the project

This is a Python desktop application with a PySide6 GUI for writers to track their writing progress, with optional gamification features.

> **Current UI:** the user-facing interface is implemented in Vue (`frontend/`).
> The PySide6 interface is legacy and is not used by the application. Do not add
> or change PySide6 UI behavior unless the user explicitly requests legacy-interface work.

The application is built with Nuitka for:

* Windows;
* macOS Apple Silicon;
* macOS Intel.

## Product name and release process

* The official product name is `worta`, always lowercase in user-facing names,
  window titles, installers, releases, and other product references.
* Existing internal identifiers may be retained when changing them would break
  compatibility with user data or updates.
* Build macOS Apple Silicon and Intel releases on the owner’s Mac using the
  existing release scripts; build Windows x64 with the existing GitHub Actions workflow.
* Use the existing signing, update, and publication mechanisms. Do not create
  new release mechanisms without separate approval.
* GitHub Releases is the primary source of published installers. Update
  `worta.ru` using its existing process.

## General working rules

* Perform only the task specified by the user.
* Do not modify unrelated parts of the project.
* Do not perform a full refactor without an explicit request.
* Before changing code, find related files and usage sites.
* Use `rg` to search the project.
* Do not inspect the entire project if the task concerns a single module.
* Preserve the existing architecture and style.
* Do not add dependencies unless necessary.
* Do not change public application behavior without an explicit request.
* Test data may be created, modified, and removed when needed to reproduce or
  verify a reported issue. Keep it isolated from production user data.
* After completing each user task, create a clear commit whose subject summarizes
  the completed change and whose body explains the important behavior and checks.

## Working with files

* First identify the smallest set of files needed for the task.
* Do not open large files in full when locating the relevant function or class is sufficient.
* Do not modify build files, dependencies, or configuration unless necessary.
* Do not modify generated files.
* Do not delete code until you have verified that it is unused.

## Python

* Use features available in the project's current Python version.
* Add type annotations where they improve clarity.
* Do not suppress exceptions with an empty `except`.
* Do not use global variables unless necessary.
* Keep function, class, and variable names clear.
* Do not complicate the solution with unnecessary abstractions.

## PySide6

* Do not run long operations in the GUI main thread.
* Do not block the UI with network requests, waiting, or expensive computations.
* Follow the existing signal and slot system.
* Do not change the UI structure without an explicit request.
* Account for Windows and macOS compatibility.
* After editing `UI template/main_window.ui`, restore the form's saved tab state
  before regenerating `UI_fiiles/main_window.py`: the main window must open on
  `Проекты`, and every nested tab group in the game mode must open on its first
  tab. Verify that the corresponding generated `setCurrentIndex()` calls use `0`,
  unless the task explicitly requires a different default.

## Localization

Russian is the source language for the interface and keys. Supported languages are Russian, English, Spanish, German, French, and Brazilian Portuguese.

When adding or changing user-facing text, always account for localization:

* Translate text from Python code through `localization.tr()`.
* Use `localization.LocalizedMessageBox` for `QMessageBox`; it translates both the title and message automatically.
* `NotificationManager.show_success()`, `show_error()`, `show_warning()`, and `show_info()` also call `tr()` automatically. Pass them the source Russian string that exists in the catalog.
* Add new permanent labels, buttons, and other UI elements to the appropriate `UI template/*.ui` file. Do not add them manually to generated `UI_fiiles/*.py` files.
* When switching language, open forms must call their `retranslateUi()`, and lists and labels populated from Python must be filled again.
* Do not translate user data: project names, stages, custom awards, files, or text entered by the user.

Examples:

```python
label.setText(tr("Настройки"))
label.setText(tr(f"⭐ Уровень: {level}"))
notifications.show_success("Изменения сохранены")
QMessageBox.warning(parent, "Ошибка", "Сначала выберите проект")
```

Translate a dynamic string by components when it embeds another translatable object. Translating an entire template preserves a dynamic value as-is, so the following code can leave a Russian item name inside translated text:

```python
# Incorrect for an embedded item: item.name remains Russian.
tr(f"Куплено: {item.name}")

# Correct:
f"{tr('Куплено')}: {localized_game_name(item.name)}"
```

For game-related code, follow these rules:

* Do not translate or change internal item names, buffs, categories, statuses, or dictionary keys in `game_data.py`, save files, or `Qt.UserRole`. Game logic continues to use canonical Russian values.
* Use `game_UI.localized_game_name()` for displayed names of built-in items and buffs. It handles an emoji prepended to the name separately.
* Use `game_UI.localized_game_description()` for dynamic buff descriptions.
* Use `game_UI.localized_item_result()` for an item-use result.
* Translate a displayed category separately with `tr(category)`, but retain the source category in list item data.

For example:

```python
display_text = (
    f"{localized_game_name(item.name)} x{count} [{tr(category)}]"
)
list_item.setText(display_text)
list_item.setData(Qt.ItemDataRole.UserRole, (category, item_key))
```

`translations_catalog.py` is generated automatically and must not be edited manually. If new Russian strings are added:

1. Ensure the strings are in `UI template/*.ui` or in a Python file listed in `PYTHON_SOURCES` in `scripts/generate_translations.py`.
2. If you add a Python module containing user-facing strings, add only that module to `PYTHON_SOURCES`.
3. Run `python3 scripts/generate_translations.py`. The script needs internet access and regenerates the entire catalog, so review the diff carefully and do not accept unrelated translation regressions.
4. Add manual terminology fixes and short translations that the generator cannot reliably derive from a dynamic string to `TRANSLATION_OVERRIDES` in `localization.py` for all five non-Russian languages.

After localization changes, run at least:

```bash
python3 -m py_compile localization.py <changed Python files>
python3 -m pytest -q tests/test_localization.py
```

Verify that new text is translated into all five languages, dynamic values are preserved, and internal game-object keys remain unchanged. The user agreement is intentionally displayed in a single English version for every non-Russian language.

## Maintaining in-application help

The in-application guide is defined by `HELP_SECTIONS` in `help_content.py` and is
opened from the Help menu in `main_UI.py`.

* Whenever a user-facing feature, workflow, restriction, reward, shortcut, setting,
  or calculation changes, review the related help section in the same task and keep
  it consistent with the implemented behavior.
* Add a new help section when a new feature cannot be clearly covered by an existing
  one. Remove or rewrite obsolete instructions when behavior is removed or renamed.
* Keep section keys stable and canonical. Write titles and HTML content in Russian,
  preserve valid HTML structure, and do not put user data into help text.
* The in-window help search indexes localized titles and article text automatically.
  Keep topic titles specific enough to return useful results.
* Native macOS Help-menu search is provided by the `NSUserInterfaceItemSearching`
  bridge in `macos_help_search.py`. Keep its index synchronized with `HELP_SECTIONS`
  and the selected language. Do not replace it with ordinary topic `QAction` objects
  or a separate "Search Help" menu action; those do not supply custom topics to the
  system Help search field.
* Add permanent help-window controls only to `UI template/help_dialog.ui`, then
  regenerate `UI_fiiles/help_dialog.py`. Changes to Help-menu controls belong in
  `UI template/main_window.ui`, followed by regeneration of
  `UI_fiiles/main_window.py`.
* After help changes, regenerate `translations_catalog.py`, verify every supported
  language, and run `tests/test_help_dialog.py` together with
  `tests/test_localization.py`.

## Building

* Do not run a full Nuitka build unless necessary.
* Do not run builds for all platforms simultaneously unless the task requires it.
* When changing build scripts, check the differences between Windows, macOS ARM, and macOS Intel.
* Do not change publication paths, SSH settings, keys, or server data without an explicit request.
* Do not expose secrets, tokens, passwords, or private keys.

## Verifying changes

After making changes:

1. Check syntax of affected Python files.
2. Run only tests relevant to the task.
3. Check imports of modified modules.
4. Test the modified function in isolation when possible.
5. Do not run the full test suite when a focused check is sufficient.

If no tests cover the modified code, report that fact.

When testing the local application, it is permitted to terminate an already
running development instance of this project (including `Run Web.sh`,
`Run Tauri.sh`, and their verified child processes) when it blocks the required
port or prevents a clean test run. First verify the process command and project
path; do not terminate an unrelated application or a production build.

For changes to the Vue interface that affect user interaction, verify the flow
in Chromium through Playwright when practical. Use `Run Web.sh` to start the
local application. If Chromium is not installed for Playwright, ask the user
for permission before running `npx playwright install chromium`.

## Extending creative rhythm, cabinet, and shop

The feature state belongs to `game.Gamer`. New fields must have defaults both in
`Gamer.__init__()` and `Gamer.migrate()`, and malformed values must be repaired in
`normalize_motivation()`. Keep save-compatible keys canonical and Russian where the
project already uses Russian keys.

Creative rhythm is coordinated by `game.py` and `game_UI.py`:

* `WEEKLY_CHALLENGES` contains challenge metadata and canonical keys.
* `GameMenuController.WEEKLY_CHALLENGE_KEYS` maps combo-box positions to those keys.
  Its order must exactly match `weekly_challenge_combo` in `UI template/main_window.ui`.
* `_advance_weekly_challenge()` is the only place that advances and rewards weekly
  challenges. Pass it explicit event data instead of inferring intent from UI state.
* `INSPIRATION_ABILITIES` maps each active ability to its cost, bonus, and pending
  bonus field. Keep its order synchronized with `inspiration_ability_combo`. Spend
  inspiration only after validation, reject a second pending effect of the same
  type, and consume the bonus only when the promised reward is actually granted.
* Specialization mastery is stored as cumulative XP in
  `Gamer.specialization_mastery`; ranks are derived from
  `SPECIALIZATION_MASTERY_THRESHOLDS`. Award mastery only for an action matching the
  currently selected specialization, and derive its passive bonus from the rank.
* Active specialization cooldowns and pending effects live in
  `specialization_ability_ready_at` and `specialization_ability_effects`. Start a
  cooldown only after successful activation, keep an inapplicable effect pending,
  and clear it at the exact reward point it modifies.
* A writing session is stored in `Gamer.writing_session`. Its timer uses wall-clock
  time through `get_session_now()` and is refreshed by the existing one-second
  `QTimer`; do not implement a blocking timer or decrement the saved duration.
* Session modes are canonical keys in `WRITING_SESSION_MODES`. Results derive a grade
  from progress, update `writing_session_streak`, and append a normalized entry to
  the last-20 `writing_session_history`. Failed sessions must not grant rewards;
  cancellations do not count as results.
* Sessions whose intention is `Отредактировать текст` count the absolute size of
  both positive and negative text changes. Route negative changes through
  `record_editing_progress()` only: they must not grant writing rewards, inspiration,
  symbol-challenge progress, or streak rewards. Unchanged totals cannot be inferred
  as edited text and intentionally create no progress record.
* Daily challenge variants are declared by `DAILY_CHALLENGE_TYPES` and
  `DAILY_CHALLENGE_DIFFICULTIES`. Keep the chosen challenge and all offered options
  for the same date; changing a choice spends inspiration only after validating the
  new option. Advance symbol, session, and editing goals only from their explicit
  matching events.
* Creative events are defined in `CREATIVE_EVENTS` and become pending after the
  configured number of productive text actions. Never open a modal while text is
  being recorded: show the pending event on the rhythm tab and resolve its safe or
  risky choice there. Apply and persist an event result exactly once, then clear it.
* New permanent controls belong in the source `.ui`; regenerate
  `UI_fiiles/main_window.py` after changing it.

The cabinet is an achievement view over manuscript progress:

* `MANUSCRIPT_MILESTONES` defines one-time rewards per project.
* `Gamer.manuscript_journeys` stores received milestone percentages by stable project
  key. `advance_manuscript_journey()` awards only thresholds not already stored.
* `CABINET_RELICS` defines name, description, condition, `required_progress`, and
  `required_projects`, plus one passive `effect_type` and `bonus`. The current generic
  unlocker supports any relic based on the number of projects that reached a
  milestone. Add a separate explicit rule only when a new relic cannot be represented
  by these two fields.
* `CABINET_SETS` derives collection bonuses from unlocked relic keys; do not persist
  duplicated set state. Apply cabinet reward bonuses at the final reward multiplier
  and route earned inspiration through `Gamer.add_inspiration()`.
* Locked relics intentionally hide their name and description but show the unlock
  condition. `update_cabinet_ui()` rebuilds the list only when its signature changes.
* Keep the cabinet page size-independent from description length: use bounded,
  word-wrapped labels and an expanding detail panel. Test several relics in a compact
  main window after changing text or layout.

Shop content is declared in `game_data.py`. Add a reusable `Item` or `FuncItem`, then
register it under its canonical key in `ITEM_REGISTRY`. A `FuncItem` handler must
support `?` without loading a save, handle `use`, normalize affected motivation
state, save once, and return a user-facing result. Temporary bonuses need a migrated
field and must be cleared only when their promised reward is actually granted.
Session consumables use `session_streak_shields` (maximum three) and
`session_grade_boosts` (maximum one). A streak shield is spent only on a failed
completed session; a quality medal upgrades bronze or silver once and remains
pending after a failed or already-gold session. Item handlers should reject use at
the stacking limit so the inventory controller does not consume the item.

For every extension, add the Russian UI strings to source Python or `.ui` files,
regenerate translations, and verify all five non-Russian languages. At minimum run:

```bash
python3 -m py_compile game.py game_UI.py game_data.py localization.py
python3 -m pytest -q tests/test_writing_motivation.py tests/test_potion_catalog.py tests/test_localization.py
```

## Response format

After completing a task, provide a brief report:

* what was changed;
* which files were affected;
* which checks were performed;
* whether known limitations or risks remain.

Do not describe obvious commands or search steps in detail.
