# wt cookbook

Patterns for common setups. Commands in hooks and pools are ordinary shell,
so in practice most of them call the repository's own scripts or task
runner; they are spelled out inline here to show what they do.

- [A dev stack per tree](#a-dev-stack-per-tree)
- [Shared services and a database per tree](#shared-services-and-a-database-per-tree)
- [One dev server, pointed at one tree at a time](#one-dev-server-pointed-at-one-tree-at-a-time)
- [Run the tree's own build of a tool](#run-the-trees-own-build-of-a-tool)
- [Keep a command out of trees](#keep-a-command-out-of-trees)
- [Start a Rust tree with a warm build](#start-a-rust-tree-with-a-warm-build)
- [Limit how many heavy commands run at once](#limit-how-many-heavy-commands-run-at-once)
- [Devices that are slow to create](#devices-that-are-slow-to-create)
- [Scripts that also run in CI](#scripts-that-also-run-in-ci)
- [Branch names from tickets](#branch-names-from-tickets)
- [A repository you can't commit config to](#a-repository-you-cant-commit-config-to)
- [Worktrees made by other tools](#worktrees-made-by-other-tools)
- [Entering a tree](#entering-a-tree)
- [Using wt from Rust](#using-wt-from-rust)

## A dev stack per tree

Each tree runs its own copy of everything, on its own ports, as its own
compose project.

```toml
ports = ["postgres", "server", "web"]

[env]
COMPOSE_PROJECT_NAME = "app-{{name_snake}}"
POSTGRES_PORT = "{{ports.postgres}}"
DATABASE_URL = "postgres://postgres@localhost:{{ports.postgres}}/app"
SERVER_URL = "http://localhost:{{ports.server}}"

[lifecycle]
create = "npm ci"
start  = "docker compose up -d --wait"
stop   = "docker compose stop"
delete = "docker compose down -v"
```

`compose.yml` reads `${POSTGRES_PORT}` for its port mapping. `wt start`
brings the stack up, `wt stop` pauses it with its data intact (`wt stop
--all` frees the whole machine), and `wt rm` takes it down with its volumes.

A tool that isn't started from inside the tree, such as an editor's MCP
client, can't see the environment. Render the value into the file it reads
instead:

```toml
[files.".mcp.json"]
content = """
{ "mcpServers": { "telemetry": { "type": "http", "url": "http://localhost:{{ports.web}}/mcp" } } }
"""
```

Rendered files are rewritten whenever the tree is entered, and wt adds them
to the repository's `info/exclude` so they never show up in `git status`.

## Shared services and a database per tree

Running every service per tree is too heavy for a big application. Instead,
share the services between trees and give each tree only its own database,
cloned from a template when something first needs it.

```toml
[env]
DATABASE_NAME = "app_{{name_snake}}"

[lifecycle]
delete = "scripts/db drop"
```

```sh
#!/bin/sh
# scripts/db: this tree's database, on the shared postgres.
set -eu
case "${1:-ensure}" in
  ensure)
    # Shared by every tree, and never stopped by wt: no hook stops it.
    docker compose -p shared -f services.yml up -d --wait postgres redis
    docker exec shared-postgres-1 psql -U app -lqt | cut -d'|' -f1 | grep -qw "$DATABASE_NAME" ||
      docker exec shared-postgres-1 createdb -U app -T app_template "$DATABASE_NAME" ;;
  drop)
    docker exec shared-postgres-1 dropdb -U app --if-exists "$DATABASE_NAME" ;;
esac
```

The recipes that need a database call `scripts/db` first. Both commands are
safe to repeat, so wt doesn't need to know whether the database exists. If
postgres is down when the tree is removed, `wt rm` stops with the delete
hook's error; start it and retry, or `wt rm --force` to skip the drop.

## One dev server, pointed at one tree at a time

Some things can only exist once: a server on port 80, a container with a
fixed name. Point it at the tree you're working in, and say whose it was.
Ask the thing itself who has it rather than keeping a record that can
drift.

```sh
#!/bin/sh
# scripts/serve [--take]: point the shared web container at this tree.
set -eu
holder=$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/app"}}{{.Source}}{{end}}{{end}}' shared-web-1 2>/dev/null || true)
if [ -n "$holder" ] && [ "$holder" != "$WT_ROOT" ] && [ "${1:-}" != --take ]; then
  echo "port 80 is serving $holder; run scripts/serve --take to move it here" >&2
  exit 1
fi
scripts/db
# The same project name in every tree means one container, recreated with
# this tree's bind mount.
docker compose -p shared -f web.yml up -d web
if [ -n "$holder" ] && [ "$holder" != "$WT_ROOT" ]; then echo "took port 80 from $holder"; fi
```

```toml
[lifecycle]
# Stop the server only if it is serving this tree.
delete = '''
holder=$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/app"}}{{.Source}}{{end}}{{end}}' shared-web-1 2>/dev/null || true)
if [ "$holder" = "$WT_ROOT" ]; then docker compose -p shared -f web.yml stop web; fi
scripts/db drop
'''
```

Hooks aren't templated, so the `{{...}}` in `docker inspect` reaches docker
untouched.

## Run the tree's own build of a tool

When the repository builds a tool you also have installed, typing its name in
a tree should run the tree's build, and never quietly run the installed one
because the tree hasn't built yet.

```toml
[bin]
mytool = "target/debug/mytool"

[lifecycle]
create = "cargo build"
```

wt writes `.wt/bin/mytool`, and `.wt/bin` comes first on `PATH` inside the
tree. Once built, `mytool` is the tree's build. Before that, it fails and
says why:

```
mytool: not built in app/fix-scroll (…/target/debug/mytool is missing): the create hook is still running; see …/.wt/create.log
```

Point any environment the tool reads at the tree too, so the tree's build
doesn't talk to the copy you use day to day:

```toml
[env]
MYTOOL_CONFIG = "{{root}}/.wt/mytool/config.toml"
```

## Keep a command out of trees

Anything in `.wt/bin` shadows the same name on `PATH`. To stop a command that
is wrong to run from a tree, for example a `make` target that acts on a
shared container, copy a guard script there:

```toml
copy = [{ from = "~/.wt/repos/app/make-guard", to = ".wt/bin/make" }]
```

```sh
#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    dev|rebuild_db) echo "make $arg acts on the shared container; use scripts/serve" >&2; exit 2 ;;
  esac
done
exec /usr/bin/make "$@"
```

## Start a Rust tree with a warm build

```toml
copy = ["target"]
```

A new tree starts with a copy-on-write clone of the main checkout's
`target/`, which costs almost nothing on APFS. Cargo decides whether a
third-party crate is fresh from its version, features, profile and compiler,
not from the tree's path, so those stay built and the first build in the
tree compiles only the workspace. The clone is as current as the main
checkout's last build.

`copy` makes a full copy where the filesystem can't clone (most Linux
filesystems), so don't list large directories there.

## Limit how many heavy commands run at once

A pool with only a size is a set of tokens. Each command holds one while it
runs.

```toml
[pool.container]
size = 2
wait = "10m"
```

```sh
wt with container -- docker compose run --rm app pytest
```

How many a machine can take is a fact about the machine, so set it in
`~/.wt/config.toml`, where it overrides the repository:

```toml
[pool.container]
size = 4
```

## Devices that are slow to create

Simulators, emulators and browsers are worth keeping between commands but
too heavy to keep forever.

```toml
[pool.phone]
size = 2
name = "app-phone-{{n}}"
acquire = 'scripts/phone acquire "$WT_LEASE"'
idle = "30m"
retire = 'scripts/phone delete "$WT_LEASE"'
wait = "45m"
```

```sh
wt with phone -- scripts/ui-test     # the command sees WT_LEASE_PHONE=app-phone-1
```

- `acquire` runs on every lease, so it should create the device when it is
  missing and otherwise reset whatever a previous command could have left
  behind.
- After a command, the device stays as it is. A tree's next command prefers
  the device it used last, so a series of commands keeps the same device.
- A device nobody has leased for `idle` is retired the next time anyone runs
  `wt with` or `wt leases`.
- `wt leases` shows who holds each device and when it will be retired.

A command run under a lease can run another that asks for the same pool: it
already holds one, so it isn't asked to wait for a second. Several pools are
taken in name order (`wt with phone tablet -- …`), so two commands can't each
hold what the other is waiting for. That only holds when a command takes
everything it needs at once: if `wt with tablet` runs something that asks
for `phone`, it takes `phone` after `tablet`, out of order. Lease both in
the outermost command.

## Scripts that also run in CI

Where wt isn't installed, run the command without coordinating:

```sh
#!/bin/sh
# scripts/with POOL... -- COMMAND...
pools=""
while [ "$#" -gt 0 ] && [ "$1" != -- ]; do pools="$pools $1"; shift; done
shift
if [ -n "${WT_TREE:-}" ] && command -v wt >/dev/null; then
  exec wt with $pools -- "$@"
fi
exec "$@"
```

The command finds its device in `WT_LEASE_<POOL>`, and falls back to a fixed
name when that is unset.

## Branch names from tickets

```toml
branch = ["{{ticket}}_{{name}}", "{{name}}"]
```

```sh
wt new vocab-routing --set ticket=PRO-1170   # branch PRO-1170_vocab-routing
wt new vocab-routing                         # branch vocab-routing
wt new vocab-routing --branch anything/else  # as given
wt new review --branch colleague/fix         # origin's branch, checked out and tracking it
```

## A repository you can't commit config to

Put the config in `~/.wt/repos/<label>/wt.toml`. When that exists, wt uses it
instead of the repository's `.wt.toml`. Keep anything else the trees need
beside it and copy it in:

```
~/.wt/repos/app/
  wt.toml
  justfile
  make-guard
```

```toml
copy = [
  { from = "~/.wt/repos/app/justfile",   to = ".wt/justfile" },
  { from = "~/.wt/repos/app/make-guard", to = ".wt/bin/make" },
]

[env]
JUST_JUSTFILE = "{{root}}/.wt/justfile"
```

Sharing this with a team is a matter of handing out the directory.

## Worktrees made by other tools

Coding tools that make their own worktrees can still use wt for the rest.
Make the worktree a tree after the tool creates it, and stop managing it
before the tool deletes it. For example, a Codex local environment
(`.codex/environments/environment.toml`):

```toml
version = 1
name = "wt"

[setup]
script = '''
tree="codex-$(basename "$(dirname "$CODEX_WORKTREE_PATH")")"
wt init "$CODEX_WORKTREE_PATH" --name "$tree"
cd "$CODEX_WORKTREE_PATH" && eval "$(wt env)"
'''

[cleanup]
script = '''
tree="codex-$(basename "$(dirname "$CODEX_WORKTREE_PATH")")"
wt rm --keep "$tree"
'''
```

`wt init` gives the worktree its ports, copies and renders its files, and
runs its create hook. `wt rm --keep` runs its stop and delete hooks and
forgets it, leaving the worktree for the tool to delete.

## Entering a tree

- `wt shell fix-scroll` opens a shell in the tree's environment.
- `wt exec fix-scroll -- cmd` runs one command. Run from inside the tree,
  it stays in the current directory.
- For your own shell, `eval "$(wt env)"` in the tree. With direnv, put that
  line in the tree's `.envrc` (add `.envrc` to `info/exclude` or your global
  gitignore).
- If your shell's startup files put other directories ahead of the tree's
  `.wt/bin`, so that `mytool` finds an installed copy inside `wt shell`, end
  them with `[ -n "$WT_BIN" ] && PATH="$WT_BIN:$PATH"`.

## Using wt from Rust

The CLI is a thin layer over the library, which a program can use to make
trees and start processes in them:

```rust
use std::process::Command;

let wt = wt::Wt::open()?;
let tree = wt.new_tree(&repo, wt::NewTree::named("fix-scroll"))?.tree;
if let Some(mut create) = tree.hook(wt::Hook::Create)? {
    create.status()?; // or spawn it and stream its output somewhere
}
let mut agent = Command::new("claude");
tree.env()?.apply(&mut agent);
agent.current_dir(&tree.root).spawn()?;

// Later, when the work is done. Check first, so the teardown hooks never
// run on a tree that then can't be removed.
wt.check_remove(&tree, wt::Remove::default())?;
for hook in [wt::Hook::Stop, wt::Hook::Delete] {
    if let Some(mut command) = tree.hook(hook)? {
        command.status()?;
    }
}
wt.remove(&tree, wt::Remove::default())?;
```

`Tree::hook` returns a ready-to-run `Command`, so the program decides where
the output goes. The library never prints.
