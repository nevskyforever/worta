import json
from pathlib import Path

import game_data


ROOT = Path(__file__).parents[1]
GAME_RS = ROOT / 'frontend' / 'src-tauri' / 'src' / 'game.rs'
TAURI_RS = ROOT / 'frontend' / 'src-tauri' / 'src' / 'lib.rs'
GAME_TS = ROOT / 'frontend' / 'src' / 'api' / 'game.ts'
GAME_CATALOG = ROOT / 'frontend' / 'src' / 'cloud' / 'gameShopCatalog.json'


def test_desktop_game_uses_explicit_typed_native_commands():
    source = GAME_TS.read_text()
    tauri_source = TAURI_RS.read_text()

    assert "currentPlatform() === 'tauri'" in source
    assert "game_command" not in source
    for command in (
        'game_state', 'game_buy_item', 'game_use_item',
        'game_process_bank_events', 'game_start_writing_session',
    ):
        assert command in source
        assert f'fn {command}' in tauri_source
    assert 'game_run_lottery' in tauri_source


def test_native_game_boundary_is_sqlite_only_and_strict():
    source = GAME_RS.read_text()

    assert 'open_projects_database' in source
    assert 'process_pending_events' in source
    assert 'deny_unknown_fields' in source
    assert 'GameRng' in source
    assert 'pickle' not in source.lower()
    assert 'gamer.pkl' not in source
    assert 'data.pkl' not in source


def test_shared_catalog_matches_legacy_buyable_game_catalog():
    assert 'include_str!("../../src/cloud/gameShopCatalog.json")' in GAME_RS.read_text()
    catalog = json.loads(GAME_CATALOG.read_text())
    shared_keys = sorted((item['category'], item['key']) for item in catalog if item['buyable'])
    legacy_keys = sorted(
        (category, key)
        for category in ('Зелья', 'Предметы', 'Парфюм')
        for key, item in game_data.ITEM_REGISTRY[category].items()
        if getattr(item, 'Buy', True)
    )
    assert shared_keys == legacy_keys
