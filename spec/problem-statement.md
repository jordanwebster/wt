## Part I — Problem statement

### 1. Context

Developers and autonomous coding agents need several independent copies of
the same project on one machine at once: parallel tasks, PR reviews,
experiments, long-running refactors beside day-to-day work. `git worktree`
gives each copy its own checked-out source tree cheaply — but not without
sharp edges of its own:

- A branch can be checked out in only one worktree; the second attempt
  fails confusingly.
- Submodules are not initialized by `worktree add` and carry per-tree state.
- Hooks, `info/exclude`, refs, and the object store are *shared* through
  the common gitdir: concurrent fetch/gc/pack-refs from two trees contend
  on the same locks.
- Deleting or moving the canonical checkout orphans every worktree.
- Tools assuming `.git` is a directory misbehave.

These failure modes are part of the problem, not background noise.

**Audiences, stated honestly.** The design will be *agent-first*: where the
two audiences' needs conflict, the agent wins, because agents are the
audience that cannot fall back on improvisation. Humans remain fully
supported, and bring needs agents do not have — changing their shell's cwd
into a tree, tab completion, confirmations before destructive actions,
readable tables, discoverability. Agents bring needs humans rarely feel:
non-interactive operation by default, machine-readable output with stable
schema, idempotent re-runs, no pager/color/prompts when not attached to a
TTY, bounded runtime, and safe concurrent operation without supervision.
Neither set is a superset of the other.

### 2. What source isolation does not solve

A worktree isolates files under version control — and, incidentally,
everything untracked inside its directory, which merely starts absent.
Everything *outside* the directory remains shared: the common gitdir, TCP
ports, the docker daemon, global caches, OS services, databases. Two
distinct problem shapes hide under "parallel copies":

- **Isolated-but-empty**: each new copy lacks dependencies, generated
  state, local config, caches — it must be bootstrapped, and kept current
  as manifests change upstream.
- **Genuinely shared**: processes from different copies collide on ports,
  names, locks, and services — which requires allocation and mutual
  exclusion.

Enumerating the pain areas:

1. **Lifecycle across repositories.** Creating a usable copy spans several
   steps (worktree add, branch conventions, dependency install, tool
   restore). Nothing records which copies exist *across many repos*; today
   referring to a copy from another directory requires knowing its full
   path.
2. **Bootstrap and currency.** Each stack's bootstrap commands are
   project-specific, so any tool that wants to make copies usable needs
   command knowledge per project — including for repos the user cannot
   commit configuration into. Long-lived copies also go stale when
   lockfiles change upstream.
3. **Runtime collisions.** Two copies' dev stacks collide on TCP ports,
   database names, docker container/volume names, unix sockets, pid files,
   dashboard ports, hardcoded localhost URLs — and, less obviously, the
   *toolchains themselves* collide: cargo's package-cache/target-dir locks,
   shared `.git` ref locks, package-manager cache locks, language-server
   indexers, file-watcher limits. Today the usual answer is "only run one
   stack at a time," enforced informally and violated by accident.
4. **Shared versus private resources.** Some expensive things should exist
   once for all copies (a Postgres server, an Elasticsearch container);
   some cheap things must be private per copy (a database name). Which is
   which is project knowledge living in people's heads. The sharpest case:
   two branches with divergent migrations pointed at one shared database is
   the canonical parallel-development disaster; private per-copy databases
   fix it but create a **data seeding** problem (where does the data come
   from?).
5. **Duplicate-copy cost.** Ecosystem caches (pnpm store, uv wheel cache,
   NuGet global cache) apply automatically once the right tool runs — but
   committed manifests often pin the slower tool (`package-lock.json` ⇒
   npm), some artifacts have no cross-copy cache at all (`node_modules`,
   cargo `target/`), and N copies × build directories is the resource that
   actually fills disks. Nothing reports or reclaims that cost today.
6. **Which copy am I?** Agents drift between directories across tool calls;
   paste host paths across copy boundaries; and two agents occasionally
   share one tree by mistake. Relatedly, agent harnesses key project state
   (memory, settings, history) on absolute path — a new worktree at a new
   path starts from zero knowledge.
7. **Sessions.** One common deployment runs agents inside terminal
   multiplexer sessions; resuming work means recreating sessions in the
   right directory with the right agent resumed, addressable by name from
   any shell. This is deployment-shaped, not universal — agents also run
   headless, in IDEs, in sandboxes — but where it applies it is daily pain.
8. **Dangerous operations from the wrong context.** Formatting code,
   migrating a shared database, or running heavy jobs concurrently has no
   guardrail today beyond human memory.
9. **Partial failure and truth-telling.** Bootstrap fails halfway; a
   database outlives its deleted tree; a port claim outlives its process;
   the registry disagrees with reality. Any management tool must be able
   to detect and describe inconsistent state, or it becomes another source
   of it.

### 3. Prior art, honestly surveyed

| Layer | Examples | Verdict |
|---|---|---|
| git worktree built-ins | `worktree add/list/prune/lock`, `--guess-remote` | integrate — wt is built on them |
| Shell wrappers / worktree managers | various aliases + post-create hook scripts | superseded — most stop at source isolation + `.env` copying |
| Agent harness built-ins | Claude Code EnterWorktree, parallel-agent worktrees | coexist — they manage *one* repo's trees for one harness session; no cross-repo registry, no coordinates, no resources |
| Task runners | make, just, npm scripts | integrate — wt routes to them, never replaces them |
| Per-directory env | direnv, mise | complement — mise/direnv may even be what task recipes invoke |
| Dev-environment frameworks | Nix/devenv, devcontainers, Tilt | respect as inputs where present (e.g. devcontainer features), otherwise ignore; adoption cost rules them out as wt's substrate |
| Stack orchestrators | docker compose, .NET Aspire | feed, never fight — both already parameterize copies (`compose -p`, Aspire dynamic ports); what nothing provides is *consistent choice of parameters across parallel copies and tools* |

