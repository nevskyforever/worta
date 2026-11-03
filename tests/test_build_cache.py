import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/maintenance/build_cache.py'


class BuildCacheTests(unittest.TestCase):
    def test_two_worktrees_reuse_and_cleanup(self):
        with tempfile.TemporaryDirectory(prefix='worta-cache-test-') as temp:
            base = Path(temp).resolve()
            env = dict(os.environ, HOME=str(base / 'home'),
                       CARGO_HOME=os.environ.get('CARGO_HOME', str(Path.home()/'.cargo')),
                       RUSTUP_HOME=os.environ.get('RUSTUP_HOME', str(Path.home()/'.rustup')))
            repo = base / 'repo'
            repo.mkdir()
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            (repo/'Cargo.toml').write_text('[package]\nname="cache_probe"\nversion="0.1.0"\nedition="2021"\n')
            (repo/'src').mkdir()
            (repo/'src/main.rs').write_text('fn main() { println!("cache intact"); }')
            subprocess.run(['git', '-C', str(repo), 'add', '.'], check=True)
            subprocess.run(['git', '-C', str(repo), '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'fixture'], check=True)
            other = base/'other'
            subprocess.run(['git', '-C', str(repo), 'worktree', 'add', '-q', '--detach', str(other)], check=True)
            def run(*args, cwd=repo):
                return subprocess.run([sys.executable, str(SCRIPT), *args], cwd=cwd, env=env, text=True, capture_output=True, check=True)
            reports = [run('report', cwd=tree).stdout.splitlines()[1:3] for tree in (repo, other)]
            self.assertEqual([line.split(' bytes:')[0] for line in reports[0]], [line.split(' bytes:')[0] for line in reports[1]])
            for tree in (repo, other):
                run('run', '--target', 'probe', '--', 'cargo', 'build', '--offline', cwd=tree)
            cached = run('run', '--target', 'probe', '--', 'cargo', 'build', '--offline', '-v', cwd=other)
            self.assertIn('Fresh cache_probe', cached.stderr)
            targets = list((base/'home').rglob('targets/probe-*'))
            self.assertEqual(len(targets), 2)
            import hashlib
            key = 'probe-' + env.get('NFPROGRESS_BUILD_PROFILE', 'development') + '-' + hashlib.sha256(str(other.resolve()).encode()).hexdigest()[:16]
            target = next(p for p in targets if p.name == key)
            self.assertEqual(subprocess.check_output([str(target/'debug/cache_probe')], text=True).strip(), 'cache intact')
            (other/'src/main.rs').write_text('fn main() { println!("other revision"); }')
            run('run', '--target', 'probe', '--', 'cargo', 'build', '--offline', cwd=other)
            self.assertEqual(subprocess.check_output([str(target/'debug/cache_probe')], text=True).strip(), 'other revision')
            run('run', '--target', 'probe', '--', 'cargo', 'build', '--offline', cwd=repo)
            original = next(p for p in targets if p != target)
            self.assertEqual(subprocess.check_output([str(original/'debug/cache_probe')], text=True).strip(), 'cache intact')
            self.assertEqual(subprocess.check_output([str(target/'debug/cache_probe')], text=True).strip(), 'other revision')
            run('run', '--target', 'probe', '--', 'cargo', 'build', '--offline', '--release', cwd=other)
            self.assertTrue((target/'release/cache_probe').is_file())
            report = run('clean', '--days', '1', '--apply')
            self.assertTrue(target.exists())
            os.utime(target/'.worta-cache', (1, 1))
            import fcntl
            with (target.parent.parent/('locks/' + key + '.lock')).open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.assertIn('ACTIVE', run('clean', '--days', '1', '--apply').stdout)
                self.assertTrue(target.exists())
            (target/'keep.dmg').write_text('installer fixture')
            run('clean', '--days', '1', '--apply')
            self.assertTrue(target.exists())
            (target/'keep.dmg').unlink()
            self.assertIn('CLEANABLE', run('clean', '--days', '1').stdout)
            self.assertTrue(target.exists())
            run('clean', '--days', '1', '--apply')
            self.assertFalse(target.exists())
            self.assertTrue(other.exists())


if __name__ == '__main__':
    unittest.main()
