# WHAT CHANGED (author narrative)

Three changes on branch `ux-sessions`, base `a6790c3`.

## 1. Every command speaks to a person

`wt` had a human rendering for six of its commands and printed a
pretty-printed JSON payload for the rest, including `register` — the first
command anyone runs — and `doctor`. Every command now renders as a headline
plus aligned facts, with an optional `next` line carrying the actionable
step. The JSON fallback in `src/app/mod.rs` is deleted so the class of
defect cannot return. `--json` keeps the same envelope schemas, field sets and ordering; six
committed envelope values change because the behaviour they describe
changed. Human format does not depend on whether output is a terminal.

Expected-state notices moved into that rendering: a tree that has never
been built reports its missing `bin` directory as a next step rather than a
warning printed above the output. Notices that report something wrong still
go to stderr as `wt: CODE — message`. For text-mode `run`, wt's guidance is
strictly on stderr and never contaminates the child's stdout.

A pseudo-terminal harness runs the built binary, captures the bytes a
developer would see, normalises volatile values and snapshots them.

## 2. Creating a tree lands you inside it

`wt new` provisions a tmux session and attaches, so the tree is where you
end up rather than somewhere you then navigate to. `wt open` no longer
refuses when no agent is configured; a session with no agent runs the same
shell `wt shell` picks. Sessions are provisioning; agents are work, so an
agent recipe runs only when wt creates a session — never on attach — and
only when configured. `wt new` lost `--agent` and gained `--no-open` and
`--no-attach`; `wt open --agent` remains. `wt open --all` now covers every
live tree and resumes a recorded agent through its `resume` recipe instead
of covering only trees with agents. Per-tree failures are retained in batch
data, later trees are still attempted, and the final exit reflects the worst
outcome.

Sessions are configured, not detected: `session.backend` (`tmux` or
`none`), `session.attach`, and an optional `session.agent`, replacing the
top-level `default_agent`, which is now a rejected setting with a remedy.
`register` resolves the backend once and writes it, and a home that
predates this change resolves it on first use rather than defaulting to
no sessions. With `backend = "none"`,
`open` and `close` refuse with a message naming the setting, and `list`,
`remove` and `prune` spawn no tmux process at all.

Attachment happens only when both stdin and stdout are terminals, without `--json`, with
`session.attach` on, and only when `WT_ACTIVATION` is unset, so an agent
already working inside a door never has its terminal captured. The session's
inner command remains `wt exec --no-gate`, preserving the gate hand-off.

## 3. Two defects and a missing repair

Adapter configuration was missing from the layer merge used when creating a
tree, so adapter-declared seeds and sync inputs never arrived: new Rust
trees got no cloned `target` and paid a cold build. Adapter defaults are now
reflink-only: when cloning is unavailable they are skipped rather than copied.
A canonical checkout
that lost its `.wt/tree_id` marker had no supported repair; `wt register
--repair` restores the marker and re-renders generated files without
touching coordinates, resources or tombstones.

wt no longer sets a tmux status line, and the inert `session.status_bar`
setting is removed with it; a wt-owned status line is deferred work.

## Specification

`spec/SPEC.md` §5.4, §9.4, §11.2 and §14 describe the new behaviour, and
the requirements addendum records the decisions behind it. `README.md`'s
tour shows the flow that now exists.
