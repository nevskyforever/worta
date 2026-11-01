# Local acceptance

Run `scripts/maintenance/local_acceptance.sh` from any directory in the
checkout:

- `quick` checks the diff, changed Python syntax, shell syntax, and workflow
  YAML.
- `backend` starts a uniquely named PostgreSQL 16 Docker container bound only
  to IPv4 loopback, then runs focused schema, legal, sync, and backend
  regressions plus the real encrypted-blob backup/restore preflight.
- `frontend` runs the historical metadata Vitest set, typecheck, production
  build, and the local Chromium stages, account, and encrypted-workspace
  acceptance flows.
- `prepush` runs the static, frontend, and backend modes in sequence.

The backend modes require Docker Engine and Python test dependencies. Each run
generates a new PostgreSQL password locally, keeps the database DSN in the
process environment, and stops only the task container after verifying its
ownership label and image. It does not create named volumes or use a configured
application database. The frontend modes require the frontend npm dependencies
and Playwright Chromium; browser tests use temporary profiles and local API
fixtures.

For automated local UI testing only, the initial local agreement screen can be
skipped without writing an agreement setting. Create a disposable directory
inside the system temporary directory and an empty `.worta-e2e-isolated` file
inside it. Start the Vite development server (including Tauri's dev server)
with `NFPROGRESS_DATA_DIR` set to that directory, `NFPROGRESS_ENV=test`,
`NFPROGRESS_BUILD_PROFILE=test`, and `WORTA_E2E_SKIP_LEGAL_GATE=1`. The
browser must access it through `localhost` or `127.0.0.1`. The skip is compiled
out of builds and is disabled unless every condition is met. Use only a local
test backend and disposable data. This does not bypass cloud account legal
checks, authentication, encryption, or any server-side security gates; it does
not record acceptance of any document.

On Apple Silicon, Docker runs the PostgreSQL image for the host architecture.
This does not establish Linux x86-64 Chromium compatibility; report that check
separately unless verified in an isolated x86-64 Linux environment.
