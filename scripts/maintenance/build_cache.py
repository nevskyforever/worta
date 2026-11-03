#!/usr/bin/env python3
"""Standard Cargo target cache runner and conservative size/cleanup report.

Release targets remain worktree-local to preserve Tauri bundle provenance.
Cargo registry and npm cache retain their standard, already shared locations.
"""
import argparse
import hashlib
import fcntl
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import sys


def root():
    return Path.home() / 'Library/Caches/worta/build' if os.uname().sysname == 'Darwin' else Path.home() / '.cache/worta/build'


def usage(path):
    return int(subprocess.check_output(['du', '-sk', str(path)], text=True).split()[0]) * 1024 if path.exists() else 0


def target_path(base, target):
    if not re.fullmatch(r'[a-zA-Z0-9_-]+', target):
        raise ValueError('Invalid Rust target')
    return base / 'targets' / target


def locked(base, target):
    base.mkdir(parents=True, exist_ok=True)
    locks = base / 'locks'
    locks.mkdir(exist_ok=True)
    lock_path = locks / (target + '.lock')
    if locks.resolve() != locks or lock_path.is_symlink():
        raise ValueError('Symlinked lock refused')
    handle = lock_path.open('a')
    fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    return handle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['run', 'report', 'clean'])
    parser.add_argument('--target', default='')
    parser.add_argument('--days', type=int, default=30)
    parser.add_argument('--apply', action='store_true')
    argv = sys.argv[1:]
    split = argv.index('--') if '--' in argv else len(argv)
    command = argv[split + 1:]
    args = parser.parse_args(argv[:split])
    base = root()
    if base.is_symlink() or base.resolve() != base:
        raise ValueError('Cache root must be a literal path')
    if args.action == 'run':
        target_path(base, args.target)
        workspace = subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], text=True).strip()
        key = args.target + '-' + os.environ.get('NFPROGRESS_BUILD_PROFILE', 'development') + '-' + hashlib.sha256(str(Path(workspace).resolve()).encode()).hexdigest()[:16]
        target = target_path(base, key)
        if command and command[0] == '--':
            command = command[1:]
        if not command:
            parser.error('run requires --target TARGET -- COMMAND')
        with locked(base, key):
            parent = base / 'targets'
            if parent.resolve() != parent:
                raise ValueError('Symlinked targets parent refused')
            target.mkdir(parents=True, exist_ok=True)
            if target.is_symlink() or target.resolve() != target:
                raise ValueError('Symlinked target refused')
            marker = target / '.worta-cache'
            if marker.is_symlink():
                raise ValueError('Symlinked cache marker refused')
            marker.touch()
            env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL='0', WORTA_MANAGED_CACHE='1')
            try:
                return subprocess.call(command, env=env)
            finally:
                marker.touch()
    print('Managed Cargo targets:', base)
    registry = Path(os.environ.get('CARGO_HOME', str(Path.home()/'.cargo'))) / 'registry'
    print('Cargo registry:', registry, 'bytes:', usage(registry))
    npm = subprocess.check_output(['npm', 'config', 'get', 'cache'], text=True).strip()
    print('npm cache:', npm, 'bytes:', usage(Path(npm)))
    # Existing release targets are intentionally isolated and report-only here.
    from clean_generated import GENERATED, repository_root
    repository = repository_root()
    for relative in GENERATED:
        local = repository / relative
        if local.is_dir() and local.resolve() == local:
            print('LOCAL; review with clean_generated.py:', local, 'bytes:', usage(local))
    for path in sorted((base / 'targets').glob('*')):
        target_path(base, path.name)
        if path.is_symlink() or path.resolve() != path or (path / '.worta-cache').is_symlink() or not (path / '.worta-cache').is_file():
            print('REFUSED', path)
            continue
        try:
            with locked(base, path.name):
                age = (time.time() - (path / '.worta-cache').stat().st_mtime) / 86400
                protected = any(any(path.rglob(pattern)) for pattern in ('*.app', '*.dmg', '*.zip', '*.db', '*.sqlite*', '*.bak', '.git'))
                stale = age >= max(1, args.days) and not protected
                print('CLEANABLE' if stale else 'KEEP', path, 'bytes:', usage(path), 'age days:', round(age, 1))
                if args.action == 'clean' and args.apply and stale:
                    shutil.rmtree(path)
        except BlockingIOError:
            print('ACTIVE; retained:', path)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
