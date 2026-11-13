# wt

`wt` makes git worktrees you can run things in side by side. Each tree gets
its own block of ports, an environment and files rendered from the
repository's config, wrappers that run the tree's own builds, and lifecycle
hooks. Pools let commands in different trees take turns with things there
are only a few of, like simulators or test containers.

```sh
wt new fix-scroll                  # a worktree from origin's default branch, then its create hook
wt exec fix-scroll -- just test    # run something in its environment
wt with phone -- just ui-test      # hold one of the machine's phones while it runs
wt rm fix-scroll                   # its stop and delete hooks, then the worktree; the branch stays
```

wt is also a Rust library, so a program that runs agents in worktrees can
make trees and start processes in them without shelling out.

See [the cookbook](docs/cookbook.md) for worked setups: a dev stack per tree,
shared services with a database per tree, a single dev server handed between
trees, simulator pools, and more.

## Install

```sh
cargo install --git https://github.com/jordanwebster/wt
```

wt runs on macOS and Linux and needs `git`. Its state lives in `~/.wt`, or
in `$WT_HOME` if that is set.

## Configuration

A repository describes its trees in `.wt.toml`. For a repository where you
can't commit one, put it in `~/.wt/repos/<label>/wt.toml` instead. When that
exists it is used in place of the repository's file; the two aren't merged.

```toml
# Branch names for `wt new` without --branch. The first template whose
# values are all set wins: `wt new vocab --set ticket=PRO-1170` makes
# PRO-1170_vocab, and `wt new vocab` makes vocab.
branch = ["{{ticket}}_{{name}}", "{{name}}"]

# Named ports, stable for the life of the tree.
ports = ["server", "web"]

# Copied when a tree is made, copy-on-write where the filesystem supports it.
# A plain path is copied from the main checkout to the same place; a path
# that is already in the tree is left alone.
copy = [
  ".claude/settings.local.json",
  { from = "~/notes/justfile", to = ".wt/justfile" },
]

# Wrappers in .wt/bin, which is first on PATH inside a tree. `amux` runs
# this tree's build, and refuses and says why when it hasn't been built yet,
# rather than running a copy installed elsewhere.
[bin]
amux = "target/debug/amux"

[env]
DATABASE_URL = "postgres://localhost:{{ports.server}}/app_{{name_snake}}"

# Rendered into the tree whenever it is entered.
[files.".mcp.json"]
content = """{ "url": "http://localhost:{{ports.web}}/mcp" }"""
# mode = "0755"

[lifecycle]
create = "just setup"                # after the tree is made
start  = "docker compose up -d"      # must be safe to repeat
stop   = "docker compose stop"       # must be safe to repeat
delete = "docker compose down -v"    # when the tree is removed, after stop

[pool.phone]
size = 2
name = "sim-{{n}}"
acquire = 'scripts/sim acquire "$WT_LEASE"'   # every lease: create it if missing, then clean it
idle = "30m"
retire = 'scripts/sim delete "$WT_LEASE"'     # once nobody has leased it for `idle`
wait = "45m"                                  # how long a lease may wait; 30m by default
```

**Templates** in `env`, `files`, pool names and `branch` are `{{name}}`
substitutions, with no logic. They can use:

- `name`, `name_snake` and `name_short` (unique across repositories);
- `label`, the repository's name in wt;
- `root`, and `main` for the main checkout;
- `ports.<name>`;
- `env.<NAME>`, in files only.

An unknown name is an error. A file that needs literal `{{` belongs in
`copy` instead.

**Hooks** run with `sh -c` at the tree's root. They aren't templated: they
read the same values from the environment, which has:

- `WT_TREE` (`label/name`), `WT_ROOT`, `WT_NAME`, `WT_LABEL` and `WT_MAIN`;
- `WT_BIN`, the tree's `.wt/bin`;
- `WT_PORT_<NAME>` for each port;
- everything in `[env]`.

The create hook's output is kept in `.wt/create.log`, and `wt ls` shows
whether it is still running or failed. Until it has succeeded, `[bin]`
wrappers refuse to run, even if a binary is there: it may have been copied
from the main checkout.

