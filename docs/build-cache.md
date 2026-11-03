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

Release workspaces use separate targets because bundles are inputs to provenance
checks. By default `build-tauri-local.sh` now runs Cargo cleanup on exit, including
failed qualification or packaging. `NFPROGRESS_TAURI_KEEP_BUNDLE=1` preserves only
the bundle at its original path: it is moved aside during `cargo clean` and then
restored without copying the target. It no longer retains all intermediates.
Only explicit `NFPROGRESS_TAURI_KEEP_BUILD_CACHE=1` retains the entire target for
diagnostics. Cargo incremental output is disabled for qualification as well as
packaging. A per-architecture workspace lock refuses competing invocations.

The fixed ARM/Intel workspaces and their node_modules are reused, rather than
created per invocation; an unchanged package lock does not rerun npm ci. Their
separate node_modules are necessary for independent parallel frontend builds and
are never linked together. The registry and npm download caches stay shared.

The existing `clean_generated.py` audits worktree-local legacy outputs. Review
its exact targets before applying: retained bundles or dependencies may still be
needed. Never apply it in another active worktree. Neither the managed cache
cleaner nor release cleanup removes Git worktrees, user profiles, documents,
backups or global registry/npm caches. Disk-audit measurements for the original
retention bug are recorded in `build-disk-audit-2026-11-04.md`.
