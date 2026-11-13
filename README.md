# wt

`wt` makes git worktrees you can run things in side by side. Each tree gets
its own block of ports, an environment and files rendered from the
repository's config, wrappers that run the tree's own builds, and lifecycle
hooks. Pools let commands in different trees take turns with things there
are only a few of, like simulators or test containers.

```sh
wt new fix-scroll          # worktree from origin's default branch, then its create hook
wt exec fix-scroll -- just test
wt with iphone -- just ui-test
wt rm fix-scroll           # its stop and delete hooks, then the worktree; the branch stays
```

It is also a Rust library, so a tool that runs agents in worktrees can make
trees and start processes in them without shelling out.

## Configuration

A repository describes its trees in `.wt.toml`. For a repository where you
can't commit one, put it in `~/.wt/repos/<label>/wt.toml` instead; when that
exists it is used in place of the repository's file.

```toml
# Branch names for `wt new` without --branch. The first template whose
# values are all set wins: `wt new vocab --set ticket=PRO-1170` makes
# PRO-1170_vocab, and `wt new vocab` makes vocab.
branch = ["{{ticket}}_{{name}}", "{{name}}"]

# Named ports, stable for the life of the tree.
ports = ["server", "web"]

# Copied from the main checkout (or from `from`) when a tree is made,
# copy-on-write where the filesystem supports it.
copy = [
  ".claude/settings.local.json",
  { from = "~/notes/justfile", to = ".wt/justfile" },
]

# Wrappers in .wt/bin, which is first on PATH inside a tree. `amux` runs this
# tree's build and refuses, saying why, when it hasn't been built yet,
# rather than finding a copy installed elsewhere.
[bin]
amux = "target/debug/amux"

[env]
DATABASE_URL = "postgres://localhost:{{ports.server}}/app_{{name_snake}}"

# Rendered into the tree whenever it is entered.
[files.".mcp.json"]
content = """{ "url": "http://localhost:{{ports.web}}/mcp" }"""

[lifecycle]
create = "just setup"                # after the tree is made
start  = "docker compose up -d"      # safe to repeat
stop   = "docker compose stop"       # safe to repeat
delete = "docker compose down -v"    # when the tree is removed, after stop

[pool.iphone]
size = 2
name = "sim-{{n}}"
acquire = 'scripts/sim acquire "$WT_LEASE"'   # every lease: create if missing, then clean
idle = "30m"
retire = 'scripts/sim delete "$WT_LEASE"'     # once unused for `idle`
```

Templates can use `name`, `name_snake`, `name_short` (unique across
repositories), `label`, `root`, `main` (the main checkout), `ports.<name>`,
and in files `env.<NAME>`. Hooks run with `sh -c` at the tree's root and read
the same values from the environment: `WT_TREE` (`label/name`), `WT_ROOT`,
`WT_NAME`, `WT_LABEL`, `WT_MAIN`, `WT_PORT_<NAME>` and everything in
`[env]`.

wt does nothing language-specific. To start a Rust tree with the main
checkout's build output, for example, add `"target"` to `copy`.

## Machine settings

`~/.wt/config.toml` (or `$WT_HOME/config.toml`):

```toml
trees_dir = "~/.wt/trees"   # the default
[ports]
base = 20000                # the default; tree n's ports start at base + n * stride
stride = 16
[pool.container]            # this machine's size for a pool, overriding the repository's
size = 4
```

## Commands

| Command | |
|---|---|
| `wt new [label/]name [--from REF] [--branch B] [--set k=v] [--no-fetch] [--no-create]` | Make a tree and run its create hook. It starts from origin's default branch, fetched first (if the fetch fails or takes over 30 s, from the copy fetched last). Your main checkout is never touched. |
| `wt init [path] [--label L] [--name N] [--no-create]` | Make an existing worktree, or the main checkout, a tree. |
| `wt rm tree [--force] [--keep]` | Run stop and delete, then remove the worktree, keeping its branch. Refuses unsaved work or failing hooks without `--force`. `--keep` leaves the worktree and stops managing it. |
| `wt ls` | Trees, with their status, branch, path and ports. |
| `wt start [tree \| --all]`, `wt stop [tree \| --all]` | Run the start or stop hook. |
| `wt exec [tree] -- cmd` | Run a command in the tree's environment, in the current directory if it is inside the tree. |
| `wt shell [tree]` | A shell in the tree's environment. |
| `wt env [tree]` | The environment as `export` lines: `eval "$(wt env)"`, or from direnv. |
| `wt path [tree]` | The tree's path. |
| `wt with pool… -- cmd` | Run a command holding one slot of each pool. |
| `wt leases` | Who holds what. |
| `wt migrate` | Import trees from wt 0.5, keeping their names and ports. |

A tree is `label/name`, just `name` when that is unambiguous, or, left out,
the tree you are in.

## Pools

`wt with iphone -- cmd` waits for a free slot of the `iphone` pool, runs
`acquire` for it, and runs `cmd` with `WT_LEASE_IPHONE` naming the slot. The
slot is released when `cmd` exits, however it exits. It prefers the slot your
tree used last, then one that has been set up before. Several pools are taken
in name order, so two commands can never wait on each other. A command that
already holds a pool (its `WT_LEASE_<POOL>` is set) doesn't take it again.
Pools are shared by name across the machine, and `wait` (default 30m) bounds
how long to wait for one.
