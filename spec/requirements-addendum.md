# Requirements addendum (product decisions made after the problem statement)

These are requirements, not implementation. They were settled after running the first implementation against three
real repositories. A clean design must satisfy them; it is free to satisfy them
with a different schema or structure than the current tool uses.

## A1. One environment per tree, several doors

Every way of running something inside a tree computes the *same*
environment: a one-shot command, a declared task, an interactive shell,
and a background session (tmux) for a human or a coding agent. What the
user types in a shell and what a task runs must never disagree about which
binary, port, socket or config file is in play.

## A2. Outside a door, nothing changes

A plain shell in any checkout (including the main checkout) behaves
exactly as if the tool did not exist: the repo's own default ports, the
installed binaries on PATH. Activation is always explicit.

## A3. The main checkout is a tree too

The canonical checkout (`~/source/orbit`) is addressable like any tree and
gets its own coordinates (port block, generated files, resources, session)
on first use, without a creation step. Motivation: a dev build of a daemon
in the main checkout must be able to run beside the *installed* release of
the same daemon, each with its own socket/state/identity.

## A4. Worktree-specific binaries shadow installed ones

A repo can declare directories (e.g. `target/debug`) that are prepended to
PATH inside every door, so `orbit` inside a tree is that tree's build. The
tool must make "which binary am I running" cheap to answer (e.g. listing
the resolved binaries, a variable the prompt can show, the session's
status bar naming the tree), and must warn when a declared directory does
not exist yet rather than letting commands silently fall through to
installed copies.

## A5. Aliases yield to the user; activation is re-entrant

Non-tool environment keys a repo declares (e.g. `PORT`, `RCT_METRO_PORT`,
`ORBIT_CONFIG`) are applied only when the invoking shell does not already
set them; a force flag overrides; the tool reports what it kept. However,
keys that a *previous activation of the tool* set must be treated as the
tool's own and replaced — entering tree B's shell from inside tree A's
shell must yield B's values, not A's.

## A6. Tool-owned directory, nothing to gitignore

Per-tree generated artefacts (sockets, state, rendered config, logs) live
in a directory the tool owns inside the tree (`.wt/`), and the tool keeps
it — and any file it renders — out of `git status` without the repo
having to change its `.gitignore`. Rendered file templates see the fully
assembled environment (aliases included) and are (re)rendered on every
door so a deleted file is restored before anything depends on it.

## A7. Resources die with the tree, even unrecorded ones

Anything a tree declares with a destroy recipe is torn down when the tree
is removed. This includes resources the application created on its own
without going through the tool (a dev server that lazily starts a
container): on removal, every declared resource's existence probe is
consulted and hits are destroyed.

## A8. Registration is consent; no content-trust protocol

Registering a repository authorizes its declared tooling to run;
registration shows what is declared. There is no hash-tracking of the
repo's config, no separate trust verb, no refusal when the file changes.
Rationale: shipped adapters already execute repo-controlled code (`npm
ci`, `cargo test`) unconditionally, so guarding only the config file was
false comfort, and it taxed agents editing the config in their own trees.

## A9. Concrete acceptance targets

Three real repositories must work, with configuration files of roughly
the shape attached (`orbit.wt.toml`, `orbitapp.wt.toml`,
`orbitcloud.wt.toml`). The schema may change if a better one is found, but
every need those files express must be expressible:

- orbit (Rust workspace): per-tree daemon config rendered from a template;
  `target/debug` on PATH; tasks overriding adapter defaults because the
  workspace's default-members differ from CI; the daemon as a tree-bound
  resource with exists/run/destroy.
- orbitapp (Expo/React Native): one port alias (`RCT_METRO_PORT`); a task
  that links a sibling checkout into place (`$WT_REPO/../orbit`) before
  another task runs.
- orbitcloud (.NET Aspire + two npm apps): seven port aliases in
  `Section__Key` form; copying gitignored per-developer files into new
  trees; a custom `sync`; a resource whose `run` is a no-op and whose
  `exists`/`destroy` are multi-line shell computing a hash of a path.

## A10. Coding agents are first-class users

Every command must be scriptable: `--json` with a stable envelope,
non-interactive unless attached to a TTY, stable exit-code classes, and
error messages that name the remedy. Agents will create trees, run tasks,
and open sessions without a human watching.

---

# Addendum 2 — decisions taken after the first design review

