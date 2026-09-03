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
`WT_TARGET`, `WT_NAME`, `WT_BRANCH`, `WT_BIN`, `WT_PATH_PREFIX`, and `PATH`,
and is compared for agreement on what remains. A70 removed the older
tree-specific exports rather than leaving inert stripping rules for them.
(Settles R6/S5; export list updated by A70.)

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
set `CARGO_BUILD_BUILD_DIR = "{{home()}}/cache/cargo-build/{{label()}}"`, one
path per repository. `CARGO_TARGET_DIR` remains unset so each tree's binary
stays in its own `target/`, preserving A41. Repository and user `env` layers
may override the adapter value by the ordinary merge rule. The split was
verified with Cargo 1.94: intermediates went to the shared directory, the
binary stayed in `./target/debug/`, concurrent warm rebuilds in two crates
blocked zero times, and only the cold dependency build serialised. pnpm uses
its content-addressed store and hard-links into `node_modules`; uv uses its
global cache and hard-links into `.venv`. (The per-repository keying of the
cargo build directory proved unsound in production and is revised to
per-tree by A64; the rest of this addendum stands.)

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

## A56. Scope is a named axis: tree, repo, machine

Every fact wt holds has exactly one scope — tree, repo, or machine — and the
scope decides where the fact is declared, where its state lives, which
variables its snapshots may carry, and what tears it down. Tree and repo scope
exist today (`tied_to`, per-tree state files, `_repo.json`, the tree-variable
stripping rule); machine scope exists implicitly in settings (geometry,
agents, the session backend, lock waits) but nothing declarative can name it.
A57–A63 extend existing mechanisms along this axis rather than adding parallel
ones: capacity for a named lock is a machine fact expressed through the
ordinary layer merge (A59), an exclusive resource occupies one slot of a wider
arena (A60), and a machine-tied task is a resource whose arena is the host
(A61). The stripping rule generalises: a repo-scoped snapshot carries no
current tree-specific keys (A28/A70); a machine-scoped snapshot additionally carries no
repo-specific keys (`WT_LABEL`, `WT_REPO`, and the `label()` / `repo()`
functions). Rationale: three consumer requests — lock capacity, exclusive
holders, machine-scoped onboarding — are one missing concept; naming the axis
once keeps them one family with one vocabulary.

## A57. A task may be only an aggregate

A task must declare one of `run`, `destroy`, or `needs`; a task with none
remains `CONFIG_INVALID`. A task with `needs` and neither `run` nor `destroy`
is an aggregate: running it runs its needs, in plan order, and nothing else —
the same node shape §6.2 already builds for root composites. An aggregate
accepts `needs` and `description` only; `lock`, `env`, `cwd`, `exists`,
`timeout` or `ready_within` on one is `CONFIG_INVALID` with a message saying
the key would guard nothing and belongs on a task that runs. Rationale: the
old rule forbade users from writing what wt writes for itself, and the workaround
(`run = "true"`) is noise in the one file most worth reading as
documentation. Named entry points — `wt run setup` for onboarding,
`wt run check` for grouped verification — are the shapes that want it.

## A58. `wt run` accepts trailing arguments

`wt run <task> [target] -- <args…>` (and the `test`/`lint`/`fmt`/`build`
aliases) resolves exactly one argument target by traversing from the invoked
root. A node with a `run` recipe is the target. A wt-composed run-less node is
transparent when it has exactly one need, preserving the ergonomic meaning of
an alias or a single-adapter root; with two or more needs it refuses with
`ARGS_ON_COMPOSITE` (2), naming its direct constituent task ids so the caller
can choose one. A user-declared aggregate (A57) refuses with that error
regardless of fan-out, naming its needs, because adding one declared need must
not silently redirect arguments. A resource reached anywhere refuses with
`ARGS_UNSUPPORTED`: its `run` is a state transition replayed from snapshots,
not a parameterised invocation. Only the resolved recipe receives `<args…>`;
nodes run before it never do. An argv recipe receives them appended
element-wise. A shell-string recipe receives them as positional parameters —
the recipe writes `"$@"` where they belong — preserving A44: the string is
never touched. A shell-string recipe that receives args but whose text
contains none of `$@`, `$*`, `${@`, `${*` or `$<digit>` is refused with
`ARGS_UNSUPPORTED` (2) naming the fix; the scan is lexical, and a `"$@"`
inside a comment satisfying it is an accepted residual. Args appear in the
log header, in `--dry-run`, and in `run --json` data; the usage error for args
given without `--` names `--`. Rationale: "run the declared task, but only this
file" is the most common agent invocation;
without it agents fall back to `wt exec` and lose the lock, the log, the
needs chain and the task `cwd`. Loud refusal beats guessing where arguments
belong in a multi-command recipe or a fan-out composite.

## A59. A named lock has capacity

A new root-only configuration key `locks."<name>"` with fields
`slots` (integer ≥ 1, default 1) and `wait` (Duration, default
`task.lock_wait`) sizes a named lock. The key flows through the ordinary
layer merge, so a repository may suggest a capacity and the user's
`[repos.<label>]` overrides it (the whole entry replaces per name, as
`task` entries do): capacity is a machine fact and the machine's
owner has the last word, by the merge rule that already exists rather than a
bespoke one. Acquisition takes any one of the N slot files, trying each in
order and polling within the deadline; `LOCK_HELD` reports occupancy
("4/4 in use"), the holders, and the remedy (`--wait`, or raising `slots`).
`wt locks` shows `held n/N` and per-slot holders. Lock level 4 and
`lock_plan` are unchanged; a lock with no `locks` entry has one slot and
the §13.3 default wait. Honouring that default is itself a change: the
previous implementation ignored `task.lock_wait` and queued without bound,
so a task carrying `lock` under default settings now fails fast with the
occupancy report instead of queueing invisibly — a behaviour change,
announced as one; `--wait`, the per-lock `wait`, or `task.lock_wait`
restores queueing deliberately. Rationale: "at most N concurrent" is the correct
policy for machine-wide weight — N test containers in a fixed-size VM — and
both previously available options were wrong: no lock exhausts the machine
and a mutex serialises a fleet to one.

