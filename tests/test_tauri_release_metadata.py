import importlib.util
import json
import tomllib
import zipfile
from pathlib import Path

import engine


ROOT = Path(__file__).resolve().parents[1]


def _load_script(filename: str, module_name: str):
    spec = importlib.util.spec_from_file_location(module_name, ROOT / 'scripts' / filename)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release_config = _load_script(
    'create-tauri-release-config.py', 'create_tauri_release_config',
).release_config
update_manifest = _load_script(
    'create-tauri-update-manifest.py', 'create_tauri_update_manifest',
).update_manifest
sync_versions = _load_script(
    'sync-tauri-versions.py', 'sync_tauri_versions',
)
artifact_revision = _load_script(
    'verify-tauri-artifact.py', 'verify_tauri_artifact',
).artifact_revision


def test_engine_versions_are_normalized_to_three_components():
    assert sync_versions.canonical_version('5.0') == '5.0.0'
    assert sync_versions.canonical_version('4.14.2') == '4.14.2'
    assert sync_versions.canonical_version('5.0-rc1') == '5.0.0-rc1'


def test_runtime_package_version_matches_canonical_engine_version():
    tauri_dir = ROOT / 'frontend' / 'src-tauri'
    config = json.loads((tauri_dir / 'tauri.conf.json').read_text(encoding='utf-8'))
    cargo = tomllib.loads((tauri_dir / 'Cargo.toml').read_text(encoding='utf-8'))

    assert sync_versions.read_engine_version() == engine.version
    assert config['version'] == engine.version
    assert config['productName'] == 'worta'
    assert config['identifier'] == 'app.nfprogress.tracker'
    assert cargo['package']['version'] == engine.version
    assert 'version: env!("CARGO_PKG_VERSION").to_string()' in (
        tauri_dir / 'src' / 'lib.rs'
    ).read_text(encoding='utf-8')


def test_version_sync_can_target_an_isolated_frontend_workspace(tmp_path):
    source_tauri_dir = ROOT / 'frontend' / 'src-tauri'
    workspace_tauri_dir = tmp_path / 'frontend' / 'src-tauri'
    workspace_tauri_dir.mkdir(parents=True)
    source_files = ('tauri.conf.json', 'Cargo.toml', 'Cargo.lock')
    source_contents = {
        filename: (source_tauri_dir / filename).read_text(encoding='utf-8')
        for filename in source_files
    }
    for filename, content in source_contents.items():
        (workspace_tauri_dir / filename).write_text(content, encoding='utf-8')

    sync_versions.synchronize('9.8.7', tmp_path / 'frontend')

    config = json.loads((workspace_tauri_dir / 'tauri.conf.json').read_text(encoding='utf-8'))
    assert config['version'] == '9.8.7'
    cargo = tomllib.loads((workspace_tauri_dir / 'Cargo.toml').read_text(encoding='utf-8'))
    assert cargo['package']['version'] == '9.8.7'
    assert 'name = "nfprogress-desktop"\nversion = "9.8.7"' in (
        workspace_tauri_dir / 'Cargo.lock'
    ).read_text(encoding='utf-8')
    for filename, content in source_contents.items():
        assert (source_tauri_dir / filename).read_text(encoding='utf-8') == content


def test_release_config_creates_nsis_updater_artifacts():
    payload = release_config(
        version='4.15.0',
        repository='owner/nfprogress',
        public_key='untrusted comment: minisign public key\nRWQexample',
    )

    assert payload['bundle']['createUpdaterArtifacts'] is True
    assert payload['bundle']['windows']['nsis']['installMode'] == 'currentUser'
    assert payload['plugins']['updater']['endpoints'] == [
        'https://github.com/owner/nfprogress/releases/latest/download/latest.json',
    ]
    assert payload['plugins']['updater']['windows']['installMode'] == 'passive'


def test_update_manifest_embeds_signature_and_stable_release_url():
    payload = update_manifest(
        version='4.15.0',
        repository='owner/nfprogress',
        tag='v4.15.0',
        artifact_name='nfprogress-windows-x86_64-4.15.0-setup.exe',
        signature='signed-content\n',
        notes='Исправления\n',
        published_at='2026-08-23T10:00:00Z',
    )

    windows = payload['platforms']['windows-x86_64']
    assert windows['signature'] == 'signed-content'
    assert windows['url'].endswith(
        '/releases/download/v4.15.0/nfprogress-windows-x86_64-4.15.0-setup.exe',
    )


def test_tauri_archive_exposes_its_source_revision(tmp_path):
    revision = '0123456789abcdef0123456789abcdef01234567'
    archive_path = tmp_path / 'nfprogress-mac.zip'
    with zipfile.ZipFile(archive_path, 'w') as archive:
        archive.writestr(
            'nfprogress-tauri-mac-arm-5.2.0/SOURCE_CODE.txt',
            f'Исходный код\n\nРевизия сборки: {revision}\n',
        )

    assert artifact_revision(archive_path) == revision
