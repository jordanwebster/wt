# wt — specification
Normative specification for the clean re-implementation of `wt`. Read
with `problem-statement.md` (requirements R1–R13) and
`requirements-addendum.md` (binding decisions A1–A31);
`acceptance/*.wt.toml` are three representative configurations the
implementation must accept unchanged.

Provenance: designed from the problem statement, reviewed adversarially, then reduced to the smallest design meeting R1–R13 and A1–A30 (A31). Requirement disputes were settled as addenda.

Vocabulary (A15): a registered repository is a **label**; a tree is
`<label>/<name>`; the canonical checkout is the tree named `canonical`,
addressed as the bare label; `WT_TARGET` is the address; `WT_REPO` is
the canonical checkout path.

Owning sections (the only place each mechanism is defined):

| Mechanism | Owner |
|---|---|
| door cost budget | §0 |
| identity file and identity check | §4.1 |
| store write/read protocol | §4.3 |
| slot/geometry allocation; tombstone inheritance; ports map | §7 |
| activation metadata, `deactivate`, `assemble` | §8.1–8.2 |
| rendering and ownership | §8.3 |
| door algorithm and spawn (`execvp`, `run` parent, `--no-gate`) | §9.1–9.2 |
| session verbs and tmux command line | §9.4 |
| task execution and `lock_plan` | §10.1 |
| resource snapshot and `execute` | §10.3 |
| resource state machine | §10.4 |
| declaration refresh; repo-tied declarations | §10.5 |
| `new` / `sync` / `remove` / `unregister` / `register` / `adopt` / `copy`/`seed` | §11.2–11.7 |
| `list` drift, `prune`, doctor codes, log retention | §12 |
| lock families, order, deadlines | §13 |

## 0. Door cost budget (normative ceiling)
A door on a `ready` tree (`wt exec`, the common agent call) performs **at most**: one read of `.wt/tree_id`; one registry read and one state read; one state write only if a rendered file's bytes changed, and one registry RMW only if a port name was appended; one `git` query for `WT_BRANCH` (§5.5); one `git ls-files` only at the first render of a path (§8.3); one blake3 per rendered file; two `flock`s (tree shared, door file); **no** bind probes, **no** declaration refresh, **no** other subprocess. The prelude excluding git must complete in under 50 ms on a warm laptop. Any later addition to §9.1 must fit this ceiling or justify raising it in this section.

## 1. Principles
1. Few concepts, one owner each (§2); one lock per mutable file (§13); pure core, thin effects, one sequencing binary (A18, §15).
3. One door algorithm (A1); the shell door's promise is at spawn (A19).
4. Truth over records: probes decide; records carry provenance and age. The hot path is budgeted (§0); machinery that only serves a crash runs on recovery paths (A30, A31).
5. Agent-first (A10, A20): `--json` wherever stdout is wt's; idempotent lifecycle verbs; bounded control plane (A14); a remedy on every error.
6. Nothing happens outside a door (A2). Ecosystem knowledge is data (§6).
7. Everything needed to tear a tree down lives under `$WT_HOME`; teardown never runs a recipe that names a tree binary after the tree is gone (A18, A25).
8. Consent precedes mutation in destructive verbs (R9, §11.4).

## 2. Concepts
| Concept | Definition | Identity | State owner |
|---|---|---|---|
| Label | registered repository: canonical path + common gitdir | `Label`; `gitdir_id` = blake3(realpath of the common gitdir) | `registry.json` |
| Tree | canonical checkout (name `canonical`, `label` ≡ `label/canonical`) or linked worktree; an address has one live **incarnation** | `label/name`; incarnation `tree_id` (random 128-bit hex) | registry + `state/<label>/<name>.json` |
| Coordinates | slot, frozen geometry `{base, stride, port_base}`, `ports` map, `name_short`, `session_name` | per incarnation; inherited across reincarnation (§7) | registry |
| Environment | exact map handed to a child + activation metadata | per door | not persisted |
| Door / Task | `run`, `exec`, `shell`, `open`, `env` / recipe resolved at a scope | — / `ScopedTask`, `PrivateId` | — / logs in `<tree>/.wt/logs/` |
| Resource | a task with `destroy`; a refreshed declaration snapshot and a frozen instance snapshot; three states (§10.4) | `ResourceKey` (§10.2) | state files |
| Session | tmux session bound to a tree; intent (`agent`) recorded; liveness from tmux (A24) | `session_name` | registry; tmux |
| Lock | `flock(2)` files, six levels (§13) | path | kernel |

## 3. Names and addressing
### 3.1 Grammars and derived identities
```
Label      := /[A-Za-z0-9][A-Za-z0-9._-]{0,31}/ , not "." or ".."
TreeName   := /[A-Za-z0-9][A-Za-z0-9._-]{0,63}/ , not "." or ".." ; "canonical" reserved
Target     := Label | Label "/" TreeName
TaskId     := /[a-z0-9][a-z0-9._-]{0,63}/ ;  RelDir := "." | RelPath(dir)
ScopedTask := (RelDir "/")? TaskId ; PrivateId := "@" AdapterId "/" ToolId "@" RelDir "/" TaskId
PortName   := /[a-z][a-z0-9_]*/ → WT_PORT_<UPPER> ; EnvKey := /[A-Za-z_][A-Za-z0-9_]*/ not /^WT_/
LockName   := /[a-z0-9][a-z0-9._-]{0,63}/ ; Duration := /[0-9]+(ms|s|m|h)/ ; TreeId := /[0-9a-f]{32}/
ScopeEnc   := RelDir with "/" → "%2F", "." for root
```
- `name_snake(s)`: lower-case; runs of `[^a-z0-9]` → `_`; trim `_`; `x` if empty. `WT_NAME_SNAKE` is many-to-one (display/derivation only).
- `name_short` (`WT_NAME_SHORT`): `name_snake(label)_name_snake(name)` truncated to 22 + `_` + first 8 hex of blake3 of the untruncated string (≤ 31, `[a-z0-9_]`); `session_name`: `wt_` + san(label)[..16] + `_` + san(name)[..24] + `_` + first 8 hex of blake3(`label/name`), `san` mapping `[^A-Za-z0-9_-]` → `_`.
- Both are computed at allocation, checked against every other address in `trees ∪ tombstones` (collision → `IDENTITY_COLLISION` (4), remedy "choose another tree name"), persisted and inherited per §7; `assemble` reads them from the registry entry.

### 3.2 Address resolution
`Address := "." | AbsPath | Target | TreeName`. Bare `x`: (1) cwd inside a live tree of label `L` and `L/x` live → `L/x`; (2) `x` is a label → `x/canonical`; (3) `NOT_FOUND` (3) whose remedy lists `L/x` candidates. No cross-label inference. `wt new` refuses a name equal to a label (`NAME_SHADOWS_LABEL` 4). cwd resolution is longest-prefix over canonicalised live tree roots (unique because paths are unique, §4.1). Omitted `[target]` ⇒ `.`.

## 4. On-disk layout
```
$WT_HOME/                                 --home > WT_HOME env > ~/.wt; exported as WT_HOME in doors; 0700
  config.toml
  registry.json, registry.lock            level-5 RMW lock
  state/<label>/_repo.json                repo-tied resource records (0600)
  state/<label>/<name>.json               tree state (0600); exists exactly while the address has a live entry
  locks/<label>/<name>.lock               level-1 tree lock (shared: doors; exclusive: lifecycle verbs); path depends only on the address
  locks/<label>/<name>.doors/<pid>.lock   door-holder record {pid, verb, since}; try-flock = liveness
  locks/git/<gitdir_id>.lock              level-2 repo-git
  locks/<label>/res/tree/<name>/<ScopeEnc>/<task>.lock ; locks/<label>/res/repo/<ScopeEnc>/<task>.lock   level-3
  locks/<label>/named/<lockname>.lock     level-4
  locks/<label>/<name>.rmw.lock, _repo.rmw.lock   level-6
  trees/<label>/<name>/                   worktrees (settings.trees_dir overrides)
<tree>/.wt/                               tool-owned, excluded, never authoritative; holds tree_id, logs, rendered files
<commondir>/info/exclude                  managed block (§4.2)
```
Files under `state/` are created 0600 and directories 0700 by explicit modes, independent of umask.

### 4.1 `registry.json` (schema 1), invariants, identity check
```
Registry  := { schema: 1, labels: Map<Label, LabelRec>, trees: [TreeRec], tombstones: [Tombstone] }
LabelRec  := { path (canonicalised), gitdir_id, common_gitdir, registered_at, trees_dir|null, default_branch|null }
TreeRec   := { tree_id, label, name, canonical, path (canonicalised), slot, geometry: {base, stride, port_base},
               ports: Map<PortName, u8>, name_short, session_name, created_at, agent|null,
               source: { kind, branch|null, pr|null, start|null } }
Tombstone := { label, name, slot, geometry, ports, name_short, session_name, path, materialized: [RelPath], removed_at, reason }
```
Invariants (load-time; violation → `REGISTRY_CORRUPT` 5): slots unique and port ranges pairwise disjoint across `trees ∪ tombstones`; `(label,name)` unique across `trees ∪ tombstones` — an address is either live or tombstoned, never both; `tree_id` unique; **path uniqueness** holds over (label paths) ∪ (non-canonical tree paths) — the canonical tree shares its label's path; `gitdir_id` unique across labels; one canonical tree per label; `name_short` and `session_name` unique across distinct addresses. `register` of a path already registered under the **same** label with identical arguments is idempotent (`registered: false`, exit 0, §11.6); `PATH_REGISTERED` (4) fires only when the path is registered under a different label, or when `--label` names an existing label bound to another path; `register`/`adopt` of a path whose common gitdir equals a registered label's → `GITDIR_REGISTERED` (4), remedy "use `wt adopt <path> --label L`"; `adopt` of a worktree of label `L` forces `--label L`.