## A60. An exclusive resource occupies one slot of a wider arena

A tree-tied resource may declare `exclusive = "repo"` or (with A61)
`exclusive = "machine"`: at most one tree holds an instance at a time,
recorded as `holder {tree, since}` in the arena's state file. Enforcement is
matched to the failure mode, which is silent cross-tree collateral: a
non-holder's record never leaves `declared` — it is neither probed nor
destroyed — so `remove` of a non-holder cannot tear down the holder's
instance, and a non-holder's `run` cannot mistake the holder's instance for
its own. `run` from a non-holder refuses `RESOURCE_HELD` (4) naming the
holder; `wt run <r> --take` destroys the holder's record through its frozen
instance, clears the holder, claims it, proceeds as itself, and prints what
it displaced. `--take` never prompts: displacing a declared-reproducible
resource is not loss (A54) and the flag carries the consent, on the same
axis as `--force`. When no holder is recorded, a run's own probe finding
the resource present makes the running tree the holder with
`external = true` — the first-probe-freezes rule unchanged; a passive
`--probe` observes without claiming, because observation must not seize
ownership. The holder is cleared by teardown of the holder's record, by
`destroy`/`refresh`, and by a confirmed-absent probe (`RESOURCE_GONE`).
The arena entry, not the local configuration, governs teardown: a checkout
whose branch predates the `exclusive` declaration still may not destroy
what another live tree holds, so teardown consults the arena for the key
before running any recipe. The repo-tied tree-variable restriction is
untouched: the gap was a missing concept, not a wrong restriction.
Rationale: "one dev server; retarget it to my tree and tell me who I bumped"
was previously expressible only through configurations that failed silently,
and the consent gate cannot be built inside a recipe.

## A61. Machine-tied tasks and resources

`tied_to = "machine"` declares a task or resource whose arena is the host:
docker running, an authenticated CLI, a base database every repository
shares. Records live in `$WT_HOME/state/_machine.json`, keyed by
`ScopedTask` exactly as `_repo.json` is; two labels declaring the same
scoped task share the record. Declarations are stripped of the current tree-
and repo-specific keys (A56/A70), and the invoking context's stripped declaration is
effective until an instance is frozen — exactly as A31 reads A28 for repo
scope, with no cross-repo agreement mechanism. Machine-tied templates and
recipes may reference neither tree-specific nor repo-specific keys
(validated as the repo-tied rule is today). Machine records are never
touched by `remove` or `unregister`; only `wt destroy` and `wt refresh` act
on them, with their unconditional prompt. No new address form is introduced:
a machine task is declared in `.wt.toml` or `[repos.<label>]` like any other
and referenced from `needs` by name. Rationale: the resource model is
executable onboarding — `exists` is "is this set up?", `run` sets it up, and
the never-prompt-off-a-TTY rule gives an agent a clean failure with a
remedy — but the steps are machine facts; declaring them repo-tied
duplicates and re-probes them per label and gives `unregister` the wrong
semantics.

## A62. A session outlives its agent

wt wraps an agent's `start`/`resume` command so that the pane's process is
the agent and, when the agent exits, the pane becomes the same interactive
shell `wt shell` would start, with the same assembled environment. There is
no `send-keys`, no timing window, and no setting: A34 is preserved (an agent
starts only when wt creates a session), tmux remains the session's liveness
truth (A24), and a session whose agent has ended is A33's statement
continued — a session without an agent is a shell. A dead agent therefore
leaves a prompt in the right directory instead of a vanished session, and
`wt open` remains idempotent. An agent that never started is not an exit:
the wrapper propagates the shell's could-not-start statuses (126, 127) as
pane death, so A49's observation window still reports the misconfiguration
with the captured pane output and no agent is recorded — a config error
must fail loudly, not become a prompt. An agent binary that itself ends
with 126 or 127 is treated as never started; that residual is accepted and
named. Rationale: agents crash and exit; recreating their context is the
cost the session existed to avoid — but only for agents that ran.

## A63. A tree carries user metadata

`TreeRec` gains `meta`, a string map (keys matching `[a-z_][a-z0-9_]*`,
values at most 1024 bytes), set at creation with `wt new --meta k=v`
(repeatable) and edited with `wt meta <target> k=v` (`k=` unsets); shown by
`status` and carried in `list --json` and `status --json`. Keys are opaque
to wt: they are not readable from templates and nothing in wt's behaviour
depends on them. Rationale: teams hang external identity — a ticket — on a
tree, and the registry defines the fleet (A36), so fleet metadata belongs in
it rather than in a side registry; but only as data, because a
template-readable key would create an unset-key failure mode nothing needs
yet.

## A64. The cargo build directory is keyed per tree and dies with it

