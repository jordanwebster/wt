# Specification notes

- SPEC §11.1 says lock liveness is consulted only outside `ready` and `failed`,
  while the `ready + verify_pending` table row distinguishes held from unheld.
  Core follows the prose and §13.2: the phase stays `ready` with
  `VERIFY_PENDING`, even while a shared door lock is held.
- SPEC §5.2 makes shell-form commands opaque `sh -c` text while resolve-time
  checks still need to recognise valid `$WT_*` occurrences inside recipes.
  Core uses a tolerant reference scan for shell text and strict Template
  validation only for argv elements, `env`, `name`, and file content.
- `Assembly::alias` ignores a contributed `PATH` defensively so invalid
  resource data cannot erase the declared-bin prefix. Valid configuration can
  never contribute `PATH`, because task environment keys reject it.
