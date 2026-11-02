import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_f10_fixture_is_documentation_only_and_has_all_target_architectures():
    fixture = json.loads(
        (ROOT / "docs/frontend-migration/examples/F10_proposed_manifest_sequence.example.json")
        .read_text(encoding="utf-8")
    )
    assert fixture["purpose"].startswith("documentation fixture only")
    assert set(fixture["transition_release"]["helper"]) == {
        "windows-x86_64", "darwin-aarch64", "darwin-x86_64",
    }
    assert set(fixture["transition_release"]["installer"]) == {
        "windows-x86_64", "darwin-aarch64", "darwin-x86_64",
    }
    assert all(
        value["sha256"] == "REPLACE_WITH_SHA256"
        for group in ("helper", "installer")
        for value in fixture["transition_release"][group].values()
    )


def test_f10_workflow_is_manual_and_publish_job_is_protected():
    workflow = (ROOT / ".github/workflows/build.yml").read_text(encoding="utf-8")
    assert "publish_production:" in workflow
    assert "github.event_name == 'workflow_dispatch'" in workflow
    assert "${{ inputs.publish_production }}" in workflow
    assert "name: nfprogress-production" in workflow

    qualification = (ROOT / ".github/workflows/release-qualification.yml").read_text(
        encoding="utf-8"
    )
    assert re.search(r"^on:\n  workflow_dispatch:", qualification, re.MULTILINE)
    assert "gh release" not in qualification
    assert "update_manifest" not in qualification


def test_f10_signing_script_is_fail_closed_and_keeps_credentials_external():
    script = (ROOT / "scripts/sign-notarize-macos.sh").read_text(encoding="utf-8")
    assert "APPLE_SIGNING_IDENTITY" in script
    assert "APPLE_NOTARY_PROFILE" in script
    assert "codesign --verify --deep --strict" in script
    assert "xcrun notarytool submit" in script
    assert "xcrun stapler staple" in script
    assert "spctl --assess" in script
    assert "TAURI_SIGNING_PRIVATE_KEY" not in script

    windows_script = (ROOT / "scripts/sign-windows-artifacts.ps1").read_text(
        encoding="utf-8"
    )
    assert "WINDOWS_CERTIFICATE_BASE64" in windows_script
    assert "WINDOWS_CERTIFICATE_PASSWORD" in windows_script
    assert "WINDOWS_TIMESTAMP_URL" in windows_script
    assert "signtool sign" in windows_script
    assert "Get-AuthenticodeSignature" in windows_script
    workflow = (ROOT / ".github/workflows/build.yml").read_text(encoding="utf-8")
    assert "Sign Windows executable and installer" in workflow
    assert "Publication is required for a release dispatch" in workflow


def test_f10_identity_and_data_root_contracts_are_stable():
    config = json.loads(
        (ROOT / "frontend/src-tauri/tauri.conf.json").read_text(encoding="utf-8")
    )
    assert config["productName"] == "worta"
    assert config["identifier"] == "app.nfprogress.tracker"
    assert "externalBin" not in config["bundle"]

    rust = (ROOT / "frontend/src-tauri/src/lib.rs").read_text(encoding="utf-8")
    assert 'join("nfprogress")' in rust
    assert 'home.join("Documents").join("nfprogress")' in rust
    assert '"migration_required: legacy_data_detected"' in rust


def test_release_workflow_requires_all_three_platform_artifacts():
    workflow = (ROOT / ".github/workflows/build.yml").read_text(encoding="utf-8")
    assert "for arch in arm intel; do" in workflow
    assert '"$GITHUB_SHA"' in workflow
    assert "Validated Windows x64, macOS ARM, and macOS Intel release builds." in workflow


def test_telegram_receipts_fail_closed_and_allow_safe_reruns(tmp_path):
    """Exercise the workflow shell against absent, sent, and uncertain receipts."""
    import os
    import subprocess
    import yaml

    workflow = yaml.safe_load((ROOT / '.github/workflows/build.yml').read_text())
    steps = workflow['jobs']['publish_release']['steps']
    script = next(step['run'] for step in steps if step.get('id') == 'telegram_receipt')
    fake_bin = tmp_path / 'bin'
    fake_bin.mkdir()
    gh = fake_bin / 'gh'
    gh.write_text('''#!/bin/sh
if [ "$1 $2" = "release view" ]; then
  case "$RECEIPT_STATE" in
    new) exit 1 ;;
    missing) echo '{"assets":[]}' ;;
    *) echo '{"assets":[{"name":"telegram-receipt.json"}]}' ;;
  esac
elif [ "$1 $2" = "release download" ]; then
  mkdir -p "$RUNNER_TEMP/telegram-receipt"
  if [ "$RECEIPT_STATE" = sent ]; then
    echo '{"version":"5.3.12","status":"sent","message_id":123}' > "$RUNNER_TEMP/telegram-receipt/telegram-receipt.json"
  else
    echo '{"version":"5.3.12","status":"pending"}' > "$RUNNER_TEMP/telegram-receipt/telegram-receipt.json"
  fi
elif [ "$1" = api ]; then
  :
else
  exit 1
fi
''')
    gh.chmod(0o755)
    for state, expected, output in (
        ('new', 0, 'already_sent=false'),
        ('sent', 0, 'already_sent=true'),
        ('pending', 1, 'indeterminate'),
        ('missing', 1, 'no Telegram receipt'),
    ):
        work = tmp_path / state
        work.mkdir()
        output_file = work / 'output'
        env = dict(os.environ, PATH=f'{fake_bin}:{os.environ["PATH"]}',
                   RUNNER_TEMP=str(work), GITHUB_OUTPUT=str(output_file),
                   GITHUB_REPOSITORY='owner/repo', APP_VERSION='5.3.12',
                   RECEIPT_STATE=state)
        result = subprocess.run(['bash', '-e', '-c', script], env=env,
                                text=True, capture_output=True)
        assert result.returncode == expected, (state, result.stderr)
        text = output_file.read_text() if output_file.exists() else result.stderr
        assert output in text


def test_release_requires_existing_tag_and_cleans_temporary_actions_artifact():
    workflow = (ROOT / ".github/workflows/build.yml").read_text(encoding="utf-8")
    assert "--verify-tag" in workflow
    assert "--target \"${{ steps.release_provenance.outputs.tag_sha }}\"" not in workflow
    assert "retention-days: 1" in workflow
    assert "Remove temporary Actions artifact after publication attempt" in workflow
    assert "actions: write" in workflow
