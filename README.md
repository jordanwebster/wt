# wt

[![CI](https://github.com/jordanwebster/wt/actions/workflows/ci.yml/badge.svg)](https://github.com/jordanwebster/wt/actions/workflows/ci.yml)

**`wt` gives a git worktree its own everything.**

A worktree is not just a second checkout — it is a second *environment*. Its
own build first on `PATH`, its own ports, its own database URL, its own
rendered config files. Several branches of one project run side by side
without colliding, and when a worktree goes away, what belonged to it goes
with it.

```sh
wt register ~/source/orbit
wt new orbit/fix-scrolling        # creates the tree, lands you inside it
orbit serve                       # this tree's build, on this tree's port
wt rm orbit/fix-scrolling
```

## A worktree owns its names

That is the whole idea. Inside a worktree, a name it owns means *that
worktree's* version — never the copy installed on your machine, and never
another worktree's.

You declare what a worktree owns. `wt` enforces each kind differently,
because each fails differently:

| You declare | in | `wt` enforces it by |
|---|---|---|
| command names | `commands` | refusing to run anything else by that name |
| variable names | `env` | using your value, not the one you inherited |
| ports | `ports` | allocating from this worktree's own slot |
| file paths | `files` | rendering them, and re-rendering if they drift |

The guarantee runs both ways. If this worktree hasn't built `orbit` yet, `wt`
**refuses** rather than quietly handing you the copy in `/usr/local/bin`:

```console
$ orbit serve
orbit: this worktree's build isn't ready yet (still building; see the reported log).
       Run /usr/local/bin/orbit if you meant the installed copy.
```

You cannot accidentally run the wrong binary, or reach a colleague's database
because your login shell exported `DATABASE_URL` months ago.

One honest limit: this covers commands resolved through `PATH`. A shell alias
or function still wins, because nothing can outrank those — `wt doctor`
reports one shadowing a name you own rather than pretending otherwise.

## Your application never learns `wt` exists

Your app reads `DATABASE_URL` and `config/local.yaml`, exactly as it always
did. `.wt.toml` is the adapter between `wt`'s values and the interface your
application already has. Nothing in your source changes.

```toml
commands = ["orbit"]              # names this worktree owns
bin      = ["target/debug"]       # where its binaries are built
ports    = ["http", "db"]         # allocated per worktree

[vars]                            # private: never leaves this file
db_name = "orbit_{{name_snake()}}"
db_url  = "postgres://localhost:{{ports.db}}/{{db_name}}"

[env]                             # claimed: what your app actually sees
DATABASE_URL = "{{db_url}}"
PORT         = "{{ports.http}}"

[files."config/local.yaml"]       # rendered fresh for each worktree
content = """
database: {{db_url}}
port: {{ports.http}}
"""
```

**`env` is claimed. `vars` are private.** `vars` exist so you write a value
once and materialise it wherever it's needed — into the environment, into a
rendered file, into both. They are never exported, so an internal path or a
generated name doesn't become ambient state in every process you launch.

`wt` replaces `.env` files for everything it launches. For tools that read
`.env` from disk regardless — Vite, Next.js, dotenv, Rails — render one with
`files` and keep the single definition in `vars`.

## Values and functions

Anything in `{{…}}` is evaluated. A bare name is a constant you defined; a name
with parentheses is a function `wt` provides.

| | |
|---|---|
| `{{db_url}}` | a constant from your own `[vars]` |
| `{{name()}}` | the worktree's name |
| `{{name_snake()}}` | the same, safe for identifiers |
| `{{label()}}` | the repository's label |
| `{{name_short()}}` | a short unique name, stable for the worktree's life |
| `{{target()}}` | `label/name`, how you address it |
| `{{branch()}}` | the branch checked out when the process started |
| `{{home()}}` | the resolved wt home |
| `{{root()}}` | the worktree's absolute path |
| `{{repo()}}` | the registered checkout's absolute path |
| `{{ports.http}}` | the port you named `http` |

Ports are a lookup, not an allocation: `{{ports.db}}` is the port you named
`db` in the `ports` list, it returns the same number every time, and a name
keeps its number when the list is reordered or extended.

Shell-string and argv recipes use the same templates. Shell strings are filled
before `sh -c`, with substitutions inserted verbatim, so quote path-valued
substitutions. Dollar signs belong entirely to the shell: `${h%??}`, `$HOME`,
and `$$` reach it unchanged.

```toml
[task.check]
run = "psql -p '{{ports.db}}' -c 'select 1'"
```

Written as a list instead of a string there is no shell to collide with, so
each element is filled in directly:

```toml
run = ["psql", "-p", "{{ports.db}}", "-c", "select 1"]
```

Every `{{` starts a template expression; whitespace inside an expression is
invalid. Set `template = false` on a `files` entry whose content or source uses
literal `{{` syntax, such as Jinja, Helm, or GitHub Actions. `wt config
<target>` shows every value with the layer it came from.

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

`wt` supports POSIX systems and requires Git 2.31 or newer. Windows is not
supported natively; under WSL it behaves as it does on Linux, and
[docs/windows-support.md](docs/windows-support.md) records what a native port
would involve. tmux 3.2 or newer is optional and enables sessions. Adapters
only require the tools the repository being used actually needs.

## Status

`wt` is at 0.1. It was built against three real repositories — a Rust service
with a per-checkout daemon, a React Native app, and a .NET Aspire stack — and
those shapes are covered by the acceptance suite. It is not packaged for any
distribution yet, and command output and `.wt.toml` keys may still change
between versions.

## Tour

Register an existing checkout, or clone and register in one operation:

```sh
wt register ~/source/orbit
wt clone https://example.com/acme/service.git --label service
```

Registration is consent for the repository's declared tasks and resource
recipes to run. The registered checkout becomes the `canonical` worktree,
addressable by its label alone, and it is a full participant — its own ports,
its own rendered files, its own claimed commands.

Create a linked worktree:

```console
$ wt new orbit/fix-scrolling --from main
Created orbit/fix-scrolling
  path    /Users/me/.wt/trees/orbit/fix-scrolling
  branch  fix-scrolling
  ports   http 20016, db 20017
```

You land inside the worktree, in its own session, with its environment live.
On either session backend, the repository's `build` task starts in a detached
supervisor and reports its log path without making `wt new` wait. While a build
is running, a command this worktree owns says so instead of running the wrong
one; `wt status` and `wt doctor` report completion or failure.

```sh
wt new orbit/review-42 --from pr:42 --no-attach   # provision, don't enter
wt new orbit/scratch --no-open                    # no session at all
wt new orbit/ticket-42 --meta ticket=ABC-42       # attach opaque fleet data
wt ls
wt status orbit/fix-scrolling
```

Metadata is a small string map on the tree record. `wt status` shows it,
`wt meta orbit/ticket-42` prints it, and `wt meta orbit/ticket-42 owner=alice
ticket=` sets `owner` and removes `ticket`. Keys are opaque to `wt`: templates
cannot read them, and no behaviour depends on them. Keys match
`[a-z_][a-z0-9_]*` and values stay under 1 KiB — `ticket=PRO-123` fits, while
a key spelled `JIRA-ID` is refused for the capitals and the dash.

Worktrees live under `$WT_HOME/trees/<label>/<name>` by default. `wt path`
prints the exact root.

### Doors: `run`, `exec`, `shell`, and sessions

Every way into a worktree computes the same environment, applies the same
claims, and renders the same managed files before starting the child.

```sh
wt run build orbit/fix-scrolling   # declared task, captured log
wt test orbit/fix-scrolling        # aliases: test/lint/fmt/build
wt test orbit/fix-scrolling -- tests/api.rs -q
wt exec orbit/fix-scrolling -- env # one-shot command; child's stdio/status
wt shell orbit/fix-scrolling       # interactive, non-login shell
wt which orbit/fix-scrolling orbit # resolve a name through this worktree
```

`exec` and `shell` hand the terminal to the child and therefore do not support
`--json`. Use `wt env --json` to inspect what a child will receive.

Arguments after `--` go to one resolved task recipe only; its dependencies do
not receive them. An argv recipe gets the arguments appended. A shell recipe
must place `"$@"` where they belong, or `wt` refuses instead of guessing. A
composite that fans out to several tasks also refuses and names the leaf tasks
you can invoke directly; an aggregate you declared yourself refuses however
many needs it has, and a resource never takes arguments. Built-in adapters
forward test arguments where their placement is unambiguous — `go test ./...`
is the exception and refuses.

`open` creates or enters a session with the environment active:

```sh
wt open orbit/fix-scrolling
wt open orbit/fix-scrolling --agent codex
wt open --all
wt close orbit/fix-scrolling
```

With `session.backend = "none"`, a per-tree `open` enters the same interactive
shell as `wt shell`; `open --all` is tmux-only. With tmux, `open --all` skips
the canonical checkout, which is the repository anchor and can be opened
explicitly. The configured `session.agent` default likewise does not apply to
that canonical checkout; an explicit `--agent` still does.

A coding agent starts only when `wt` *creates* a session, never when it
attaches to one that already exists — a session is provisioning, and an agent
is work. Leave `session.agent` unset and sessions get your shell; start an
agent yourself once you are inside.

When an agent exits, its pane becomes the same interactive shell `wt shell`
would start, in the same tree and environment. A command that never starts is
different: shell statuses 126 and 127 tear the pane down during the startup
window, so `wt open` reports `SESSION_CREATE_FAILED` with the captured output
instead of turning a configuration error into a prompt.

```toml
[session]
backend = "tmux"      # or "none"
attach  = true
agent   = "codex"     # optional; unset means a shell

[agents.codex]
start  = ["codex"]
resume = ["codex", "resume", "--last"]
```

Remove a worktree when finished. `wt rm` (`wt remove` is an alias) asks only when the
removal would destroy work: uncommitted changes, or commits no remote carries
on a branch it is about to delete. A clean worktree whose commits are pushed is
removed without a prompt. `--force` permits a removal that loses work and is
itself the consent, so it never prompts; without a terminal such a removal is
refused rather than guessed at. The other destructive commands — `unregister`,
`destroy`, `refresh`, `prune` — prompt on a terminal and require `--yes`
otherwise.

Targets use the same address rules as every other command: a bare name first
resolves within the current repository label, then as a label itself; an
unresolved name reports fully qualified candidates. Repeating removal with an
explicit `label/name` after it has been tombstoned succeeds as an explained
no-op.

```sh
wt rm orbit/fix-scrolling            # asks only if there is work to lose
wt rm orbit/fix-scrolling --force    # discards it without asking
wt prune --yes
```

Removal closes the session, then destroys the resources this worktree actually
created — the plan recorded when they were created, not whatever the branch
happens to say now — and then removes the Git worktree. The branch goes too
when its commits are on a remote, since `origin` can restore it; a branch
carrying unpushed commits is kept, and the summary says so. `--delete-branch`
deletes it either way, `--keep-branch` never does. `wt prune` reports or
repairs stale records and out-of-band deletions.

## `.wt.toml` reference

`.wt.toml` is optional and normally committed at the repository root. Detected
adapters supply useful defaults; your configuration overrides or extends them.

Top-level keys:

- `commands`: the command names this worktree owns. Typing one always runs
  this worktree's build or explains why it can't. Declare it yourself: no
  built-in adapter contributes command names yet, though the configuration
  layer for them exists. The name `wt` cannot be claimed — a name that refuses
  until it is built would make `wt build` unrunnable.
- `bin`: relative directories prepended to `PATH`. Where the binaries are;
  `commands` is which names they provide.
- `ports`: named ports, reachable as `{{ports.<name>}}`.
- `vars`: private values, composed from functions and from each other. Never
  exported. Evaluated as a dependency graph, so order in the file does not
  matter; a cycle or an unknown name is an error naming the keys involved.
- `env`: the environment your application sees. A declared value wins over
  whatever you inherited, and the inherited value is restored when you leave.
  A value of `false` removes an inherited key without supplying one.
- `files`: whole files rendered for each worktree. Exactly one of `content` or
  `source`; `marker` sets the provenance-comment prefix (empty disables it),
  and `mode` defaults to `0644`.
- `copy`: paths that **must** be present — a local secret, an editor setting.
  Populated from the registered checkout at creation by a recursive byte copy.
- `task`: declared tasks and resources.
- `locks."name"`: capacity and wait policy for a named task lock.
- `dirs."path"`: a directory scope for monorepos. Nearer settings win; root
  tasks compose subproject tasks.
- `adapters`: choose a tool or disable an adapter for a scope.
- `sync_inputs`: paths that decide whether dependency state has drifted.
- `detect`: adapter scan depth and ignored paths.

### Tasks and resources

A task accepts `run`, `exists`, `needs`, `lock`, `env`, `cwd`, `timeout`, and
`description`. `needs` forms an acyclic graph, so a task runs after everything
it depends on. `exists` exits 0 when present, 1 when absent, and 2 or greater
when it cannot tell.

A task with only `needs` and an optional `description` is an aggregate. It is
useful as a named entry point without a placeholder `run = "true"`:

```toml
[task.setup]
description = "prepare this machine and this checkout"
needs = ["cli-login", "database", "build"]
```

**The task named `build` runs automatically after a worktree is created.** It
runs under the same detached supervisor on either session backend. Give it
`needs` to pull in whatever else setup requires:

```toml
[task.database]
tied_to = "tree"
exists  = "psql -lqt | cut -d'|' -f1 | grep -qw '{{db_name}}'"
run     = "createdb '{{db_name}}' && sqlx migrate run"
destroy = "dropdb '{{db_name}}'"

[task.build]
run   = "cargo build"
needs = ["database"]
```

A task with `destroy` is a resource, and also requires `exists` and `tied_to`:

- `tied_to = "tree"` gives each worktree its own.
- `tied_to = "repo"` shares one across the repository; it is destroyed at
  `wt unregister`, not at ordinary removal.
- `tied_to = "machine"` shares one host-wide, even across registered labels;
  only explicit `wt destroy` or `wt refresh` tears it down.
- `name` supplies `WT_SELF` to that resource's recipes.
- `snapshot_env` names extra parent values teardown must retain.
- `ready_within` polls after `run` until the resource is present.

`run` is optional for resources your application creates itself: a later probe
discovers it, and removal still destroys it. A failed probe never triggers
`run` or `destroy`. Use `wt tasks` to inspect the effective graph and `wt run
--dry-run` for one execution.

Machine resources make the task graph an executable onboarding check. An
aggregate such as `setup` can depend on host facts and ordinary tree work using
the same `needs` list:

```toml
[task.cli-login]
tied_to = "machine"
exists  = "acme auth status >/dev/null 2>&1"
run     = "acme auth login"
destroy = "acme auth logout"

[task.setup]
needs = ["cli-login", "build"]
```

For a tree-tied resource that must have only one live instance in a wider
arena, set `exclusive = "repo"` or `exclusive = "machine"`. The arena records
the holder tree. Another tree's `run` reports `RESOURCE_HELD` without probing
or destroying that holder; `wt run server other/tree --take` explicitly tears
down the holder through its recorded snapshot, claims the arena, and reports
which tree it displaced. `--take` is the consent and never prompts.

### Named lock capacity

A task's `lock` can be a mutex or a bounded slot pool:

```toml
[locks.integration]
slots = 2
wait = "30s"

[task.integration]
lock = "integration"
run = "./test-integration"
```

Without a `locks."name"` entry the lock has one slot. The default wait now
comes from `task.lock_wait` and is honoured; with the default `0s`, contention
fails fast instead of queueing invisibly. Use the per-lock `wait`, `wt run
--wait`, or `--wait forever` when queueing is intentional. `wt locks` shows
`held n/N` and the holder of each occupied slot. The pool is scoped to the
repository: two labels declaring the same lock name count their slots
separately, so cap what one repository's worktrees may run at once.

## Environment rules

Inside a worktree, `wt` exports two tiers:

| Tier | Variables | Change policy |
| --- | --- | --- |
| Interface | `WT_TARGET`, `WT_LABEL`, `WT_NAME`, `WT_ROOT`, `WT_REPO`, `WT_HOME`, `WT_BRANCH` | Stable scripting interface; changes are announced deliberately. |
| Mechanism | `WT_ACTIVATION`, `WT_PATH_PREFIX`, `WT_BIN`, `WT_SELF`, `WT_TASK` | Internal door/task plumbing; may change as the mechanism evolves. |

Name transformations belong in template functions such as `{{name_snake()}}`
and `{{name_short()}}`, not environment variables. `WT_SESSION`,
`WT_NAME_SNAKE`, `WT_NAME_SHORT`, and `WT_SLOT` are not exported. Inside tmux,
use tmux's `$TMUX_PANE` for the current pane and `wt ls --json` for registry
lookup.

Ports are **not** exported. They are an input to your configuration, not part
of your application's environment: your app reads `PORT` or `DATABASE_URL`
because you declared them. `wt status` shows the allocation.

Activation is explicit and re-entrant. Outside a worktree your shell and its
`PATH` are untouched. Entering worktree B from a shell activated for A restores
A's values first, then applies B's; leaving restores what you had.

```sh
wt env orbit/fix-scrolling          # values plus binary inventory
wt env orbit/fix-scrolling --sh     # shell export/unset statements
wt env orbit/fix-scrolling --dotenv
wt env --deactivate --sh
```

`.wt/` and managed or copied paths are added to the repository's shared
`info/exclude`, so `git status` stays clean without touching `.gitignore`. If a
rendered file no longer matches its recorded hash, `wt` preserves your edit and
asks you to remove the file before regenerating it.

## Adapters

Adapters are built-in knowledge about ecosystems. Detection is scoped for
monorepos, lockfiles choose package managers, and your configuration wins over
any default.

| Stack | Detected by | Provides |
|---|---|---|
| Rust | `Cargo.toml` | cargo sync/build/test/clippy/fmt |
| Node | `package.json` plus an npm, pnpm, Yarn, or Bun lockfile | install and declared package scripts |
| .NET | solution or project files | restore/build/test/format |
| Python | `pyproject.toml`, lockfiles, or requirements | uv, Poetry, or pip workflows |
| Go | `go.mod` | download/build/test/vet/gofmt |
| Git submodules | `.gitmodules` | recursive initialization |

A fresh worktree starts warm **in that ecosystem's own terms**, not by copying
build output around. Cargo 1.91+ separates build intermediates from outputs;
the adapter gives every tree its own
`{{home()}}/cache/cargo-build/{{label()}}/{{name_short()}}` build directory —
private, because Cargo's unit hashes ignore the workspace path, so trees
sharing one directory would corrupt each other's generated code and
freshness — while each tree keeps its binaries in its own `target/`. The
directory is deleted with the tree, `wt prune` reaps orphans, and
`wt ls --disk` sizes it (`cache_kb`). Cross-tree warmth comes from
content-addressed caches instead: install sccache and set
`rustc-wrapper = "sccache"` in `~/.cargo/config.toml [build]`, and every tree
compiles shared dependencies as cache hits. pnpm hard-links packages from its
content-addressed store, and uv hard-links from its global cache into
`.venv`. `wt doctor` reports whether the relevant cache or accelerator is in
use; it never switches tools for you.

And a cold first build is not a wait: the `build` task runs in the background
after `wt new`, so you are working in the worktree while it compiles.

## Automation, JSON, and exit codes

Reporting and lifecycle commands support `--json` and emit one stable envelope
with `wt.schema`, `wt.version`, `ok`, `command`, `data`, `notices`, and
`error`. A failure normally has `data: null`, but a batch may retain partial
data with `ok: false`; `open --all` does this so successful and failed session
attempts remain inspectable. Its shared session list has three record shapes:
open (`created`/`existing`), closed (`closed`), and failed (`failed: true`),
though each verb emits only the shapes relevant to it. Arrays have stable ordering. Environment values appear only in `wt
env`; resource snapshots are never printed.

`exec`, `shell`, and an attaching `open` pass the child's streams and status
through, and therefore refuse `--json`. `run --json` keeps stdout for the
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

Passthrough doors and started tasks preserve the child's exit status. Error
reports always carry a stable code and a remedy.

## Layout and maintenance

`WT_HOME` defaults to `~/.wt` and can be changed with the environment variable
or the global `--home` option.

```text
$WT_HOME/
  config.toml                 user settings and per-repository overrides
  registry.json               registered repositories, trees, allocations
  state/<label>/<name>.json   per-tree lifecycle and resource snapshots
  state/<label>/_repo.json    repository-tied resource state
  state/_machine.json         machine-tied resource state
  trees/<label>/<name>/       linked worktrees (unless trees_dir overrides it)
  cache/cargo-build/<label>/<name_short>/   per-tree build intermediates; die with the tree
  locks/                      bounded coordination locks and holder records

<tree>/.wt/
  tree_id                     ownership identity
  shims/                      one entry per owned command name
  logs/                       newest task logs (20 per task by default)
  ...                         rendered files and application-owned data
```

Useful maintenance commands: `wt ls --probe --disk`, `wt doctor`, `wt locks`,
`wt prune`, and `wt unregister <label> --yes`. `shell-init` keeps completion
support, restores a worktree's binary prefix if a shell startup file reordered
`PATH`, and adds a guarded `(<target>) ` prompt prefix inside door shells:

```sh
eval "$(wt shell-init zsh)"
```

See the [cookbook](docs/cookbook.md) for policy shims, agent wrappers, and
editor integration patterns.

Normative behaviour lives in [spec/SPEC.md](spec/SPEC.md). The original problem
statement is at [spec/problem-statement.md](spec/problem-statement.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