Binding. These resolve the questions the first design review raised.
They are settled.

## A11. `create` runs `sync`; `verify` is explicit

R3's bar ("the project's own declared verify step succeeds") is met by
the verify step being available and runnable on a freshly created tree;
creation itself runs only dependency installation. Full test suites take
minutes (orbit: ~15) and agents control wall-clock. `wt new --verify`
runs it as part of creation and reports its outcome distinctly.

## A12. Package-manager caches are assumed concurrency-safe

cargo, npm, pnpm, yarn, bun, uv, poetry, pip, dotnet and go lock their
own shared caches. wt does not serialise adapter `sync` tasks across
trees or repos and does not isolate caches. R5's "toolchain cache locks"
refers to *per-tree build outputs* (target/, node_modules, bin/obj),
which are per tree by construction.

## A13. Application behaviour is not enforced

wt supplies distinct coordinates to every tree and offers declared
`lock`s for repos that want hard mutual exclusion. Whether an
application honours injected values is out of scope (the problem
statement's non-goal "process supervision" covers it). S2's "refuses
loudly" is satisfied by: distinct values always, a declared lock when
the repo wants refusal, and port-bound findings in `doctor`.

## A14. Bounded runtime applies to wt's control plane

Every operation wt performs on its own behalf (git, probes, locks,
state) is bounded and has a timeout. Child processes the user asks for
(one-shot commands, tasks, shells, sessions) run as long as they run;
tasks may declare `timeout`. Idempotent re-run applies to lifecycle and
state verbs (register, new, sync, remove, prune, doctor), not to running
a user's command twice.

## A15. Keep the approved verbs, schema keys and variable names

This command surface is settled and is not to be renamed:

    wt register | unregister | clone
    wt new <label>/<name> [--branch B] [--from REF] [--detach] [--no-sync] [--verify] [--no-open] [--no-attach]
    wt list | remove | prune | path | doctor
    wt run <task> [target]            # declared tasks; sync/test/lint/fmt/build are aliases
    wt exec [target] -- <cmd…>        # one-shot door
    wt shell [target]                 # interactive door
    wt env [target] [--sh|--dotenv|--json]
    wt open [target] [--agent X] [--no-attach] [--all]  /  wt close [target|--all]
    wt destroy <task> [target]  /  wt refresh <task> [target]

New verbs may be added (`adopt`, `forget`, `which`, `tasks`, `config`,
`locks`). `.wt.toml` keys stay as they are (`ports`, `env`, `bin`,
`copy`, `files` with `content`/`source`/`marker`, `task.*` with
`run`/`exists`/`destroy`/`needs`/`lock`/`name`/`tied_to`/`env`/`cwd`/
`timeout`/`description`, `dirs."sub"`, `adapters`); new keys may be
added (`ready_within`; `scope` is NOT introduced — `tied_to`
stays). Variables stay: `WT_LABEL`, `WT_NAME`, `WT_NAME_SNAKE`,
`WT_NAME_SHORT`, `WT_TARGET`, `WT_BRANCH`, `WT_ROOT`, `WT_REPO`
(canonical path), `WT_HOME`, `WT_PORT_BASE`, `WT_PORT_<NAME>`,
`WT_SESSION`, `WT_SELF`, `WT_TASK`; new ones may be added (`WT_SLOT`,
`WT_BIN`, activation metadata). The canonical tree is named
`canonical`. The managed exclude block keeps its marker text
`# >>> wt managed >>>` / `# <<< wt managed <<<`. The three attached
configs must parse unchanged.

## A16. No state migration; a clean home is acceptable

The existing `~/.wt` holds no worktrees and no resources (three
canonical entries only, created today). The new implementation may
require a fresh `$WT_HOME`; it must detect an old-format home and refuse
with the remedy "move or delete it, then re-register". Port block
geometry may change. The JSON envelope may change shape; no automation
depends on the old one.

## A17. `open` without tmux

When attached to a TTY, `wt open --agent X` without tmux runs the agent
in the foreground and says so; with `--no-attach`, `--all`, or no TTY
it fails with a remedy. `list` reports sessions as unknown when tmux is
absent. (This is the existing behaviour; R13 is satisfied by the clear
message.)

## A18. Three crates, state under `$WT_HOME`

The pure core and the effects layer are separate crates; CLI and
orchestration share the binary crate. All state wt needs to tear a tree
down (resource snapshots, coordinates, rendered-file inventory) lives
under `$WT_HOME`, keyed by tree identity, never only inside the tree.

## A19. The shell door's promise is at spawn; rc files are detected, not fought

`wt shell` starts the user's shell with the assembled environment. Shell
startup files may alter PATH or other keys; wt cannot prevent that
without owning the shell. The binding promise for the shell door is:
the environment *handed to the shell* equals every other door's, and
`wt doctor` (and the shell banner) detect and name the case where the
running shell's PATH no longer begins with the exported shim-plus-bin
`WT_PATH_PREFIX`
(`PATH_NOT_SHADOWED`, remedy: the `shell-init` guard or fixing the rc
file). A1/A4 are read with that qualification for interactive shells
only; tasks, `exec`, and the agent command inside a session are
unaffected because they do not source rc files.

## A20. Passthrough doors have no JSON envelope

`exec`, `shell`, and an attaching `open` hand stdout/stderr and the exit
code to the child; they refuse `--json` with a remedy. The machine
contract for those cases is the exit code plus `wt env --json` (what the
child will see) and `wt run --json` (captured output, one envelope).
A10 is read with that exemption.

## A21. Teardown never falls through to an installed binary

A resource snapshot records, for each recipe, the absolute path the
first argv word resolved to at snapshot time when it resolved inside a
declared `bin` directory. At teardown, if that path no longer exists the
recipe is not run; the resource is marked orphaned with the remedy
"rebuild the tree's binaries or destroy by hand". wt does not stage
copies of binaries or rendered files under `$WT_HOME`; rendered file
*contents* referenced by a recipe's environment may be preserved in the
snapshot if small (≤ 64 KiB), otherwise the same orphan rule applies.

## A22. A resource without `run` is "declared"

A task with `destroy` and no `run` is a resource the application
creates. `wt run <it>` succeeds with the notice "declared; created by
the application" and leaves it `declared`/`present` per the probe;
`refresh` destroys if present and returns to `declared`. The orbitcloud
acceptance config has been edited to drop its `run = "true"`.

## A23. Render race with a concurrent human edit is an accepted residual

Between wt's hash check of a rendered file and its rename, a human
editor may save the same path. wt holds its render lock against other
wt processes, not against editors; the window is documented and the
provenance header tells the human the file is regenerated. No further
mechanism is required. 

## A24. Sessions hold the gate only while their inner door runs a task

A tmux session's long-lived process is the agent or shell, launched
through an inner `wt exec`; that inner door holds the tree gate only
while *wt itself* is mutating (render/inventory), then releases it
before handing control to the child. Sessions are therefore closed by
`remove` under the exclusive gate as specified; the gate is not a
liveness lease for sessions — tmux is. One-shot `wt exec` and `wt run`
keep the gate for the child's lifetime as before. (Settles the
deadlock; the specification defines the exact hand-off.)

