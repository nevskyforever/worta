#!/usr/bin/env python3
"""Prepare the static worta.ru download catalog."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


SEMVER = re.compile(r'^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$')
PLATFORMS = {
    'windows': 'nfprogress-windows-x86_64-{version}-setup.exe',
    'macos-arm': 'nfprogress-mac-arm-{version}.zip',
    'macos-intel': 'nfprogress-mac-intel-{version}.zip',
}


def release_catalog(
    version: str,
    *,
    existing: dict[str, object] | None = None,
    platforms: set[str] | None = None,
) -> dict[str, object]:
    if not SEMVER.fullmatch(version):
        raise ValueError(f'Invalid semantic version: {version!r}.')
    selected = platforms or set(PLATFORMS)
    unknown = selected - set(PLATFORMS)
    if unknown:
        raise ValueError(f'Unknown platform(s): {", ".join(sorted(unknown))}.')

    previous = {}
    if existing and existing.get('schema_version') == 1:
        entries = existing.get('releases')
        if isinstance(entries, list):
            previous = {
                entry['platform']: entry
                for entry in entries
                if isinstance(entry, dict) and entry.get('platform') in PLATFORMS
            }

    releases = []
    for platform, filename_template in PLATFORMS.items():
        if platform in selected:
            filename = filename_template.format(version=version)
            release = {
                'platform': platform,
                'status': 'available',
                'version': version,
                'url': f'https://worta.ru/app/{filename}',
                'message': f'Скачать WORTA {version}',
            }
        else:
            release = previous.get(platform, {
                'platform': platform,
                'status': 'unavailable',
                'version': None,
                'url': None,
                'message': 'Проверенные установщики временно недоступны.',
            })
        releases.append(release)
    return {
        'schema_version': 1,
        'releases': releases,
        'message': f'Выберите установщик WORTA {version} для своей платформы.',
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--source', type=Path)
    parser.add_argument('--platform', action='append', choices=PLATFORMS)
    args = parser.parse_args()

    existing = None
    if args.source and args.source.is_file():
        existing = json.loads(args.source.read_text(encoding='utf-8'))
    payload = release_catalog(
        args.version,
        existing=existing,
        platforms=set(args.platform) if args.platform else None,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(args.output)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
