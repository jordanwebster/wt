# wt

`wt` is a worktree manager for humans and coding agents who need several
copies of one repository running at the same time.

Git worktrees isolate source files. They do not isolate ports, sockets,
databases, containers, generated configuration, local secrets, or the binary
selected from `PATH`. `wt` gives every checkout stable coordinates and makes
the same environment available through tasks, one-shot commands, shells, and
background sessions. When a worktree goes away, `wt` tears down the resources
that belonged to it.

```sh
wt register ~/source/myapp
wt new myapp/fix-login
wt shell myapp/fix-login
wt run test myapp/fix-login
wt remove myapp/fix-login --yes
```

## Why wt

Parallel development becomes unreliable when two checkouts both assume port
`3000`, use the same database name, or launch an installed server instead of
the binary just built in the current tree. Per-repository scripts can patch
one of those problems, but usually provide neither a cross-repository address
book nor reliable cleanup after crashes and out-of-band deletion.

`wt` applies four rules:

1. A tree is addressed as `<label>/<name>` from any directory. The registered
   checkout is the `canonical` tree and can be addressed by its label alone.
2. Ports are allocated once and names, paths, and environment values are
   derived from that identity.
3. Activation is explicit. Outside a `wt` door, the invoking shell and its
   `PATH` are unchanged.
4. A declared resource is probed and destroyed during removal, including a
   resource the application created without running it through `wt`.

## Status

`wt` is at 0.1. It was built against three real repositories — a Rust service
with a per-checkout daemon, a React Native app, and a .NET Aspire stack — and
those shapes are covered by the acceptance suite. It is not yet packaged for
any distribution, and command output and `.wt.toml` keys may still change
between versions.

## Install

```sh
cargo install --git https://github.com/jordanwebster/wt
wt --version
```

Or from a checkout:

```sh
cargo install --path .
wt --version
```

To keep the binary in the repository instead:

```sh
cargo build --release
./target/release/wt --version
```

`wt` supports POSIX systems and requires Git 2.31 or newer. tmux 3.2 or newer
is optional and enables detached sessions. The adapters only require the
tools selected by the repository being used.

## Tour

Register an existing checkout, or clone and register in one operation:

```sh
wt register ~/source/myapp
wt clone https://example.com/acme/service.git --label service
```

Registration is consent for the repository's declared tasks and resource
recipes to run. It also initializes the canonical tree, including its own
ports and rendered files. The canonical checkout is a full participant:

```sh
wt env myapp --dotenv
wt exec myapp -- cargo test
wt shell myapp
```

Create a linked worktree. By default `new` runs the detected or declared
`sync` task; add `--verify` to run the repository's verify plan too.

```sh
wt new myapp/fix-login --from main
wt new myapp/review-42 --from pr:42 --verify
wt list
wt status myapp/fix-login
```

Worktrees normally live under `$WT_HOME/trees/<label>/<name>`. `wt path`
prints the exact root, and `wtcd` from shell initialization changes to it.

### Doors: `run`, `exec`, `shell`, and `open`

Every door computes the same environment, prepends the same declared binary
directories to `PATH`, and renders the same managed files before starting the
child.

```sh
wt run build myapp/fix-login       # declared task, captured log
wt test myapp/fix-login            # aliases: test/lint/fmt/build
wt exec myapp/fix-login -- env     # one-shot command; child's stdio/status
wt shell myapp/fix-login           # interactive, non-login shell
wt which myapp/fix-login myserver  # resolve through the door PATH
```

`exec` and `shell` are passthrough doors: they deliberately do not support
`--json`. Use `wt env --json` to inspect what a child will receive and `wt run
--json` when one machine-readable envelope is required.

`open` creates or enters an agent session with the tree environment active:

```sh
wt open myapp/fix-login --agent codex
wt open myapp/fix-login --agent codex --no-attach
wt open --all
wt close myapp/fix-login
```

With tmux, the session is rooted in the worktree and its status names the
target. Without tmux, an attaching `open --agent …` can run the agent in the
foreground when attached to a terminal; detached and batch forms require
tmux. Configure custom agents in `$WT_HOME/config.toml`:

```toml
default_agent = "codex"

[agents.codex]
start = ["codex"]
resume = ["codex", "resume", "--last"]
```

Remove a linked tree when finished. Destructive commands prompt on a terminal
and require `--yes` when non-interactive.

