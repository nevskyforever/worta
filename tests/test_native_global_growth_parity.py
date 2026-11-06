"""Compare the genuine Native daily-goal command with the approved Python oracle."""
import json
import os
import subprocess
from datetime import date
from pathlib import Path

import pytest

import engine
import game


def test_native_daily_goal_and_python_global_start_match(monkeypatch):
    configured = os.environ.get('NFPROGRESS_GAME_NATIVE_BIN')
    if not configured:
        pytest.skip('Requires the freshly built Native Rust test binary via NFPROGRESS_GAME_NATIVE_BIN')
    binary = Path(configured).resolve(strict=True)
    today = date.today()
    monkeypatch.setattr(engine, 'today_for_test', lambda: today)
    monkeypatch.setattr(engine, 'load_settings', lambda: {'game_mode': True, 'global_streak': True})
    monkeypatch.setattr(engine, 'save_data', lambda data: None)
    monkeypatch.setattr(game.Gamer, 'save', lambda self: None)
    note = engine.Note(100, 100, 10)
    project = engine.Project(name='Project', goal=1000, total_symbols=100,
                             notes=[note], personal_goal_for_the_day=100)
    data = {'projects': {'Project': project}, 'global_streaks': [],
            'max_global_streak': 0, 'global_streak_status': 'No'}
    assert project.get_today_goal_value() == 100
    status = engine.global_streak_status(data, today=today)
    assert status == 'Start'
    gamer = game.Gamer(coins=10, exp=500)
    gamer.give_streak_bonus(status, 'Global', engine.streak_length(data['global_streaks']))
    result = subprocess.run([str(binary), 'production_daily_goal_grows', '--test-threads=1', '--nocapture'],
                            text=True, capture_output=True, timeout=120, check=True)
    line = next(line for line in result.stdout.splitlines() if 'WORTA_GROWTH_PARITY:' in line)
    native = json.loads(line.split('WORTA_GROWTH_PARITY:', 1)[1])
    assert float(native['coins']) == gamer.coins
    assert float(native['experience']) == gamer.exp
    assert native['global_streak']['history'] == [{'day': today.isoformat(), 'frozen': False}]
    assert native['global_streak']['maximum'] == data['max_global_streak']