## A25. Teardown of a missing tree never runs a recipe that names a tree binary

No recipe grammar restriction. At instance-freeze time the snapshot
records the inventory of executable names found in the tree's declared
`bin` directories. At teardown, if the tree root is gone (or any
recorded `bin` directory is gone), the recipe is run only if none of its
words (split on whitespace and the shell metacharacters `;|&()<>`,
quotes stripped) equals a recorded executable name; otherwise the
resource is `orphaned(exe_missing)` with the by-hand remedy. The tree's
`bin` directories are never on PATH during such a teardown. Recipes in a
repo with no `bin` declarations are unaffected. (Replaces the pinning
rule; settles R3/S3.)

## A26. No reincarnation over an undestroyed predecessor

Tombstones are always record-free: a tree's resource records are gone
before its tombstone exists. `wt new` for an address whose *live* entry
has lost its directory but still has resource records refuses with
`TREE_MISSING_PENDING` and the remedy `wt prune --records
<label>/<name>`. A reincarnation therefore always starts from a
record-free tombstone (or a record-free missing entry), inherits its
coordinates and identity values, and the tombstone is deleted — no
predecessor chain is kept. (Settles R4/S4; wording aligned with the
final design's T6/T7.)

## A27. Hostile rename of a tree directory during `remove` is an accepted residual

`remove` verifies `.wt/tree_id` and the common gitdir through a directory
fd under the exclusive gate immediately before the first destructive
step. A non-wt actor renaming the directory between that check and
git's own open is documented and not defended against. (Settles R5.)

## A28. Repo-tied snapshots carry no tree-specific environment

Repo-tied recipes may not reference tree-specific variables (already a
validation rule); their snapshot `env` therefore excludes `WT_ROOT`,
`WT_TARGET`, `WT_NAME*`, `WT_SLOT`, `WT_PORT_*`, `WT_SESSION`, `WT_BIN`,
`PATH` and is compared for agreement on what remains. (Settles R6/S5.)

## A29. Path-derived resource identities are shared by whatever occupies the path

A recipe that derives a resource's identity from `$WT_ROOT` (orbitcloud's
Aspire container names are SHA256 of the AppHost path) names the same
external object for every checkout that ever occupies that path. When
the predecessor's records are cleaned up after the directory was
replaced, destroying that object also affects the replacement; this is
the recipe's own semantics and is accepted. wt's guarantee is narrower:
it never *runs* in the replacement directory and never puts the
replacement's binaries on PATH (A25/T5). (Settles T5.)