```sh
wt remove myapp/fix-login --yes
wt prune --yes
```

Removal closes the session, probes every tree-tied resource, destroys present
ones, and then removes the Git worktree. `wt prune` reports or repairs stale
records and out-of-band deletions. It refuses unsafe teardown when a missing
tree's recipe names a binary that used to live in that tree, so it cannot fall
through to an installed binary of the same name.

## `.wt.toml` reference

`.wt.toml` is optional and normally committed at the repository root.
Detected adapters provide useful defaults; project configuration overrides or
extends them.

```toml
ports = ["http", "debug"]
bin = ["target/debug"]
copy = [".env", ".mcp.json"]
seed = ["target"]
sync_inputs = ["Cargo.lock", "Cargo.toml"]

[env]
PORT = "$WT_PORT_HTTP"
MYAPP_CONFIG = "$WT_ROOT/.wt/myapp/config.yaml"

[files.".wt/myapp/config.yaml"]
marker = "#"
mode = "0644"
content = """
name: $WT_NAME_SHORT
socket: $WT_ROOT/.wt/myapp/server.sock
port: $WT_PORT_HTTP
"""

[files.".wt/banner.txt"]
source = "support/banner.txt"

[task.build]
run = "cargo build --workspace"
description = "Build every workspace member"

[task.daemon]
tied_to = "tree"
name = "$WT_NAME_SHORT"
needs = ["build"]
exists = "myserver status >/dev/null 2>&1"
run = "myserver start"
destroy = "myserver stop"
ready_within = "5s"
timeout = "60s"

[task.daemon.env]
MYAPP_LOG = "$WT_ROOT/.wt/myapp/server.log"

[dirs."frontend".task.test]
run = ["npm", "test"]
cwd = "frontend"
lock = "frontend-tests"

[adapters.node]
tool = "pnpm"
```

Top-level keys:

- `ports`: named ports exposed as `WT_PORT_<NAME>`. Existing names retain
  their index when the list is reordered or extended.
- `bin`: relative directories prepended to `PATH` in every door. A missing
  directory produces `BIN_DIR_MISSING` rather than silently hiding the risk
  of an installed fallback.
- `env`: environment aliases. A value of `false` removes an inherited key.
- `copy`: ignored local files or directories copied from the canonical tree
  once when a worktree is created. Tracked sources are refused.
- `seed`: like `copy`, with filesystem cloning attempted for large caches.
- `files`: whole files rendered on every door. Exactly one of `content` or
  `source` is required; `marker` selects the provenance-comment prefix (an
  empty marker disables the header), and `mode` defaults to `0644`.
- `task`: declared tasks and resources.
- `dirs."path"`: a directory scope. Nearby task/env/file settings override
  root settings, while root tasks can compose adapter tasks from subprojects.
- `adapters`: choose a tool or disable an adapter for a scope.
- `sync_inputs`: paths used to decide whether dependency state has drifted.
- `detect`: adapter scan depth and ignored paths.

Commands and templates may be a shell string or an argv array. Templates
support `$NAME`, `${NAME}`, and `$$`. Relative paths may not contain `..` or
escape the tree.

### Tasks and resources

A task accepts `run`, `exists`, `needs`, `lock`, `env`, `cwd`, `timeout`, and
`description`. `exists` exits 0 when present, 1 when absent, and 2 or greater
when infrastructure failure means it cannot tell. `needs` forms an acyclic
task graph. Use `wt tasks` to inspect the effective plan and `wt run --dry-run`
to inspect one execution.

A task with `destroy` is a resource. It also requires `exists` and `tied_to`:

- `tied_to = "tree"` gives each tree its own lifecycle.
- `tied_to = "repo"` shares the resource across the registered repository and
  destroys it during `unregister`, not ordinary tree removal.
- `name` supplies `WT_SELF` to that resource's recipes.
- `snapshot_env` names additional parent variables that teardown must retain.
- `ready_within` polls after `run` until the resource becomes present.

`run` is optional for resources created by the application. Running such a
resource reports it as declared; a later probe can discover it, and removal
will still destroy it. A failed probe never triggers `run` or `destroy`.
`destroy` and `refresh` can drive a resource explicitly.

## Environment rules

The tree identity variables are:

`WT_LABEL`, `WT_NAME`, `WT_NAME_SNAKE`, `WT_NAME_SHORT`, `WT_TARGET`,
`WT_BRANCH`, `WT_ROOT`, `WT_REPO`, `WT_HOME`, `WT_SLOT`, `WT_PORT_BASE`, each
`WT_PORT_<NAME>`, `WT_SESSION`, and `WT_BIN`. `WT_TASK` is set while a task
runs; `WT_SELF` is set in resource recipes. `WT_BRANCH` is the branch observed
when the child starts and does not change inside an already-running shell or
session.

Tool-owned identity keys and the `bin` prefix are always applied. Ordinary
aliases from `[env]` do not replace values already supplied by the user unless
`--force-env` is used. Activation metadata makes entry re-entrant: entering
tree B from a shell activated for tree A first restores A's prior values, then
applies B. A malformed activation marker is ignored with a notice.

```sh
wt env myapp/fix-login             # values plus binary inventory
wt env myapp/fix-login --sh        # shell export/unset statements
wt env myapp/fix-login --dotenv
wt env --deactivate --sh
```

`.wt/` and managed or copied paths are added to the repository's shared
`info/exclude`, so coordination does not require a `.gitignore` change and
`git status` stays clean. If a rendered file no longer matches the recorded
hash, `wt` preserves it and asks the user to remove it before regeneration.

## Adapters

Adapters are declarative built-in knowledge. Detection is scoped for
monorepos, lockfiles choose package managers, and repository configuration
wins over defaults.

| Stack | Detection | Default capabilities |
|---|---|---|
| Rust | `Cargo.toml` | cargo sync/build/test/clippy/fmt |
| Node | `package.json` plus npm, pnpm, Yarn, or Bun lockfile | install and declared package scripts |
| .NET | solution or project files | restore/build/test/format |
| Python | `pyproject.toml`, lockfiles, or requirements | uv, Poetry, or pip workflows |
| Go | `go.mod` | download/build/test/vet/gofmt |
| Git submodules | `.gitmodules` | recursive initialization |

Adapters respect committed manifests. `wt doctor` can recommend accelerators
such as pnpm, uv, or sccache, but never switches tools automatically.

## Automation, JSON, and exit codes

Lifecycle and reporting commands support `--json` and emit one stable
envelope containing `wt.schema`, `wt.version`, `ok`, `command`, `data`,
`notices`, and `error`. Arrays have stable ordering. Environment values appear
only in `wt env`; resource snapshots are never printed.

`exec`, `shell`, and an attaching `open` pass through the child's streams and
status and therefore refuse `--json`. `run --json` keeps stdout for the single
envelope and tees child output to stderr and the task log.

| Exit | Class | Meaning |
|---:|---|---|
| 0 | ok | success, including an idempotent no-op |
| 1 | internal | a `wt` bug |
| 2 | usage | invalid invocation or missing confirmation |
| 3 | not found | target or task does not exist |
| 4 | conflict | branch, name, path, port, tree, or lock conflict |
| 5 | state | invalid config, unsafe filesystem state, or missing tool |
| 6 | child failed | task, verify, probe, or destroy failed |
| 7 | external | Git or tmux failed |
| 8 | timeout | a bounded control-plane operation expired |

In text mode, passthrough doors and started tasks preserve the child's exit
status. Error reports always include a stable code and a remedy.

## Layout and maintenance

`WT_HOME` defaults to `~/.wt` and may be changed with the environment variable
or global `--home` option.

```text
$WT_HOME/
  config.toml                 user settings and per-repository overrides
  registry.json               registered repositories, trees, allocations
  state/<label>/<name>.json   per-tree lifecycle and resource snapshots
  state/<label>/_repo.json    repository-tied resource state
  trees/<label>/<name>/       linked worktrees (unless trees_dir overrides it)
  locks/                      bounded coordination locks and holder records

<tree>/.wt/
  tree_id                     ownership identity
  logs/                       newest task logs (20 per task by default)
  ...                         rendered files and application-owned data
```

Useful maintenance commands include `wt list --probe --disk`, `wt doctor`,
`wt locks`, `wt prune`, and `wt unregister <label> --yes`. `shell-init` emits
`wtcd`, `wtsh`, completions support, and a guard that restores a tree's binary
prefix if a shell startup file reordered `PATH`:

```sh
eval "$(wt shell-init zsh)"
```

The normative behavior is documented in [spec/SPEC.md](spec/SPEC.md). The
original problem statement remains available at
[spec/problem-statement.md](spec/problem-statement.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