**Git:** wt adds `.wt/`, rendered files and copied paths to the repository's
`info/exclude`, so they don't show up in `git status`. wt does nothing
language-specific: to start a Rust tree with the main checkout's build
output, for example, add `"target"` to `copy`.

## Machine settings

`~/.wt/config.toml`:

```toml
trees_dir = "~/.wt/trees"   # the default; trees go in <trees_dir>/<label>/<name>
[ports]
base = 20000                # the defaults; tree n's ports start at base + n * stride
stride = 16
[pool.container]            # this machine's size, wait or idle for a pool,
size = 4                    # overriding what the repository says
```

## Commands

A tree is named `label/name`, or just `name` when that is unambiguous. Left
out, it is the tree you are in. A repository's label is its main checkout's
directory name unless `wt init --label` says otherwise.

| Command | |
|---|---|
| `wt new [label/]name [--from REF] [--branch B] [--set k=v] [--no-fetch] [--no-create] [--wait]` | Make a tree, and return as soon as it exists: copying its inputs and running its create hook carry on in the background (`--wait` does them first). It starts from origin's default branch, fetched first; if the fetch fails or takes over 30 s, it uses the copy fetched last. A branch that already exists, locally or on origin, is checked out. Your main checkout is never touched. |
| `wt init [path] [--label L] [--name N] [--no-create] [--wait]` | Make an existing worktree, or the main checkout, a tree, and set it up as `new` does. Running it again keeps the tree's name and ports. |
| `wt rm tree [--force] [--keep]` | Run stop and delete, then remove the worktree, keeping its branch. Refuses uncommitted or untracked work, or a failing hook, without `--force`. `--keep` still runs stop and delete, then leaves the worktree on disk and stops managing it. |
| `wt ls` | Trees with their status, branch, path and ports. |
| `wt start [tree \| --all]`, `wt stop [tree \| --all]` | Run the start or stop hook. |
| `wt exec [tree] -- cmd` | Run a command in the tree's environment. Run from inside the tree, it stays in the current directory; otherwise it starts at the root. |
| `wt shell [tree]` | A shell in the tree's environment. |
| `wt env [tree]` | The environment as `export` lines, for `eval "$(wt env)"` or direnv. |
| `wt path [tree]` | The tree's path. |
| `wt with pool… -- cmd` | Run a command holding one slot of each pool. |
| `wt leases` | Each pool slot that has been used: who holds it, who used it last, and when it will be retired. |
| `wt migrate` | Import trees from wt 0.5, keeping their names and ports. |

`exec` and `with` exit with the command's own status.

If your shell's startup files move other directories in front of the
tree's `.wt/bin` (in `wt shell`, for instance), add this to the end of
them:

```sh
[ -n "$WT_BIN" ] && PATH="$WT_BIN:$PATH"
```

## Pools

`wt with phone -- cmd` waits for a free slot of the `phone` pool and runs
`acquire` on it. It then runs `cmd` with `WT_LEASE_PHONE` naming the slot,
and releases the slot when `cmd` exits, however it exits.

- **Which slot:** it prefers the slot your tree used last, then one that has
  been set up before.
- **Several pools** are taken in name order, so two commands can never wait
  on each other. To keep that true, take every pool a command needs in its
  outermost `wt with`: a nested `wt with` can only take what its parent
  doesn't hold yet, which may be out of order.
- **Nesting:** a command that already holds a pool (its `WT_LEASE_<POOL>` is
  set) doesn't take it again.
- **Retiring:** slots unused for `idle` are retired whenever anyone runs
  `wt with` or `wt leases`.
- **Sharing:** pools are shared by name across the machine. Outside a tree,
  only pools in `~/.wt/config.toml` are known.

## Using wt from Rust

```rust
let wt = wt::Wt::open()?;
let tree = wt.new_tree(&repo, wt::NewTree::named("fix-scroll"))?.tree;
tree.copy_inputs()?; // then run tree.hook(wt::Hook::Create), if you want it
let mut agent = std::process::Command::new("claude");
tree.env()?.apply(&mut agent);
agent.current_dir(&tree.root).spawn()?;
```

Hooks come back as ready-to-run commands from `Tree::hook`, so the program
decides where their output goes. The library never prints. The
[cookbook](docs/cookbook.md#using-wt-from-rust) has the whole lifecycle.