## A30. Pragmatic simplifications of crash-consistency machinery

Decided by weighing hot-path cost against failure likelihood. These
apply over the corresponding SPEC text; the spec's guarantees are weakened exactly as stated here and no
further.

1. **Rendering is single-phase.** No `intent` record. A rendered file is
   wt's iff its bytes hash to the recorded `hash`; a crash between
   writing the file and recording its hash makes the next door report
   `RENDER_ONTO_USER_FILE` with the remedy `rm <path>`. The render lock
   and the tracked/symlink refusals stay.
2. **State RMW holds its lock across read-modify-write.** No `gen`
   compare-and-retry; `STATE_CONTENDED` does not exist. RMW locks remain
   leaf locks held for the duration of one read-modify-write only.
3. **Failpoint tests are representative, not exhaustive.** One named
   failpoint per lifecycle verb (`new` after `worktree add`, `sync`
   mid-task, `remove` after destroy before `worktree remove`, render
   after write) plus the resource destroy→record-drop boundary. The
   resume/recovery *behaviour* specified for every transition still
   holds; only the test matrix is reduced.

Everything else in the crash-consistency model (atomic writes, derived
phase, resume-from-first-unfinished-step, op claims, identity files,
teardown snapshots, tombstones, the lock order) stands.

## A31. Minimality review adopted

An independent review with no access to the design history assessed
every mechanism for BUILD / SIMPLIFY / CUT against R1–R13 and A1–A30.
Its verdict table is adopted in full, with two exceptions and the clause
edits it names:

- Exceptions: (1) `shell` keeps the shared gate, at no cost — the gate
  fd survives `execvp` (shells do not close inherited fds), so
  `wt remove` of a tree someone is sitting in reports `TREE_IN_USE`
  naming the shell; (2) the squat probe stays bind+connect on v4 only.
- A28 is read as "stripped", not "compared for agreement": there is no
  repo-tied agreement mechanism; the invoking tree's stripped
  declaration is effective until an instance is frozen.
- A30's "op claims stand" is read as: one op record in the tree state
  file; no registry claim and no `op_id`.
- The door parent process is dropped: passthrough doors `execvp` with
  the gate fd inherited; a child that closes inherited fds releases the
  gate early — accepted residual alongside A23/A27.
- `forget` is folded into `remove`; the session hand-off ticket is
  replaced by `wt exec --no-gate` (prelude, release, `execvp`), honoured
  only under `$TMUX`.
- Resources have three states (`declared`, `present`, `orphaned`) plus a
  frozen `instance`, `external`, `last_probe`, `last_error`; probes
  decide; no persisted op records, no preserved inputs.
- Activation keeps `applied`/`prior`; `deactivate` restores without
  comparing current values (a user edit to a tool-set key inside a door
  is overridden by the next door; L3 is withdrawn).
- Ports are recorded as an append-only name→index map.

The review's under-built items are also adopted: probes must exit
≥ 2 on infrastructure errors and the orbitcloud acceptance config gains
`docker info >/dev/null 2>&1 || exit 2;` before its `exists`; `wt remove
--keep-orphans`; `PATH_OCCUPIED` on `new`; lockfile drift against the
base branch in `list`; `WT_BRANCH` documented as at-spawn; `.wt/logs`
kept to the last 20 per task.

## A32. Human output is complete and JSON is opt-in

