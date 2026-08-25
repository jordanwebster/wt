# Phase 2 result

Built the complete session flow from outline section B plus A2. `wt new` now
provisions a tmux session and, on an eligible terminal, prints its existing
human summary before attaching. Sessions without an agent run the same shell
as `wt shell`; `wt open` no longer requires an agent. Agent recipes run only
when wt creates a session, recorded agents resume, and `open --all` covers
every live tree without attaching. `new` now has `--no-open` and
`--no-attach`, and no longer accepts `--agent`; `open --agent` remains.

Session configuration now declares `backend`, `attach`, and optional `agent`.
Registration resolves an absent backend once and writes the choice. The
removed top-level agent setting fails with a remedy naming `session.agent`.
Backend `none` explicitly refuses `open` and `close`, while `list`, `remove`,
and `prune` invoke no tmux process. The A24 `wt exec --no-gate` hand-off is
preserved, and removal closes shell sessions as well as agent sessions.

The phase-1 PTY harness now gives children a real controlling terminal. The
new tmux tier uses one `tmux -L wt-test-<unique>` server per fixture, kills it
in teardown, drives panes with `send-keys`, polls `capture-pane` with bounded
deadlines, and retains the live environment transcript. CI installs tmux on
both Linux and macOS. Proofs for A2 and B1–B9 are in `proofs/`, each with an
automated test, captured demonstration, replay command, and gap clause.

Deliberately not built: `session.on_create`, tmux window layouts, or a wt
status-line design. The existing `status_bar` setting remains inert. No review
artifact or operator-owned review work was created, and no new item was added
to `filed.md`.

Review hardest: the post-summary deferred attach path in `src/app/mod.rs` and
`src/app/new.rs`; agent start/resume precedence and concurrent-session loser
handling in `src/app/open.rs`; preservation of existing TOML when registration
inserts the backend; and controlling-terminal behavior of the PTY harness on
both CI operating systems.

All required gates passed on 2026-08-24:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test --workspace --features failpoints`
