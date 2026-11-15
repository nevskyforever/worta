# Build disk audit — 2026-11-04

All sizes below use GiB (2^30 bytes). No system snapshots, user profiles,
installers, source trees, worktrees or 6.0 outputs were deleted.

## Confirmed causes

* `release-tauri-local.sh` sets KEEP_BUNDLE=1 for qualification. The old
  build-script condition treated this as retaining the complete Cargo target,
  leaving roughly 12 GiB after ARM/Intel qualification in main.
* Cleanup was installed only after compilation and checks, so failed checks
  retained intermediate output. Prior temporary worktrees also retained their
  independent Cargo targets: the newer managed dev cache did not cover these.
* ARM and Intel each compile host/test/release intermediates; these are distinct
  Cargo profiles, not user documents. Qualification did not explicitly disable
  incremental compilation. Stable frontend workspaces reuse dependencies; they
  are not new full copies on each invocation. Two isolated node_modules are
  required for parallel frontend builds. Registry/npm download caches are shared.
* Release-manifest temporary directories and releases JSON files were not covered
  by the release cleanup trap. These are a small secondary leak, now covered.

## Largest worktrees before cleanup

Nested Desktop/nfprogress aggregate directories are excluded to avoid counting
worktrees twice.

| Worktree | Before GiB | Treatment |
|---|---:|---|
| /private/tmp/worta-5312-publication | 12.727 | Remove confirmed inactive Cargo intermediates only |
| /private/tmp/worta-5311-from-6 | 11.562 | Same; this is the old codex/worta-5.3.11 tree, not active 6.0 |
| /Users/romankisockin/Desktop/nfprogress/ts_migration | 8.434 | 6.0 protected; unchanged |
| /private/tmp/worta-f7-port | 4.059 | Remove inactive Cargo intermediates only |
| /Users/romankisockin/Desktop/nfprogress/mfprogress_main | 3.381 | Remove inactive Cargo intermediates only |

Other retained caches: ~/Library/Caches/nfprogress 2.864 GiB;
~/Library/Caches/worta 1.924 GiB; ~/.npm 0.838 GiB; ~/.cargo 0.325 GiB.
Old test profiles (~1.34 GiB across three prominent /tmp profiles) contain SQLite
and remain protected. Mounted old DMGs and all release archives were retained.

## Exact cleanup scope

Paths were literal directories, ignored and untracked by their owning Git tree.
Open-file/CWD checks found no use of the selected directories; active Cargo
processes were checked before deletion. Only Cargo debug/host-release directories
and target-release deps/build/incremental/.fingerprint/examples were removed.
Bundle directories and architecture release executables were preserved. Candidates
were scanned for profiles, SQLite, backups and installers before deletion.

| Owning tree | Removed allocated GiB |
|---|---:|
| /private/tmp/worta-5312-publication | 11.058514 |
| /private/tmp/worta-5311-from-6 | 9.837696 |
| /private/tmp/worta-5311-release-build | 1.878826 |
| /private/tmp/worta-f7-port | 3.557098 |
| /Users/romankisockin/Desktop/nfprogress/mfprogress_main | 2.358341 |

Exact removed paths (not broad worktree deletion):

- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/intel/frontend/src-tauri/target/debug` — 1244061696 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/intel/frontend/src-tauri/target/release` — 687386624 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/debug` — 3136942080 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/deps` — 866177024 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/build` — 12042240 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/incremental` — 0 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/.fingerprint` — 4239360 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/examples` — 0 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/arm/frontend/src-tauri/target/debug` — 1245093888 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/arm/frontend/src-tauri/target/release` — 687947776 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/debug` — 3112185856 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/deps` — 862937088 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/build` — 10735616 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/incremental` — 0 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/.fingerprint` — 4239360 allocated bytes
- `/private/tmp/worta-5312-publication/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/examples` — 0 allocated bytes
- `/private/tmp/worta-5311-from-6/frontend/src-tauri/target/debug` — 7082098688 allocated bytes
- `/private/tmp/worta-5311-from-6/frontend/src-tauri/target/aarch64-apple-darwin/debug` — 240799744 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/intel/frontend/src-tauri/target/release` — 725807104 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/deps` — 926433280 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/build` — 19791872 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/incremental` — 0 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/.fingerprint` — 4345856 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/examples` — 0 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/arm/frontend/src-tauri/target/release` — 687382528 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/deps` — 861536256 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/build` — 10715136 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/incremental` — 0 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/.fingerprint` — 4235264 allocated bytes
- `/private/tmp/worta-5311-from-6/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/examples` — 0 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/intel/frontend/src-tauri/target/release` — 375468032 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/deps` — 168886272 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/build` — 225280 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/incremental` — 0 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/.fingerprint` — 1630208 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/intel/frontend/src-tauri/target/x86_64-apple-darwin/release/examples` — 0 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/arm/frontend/src-tauri/target/release` — 705515520 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/deps` — 756547584 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/build` — 5111808 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/incremental` — 0 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/.fingerprint` — 3989504 allocated bytes
- `/private/tmp/worta-5311-release-build/.tauri-build-workspaces/arm/frontend/src-tauri/target/aarch64-apple-darwin/release/examples` — 0 allocated bytes
- `/private/tmp/worta-f7-port/frontend/src-tauri/target/debug` — 3819405312 allocated bytes
- `/Users/romankisockin/Desktop/nfprogress/mfprogress_main/frontend/src-tauri/target/debug` — 2532249600 allocated bytes

## Physical result on the same APFS Data volume

Immediately before cleanup: 10613735424 available bytes
(9.884811 GiB).
Immediately after cleanup and sync: 20475998208 bytes
(19.069759 GiB).
**Measured gain: 9.184948 GiB.**
Removed allocated-file sum: 28.690475 GiB.
These are different measurements; the removed-file sum is not claimed as physical gain.
A later reading after checks was 19.006702 GiB free.
Other live processes can change volume availability between readings.

## APFS findings and limits

Data is disk3s5 in the shared internal disk3 container. System, Preboot, Recovery
and VM consume space in that same container: subtracting Data usage alone from
container size does not give available space. External Files/Games/Backup belong
to another container and were not cleanup targets.

Two purgeable Time Machine snapshots remain, dated 2026-11-03 19:48:45 and
20:34:06. Snapshot retention, shared extents/clones and retained hard-linked
outputs can make summed directory blocks exceed reclaimed physical blocks.
The snapshots' exact exclusive retained bytes and clone extents were not exposed
by the read-only CLI inspection, so the difference is NOT attributed entirely
to snapshots. No deleted-but-open WORTA file was found among accessible processes.
No snapshot or backup was deleted. Further snapshot removal requires separate
owner authorization; it is not necessary for the measured 9+ GiB gain.

## Prevention and checks

The build script now installs EXIT cleanup before qualification, disables Cargo
incremental output, and locks each architecture workspace. KEEP_BUNDLE preserves
only the bundle by moving it aside during standard cargo clean, then restoring
its original path. Full target retention requires explicit KEEP_BUILD_CACHE=1.
Packaging stages the ZIP before replacing the previous installer.

Focused tests cover parallel ARM/Intel workspaces, two repeated builds with only
one npm install, early qualification/packaging failures, active-workspace refusal,
installer retention, and two tiny real Cargo build/cleanup cycles. Packaging is
stubbed in that integration check; no full ARM/Intel Tauri build or GUI test was
run. Publication, versions and Actions were not changed or triggered.