A53 keyed `CARGO_BUILD_BUILD_DIR` per repository so every tree of a label
shared one intermediates directory. Production falsified the premise that
sharing is safe: Cargo's unit hashes deliberately omit the workspace path
(that is what makes a build directory relocatable), so two checkouts of one
workspace address the same unit slots. Build-script output generated from
workspace-local inputs — protobuf codegen was the observed case — is then
overwritten by whichever tree builds last, and `rerun-if-changed` freshness
is mtime-based while git writes arbitrary mtimes, so the loser reuses the
winner's generation and fails with errors that describe neither tree. The
same collision covers workspace crates' fingerprints and rlibs. The shared
directory also outlived every tree that fed it (77 GB observed for one
repository, four trees), because nothing owned its lifecycle.

Both cargo tools now set
`CARGO_BUILD_BUILD_DIR = "{{home()}}/cache/cargo-build/{{label()}}/{{name_short()}}"`:
one directory per tree, grouped under the label so attribution and reaping
are directory listings. `name_short` is deterministic in `(label, name)`, so
recreating an address readopts its cache warm. wt does not attempt to make
sharing safe with finer invalidation — that would need repository knowledge
(which files feed which build scripts) wt cannot have; cross-tree reuse
belongs to content-addressed layers instead, which is what the sccache nudge
recommends (its `used_if_file` rule now recognises a machine-wide
`~/.cargo/config.toml` `build.rustc-wrapper`, the recommended activation,
alongside `RUSTC_WRAPPER`).

Lifecycle follows the key. `remove` deletes the tree's cache at §11.4 step
11 — only a path under `$WT_HOME/cache` whose final component is the tree's
`name_short`, taken from the door's rendered value when a door exists, else
from the adapter scheme; an override elsewhere keeps its own lifecycle, and
a failed delete is a warn notice (`CACHE_DELETE_FAILED`), not a failed
removal. `prune` reaps `CACHE_ORPHAN` entries — anything under
`cache/cargo-build` that is no registered label's live or tombstoned
`name_short`, which also migrates the retired per-repository layout — and
`doctor` reports them. `list --disk` sizes each tree's cache as `cache_kb`,
because the 77 GB accumulated precisely while attributed to nothing.

## A65. Every `removed: false` carries its reason

`wt remove` resolves addresses by the same §3.2 rules as every other verb. An
unresolvable address is `NOT_FOUND` (3) with the ordinary candidate remedy. The
one idempotent exception is an explicit `label/name` with no live tree and a
matching tombstone: it exits 0 with `removed: false` and an `ALREADY_REMOVED`
info notice. Declining the work-loss prompt likewise exits 0 with
`removed: false` and a `REMOVE_DECLINED` info notice naming the target and
saying that nothing changed. Human output states these reasons.

The old parser silently interpreted a bare tree name as a label and returned
an unexplained success when that fabricated target was absent. Shared address
resolution makes removal agree with the rest of the CLI, while the narrowly
tombstone-backed exception preserves safe script idempotence without hiding a
mistyped or contextless address.

## A66. One template syntax applies to every templated string

`{{name}}`, `{{fn()}}`, and `{{ports.<name>}}` are the sole evaluation forms.
Every `{{` begins an expression, expressions contain no whitespace, and a
malformed expression or an unknown name outside the vars DAG is
`CONFIG_INVALID` at its source location. The function set, vars DAG (including
`VARS_UNKNOWN` for an unknown dependency), and port lookup semantics are
unchanged. Shell-form
`run`, `exists`, and `destroy` recipes are now templated before `sh -c`, with
substitutions inserted verbatim; argv elements continue to be templated
independently. Path-valued shell substitutions therefore need shell quoting,
or the recipe should use argv form.

Dollar signs have no meaning to wt. `${...}`, `$NAME`, and `$$` pass through as
literal text, and the legacy `$WT_*` spelling hints and bare-name refusal are
deleted. A `files` entry may set `template = false` (default true) to render its
content or source verbatim while preserving hash ownership, markers, and mode.
This is the opt-out for Jinja, Helm, GitHub Actions, and other formats that own
`{{`; literal `{{` in any non-file template remains unsupported, an accepted
limitation.

A `task` may set the same `template = false`, which covers its `run`, `exists`,
and `destroy` recipes in either command form. This is the opt-out for a recipe
carrying text owned by a `{{`-using format, such as the Go template in
`docker inspect --format '{{.State.Status}}'`. `name` and `env` stay templated
under the opt-out, because they hold wt's own values and are how an untemplated
recipe receives them: the resource's resolved name arrives as `$WT_SELF` and a
declared port as any `env` entry that reads `{{ports.<name>}}`. A repo- or
machine-tied recipe is still refused for referencing a tree-specific `$WT_*`
name, which reaches the shell whether or not wt templated the string.

(This replaces the original text of this paragraph, which claimed argv form or a
shell-variable splice would carry literal braces. Neither does: argv elements
are validated identically to shell strings, and no templated string can produce
a literal `{{` at all, since the text between `{{` and the first `}}` must parse
as an expression.)

This supersedes A44's `${...}`/`$$` syntax and never-template-shell rule, and
the untouched-shell clause in A58; A44's distinction between declared
constants, functions, and port lookups and A58's argument-forwarding behavior
remain in force. The old design put two rules on either side of the same seam:
a declared value in a shell recipe could silently expand to empty in the shell,
while a rendered shell or Compose file needed a wt-specific dollar escape.
Using syntax disjoint from the shell gives every string one rule and leaves `$`
entirely to the shell or rendered format.

## A67. Session names are sanitised target strings

For a new allocation, `session_name` is the target's display form — `label` for
the canonical tree and `label/name` otherwise — with `.` and `:` mapped to `_`.
There is no `wt_` prefix, truncation, or hash fallback. A collision with any
live or tombstoned session name is `IDENTITY_COLLISION` with the existing
choose-another-tree-name remedy. Existing persisted names and tombstone
inheritance are untouched. Every tmux operation addressing a target uses
tmux's `=name` exact-match form, because an unadorned target prefix-matches.

