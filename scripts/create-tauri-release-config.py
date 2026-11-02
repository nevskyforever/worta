#!/usr/bin/env python3
"""Create the temporary updater-enabled release overlay consumed by Tauri CI."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


SEMVER = re.compile(r'^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$')
def release_config(
    *,
    version: str,
    public_key: str,
) -> dict[str, object]:
    if not SEMVER.fullmatch(version):
        raise ValueError(f'Invalid semantic version: {version!r}.')
    public_key = public_key.strip()
    if not public_key:
        raise ValueError('The Tauri updater public key is empty.')
    endpoint = 'https://worta.ru/app/latest.json'
    return {
        'version': version,
        'bundle': {
            'targets': ['nsis'],
            'createUpdaterArtifacts': True,
            'windows': {
                'nsis': {'installMode': 'currentUser'},
            },
        },
        'plugins': {
            'updater': {
                'pubkey': public_key,
                'endpoints': [endpoint],
                'windows': {'installMode': 'passive'},
            },
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--public-key-file', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()

    payload = release_config(
        version=args.version,
        public_key=args.public_key_file.read_text(encoding='utf-8'),
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