Project-local scripts encode the right commands but bind to a checkout the
user controls; the real constraint for third-party repos is that the user
cannot *commit upstream*, so any repo-level configuration must have a
user-side home.

### 4. Scenarios (with pass/fail observables)

**S1 — Parallel agents, one repo.** Agent A implements a feature; agent B
fixes a bug; both run tests against databases; the app under test reads its
database name from configuration.
Pass: both agents create their copies and run the test suite concurrently
without collision or manual port/db assignment; neither observes the other's
data; `wt list` shows both copies with truthful state.

**S2 — Human reviews a PR while a feature stack serves.** Developer's main
checkout runs a dev server; they want to review PR #42 locally at the same
time.
Pass: one command creates a review copy from the PR ref; if the PR's stack
can read injected coordination values, both stacks serve simultaneously;
otherwise the second stack refuses with a clear message instead of silently
colliding.

**S3 — Resume after two weeks.** An agent's tree sat untouched while
upstream moved; the developer returns.
Pass: `wt list` shows the tree's branch, dirty state, and whether its
private resources still exist; sessions recreate by name; stale-state
detection flags lockfile drift against the base branch.

### 5. Requirements

Observable behavior the solution must provide (mechanism-free):

- **R1 Addressability.** Any worktree of any registered repository can be
  referred to unambiguously from any working directory without knowing its
  filesystem path. Creating, listing, and deleting copies works across all
  registered repositories through one interface.
- **R2 Remote sources.** A copy can be created from a remote branch or pull
  request, including from repositories not previously present on disk
  (reviewing inbound changes is the dominant motivating case).
- **R3 Usable copies.** After creation returns successfully, the project's
  own declared verify step (test or build) succeeds in the new copy without
  further manual steps. If bootstrap fails partway, the copy remains on
  disk with its failure recorded and retryable; heavyweight one-time setup
  is available explicitly and is not required for R3's bar.
- **R4 Zero-teaching command execution.** For recognized common stacks,
  the tool runs the project's own bootstrap/verify steps without being
  taught; for unrecognized or exceptional cases it can be taught per
  repository and per directory, including by users who cannot commit to
  the repository.
- **R5 No silent collisions.** Starting a second stack whose resources
  would collide either succeeds with distinct coordination values or fails
  loudly with a clear message; it never silently corrupts or contends.
  This covers application collisions and, within the tool's own
  operations, toolchain/gitdir contention.
- **R6 Resource lifecycle truth.** Private-per-copy resources exist exactly
  while needed mechanisms require them and never outlive their copy;
  shared resources exist once; the tool can always report what exists,
  what is pending, and what is orphaned — including after crashes or
  out-of-band deletion. Data for private resources comes from declared
  seeds/templates; the tool never invents one.
- **R7 Marginal-copy efficiency.** The Nth copy costs materially less
  wall-clock time and disk than the first; the tool uses the package
  manager the repo's lockfile selects, and reports (never silently applies)
  faster alternatives.
- **R8 Machine contract.** All commands: non-interactive by default;
  machine-readable output with a stable schema and stable ordering
  (`--json`); defined exit-code classes; idempotent re-runs; TTY-aware
  output (no color/pager/prompts unless attached); bounded runtime.
- **R9 Human contract.** Destructive actions confirm unless explicitly
  waived; state is presented readably; help and discovery exist without
  reading docs; a helper exists for entering a tree's directory from an
  interactive shell (a child process cannot change its parent's cwd).
- **R10 Consent boundary.** Registering a repository is the act that
  authorizes its tooling to run; `register`/`clone` show what the repo's
  `.wt.toml` declares at that moment. No separate per-content trust
  protocol: adapters already execute repo-controlled code (`npm ci`,
  `cargo test`) unconditionally, so hashing only `.wt.toml` would guard a
  side door while the front door stays open, and would tax the agents
  that edit `.wt.toml` in their own trees.
- **R11 Truthful state.** The tool detects and reports disagreement between
  its records and reality (missing directories, dead claims, unknown
  worktrees) rather than failing mysteriously downstream.
- **R12 Non-invasiveness.** Using the tool requires no changes to
  committed files except an optional config file; tracked files are never
  modified nor trees dirtied by the tool itself. Applications function
  unchanged without the tool, at reduced capability: full concurrency
  requires a wt-agnostic configurability change to the application (read
  coordination values from configuration with today's values as defaults).
- **R13 Graceful degradation.** Features degrade with clear messages, not
  errors, where facilities (multiplexers, credential helpers) are absent.
  POSIX-first; Windows is a non-goal for v1 (see §6).

### 6. Explicit non-goals (v1)

- Owning process/container orchestration: wt never starts or stops
  long-running processes. It may create logical objects (like a database)
  inside infrastructure someone else runs, via declared recipes.
- General-purpose task runner: task definitions exist only to serve
  copy lifecycle, verification, and resource recipes.
- Merge-back/integration workflow (opening PRs, merging branches): out of
  scope; wt ends where `git push` begins.
- Secrets management: propagation of existing secrets only; no vault, no
  generation.
- Reproducing Nix-grade reproducibility or hermetic builds.
- Multi-host/distributed environments (a remote devbox counts as "one
  machine").
- Windows support in v1 (POSIX-only; documented as such).
- Replacing agent-harness worktree features or GUI/TUI surfaces of its own.

---

