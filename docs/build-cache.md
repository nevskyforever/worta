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
managed targets. Add `--apply` to delete the listed inactive targets. The runner
also purges oversized targets before and after a command (2 GiB by default,
configurable with `NFPROGRESS_TAURI_TARGET_MAX_MB`). Targets containing app bundles,
DMG or ZIP installers are retained. Direct Cargo invocations must not write into
managed targets: use the runner so cleanup and builds share the same lock.

Release workspaces use separate targets because bundles are inputs to provenance
checks. By default `build-tauri-local.sh` now runs Cargo cleanup on exit, including
failed qualification or packaging. `NFPROGRESS_TAURI_KEEP_BUNDLE=1` preserves only
the bundle at its original path: it is moved aside during `cargo clean` and then
restored without copying the target. It no longer retains all intermediates.
Only explicit `NFPROGRESS_TAURI_KEEP_BUILD_CACHE=1` retains the entire target for
diagnostics after a successful build within the configured size limit. Cargo
incremental output is disabled. Qualification omits Rust debug symbols and
cleans the dev profile before packaging. A per-architecture workspace lock
refuses competing invocations.

ARM and Intel share one npm installation in `tauri-npm/install` under the
cache root. It is rebuilt under a lock when package manifests or the host
Node/npm version change. Each build gets a private copy using APFS copy-on-write
when supported and a normal copy otherwise. The npm download cache and Cargo
registry are already shared. Local development keeps its own `frontend/node_modules`.

On success, failure, or a handled interrupt, the build removes its private
dependency copy, frontend output and temporary directory, including test
databases. Set `WORTA_BUILD_CACHE_DIR` to an absolute path to place the shared
cache on a different volume. Release compilation stays in the worktree, so
use a worktree on that volume too if the system disk is short of space.

The existing `clean_generated.py` audits worktree-local legacy outputs. Review
its exact targets before applying: retained bundles or dependencies may still be
needed. Never apply it in another active worktree. Neither the managed cache
cleaner nor release cleanup removes Git worktrees, user profiles, documents,
backups or global registry/npm caches. Disk-audit measurements for the original
retention bug are recorded in `build-disk-audit-2026-11-04.md`.