This supersedes the §3.1 hashed `session_name` formula. A collision now means
two display names differ only where tmux cannot preserve their punctuation; it
is rare enough that refusing the ambiguity is clearer than hiding it behind a
generated suffix.

## A68. `open` is universal and the canonical tree is an anchor

With `session.backend = "none"`, a per-tree `open` performs the ordinary door
and execs the same interactive shell program, arguments, environment, and cwd
as `wt shell`; `open --no-attach` is an exit-zero no-op with a notice because
there is no session to provision, while `open --agent X` refuses with a remedy
naming `session.backend = "tmux"`. `close` is an exit-zero no-op carrying
`closed:false` and a sessions-disabled notice. `open --all` remains tmux-only
and tells callers to open trees individually otherwise. On tmux it skips
canonical trees: the canonical checkout is an explicit anchor, not another
fleet session.

The `session.agent` default likewise does not select an agent for an explicit
canonical open. An explicit `--agent` works there, and a recorded agent resumes
as before. The attachment predicate, startup observation window, and
agent-to-shell wrapper are unchanged. This supersedes §9.4's
`SESSION_DISABLED` refusal for per-tree open and close.

## A69. Automatic build has a backend-independent detached supervisor

After a tree becomes ready, `new` launches its effective `build` task through a
setsid/double-fork supervisor on both backends. The supervisor invokes the
ordinary task path, so `needs`, locks, environment assembly, logging, and
foreground `wt build` behavior remain one implementation. Its pid is recorded
in `BuildState`. It writes `running` before launch and atomically replaces the
status with `ok` or `failed` after the originating CLI has exited. Human output
reports that the build started and names its log; JSON carries
`{started, log, pid}` and never waits. A `running` status with a dead recorded
pid is abandoned: doctor warns `BUILD_ABANDONED` with a `wt build <target>`
remedy, list/status report `abandoned`, and an owned-command shim uses the
ordinary not-built refusal rather than claiming that the build is in progress.
A foreground `wt build` that resets the status to `running` records its own
pid in the same step, so a live foreground run is never judged against the
finished supervisor's dead pid.

Build failure is subsequently surfaced by `status`, `doctor`, and owned-command
shims. The tree state contains no window field and tmux owns no setup window.
This supersedes A47's window mechanism and synchronous none-backend build;
A47's automatic task choice, `--no-build` behavior, and task-graph rationale
remain in force.

## A70. Exported environment separates interface from mechanism

The stable interface is `WT_TARGET`, `WT_LABEL`, `WT_NAME`, `WT_ROOT`,
`WT_REPO`, `WT_HOME`, and `WT_BRANCH`, joined inside a resource task by
`WT_SELF`, the resource's resolved name, which the untemplated recipes of A66
depend on. The mechanism tier — `WT_ACTIVATION`, `WT_PATH_PREFIX`, `WT_BIN`,
and `WT_TASK` — may change as door implementation changes. This supersedes the previous §5.5
export list: `WT_SESSION`, `WT_NAME_SNAKE`, `WT_NAME_SHORT`, and `WT_SLOT` are
deleted exports. Snapshot minimisation strips the remaining tree-specific keys,
including `WT_BRANCH`, at repo and machine scope as appropriate.

Derivations belong to template functions such as `{{name_snake()}}` and
`{{name_short()}}`, which remain. A session name is redundant with the target
after A67; a script addressing its own pane has tmux's `$TMUX_PANE`, and fleet
lookup belongs through `wt list --json`. Shell initialisation therefore keeps
completion and the PATH guard, removes the `wtcd` and `wtsh` helpers, and adds
a guarded `(<target>) ` prompt prefix that is inert outside a door. The built-in
Claude recipes name the bare tree in both directions —
`["claude", "--name", "{{name()}}"]` and
`["claude", "--resume", "{{name()}}"]` — because `--resume` resolves a session
title, so the named start has an exact inverse. (`--continue`, which the recipe
carried first, resumes whatever conversation in the directory is newest, which
is a different session whenever more than one agent has run in the tree.) Codex
keeps `codex` and `codex resume --last`: it has no launch-time naming to invert.

## A71. Editing is a door

`wt edit [target]` enters the ordinary door and replaces wt with an editor at
the tree root. It resolves the command from the templated settings `editor`
key, then verbatim `$VISUAL`, then verbatim `$EDITOR`; no value is `EDITOR_UNSET`, with a remedy
that names `editor`. Like `exec` and `shell`, it is a passthrough door: the
editor receives the full assembled environment and child status, while
`--json` is refused with `JSON_UNSUPPORTED`. Terminal editors and cold GUI
launches therefore see the tree exactly as tasks do. A GUI command that merely
forwards to an existing process cannot change that process's environment;
run configurations must use `wt exec`, or integrated terminals `wt shell`.
Rationale: editing is the remaining everyday entrance into a worktree, and an
editor that starts outside the door silently chooses the wrong binaries,
ports, and rendered files.

## A72. A worktree can outlive wt's adoption of it

