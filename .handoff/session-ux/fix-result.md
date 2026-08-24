# Session UX fix-pass result

Branch: `ux-sessions`

## 1. Adapter seeds are reflink-only

Adapter-contributed seeds now follow a clone-or-skip path keyed by
`config.adapter_seed`. A failed clone removes any partial destination, records
no materialization, and emits info notice `SEED_SKIPPED_NO_REFLINK` with the
path and failure reason. Repository-declared `seed` retains its reflink-first,
byte-copy fallback and `SEED_COPIED_NOT_CLONED` notice.

SPEC §11.7 and §12 name the distinction and new notice. A1 conditionally
observes the integration outcome on the host filesystem and directly forces the
non-cloning filesystem decision through the failpoint because no separate
non-reflink filesystem was mounted here.

Commit: `7c2c4f3 Keep adapter seeds reflink-only`.

## 2. `new` retains a successfully created tree when sessions fail

Session provisioning failure after tree creation is converted to warning notice
`SESSION_CREATE_FAILED`, whose remedy is `wt open <target>`. `new` exits zero,
prints the completed tree summary, and retains the full `NewData` payload in
both human and JSON routes. Contract tests assert the path, ready phase, warning
level, remedy, session absence, exit code, and JSON data.

Commit: `6f0e55c Keep tree results when sessions fail`.

## 3. One `next` row on every route

`Output::with_notices` de-duplicates notices by `(code, subject)` before either
renderer sees them. C3 now runs `new` through equivalent terminal and redirected
homes and compares normalized output byte-for-byte; the retained capture has
one `next` row.

Commit: `6f0e55c Keep tree results when sessions fail`.

## 4. `run` never writes wt guidance to child stdout

Text-mode `run` sends wt notices and `next` guidance to stderr. A regression
test redirects the child stdout and requires its exact payload (`1.2.3`) with
no wt-authored bytes. JSON continues to carry notices in the envelope.

Commit: `cbb962e Make session guidance and settings honest`.

## 5. `open --all` contains per-tree failures

Each tree is entered and provisioned independently. Failures become structured
session outcomes containing target, session name, error code, message, and
remedy; later trees are still attempted. The command retains all outcomes in
`data`, returns the worst error only after the loop, and keeps the warning
notice. B6 captures a three-tree batch whose middle `TREE_REPLACED` failure no
longer prevents the last session from being created. Its gap explicitly says a
per-tree `LOCK_TIMEOUT` was not injected.

Commit: `6f0e55c Keep tree results when sessions fail`.

## 6. Existing homes resolve a backend on first session use

An absent `session.backend` is resolved and persisted on the first `new`,
`open`, or `close`, as well as during fresh registration. The selection is
announced once on stderr as `sessions: tmux <version> (set session.backend to
change)` or its `none` counterpart; subsequent session verbs do not probe
again. `doctor` reports `SESSION_BACKEND` with the effective backend. Tests
remove a registered home’s config, exercise first-use resolution, verify the
write and announcement, then verify silence on the second use.

Commit: `cbb962e Make session guidance and settings honest`.

## 7. Review lows

- Doctor counts now use `error/errors`, `warning/warnings`, and `note/notes`
  correctly; affected goldens were updated.
- A valid inline `session = { ... }` table is refused before mutation with an
  actionable remedy to rewrite it as `[session]`. This uses the explicit
  “edit or refuse” choice from the brief and never appends a duplicate table.
- Attachment requires both stdin and stdout to be terminals.
- Empty config arrays and maps both render as `-`; trailing row whitespace was
  removed from the goldens.
- `BIN_DIR_MISSING` carries an internal structured target/path/build-task hint.
  Neither renderer parses its English message, and repositories without a
  declared `build` task are not told to run one.
- Private-tmux polls sleep for a bounded 20 ms between client processes while
  retaining their existing deadlines.

Commit: `cbb962e Make session guidance and settings honest`.

## 8. Sync reports skipped nodes as skipped

The human summary counts `present` outcomes as skipped rather than folding them
into the denominator of a “passed” fraction. The former `sync 1/2 passed` case
now reads `sync 1 passed, 1 skipped`, and its snapshot asserts that wording.

Commit: `cbb962e Make session guidance and settings honest`.

## 9. Removed inert `session.status_bar`

The setting, parser field, and unused tmux setter are gone. SPEC §5.4 no longer
declares it; §9.4 and addendum A38 say that a wt-owned status line is deferred
and no current setting controls one. The production status line was not
reinstated.

Commit: `cbb962e Make session guidance and settings honest`.

## Specification and documentation

`98361ea Specify resilient session behavior` updates SPEC, addendum, README,
and snapshots for the corrected behavior. The retained narrative now describes
the same constraints: six JSON value changes rather than an unchanged envelope,
contained batch failures, nonfatal session provisioning, stderr-only `run`
guidance, reflink-only adapter seeds, first-use backend resolution, and the
deferred status line.

## Proof repair

`a7326bb Retain honest session UX proofs` adds opt-in observation points to the
tests, retains captures A1–E2 under `captures/`, and rewrites every demonstration
to quote observed bytes with the exact regeneration command. B2 includes the
wrapped command echo. B6 proves zero clients on a non-JSON `open --all` route
and separately captures partial failure data. C2 limits its claim to unchanged
schema/field set/ordering and lists the six changed envelope values. C3 compares
terminal and redirected `new` output. Every reviewer-identified undeclared gap
is now stated in its own proof.

D1, D2, D3, E1, E2, and E3 are present. E3 is explicitly an unrun operator
recipe using a scratch `WT_HOME` and disposable real-repository clones; it has
no fabricated capture.

## Verification

All required gates passed after the final proof instrumentation:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test --workspace --features failpoints`

The complete raw output is retained in `captures/E1.txt`. The real private-tmux
`session_flow` tier also passed three consecutive runs, retained in
`captures/E2.txt`.

No command in this pass read or wrote `~/.wt` or any operator repository under
`~/source`; all product tests used temporary fixture homes and repositories.

## Reviewer disposition

I found no substantive reviewer finding to dispute. The reviewer correctly
noted that `session.status_bar` was already inert before this branch and did not
present that historical condition as a new defect. For the inline-table low, I
selected the brief’s permitted actionable-refusal outcome rather than attempting
a general TOML-preserving inline-table rewrite.