**Identity file and identity check.** Every tree carries `<tree>/.wt/tree_id` (the incarnation's `tree_id` + newline), written at S1/I1 of `new`/`register`/`adopt` (§11). The **identity check** `ID(path, entry)` is: read `<path>/.wt/tree_id` (`O_NOFOLLOW`) and compare with `entry.tree_id`; a missing or different value is `TREE_REPLACED` (5): wt neither renders into, runs in, nor removes that directory; phase `replaced` (§11.1); remedy in order: `wt prune --records <target>`, `wt remove <target>` (records-only path, §11.4), `wt adopt <path>`. Callers: the door algorithm (§9.1 D0), `remove`/`unregister` (§11.4 step 6, immediately before the first mutation), `sync`, `status`/`list`. A non-wt actor replacing the directory between the check and git's own open is an accepted residual (A27).

### 4.2 `info/exclude` managed block
Markers `# >>> wt managed >>>` … `# <<< wt managed <<<`. Content = sorted, deduplicated union of `/.wt/`, `/**/.wt-tmp-*` (crash-left render temporaries), and every materialised path (prefixed `/`) of all live trees (from their state files) and tombstones (`materialized`) of the label. Recomputed **under the registry RMW lock** (level 5; the block is a function of registry-named files) whenever a materialised set changes, at register/adopt/new, at tombstoning and tombstone collection, and at unregister. Text outside the markers is preserved byte-for-byte; an unclosed block is closed at EOF (`EXCLUDE_REPAIRED`).

### 4.3 Store protocol
Write `P`: `tmp` in the same directory (`O_CREAT|O_EXCL`, 0600), write, `fsync`, `rename(tmp, P)`, `fsync(dir)`. Readers take no lock and never observe a partial `P`. Unparsable `P` → `*_CORRUPT` (5), remedy "delete `P` and re-run `wt register`/`wt adopt` for the affected checkouts"; no silent recovery. Files carry `schema`. RMW = RMW lock (§13, held across read → mutate → write) → release; RMW locks are leaf locks. **State-file rule.** `state/<label>/<name>.json` is written at step R of `new`/`register`/`adopt` (before the registry write that names the entry; a fresh incarnation overwrites it) and deleted when the entry is tombstoned (§11.4 step 11); a state file without a live entry is `STATE_ORPHAN` (info) and deleted by `prune`.

### 4.4 Old-format detection (A16)
First operation of every command, before any read of its own files and before any write: `registry.toml` or `state.toml` present without `registry.json` → `HOME_OLD_FORMAT` (5); both present → `HOME_MIXED` (5); remedy "move or delete `$WT_HOME`, then re-register".

## 5. Configuration
### 5.1 Layers
| # | Layer | Location |
|---|---|---|
| 0 | adapter tables (§6) | built in |
| 1 | repo `.wt.toml` of the tree being operated on (root + `[dirs."sub"]`) | tree |
| 2 | user `$WT_HOME/config.toml` `[repos.<label>]` (root + `dirs`) | home |
| 3 | tree overlay `<tree>/.wt/config.toml` | tree |

Teardown reads no layer (§10.3).

### 5.2 `.wt.toml` grammar (A15; ★ new keys)
```
Config  := Scope & { ports?: [PortName], dirs?: Map<RelDir, Scope>, seed?: [RelPath] ★, sync_inputs?: [RelPath] ★, detect?: Detect ★ }
Scope   := { bin?: [RelPath], env?: Map<EnvKey, Template|false>, copy?: [RelPath], files?: Map<RelPath, File|false>,
             task?: Map<TaskId, Task|false>, adapters?: Map<AdapterId, { tool?: ToolId, disabled?: bool }> }
Detect  := { depth?: 0|1|2 (1), ignore?: [RelPath] }
File    := { content?: Template, source?: RelPath, marker?: String ("#"; "" = no header), mode?: OctalString ★ ("0644") }
Task    := { run?: Cmd, exists?: Cmd, destroy?: Cmd, needs?: [ScopedTask|PrivateId], lock?: LockName, name?: Template,
             tied_to?: "tree"|"repo", env?: Map<EnvKey, Template>, cwd?: RelPath, timeout?: Duration, description?: String,
             ready_within?: Duration ★, snapshot_env?: [EnvKey] ★ }
Cmd     := String | [String]          // `sh -c` (never pre-expanded) | argv (each element expanded)
Template:= String                      // $NAME, ${NAME}, $$; `$` before a non-name char is literal
```
`false` deletes an inherited entry. The three acceptance files parse unchanged (§16).

### 5.3 Directory scopes (A7)
Scopes are declared by `[dirs."d"]` (layers 1–3) or detected (§6). The scope chain for cwd `c` is `c`'s relative dir and its parents up to root, nearest first, keeping declared/detected scopes; for `d/t` it starts at `d`.

| Key | Rule |
|---|---|
| `task` | nearest scope wins by `TaskId`; within a scope tree > user > repo > adapter; default `cwd` = scope dir; explicit `cwd` is root-relative |
| `env`, `files`, `copy` | accumulate root-first; nearer scope overrides by key/path; same layer precedence within a scope |
| `bin` | concatenated root-first then nearer, deduplicated, nearer first on PATH |
| `adapters` | per scope, merged by id across layers |
| `ports`, `seed`, `sync_inputs`, `detect` | root only |

A resource's scope is the scope at which its effective task was declared.

### 5.4 Settings and geometry
```
Settings := { schema?: 1, trees_dir?, agents?: Map<String, { start: Cmd, resume: Cmd }>,
              ports?: { base?: u16 (20000), stride?: u8 (16) }, git?: { timeouts?: GitTimeouts }, task?: TaskDefaults,
              locks?: LockWaits, session?: { backend?: "tmux"|"none", attach?: bool (true), agent?: String|null,
                                            tmux_timeout?: Duration ("10s") },
              logs?: { keep?: u16 (20) } ★, shell?: { program?: AbsPath }, repos?: Map<Label, Config> }
```
Validation at load (`u32` arithmetic) else `SETTINGS_INVALID` (5): `1024 ≤ base ≤ 65535`; `1 ≤ stride ≤ 255`; `max_slots = (65536 − base)/stride ≥ 1`; `session.agent`, when set, names a declared agent. The removed top-level agent setting is rejected with a remedy naming `session.agent`. At `register`, or on the first `new`, `open`, or `close` in a home that predates this setting, an absent `session.backend` is resolved once: wt checks for tmux ≥ 3.2, writes `"tmux"` when found and `"none"` otherwise, and prints `sessions: tmux <version> (set session.backend to change)` or its `none` equivalent on stderr. A non-table `session` declaration that cannot be extended without rewriting is refused with a remedy to rewrite it as `[session]`. No command detects a session backend again after the key is written. `doctor` reports the effective backend as `SESSION_BACKEND` (info). Geometry is per incarnation and immutable (§7); `assemble` uses `TreeRec.geometry`; settings changes affect only future allocations; doctor `GEOMETRY_CHANGED` (info). Per tree `ports.len() ≤ geometry.stride` else `CONFIG_INVALID`. Built-in agents: `claude` (`claude` / `claude --continue`), `codex` (`codex` / `codex resume --last`).

### 5.5 Tool variables (A15; ★ new)
`WT_LABEL`, `WT_NAME`, `WT_NAME_SNAKE`, `WT_NAME_SHORT`, `WT_TARGET`, `WT_BRANCH`, `WT_ROOT`, `WT_REPO` (canonical path), `WT_HOME` (resolved home), `WT_SLOT` ★, `WT_PORT_BASE`, `WT_PORT_<NAME>`, `WT_SESSION`, `WT_BIN` ★, `WT_ACTIVATION` ★ (the marker, §8.1), `WT_TASK`, `WT_SELF`. `WT_BRANCH` is the HEAD branch **at spawn** (empty if detached), read by one bounded `git symbolic-ref --short -q HEAD`; it is not updated while a shell or session lives (A31). All except `WT_ACTIVATION` are ordinary variables with a recorded prior (§8.1). **Tree-specific keys** (used by §10.3): `WT_ROOT`, `WT_TARGET`, `WT_NAME`, `WT_NAME_SNAKE`, `WT_NAME_SHORT`, `WT_SLOT`, `WT_PORT_BASE`, `WT_PORT_*`, `WT_SESSION`, `WT_BIN`, `PATH`.

### 5.6 Validation: static vs late-bound
| Check | When | Error |
|---|---|---|
| grammar, unknown keys, identifiers, lexical paths (§5.7), durations, modes, template syntax; `$WT_*` names ∈ tool set ∪ declared ports ∪ `WT_SELF`/`WT_TASK` where allowed ; `destroy ⇒ exists ∧ tied_to`; `ready_within ⇒ exists`; `run ∨ destroy`; one of `content`/`source`; port names unique; `ports.len() ≤ stride`; a `copy`/`seed` entry that is also a `files` key | parse/validate | `CONFIG_INVALID` (5) with `path:line:col` |
| alias→alias: `$NAME` in an `env` template where `NAME` is a key of the tree's effective env map → `ALIAS_REFERENCES_ALIAS`; `$NAME` in a task `env` map naming another key of the same map → `TASK_ENV_SELF_REFERENCE` | resolve | `CONFIG_INVALID` |
| `needs` resolvable/acyclic; `tied_to = repo` templates reference no tree-specific key (§5.5) | resolve | `CONFIG_INVALID` |
| `$NAME` (non-`WT_`, not an alias) resolved from the frozen context (§8.2) | door | `ENV_UNDEFINED` (5): key, template, door |
| `bin`/`cwd` existence, `source` readability | door | `BIN_DIR_MISSING` (notice), `CWD_MISSING` (5), `FILE_SOURCE_MISSING` (5) |

### 5.7 Path containment and no-follow I/O
Lexical: non-empty, no leading `/`, no `..`, no `.` component (except the whole `"."`), no NUL; normalised. The tree root is canonicalised once at registration (`ROOT_IS_SYMLINK` 5 if a symlink). Writes into a tree: parent directories created with `mkdir -p` semantics; the target inspected with `fstatat(AT_SYMLINK_NOFOLLOW)` and opened `O_NOFOLLOW` (a symlink target → `RENDER_ONTO_SYMLINK`/`COPY_EXISTS` per caller). Render: `tmp` beside the target (`O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW`) → write → `fsync` → `rename`. `copy` and repository-declared `seed`: source walked with `fstatat(AT_SYMLINK_NOFOLLOW)`, symlinks recreated, directories created, files copied with reflink attempted per file (`fclonefileat`/`FICLONE`) and read/write fallback on `EXDEV`/`ENOTSUP`. Adapter seeds use the same contained walk but require reflinks for files; if any file cannot be cloned, the partial destination is removed and the whole adapter seed is skipped (§11.7). `bin`: lexical join (PATH semantics). `cwd`: lexical containment; `chdir` follows symlinks (documented). `source`: the same rules from the canonical root.

## 6. Adapters
### 6.1 Tables
```
Adapter := { name, detect: [Glob], default_tool, nudge?: [ { if_tool?, want, hint, used_if_env? } ], tools: Map<ToolId, Tool> }
Tool    := { lockfile?: [FileName], sniff?: [ { file, toml_key?, contains? } ], requires?: Binary, sync_inputs?: [FileName], seed?: [RelPath], env?, task? }
```
Selection per scanned dir: user/repo `adapters.<id>.tool` > lockfile > sniff > `default_tool`; first adapter in fixed order (`cargo, node, dotnet, python, go`) wins per dir; `submodules` is root-only and independent. Detection is pure over a `DirSnapshot` (names at depth ≤ `detect.depth`; contents of `package.json` and files named by `sniff`). Default ignore: `.git .wt node_modules target bin obj dist build .venv vendor .next .expo` and dotdirs.

| Adapter/tool | Selected by | sync | build | test | lint | fmt | sync inputs | seed |
|---|---|---|---|---|---|---|---|---|
| cargo/cargo | `Cargo.toml` | `cargo fetch` | `cargo build --all-targets` | `cargo test` | `cargo clippy --all-targets -- -D warnings` | `cargo fmt` | `Cargo.lock`, `Cargo.toml` | `target` (reflink only) |
| cargo/cargo-nightly-fmt | `rustfmt.toml`/`.rustfmt.toml` parsed as TOML with top-level `unstable_features`/`group_imports`/`imports_granularity` | same | same | same | same | `cargo +nightly fmt` | same | same |
| node/npm | `package-lock.json`/`npm-shrinkwrap.json`; default with `NO_LOCKFILE` (then `npm install`) | `npm ci` | `npm run build`† | `npm test` | `npm run lint`† | `npm run format`† | lockfile, `package.json` | `node_modules` |
| node/pnpm | `pnpm-lock.yaml` | `pnpm install --frozen-lockfile` | † | † | † | † | idem | `node_modules` |
| node/yarn | `yarn.lock` | `yarn install --immutable` (`.yarnrc.yml`) / `--frozen-lockfile` | † | † | † | † | idem | `node_modules` |
| node/bun | `bun.lock(b)` | `bun install --frozen-lockfile` | † | † | † | † | idem | `node_modules` |
| dotnet/dotnet | `*.sln`, `*.slnx`, `*.csproj`, `*.fsproj` | `dotnet restore` | `dotnet build --no-restore` | `dotnet test` | `dotnet format --verify-no-changes` | `dotnet format` | `*.csproj`, `packages.lock.json`, `Directory.Packages.props` | — |
| python/uv | `uv.lock` | `uv sync --frozen` | `uv build` | `uv run pytest` | `uv run ruff check .` | `uv run ruff format .` | `uv.lock`, `pyproject.toml` | `.venv` (reflink only) |
| python/poetry | `poetry.lock` | `poetry install` | `poetry build` | `poetry run pytest` | `poetry run ruff check .` | `poetry run ruff format .` | `poetry.lock`, `pyproject.toml` | — |
| python/pip | `requirements.txt`/`setup.py`/`pyproject.toml` without lockfile | venv + `pip install -r requirements.txt` (or `-e .`) | — | `.venv/bin/pytest` | `.venv/bin/ruff check .` | `.venv/bin/ruff format .` | `requirements*.txt`, `pyproject.toml` | `.venv` (reflink only) |
| go/go | `go.mod` | `go mod download` | `go build ./...` | `go test ./...` | `go vet ./...` | `gofmt -l -w .` | `go.sum`, `go.mod` | — |
| submodules | `.gitmodules` | `git submodule update --init --recursive` (`sys_locks: [RepoGit]`, git class `submodule`) | — | — | — | — | `.gitmodules` | — |

† only when `package.json` declares the script. Nudges: `node: if npm → pnpm`; `python: if pip|poetry → uv`; `cargo: sccache, used_if_env = ["RUSTC_WRAPPER=sccache"]`; doctor evaluates `used_if_env` against the effective door env → `ACCELERATOR_INACTIVE` (warn) / `ACCELERATOR_AVAILABLE` / `ACCELERATOR_MISSING` (info); never applied (R7). R7 mechanisms applied: worktree object sharing; `seed` reflink with per-file fallback (§5.7; adapter default seeds are skipped when the first file falls back, `SEED_SKIPPED_NO_REFLINK` info); `wt list --disk`.

### 6.2 Composition and private ids
Every adapter hit at scope `d` contributes private nodes `@<adapter>/<tool>@<d>/<k>` (`cwd = d`, origin `adapter`), never overridden by layers, addressable by `needs` and `wt tasks --private`. Public: at a non-root scope `d`, `d/k` is the layer task if declared there, else an alias of the private node; at root, `k` is the layer task if declared at root, else the composite `{ needs: [@submodules/git@./k?, @<root adapter>@./k?, d1/k, d2/k …] }` over sorted scopes with an effective `d/k`; an empty composite does not exist. `verify` = `test` else `build` else absent (`NO_VERIFY`). orbitcloud before the repo layer: `sync` = composite over `@dotnet/dotnet@./sync`, `frontend/sync`, `website/sync`; after `[task.sync]`, `sync` is the repo task and all others remain addressable.

## 7. Coordinates: allocation, inheritance, ports
Allocation happens inside the reserving registry transaction of `new`/`register`/`adopt` (§11):

| Case | Rule |
|---|---|
| a tombstone exists for this address | **inherit** the tombstone's slot, geometry, `ports`, `name_short`, `session_name`; delete the tombstone in the same registry transaction; the new incarnation gets a fresh `tree_id`. A tombstone's session, if tmux still reports one, is left to `open` (which attaches to it) — no check is made |
| otherwise | slot = the smallest slot in `0..max_slots` not held by any live tree or tombstone and not squatted; `port_base = base + slot·stride` from current settings; reject a candidate whose range overlaps any persisted range (`GEOMETRY_CONFLICT`, next slot); compute `name_short`/`session_name` (§3.1) → `IDENTITY_COLLISION` on collision; persist all of them |

Squatted = the bind+connect probe (bind on IPv4 loopback and connect, 50 ms per port) finds any port of the block in use (`SLOT_SQUATTED` info). Exhaustion → `SLOTS_EXHAUSTED` (4). Ports are allocations, not functions of the name; scripts read `wt env`. Application behaviour is not enforced (A13).

**`ports` map** is recorded per incarnation as an append-only `Map<PortName, index>`: for `register`/`adopt` from the checkout's effective config at allocation; for `new` at step S4 (§11.2). `WT_PORT_<NAME>` = `port_base + ports[name]`. **Append rule `PORTS(tree, cfg)`** (called by §9.1 D4 under the shared tree lock, committing via registry RMW): every name in `cfg.ports` absent from the map is assigned the smallest index not in the map's values; a name absent from `cfg.ports` keeps its index (never reused); config order is irrelevant; if no index `< stride` is free → `PORTS_EXHAUSTED` (4), remedy "`wt remove` + `wt new` (a fresh map), or raise `ports.stride` for future trees".

## 8. Environment
### 8.1 Activation marker and `deactivate`
`WT_ACTIVATION` is the **only** marker; its value is metadata about every other key the activation changed. Every other `WT_*` key is ordinary (a marker-free parent may contain `WT_TARGET`; it is just a prior value).

```
Activation := { v: 1, target, home, applied: Map<Key, String>, prior: Map<Key, String|null> }   // same key set; never WT_ACTIVATION
Key        := EnvKey | "PATH" | /^WT_/ except "WT_ACTIVATION"
deactivate(parent) -> { clean, prior: Option<Activation>, report }
  if WT_ACTIVATION ∉ parent: return { clean: parent, prior: None }
  act = parse(parent.WT_ACTIVATION); if unparsable, v != 1, keys(applied) != keys(prior), any key ∉ Key:
        notice ACTIVATION_IGNORED (warn); return { clean: parent minus WT_ACTIVATION, prior: None }
  clean = parent minus WT_ACTIVATION
  for (k, _) in act.applied (lexical): clean[k] = prior[k] or remove; report.restored += k        // no comparison with the current value (A5, A31)
```
`deactivate` may do exactly this and nothing else. A user who edits a tool-set key inside a door is overridden by the next door; a user who edits `WT_ACTIVATION` gets what they wrote, bounded to listed keys; this is not a security boundary. Every shell-facing inverse (`wt env --deactivate --sh`, §8.5) is a transliteration of exactly these actions.

### 8.2 `assemble`
```
EnvInputs  := { cfg: EffectiveScope, tree: TreeIdentity (registry entry incl. geometry, ports, name_short, session_name), home,
                contributed: [(ResourceKey, EnvMap)]   // ONLY resources in state `present`, sorted by key; values literal
                task: Option<TaskContext>, parent, dirs: Fn(&Path)->bool, force_env }
EnvOutput  := { env, activation, activation_json, render: [Render], report }
1. { clean, prior, dreport } = deactivate(parent)
2. env = clean; applied = {}; prior_map = {};  set(k, v): prior_map[k] = clean.get(k); applied[k] = v; env[k] = v
3. tool vars (§5.5 except WT_BIN, WT_ACTIVATION, WT_TASK, WT_SELF): set(k, v)
4. PATH: abs = cfg.bin (scope chain) joined to root; missing → report.missing_bins; set(PATH, join(abs ++ split(clean.PATH))); set(WT_BIN, join(abs))
5. contributed: for (k, v) sorted: alias_rule(k, v)
6. ctx = tool ∪ applied-contributed ∪ clean (frozen once); aliases: for (k, tpl) in cfg.env sorted: alias_rule(k, expand(tpl, ctx))
   alias_rule(k, v): if clean has k and !force_env: report.kept += k  else: set(k, v); report.(overrode|set) += k
7. task door only: set(WT_TASK, id); if resource: set(WT_SELF, expand(name, env)); for (k, tpl) in task.env sorted: set(k, expand(tpl, env))   // ends with this node
8. activation = { v:1, target, home, applied, prior: prior_map }; activation_json = canonical JSON (sorted keys, compact); env[WT_ACTIVATION] = activation_json   // not via set()
9. files: for (path, f) sorted: render += Render{ path, content: expand(text, env), mode, header }
10. report = { kept, set, overrode, missing_bins, restored: dreport.restored }
```
Every non-kept assignment is owned (in `applied`), including task env and contributed env (the A5 exception: they are tool-set and replaced by nested doors).

### 8.3 Rendering and ownership (A30.1, A31)
Tree state records `materialized: [ { path, kind: rendered|copied|seeded, hash|null, tracked_checked_at } ]`. Rendering runs **inside the tree-state RMW hold** (level 6): observe → decide → write → record, with no subprocess inside the hold. The tracked check (`git ls-files --error-unmatch -- <paths>`, one call) runs **before** the hold, only for paths without a record and at every `new`/`register`/`adopt`/`sync`; doors otherwise trust the record. Decision `render::decide(observed, record, new_bytes)`:

| Target | Record | Decision |
|---|---|---|
| 1. tracked by git (when checked) | any | `RENDER_ONTO_TRACKED` (5) |
| 2. symlink (`fstatat(AT_SYMLINK_NOFOLLOW)`) | any | `RENDER_ONTO_SYMLINK` (5) |
| 3. absent | any | `Write` |
| 4. regular file, `blake3(bytes) == record.hash` | rendered | `Write` if `new_bytes` differ else `Unchanged` |
| 5. regular file, otherwise | — | `RENDER_ONTO_USER_FILE` (5); remedy "`rm <path>` so wt can render it again, or disable it in `<tree>/.wt/config.toml`: `files.\"<path>\" = false`" |

Rows are evaluated in order; the first matching row decides. `Write` = write via §5.7 then record `hash = blake3(new_bytes)` in the same RMW. A crash between the write and the record makes the next door report row 5 (A30.1). Header when `marker != ""`: `<marker> generated by wt for <target>. If you edit this file, wt stops re-rendering it; delete it to let wt regenerate it, or set files."<path>" = false in .wt/config.toml`. The hash is the sole write authority; the header is provenance. The exclude block (§4.2) is recomputed after the hold when the materialised set changed. Accepted residuals: a human save between the hash check and the rename is overwritten (A23); two doors of one tree rendering different bytes in the same millisecond may leave a record that the next door reports as row 5 (A31).

### 8.4 Laws
For any parent `p` without `WT_ACTIVATION` and `e = assemble(p, …).env`: **L1** `deactivate(e).clean == p`; **L2** `assemble(B, e).env == assemble(B, p).env`. A structurally invalid marker is ignored with `ACTIVATION_IGNORED` and the door proceeds from the parent as-is.

### 8.5 `wt env`
`wt env [target] [--sh|--dotenv|--json] [--deactivate] [--force-env]`. Text: the full env, then the bin inventory (each declared dir, exists?, executables). `--sh`: `export K='v'` lines (`'\''` escaping) plus `unset K` for restored keys whose prior was null; `--dotenv`: `KEY=value`; `--json`: §14.4. `--deactivate --sh`: the actions of §8.1 as shell lines — first `unset WT_ACTIVATION`, then in lexical order one `export K='prior'`/`unset K` per key in `report.restored`. These are the only verbs that print environment values.

## 9. Doors
### 9.1 Door algorithm
Used by `exec`, `run` (per plan, §10.1), `shell`, `open` and `env`. The door-equivalent steps inside `new`/`sync`/`register`/`adopt` run under that verb's exclusive tree lock and skip D0–D2.

| Step | Action | Lock |
|---|---|---|
| D0 | identity check `ID(path, entry)` (§4.1) | — |
| D1 | phase (§11.1) must be `ready` or `failed` (`failed` → notice `TREE_NOT_READY`); otherwise `TREE_BUSY{phase}` (4) | — |
| D2 | acquire the tree lock **shared** (non-blocking; `TREE_BUSY{verb}` if held exclusively); create `<name>.doors/<pid>.lock` with `{pid, verb, target, since}` and hold its flock | 1 |
| D3 | read the effective config (§5) | — |
| D4 | `PORTS(tree, cfg)` (§7): persist appended names | 5 |
| D5 | `deactivate` + `assemble` (§8.1–8.2) from the updated entry | — |
| D6 | render (§8.3) and, if the materialised set changed, exclude (§4.2) | 6, 5 |
| D7 | spawn (§9.2); for `env` print and release | — |

cwd: caller's cwd if inside the tree, else the tree root (`run`: node cwd). `BIN_DIR_MISSING` is expected-state guidance: text mode renders one actionable `next` line in the command summary (or on stderr immediately before a passthrough or `run` child), including under `--quiet`; it is never formatted as `wt: BIN_DIR_MISSING — …` and wt never writes it to a `run` child's stdout. When an effective `build` task exists the action names `wt build <target>`; otherwise it says to create the missing directory without naming a nonexistent task. The notice carries the target, path and task availability as internal structured guidance rather than deriving them from its message. Its public shape remains unchanged in `notices[]` in JSON. Other notices go to stderr on a TTY or `--verbose`, and always to `notices[]`. Notices are de-duplicated by `(code, subject)` before rendering. Port-bound findings are reported by `list`/`status`/`doctor` (§12), never by a door (§0).

### 9.2 Spawn: `execvp`, the `run` parent, `--no-gate`
**Passthrough doors** (`exec`, `shell`): after D6 the wt process clears `FD_CLOEXEC` on the tree-lock and door-file fds and `execvp`s the child with the assembled env; the pid is unchanged, so the door file names the running process. `flock` is held by the open file description and survives `exec`; a child that closes every inherited fd releases the lock early — an accepted residual (A31; alongside A23/A27). Shells keep inherited fds, so `wt remove` of a tree someone is sitting in reports `TREE_IN_USE` naming the shell (A31 exception 1).

**`run` nodes** keep a wt parent for the child's lifetime: it holds the lock fds (`FD_CLOEXEC` set, not inherited), spawns the child with inherited stdio (`--json`: stdout captured, §9.3), tees output to the log, enforces `timeout`, waits, and exits with the child's code or `128+n` on signal death.

**`wt exec --no-gate <target> -- <cmd>`** (A24, A31): honoured only when `$TMUX` is set (otherwise `NO_GATE_REFUSED` (2), remedy "sessions are started by `wt open`"); performs D0–D6, removes its door file, closes the lock fd, and `execvp`s the child. The session therefore holds no wt lock; tmux is its liveness truth. Nested one-shot doors started by the agent take their own shared lock.

### 9.3 Machine protocol per door (A20)
| Door | `--json` | stdout / stderr | exit status |
|---|---|---|---|
| `exec`, `shell` | refused: `JSON_UNSUPPORTED` (2), remedy "passthrough doors have no envelope: use `wt env --json` or `wt run --json`" | child's | child's |
| `open`, `open --all`, `close` | supported; JSON suppresses attachment | one envelope / notices | classes |
| `run <task>` text | — | child stdio tee'd to the log and inherited; notices on stderr | child's once started; signal `n` → `128+n` |
| `run <task> --json` | supported | stdout = exactly one envelope; child stdout+stderr merged to stderr and the log | classes (0 / 6 `TASK_FAILED` with `error.details.child` / 8) |
| `env` | supported | envelope or export lines | classes |

Transport (A19): `exec` and `run` children receive the assembled env; `shell` execs `settings.shell.program` (default `$SHELL`, else `/bin/sh`) interactive, non-login, with the assembled env — the promise is at spawn; rc files may alter PATH; the pre-spawn banner names `WT_BIN`; `wt doctor` inside reports `PATH_NOT_SHADOWED` (remedy: the `shell-init` guard, §14.6, or the rc file).

### 9.4 Sessions
`wt open [target] [--agent X] [--no-attach] [--all]`, `wt close [target|--all]`. `session.backend = "tmux"` declares tmux as the backend; commands do not probe its version after registration.

| Situation | Behaviour |
|---|---|
| session exists (`has-session`) | release the tree lock (D2 fd and door file) as soon as `has-session` answers; never start or resume an agent; then attach when the attachment predicate below holds. Attach is a tmux client and holds no wt lock. |
| session absent, agent selected by `--agent X` or `session.agent` | `tmux new-session -d -s <session_name> -c <root> -- wt exec --no-gate <target> -- <agent start>` (the inner door assembles the environment; nothing is passed with `-e`); record the agent only after this process creates the session. A concurrent creator wins without causing a second start. |
| session absent, tree has a recorded agent and no explicit override | create through the recorded agent's `resume` recipe; the record is unchanged. |
| session absent, no agent selected or recorded | create through `wt exec --no-gate <target> -- <interactive shell>` using the same program and arguments as `wt shell`; leave the tree's agent null. |
| `--all` | attempt every live tree; recorded agents use `resume`, unrecorded trees use `session.agent`'s `start` when configured and shells otherwise; never attach. A failure for one tree is recorded as `{target, name, failed:true, code, message, remedy}` and does not stop later trees. The command exits with the highest exit class observed after the batch; JSON has `ok:false`, retains `data.sessions`, and reports the worst error at top level. |
| `session.backend = "none"` | `open` and `close` refuse with `SESSION_DISABLED` (5), naming `session.backend` and the `"tmux"` value that enables them. `list`, `remove`, and `prune` execute no tmux process. There is no foreground-agent fallback. |

Attachment occurs only when all hold: `session.attach = true`; both stdin and stdout are terminals; output is not JSON; `$WT_ACTIVATION` is unset; neither `--no-attach` nor `--all` applies. Inside tmux wt uses `switch-client`; otherwise it uses `attach-session`. These conditions affect attachment only: except for `new --no-open`, an absent session is still created. An agent therefore starts only when wt successfully creates a session, never when it attaches to one.

Consequently the only tree-lock holders are passthrough doors (through their exec'd child), `run` parents, and doors in their prelude (D2–D6); sessions and attach clients hold none. Sessions are closed by `remove`/`unregister` (§11.4 step 5), by `prune` before tombstoning (§12), and by `wt close`.

**`wt close [target|--all]`**: resolve the target; backend `none` → the refusal above; if `has-session <session_name>` → `kill-session` (no lock is taken: sessions hold none); JSON is always `{ sessions: [ { target, session: session_name, closed: bool } ] }` — one element without `--all` (`closed: false` when no session existed); `--all` iterates every live tree. Idempotent.

## 10. Tasks and resources
### 10.1 Plans, `lock_plan`, execution
```
TaskPlan := { root, nodes: [Node] }   // topological; ties by (scope, id)
Node     := { id, scope, origin, cwd, run|null, exists|null, destroy|null, tied_to|null, name|null, env, lock|null, timeout|null, ready_within|null, sys_locks: ["RepoGit"] }
Probe    := Present(0) | Absent(1) | Failed{exit(n≥2)|timeout|spawn}
            // CONTRACT: an `exists` recipe exits 0 present, 1 absent, ≥2 when it cannot tell (daemon down, tool missing); `sh -c` recipes
            // talking to infrastructure should begin with a reachability test that exits 2 (A31; orbitcloud does)
lock_plan(node, held) -> ascending [ Tree(shared) unless held, RepoGit if "RepoGit" ∈ sys_locks, Resource(key) if resource, Named(node.lock) if lock ]   // levels 1–4; the sole acquisition authority; 5/6 are leaf RMWs inside steps
```
`wt run <task> [target] [--wait d|forever] [--timeout d] [--force-env] [--dry-run] [--no-log]` (aliases `test lint fmt build`; `sync` is §11.3; `destroy`/`refresh` drive §10.4):

| Step | Action | Lock |
|---|---|---|
| X0 | door D0–D6 (§9.1) | 1,5,6 |
| X1 | for each node in plan order: `assemble` with `contributed` = resources currently `present`; acquire `lock_plan(node, held)` in order | per plan |
| X2 | resource node: refresh its declaration (§10.5) then `resource::step` with event `Run` (§10.4). Task node: `exists` → Present: "present", skip; Failed: `TASK_PROBE_FAILED` (6), stop; then spawn `run` (cwd `node.cwd`) through the `run` parent (§9.2); non-zero → "failed", stop | — |
| X3 | release the node's guards in reverse order; continue | — |

Output is tee'd to `<tree>/.wt/logs/<ScopeEnc>-<task>-<utc>.log`; before writing a new log, logs of the same `(scope, task)` beyond the newest `logs.keep − 1` are deleted (§12). `--dry-run` prints the plan with `lock_plan` output and executes nothing; task env ends with its node.

### 10.2 Resource identity and lock
`ResourceKey := { label, tied_to, name|null (tree name; null for repo), scope: RelDir, task }`; state key `"<ScopedTask>"`; lock path per §4 (level 3), held from probe through the final commit. CLI selection by `ScopedTask`.

### 10.3 Snapshots and `execute`
```
ResourceSnapshot := { schema: 1, key, name, cwd_rel: RelDir, exists: CmdExpanded|null, destroy: CmdExpanded, run: CmdExpanded|null,
                      env: Map<String,String>,                     // minimised (below)
                      bin_dirs: [AbsPath], bin_exes: [String],     // declared bin dirs and the executable names found in them at snapshot time (A25)
                      roots: { tree, home }, recorded_at }
CmdExpanded := { shell: String } | { argv: [String] }
```
- **Env minimisation.** `env` contains exactly: all `WT_*` keys of the recording assembly except `WT_ACTIVATION`; `PATH`; every declared alias key and the resource's task-env keys with their assembled values; keys listed in `snapshot_env`. For **repo-tied** resources the tree-specific keys of §5.5 are removed (A28). Nothing else is stored; a recipe that needs a frozen parent value names it in `snapshot_env`. `register` prints the keys each resource will persist; no report prints values (§14.4).
- **Executable inventory (A25).** `bin_dirs` = the tree's declared `bin` directories (absolute) and `bin_exes` = the names in them that `exec` would run — symlinks resolved, since a link to a binary runs under its own name — both captured at snapshot time.
- **Working directory.** Tree-tied: `roots.tree/cwd_rel`. Repo-tied: the label's current canonical path (read from the registry) joined with `cwd_rel`; `register --move-to` therefore needs no snapshot rewrite.

`execute(snapshot, Exists|Destroy|Run, tree_missing)`, reading no config layer:
1. **Environment.** The invoking wt process's environment overlaid by `snapshot.env` (snapshot keys win).
2. **Missing-tree rule (A25).** `tree_missing` = `roots.tree` is absent **or the tree's phase is `replaced`** (§11.1) — a replaced directory is never used. If `tree_missing` or any `bin_dirs` entry is absent: split **the recipe text about to be run** on whitespace and `; | & ( ) < >`, strip `'` and `"`; if any word equals a name in `bin_exes` → do not run; result `orphaned(exe_missing)`, remedy "rebuild the tree's binaries, or destroy by hand: `<recipe>`". Otherwise run with `PATH` stripped of every `bin_dirs` entry. If the tree exists (not replaced) and all `bin_dirs` exist, run with the env unchanged. wt's guarantee for a replaced or missing tree is exactly: no process is spawned with a cwd inside the replacement directory, and no replacement binary is on PATH; identities a recipe derives from the recorded `WT_ROOT` string are the recipe's own semantics (A29).
3. **cwd.** Tree-tied: `roots.tree/cwd_rel` if it exists and `tree_missing` is false, else `$TMPDIR` (else `/tmp`) with `WT_ROOT` left at the recorded string. Repo-tied: canonical path `/cwd_rel`; absent → `orphaned(repo_root_missing)`, remedy "`wt register … --move-to`, or destroy by hand".
4. Spawn `sh -c shell` or `argv`; deadlines §13.3.

### 10.4 Resource state machine (A22, A31)
```
ResourceRecord := { key, declaration: ResourceSnapshot, instance: ResourceSnapshot|null, state: declared|present|orphaned, reason|null,
                    external: bool, undeclared: bool, last_probe: {at, result}|null, last_error: {at, event, message, child|null}|null, since }
```
Probes, runs and destroys use `instance` when present, else `declaration` (§10.5). The **instance is frozen** from the fresh declaration (i) immediately before wt spawns `run` (durable before the spawn, so a crash mid-run still leaves a teardown snapshot) or (ii) at the first Present probe when `instance` is null (`external = true`); it is cleared only by a confirmed-absent probe. `Destroy` carries `teardown` (true inside `remove`, `unregister`, `prune`). Every state write is durable before the next effect; a `Failed` probe never triggers `run` or `destroy`. There are no persisted in-progress states: the resource lock (§10.2) serialises transitions and the next probe decides after a crash (principle 4). `name` default: tree-tied `${WT_NAME_SHORT}_<name_snake(ScopedTask)>`, repo-tied `${WT_LABEL}_<name_snake(ScopedTask)>`; `WT_SELF` is the expanded value.

| State | Run | Probe (`list`/`status --probe`) | Destroy | Refresh |
|---|---|---|---|---|
| **declared** | probe: Present → **present** (freeze if null; external); Absent ∧ `run` → freeze instance → run → RunFail: stays, `last_error`, `TASK_FAILED` (6); RunOk → `ready_within` poll or one probe: Present → **present**; Absent → stays, `last_error(absent_after_run)` (6); Failed → stays, `last_error(probe_failed)` (6). Absent ∧ no `run` → stays, exit 0, notice `RESOURCE_DECLARED_EXTERNAL`. Failed → stays, `RESOURCE_PROBE_FAILED` (6) | Present → present (freeze if null); Absent → stays, instance cleared; Failed → stays, finding | probe: Present → as **present**/Destroy; Absent → teardown: **dropped**, else stays; Failed → teardown: **orphaned**(probe_failed), else `RESOURCE_PROBE_FAILED` (6) | as Destroy, then as Run |
| **present** | probe: Present → stays, "present"; Absent → **declared** (instance cleared, `RESOURCE_GONE`) then as declared; Failed → stays, `RESOURCE_PROBE_FAILED` | Present → stays; Absent → declared, cleared (`RESOURCE_GONE`); Failed → stays, finding | run `destroy`: DestroyFail (incl. `exe_missing`, `repo_root_missing`, timeout) → **orphaned**(reason); DestroyOk → probe: Absent → instance cleared; **dropped** if `undeclared` or teardown, else **declared**; Present → **orphaned**(still_present); Failed → **orphaned**(probe_failed) | as Destroy; then as Run if `run` else success |
| **orphaned** | refused `RESOURCE_ORPHANED` (6) | Present → stays; Absent → **declared**, cleared (`RESOURCE_GONE`); Failed → stays | as **present**/Destroy (retry) | refused |

Invariants: a record exists for every effective tree-tied resource from its first refresh; `present` only on a Present probe; a record is dropped only after a confirmed-absent probe and only when undeclared or during teardown; teardown terminates (after one `Destroy{teardown}` pass every record is dropped or `orphaned`); a no-`run` resource is never run.

### 10.5 Declaration refresh; repo-tied declarations
**When.** Declarations are refreshed at `register`/`adopt` (I3), `new` (S4), `sync`, `wt run <resource>` (X2, before the step), `list`/`status --probe`, and `remove`/`unregister` step 8 — never by an ordinary door (§0). For each effective resource `r` across all scopes: run `assemble` once more with `task = TaskContext{r}` and `r`'s scope from the same `clean` parent, render output discarded, notices suppressed except `ENV_UNDEFINED` (→ `REFRESH_SKIPPED{r}` warn, record unrefreshed); build `r`'s snapshot (§10.3); then, by `tied_to`:
- **tree-tied** → the tree's state file (tree RMW, level 6): upsert the record: absent → **declared** with `declaration`; otherwise replace `declaration` only, never `instance`;
- **repo-tied** → `_repo.json` (repo RMW, level 6): upsert `resources[<ScopedTask>].declaration` from the refreshing tree's **stripped** snapshot (A28, A31); `instance` and state are never touched by a refresh.

Undeclared tasks keep their records with `undeclared = true` until dropped by a confirmed-absent probe during teardown. **Repo-tied semantics:** while `instance` exists it governs; otherwise the most recent stripped declaration (the invoking tree's, since an action refreshes first) is effective. There is no cross-tree agreement check. Ordinary `remove` of a non-canonical tree tears down tree-tied records only; repo-tied instances survive while the label remains and are destroyed only by `destroy`/`refresh`/`unregister`.

Worked example — orbitcloud `pgdata` (no `run`): `new` → declared; `run pgdata` → Absent → declared, exit 0 with the notice; Aspire creates the container; `list --probe` → present (external, instance frozen); `refresh` → destroy → declared; `remove` → `Destroy{teardown}`: present → destroyed and dropped, absent → dropped; docker down → probe exits 2 → `orphaned(probe_failed)` and `remove` stops (§11.4 step 9) unless `--keep-orphans`. orbit `daemon` (`needs = ["build"]`): `run daemon` → `build` runs → instance frozen (its `bin_exes` contains `orbit`) → `orbit server start` → confirming probe → present; after `rm -rf <tree>`: `prune` → missing-tree rule: the `destroy` text contains the word `orbit` ∈ `bin_exes` → `orphaned(exe_missing)`; the installed `orbit` is never run.

## 11. Tree lifecycle
### 11.1 Tree state and derived phase
```
TreeState := { schema: 1, tree_id, label, name, phase: initialising|bootstrapping|ready|failed|removing,
               op: {verb: "new"|"sync"|"remove"|"register"|"adopt", pid, started}|null, verify_pending: bool,
               sync: {at, ok, inputs, log}|null, verify: {at, ok, log}|null,
               resources: Map<ScopedTask, ResourceRecord>, materialized: [...], last_error|null }
```
`op` is the only op record (A30 as read by A31). `derive_phase(obs)`, `obs = { entry?, dir_exists, identity_ok (§4.1), state?, lock_held, git_knows }` where `lock_held` = a non-blocking exclusive try-flock of the tree lock fails, consulted **only** when `state.phase ∉ {ready, failed}` (doors are refused in those phases, so a holder is the lifecycle verb itself):

| Entry | Dir | state.phase | lock held | Phase | Remedy |
|---|---|---|---|---|---|
| yes | yes, ok | ready, `!verify_pending` | — | `ready` | — |
| yes | yes, ok | ready, `verify_pending` | yes / no | `verifying` / `ready` + `VERIFY_PENDING` | wait / `wt new … --verify` resumes V |
| yes | yes | initialising | yes / no | `initialising` / `init-interrupted` | wait / re-run `register`/`adopt` |
| yes | yes | bootstrapping | yes / no | `bootstrapping` / `interrupted` | wait / `wt new` or `wt sync` resumes |
| yes | yes | failed | — | `failed` | `wt sync` |
| yes | yes | removing | yes / no | `removing` / `remove-interrupted` | wait / `wt remove` |
| yes | no | bootstrapping (`op.verb = new`) | yes / no | `creating` / `claimed` | wait / `wt new` resumes |
| yes | no | any other | — | `missing` | `wt prune` / `wt remove` |
| yes | yes | no state file | — | `incomplete` | `wt new` resumes / `wt remove` |
| yes | yes, identity fails | any | — | `replaced` | §4.1 remedy |
| no | — | — | — | `unmanaged` / `stale-git` | `wt adopt` / `wt prune` |

### 11.2 `new`
```
wt new <label>/<name> [--branch B] [--from REF] [--detach] [--no-sync] [--verify] [--no-fetch] [--no-open] [--no-attach]
REF := <local branch> | <remote>/<branch> | pr:N | <PR URL> | <rev>
```
`--from` bare `X`: `refs/heads/X` → `refs/remotes/origin/X` → rev; both present and different → local wins with `FROM_LOCAL_SHADOWS_REMOTE`; `origin/X` forces remote. Default start `origin/<default>` after a bounded fetch (`--no-fetch` skips; unpushed local default-branch commits need `--from main`). Default branch: `origin/HEAD` → `main`/`master`/`trunk` → HEAD, cached, refreshed on fetch. PR refspec by origin host: github `refs/pull/N/head`; gitlab `refs/merge-requests/N/head`; bitbucket `refs/pull-requests/N/from`; unknown: pull then merge-requests; fetched as `refs/wt/pr/N`, local branch `pr/N`, default name `pr-N`. A PR URL selects the label whose normalised origin (https or scp-style ssh, `.git` stripped) matches host and `owner/repo`; zero/many → error with remedy. `B` defaults to `<name>`; `--branch feature/x` without a name → `feature-x`. `AddSpec`: existing branch · `-b B <start>` with `--no-track` unless start is `refs/remotes/*` · `--detach`.

Decision (inside the registry transaction, under the exclusive tree lock):

| Existing entry | Decision |
|---|---|
| none (a tombstone may exist: §7 inherits and deletes it) | allocate per §7; write the state file `{bootstrapping, op{new}}`; write the entry; from G |
| `ready`, identical source | no `--verify`: `created: false`, exit 0; `--verify`: set `verify_pending`, run V, `created: false` |
| `ready`, `verify_pending`, identical source | resume V (`resumed: true`) |
| `ready`, different source | `NAME_TAKEN` (4) |
| `missing` (live entry, directory gone), identical source | any resource record in its state → `TREE_MISSING_PENDING` (4), remedy "`wt prune --records <target>` then `wt new`"; no record → **fresh incarnation**: overwrite the state file `{bootstrapping, op{new}}` with a fresh `tree_id`; one registry write: new `tree_id`, coordinates inherited from the entry; from G |
| `creating`/`claimed`/`bootstrapping`/`interrupted`/`failed`/`incomplete`, identical source | resume from the first unfinished step (dir missing → G; dir present, no state → S1; bootstrapping/failed → S5); `resumed: true` |
| same, different source | `NAME_TAKEN`, remedy `wt remove` |

| Step | Action | Lock |
|---|---|---|
| S0 | tree lock exclusive for the address (deadline `locks.tree_exclusive`; `TREE_IN_USE` (4) naming holders) | 1 |
| R | state file (6) then registry txn (5): decision; §7 allocation and inheritance; path/gitdir uniqueness (§4.1); exclude recompute on inheritance | 6, 5 |
| G | fetch (class `fetch`); porcelain list: branch held elsewhere → `BRANCH_IN_USE` (4); **path check**: `<path>` exists and is not a worktree git lists at that path → `PATH_OCCUPIED` (5), remedy "move or delete `<path>` (a non-wt object is there)"; `worktree add` | 2 |
| S1 | state `{bootstrapping, op}` (confirmed); write `.wt/tree_id` | 6 |
| S2 | exclude block | 5 |
| S3 | `copy`, `seed` (§11.7); record `materialized`; exclude | 6, 5 |
| S4 | `PORTS` (§7); assemble + declaration refresh (§10.5) + render (§8.3) | 5, 6 |
| S5 | `sync` through §10.1 under the held lock (unless `--no-sync`) | 2,3,4,6 |
| S6 | state: `sync.inputs`, phase `ready`, `op = null`, `verify_pending = --verify` | 6 |
| V | `--verify`: run `verify` through §10.1; state `verify = {at, ok, log}`, `verify_pending = false` | 2,3,4,6 |
| F | release | — |

After F, `new` prints its phase-1 human summary, then applies §9.4: with backend `tmux`, it ensures the session and attaches when the attachment predicate holds. `--no-open` skips both creation and attachment. `--no-attach` still creates the session. Backend `none` leaves the ready tree without a session. Agent selection comes only from `session.agent`; `new` has no agent flag. Session provisioning is additional to the completed tree: if it fails, `new` emits warning notice `SESSION_CREATE_FAILED` naming `wt open <target>` as the retry, exits 0, and retains the complete `NewData` payload in JSON as well as text mode.

Failures: G → class 7/5/4, entry stays (`claimed`, resumable); S4/S5 → `failed`, tree remains (R3); V → `VERIFY_FAILED` (6), tree `ready` with `verify_pending` cleared. Never `created:false` without `ready`.

### 11.3 `sync`
`wt sync [target] [--force]`: tree lock exclusive (1, deadline; `TREE_IN_USE`) → identity check → state phase `bootstrapping`, `op{sync}` (6) → tracked check for rendered paths (§8.3) → declaration refresh (§10.5) → run `sync` through §10.1 → record input hashes (`git hash-object` of adapter ∪ config `sync_inputs`) → `ready`/`failed`, `op = null` (6) → release. Unchanged inputs ∧ `ok` ⇒ no-op unless `--force`.

### 11.4 `remove`
Classification (pure over git results): "unpushed" = upstream present ∧ `rev-list --count @{u}..HEAD > 0`; upstream `[gone]` ⇒ yes; no upstream or detached ⇒ `git branch -r --contains HEAD` empty; tags ignored. `dirty` = `git status --porcelain=v1 --untracked-files=normal` non-empty.

| Step | Action | Lock | Mutates |
|---|---|---|---|
| 1 | resolve; canonical → `USE_UNREGISTER` (2) (this refusal applies to `wt remove` only; `unregister` skips it, §11.5) | — | no |
| 2 | observe: dir exists?, identity (§4.1), dirty, unpushed, `has-session`, live door holders, tree-tied records with fresh probes | 3 | no |
| 3 | build `RemovePlan`; dirty/unpushed without `--force` → `TREE_DIRTY` (5) | — | no |
| 4 | consent for every removal (clean, forced, missing or replaced directory): TTY without `--yes` → prompt with the plan (it lists the session and the door holders); non-TTY without `--yes` → `CONFIRM_REQUIRED` (2); `n` → exit 0, `removed: false` | — | no |
| 5 | `kill-session` if `has-session` (consented); then tree lock exclusive with deadline `locks.tree_exclusive` (`--wait d`); timeout → `TREE_IN_USE` (4) naming the holders, remedy "wait for or stop them" | 1 | session only |
| 6 | revalidate under the lock: identity check (§4.1) (mismatch → `TREE_REPLACED`, nothing destroyed); dirty/unpushed re-observed (newly dirty without `--force` → `TREE_DIRTY`, nothing changed) | — | no |
| 7 | state `removing`, `op{remove}` | 6 | yes |
| 8 | declaration refresh if the dir exists (§10.5); every tree-tied record: §10.4 `Destroy{teardown}`; all attempted | 3, 6 | yes |
| 9 | any record not dropped → without `--keep-orphans`: `DESTROY_FAILED` (6), tree stays `removing` → `remove-interrupted` with its records, nothing below runs (remedy: fix, then `wt remove` again, or `--keep-orphans`, or `wt prune --records <target>`); with `--keep-orphans`: state `op = null`, continue; the entry stays live after step 10 (derived phase `missing` with records; `TREE_MISSING_PENDING` protects the name) and step 11 is skipped | — | |
| 10 | `git worktree remove --force <entry.path>`; `--delete-branch` if merged or `--force`; missing dir → `git worktree prune` | 2 | yes |
| 11 | registry: entry → tombstone (record-free; carries `materialized` paths); delete the state file; exclude block | 5 | yes |
| 12 | release | — | |

**Missing directory:** steps 2 (records only), 3–5, 7–8 (records only, `execute` with `tree_missing = true`), 9, 10 (`git worktree prune`), 11. **Replaced directory** (phase `replaced`): steps 2 (records only), 4–5, 7–9 with `tree_missing = true`; no git step (the directory is not ours; doctor reports it as `UNMANAGED_WORKTREE`/`STALE_GIT_WORKTREE`); then 11 (A31). Already absent ⇒ `removed: false`, exit 0.

### 11.5 `unregister`
`wt unregister <label> [--yes] [--force]`: refuses while non-canonical trees exist (`TREES_EXIST` 5) unless `--force` (removes them first via §11.4, one consent prompt listing all). For the canonical tree the teardown is performed inline: §11.4 step 1 **without** its canonical refusal, then steps 2–8 exactly as written (step 8 is the **only** tree-tied teardown pass), then **every repo-tied record** with `Destroy{teardown}`; **failure barrier**: if any tree-tied or repo-tied record is not dropped → `DESTROY_FAILED` (6), the canonical tree stays `removing` → `remove-interrupted`, and nothing below runs; otherwise artefact cleanup — hash-owned rendered files deleted via §5.7, `.wt/` deleted (consented; it may hold application data), anything else `ARTIFACT_KEPT` with its exclusion retained; exclude block removed if nothing kept; registry and state records deleted. The checkout is never deleted.

### 11.6 `register` and `adopt`
`wt register [path] [--label L] [--move-to PATH] [--repair]`, `wt adopt <path> [--label L] [--name N]`:

| Step | Action | Lock |
|---|---|---|
| S0 | tree lock exclusive for the address | 1 |
| R | write the state file `{initialising, op}` (6), then registry txn (5): path/gitdir uniqueness (§4.1); label (register) and tree entry; §7 allocation incl. `ports` from the checkout's config; identical existing registration with no `op` ⇒ `registered: false` (the pre-written file is deleted); `init-interrupted` ⇒ resume | 6, 5 |
| I1 | write `.wt/tree_id` | — |
| I2 | exclude block; print the declared summary incl. the keys resources will persist (R10) | 5 |
| I3 | assemble + declaration refresh (§10.5) + render (§8.3) | 6, 5 |
| I4 | state `ready`, `sync: null`, `op = null` | 6 |

`adopt` requires the path to be listed by `git worktree list` of that gitdir (`NOT_A_WORKTREE` 5). `register --move-to` updates the canonical path and runs `git worktree repair`.

`register <path> --label L --repair` recovers a canonical checkout in derived phase `replaced` because its `.wt/tree_id` marker is absent or wrong. It succeeds only when `path` is label `L`'s recorded canonical path, its common gitdir still matches the label, and the phase is `replaced`; otherwise `REPAIR_REFUSED` (5). Under the exclusive tree lock it rewrites the marker from the registry entry, recomputes the exclude block, and re-renders hash-owned files. It does not allocate or append coordinates, refresh resource declarations, alter resource/sync/verify state, or touch tombstones. `doctor`'s `TREE_REPLACED` remedy for a canonical tree names this command.

### 11.7 `copy` and `seed`
Run exactly once per incarnation at `new` S3 (never for canonical or adopted trees). Source root = the canonical checkout. Per entry: source absent → `COPY_ABSENT` info; tracked by git → `COPY_TRACKED` (5), `new` aborts at S3 (`incomplete`, resumable); destination exists → `COPY_EXISTS` info, never overwritten; otherwise copied via §5.7 (files byte-for-byte with mode, directories recursively, symlinks recreated); record `materialized {kind: copied|seeded, hash: null}`. Repository-declared `seed` prefers reflink per file and falls back to a byte copy with `SEED_COPIED_NOT_CLONED` info. An adapter seed is reflink-only: if any file cannot be cloned, remove any partial destination, skip the entry without a materialized record, and emit `SEED_SKIPPED_NO_REFLINK` info naming the path and reason. Copied/seeded paths are not hash-owned and are never re-rendered or individually deleted; they are excluded while the tree or its tombstone exists. Task side effects outside the tree are never tracked; a side effect that occupies a future tree path surfaces as `PATH_OCCUPIED` at that tree's `new` (§11.2 G).

## 12. Truth: `list`, `status`, `doctor`, `prune`, logs
`wt list [label] [--probe] [--fast] [--disk]`, `wt status [target] [--probe]`: address, phase (§11.1), branch/detached, dirty counts, upstream ahead/behind, behind default, sync state (`ok | stale (<files>) | failed | never`, `behind <default> by N`, **`drift (<files>)`** = sync inputs changed on the default branch since the merge-base: one bounded `git diff --name-only HEAD...origin/<default> -- <sync_inputs>` per tree, S3/A31; `--fast` skips it), session `yes|no|unknown`, agent, resources `{scope, task, state, external, undeclared, last_probe, last_error}`, slot/ports (+`bound` from one bind probe per declared port, skipped by `--fast`), path. `--probe` refreshes declarations (§10.5) and runs `exists` per record under its resource lock.

`wt doctor [label] [--probe]` findings `{severity, code, subject, message, remedy}`:

| Code | Condition (owner) |
|---|---|
| `STATE_ORPHAN` (info: a state file whose address has no live entry; deleted by `prune`); `REPO_PATH_MISSING`, `TREE_REPLACED` | §4.1, §4.3–4.4, §5.4 |
| `TREE_MISSING`, `TREE_INCOMPLETE`, `TREE_INTERRUPTED`, `INIT_INTERRUPTED`, `REMOVE_INTERRUPTED`, `TREE_CLAIMED`, `VERIFY_PENDING`; `UNMANAGED_WORKTREE`, `STALE_GIT_WORKTREE`, `BRANCH_MERGED` (`merge-base --is-ancestor` ∧ not equal), `UPSTREAM_GONE` (`%(upstream:track) == [gone]`) | §11.1; git vs registry |
| `RESOURCE_ORPHANED`, `RESOURCE_GONE`, `RESOURCE_UNDECLARED`, `RESOURCE_PROBE_FAILED`, `REFRESH_SKIPPED`, `NAME_MAY_COLLIDE` (info: a `name` template uses `WT_NAME_SNAKE`/`WT_NAME` but none of `WT_NAME_SHORT`/`WT_SLOT`/`WT_ROOT`) | §10.3–10.5 |
| `TREE_MISSING_PENDING`, `GEOMETRY_CHANGED` (info), `SLOT_SQUATTED`, `PORT_SQUATTED` (warn: bound with no session and no running task), `PORTS_EXHAUSTED` | §7 |
| `ADAPTER_TOOL_MISSING`, `ACCELERATOR_*`, `NO_LOCKFILE`, `NO_ADAPTER`, `NO_VERIFY` | §6 |
| `NO_COORDINATION` (info: the label's effective root config declares no `ports`, no `env` alias and no resource, so parallel trees share the application's default coordinates; remedy "declare `ports`/`env` in `.wt.toml` or `$WT_HOME/config.toml [repos.<label>]`"; A13); `SESSION_BACKEND` (info: the effective session backend) | §12, §5.4 |
| `BIN_DIR_MISSING`, `PATH_NOT_SHADOWED`, `PORT_BOUND`, `SEED_SKIPPED_NO_REFLINK` (info); `EXCLUDE_MISSING`, `EXCLUDE_REPAIRED`, `ACTIVATION_IGNORED` | §9, §12, §4.2, §8.1 |
| `IDENTIFIER_LONG` (resource name > 63); `TREE_IN_USE` (info, holders), `GIT_TOO_OLD` (< 2.31) | §5, §13, tooling |

`wt prune [label] [--yes] [--merged] [--gone] [--records <target>]`: retries orphaned destroys (`Destroy` on `orphaned`); runs §11.4's missing-directory path for `missing` trees (ending in a tombstone); `git worktree prune`; deletes `STATE_ORPHAN` files; `--merged`/`--gone` remove clean trees so classified (dirty ⇒ `keep`). Before any step that creates a tombstone, `prune` `kill-session`s the address's session if tmux reports it (consented by the same prompt). `--records <target>` applies to **live entries** in phase `missing`, `replaced` or `remove-interrupted`: it drives that entry's records with `Destroy{teardown}` from their own snapshots (§10.3, `tree_missing = true` for `missing`/`replaced`) and never acts on any directory; it creates no tombstone (the entry stays live until `wt remove`/`wt new`). **Tombstone collection**: for each tombstone of the label, after the session check, delete the tombstone and recompute the exclude block in one registry RMW (5). Consent: TTY without `--yes` → prompt; **non-TTY without `--yes` → print the plan, exit 0 with `data.applied = false` and notice `CONFIRM_REQUIRED`** (prune is a report-then-act verb; §14.2).

**Log retention (A31).** `<tree>/.wt/logs/` keeps the newest `logs.keep` (default 20) logs per `(scope, task)`; older ones are deleted by the `run` that creates a new log (§10.1). `--no-log` writes none.

## 13. Concurrency
### 13.1 Lock families
| Level | Lock | Mode | Holders and hold scope (owner) |
|---|---|---|---|
| 1 | tree | shared / exclusive | shared: passthrough doors through their exec'd child, `run` parents for the child's lifetime, doors in D2–D6 (§9.1–9.2); exclusive: `register`/`adopt`/`new` (through V)/`sync`/`remove`/`unregister` for their whole run (§11) |
| 2 | repo-git (by `gitdir_id`) | exclusive | wt's own git mutations (`fetch`, `worktree add/remove/prune/repair`) and `@submodules` nodes |
| 3 | resource | exclusive | one resource transition incl. probes (§10.2) |
| 4 | named | exclusive | a task node with `lock` (§10.1) |
| 5 | registry RMW | exclusive | one RMW (leaf); includes exclude-block recomputation (§4.2) |
| 6 | state RMW | exclusive | one RMW (leaf); includes rendering (§8.3) |

Lock files carry `pid target verb since`. Leaf levels 5–6 are never held while acquiring another lock or across a subprocess.

### 13.2 Order, tokens, per-verb lock sequence
Acquire strictly in increasing level; leaf levels inside any step. `TreeToken` (1) and `GitToken` (2) are `!Clone` fd-owning values; `lock_plan` (§10.1) omits held levels; an executor never re-acquires a level it holds a token for. Step ids are defined in the owning sections; `s`/`x` = shared/exclusive.

| Verb | Sequence |
|---|---|
| door (`exec`/`shell`/`env`/`open`) | D2(1s) D4(5) D6(6,5) D7 — passthrough: 1s inherited by the child; `open`: 1s released per §9.4 before attach; `--no-gate`: 1s released before `execvp` (§9.2) |
| `run` | X0 = door D2–D6; per node X1 `lock_plan` ⊆ {2,3,4} with 6 inside; X3 release |
| `register`/`adopt` | S0(1x) R(6,5) I2(5) I3(6,5) I4(6) |
| `new` | S0(1x) R(6,5) G(2) S1(6) S2(5) S3(6,5) S4(5,6) S5(nodes: 2?,3?,4?,6) S6(6,5) V(nodes) F |
| `sync` | 1x, 6, nodes, 6 (§11.3) |
| `remove` / `unregister` | step 2 probes (3), consent, 5(1x), 7(6), 8 (3 per record, 6), 10(2), 11(5) / as `remove` plus repo-tied records (3,6), cleanup, registry (5) |
| `prune` | per item as `remove` (session kill before step 11); `--records`: step 8 only (3, 6); tombstone collection: session check, 5 |
| `close` / `list`/`status`/`doctor` | none / no long locks; 3 for `--probe`, 6 to record; try-flock for liveness only in non-ready phases (§11.1) |

### 13.3 Deadlines (A14)
Every wait and every wt-owned subprocess has a default deadline; only `--wait forever` on named locks is unbounded, explicitly. Expiry: SIGTERM, 5 s, SIGKILL; the error names the class.

| Item | Key | Default | Override | On expiry |
|---|---|---|---|---|
| tree shared / exclusive | — / `locks.tree_exclusive` | non-blocking / 30s | — / `--wait d` | `TREE_BUSY` (4) / `TREE_IN_USE` (4) |
| repo-git | `locks.repo_git` | 60s | `--wait d` | `LOCK_HELD` (4) |
| resource lock | `locks.resource` | 120s | `--wait d` | `LOCK_HELD` (4) |
| named lock | `task.lock_wait` | 0s | `--wait d|forever` | `LOCK_HELD` (4) |
| registry / state RMW | `locks.rmw` | 5s | — | `LOCK_TIMEOUT` (8) |
| git query / fetch / clone / worktree / submodule | `git.timeouts.*` | 30s / 120s / 600s / 60s / 600s | `--no-fetch`; task `timeout` | `TIMEOUT` (8) |
| `exists` probe; `destroy`; task `run` | `task.probe_timeout` / `task.destroy_timeout` / `task.timeout` | 10s / 60s / none | task `timeout`, `--timeout` | Probe=Failed / DestroyFail / `TIMEOUT` (8) |
| tmux commands / port probe | `session.tmux_timeout` / — | 10s / 50 ms per connect | — | `TMUX_FAILED` (7) / treated as free |

## 14. CLI
### 14.1 Commands (A15 verbs; ★ added)
| Command | Idempotent | Owner |
|---|---|---|
| `wt register [path] [--label L] [--move-to PATH] [--repair]` | yes (resumes/repairs) | §11.6 |
| `wt unregister <label> [--yes] [--force]` | yes | §11.5 |
| `wt clone <url> [--label L] [--path P]` | yes | `git clone` (class `clone`) to `P` (default `$PWD/<stem>`), then §11.6 |
| `wt new <label>/<name> [--branch B] [--from REF] [--detach] [--no-sync] [--verify] [--no-fetch] [--no-open] [--no-attach]` | yes (phase-aware) | §11.2 |
| `wt adopt <path> [--label L] [--name N]` ★ | yes | §11.6 |
| `wt remove <target> [--yes] [--force] [--delete-branch] [--keep-orphans] [--wait d]` | yes | §11.4 |
| `wt sync [target] [--force]` | yes | §11.3 |
| `wt list [label] [--probe] [--fast] [--disk]`, `wt status [target] [--probe]` ★ | — | §12 |
| `wt prune [label] [--yes] [--merged] [--gone] [--records T]` | yes | §12 |
| `wt run <task> [target] …` (aliases `test lint fmt build`); `wt destroy <ScopedTask> [target]`, `wt refresh <ScopedTask> [target]` | per task / per §10.4 | §10.1, §10.4 |
| `wt exec [target] [--force-env] [--no-gate] -- <cmd…>` | — | §9.1–9.2; `--help`: "passthrough door; not a task (see `wt run`); no `--json` (A20)" |
| `wt shell [target] [--force-env]` | — | §9.3 |
| `wt env [target] …` | — | §8.5 |
| `wt open [target] [--agent X] [--no-attach] [--all]`, `wt close [target\|--all]` | yes | §9.4 |
| `wt path [target]`, `wt which [target] <cmd>` ★, `wt tasks [target] [--private]` ★, `wt config [target] [--origin]` ★, `wt locks [label]` ★ | — | the root (one line); one executable under the door PATH; effective tasks; effective config per key with layer; lock table |
| `wt doctor [label] [--probe]` | — | §12 |
| `wt shell-init <zsh\|bash\|fish>` ★, `wt completions <shell>` ★ | — | §14.6 |

Global: `--json`, `--yes`, `--quiet`, `--verbose`, `--color auto|always|never`, `--home DIR`. Unknown subcommand → exit 2 with the three closest names; every `--help` carries one example.

Text mode is the default and every verb has an intentional human rendering; JSON is emitted only with `--json` (passthrough exceptions: A20). There is no generic JSON-to-text fallback. Summaries use:

```
<headline: what happened>
  <key>  <value>
  next   <action, when one is needed>
```

The headline comes first. Fact keys are lower case and aligned within the block. Empty optional sections are omitted, but failures, orphaned resources and pending verification remain visible. `status`, `doctor` and `config` summarise rather than restating their JSON payloads. `path` prints only the root and `which` prints only the resolved executable (or `not found`). `list`, `tasks`, `config` and `locks` use aligned columns with a header where it aids reading; `tasks` is the effective task table, `config` shows effective keys with scope and layer, and `locks` is the coordination lock table. Output is plain ASCII apart from optional ANSI colour on existing diagnostic codes.

### 14.2 TTY and bounded-runtime rules (A14)
Control-plane deadlines per §13.3; user children run as long as they run. Idempotent re-run applies to `register`, `unregister`, `clone`, `new`, `adopt`, `sync`, `remove`, `prune`, `open --no-attach`, `close`. stdin not a TTY ⇒ never prompt. Human stdout has the same format when redirected as it has on a terminal; only ANSI colour is omitted according to `--color`. `--json` selects the envelope instead. Every destructive lifecycle verb whose only purpose is destruction (`remove`, `unregister`, `destroy`, `refresh`) prompts on a TTY without `--yes` and requires `--yes` otherwise (`CONFIRM_REQUIRED` 2); a declined prompt exits 0 with `*: false` and mutates nothing. **Exception**: `prune` is a report-then-act verb; without `--yes` on a non-TTY it prints its plan and exits 0 with `data.applied = false` and the notice `CONFIRM_REQUIRED` (§12).

### 14.3 Exit classes and error type
| Code | Class | Examples |
|---|---|---|
| 0 | ok | incl. idempotent no-ops |
| 1 | internal | bug |
| 2 | usage | `CONFIRM_REQUIRED`, `JSON_UNSUPPORTED`, `USE_UNREGISTER`, `NO_GATE_REFUSED` |
| 3 | not found | `NOT_FOUND` |
| 4 | conflict | `NAME_TAKEN`, `BRANCH_IN_USE`, `LOCK_HELD`, `TREE_BUSY`, `TREE_IN_USE`, `SLOTS_EXHAUSTED`, `NAME_SHADOWS_LABEL`, `PATH_REGISTERED`, `GITDIR_REGISTERED`, `GEOMETRY_CONFLICT`, `PORTS_EXHAUSTED`, `IDENTITY_COLLISION`, `TREE_MISSING_PENDING` |
| 5 | state | `TREE_DIRTY`, `CONFIG_INVALID` (+ subcodes), `SETTINGS_INVALID`, `SESSION_DISABLED`, `ENV_UNDEFINED`, `TOOL_MISSING`, `COPY_TRACKED`, `RENDER_ONTO_*`, `PATH_OCCUPIED`, `HOME_OLD_FORMAT`, `*_CORRUPT`, `NOT_A_WORKTREE`, `VERIFY_PENDING`, `ROOT_IS_SYMLINK`, `TREE_REPLACED`, `TREES_EXIST`, `CWD_MISSING`, `FILE_SOURCE_MISSING` |
| 6 | child failed | `SYNC_FAILED`, `TASK_FAILED`, `DESTROY_FAILED`, `VERIFY_FAILED`, `NOT_READY`, `RESOURCE_PROBE_FAILED`, `RESOURCE_ORPHANED`, `TASK_PROBE_FAILED` |
| 7 | external | `GIT_FAILED`, `FETCH_FAILED`, `TMUX_FAILED` |
| 8 | timeout | `TIMEOUT`, `LOCK_TIMEOUT` |

Passthrough doors exit with the child's status (§9.3). `Error::Internal { message }` or `Error::Fail { class, code, message, remedy: String, details }` — a remedy cannot be omitted.

### 14.4 JSON envelope and success schemas
```
Envelope := { wt: {schema: 1, version}, ok, command, data|null, notices: [ {level, code, subject|null, message} ], error|null }
Error    := { class, exit, code, message, remedy, details }
Child    := { code: i32|null, signal: i32|null }
Tree     := { target, label, name, canonical, tree_id, path, slot, geometry: {base, stride, port_base}, phase, branch|null, detached_sha|null,
              dirty: {modified, untracked}|null, upstream: {ahead, behind}|null, behind_default|null,
              sync: {state, at|null, changed: [RelPath], drift: [RelPath]}, verify: {ok, at}|null,
              session: "yes"|"no"|"unknown", session_name, agent|null, resources: [Resource], ports: [ {name, port, bound|null} ], disk_kb|null }
Resource := { scope, task, tied_to, name, state, reason|null, external, undeclared, has_instance, last_probe: {at, result}|null, last_error: {at, event, message}|null }
StepRep  := { id, scope, status, child|null, duration_ms }
```

| Verb | `data` |
|---|---|
| `register` | `{ label, path, gitdir_id, registered, resumed, tree: Tree, declared: { tasks: [ScopedTask], resources: [ {scope, task, tied_to, snapshot_keys: [EnvKey]} ], env: [EnvKey], files: [RelPath], bin: [RelPath], ports: [PortName], copy: [RelPath] }, config_errors: [ {path, line, col, message} ] }` |
| `clone` | `{ url, path, cloned } & register.data` |
| `unregister` | `{ label, unregistered, destroyed: [ {scope, task, state, child|null} ], artifacts: [ {path, action: "deleted"|"kept"} ] }` |
| `new` / `adopt` | `{ tree, created, resumed, sync: StepRep[]|null, verify: {ok, steps: StepRep[]}|null }` / `{ tree, adopted, resumed }` |
| `remove` | `{ target, removed, destroyed: [ {scope, task, state, child|null} ], orphans_kept: [ScopedTask], branch_deleted, session_closed }` |
| `sync` | `{ target, ran, steps: StepRep[], inputs: [ {path, hash} ] }` |
| `run` | `{ target, task, child|null, log|null, steps: StepRep[] }`; `--dry-run`: `{ task, steps: [ {id, scope, origin, cwd, run, exists, lock, sys_locks, resource, tied_to} ] }` |
| `destroy` / `refresh` | `{ target, scope, task, before, after, child|null }` |
| `open` (non-attaching) / `close` | `{ sessions: [ {target, name, created, existing, agent|null, foreground} | {target, name, failed:true, code, message, remedy} ] }` / `{ sessions: [ {target, session, closed} ] }`; only `open --all` contains the failure variant and may return a failure envelope with this data retained |
| `env` | `{ target, set, kept, overrode, restored, missing_bins, rendered, bins: [ {dir, exists, executables} ], env: Map, activation: Activation }` |
| `list` | `{ trees: [Tree], locks: [ {name, label, holder: {pid, target, verb, since}} ] }`; `status` | `Tree & { tasks: [TaskInfo], config_errors }` |
| `path` / `which` | `{ target, path }` / `{ target, cmd, path|null, in_bin }` |
| `tasks` / `config` / `locks` | `{ target, tasks: [ {id, scope, origin, cwd, needs, resource, tied_to|null, lock|null, description|null} ] }` / `{ target, entries: [ {key, scope, layer, value} ] }` (env values shown as keys only) / `{ locks: [ {level, name, path, held, holder|null} ] }` |
| `prune` | `{ applied: bool, items: [ {target, reasons: [String], action, result|null} ] }` |
| `doctor` / `shell-init` / `completions` | `{ findings: [ {severity, code, subject, message, remedy} ], counts: {error, warn, info} }` / `{ shell, script }` |

Redaction: environment values appear only in `env` output; `ResourceSnapshot.env` never appears anywhere.

### 14.5 Stable ordering
| Array | Order |
|---|---|
| `list.trees` | `(label, canonical first, name)` |
| `Tree.resources`, `*.destroyed`, `remove.orphans_kept` | `(tied_to: tree, repo; scope; task)` |
| `Tree.ports` | recorded index (semantic) |
| `Tree.sync.changed`, `Tree.sync.drift`, `sync.inputs`, `register.declared.*`, `env.*` string arrays, `unregister.artifacts`, `env.bins[].executables` | lexical |
| `*.steps`, `tasks.tasks`, `run --dry-run.steps` | plan order (topological, ties `(scope, id)`) / `(scope, id)` |
| `tasks.tasks[].needs`, `prune.items[].reasons`, arrays inside `config.entries[].value` | declaration order (semantic) |
| `notices` / `doctor.findings` | `(level: warn, info; code; subject; message)` / `(severity: error, warn, info; code; subject)` |
| `open.sessions`, `prune.items` / `list.locks`, `locks.locks` / `config.entries` / `*.config_errors` | `(target)` / `(level, name)` / `(key, scope, layer precedence)` / `(path, line, col, message)` |
| any other array | sorted lexically by canonical JSON of its elements |
| maps | sorted keys |

Byte stability is claimed only after normalising the declared nondeterministic fields: `wt.version`, every `at`/`since`/`started`/`recorded_at`/`removed_at`, `duration_ms`, `log`, `pid`, `tree_id`, `disk_kb`, `holder.since`, `last_probe.at`, `last_error.at`.

### 14.6 `shell-init`, `wtcd`, the PATH guard
`wt shell-init <shell>` prints:
```sh
# zsh/bash
wtcd() { local p; p="$(command wt path -- "$@")" || return $?; builtin cd -- "$p"; }
wtsh() { eval "$(command wt env --sh -- "$@")"; }
if [ -n "$WT_BIN" ] && [ "${PATH#"$WT_BIN:"}" = "$PATH" ]; then PATH="$WT_BIN:$PATH"; echo "wt: PATH_NOT_SHADOWED — re-prepended $WT_BIN" >&2; fi
```
```fish
function wtcd; set -l p (command wt path -- $argv); or return $status; cd -- $p; end
function wtsh; command wt env --sh -- $argv | source; end
if set -q WT_BIN
    set -l wtbin (string split : -- $WT_BIN)
    set -l n (count $wtbin)
    if test (count $PATH) -lt $n; or test "$(string join : -- $PATH[1..$n])" != "$(string join : -- $wtbin)"
        set -gx PATH $wtbin $PATH
        echo "wt: PATH_NOT_SHADOWED — re-prepended $WT_BIN" >&2
    end
end
```
Errors from `wt path` propagate; `wt completions <shell>` completes targets from `wt list --json`.

## 15. Crates (A18)
```
crates/wt-core   pure: model, config (grammar/merge/scopes/validate/template), adapters, address, coords, ports, env, render::decide,
                 task (graph/plan/lock_plan), resource::step, declarations::reconcile, lifecycle (derive_phase, new::decide, init::decide,
                 remove::plan/revalidate/classify, from_ref, drift), session::name, doctor, report
crates/wt-sys    effects as plain modules, each public function wrapping one syscall, subprocess or file format:
                 git, fsx (store protocol, no-follow open, reflink, exclude splice), lock (six levels, holders, deadlines), proc (execvp door,
                 `run` parent with tee and timeouts), net (bind+connect probe), tmux, snapshot
crates/wt        binary: cli/ + app/ (door, executor, commands)
```
`wt-core`: no `std::{fs,process,env,net,thread}`, no `SystemTime::now`; dependency allow-list `serde serde_json toml blake3 indexmap thiserror` (clippy disallowed lists + CI manifest check). The binary crate cannot name `std::process::Command` or `std::fs` write/rename/remove (clippy `disallowed_methods`, `-D warnings`) and depends only on `wt-core`, `wt-sys`, `clap`, `serde`, `serde_json`. Long-held locks are fd-owning tokens (`TreeToken`, `GitToken`).

### 15.1 Decision APIs (pure)
`derive_phase(TreeObs)`; `new::decide(EntryView, Request)`; `init::decide`; `render::decide`; `resource::step(Option<Record>, Event)`; `declarations::reconcile(records, declared)`; `remove::classify(GitObs)`, `remove::plan(Obs)`, `remove::revalidate(plan, Obs2)`; `exclude::block`, `exclude::splice`; `deactivate`, `assemble`; `task::plan`, `task::lock_plan`; `session::name(label, name)`; `coords::choose(allocated_ranges, squatted, settings, tombstone)`; `ports::append(map, cfg_ports, stride)`; `drift(diff_names, sync_inputs)`; `settings::validate`.

## 16. Acceptance configs (A9, A15, A22, A31)
All three inputs parse unchanged.
- **orbit**: `register ~/source/orbit` → canonical tree with a slot, `.wt/orbit/config.yaml` rendered (hash-owned, `host_name: <name_short>`), `target/debug` first on PATH inside every door (`BIN_DIR_MISSING` until the first build); `wt new orbit/feat` → same, own ports and config; `wt run daemon` → `build` → instance frozen with `bin_exes ∋ orbit` → `orbit server start` → probe `orbit list` → present; `wt remove orbit/feat` → `orbit server stop` under the tree's own PATH, dropped, tombstone; `rm -rf` then `prune` → `orphaned(exe_missing)`, installed `orbit` never invoked. The canonical daemon runs beside the installed release (A3).
- **orbitapp**: one port (`WT_PORT_METRO` → `RCT_METRO_PORT`); `wt run ios` → `orbit-src` probes `$WT_ROOT/../orbit/crates/orbit`, links the sibling when absent (a side effect outside the tree, untracked), then `npm run ios`; a later `wt new orbitapp/orbit` reports `PATH_OCCUPIED`.
- **orbitcloud**: seven aliases in `Section__Key` form from `WT_PORT_*`; `.mcp.json` and `.claude/settings.local.json` copied at `new` S3 and excluded; custom `sync` replaces the composite; `pgdata` (no `run`) per the §10.5 example — with Docker stopped its probe exits 2 (`docker info … || exit 2`), so `remove` reports `orphaned(probe_failed)` and stops, `remove --keep-orphans` removes the worktree and leaves the record for `prune --records`; with Docker running, `remove` destroys the Aspire containers/volumes/network named from the path hash and drops the record.

## 17. Test plan
Levels **U** (wt-core), **I** (wt-sys/app on temp repos with shims: tmux, docker, npm, cargo, installed-`orbit` recorder, sleeping git per class, probe shell, probe agent, fd-closing child), **C** (CLI contract). Failpoints (feature `failpoints`, A30.3): `WT_FAILPOINT=<name>[:exit|:sigkill|:pause=<ms>]` at exactly: `new.G` (after `worktree add`), `sync.mid` (between two sync nodes), `remove.8` (after a destroy, before `worktree remove`), `render.write` (after the file write, before the record), `resource.destroyed` (after `DestroyOk`, before the record drop), `resource.frozen` (after the instance freeze, before `run`). Tests reference the owning section and assert its stated outcome; they restate no algorithm.

| Area (owner) | Proof | Level |
|---|---|---|
| §0 | a `wt exec` on a ready tree: subprocess tracer shows ≤ 1 git query (plus `ls-files` on the first render only); no bind; no state write when nothing changed; two flocks; §13.3: each subprocess class with a sleeping shim hits its deadline, lock waits bounded, `list` in a non-ready phase never blocks a concurrent verb | I |
| §8.1–8.4 | proptest over marker-free parents (incl. pre-set `WT_*`, PATH variants, pre-set aliases, force, task env, two trees) asserting L1, L2 and "effect ⊆ applied keys"; a corrupted marker → `ACTIVATION_IGNORED` and the door proceeds; a user-edited tool-set key is replaced by the next door; `--deactivate --sh` evaluated restores the parent | U, C |
| §8.3 | edited bytes with header+record → `RENDER_ONTO_USER_FILE`; a tracked path → `RENDER_ONTO_TRACKED` at first render and after `sync`; `render.write` failpoint → next door reports row 5 with the `rm` remedy | I |
| §9.1–9.2 | env identical across `env --dotenv`, `exec -- env`, `run` node, probe shell at spawn, tmux probe agent; door file names the exec'd child's pid; `remove` during `exec -- sleep` → `TREE_IN_USE` naming it; fd-closing child releases the lock (documented residual); `--no-gate` outside `$TMUX` → `NO_GATE_REFUSED`; `BIN_DIR_MISSING` always visible; adversarial `run` children (partial JSON, no newline, 10 MB, invalid UTF-8, signal death) → one envelope; passthrough refusals (§9.3) | I, C |
| §9.4 + §11.4 step 5 | attached `open` then `remove --yes` → session killed, lock acquired, removal completes; a `wt shell` in the tree → `TREE_IN_USE` naming the shell; two concurrent `open`s → one session | I |
| §10.1 | `lock_plan` order asserted by a lock-order tracer (out-of-order acquisition panics in test builds); task env not contributed; present resource env contributed; log retention keeps 20 per task | I |
| §10.3 | snapshot env = exactly the minimised set (never `WT_ACTIVATION`; parent keys only via `snapshot_env`); teardown env = invoker's env overlaid; A25 scans only the recipe about to run; files 0600 under umask 000; repo-tied env has no tree-specific key; orbit daemon after `rm -rf`: `prune` → `orphaned(exe_missing)`, installed `orbit` never invoked; a recipe without tree words runs with bins removed from PATH; canonical root gone → `repo_root_missing` | I |
| §10.4 | pgdata sequence (declared → run notice → external present → refresh → declared → remove drops absent/destroys present); probe exit 2 → never runs/destroys, teardown → orphaned; `resource.frozen` failpoint → next `run` probes and settles; destroy failure → orphaned, others attempted; declaration deleted after creation → still destroyed from the instance; sibling-scope same-named resources distinct | I, C |
| §10.5 | instance frozen after `needs`, later config edit does not change it; no refresh on a plain door; repo-tied: the invoking tree's stripped declaration is used until an instance exists, then the instance governs | I, C |
| §11.1 | `derive_phase` exhaustive over the table incl. `replaced`, `claimed`, `missing` with records | U |
| §11.2 | `new.G` failpoint → `wt new` resumes once (`resumed: true`); two `new` same address → one `TREE_IN_USE`; crash during V → `--verify` resumes V; `remove` then `new` same name → inherits slot/ports/identities with a fresh `tree_id`; `rm -rf` a tree with a present resource then `new` → `TREE_MISSING_PENDING`; after `prune --records` → fresh incarnation; a foreign directory at the path → `PATH_OCCUPIED`; `sync.mid` failpoint → `interrupted`, `wt sync` resumes, unchanged inputs → no-op (§11.3) | I, C |
| §11.4 | `n` / `TREE_DIRTY` / non-TTY without `--yes` leave phase, op, session untouched; clean, missing and replaced trees prompt; `remove.8` failpoint → `remove-interrupted`, re-run completes; probe exit 2 → `DESTROY_FAILED`; `--keep-orphans` removes the worktree and leaves a `missing` entry with records; repo-tied instance survives tree removal | C, I |
| §11.5–11.6 | `unregister` runs the canonical teardown inline, closes the canonical session, one tree-tied pass then the repo-tied pass, stops at the failure barrier; `register` → `list` ready/`sync: never`; `register` → doors before any `new`; interrupted init resumes; duplicate path/gitdir refused; §11.7: `COPY_ABSENT`/`COPY_TRACKED`/`COPY_EXISTS`, copied file never re-rendered/deleted, excluded from `git status` | I, C |
| §7 | disjoint ranges (proptest over geometry incl. `stride 0`/overflow rejection); tombstone ranges avoided; `IDENTITY_COLLISION`; appended port seen by the allocating door's child; reordered `ports` changes nothing; removed name keeps its index; `PORTS_EXHAUSTED`; settings change leaves live `wt env` unchanged | U, I, C |
| §4.1–4.4 | invariants incl. path uniqueness and no live/tombstone coexistence; `register` twice same label → `registered: false`; other label → `PATH_REGISTERED`; store crash at the rename boundary leaves the old file; `HOME_OLD_FORMAT` before any write; `TREE_REPLACED` on every verb for a replaced directory; `STATE_ORPHAN` collected by `prune` | U, I, C |
| §12 | `list` reports `drift` when the default branch changed a sync input; `prune` tombstone collection; `prune --records` on `missing`/`replaced`/`remove-interrupted` never touches the directory (spawn tracer: no cwd or PATH inside it); non-TTY `prune` without `--yes` → exit 0, `applied: false`; `NO_COORDINATION` for a config without ports/env/resources; `close` idempotent JSON | I, C |
| §14.4–14.5 | every verb's `--json` validates; ordering on raw output (a test walks the schema for unlisted arrays); bytes compared after normalisation; §14.6: `wtcd`/`wtsh` and the PATH guard in zsh, bash, fish (fish sourced twice with a two-directory `WT_BIN`: restored once, PATH does not grow) | C |
| R1–R13, A1–A31 | R12/A2: filesystem allowlist snapshot around `new` + doors (only `<tree>`, `<tree>/.wt`, declared materialisations, `$WT_HOME`, `<commondir>/{info/exclude,worktrees/*,refs/wt/*,FETCH_HEAD,objects/*}`), `git status` clean, tracked bytes unchanged, `unregister` leaves the checkout clean or reports `ARTIFACT_KEPT`; each requirement maps to the rows above by its owning section; A9: the three inputs parse byte-for-byte; golden `tasks --json`; orbitcloud recipes run verbatim against the docker shim asserting the path hash and the exit-2 reachability guard | U, I, C |

## 18. Decisions, exclusions, implementer's choices
| Decision | One-line reason |
|---|---|
| Tasks and resources are one table; `destroy` ⇒ resource; worktrees outside the checkout | one grammar, one DAG; nothing to ignore |
| Approved verbs/keys/variables; fresh home; three crates; passthrough doors without envelope; shell promise at spawn; one deadlines table | A15, A16, A18, A20, A19, A14 |
| Activation = prior/applied deltas; restore without comparison | L1–L2 provable; A5's plain reading (A31) |
| Render ownership by content hash inside the state RMW; single-phase | edits preserved; no extra lock (A30.1, A31) |
| Passthrough doors `execvp` with the lock fd inherited; `run` keeps a parent; sessions `--no-gate` | A24; fd-closing children are a residual (A31) |
| `run` inherits stdin but pipes stdout/stderr through its tee parent; TTY stdin stays in the foreground process group | logs and JSON need byte-exact capture; interactive input must not receive `SIGTTIN` |
| State keyed by address; tombstones carry their exclusions; rename-only store | A26 makes predecessors record-free before reincarnation; rename is the crash safety |
| One op record in the state file; liveness from the tree lock | A30 as read by A31 |
| Resources: three states, instance frozen before `run`; probes decide | principle 4; the resource lock serialises (A31) |
| Repo-tied: invoking tree's stripped declaration until an instance exists | A28 as read by A31 |
| Ports as an append-only name→index map | reorder-proof; no reallocation verb (A31) |
| Snapshots: minimised env + explicit `snapshot_env`, bin inventory, fail-safe orphaning | A21, A25, secrets |
| Smallest-free slot + squat probe (bind+connect, v4) | explainable (A31 exception 2) |
| Probes exit ≥ 2 on infrastructure errors; `remove --keep-orphans` | R6 truth when the daemon is down (A31) |
| No adapter fallback to `<tool> $WT_TASK` | typos must not run tools |

Left out: process supervision; port reservation; agent memory migration beyond `copy`; file watching/shell hooks; Windows; tree rename; multiple canonical checkouts per label; daemon/TUI; config trust protocols; merge-back; secrets beyond `copy`; serialising the user's own git/cargo; field-level task merge; state migration (A16); cross-label pairing variables; preventing rc files from altering PATH (A19); defending against hostile renames (A27); `wt unlock`; cross-tree agreement for repo-tied declarations; state-file backups.

Implementer's choices (explicitly free): JSON whitespace (sorted keys, deterministic); temp-file names (same directory, exclusive create); log buffering/header beyond the listed fields (never env values); prompt and notice wording (codes, remedies, plan content, confirmation timing are normative); probe-shell fixture; teardown cwd for missing trees = `$TMPDIR` else `/tmp`. A wt-owned tmux status line is deferred and no setting controls one.

## 19. Consistency check
| Referrer | Owner | Mechanism |
|---|---|---|
| §9.1 D0, §11.3, §11.4 steps 2/6, §12 | §4.1 | identity check and `TREE_REPLACED` remedy |
| §9.1 D4, §11.2 S4, §11.6 R; §11.2 decision table, §12 `prune` | §7 | ports map and append rule; tombstone inheritance and deletion (A26) |
| §8.5, §9.1 D5, §10.5 | §8.1–8.2 | `deactivate`/`assemble` |
| §9.1 D6, §11.2 S4, §11.3, §11.6 I3, §13.1 | §8.3 | rendering inside the state RMW; tracked-check timing |
| §9.4, §11.4 step 5, §13.2, §17 | §9.2 | `execvp` with the inherited fd; `run` parent; `--no-gate` |
| §10.4, §10.5, §11.4 step 8, §12, §16 | §10.3–10.4 | snapshot, `execute` overlay and missing-tree rule; three-state machine and `Destroy{teardown}` |
| §10.1 X2, §11.2 S4, §11.6 I3, §11.4 step 8, §12 (and §9.1 by absence) | §10.5 | when declarations are refreshed; repo-tied effective declaration |
| §12, §13.2, §11.5 | §11.4 | remove sequence incl. missing/replaced paths and `--keep-orphans` |
| §7, §11.2 R, §11.4 step 11, §11.6 R, §12 | §4.3 | state-file rule: written at R, deleted at tombstoning, orphans collected |
| §0 | §9.1, §8.3, §10.5, §12, §13.3 | the door ceiling; mechanisms moved off the hot path; every wait class has a deadline row |
