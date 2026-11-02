#!/usr/bin/env python3
"""Create a static Tauri v2 updater manifest for one signed Windows asset."""

from __future__ import annotations

import argparse
import json
import re
from datetime import datetime, timezone
from pathlib import Path


SEMVER = re.compile(r'^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$')
TAG = re.compile(r'^[A-Za-z0-9_.-]+$')


def update_manifest(
    *,
    version: str,
    tag: str,
    artifact_name: str,
    signature: str,
    notes: str,
    published_at: str,
) -> dict[str, object]:
    if not SEMVER.fullmatch(version):
        raise ValueError(f'Invalid semantic version: {version!r}.')
    if not TAG.fullmatch(tag):
        raise ValueError(f'Invalid release tag: {tag!r}.')
    if Path(artifact_name).name != artifact_name or not artifact_name.lower().endswith('.exe'):
        raise ValueError('The updater artifact must be a plain Windows .exe filename.')
    signature = signature.strip()
    if not signature:
        raise ValueError('The updater signature is empty.')

    asset_url = f'https://worta.ru/app/{artifact_name}'
    return {
        'version': version,
        'notes': notes.strip(),
        'pub_date': published_at,
        'platforms': {
            'windows-x86_64': {
                'signature': signature,
                'url': asset_url,
            },
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--artifact', type=Path, required=True)
    parser.add_argument('--signature', type=Path, required=True)
    parser.add_argument('--notes-file', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--published-at')
    args = parser.parse_args()

    if not args.artifact.is_file():
        raise SystemExit(f'Updater artifact does not exist: {args.artifact}')
    if not args.signature.is_file():
        raise SystemExit(f'Updater signature does not exist: {args.signature}')
    published_at = args.published_at or datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z')
    payload = update_manifest(
        version=args.version,
        tag=args.tag,
        artifact_name=args.artifact.name,
        signature=args.signature.read_text(encoding='utf-8'),
        notes=args.notes_file.read_text(encoding='utf-8'),
        published_at=published_at,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2) + '\n',
        encoding='utf-8',
    )
    print(args.output)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