`wt forget <target> [--yes]` removes wt's records and owned artifacts for one
live non-canonical tree without touching its directory, branch, or git
worktree registration. Canonical trees use `unregister`. Instantiated
resources, live sessions, and door holders refuse with remedies to destroy or
remove, close, and wait respectively; the refusal names the resources. A record
carrying no instance is a declaration and nothing more, and a record whose
exclusive arena is held by another live tree belongs to that tree — the test is
the one `destroy` itself applies, so `forget` never refuses over a record that
`destroy` would decline to clear. (This corrects the original wording, which
refused on any resource record. Every tree of a repo declaring a resource
carries such records from its first door, which made `forget` unreachable there
and its remedy circular.) Consent is identical to `unregister`, because
`.wt/` may contain application data: a terminal prompts, non-interactive use
requires `--yes`, and a declined prompt exits successfully with
`forgotten:false` and no mutation. Hash-owned rendered files and `.wt/` are
cleaned, exclude entries and state are removed, and the registry entry becomes
a tombstone with reason `forgotten`. Rationale: adoption can attach the wrong
name or policy to a perfectly valid worktree; correcting that record must not
pretend the checkout itself is disposable.

## A73. Adoption and the command surface describe actual intent

`adopt` accepts the same repeatable metadata assignments as `new`, plus a
declared `--agent`; it records both without starting anything. A recorded
agent makes the first `open` use the resume recipe, preserving the fact that
an adopted tree already has history. `rm` and `ls` are the primary documented
spellings while `remove` and `list` remain visible aliases. Help is ordered by
intent — Everyday, Setup, Working inside a tree, Upkeep — and exposes the
`test`, `lint`, `fmt`, and `build` run aliases. Human `ls --meta <key>` adds the
selected value as a fleet column; JSON continues carrying the complete map.
The JSON envelope keeps the canonical long command names `list` and `remove`
regardless of which accepted spelling was typed; help spelling is a human
interface, while the envelope is the stable machine interface.
Finally, when a repo-scope configuration key is misplaced at settings top
level, both lines of the error are wt's own: the message names the misplaced
entry — `locks.integration`, not merely `locks` — as a repo-scope key rather
than a settings key, and the remedy places it below `[repos.<label>]`. serde's
own text is kept only for a mistake that really is against the settings schema,
because its "expected one of" list names alternatives that are all wrong for a
key belonging to another scope. Rationale:
the terse verbs are the daily interface, adoption should preserve the same
fleet facts as creation, and configuration errors should point across the
scope boundary instead of merely saying that a familiar key is unknown.

## A74. The registry stores what was allocated, not what is derived

`name_short` and `session_name` are functions of the address, so no record
holds either one. `TreeRec` and `Tombstone` carry `slot`, frozen `geometry`,
and the `ports` map; both identities are computed where they are used, from
`label` and `name`. Allocation still refuses `IDENTITY_COLLISION` when a
candidate address derives an identity another address already holds, and the
registry invariant that both are unique across distinct addresses is checked
over the derived values. Tombstone inheritance therefore covers the allocated
coordinates only: a re-created or re-adopted address keeps the ports its
directory already refers to, and takes today's name. `adopt` deletes the
inherited tombstone in the same registry transaction, as `new` already did.
Older registries load unchanged and shed the two fields on their next write.

This supersedes A67's "existing persisted names and tombstone inheritance are
untouched" and the coordinate list in §7 and §3.1. Rationale: A67 made the
session name a pure function of the target with no hash fallback, and a value
that is derived and also stored has two sources of truth that only agree until
the derivation changes — which A67 changed. Storing it froze every tree created
before A67 into its old name with no way to take the new one. `name_short` is
unaffected in value, since its formula did not change; a pre-A67 tree's tmux
session does change name, and a session running under the old one is orphaned
by the upgrade, so close sessions before upgrading.

## A75. wt records where its own wall time went

wt appends one JSON object per event to `$WT_HOME/logs/wt.jsonl`, on by
default and disabled by `[logs] trace = false`. Each record carries a schema
version, a UTC timestamp, the invocation's random id and sequence number, the
pid, the command, a kind, a name, and a duration in milliseconds. Kinds are
`child` for one subprocess, `lock` for an acquisition that actually blocked,
`span` for a stretch of internal work, and `cmd` for the invocation itself,
which closes with an exit code or, for the commands that exec, with the program
it handed over to. Each record is a single append of at most 4096 bytes — the
size POSIX makes atomic — so the parallel doors write one file without a lock
and without interleaving a line; an over-long record is truncated. The file
rotates to `wt.jsonl.1` past 8 MiB, checked once per invocation. Writing a
record never fails a command.

Argument text appears only for invocations wt composed itself, named by their
leading subcommand words, so `git` and `tmux` are legible while a task recipe
is identified by task and scope and never by content, which can hold a
credential. This is separate from `<tree>/.wt/logs/` (A31), which keeps task
output. Rationale: slowness is noticed after the fact, so the measurement has
to already be there; it belongs in one file per home because the question is
almost always which of many child processes cost the time, and answering it
from the command's own output would mean printing a report nobody asked for.
Measurement only — behaviour is explained by notices and error codes, and a
second channel for that would rot.

## A76. Onboarding is one interactive verb that composes the others

`wt setup` is the first-run and add-more-repos verb: it finds git checkouts
already on the machine, registers the ones the user picks, adopts the linked
worktrees of those it registered, installs the shell integration, and settles
tmux and the default agent. It is the only verb whose primary mode is a
terminal.

Its mutation set is closed and named: `register`, `adopt`, a write to
`$WT_HOME/config.toml`, an append to a shell rc file, a write or append to a
tmux config, and a package-manager install run on the terminal. The first two
are verbs; the rest have no verb, and `setup` prints each as the shell line
that reproduces it — except the settings write, which edits a structured
document in place and is printed as a comment saying what it sets, because
no shell line reproduces it honestly — so `--dry-run` shows the run as the
commands that would produce it. `--dry-run` asks nothing — it takes the default answer to every
card, ticking every repository found — and therefore needs no terminal, which is what makes the whole pipeline
testable and what an agent runs to see what `setup` would do. Interactively,
`setup` gathers every answer first, renders one plan, takes one consent, and
only then applies: quitting before that consent exits 0 having mutated
nothing, and is not a failure.

