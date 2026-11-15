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
update_legacy_release_manifest = _load_script(
    'update-release-manifest.py', 'update_legacy_release_manifest',
)
sync_versions = _load_script(
    'sync-tauri-versions.py', 'sync_tauri_versions',
)
artifact_revision = _load_script(
    'verify-tauri-artifact.py', 'verify_tauri_artifact',
).artifact_revision
release_catalog = _load_script(
    'create-worta-releases-manifest.py', 'create_worta_releases_manifest',
).release_catalog


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
    tauri_source = (tauri_dir / 'src' / 'lib.rs').read_text(encoding='utf-8')
    assert 'https://worta.ru/app/update_manifest.json' in tauri_source
    assert 'url.starts_with("https://worta.ru/app/")' in tauri_source
    assert 'https://nfproject.ru/app/update_manifest.json' not in tauri_source


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
        public_key='untrusted comment: minisign public key\nRWQexample',
    )

    assert payload['bundle']['createUpdaterArtifacts'] is True
    assert payload['bundle']['windows']['nsis']['installMode'] == 'currentUser'
    assert payload['plugins']['updater']['endpoints'] == [
        'https://worta.ru/app/latest.json',
    ]
    assert payload['plugins']['updater']['windows']['installMode'] == 'passive'


def test_update_manifest_embeds_signature_and_stable_release_url():
    payload = update_manifest(
        version='4.15.0',
        tag='v4.15.0',
        artifact_name='nfprogress-windows-x86_64-4.15.0-setup.exe',
        signature='signed-content\n',
        notes='Исправления\n',
        published_at='2026-08-23T10:00:00Z',
    )

    windows = payload['platforms']['windows-x86_64']
    assert windows['signature'] == 'signed-content'
    assert windows['url'] == (
        'https://worta.ru/app/nfprogress-windows-x86_64-4.15.0-setup.exe'
    )


def test_worta_download_catalog_preserves_platforms_until_qualified():
    previous = release_catalog('5.3.10', platforms={'macos-arm'})
    updated = release_catalog(
        '5.3.11', existing=previous, platforms={'macos-intel'},
    )

    releases = {item['platform']: item for item in updated['releases']}
    assert releases['macos-intel']['url'] == 'https://worta.ru/app/nfprogress-mac-intel-5.3.11.zip'
    assert releases['macos-arm']['version'] == '5.3.10'
    assert releases['windows']['status'] == 'unavailable'


def test_update_manifest_supports_a_separate_worta_destination(tmp_path, monkeypatch):
    archive = tmp_path / 'nfprogress-mac-arm-5.3.11.zip'
    archive.write_bytes(b'qualified archive')
    output_manifest = tmp_path / 'worta-update_manifest.json'
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('RELEASE_BASE_URL', 'https://worta.ru/app')
    monkeypatch.setenv('RELEASE_MANIFEST_PATH', str(output_manifest))
    monkeypatch.setattr(
        'sys.argv',
        ['update-release-manifest.py', '5.3.11', 'macos_arm', str(archive)],
    )

    assert update_legacy_release_manifest.main() == 0
    payload = json.loads(output_manifest.read_text(encoding='utf-8'))
    assert payload['macos_arm']['url'] == (
        'https://worta.ru/app/nfprogress-mac-arm-5.3.11.zip'
    )


def test_legacy_windows_manifest_rejects_nsis_without_mutation(tmp_path, monkeypatch):
    installer = tmp_path / 'setup.exe'
    installer.write_bytes(b'NSIS installer')
    manifest = tmp_path / 'update_manifest.json'
    original = '{"windows_version":"5.3.9"}\n'
    manifest.write_text(original, encoding='utf-8')
    monkeypatch.setenv('RELEASE_BASE_URL', 'https://nfproject.ru/app')
    monkeypatch.setenv('RELEASE_MANIFEST_PATH', str(manifest))
    monkeypatch.setattr('sys.argv', ['update-release-manifest.py', '5.3.12', 'windows', str(installer)])

    assert update_legacy_release_manifest.main() == 2
    assert manifest.read_text(encoding='utf-8') == original


def test_worta_windows_manifest_accepts_nsis(tmp_path, monkeypatch):
    installer = tmp_path / 'setup.exe'
    installer.write_bytes(b'NSIS installer')
    manifest = tmp_path / 'update_manifest.json'
    monkeypatch.setenv('RELEASE_BASE_URL', 'https://worta.ru/app')
    monkeypatch.setenv('RELEASE_MANIFEST_PATH', str(manifest))
    monkeypatch.setattr('sys.argv', ['update-release-manifest.py', '5.3.12', 'windows', str(installer)])

    assert update_legacy_release_manifest.main() == 0
    payload = json.loads(manifest.read_text(encoding='utf-8'))
    assert payload['windows']['version'] == '5.3.12'
    assert payload['windows']['size'] == len(b'NSIS installer')
    assert payload['windows']['url'].startswith('https://worta.ru/app/')


def test_tauri_archive_exposes_its_source_revision(tmp_path):
    revision = '0123456789abcdef0123456789abcdef01234567'
    archive_path = tmp_path / 'nfprogress-mac.zip'
    with zipfile.ZipFile(archive_path, 'w') as archive:
        archive.writestr(
            'nfprogress-tauri-mac-arm-5.2.0/SOURCE_CODE.txt',
            f'Исходный код\n\nРевизия сборки: {revision}\n',
        )

    assert artifact_revision(archive_path) == revision
