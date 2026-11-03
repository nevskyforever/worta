# Local build caches

Use `python3 scripts/maintenance/build_cache.py report` to report managed Cargo
targets and the standard Cargo registry/npm cache locations. `Run Tauri.sh`
uses the managed target automatically. For checks:

```
python3 scripts/maintenance/build_cache.py run --target aarch64-apple-darwin -- cargo test --manifest-path frontend/src-tauri/Cargo.toml --target aarch64-apple-darwin
```

The root is `~/Library/Caches/worta/build` on macOS (`~/.cache/worta/build`
on Linux). Targets are partitioned by architecture, NFPROGRESS_BUILD_PROFILE (default development),
and a hash of the canonical worktree path.
Cargo separates debug/release profiles. Sharing a target across different source
paths was rejected after an isolated test reproduced reuse of a wrong executable.
The registry and npm download caches remain shared between worktrees. The runner serializes access
per target, including cleanup, and disables incremental output. A busy
cache is refused, never deleted. No additional dependency cache is copied.
Cargo registry uses CARGO_HOME (normally ~/.cargo); npm uses its configured cache.
node_modules remain independent, without shared links.

`python3 scripts/maintenance/build_cache.py clean --days 30` previews stale
managed targets. Add `--apply` to delete the listed inactive targets. The last-use
marker is refreshed before and after each command. Targets containing app bundles,
DMG or ZIP installers are retained. Direct Cargo invocations must not write into
managed targets: use the runner so cleanup and builds share the same lock.

Release workspaces retain separate targets because their bundles are inputs to
release provenance checks; existing release scripts and installer retention remain
unchanged. The existing `clean_generated.py` audits worktree-local legacy outputs;
its `--apply` requires stopping builds and reviewing the exact targets first.
It must never be applied in another active worktree. Neither cleanup tool removes
Git worktrees, user profiles, documents, backups, global registry or npm caches.
