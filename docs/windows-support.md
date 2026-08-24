# Windows support

`wt` is POSIX-only today. Under WSL it behaves exactly as it does on Linux,
and under Git Bash or MSYS2 much of it works because the shell there is a
POSIX shell. Native Windows is unimplemented.

This note records what a native port would involve, for anyone who wants to
attempt it. It assumes tmux-backed sessions (`wt open`) are out of scope —
`wt open` already degrades to running in the foreground and saying so when
tmux is absent, so dropping it costs no design.

## What already ports

`wt-core` holds every decision the tool makes: coordinates and port
allocation, environment assembly, the resource state machine, tree
lifecycle, config parsing and merging, adapters, and all report and JSON
shapes. It is lint-enforced against `fs`, `process`, `env`, `net` and the
clock (SPEC §15), which was done for testability but has the side effect
that no platform assumption lives there. It compiles and passes its tests
anywhere Rust runs.

In `wt-sys`, `net.rs` (the bind-and-connect port probe) is portable as
written, and `git.rs` is mostly portable because it shells out to `git`,
which behaves the same on Windows.

## What has to be written

Every system call lives in `wt-sys`, about 4,700 lines:

| File | Lines | What Windows needs |
|---|---|---|
| `fsx.rs` | 1,468 | The hardest. Around 38 call sites of `openat`, `fstatat`, `unlinkat` and `O_NOFOLLOW`. Windows can open without following a reparse point (`FILE_FLAG_OPEN_REPARSE_POINT`, plus `FILE_FLAG_BACKUP_SEMANTICS` for directories), but this is a rewrite, not a shim. The `0600` privacy of the state store needs ACLs; there is no umask. `fclonefileat`/`FICLONE` has no NTFS equivalent, and the read-write fallback already exists. |
| `proc.rs` | 668 | See "Doors are an exec" below. |
| `lock.rs` | 567 | The cleanest port. `flock` becomes `LockFileEx`, and the central idea survives intact: a handle released by process death is the liveness test, so a crashed holder never wedges a lock (SPEC §13.1). |
| `snapshot.rs` | 508 | The executable inventory that stops a teardown recipe reaching an installed binary (SPEC §10.3) asks whether a mode bit is set. On Windows it must consider `PATHEXT`, and record both `tool.exe` and the stem `tool`, because recipes name the bare word. Small, and it is the guard that protects real daemons and containers on a developer's machine — worth getting right first, not last. |
| `tmux.rs` | 234 | Out of scope by assumption. |

Note that the only platform seam in the codebase today is a pair of `cfg`
attributes selecting between two implementations of `reflink_at`, one using
`fclonefileat` and one `FICLONE`. There is no third arm, so the crate does
not compile at all on a target that is neither macOS nor Linux. There is
also no platform abstraction to implement against: the first task is
introducing one inside `wt-sys` and re-expressing the existing POSIX path
through it, before any Windows code is written.

## Three assumptions that are not system calls

These are why a port is a design fork rather than a compile fix.

**Doors are an exec.** `wt shell`, `wt exec` and `wt open` end by replacing
the `wt` process with the target (SPEC §9.1). Windows has no `exec`. The
alternative is spawning a child and leaving `wt` in the middle as a parent,
which changes process trees, exit-code propagation and Ctrl-C handling, and
invalidates the door cost budget (SPEC §0) that pins how many processes a
door may spawn.

**The tree lock rides through that exec in an inherited descriptor.** The
door hands the tree and door lock descriptors across `execvp` so the lock is
held for the whole life of the shell, which is what lets `wt remove` refuse
with `TREE_IN_USE` and name the shell someone is sitting in. Windows can
inherit handles into a child it spawns, so this works — but it works
*because* of the change above, not around it.

**Every string recipe is `sh -c`.** A `run`, `exists` or `destroy` written
as a string is handed to `sh` (SPEC §5.2), and repository configurations use
POSIX idioms freely: `>/dev/null 2>&1`, `||`, command substitution,
`xargs -r`. This is the configuration surface rather than the
implementation, so no amount of `wt-sys` work avoids it. It is the change
that would actually be felt by users.

## A sketch for per-platform configuration

Configuration already has four layers with per-key merge rules and `false`
to delete an inherited entry (SPEC §5.1–5.3). A platform variant fits as an
overlay *within* a layer, which needs no new merge machinery:

```
Config     := Scope & { ports?, dirs?, seed?, sync_inputs?, detect?,
                        platform?: Map<PlatformId, Config> }
PlatformId := "windows" | "macos" | "linux" | "unix"
Scope      := { bin?, env?, copy?, files?, task?, adapters?, shell?: Cmd }
```

Two additions: a `platform` map whose values are ordinary configurations,
and `shell` — the interpreter a bare-string recipe is handed to, defaulting
to `["sh", "-c"]` on Unix and `["cmd", "/C"]` on Windows. A repository that
prefers POSIX semantics everywhere can set `shell = ["sh", "-c"]` under
`[platform.windows]` and document Git Bash as a prerequisite; the choice is
then visible in `wt config` instead of implied.

Resolution: within each layer, resolve the base scope, then apply matching
platform overlays family-first (`unix` before `macos`) using that layer's
own merge rules. Then move to the next layer. Platform is a *variant*, not
an *authority*: a repository's `[platform.windows]` block does not outrank a
plain entry in the user's `config.toml`. Deciding this the other way makes
"later layers win" untrue and the effective configuration hard to predict.

Because only the differing leaf is restated, the common case is one line:

```toml
[task.daemon]
tied_to = "tree"
needs = ["build"]
exists = "orbit list >/dev/null 2>&1"
run = "orbit server start"
destroy = "orbit server stop"

[platform.windows.task.daemon]
exists = "orbit list > NUL 2>&1"
```

`tied_to`, `needs`, `run` and `destroy` are inherited. A task that cannot
work on a platform is deleted there with `false`. A recipe that differs
wholesale — one deriving a hash and removing containers, say — is written
once per platform and the POSIX version is left untouched.

An individual on Windows does not have to wait for a repository to adopt
any of this: the tree overlay `<tree>/.wt/config.toml` is layer 3, so a
recipe can be fixed locally, on one machine, without touching the
repository's own `.wt.toml`.

The whole of this resolution lives in `wt-core`, which is pure. What a
configuration means on Windows is therefore unit-testable from any
platform, including CI runners that are not Windows — so this part can be
built and proven long before the `wt-sys` port is finished. That argues for
landing `platform` and `shell` first: they are independently useful for
repositories shared between macOS and Linux, and they turn the Windows
question into "port `wt-sys`" rather than "port `wt-sys` and redesign the
configuration surface at the same time."

## What the sketch does not solve

- **Path separators.** `$WT_ROOT/.wt/orbit/config.yaml` expands to
  `C:\src\orbit/.wt/...`. Mixed separators are accepted by most Windows
  tooling and fatal to some. Normalising them would be wrong under Git
  Bash, where forward slashes are wanted, so expansion should stay verbatim
  and the overlay should carry the exceptions. This is a documentation
  burden whichever way it goes.
- **Line endings in rendered files.** Needs an `eol = "crlf" | "lf"` key on
  a `files` entry, overridable per platform.
- **`mode`** on a rendered file becomes a no-op, and rendered secrets are
  only as private as the ACLs make them.
- **Symlinks**, used by some repositories' own tasks, require Developer
  Mode or elevation.
- **Agent sessions** attach through a pty; the equivalent is ConPTY.
- **`shell-init`** emits shell functions for POSIX shells and would need a
  PowerShell profile equivalent.

CI would need a `windows-latest` job from the first commit of such an
effort, or the port rots between contributions.