Interactivity follows §14.2 rather than excepting itself from it. Without a
terminal on stdin `setup` refuses with `CONFIRM_REQUIRED` (2) and a remedy
naming `wt register` and `--dry-run`, because a wizard that cannot ask has
nothing to offer an agent that those do not offer better. `--json` is refused for the same
reason `exec` refuses it (A20): the envelope describes one operation's result,
and a session of questions is not one. `setup` is idempotent — a second run is
how a user adds repositories later, so already-registered checkouts appear in
its list marked as registered rather than filtered out of it, and every setting
it asks about offers its current value as the default.

The discovery walk is bounded by depth rather than by a list of directories to
look in. Walking `$HOME` to a fixed depth, declining to descend into a
directory once it is known to be a checkout, costs a readdir per directory
above a checkout and finds repositories wherever the user actually keeps them;
a curated roots list finds nothing in the home of anyone who chose different
names. Candidates are grouped by common gitdir and then by normalised origin,
because that grouping is what makes the choice legible: linked worktrees of one
checkout can only be adopted under that checkout's label (§11.6), a second
checkout of one origin can only become a second label, and the user is choosing
between those two outcomes rather than between paths. The two are one card: a
worktree's row sits under its checkout's and is enabled exactly while that
checkout is selected or already registered, so the dependency is visible
rather than enforced by the order of questions. Nothing is ticked on the
user's behalf: a registration is opt in, because a wrong tick that slips past
a long list costs a registration nobody wanted, while a missed one costs a
keystroke. Recency does the work it can do honestly — it puts the most
recently touched checkouts first and folds the stale tail behind one line —
and decides nothing. Every proposed label and name is editable in place.

One effect precedes the consent, and is named here rather than left as a
contradiction. Reading what a tmux configuration *effectively* sets means
starting tmux with it, on a throwaway socket and under a deadline, and a
configuration may run arbitrary commands at startup — a plugin manager
ordinarily does. Those commands run, once, before the card that reports the
differences can be drawn. The alternative is reporting differences from a
textual reading of one file, which is wrong for every configuration that
sources a fragment, loads a plugin, or sets an option twice; and the effects in
question are the ones that already run at every tmux start on that machine.
Nothing wt itself writes happens before the consent.

`setup` may only ever propose what `doctor` could report, and `doctor` reports
one thing it would otherwise have no way to say. `SHELL_INIT_MISSING` (info)
fires when no rc file wt knows installs §14.6's guard while a registered label
claims commands or declares a `bin` directory: without it an ordinary rc file
that reorders `PATH` silently defeats §9.2's guarantee, and the failure is
invisible from inside the door it breaks. It is info rather than warn because
a guard sourced from a fragment file, or from a shell wt does not know, is a
legitimate configuration it cannot distinguish from a fault, and a warning a
user learns to ignore costs more than the finding earns. It reads three files
and spawns nothing: `doctor` is a hot path, and a finding about tmux's key
handling that needed a throwaway tmux server on every run — with the user's
plugins started each time — was cut for that reason; `setup` reports the same
differences at the moment they can be acted on. For the same reason
`PATH_NOT_SHADOWED` is reported only from inside a door and only for that
door's own tree — outside one the prefix is *expected* to be absent, another
label's prefix is expected to be absent even inside one, and firing per
registered label taught the reader to skip the whole section.

## A77. The branch a tree gets is the repository's convention, not its address

A worktree's name is an address: it is typed at every command and it shows up
in the tree path, the session name, and the prompt. A branch name is a shared
artifact of the repository: teams spell it `<ticket>_<short description>`,
prefix it with an owner, or namespace throwaways. Deriving one from the other,
as `B` defaults to `<name>` does today, forces the convention into the address
— every command then carries a ticket id — or gives up the convention.

A repository, or a user for one label, declares how the branch is spelled:

```toml
branch = ["{{meta.ticket}}_{{name()}}", "{{name()}}"]
```

`branch` is one template or an ordered list of them, declared at the root of
`.wt.toml` or of `$WT_HOME/config.toml [repos.<label>]`, and it decides the
branch of a creation that does not name one. The first candidate whose
metadata references are all satisfied decides. When none is — the list is
exhausted, or no `branch` is declared at all — the branch is `<name>`, the
rule that holds today. A bare template is the one-element list, so
`branch = "{{meta.ticket}}_{{name()}}"` reverts to `<name>` for a creation
carrying no ticket, and a list is how a repository spells out what the
unticketed case gets instead (`"wip/{{name()}}"`). Requiring a ticket is
deliberately not expressible: ad-hoc trees for a short investigation are
ordinary use, and a creation that refuses until the user invents a ticket
teaches them to pass `--branch` every time, which is the convention lost by a
longer route.

The conditional lives in the list rather than in the template language. What
an ad-hoc creation needs is not a substitute value but the disappearance of a
whole segment — `{{meta.ticket}}_{{name()}}` without a ticket must not yield
`_fix-scroll` — and expressing that inside one string takes an optional-group
delimiter, which A66 would then extend to every templated string. An ordered
list says the same thing in the configuration's own shape, and says it
visibly: both outcomes are on the page, so a mistyped metadata key surfaces as
the wrong candidate in the `branch` line `new` already prints, rather than as a
rule the reader has to know to look for.