Every verb has an intentional human rendering in text mode. JSON envelopes
are emitted only when the caller passes `--json`, subject to A20's
passthrough exception; there is no generic pretty-printed JSON fallback.
Human output does not change shape when stdout is redirected, apart from
colour being omitted according to `--color`. Summaries start with what
happened, align concise lower-case facts underneath, and include an
actionable `next` line for expected states such as a bin directory that
does not exist before the first build. Tables are reserved for commands
whose answer is tabular; `path` and `which` remain one-value
answers. Rationale: the default interface is read by developers, while the
stable envelope remains an explicit automation contract.

## A33. Sessions are provisioning; agents are work

`wt new` completes the tree, prints its summary, then creates its session and
attaches when the session attachment predicate permits. A session without an
agent runs the same interactive shell selected by `wt shell`; `wt open` never
refuses merely because no agent is configured. `wt new --no-open` creates no
session, while `--no-attach` still provisions one. `wt new` has no agent flag;
`wt open --agent X` remains the explicit one-off agent selection. Rationale: a
detached shell is inert, free, and reversible provisioning, while starting an
agent spends resources and may cause work in the repository.

Session provisioning remains subordinate to tree creation. If creating the
session fails after the tree is ready, `new` reports a warning with `wt open
<target>` as the retry, returns the complete tree result, and exits 0. Rationale:
automation must not lose the durable result of the primary operation because an
optional navigation aid failed.

## A34. Agent recipes run only on session creation

An agent's `start` recipe runs only when wt successfully creates the tmux
session. Attaching or switching to a live session never runs an agent recipe.
A tree's agent field is written only for a `start` recipe that wt launched; it
is the durable intent used for a later `resume`. Rationale: attaching is a
navigation operation and must not duplicate paid or acting work.

## A35. The session backend is declared once

`session.backend` is `"tmux"` or `"none"`. When the key is absent,
`register` checks once for tmux 3.2 or newer, writes the result to the user
configuration, and reports the choice and how to change it. A home registered
before this key existed performs the same one-time resolution on its first
`new`, `open`, or `close`, so upgrading cannot silently disable sessions. Later
commands obey the written setting and do not infer capability from the host. The session
settings also contain `attach` and the optional `agent`; the removed
top-level agent setting is an error whose remedy names `session.agent`.
Rationale: stable configuration should not change behavior as PATH or host
tooling changes between invocations.

## A36. `open --all` provisions every live tree and resumes intent

`wt open --all` ensures a session for every live tree and attaches to none.
When a tree records an agent, wt uses that agent's `resume` recipe; otherwise
it starts the explicitly configured `session.agent` or a shell. Rationale:
the tree registry defines the live fleet, while the recorded agent denotes
continuation rather than fresh work.

Failures are contained per tree: every remaining tree is attempted, each
failure is present in the session data, and the final exit class is the worst
outcome. Rationale: a batch that has already mutated earlier sessions must
report partial progress instead of discarding it or making retries repeat the
same hidden prefix.

## A37. A disabled backend refuses explicitly

