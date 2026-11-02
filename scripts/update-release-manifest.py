#!/usr/bin/env python3
"""Update the cross-platform legacy release manifest."""

from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path


PLATFORMS = {
    "windows": ("nfprogress-windows-x86_64-{version}-setup.exe", None),
    "macos_arm": ("nfprogress-mac-arm-{version}.zip", None),
    "macos_intel": ("nfprogress-mac-intel-{version}.zip", None),
}


def _version_sort_key(version: str) -> list[int]:
    return [int("".join(c for c in part if c.isdigit()) or 0) for part in version.split(".")]


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    if len(sys.argv) not in (3, 4, 5):
        print("Usage: update-release-manifest.py VERSION PLATFORM [ARTIFACT] [INSTALLER]", file=sys.stderr)
        return 2

    version = sys.argv[1].strip()
    platform = sys.argv[2].strip()
    if not version or platform not in PLATFORMS:
        print("Version is empty or platform is unknown.", file=sys.stderr)
        return 2

    filename, _ = PLATFORMS[platform]
    filename = filename.format(version=version)
    if platform == "windows":
        # The Tauri Windows release has an NSIS installer, not the legacy ZIP.
        artifact = None
        installer = Path(sys.argv[3]) if len(sys.argv) >= 4 else None
    else:
        artifact = Path(sys.argv[3]) if len(sys.argv) >= 4 else None
        installer = Path(sys.argv[4]) if len(sys.argv) == 5 else None
        if artifact is None:
            print("macOS release requires an archive to calculate sha256 and size.", file=sys.stderr)
            return 2
    for path, label in ((artifact, "Artifact"), (installer, "Installer")):
        if path is not None and not path.is_file():
            print(f"{label} not found: {path}", file=sys.stderr)
            return 2

    base_url = os.environ.get("RELEASE_BASE_URL", "https://nfproject.ru/app").rstrip("/")
    manifest_path = Path(os.environ.get("RELEASE_MANIFEST_PATH", "update_manifest.json"))
    manifest = json.loads(manifest_path.read_text(encoding="utf-8")) if manifest_path.exists() else {}
    notes = os.environ.get("RELEASE_NOTES", "").strip()
    if notes:
        manifest["notes"] = notes
    else:
        manifest.setdefault("notes", "Исправлены ошибки и улучшена стабильность.")

    url = f"{base_url}/{filename}"
    if platform == "windows":
        section = {
            "version": version,
            "url": url,
            "installer_url": url,
        }
        if installer is not None:
            installer_sha256 = _sha256(installer)
            installer_size = installer.stat().st_size
            # Keep the historical secure-manifest names as aliases. Legacy
            # clients read these fields even though the current website uses
            # installer_url and installer_* metadata.
            section["sha256"] = installer_sha256
            section["size"] = installer_size
            section["entry_point"] = "nfprogress/nfprogress.exe"
            section["installer_sha256"] = _sha256(installer)
            section["installer_size"] = installer_size
        manifest["windows"] = section
        manifest["windows_version"] = version
        manifest["windows_url"] = url
    else:
        section = {"version": version, "url": url}
        if artifact is not None:
            section["sha256"] = _sha256(artifact)
            section["size"] = artifact.stat().st_size
        if installer is not None:
            section["installer_url"] = (
                f"{base_url}/nfprogress-windows-x86_64-{version}-setup.exe"
            )
            section["installer_sha256"] = _sha256(installer)
            section["installer_size"] = installer.stat().st_size
        manifest[platform] = section
        manifest[f"{platform}_version"] = version
        manifest[f"{platform}_url"] = url

    versions = [str(manifest.get("version", ""))]
    for platform_name in ("windows", "macos_arm", "macos_intel"):
        section = manifest.get(platform_name)
        if isinstance(section, dict) and section.get("version"):
            versions.append(str(section["version"]))
    for key in ("windows_version", "macos_arm_version", "macos_intel_version"):
        if manifest.get(key):
            versions.append(str(manifest[key]))
    manifest["version"] = max((item for item in versions if item), key=_version_sort_key)
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Updated {manifest_path}: {platform}={version}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