A branch template is evaluated before the tree exists, so it may reference only
`meta.<key>`, `name()`, `name_snake()`, `name_short()` and `label()`. There is
no root, repository path, port or `vars` value yet, and `branch()` is the value
being computed; every other reference is `CONFIG_INVALID` at its source
location. For the same reason only layers 1 and 2 (§5.1) carry a `branch`: no
adapter contributes one, and the tree overlay does not exist when a creation
chooses a branch.

`meta.<key>` reads that creation's `--meta` values. A reference is satisfied
only by a key present with a non-empty value, and it is legal only in a branch
template: the same reference in `env` or `files` would make a door refuse for
every tree that lacks the key, and metadata is editable afterwards, so an edit
documented as opaque bookkeeping would silently re-render a tree's files. The
branch is decided once, from the values that creation carried, and recorded.
An address that already exists is compared against its recorded branch rather
than a freshly rendered one, so a `wt meta` edit or an edited convention cannot
turn a bare re-run into a different source; `wt new` stays idempotent for a
tree whose ticket lives in the record rather than on the command line.

`--branch` still wins outright, `--detach` still produces no branch, and a
creation from a pull request still gets `pr/N`: that branch mirrors someone
else's work and is not this repository's naming convention. The rendered value
must be a valid branch name. Metadata is free-form text, so a value carrying a
space or `..` is refused — naming the candidate, what it rendered to, and
`--branch` — rather than sanitised into a name the user did not choose. The
recorded `source.branch` remains what a re-run compares against, so editing a
`branch` template makes a re-run of an existing address a different source,
exactly as editing `--branch` does.

## A78. A pull request creation lands on the pull request's branch

`--from pr:N` exists so that someone can work on a pull request: review it,
fix it, push the fix. The mirror `pr/N` served the first of those and
sabotaged the last. `refs/pull/N/head` is a read-only ref that carries the
pull request's commits and nothing else — not the name of the branch they
came from — so the local branch wt made from it tracked nothing, and git's
default for a branch that tracks nothing is to push it under its own name.
An agent asked to fix a pull request from such a worktree pushed `pr/N` to
origin: a new branch, a stray one, and the pull request untouched.

The forge knows the branch. For a GitHub origin, wt asks `gh` for the pull
request's head before it decides anything, and a head that is a branch of
origin makes the creation the same as `--from origin/<head>`: the worktree is
that branch, tracking it, and a plain `git push` updates the pull request.
Asking first is the point — a creation that cannot learn the branch refuses
rather than quietly producing the mirror, because the mirror is exactly the
worktree that misled the agent. The refusal carries gh's own reason and a
way in: `gh auth login` when gh says it is not logged in, `gh auth status`
otherwise, and `--from origin/<branch>` for someone who knows the branch and
does not want gh involved.

A forge CLI is an acceptable dependency here. Checking out a pull request
already presumes a forge, and the forge's client is how that forge is
addressed; only GitHub is asked today, and other forges keep the mirror
until they get the same treatment. The mirror also remains for the cases
that genuinely cannot track: a pull request from a fork, whose branch is not
on origin, and `--no-fetch`, which keeps the creation offline. Neither is
silent any more. The creation warns that `pr/N` tracks nothing and where a
push would go, and names the fork owner and branch, or the flag to drop, so
the worktree's limit is on the page when it is made rather than discovered
at push time.

An address that already records the same pull request keeps its recorded
branch and asks nothing unless its worktree has to be re-added from a start
the record cannot supply: the branch was decided when the tree was made, and
a re-run must stay idempotent — and must not fail on a missing or logged-out
`gh` — as A77 settled for every other source. Two addresses for one pull
request are `BRANCH_IN_USE`, as two addresses for one branch always were.

What the creation says the worktree tracks needs two witnesses: the forge
must have named this very branch as the pull request's, and git, read back
once the worktree exists, must confirm the upstream. A branch that already
existed locally under the head's name is checked out as it is and may track
nothing or sit behind origin; `--branch` may name a branch that tracks its
own origin twin and has nothing to do with the pull request; `--detach`
tracks nothing by construction. Each gets the warning with the push that
would reach the pull request, and the affirmative notice appears only when
both witnesses agree — a push that lands somewhere else, however well it
tracks, is the failure this addendum exists to end.

## A79. A tree's build lives in its own `target/` and is seeded from the canonical by clone

The per-tree build directory under `$WT_HOME/cache` (A64) was measured in
production against the trees it served: one active amux tree held 39 GB
after a day of agent work, and removing it took 78 seconds. The size was not
the directory's fault — cargo keeps one artifact set per build configuration
and never deletes a superseded one — but the directory had lost its reason to
exist. It was introduced so trees could share intermediates (A53); that
sharing was withdrawn when it corrupted builds (A64); what remained was a
private directory outside the tree that was cold on creation, could be
readopted warm only by recreating the same address, and needed its own orphan
detection, prune action, sizing, and removal step.

The tree's whole build now lives in its own `target/`. The cargo tools set no
directory variable at all, so a plain `cargo build` in a tree does what any
Rust developer expects, `cargo clean` works, and the build dies with the tree
in `git worktree remove`. Two trees still never share a build directory, for
the reason A64 established: Cargo's unit hashes omit the workspace path, so
two checkouts sharing one directory silently hand each other their compiled
crates — measured again on this round, an untouched tree reported a finished
build whose library carried the other tree's symbol.

