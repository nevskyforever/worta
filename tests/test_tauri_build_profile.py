import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PROFILE_SCRIPT = ROOT / 'scripts' / 'apply-tauri-build-profile.mjs'


def test_test_profile_overlay_is_distinct_and_production_overlay_is_unchanged(tmp_path):
    config_path = tmp_path / 'tauri.conf.json'
    original = {
        'productName': 'worta',
        'identifier': 'app.nfprogress.tracker',
        'app': {'windows': [{'title': 'nfprogress'}]},
    }
    config_path.write_text(json.dumps(original), encoding='utf-8')

    subprocess.run(
        ['node', str(PROFILE_SCRIPT), str(config_path), 'test'],
        check=True,
    )
    test_config = json.loads(config_path.read_text(encoding='utf-8'))
    assert test_config['productName'] == 'worta'
    assert test_config['identifier'] == 'app.nfprogress.tracker.test'
    assert test_config['app']['windows'][0]['title'] == 'worta Test'

    config_path.write_text(json.dumps(original), encoding='utf-8')
    subprocess.run(
        ['node', str(PROFILE_SCRIPT), str(config_path), 'production'],
        check=True,
    )
    assert json.loads(config_path.read_text(encoding='utf-8')) == original
