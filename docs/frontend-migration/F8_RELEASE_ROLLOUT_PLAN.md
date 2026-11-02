# nfprogress — F8 release rollout plan

F10 дополняет план явным переходом helper → installer → Tauri. Это план подготовки, не команда публикации. `update_manifest.json` и
`update_manifest_legacy.json` намеренно не изменяются в F8/F9.

## Artifacts and exact later changes

For a future release, publish only after F10 qualification:

- signed macOS ARM artifact and its SHA-256;
- signed macOS Intel artifact and its SHA-256;
- signed Windows x64 NSIS installer, `.sig` and Tauri `latest.json`;
- release notes and a checksum list;
- separately packaged migration helper/bridge artifact, if that is the chosen
  legacy strategy.

For the 5.3.11 transition, legacy clients keep their existing
`nfproject.ru/app` update source and receive compatible release files there.
New WORTA clients use `worta.ru/app` for updater manifests and payloads; the
landing page reads `worta.ru/releases.json`. GitHub Releases remains the
official release record and downloadable archive. Publish through the existing
`scripts/upload-release.sh`, using `/home/n/nevskyfore/nfproject/public_html/app`
for legacy assets, `/home/n/nevskyfore/worta/public_html/app` for new updater
assets, and `/home/n/nevskyfore/worta/public_html` for the download catalog.
Update each generation's manifest independently and verify the hosted files
before publishing the GitHub Release.

The release config must continue to use identifier `app.nfprogress.tracker`,
Tauri updater signatures, Windows `x86_64-pc-windows-msvc`, and the existing
macOS ARM/Intel targets. No private signing key belongs in the repository.

## Sequence

1. Complete the P0 gates in `F8_RELEASE_QUALIFICATION.md`.
2. Produce fresh artifacts from the release commit and prove the embedded
   source revision equals that commit.
3. Rehearse the local helper and restore flow on copies of representative real
   profiles.
4. Release the transitional legacy version to developer/private testing.
5. Release the signed helper/installer to a small tester cohort with manual support coverage.
6. Expand to a wider cohort after migration/restart/restore evidence is clean.
7. Enable the Tauri updater for migrated users, then enable general availability and update remote manifests.

No telemetry is introduced by this plan. Support should collect only versions,
state codes, counts and IDs unless the user explicitly enables debug logging;
raw note/document bodies are excluded.

F9 helper invocation is explicit (`nfprogress-migration-helper detect|preview|
prepare|verify --source <data-root>`). It is never spawned by Tauri startup.
The helper is a separate artifact, not a Tauri sidecar, and does not require
external Word/Scrivener assets for internal migration.

## Rollback

Stop the rollout and revert the remote manifest if a P0/P1 issue appears.
Preserve existing data and all pre-upgrade backups. Do not ask users to launch
an old Python app against a schema it cannot read. A safe rollback is either:

- reinstall the prior app while retaining its unchanged pre-migration profile;
- restore the matching pre-migration backup, then reinstall the prior app;
- keep the new SQLite and fix the new app when the schema is already migrated.

Downgrade is unsupported after schema migration unless a tested matching backup
exists. The release owner must not delete rollback artifacts during the initial
rollout window.

## Current readiness decision

F8 does not authorize publication. Local ARM/Intel qualification artifacts were
built and inspected, but they are unsigned qualification outputs; Windows was
not built locally and signing readiness is configuration-only. Release artifacts
must still be rebuilt and signed by the release pipeline from the final release
commit. The workflow contains a publish job, but F10 makes it manual-only and
places it behind the protected `nfprogress-production` environment. The manual
qualification workflow never publishes.

## F11 evidence addendum

F11 confirms that the owner currently accepts unsigned/ad-hoc/self-signed
distribution. Developer ID, notarization, and Authenticode are therefore
known `P1` trust limitations, with expected Gatekeeper and SmartScreen
warnings, rather than standalone release blockers. Functional qualification
still requires the complete helper → installer → Tauri first-launch/restart
path on macOS ARM64 and Windows x64. macOS Intel is deferred and is not part
of this initial rollout wave. See
`F11_PLATFORM_QUALIFICATION.md`, `F11_TRANSITION_REHEARSAL.md`, and
`F11_RELEASE_GATE_CHECKLIST.md` for the evidence and open gates.
