import os
import subprocess
import textwrap
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def _write_executable(path: Path, content: str) -> None:
    path.write_text(textwrap.dedent(content).lstrip(), encoding='utf-8')
    path.chmod(0o755)


def _create_build_fixture(tmp_path: Path) -> tuple[Path, Path]:
    root = tmp_path / 'project'
    scripts_dir = root / 'scripts'
    frontend_tauri_dir = root / 'frontend' / 'src-tauri'
    scripts_dir.mkdir(parents=True)
    frontend_tauri_dir.mkdir(parents=True)
    (scripts_dir / 'build-tauri-local.sh').write_text(
        (ROOT / 'scripts' / 'build-tauri-local.sh').read_text(encoding='utf-8'),
        encoding='utf-8',
    )
    (scripts_dir / 'build-tauri-local.sh').chmod(0o755)
    (scripts_dir / 'prepare-tauri-test-data.sh').write_text(
        (ROOT / 'scripts' / 'prepare-tauri-test-data.sh').read_text(encoding='utf-8'),
        encoding='utf-8',
    )
    (scripts_dir / 'prepare-tauri-test-data.sh').chmod(0o755)
    (root / 'frontend' / 'package.json').write_text('{}\n', encoding='utf-8')
    (root / 'frontend' / 'package-lock.json').write_text('{}\n', encoding='utf-8')
    (frontend_tauri_dir / 'tauri.conf.json').write_text('{}\n', encoding='utf-8')
    (frontend_tauri_dir / 'Cargo.toml').write_text(
        '[package]\nname = "nfprogress-desktop"\nversion = "5.3.0"\n',
        encoding='utf-8',
    )
    (root / 'mindmap_assets').mkdir()
    (root / 'mindmap_assets' / 'index.html').write_text('', encoding='utf-8')
    for filename in ('Icon-256.png', 'appicon.icns', 'icon.ico', 'LICENSE', 'SOURCE_CODE.txt'):
        (root / filename).write_text(filename, encoding='utf-8')

    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    _write_executable(
        bin_dir / 'uname',
        """
        #!/bin/bash
        if [ "${1:-}" = "-m" ]; then
          printf '%s\\n' arm64
        else
          printf '%s\\n' Darwin
        fi
        """,
    )
    _write_executable(
        bin_dir / 'cargo',
        """
        # The local build keeps no Cargo target unless retention is explicit.
        if [ "${1:-}" = "clean" ] && [ -n "${CARGO_TARGET_DIR:-}" ]; then
          rm -rf -- "$CARGO_TARGET_DIR"
        fi
        exit 0
        """,
    )
    _write_executable(
        bin_dir / 'rustup',
        """
        #!/bin/bash
        printf '%s\\n' aarch64-apple-darwin x86_64-apple-darwin
        """,
    )
    _write_executable(
        bin_dir / 'rsync',
        """
        #!/bin/bash
        set -euo pipefail
        source_path="${@: -2:1}"
        destination_path="${@: -1}"
        mkdir -p "$destination_path"
        for argument in "$@"; do
          if [ "$argument" = '--include=*.py' ]; then
            for file in "$source_path"/*.py; do
              [ ! -f "$file" ] || cp -p "$file" "$destination_path"/
            done
            exit 0
          fi
        done
        cp -R "$source_path"/. "$destination_path"/
        """,
    )
    _write_executable(
        bin_dir / 'npm',
        """
        #!/bin/bash
        set -euo pipefail
        if [ "${1:-}" != "ci" ]; then
          exit 2
        fi
        sleep 0.2
        mkdir -p node_modules
        : > node_modules/.package-lock.json
        printf '%s\\n' "$PWD" >> "$TEST_NPM_LOG"
        """,
    )
    _write_executable(
        bin_dir / 'node',
        """
        # The fixture only needs the version helper's observable output.
        for argument in "$@"; do
          if [ "$argument" = "--version-only" ]; then
            printf '%s\\n' 5.3.0
            exit 0
          fi
        done
        printf '%s\\n' 'Synchronized Tauri files to 5.3.0.'
        printf '%s\\n' 5.3.0
        """,
    )
    _write_executable(
        bin_dir / 'python3',
        """
        #!/bin/bash
        set -euo pipefail
        if [ "${1:-}" = "-c" ]; then
          exit 0
        fi
        if [ "${1:-}" = "-m" ] \
          && [ "${2:-}" = "backend.app" ] \
          && [ "${3:-}" = "--prepare-dev-data" ]; then
          printf '%s\n' "$PWD" >> "$TEST_PREPARE_LOG"
          exit 0
        fi
        case "${1:-}" in
          *sync-tauri-versions.py)
            for argument in "$@"; do
              if [ "$argument" = "--version-only" ]; then
                printf '%s\\n' 5.3.0
                exit 0
              fi
            done
            ;;
          *)
            exit 2
            ;;
        esac
        """,
    )
    _write_executable(
        scripts_dir / 'build-tauri-dmg.sh',
        """
        #!/bin/bash
        set -euo pipefail
        printf '%s\\n' "$NFPROGRESS_TAURI_FRONTEND_DIR" >> "$TEST_DMG_LOG"
        mkdir -p "$NFPROGRESS_TAURI_FRONTEND_DIR/src-tauri/target/$1/release/bundle"
        : > "$NFPROGRESS_TAURI_FRONTEND_DIR/src-tauri/target/$1/release/bundle/temporary-app-output"
        mkdir -p "$(dirname "$2")"
        : > "$2"
        """,
    )
    _write_executable(
        bin_dir / 'git',
        """
        #!/bin/bash
        printf '%s\\n' 0123456789abcdef0123456789abcdef01234567
        """,
    )
    _write_executable(
        bin_dir / 'ditto',
        """
        #!/bin/bash
        set -euo pipefail
        output_path="${!#}"
        : > "$output_path"
        """,
    )
    return root, bin_dir