Warmth comes from a seed, in the narrow form A45 reserved for build output:
clone-only, never a copy. Adapter tools declare `seed` directories (§6.1);
cargo names `target/debug/{.fingerprint,build,deps,incremental}`. At `new`
S3, after `copy`, wt clones each from the canonical checkout into the tree
copy-on-write (§11.8). The clone shares its blocks with the canonical until
either side writes, so it costs neither time nor space in proportion to its
contents; the tree then starts with the canonical's compiled dependencies and
rebuilds only its own crates, whose sources cargo sees as newer. Measured on
wt itself: clone, then `cargo build` compiled three workspace crates and no
dependencies. The uplifted binaries directly under `target/debug` are not
seeded, so a tree only ever runs what it linked itself (A41). A filesystem
that cannot clone — another volume, or no whole-directory primitive, which
today means anything but APFS — leaves the tree cold with a notice; a byte
copy is never attempted, because a copy of build output is exactly the
multi-gigabyte creation A53 retired. Whether cloning `incremental/` speeds
the rebuild of the workspace crates or merely occupies space is a
measurement not yet made; it stays in the list until it is.

`seed` is the adapter's to declare and not a configuration key, so the
unknown-key rule still refuses `seed` in a repository or user layer: the
mechanism is per-ecosystem knowledge, which is where A46 put it. The cache
root, `CACHE_ORPHAN`, the prune `delete-cache` action, `CACHE_DELETE_FAILED`,
`cache_deleted` and `cache_kb` are gone; `list --disk` reports `build_kb`,
the size of the top-level directories holding the seeded paths. A
`$WT_HOME/cache/cargo-build` left by an earlier version is not reaped — there
is no longer anything that knows the layout — and is safe to delete by hand.
The sccache nudge stands: it shares compile time across machines, which a
clone cannot.

Two consequences are left for the anchor work that follows. The canonical
is the seed, so it has to be built for the seed to be worth anything — today
that is the user's job, and the canonicals on the measured machine had no
build at all. And nothing yet reclaims superseded artifacts inside a live
tree, so the plateau cargo reaches is still the plateau; the repository-side
configuration changes made on this round (one feature set for every host
command, no abort profile in dev) shrink that plateau from 39 GB to under
7 GB for the same workspace, which is what makes the seed cheap to carry.

## A80. The canonical is kept at the default branch's tip and built, and build output is swept by what the workspace resolves to

A79 made the canonical checkout the seed every new tree clones from, and
left two things open: the canonical had to be built for the seed to be
worth anything, which was the user's job, and nothing reclaimed superseded
artifacts, so a build directory still grew to cargo's plateau. Both were
measured on the same machine on the same day. wt's own canonical had
accumulated 401 thousand files of superseded units; every tree seeded from
it inherited them, and removing such a tree took 42 seconds against 0.9
seconds from a clean canonical of 5 thousand files. Seeding from a
canonical nobody builds is seeding cold.

The anchor refresh is one verb, `wt anchor <label>` (§11.10): fetch the
default branch, move the canonical to its tip, build it through the
ordinary `build` task, sweep. It records nothing new beyond `head` in the
build state, so `status` and `doctor` classify the canonical's build with
the codes they already have, and a canonical that seeds trees but has no
build of its commit is a `doctor` finding with the verb as its remedy. The
verb starts by itself where the canonical is most likely to have fallen
behind: after `new` has started the tree's own build, after `rm`, and after
`sync` of a linked tree — detached through the supervisor A69 introduced,
with its scheduling priority lowered so the foreground tree's build wins
the machine, and only for labels whose adapters seed, since a label with
nothing to seed has nothing to keep warm. `new` does not wait for it. The
seed carries dependency units only, which change with the lockfile and the
toolchain rather than with every commit, and §11.8 already accepts a
partial snapshot. A per-label lock serialises refreshes and a second one
returns busy rather than waiting. In the register model the canonical is
the user's checkout, so it moves only by fast-forward, and only when the
default branch is checked out in it with no modified tracked file and
origin strictly ahead; every other state is left alone and named.

The sweep (§11.9) had to be told what is live, and cargo will not say
directly: it keeps one artifact set per configuration, deletes none, and
writes no mark on a unit it merely reuses, so a rule by age deletes
artifacts the next build would have read. What cargo will say is what the
workspace resolves to — `cargo metadata --locked --offline` lists every
package's target source paths — and each unit records the crate root it was
compiled from in its own dep-info. A unit whose crate root the resolve no
longer contains is dead: an old dependency version, a deleted target, and
the canonical's own workspace units carried into a tree by the seed, which
that tree can never use because its unit hashes embed a different path.
Among units that are the same thing built twice — same target, features,
profile, flags and output kind — only the newest survives, which is how a
bumped dependency or toolchain retires its predecessor once the successor
exists. Build-script runs carry no dep-info and live while a live unit's
recorded dependencies name their fingerprint. `incremental/` is the one
place age is honest, because rustc rewrites a session directory on every
use, so one untouched for two weeks goes. A pass that matches nothing to
the workspace refuses rather than deleting everything, since it is then
looking at some other checkout's output. The pass holds cargo's own
build-directory lock, so it excludes a build the way two builds exclude
each other, and stands down when the lock is held. It runs after every
build wt launches and inside plain `wt prune`; there is no `--cache` flag,
because superseded build output is one more kind of garbage `prune`
already reclaims.

`incremental/` leaves the seed list. rustc names its session directories
by crate metadata, which embeds the workspace path, so a tree never reuses
the canonical's; cloning it was pure carry, and the sweep would have had
to delete it at once. Feature-set variants and check-mode twins are kept
by the sweep on purpose: each command reuses its own, and deleting them
after every build would make `wt lint` recompile every dependency's
metadata each time. Retiring the feature variants is `cargo hakari`'s job,
which remains open on the repository side.
