#!/usr/bin/env python3
"""Keep one reusable macOS npm install and clone it into each build."""
import fcntl
import hashlib
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

from build_cache import root


def main(frontend: Path) -> None:
    cache = root() / 'tauri-npm'
    cache.mkdir(mode=0o700, parents=True, exist_ok=True)
    if cache.resolve() != cache:
        raise ValueError(f'Symlinked cache path: {cache}')
    seed = cache / 'install'
    stamp = seed / 'fingerprint'
    manifests = ('package.json', 'package-lock.json', '.npmrc')
    fingerprint = hashlib.sha256()
    for name in manifests:
        path = frontend / name
        if path.is_file():
            fingerprint.update(name.encode() + b'\0' + hashlib.sha256(path.read_bytes()).digest())
    for command in (['node', '-p', 'process.platform + "/" + process.arch + "/" + process.version'],
                    ['npm', '--version']):
        fingerprint.update(subprocess.check_output(command, cwd=frontend))
    fingerprint.update(os.environ.get('NODE_ENV', '').encode())
    key = fingerprint.hexdigest()

    with (cache / '.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if seed.is_symlink():
            raise ValueError(f'Symlinked npm installation: {seed}')
        if not (seed / 'node_modules/.package-lock.json').is_file() or not stamp.is_file() or stamp.read_text() != key:
            with tempfile.TemporaryDirectory(prefix='install-', dir=cache) as staging:
                install = Path(staging) / 'install'
                install.mkdir()
                for name in manifests:
                    if (frontend / name).is_file():
                        shutil.copyfile(frontend / name, install / name)
                subprocess.run(['npm', 'ci', '--prefer-offline', '--include=dev', '--include=optional'],
                               cwd=install, check=True)
                for name in manifests:
                    (install / name).unlink(missing_ok=True)
                (install / 'fingerprint').write_text(key)
                if seed.exists():
                    shutil.rmtree(seed)
                install.rename(seed)
        destination = frontend / 'node_modules'
        if destination.exists():
            shutil.rmtree(destination)
        # APFS clones share unchanged blocks. cp falls back to a copy on
        # filesystems or volumes that cannot clone the source.
        flags = '-cRpP' if platform.system() == 'Darwin' else '-RpP'
        subprocess.run(['cp', flags, str(seed / 'node_modules'), str(destination)], check=True)


if __name__ == '__main__':
    main(Path(sys.argv[1]).resolve(strict=True))