A17's foreground-agent fallback is replaced. With `session.backend =
"none"`, `open` and `close` refuse with a message naming the setting and the
value that enables tmux. `list`, `remove`, and `prune` execute no tmux process.
Rationale: falling back from a declared session model to an unrecorded
foreground process makes configuration lie and changes lifetime semantics.

## A38. A wt-owned tmux status line is deferred

wt does not set `status-left`, and there is no `session.status_bar` setting.
Tree identity remains available through the tmux session name and the wt
environment. A designed wt-owned status line may be added later together with
its ownership and composition rules. Rationale: an inert setting promises
control it does not provide, while overwriting a developer's status line
without a composition design is not acceptable.

## A39. Adapter seeds never become byte copies

**Seed-related parts superseded by A53.**

Repository-declared seeds continue to prefer reflinks and fall back to copying.
Adapter-contributed defaults such as Cargo's `target` and Python's `.venv` are
reflink-only: when cloning is unavailable, wt removes any partial destination,
skips the entry, and reports `SEED_SKIPPED_NO_REFLINK`. Rationale: an adapter
default must not turn routine tree creation into an implicit multi-gigabyte
copy on a filesystem that lacks cloning.

## A40. Every declaration is a claim, and each kind is enforced differently

`commands`, `env`, `ports` and `files` all declare names a tree owns. Each is
enforced by the mechanism that matches how it fails: a command name refuses
rather than resolving elsewhere, an environment name overrides what was
inherited, a port is allocated from the tree's own slot, and a file path is
rendered and re-rendered. `vars` are deliberately not a claim: they never
leave the configuration. Rationale: one noun the user learns once explains
four behaviours, and the differences stop looking arbitrary once they are
read as remedies matched to failure modes — a missing command fails silently
and wrongly, while a missing variable fails loudly at first use.

## A41. A tree owns command names, not merely binary directories

`bin` declares where a tree's executables are; `commands` declares which names
it provides. A declared-but-unbuilt name resolves to a wt-owned entry under
`<tree>/.wt/shims/` that refuses with the reason and the remedy, naming the
installed path it declined to run, and offering it as an explicit absolute
path. Names the tree does not claim resolve normally.

Claims are declared, never sniffed: deriving them from a build directory's
contents yields build-script and hashed test binaries, and yields nothing at
all before the first build, which is exactly when the guarantee is needed.
Adapters may contribute `commands` at the adapter layer, where it is a
declaration like any other and is overridable.

Shims interpose on every invocation rather than being bypassed once the real
binary exists. Rationale: interactive shells cache the resolved path of a
command and re-resolve only when the cached path fails. A shim ordered after
the build directory would be cached on first use, keep succeeding as a
refusal, and so continue refusing after a successful build — replacing a
silent wrong answer with a persistent false one.

The guarantee is scoped to `PATH` resolution. A shell alias or function
outranks `PATH` and cannot be displaced; `doctor` reports one shadowing an
owned name rather than the promise pretending to cover it.

## A42. A declared environment value wins over an inherited one

`[env]` aliases override the parent environment by default; the prior value is
recorded and restored on deactivation, and the override is visible in `wt env`.
`--force-env` is no longer required for this and the "kept" outcome disappears
for declared keys. Rationale: the previous default let a value exported by a
login shell — a production `DATABASE_URL` is the motivating case — silently
defeat the repository's declaration, which is the precise hazard the
declaration exists to remove. This is a behaviour change for anyone relying on
inheritance reaching a tree, and is announced as one.

## A43. Ports are configuration inputs, not environment variables

`WT_PORT_<NAME>` and `WT_PORT_BASE` are no longer exported into a door. Ports
remain addressable in configuration and are reported by `status`, `list` and
`env`. Rationale: an application reads the name the repository declared for
it, so exporting wt's own spelling puts wt's vocabulary into the application's
environment — the thing `.wt.toml` exists to prevent. Identity values remain
exported because they are typed by hand in a shell.

## A44. Templates evaluate constants and functions

`${…}` is the sole evaluation form; `$$` is a literal dollar and a bare `$` is
literal text. Inside it, a bare name reads a `vars` constant and a
parenthesised name calls a wt-provided function: `home()`, `root()`, `repo()`,
`branch()`, `target()`, `name_snake()`, `name_short()`, and the dotted constant `ports.<name>`.
Rationale: the previous spelling gave tool-provided values and user-defined
values one syntax and one namespace, so a reader could not tell which was
which. Function syntax marks what wt computes about the tree; a declared port is a
dotted constant, `ports.<name>`, because looking one up is a lookup and not an
allocation — the value is fixed when the name is declared and repeats
identically. The remaining distinction is answered by whether the name appears
in `[vars]`. A shell-string recipe is never templated at all, so wt's syntax
and the shell's never share a string; a recipe needing a non-exported value
declares it in the task's own `env`, and the argv form, having no shell, is
templated per element.

## A45. `copy` and `seed` express required and opportunistic population

**Seed-related parts superseded by A53.**

Supersedes A39: the reflink-only rule now follows from what a seed *is*, not
from who declared it. `copy` names paths that must be present in a new tree and are populated
whatever the filesystem supports. `seed` names paths worth having only if
cloning is available, and is skipped with a notice otherwise. Rationale: the
two look like one primitive differing only in mechanism, but the guarantee
differs: a local secret must arrive on a filesystem that cannot clone, while a
build cache must not turn tree creation into a multi-gigabyte copy on exactly
that filesystem. Documentation describes them as required versus opportunistic
rather than by mechanism.

## A46. Adapters own per-ecosystem cheapness

Making a fresh tree cheap is adapter knowledge expressed as ordinary
configuration at the adapter layer — a shared build directory for Cargo
intermediates, a content-addressed store for Node, and a global cache for
Python. An adapter must never share a build *output* directory across trees: doing so
would make one tree's binary answer to every tree's name and would defeat A41.
Rationale: cloning build output is a filesystem-level answer to a question the
ecosystem usually answers better, and the ecosystem's answer is both cheaper
and portable to filesystems without cloning.

## A47. The task named `build` runs automatically after creation

`wt new` starts the effective `build` task once the tree is ready; `--no-build`
opts out. With tmux it runs in a second session window and `new` returns
immediately. With `session.backend = "none"` it runs synchronously because no
background host exists; JSON waits and returns one success or partial-failure
envelope.
Setup that is more than compilation is expressed by giving `build` a `needs`
list, not by a separate hook mechanism. Rationale: the task graph already
carries dependencies, ordering, composition and a closed verb set that wt can
reason about; a parallel array of opaque commands would duplicate it while
telling wt nothing about what each command is for. Because wt knows a build is
running, a shim can say so instead of reporting the binary as merely absent.

## A48. Teardown runs the plan that created the resources

Removal destroys recorded resource instances newest first, using a durable
recording sequence rather than reversing the declared dependency graph. The
frozen-instance rule—so a branch that edits a recipe cannot redirect teardown
at something it never made—predates this requirement. A48 adds only the
newest-first ordering and does not newly justify the absence of an approval
gate for lifecycle recipes.

## A49. A session carries the bootstrap home and has a startup observation window

`wt` creates a session with the resolved wt home passed explicitly, then
observes it for a bounded 250 ms startup window. A session that exits in that
window is reported as `SESSION_CREATE_FAILED` with captured pane output; the
window is not proof of later survival. Rationale: tmux copies no arbitrary
variable from client to session on an existing server, so an inner `wt` would
otherwise inherit whatever home the server captured when it started, resolve
the wrong state, exit, and have its session destroyed — while wt reported
success, because `new-session` had itself succeeded. Two homes are legitimate
and distinct: the bootstrap home says where the tree is defined, and a
tree-local home would say what commands *inside* the tree address. **Not yet
implemented:** the `WT_*` env namespace is still closed to repositories, so a
tree cannot declare its own home and wt cannot yet give an agent working on wt
a registry of its own. Carving that one exception is the remaining step; the
bootstrap half above stands on its own and is what fixes the reported defect.

## A50. Expected states leave the door entirely

`BIN_DIR_MISSING` is no longer emitted by a door, and no command summary
carries a `next` line of guidance. A missing declared `bin` directory is
reported by `doctor`. `doctor` continues to print each finding's own remedy on
a `next` line: that is the verb whose entire purpose is to say what to do, and
the line is per finding rather than appended to an unrelated summary.
Rationale: A41 prevents the harm the notice existed to warn about, so the
notice became advice about a condition that is true of every freshly created
tree — a warning that fires every time trains its reader to ignore the one
that matters. The door enforces; `doctor` reports.

## A51. The removed agent setting is no longer named

A configuration carrying the former top-level agent key is rejected by the
ordinary unknown-key rule rather than by a bespoke check naming its
replacement. Rationale: the bespoke check defends configurations that do not
exist, and the settings parser already refuses unknown keys.

## A52. Cargo seeds nothing; the ecosystem's own cache is the answer

**Seed-related parts and the sccache-only answer are superseded by A53.**

The cargo adapter contributes no `seed`. Rationale: a reflink costs nothing
per byte but one syscall per file, and a Rust build directory is hundreds of
thousands of files — a real 74 GB `target` took over a minute to reach 0.3%
of itself, on a filesystem where cloning works. "Free per byte" is not the
same as cheap, and the adapter default turned every `wt new` on a mature Rust
repository into a multi-hour operation with no output.

Rust's own answer is a shared compilation cache: `sccache` caches individual
crate compilations across trees with no copying and no output collision, and
the existing accelerator nudge already points at it. A repository may still
declare `seed` itself — the reflink-only rule of A45 continues to apply to it
— but a default that is catastrophic on large repositories is not a default.

This does not extend to sharing a build *output* directory across trees: that
would make one tree's binary answer to every tree's name and defeat A41. Only
the compilation cache is shared.

Creation stays cheap without the seed because A47 moved `build` into the
background, so a cold first build is not a wait. The same reasoning applies to
`node_modules` and `.venv`, which are also file-count heavy and also have
ecosystem-native shared stores; those defaults are left in place pending the
same measurement.

## A53. Build output is never copied; ecosystem caches make trees warm

`seed` is removed from the configuration grammar, adapter schema, merge
rules, execution, notices, and persisted materialization kinds. The ordinary
unknown-key rule refuses a configuration that still declares it; there is no
compatibility shim or migration path. `copy` remains for small, required,
gitignored developer files and is a contained recursive byte copy: regular
files are copied with their mode, directories are walked, and symlinks are
recreated without following them outside either root.

The measurement that retires the primitive is file-count dominated. wt's
per-file cloning of a 74 GB Rust `target` containing 252,767 files projected
at seven hours. A whole-subtree filesystem clone copied 73,034 files in 12.6
seconds. Optimising the per-file loop would preserve the wrong ownership
model, so wt no longer copies build output at all.

Each ecosystem supplies the sharing mechanism. Cargo 1.91+ separates
`target-dir` outputs from `build-dir` intermediates. Both cargo adapter tools
set `CARGO_BUILD_BUILD_DIR = "${home()}/cache/cargo-build/${label()}"`, one
path per repository. `CARGO_TARGET_DIR` remains unset so each tree's binary
stays in its own `target/`, preserving A41. Repository and user `env` layers
may override the adapter value by the ordinary merge rule. The split was
verified with Cargo 1.94: intermediates went to the shared directory, the
binary stayed in `./target/debug/`, concurrent warm rebuilds in two crates
blocked zero times, and only the cold dependency build serialised. pnpm uses
its content-addressed store and hard-links into `node_modules`; uv uses its
global cache and hard-links into `.venv`.

The closed template function set adds `home()`, which resolves to the same wt
home exposed as `WT_HOME`. A cold first build remains non-blocking because A47
runs `build` in the background.

## A54. Consent is gated on loss, not on the verb

`wt remove` prompts only when the removal destroys something that cannot be
recovered once it finishes. Two things qualify: uncommitted changes in the
worktree, and commits reachable only from a branch this run deletes. A clean
tree whose commits are on a remote is removed without a prompt, as are the
missing-directory and replaced-directory paths, which delete no work at all.

The prompt that fired on every removal carried no information. A user who is
asked to confirm the harmless case learns to answer `y` without reading, and
the one removal in fifty that discards a day's work is answered the same way.
Consent is worth asking for only where a wrong answer costs something.

The two flags stay on separate axes. `--force` permits a work-losing removal
*and* supplies its consent, so it never prompts: the user who types it has
already said the thing the prompt would ask. `--yes` only suppresses prompts
for removals that are permitted anyway; it does not unlock the destruction of
uncommitted work, because it is a global flag that lands in aliases and agent
command lines where the tree in question was never examined. Without a TTY a
work-losing removal is refused with `TREE_DIRTY` (5) and the remedy names
`--force`; `CONFIRM_REQUIRED` no longer arises from `remove`.

Removal now deletes the tree's local branch when its commits are on a remote,
because the branch is then a name that can be recreated from `origin` and
leaving it behind litters every repository wt is used on. When the commits are
not on a remote the branch is the only thing keeping them reachable, so it is
kept and the summary says so with the count. `--delete-branch` deletes it
regardless, `--keep-branch` never deletes it, and an adopted tree's branch is
never deleted by default because it predates wt's knowledge of the repository.
This is the sole reason `remove` may prompt about commits: with the branch kept
by default, unpushed commits survive the removal and need no consent.

The rule is `remove` only. `unregister`, `destroy` and `refresh` keep their
unconditional prompt: the first tears down a whole registration and is rare
enough that the keystroke costs nothing, and the other two are aimed at a
named resource whose teardown is the user's whole intent.

Resource teardown is not counted as loss. Removal runs every tree-tied
`Destroy{teardown}` without asking, on A7's premise that a declared resource is
reproducible from its declaration. A teardown recipe that destroys data no
declaration can rebuild is outside the model, as it already was.

## A55. `rm` and `ls` are accepted spellings

`wt rm` resolves to `remove` and `wt ls` to `list`. Both are hidden from
`--help`, the canonical names in §14.1 are unchanged, and A15's settled surface
is not reopened: these are spellings of approved verbs, not new verbs. The
unknown-subcommand allowlist accepts them, so neither reaches the
nearest-name tip — which, ranking by edit distance alone, answered `rm` with
`fmt, run, env` and never suggested `remove`.