def test_parallel_architecture_builds_use_separate_frontend_workspaces(tmp_path):
    root, bin_dir = _create_build_fixture(tmp_path)
    for arch, prefix in (
        ('arm', 'nfprogress-tauri-mac-arm'),
        ('intel', 'nfprogress-tauri-mac-intel'),
    ):
        build_dir = root / f'build-tauri-{arch}'
        build_dir.mkdir()
        (build_dir / f'{prefix}-4.9.0.zip').write_bytes(b'old zip')
        (build_dir / f'{prefix}-4.9.0.dmg').write_bytes(b'old dmg')
    npm_log = tmp_path / 'npm.log'
    dmg_log = tmp_path / 'dmg.log'
    common_env = os.environ | {
        'PATH': f'{bin_dir}{os.pathsep}{os.environ["PATH"]}',
        'TEST_NPM_LOG': str(npm_log),
        'TEST_DMG_LOG': str(dmg_log),
    }

    arm_process = subprocess.Popen(
        [str(root / 'scripts' / 'build-tauri-local.sh'), 'arm'],
        cwd=root,
        env=common_env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    intel_process = subprocess.Popen(
        [str(root / 'scripts' / 'build-tauri-local.sh'), 'intel'],
        cwd=root,
        env=common_env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    arm_stdout, arm_stderr = arm_process.communicate(timeout=15)
    intel_stdout, intel_stderr = intel_process.communicate(timeout=15)

    assert arm_process.returncode == 0, arm_stdout + arm_stderr
    assert intel_process.returncode == 0, intel_stdout + intel_stderr
    workspace_root = root / '.tauri-build-workspaces'
    arm_frontend = workspace_root / 'arm' / 'frontend'
    intel_frontend = workspace_root / 'intel' / 'frontend'
    assert (arm_frontend / 'node_modules' / '.package-lock.json').is_file()
    assert (intel_frontend / 'node_modules' / '.package-lock.json').is_file()
    assert not (root / 'frontend' / 'node_modules').exists()
    assert sorted(npm_log.read_text(encoding='utf-8').splitlines()) == sorted([
        str(arm_frontend),
        str(intel_frontend),
    ])
    assert sorted(dmg_log.read_text(encoding='utf-8').splitlines()) == sorted([
        str(arm_frontend),
        str(intel_frontend),
    ])
    assert sorted(path.name for path in (root / 'build-tauri-arm').iterdir()) == [
        'nfprogress-tauri-mac-arm-5.3.0.zip',
    ]
    assert sorted(path.name for path in (root / 'build-tauri-intel').iterdir()) == [
        'nfprogress-tauri-mac-intel-5.3.0.zip',
    ]
    assert not (arm_frontend / 'src-tauri' / 'target' / 'aarch64-apple-darwin' / 'release' / 'bundle').exists()
    assert not (intel_frontend / 'src-tauri' / 'target' / 'x86_64-apple-darwin' / 'release' / 'bundle').exists()
    assert not (arm_frontend / 'src-tauri' / 'binaries').exists()
    assert not (intel_frontend / 'src-tauri' / 'binaries').exists()
    assert not (arm_frontend / 'src-tauri' / 'target').exists()
    assert not (intel_frontend / 'src-tauri' / 'target').exists()


def test_test_build_profile_uses_distinct_artifact_prefix(tmp_path):
    root, bin_dir = _create_build_fixture(tmp_path)
    common_env = os.environ | {
        'PATH': f'{bin_dir}{os.pathsep}{os.environ["PATH"]}',
        'NFPROGRESS_BUILD_PROFILE': 'test',
        'TEST_NPM_LOG': str(tmp_path / 'npm.log'),
        'TEST_DMG_LOG': str(tmp_path / 'dmg.log'),
        'TEST_PREPARE_LOG': str(tmp_path / 'prepare.log'),
    }

    result = subprocess.run(
        [str(root / 'scripts' / 'build-tauri-local.sh'), 'arm'],
        cwd=root,
        env=common_env,
        capture_output=True,
        text=True,
        timeout=15,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert (root / 'build-tauri-arm' / 'nfprogress-test-tauri-mac-arm-5.3.0.zip').is_file()
    assert (tmp_path / 'prepare.log').read_text(encoding='utf-8').splitlines() == [str(root)]
