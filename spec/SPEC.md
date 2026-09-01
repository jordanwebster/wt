# wt — specification
Normative specification for the clean re-implementation of `wt`. Read
with `problem-statement.md` (requirements R1–R13) and
`requirements-addendum.md` (binding decisions A1–A75);
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
| `new` / `sync` / `rm` / `forget` / `unregister` / `register` / `adopt` / `copy` | §11.2–11.7 |
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
9. Scope is a named axis (A56): every fact has one scope — tree, repo, or
   machine — deciding where it is declared, where its state lives, which
   variables its snapshots carry, and what tears it down.

## 2. Concepts
| Concept | Definition | Identity | State owner |
|---|---|---|---|
| Label | registered repository: canonical path + common gitdir | `Label`; `gitdir_id` = blake3(realpath of the common gitdir) | `registry.json` |
| Tree | canonical checkout (name `canonical`, `label` ≡ `label/canonical`) or linked worktree; an address has one live **incarnation** | `label/name`; incarnation `tree_id` (random 128-bit hex) | registry + `state/<label>/<name>.json` |
| Coordinates | slot, frozen geometry `{base, stride, port_base}`, `ports` map | per incarnation; inherited across reincarnation (§7) | registry |
| Derived identity | `name_short`, `session_name` | functions of the address (A74) | derived, never stored |
| Environment | exact map handed to a child + activation metadata | per door | not persisted |
| Door / Task | `run`, `exec`, `shell`, `open`, `env` / recipe resolved at a scope | — / `ScopedTask`, `PrivateId` | — / logs in `<tree>/.wt/logs/` |
| Resource | a task with `destroy`; a refreshed declaration snapshot and a frozen instance snapshot; three states (§10.4) | `ResourceKey` (§10.2) | state files |
| Session | tmux session bound to a tree; intent (`agent`) recorded; liveness from tmux (A24) | `session_name` | tmux |
| Lock | `flock(2)` files, six levels (§13) | path | kernel |

## 3. Names and addressing
### 3.1 Grammars and derived identities
```
Label      := /[A-Za-z0-9][A-Za-z0-9._-]{0,31}/ , not "." or ".."
TreeName   := /[A-Za-z0-9][A-Za-z0-9._-]{0,63}/ , not "." or ".." ; "canonical" reserved
Target     := Label | Label "/" TreeName
TaskId     := /[a-z0-9][a-z0-9._-]{0,63}/ ;  RelDir := "." | RelPath(dir)
ScopedTask := (RelDir "/")? TaskId ; PrivateId := "@" AdapterId "/" ToolId "@" RelDir "/" TaskId
PortName   := /[a-z][a-z0-9_]*/ → {{ports.<name>}} in templates, not exported (A43) ; EnvKey := /[A-Za-z_][A-Za-z0-9_]*/ not /^WT_/ ; VarKey := /[a-z_][a-z0-9_]*/ ; CommandName := basename, no '/' or NUL
LockName   := /[a-z0-9][a-z0-9._-]{0,63}/ ; Duration := /[0-9]+(ms|s|m|h)/ ; TreeId := /[0-9a-f]{32}/
ScopeEnc   := RelDir with "/" → "%2F", "." for root
```
- `name_snake(s)`: lower-case; runs of `[^a-z0-9]` → `_`; trim `_`; `x` if empty. It is a template derivation, not an environment export.
- `name_short`: `name_snake(label)_name_snake(name)` truncated to 22 + `_` + first 8 hex of blake3 of the untruncated string (≤ 31, `[a-z0-9_]`). `session_name` is the target's display form (`label/name`, canonical `label`) with `.` and `:` each mapped to `_`; it has no prefix, truncation, or hash suffix (A67).
- Both are functions of the address and neither is stored (A74). They are derived wherever they are used, and at allocation each is checked against every other address in `trees ∪ tombstones` (collision → `IDENTITY_COLLISION` (4), remedy "choose another tree name").

### 3.2 Address resolution
`Address := "." | AbsPath | Target | TreeName`. Bare `x`: (1) cwd inside a live tree of label `L` and `L/x` live → `L/x`; (2) `x` is a label → `x/canonical`; (3) `NOT_FOUND` (3) whose remedy lists `L/x` candidates. No cross-label inference. `wt new` refuses a name equal to a label (`NAME_SHADOWS_LABEL` 4). cwd resolution is longest-prefix over canonicalised live tree roots (unique because paths are unique, §4.1). Omitted `[target]` ⇒ `.`.

## 4. On-disk layout
```
$WT_HOME/                                 --home > WT_HOME env > ~/.wt; exported as WT_HOME in doors; 0700
  config.toml
  registry.json, registry.lock            level-5 RMW lock
  state/_machine.json                     machine-tied resource records (0600)
  state/<label>/_repo.json                repo-tied resource records (0600)
  state/<label>/<name>.json               tree state (0600); exists exactly while the address has a live entry
  locks/<label>/<name>.lock               level-1 tree lock (shared: doors; exclusive: lifecycle verbs); path depends only on the address
  locks/<label>/<name>.doors/<pid>.lock   door-holder record {pid, verb, since}; try-flock = liveness
  locks/git/<gitdir_id>.lock              level-2 repo-git
  locks/<label>/res/tree/<name>/<ScopeEnc>/<task>.lock ; locks/<label>/res/repo/<ScopeEnc>/<task>.lock   level-3
  locks/_machine/res/<ScopeEnc>/<task>.lock   machine resource lock, level-3
  locks/<label>/named/<lockname>/<i>.lock   level-4 named-lock slot; an old flat <lockname>.lock is inert
  locks/<label>/<name>.rmw.lock, locks/<label>/_repo.rmw.lock, locks/_machine.rmw.lock   level-6
  trees/<label>/<name>/                   worktrees (settings.trees_dir overrides)
  cache/cargo-build/<label>/<name_short>/ per-tree build intermediates (adapter-keyed, §6.1); deleted with the tree, orphans reaped by prune
<tree>/.wt/                               tool-owned, excluded, never authoritative; holds tree_id, logs, rendered files
<commondir>/info/exclude                  managed block (§4.2)
```
Files under `state/` are created 0600 and directories 0700 by explicit modes, independent of umask.

### 4.1 `registry.json` (schema 1), invariants, identity check
```
Registry  := { schema: 1, labels: Map<Label, LabelRec>, trees: [TreeRec], tombstones: [Tombstone] }
LabelRec  := { path (canonicalised), gitdir_id, common_gitdir, registered_at, trees_dir|null, default_branch|null }
TreeRec   := { tree_id, label, name, canonical, path (canonicalised), slot, geometry: {base, stride, port_base},
               ports: Map<PortName, u8>, created_at, agent|null,
               meta: Map<VarKey, String>, source: { kind, branch|null, pr|null, start|null } }
Tombstone := { label, name, slot, geometry, ports, path, materialized: [RelPath], removed_at, reason }
```
Invariants (load-time; violation → `REGISTRY_CORRUPT` 5): slots unique and port ranges pairwise disjoint across `trees ∪ tombstones`; `(label,name)` unique across `trees ∪ tombstones` — an address is either live or tombstoned, never both; `tree_id` unique; **path uniqueness** holds over (label paths) ∪ (non-canonical tree paths) — the canonical tree shares its label's path; `gitdir_id` unique across labels; one canonical tree per label; the `name_short` and `session_name` derived from each address (§3.1) unique across distinct addresses. `register` of a path already registered under the **same** label with identical arguments is idempotent (`registered: false`, exit 0, §11.6); `PATH_REGISTERED` (4) fires only when the path is registered under a different label, or when `--label` names an existing label bound to another path; `register`/`adopt` of a path whose common gitdir equals a registered label's → `GITDIR_REGISTERED` (4), remedy "use `wt adopt <path> --label L`"; `adopt` of a worktree of label `L` forces `--label L`.

`TreeRec.meta` is an opaque sorted string map. Keys match
`[a-z_][a-z0-9_]*`; values are at most 1024 bytes. Both are validated before
a write. Metadata is data only: templates cannot read it and no wt behaviour
depends on it (A63).

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
Config  := Scope & { ports?: [PortName], locks?: Map<LockName, LockCfg> ★, dirs?: Map<RelDir, Scope>, sync_inputs?: [RelPath] ★, detect?: Detect ★ }
Scope   := { bin?: [RelPath], commands?: [CommandName] | Map<CommandName,bool> ★, vars?: Map<VarKey, Template|false> ★,
             env?: Map<EnvKey, Template|false>, copy?: [RelPath], files?: Map<RelPath, File|false>,
             task?: Map<TaskId, Task|false>, adapters?: Map<AdapterId, { tool?: ToolId, disabled?: bool }> }
Detect  := { depth?: 0|1|2 (1), ignore?: [RelPath] }
LockCfg := { slots?: u16 (1, minimum 1), wait?: Duration }
File    := { content?: Template, source?: RelPath, template?: bool (true), marker?: String ("#"; "" = no header), mode?: OctalString ★ ("0644") }
Task    := { run?: Cmd, exists?: Cmd, destroy?: Cmd, template?: bool (true), needs?: [ScopedTask|PrivateId], lock?: LockName, name?: Template,
             tied_to?: "tree"|"repo"|"machine", exclusive?: "repo"|"machine", env?: Map<EnvKey, Template>, cwd?: RelPath, timeout?: Duration, description?: String,
             ready_within?: Duration ★, snapshot_env?: [EnvKey] ★ }
Cmd     := String | [String]          // shell: whole string templated, then `sh -c`; argv: each element templated; neither when the task sets template = false
Template:= String                      // {{name}} reads a var, {{ports.n}} a declared port, {{fn()}} calls; no interior whitespace
```
Every task declares at least one of `run`, `destroy`, or `needs`. A task with
`needs` and neither `run` nor `destroy` is an aggregate and accepts only
`needs` and `description`; any other task key would guard nothing and belongs
on a task that runs (A57).
`exclusive` is valid only on a tree-tied resource: the task must declare
`destroy`, `exists`, and `tied_to = "tree"`; any other use is
`CONFIG_INVALID` naming this rule (A60).
`false` deletes an inherited map entry; in the map form of `commands`, `true`
claims a name and `false` deletes an inherited claim. The array form remains the
compact declaration form. The three acceptance files (§16) are restated in
this grammar; their behaviour is unchanged.

**Claims and privacy (A40, A41, A44).** `commands`, `env`, `ports` and `files`
declare names the tree owns and are enforced per §9.2; `vars` are private and
never exported. `CommandName` is a non-empty basename with no `/` or NUL;
`wt`, `.` and `..` are reserved.
`VarKey` matches `[a-z_][a-z0-9_]*` and may not collide with a function name.
Template functions are exactly `home()`, `root()`, `repo()`, `branch()`, `label()`,
`name()`, `name_snake()`, `name_short()` and `target()`. `home()` returns the
resolved wt home. A declared port is a
dotted constant, `ports.<name>` — a lookup, not an allocation: the value is
fixed when the name is first declared and repeats identically. `ports` is a
reserved `VarKey`. Any other call, or `ports.<name>` for a name absent from
`ports`, is `CONFIG_INVALID` naming the offending reference. `vars` resolve as a DAG within a scope — file order is not
significant — and a cycle or unknown name is `CONFIG_INVALID` naming every key
on the cycle. `{{` always begins a template expression. The expression must
close with `}}`, contain no whitespace, and name a declared `vars` key, a
declared port, or a permitted function; malformed and unknown expressions are
`CONFIG_INVALID` at their source location (unknown dependencies within the
`vars` DAG retain the `VARS_UNKNOWN` rule below). `env`, files whose `template` is
true, task `name`, the recipes of tasks whose `template` is true, and `vars` may
read `vars` and call functions.
A shell-string recipe is templated before `sh -c`; substitutions are inserted
verbatim, so path-valued substitutions must be quoted or expressed with argv.
Each argv element is templated independently. `$` has no meaning to wt:
`$HOME`, `${h%??}`, `${root()}`, and `$$` all pass through literally for the
shell or rendered file to interpret. A file with `template = false` passes its
content or source text through verbatim while retaining the ordinary rendering,
hash-ownership, marker, and mode rules. A task with `template = false` likewise
passes its `run`, `exists`, and `destroy` through verbatim in either command
form, while its `name` and `env` stay templated (A66); such a recipe reads wt's
values from the environment, including `$WT_SELF`. The repo- and machine-tied
`$WT_*` refusals apply to an untemplated recipe unchanged, since those names
still reach the shell. Literal `{{` is unsupported in every other template
string. `ALIAS_REFERENCES_ALIAS`
is withdrawn: composition happens in `vars`.

### 5.3 Directory scopes (A7)
Scopes are declared by `[dirs."d"]` (layers 1–3) or detected (§6). The scope chain for cwd `c` is `c`'s relative dir and its parents up to root, nearest first, keeping declared/detected scopes; for `d/t` it starts at `d`.

| Key | Rule |
|---|---|
| `task` | nearest scope wins by `TaskId`; within a scope tree > user > repo > adapter; default `cwd` = scope dir; explicit `cwd` is root-relative |
| `env`, `vars`, `files`, `copy` | accumulate root-first; nearer scope overrides by key/path; same layer precedence within a scope |
| `bin` | concatenated root-first then nearer, deduplicated, nearer first on PATH |
| `commands` | union of claims across scopes, deduplicated and sorted; within one scope, higher-layer `false` deletes a lower-layer claim |
| `adapters` | per scope, merged by id across layers |
| `ports`, `locks`, `sync_inputs`, `detect` | root only; `locks` merges by `LockName` through the ordinary layer precedence |

A resource's scope is the scope at which its effective task was declared.

### 5.4 Settings and geometry
```
Settings := { schema?: 1, trees_dir?, editor?: Cmd, agents?: Map<String, { start: Cmd, resume: Cmd }>,
              ports?: { base?: u16 (20000), stride?: u8 (16) }, git?: { timeouts?: GitTimeouts }, task?: TaskDefaults,
              locks?: LockWaits, session?: { backend?: "tmux"|"none", attach?: bool (true), agent?: String|null,
                                            tmux_timeout?: Duration ("10s") },
              logs?: { keep?: u16 (20), trace?: bool (true) } ★, shell?: { program?: AbsPath }, repos?: Map<Label, Config> }
```
Validation at load (`u32` arithmetic) else `SETTINGS_INVALID` (5): `1024 ≤ base ≤ 65535`; `1 ≤ stride ≤ 255`; `max_slots = (65536 − base)/stride ≥ 1`; `session.agent`, when set, names a declared agent; `editor`, when set, is a non-empty templated `Cmd`. An unknown top-level settings key that is a valid repo-scope `Config` key is reported as wt's own error rather than serde's: the message names the misplaced entry as a repo-scope key rather than a settings key, and the remedy puts it under `[repos.<label>]` (A73). A configuration carrying the former top-level agent key is refused by the ordinary unknown-key rule, with no bespoke check naming its replacement (A51). At `register`, or on the first `new`, `open`, or `close` in a home that predates this setting, an absent `session.backend` is resolved once: wt checks for tmux ≥ 3.2, writes `"tmux"` when found and `"none"` otherwise, and prints `sessions: tmux <version> (set session.backend to change)` or its `none` equivalent on stderr. A non-table `session` declaration that cannot be extended without rewriting is refused with a remedy to rewrite it as `[session]`. No command detects a session backend again after the key is written. `doctor` reports the effective backend as `SESSION_BACKEND` (info). Geometry is per incarnation and immutable (§7); `assemble` uses `TreeRec.geometry`; settings changes affect only future allocations; doctor `GEOMETRY_CHANGED` (info). Per tree `ports.len() ≤ geometry.stride` else `CONFIG_INVALID`. Built-in agents: `claude` (`claude --name {{name()}}` / `claude --resume {{name()}}`), `codex` (`codex` / `codex resume --last`).

### 5.5 Tool variables (A15, A70)
The stable **interface** tier is `WT_TARGET`, `WT_LABEL`, `WT_NAME`, `WT_ROOT`, `WT_REPO` (canonical path), `WT_HOME` (resolved home), and `WT_BRANCH`, joined inside a resource task by `WT_SELF`, the resource's resolved name, which A66's untemplated recipes depend on. The **mechanism** tier, which may change without compatibility notice, is `WT_ACTIVATION`, `WT_PATH_PREFIX` (the exact shim-plus-bin prefix), `WT_BIN`, and `WT_TASK`. `WT_SESSION`, `WT_NAME_SNAKE`, `WT_NAME_SHORT`, and `WT_SLOT` are not exported (A70); the name derivations remain template functions. `WT_PORT_BASE` and `WT_PORT_<NAME>` are likewise not exported (A43): ports are configuration inputs reached by `{{ports.<name>}}` (§5.2) and reported by `status`, `list` and `env`. `WT_BRANCH` is the HEAD branch **at spawn** (empty if detached), read by one bounded `git symbolic-ref --short -q HEAD`; it is not updated while a shell or session lives (A31). All except `WT_ACTIVATION` are ordinary variables with a recorded prior (§8.1). **Tree-specific keys** (used by §10.3): `WT_ROOT`, `WT_TARGET`, `WT_NAME`, `WT_BRANCH`, `WT_BIN`, `WT_PATH_PREFIX`, `PATH`.

### 5.6 Validation: static vs late-bound
| Check | When | Error |
|---|---|---|
| grammar, unknown keys, identifiers, lexical paths (§5.7), durations, modes, template syntax in every enabled Template and every Cmd element; every `{{…}}` names a declared `vars` key or a permitted function with a declared port argument (§5.2); `destroy ⇒ exists ∧ tied_to`; `ready_within ⇒ exists`; `run ∨ destroy ∨ needs`; an aggregate carries only `needs` and `description`; one of `content`/`source`; port names unique; `ports.len() ≤ stride`; `commands` entries unique and basenames; a `copy` entry that is also a `files` key | parse/validate | `CONFIG_INVALID` (5) with `path:line:col` |
| `vars` DAG acyclic and fully resolvable within the effective scope → `VARS_CYCLE` / `VARS_UNKNOWN` naming every key involved; an unknown `{{NAME}}` elsewhere → `CONFIG_INVALID`; `{{NAME}}` in a task `env` map naming another key of the same map → `TASK_ENV_SELF_REFERENCE` | resolve | `CONFIG_INVALID` |
| `needs` resolvable/acyclic; `tied_to = repo` templates and recipes reference no tree-specific key (§5.5); `tied_to = machine` templates and recipes additionally reference neither `WT_LABEL`/`WT_REPO` nor `label()`/`repo()` | resolve | `CONFIG_INVALID` |
| `bin`/`cwd` existence, `source` readability | door (`bin`: doctor, A50) | `BIN_DIR_MISSING` (doctor finding), `CWD_MISSING` (5), `FILE_SOURCE_MISSING` (5) |

### 5.7 Path containment and no-follow I/O
Lexical: non-empty, no leading `/`, no `..`, no `.` component (except the whole `"."`), no NUL; normalised. The tree root is canonicalised once at registration (`ROOT_IS_SYMLINK` 5 if a symlink). Writes into a tree: parent directories created with `mkdir -p` semantics; the target inspected with `fstatat(AT_SYMLINK_NOFOLLOW)` and opened `O_NOFOLLOW` (a symlink target → `RENDER_ONTO_SYMLINK`/`COPY_EXISTS` per caller). Render: `tmp` beside the target (`O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW`) → write → `fsync` → `rename`. `copy`: source walked with `fstatat(AT_SYMLINK_NOFOLLOW)`, regular files copied byte-for-byte, symlinks recreated, and directories created. `bin`: lexical join (PATH semantics). `cwd`: lexical containment; `chdir` follows symlinks (documented). `source`: the same rules from the canonical root.

## 6. Adapters
### 6.1 Tables
```
Adapter := { name, detect: [Glob], default_tool, nudge?: [ { if_tool?, want, hint, used_if_env? } ], tools: Map<ToolId, Tool> }
Tool    := { lockfile?: [FileName], sniff?: [ { file, toml_key?, contains? } ], requires?: Binary, sync_inputs?: [FileName], env?, task? }
```
Selection per scanned dir: user/repo `adapters.<id>.tool` > lockfile > sniff > `default_tool`; first adapter in fixed order (`cargo, node, dotnet, python, go`) wins per dir; `submodules` is root-only and independent. Detection is pure over a `DirSnapshot` (names at depth ≤ `detect.depth`; contents of `package.json` and files named by `sniff`). Default ignore: `.git .wt node_modules target bin obj dist build .venv vendor .next .expo` and dotdirs.

| Adapter/tool | Selected by | sync | build | test | lint | fmt | sync inputs |
|---|---|---|---|---|---|---|---|
| cargo/cargo | `Cargo.toml` | `cargo fetch` | `cargo build --all-targets` | `if [ "$#" -gt 0 ]; then cargo test "$@"; else cargo test; fi` | `cargo clippy --all-targets -- -D warnings` | `cargo fmt` | `Cargo.lock`, `Cargo.toml` |
| cargo/cargo-nightly-fmt | `rustfmt.toml`/`.rustfmt.toml` parsed as TOML with top-level `unstable_features`/`group_imports`/`imports_granularity` | same | same | same | same | `cargo +nightly fmt` | same |
| node/npm | `package-lock.json`/`npm-shrinkwrap.json`; default with `NO_LOCKFILE` (then `npm install`) | `npm ci` | `npm run build`† | `if [ "$#" -gt 0 ]; then npm test -- "$@"; else npm test; fi` | `npm run lint`† | `npm run format`† | lockfile, `package.json` |
| node/pnpm | `pnpm-lock.yaml` | `pnpm install --frozen-lockfile` | † | `if [ "$#" -gt 0 ]; then pnpm test "$@"; else pnpm test; fi`† | † | † | idem |
| node/yarn | `yarn.lock` | `yarn install --immutable` (`.yarnrc.yml`) / `--frozen-lockfile` | † | `if [ "$#" -gt 0 ]; then yarn test "$@"; else yarn test; fi`† | † | † | idem |
| node/bun | `bun.lock(b)` | `bun install --frozen-lockfile` | † | `if [ "$#" -gt 0 ]; then bun test "$@"; else bun test; fi`† | † | † | idem |
| dotnet/dotnet | `*.sln`, `*.slnx`, `*.csproj`, `*.fsproj` | `dotnet restore` | `dotnet build --no-restore` | `if [ "$#" -gt 0 ]; then dotnet test "$@"; else dotnet test; fi` | `dotnet format --verify-no-changes` | `dotnet format` | `*.csproj`, `packages.lock.json`, `Directory.Packages.props` |
| python/uv | `uv.lock` | `uv sync --frozen` | `uv build` | `if [ "$#" -gt 0 ]; then uv run pytest "$@"; else uv run pytest; fi` | `uv run ruff check .` | `uv run ruff format .` | `uv.lock`, `pyproject.toml` |
| python/poetry | `poetry.lock` | `poetry install` | `poetry build` | `if [ "$#" -gt 0 ]; then poetry run pytest "$@"; else poetry run pytest; fi` | `poetry run ruff check .` | `poetry run ruff format .` | `poetry.lock`, `pyproject.toml` |
| python/pip | `requirements.txt`/`setup.py`/`pyproject.toml` without lockfile | venv + `pip install -r requirements.txt` (or `-e .`) | — | `if [ "$#" -gt 0 ]; then .venv/bin/pytest "$@"; else .venv/bin/pytest; fi` | `.venv/bin/ruff check .` | `.venv/bin/ruff format .` | `requirements*.txt`, `pyproject.toml` |
| go/go | `go.mod` | `go mod download` | `go build ./...` | `go test ./...` | `go vet ./...` | `gofmt -l -w .` | `go.sum`, `go.mod` |
| submodules | `.gitmodules` | `git submodule update --init --recursive` (`sys_locks: [RepoGit]`, git class `submodule`) | — | — | — | — | `.gitmodules` |

† only when `package.json` declares the script. Nudges: `node: if npm → pnpm`; `python: if pip|poetry → uv`; `cargo: sccache, used_if_env = ["RUSTC_WRAPPER=sccache"], used_if_file = [{~/.cargo/config.toml, build.rustc-wrapper ∋ sccache}]`; doctor evaluates `used_if_env` against the effective door env and each `used_if_file` rule (a sniff over a machine config file, `~/` via HOME, dotted `toml_key` walk, `contains` against the value) against the file's content → `ACCELERATOR_INACTIVE` (warn) / `ACCELERATOR_AVAILABLE` / `ACCELERATOR_MISSING` (info); never applied (R7). R7 mechanisms applied: worktree object sharing and `wt list --disk`. **Adapters own per-ecosystem cheapness (A46, A53)** and express it as ordinary layer-0 configuration. Both cargo tools set `CARGO_BUILD_BUILD_DIR = "{{home()}}/cache/cargo-build/{{label()}}/{{name_short()}}"`, one build directory per tree grouped under the repository, while leaving `CARGO_TARGET_DIR` unset so each tree owns its binaries under its own `target/` (§9.2a). The directory is per-tree, never shared between trees (A64): Cargo's unit hashes omit the workspace path, so two checkouts of one workspace would write the same slots and corrupt each other's build-script output and mtime freshness; cross-tree reuse belongs to content-addressed layers (the sccache nudge, `CARGO_HOME`). `remove` deletes the directory with the tree (§11.4 step 11) and `prune` reaps orphans (§12). pnpm uses its content-addressed store and uv its global cache. Repository and user `env` layers override the cargo value by the ordinary merge rule, and the door reports inherited values it displaces. An adapter *may* contribute `commands`, and the merge and delete rules treat such a contribution like any other layer-0 key; no built-in adapter declares any today, because the names live in a manifest rather than in the static catalog, so a repository declares its own. `wt` is never a legal claim (§5.6): a name that refuses until built would make `wt build` unrunnable.

### 6.2 Composition and private ids
Every adapter hit at scope `d` contributes private nodes `@<adapter>/<tool>@<d>/<k>` (`cwd = d`, origin `adapter`), never overridden by layers, addressable by `needs` and `wt tasks --private`. Public: at a non-root scope `d`, `d/k` is the layer task if declared there, else an alias of the private node; at root, `k` is the layer task if declared at root, else the composite `{ needs: [@submodules/git@./k?, @<root adapter>@./k?, d1/k, d2/k …] }` over sorted scopes with an effective `d/k`; an empty composite does not exist. A user-declared needs-only task is the same composite node shape, retaining its layer origin (A57). `verify` = `test` else `build` else absent (`NO_VERIFY`). orbitcloud before the repo layer: `sync` = composite over `@dotnet/dotnet@./sync`, `frontend/sync`, `website/sync`; after `[task.sync]`, `sync` is the repo task and all others remain addressable.

## 7. Coordinates: allocation, inheritance, ports
Allocation happens inside the reserving registry transaction of `new`/`register`/`adopt` (§11):

| Case | Rule |
|---|---|
| a tombstone exists for this address | **inherit** the tombstone's slot, geometry, and `ports` — the allocated coordinates, the derived identities being functions of the address (A74); delete the tombstone in the same registry transaction, in `adopt` as in `new`; the new incarnation gets a fresh `tree_id`. A tombstone's session, if tmux still reports one, is left to `open` (which attaches to it) — no check is made |
| otherwise | slot = the smallest slot in `0..max_slots` not held by any live tree or tombstone and not squatted; `port_base = base + slot·stride` from current settings; reject a candidate whose range overlaps any persisted range (`GEOMETRY_CONFLICT`, next slot); derive `name_short`/`session_name` (§3.1) → `IDENTITY_COLLISION` on collision; persist the allocated coordinates |

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
EnvInputs  := { cfg: EffectiveScope, tree: TreeIdentity (registry entry incl. geometry and ports, with `name_short` derived), home,
                contributed: [(ResourceKey, EnvMap)]   // ONLY resources in state `present`, sorted by key; values literal
                task: Option<TaskContext>, parent, dirs: Fn(&Path)->bool }
EnvOutput  := { env, activation, activation_json, render: [Render], report }
1. { clean, prior, dreport } = deactivate(parent)
2. env = clean; applied = {}; prior_map = {};  set(k, v): prior_map[k] = clean.get(k); applied[k] = v; env[k] = v
3. interface tool vars (§5.5 except the task-only keys) and non-task mechanism values except WT_BIN, WT_PATH_PREFIX, WT_ACTIVATION: set(k, v)
4. PATH: abs = cfg.bin (scope chain) joined to root; missing → report.missing_bins; shims = root/.wt/shims when cfg.commands is non-empty (§9.2a), else absent; prefix = shims ++ abs; set(PATH, join(prefix ++ split(clean.PATH))); set(WT_BIN, join(abs)); set(WT_PATH_PREFIX, join(prefix))
5. contributed: for (k, v) sorted: alias_rule(k, v)
6. vars: resolve cfg.vars as a DAG over the function set (§5.2); the result is **not** written to env. ctx = vars ∪ tool ∪ applied-contributed ∪ clean (frozen once); aliases: for (k, tpl) in cfg.env sorted: alias_rule(k, expand(tpl, ctx))
   alias_rule(k, v): set(k, v); report.(overrode|set) += k   // A42: a declared value always wins; the prior is recorded and restored on deactivation
7. task door only: set(WT_TASK, id); if resource: set(WT_SELF, expand(name, env)); for (k, tpl) in task.env sorted: set(k, expand(tpl, env))   // ends with this node
8. activation = { v:1, target, home, applied, prior: prior_map }; activation_json = canonical JSON (sorted keys, compact); env[WT_ACTIVATION] = activation_json   // not via set()
9. files: for (path, f) sorted: body = if f.template then expand(text, env) else text; render += Render{ path, content: body, mode, header }
10. report = { set, overrode, missing_bins, restored: dreport.restored }
```
Every assignment is owned (in `applied`), including task env and contributed env (the A5 exception: they are tool-set and replaced by nested doors). `report.kept` and `force_env` are withdrawn (A42): a declared alias is a claim, so it always wins, and `overrode` names every inherited value it displaced. `wt env` shows both the applied and the prior value for an overridden key so the displacement is visible rather than silent.

### 8.3 Rendering and ownership (A30.1, A31)
Tree state records `materialized: [ { path, kind: rendered|copied, hash|null, tracked_checked_at } ]`. For `files` with `template = true`, `new_bytes` are the expanded content or source; with `template = false`, they are the verbatim content or source, in both cases with the configured marker header when enabled. Rendering runs **inside the tree-state RMW hold** (level 6): observe → decide → write → record, with no subprocess inside the hold. The tracked check (`git ls-files --error-unmatch -- <paths>`, one call) runs **before** the hold, only for paths without a record and at every `new`/`register`/`adopt`/`sync`; doors otherwise trust the record. Decision `render::decide(observed, record, new_bytes)`:

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
`wt env [target] [--sh|--dotenv|--json] [--deactivate]`. Text: the full env, then the bin inventory (each declared dir, exists?, executables). `--sh`: `export K='v'` lines (`'\''` escaping) plus `unset K` for restored keys whose prior was null; `--dotenv`: `KEY=value`; `--json`: §14.4. `--deactivate --sh`: the actions of §8.1 as shell lines — first `unset WT_ACTIVATION`, then in lexical order one `export K='prior'`/`unset K` per key in `report.restored`. These are the only verbs that print environment values.

## 9. Doors
### 9.1 Door algorithm
Used by `exec`, `edit`, `run` (per plan, §10.1), `shell`, `open` and `env`. The door-equivalent steps inside `new`/`sync`/`register`/`adopt` run under that verb's exclusive tree lock and skip D0–D2.

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

cwd: caller's cwd if inside the tree, else the tree root (`run`: node cwd; `edit`: tree root always). A door emits no `BIN_DIR_MISSING` notice and no command summary carries a `next` guidance line (A50; `doctor`'s per-finding remedy line is unaffected): §9.2's shims remove the hazard the notice reported, and a missing declared `bin` directory is a `doctor` finding. Notices go to stderr on a TTY or `--verbose`, and always to `notices[]`. Each producer contributes its notice set once; renderers do not hide duplicate production. Port-bound findings are reported by `ls`/`status`/`doctor` (§12), never by a door (§0).

### 9.2 Spawn: `execvp`, the `run` parent, `--no-gate`
**Passthrough doors** (`exec`, `edit`, `shell`): after D6 the wt process clears `FD_CLOEXEC` on the tree-lock and door-file fds and `execvp`s the child with the assembled env; the pid is unchanged, so the door file names the running process. `edit` resolves its command as settings `editor` → `$VISUAL` → `$EDITOR`; only the settings command is templated, environment commands are used verbatim in shell form, and absence is `EDITOR_UNSET` (5) whose remedy names `editor`. `flock` is held by the open file description and survives `exec`; a child that closes every inherited fd releases the lock early — an accepted residual (A31; alongside A23/A27). Shells keep inherited fds, so `wt rm` of a tree someone is sitting in reports `TREE_IN_USE` naming the shell (A31 exception 1).

**`run` nodes** keep a wt parent for the child's lifetime: it holds the lock fds (`FD_CLOEXEC` set, not inherited), spawns the child with inherited stdio (`--json`: stdout captured, §9.3), tees output to the log, enforces `timeout`, waits, and exits with the child's code or `128+n` on signal death.

**`wt exec --no-gate <target> -- <cmd>`** (A24, A31): honoured only when `$TMUX` is set (otherwise `NO_GATE_REFUSED` (2), remedy "sessions are started by `wt open`"); performs D0–D6, removes its door file, closes the lock fd, and `execvp`s the child. The session therefore holds no wt lock; tmux is its liveness truth. Nested one-shot doors started by the agent take their own shared lock.

### 9.2a Owned command names (A41)
Each name in the effective `commands` is materialised at D6 as `<root>/.wt/shims/<name>`, a symlink to the absolute path of the wt binary that rendered it. The shims directory is created only when `commands` is non-empty, is never a declared `bin` directory (so it contributes nothing to `bin_exes`, §10.4/A25), and — like the tree's `bin` directories — is absent from PATH during teardown of a missing tree. `doctor` reports and repairs a shim whose target no longer exists (`SHIM_BROKEN`) and a name shadowed by a shell alias or function (`SHIM_SHADOWED`, info), which PATH cannot displace.

Invoked through a shim, wt takes a fast path that reads no configuration and acquires no lock. A bare `wt` name always remains the ordinary CLI. For another bare name, wt scans PATH in order for the first absolute `<root>/.wt/shims/<name>` that is a direct symlink to the running binary; a path-bearing `argv[0]` must itself have that shape. If no matching shim exists, wt fails closed with `SHIM_INVOCATION_INVALID` and tells the user to restore the door PATH prefix. From the trusted shim it derives `<root>`, then searches the tree's declared `bin` directories in order for an executable of that name.

| Found | Action |
|---|---|
| yes | `execv` it with the original argv and environment; wt is replaced |
| no, and the recorded build status is `running` and its supervisor pid is live (§11.2) | exit 5, `COMMAND_NOT_BUILT`, with the ordinary remedy below plus the active build's log |
| no | exit 5, `COMMAND_NOT_BUILT`, message naming the tree, the declared `bin` directories searched, `wt build <target>`, and either the installed copy's absolute path or that none was found; terminal build records do not replace this remedy |

A shim never falls through to the rest of PATH: interposing on every invocation is required because shells cache a resolved command path and re-resolve only when it fails, so a shim ordered after the `bin` directories would keep answering after a successful build. The refusal writes to stderr and never to a `run` child's stdout (§9.3).

### 9.3 Machine protocol per door (A20)
| Door | `--json` | stdout / stderr | exit status |
|---|---|---|---|
| `exec`, `edit`, `shell` | refused: `JSON_UNSUPPORTED` (2), remedy "passthrough doors have no envelope: use `wt env --json` or `wt run --json`" | child's | child's |
| tmux `open`, `open --all`, `close` | supported; JSON suppresses attachment | one envelope / notices | classes |
| backend-none `open` | entering is refused like `shell` under `--json`; `--no-attach` supports JSON | shell child's, or one no-op envelope | child's, or 0 |
| `run <task>` text | — | child stdio tee'd to the log and inherited; notices on stderr | child's once started; signal `n` → `128+n` |
| `run <task> --json` | supported | stdout = exactly one envelope; child stdout+stderr merged to stderr and the log | classes (0 / 6 `TASK_FAILED` with `error.details.child` / 8) |
| `env` | supported | envelope or export lines | classes |

Transport (A19): `exec` and `run` children receive the assembled env; `shell` execs `settings.shell.program` (default `$SHELL`, else `/bin/sh`) interactive, non-login, with the assembled env — the promise is at spawn; rc files may alter PATH; the pre-spawn banner names `WT_BIN`; `wt doctor` inside reports `PATH_NOT_SHADOWED` (remedy: the `shell-init` guard, §14.6, or the rc file).

### 9.4 Sessions
`wt open [target] [--agent X] [--no-attach] [--all]`, `wt close [target|--all]`. `session.backend = "tmux"` declares tmux as the backend; commands do not probe its version after registration.

| Situation | Behaviour |
|---|---|
| session exists (`has-session`) | release the tree lock (D2 fd and door file) as soon as `has-session` answers; never start or resume an agent; then attach when the attachment predicate below holds. Attach is a tmux client and holds no wt lock. |
| session absent, agent selected by `--agent X` or `session.agent` | `tmux new-session -d -s <session_name> -c <root> -e WT_HOME=<resolved home> -- wt exec --no-gate <target> -- sh -c '"$@"; s=$?; case "$s" in 126\|127) exit "$s";; esac; exec <shell-program>' wt-agent <agent start>` (the inner door assembles the rest of the environment); `<shell-program>` is the configured absolute `shell.program`, or `"${SHELL:-/bin/sh}"` evaluated when the agent exits, and receives the same interactive arguments as `wt shell`; record the agent only after the startup observation window passes. `session.agent` does not select an agent for a canonical tree, though explicit `--agent X` does. A concurrent creator wins without causing a second start. |
| session absent, tree has a recorded agent and no explicit override | create through the recorded agent's `resume` recipe with the same agent-to-shell wrapper; on agent exit the pane execs the `wt shell` program with the same assembled environment; the record is unchanged. |
| session absent, no agent selected or recorded | create through `wt exec --no-gate <target> -- <interactive shell>` using the same program and arguments as `wt shell`; leave the tree's agent null. |
| `--all` | tmux-only; attempt every live non-canonical tree; recorded agents use `resume`, unrecorded trees use `session.agent`'s `start` when configured and shells otherwise; never attach. The canonical anchor is opened only when named explicitly. A failure for one tree is recorded as `{target, name, failed:true, code, message, remedy}` and does not stop later trees. The command exits with the highest exit class observed after the batch; JSON has `ok:false`, retains `data.sessions`, and reports the worst error at top level. |
| `session.backend = "none"` | a per-tree `open` performs the full door and execs the interactive `wt shell` program with its arguments and cwd; `open --no-attach` is an exit-0 no-op with a sessions-disabled notice because there is nothing to provision; `open --agent X` refuses with remedy `session.backend = "tmux"`; `open --all` refuses with a remedy naming per-tree `wt open`; `close` is an exit-0 no-op with `closed:false` and a sessions-disabled notice. `list`, `remove`, `prune`, and `close` execute no tmux process. |

A session whose agent has ended is a shell (A33 continued); tmux remains the
session's liveness truth (A24).

**Creation has a startup observation window (A49).** `new-session` exiting 0 means tmux created a session, not that it remained alive. wt polls `has-session` for 250 ms after releasing the bootstrap gate; if the session disappears in that window, wt reports `SESSION_CREATE_FAILED` (7) carrying the pane's captured output. The agent wrapper propagates could-not-start statuses so the window still catches a misconfigured agent, while an agent that ran and exited leaves the shell (A62); the named A62 residual — an agent that runs and itself ends 126/127 — is caught only when it ends inside the window, the same timing heuristic. This catches immediate bootstrap failures but is not proof that the session will survive after the window. The resolved home is passed with `-e` because tmux copies no arbitrary variable from client to session on an existing server; only `update-environment` names cross, so an inner wt would otherwise inherit whatever home the server captured when it started. This is the bootstrap home, distinct from a working home a repository may set for processes inside the tree (A49).

Attachment occurs only when all hold: `session.attach = true`; both stdin and stdout are terminals; output is not JSON; `$WT_ACTIVATION` is unset; neither `--no-attach` nor `--all` applies. Inside tmux wt uses `switch-client`; otherwise it uses `attach-session`. These conditions affect attachment only: except for `new --no-open`, an absent session is still created. An agent therefore starts only when wt successfully creates a session, never when it attaches to one.

Consequently the only tree-lock holders are passthrough doors (through their exec'd child), `run` parents, and doors in their prelude (D2–D6); sessions and attach clients hold none. Sessions are closed by `remove`/`unregister` (§11.4 step 5), by `prune` before tombstoning (§12), and by `wt close`.

Every tmux command that addresses an existing target uses tmux's exact form `=<session_name>`; `-s` at creation still receives the unprefixed derived name.

**`wt close [target|--all]`**: resolve the target; backend `none` → the no-op above; if `has-session =<session_name>` → `kill-session` (no lock is taken: sessions hold none); JSON is always `{ sessions: [ { target, session: session_name, closed: bool } ] }` — one element without `--all` (`closed: false` when no session existed); `--all` iterates every live tree. Idempotent.

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
`wt run <task> [target] [--wait d|forever] [--timeout d] [--dry-run]
[--no-log] [--take] [-- <args…>]` (aliases `test lint fmt build`; `sync` is §11.3;
`destroy`/`refresh` drive §10.4). Trailing arguments are resolved and validated
before X1, and before `--dry-run` prints its plan. Starting at the invoked root,
a node with a `run` recipe is the resolved argument target. Arguments reach
only that recipe; nodes that run before it never receive them. An argv recipe
appends them element-wise after templating. A shell-string recipe is templated
and then spawned as `sh -c <expanded-recipe> <task-id> <args…>`, so the recipe places them
through positional parameters. With arguments present, a lexical scan must
find `$@`, `$*`, `${@`, `${*`, or `$<digit>` in the shell text, else
`ARGS_UNSUPPORTED` (2) names `"$@"` as the fix; a match in a comment is an
accepted residual. A wt-composed run-less node is transparent through an
exactly-one-need chain, so `wt test -- -k foo` in a single-adapter repository
reaches the adapter recipe. A fan-out of two or more refuses with
`ARGS_ON_COMPOSITE` (2), naming the node's direct constituent tasks. A
user-declared aggregate (A57) refuses with `ARGS_ON_COMPOSITE` regardless of
fan-out, naming its needs. A resource reached anywhere in the traversal
refuses with `ARGS_UNSUPPORTED` because its `run` is a state transition
replayed from snapshots, not a parameterised invocation. Arguments appear in
the log header, in `--dry-run`, and in `run --json` data; giving them without
`--` produces a usage error that names `--` (A58).

`--take` is valid only when the invoked root is an exclusive resource;
otherwise `TAKE_REQUIRES_EXCLUSIVE` (2) names that rule. If another tree is
the holder, displacement is a separate serialised transition: acquire only
the holder's level-3 resource lock, drive its record with `Destroy` through
its frozen instance, clear the arena holder, and release before the ordinary
run acquires the caller's resource lock and claims the arena. Two level-3
locks are never held together. `DestroyFail` leaves the holder record
`orphaned`, retains the arena holder, and stops with `DESTROY_FAILED` because
the instance may remain. A live holder with no record for the key is cleared
without running any destroy (there is nothing recorded to destroy) and the
report names it; an instance such a holder actually left running is outside
the record and is the accepted residual. `--take` never prompts; it carries
consent (A54, A60). Success reports `displaced <holder>` in text and in §14.4
data.

| Step | Action | Lock |
|---|---|---|
| X0 | door D0–D6 (§9.1) | 1,5,6 |
| X1 | for each node in plan order: `assemble` with `contributed` = resources currently `present`; acquire `lock_plan(node, held)` in order | per plan |
| X2 | resource node: refresh its declaration (§10.5) then `resource::step` with event `Run` (§10.4). Task node: `exists` → Present: "present", skip; Failed: `TASK_PROBE_FAILED` (6), stop; then spawn `run` (cwd `node.cwd`) through the `run` parent (§9.2), with trailing arguments only when this is the resolved argument target; non-zero → "failed", stop | — |
| X3 | release the node's guards in reverse order; continue | — |

Output is tee'd to `<tree>/.wt/logs/<ScopeEnc>-<task>-<utc>.log`; before writing a new log, logs of the same `(scope, task)` beyond the newest `logs.keep − 1` are deleted (§12). `--dry-run` prints the plan with `lock_plan` output and executes nothing; task env ends with its node.

### 10.2 Resource identity and lock
`ResourceKey := { label|null, tied_to, name|null, scope: RelDir, task }`;
tree and repo keys carry the label, machine keys carry `label: null`; only a
tree key carries its tree name. The state key is `"<ScopedTask>"`, including
in `_machine.json`, so two labels declaring the same machine-scoped task share
one record. The lock path is per §4 (level 3), held from probe through the
final commit. CLI selection is by `ScopedTask`.

For a resource with `exclusive = "repo"|"machine"`, the corresponding arena
store (`_repo.json` or `_machine.json`) additionally carries
`exclusive.<ScopedTask> = { holder: <tree target>, since }`. Holder changes
are made only under that store's RMW lock. A holder naming no live registry
tree is treated as absent (A60).

### 10.3 Snapshots and `execute`
```
ResourceSnapshot := { schema: 1, key, name, cwd_rel: RelDir, exists: CmdExpanded|null, destroy: CmdExpanded, run: CmdExpanded|null,
                      env: Map<String,String>,                     // minimised (below)
                      bin_dirs: [AbsPath], bin_exes: [String],     // declared bin dirs and the executable names found in them at snapshot time (A25)
                      roots: { tree, home }, recorded_sequence, recorded_at }
CmdExpanded := { shell: String } | { argv: [String] }
```
- **Env minimisation.** `env` contains exactly: all currently assembled `WT_*` keys except `WT_ACTIVATION`; `PATH`; every declared alias key and the resource's task-env keys with their assembled values; keys listed in `snapshot_env`. For **repo-tied** resources the tree-specific keys of §5.5 (`WT_ROOT`, `WT_TARGET`, `WT_NAME`, `WT_BRANCH`, `WT_BIN`, `WT_PATH_PREFIX`, `PATH`) are removed (A28, A70). For **machine-tied** resources those keys and the repo-specific `WT_LABEL` and `WT_REPO` keys are removed (A56). Removed exports such as `WT_SESSION`, `WT_NAME_SNAKE`, `WT_NAME_SHORT`, and `WT_SLOT` are neither assembled nor specially persisted. Nothing else is stored; a recipe that needs a frozen parent value names it in `snapshot_env`. `register` prints the keys each resource will persist; no report prints values (§14.4).
- **Executable inventory (A25).** For tree- and repo-tied resources, `bin_dirs` = the tree's declared `bin` directories (absolute) and `bin_exes` = the names in them that `exec` would run — symlinks resolved, since a link to a binary runs under its own name — both captured at snapshot time. Machine-tied resources capture neither: tree binaries are tree-specific.
- **Working directory.** Tree-tied: `roots.tree/cwd_rel`. Repo-tied: the label's current canonical path (read from the registry) joined with `cwd_rel`; `register --move-to` therefore needs no snapshot rewrite. Machine-tied: `roots.home/cwd_rel`.

`execute(snapshot, Exists|Destroy|Run, tree_missing)`, reading no config layer:
1. **Environment.** The invoking wt process's environment overlaid by `snapshot.env` (snapshot keys win).
2. **Missing-tree rule (A25).** `tree_missing` = `roots.tree` is absent **or the tree's phase is `replaced`** (§11.1) — a replaced directory is never used. If `tree_missing` or any `bin_dirs` entry is absent: split **the recipe text about to be run** on whitespace and `; | & ( ) < >`, strip `'` and `"`; if any word equals a name in `bin_exes` → do not run; result `orphaned(exe_missing)`, remedy "rebuild the tree's binaries, or destroy by hand: `<recipe>`". Otherwise run with `PATH` stripped of every `bin_dirs` entry. If the tree exists (not replaced) and all `bin_dirs` exist, run with the env unchanged. wt's guarantee for a replaced or missing tree is exactly: no process is spawned with a cwd inside the replacement directory, and no replacement binary is on PATH; identities a recipe derives from the recorded `WT_ROOT` string are the recipe's own semantics (A29).
3. **cwd.** Tree-tied: `roots.tree/cwd_rel` if it exists and `tree_missing` is false, else `$TMPDIR` (else `/tmp`) with `WT_ROOT` left at the recorded string. Repo-tied: canonical path `/cwd_rel`; absent → `orphaned(repo_root_missing)`, remedy "`wt register … --move-to`, or destroy by hand". Machine-tied: `roots.home/cwd_rel`.
4. Spawn `sh -c shell` or `argv`; deadlines §13.3.

### 10.4 Resource state machine (A22, A31)
```
ResourceRecord := { key, declaration: ResourceSnapshot, instance: ResourceSnapshot|null, state: declared|present|orphaned, reason|null,
                    external: bool, undeclared: bool, last_probe: {at, result}|null, last_error: {at, event, message, child|null}|null, since }
```
Probes, runs and destroys use `instance` when present, else `declaration` (§10.5). The **instance is frozen** from the fresh declaration (i) immediately before wt spawns `run` (durable before the spawn, so a crash mid-run still leaves a teardown snapshot) or (ii) at the first Present probe when `instance` is null (`external = true`); it is cleared only by a confirmed-absent probe. `Destroy` carries `teardown` (true inside `remove`, `unregister`, `prune`). Every state write is durable before the next effect; a `Failed` probe never triggers `run` or `destroy`. There are no persisted in-progress states: the resource lock (§10.2) serialises transitions and the next probe decides after a crash (principle 4). `name` default: tree-tied `{{name_short()}}_<name_snake(ScopedTask)>`, repo-tied `{{label()}}_<name_snake(ScopedTask)>`, machine-tied `machine_<name_snake(ScopedTask)>`; `WT_SELF` is the expanded value.

| State | Run | Probe (`list`/`status --probe`) | Destroy | Refresh |
|---|---|---|---|---|
| **declared** | probe: Present → **present** (freeze if null; external); Absent ∧ `run` → freeze instance → run → RunFail: stays, `last_error`, `TASK_FAILED` (6); RunOk → `ready_within` poll or one probe: Present → **present**; Absent → stays, `last_error(absent_after_run)` (6); Failed → stays, `last_error(probe_failed)` (6). Absent ∧ no `run` → stays, exit 0, notice `RESOURCE_DECLARED_EXTERNAL`. Failed → stays, `RESOURCE_PROBE_FAILED` (6) | Present → present (freeze if null); Absent → stays, instance cleared; Failed → stays, finding | probe: Present → as **present**/Destroy; Absent → teardown: **dropped**, else stays; Failed → teardown: **orphaned**(probe_failed), else `RESOURCE_PROBE_FAILED` (6) | as Destroy, then as Run |
| **present** | probe: Present → stays, "present"; Absent → **declared** (instance cleared, `RESOURCE_GONE`) then as declared; Failed → stays, `RESOURCE_PROBE_FAILED` | Present → stays; Absent → declared, cleared (`RESOURCE_GONE`); Failed → stays, finding | run `destroy`: DestroyFail (incl. `exe_missing`, `repo_root_missing`, timeout) → **orphaned**(reason); DestroyOk → probe: Absent → instance cleared; **dropped** if `undeclared` or teardown, else **declared**; Present → **orphaned**(still_present); Failed → **orphaned**(probe_failed) | as Destroy; then as Run if `run` else success |
| **orphaned** | refused `RESOURCE_ORPHANED` (6) | Present → stays; Absent → **declared**, cleared (`RESOURCE_GONE`); Failed → stays | as **present**/Destroy (retry) | refused |

Invariants: a record exists for every effective tree-tied resource from its first refresh; `present` only on a Present probe; a record is dropped only after a confirmed-absent probe and only when undeclared or during teardown; teardown terminates (after one `Destroy{teardown}` pass every record is dropped or `orphaned`); a no-`run` resource is never run.

**Exclusive resources (A60).** Only the arena holder may probe or destroy the
shared instance. A non-holder's record remains `declared`: `list`/`status
--probe` skip it and `remove`/`prune` drop its teardown record without running
either recipe. The arena entries govern teardown independently of the local
declaration: before a teardown or destroy/refresh path probes or destroys a
tree-tied record whose declaration names an arena, wt consults that arena and
any live other holder triggers the non-holder rule; with `exclusive = null`,
the unconditional divergence guard consults both arenas but honours an entry
only when its holder target has the record's label, because the guard protects
checkouts of the same repository and cross-label machine exclusivity is
enforced through the declaring configuration (A60). An absent or dead holder
proceeds normally. A run by another live holder refuses `RESOURCE_HELD` (4),
names the holder tree, and gives `--take` as the remedy. With no holder, run
first probes: Present claims the arena and then freezes an external instance;
Absent claims before the ordinary declared → run → present transition. The
claim therefore survives a crash during run alongside
the already-frozen teardown instance. A holder runs normally. Its confirmed
Absent probe clears both its instance and arena holder (`RESOURCE_GONE`); if
the same invocation will run again, it reclaims before doing so. Successful
holder teardown by `destroy`, `refresh`, `remove`, or `prune` clears the arena
holder; failed destroy retains it.

### 10.5 Declaration refresh; repo- and machine-tied declarations
**When.** Declarations are refreshed at `register`/`adopt` (I3), `new` (S4), `sync`, `wt run <resource>` (X2, before the step), `list`/`status --probe`, and `remove`/`unregister` step 8 — never by an ordinary door (§0). For each effective resource `r` across all scopes: run `assemble` once more with `task = TaskContext{r}` and `r`'s scope from the same `clean` parent, render output discarded, notices suppressed except `ENV_UNDEFINED` (→ `REFRESH_SKIPPED{r}` warn, record unrefreshed); build `r`'s snapshot (§10.3); then, by `tied_to`:
- **tree-tied** → the tree's state file (tree RMW, level 6): upsert the record: absent → **declared** with `declaration`; otherwise replace `declaration` only, never `instance`;
- **repo-tied** → `_repo.json` (repo RMW, level 6): upsert `resources[<ScopedTask>].declaration` from the refreshing tree's **stripped** snapshot (A28, A31); `instance` and state are never touched by a refresh.
- **machine-tied** → `_machine.json` (machine RMW, level 6): upsert
  `resources[<ScopedTask>].declaration` from the invoking context's snapshot
  stripped of tree- and repo-specific keys (A56, A61); `instance` and state
  are never touched by a declaration refresh. The store uses the same
  `{schema, label, resources}` shape and protocol as `_repo.json`, with
  `label: null`.

**Teardown obeys the record, never the working tree (A48).** Frozen-instance teardown predates A48: at `remove`/`unregister` step 8 the refresh may only replace a `declaration`, while a resource with a frozen `instance` is destroyed through that instance and a vanished declaration remains recorded as `undeclared = true`. A48 adds newest-first teardown, ordered by the durable `recorded_sequence` of the effective snapshots (with `recorded_at` only as a compatibility fallback for older records). It does not newly justify the absence of an approval gate.

Undeclared tasks keep their records with `undeclared = true` until dropped by a confirmed-absent probe during teardown. **Repo- and machine-tied semantics:** while `instance` exists it governs; otherwise the most recent stripped declaration (the invoking context's, since an action refreshes first) is effective. There is no cross-tree or cross-repo agreement check. Ordinary `remove` of a non-canonical tree tears down tree-tied records only; repo-tied instances survive while the label remains and are destroyed only by `destroy`/`refresh`/`unregister`. Machine records are never touched by `remove` or `unregister`; their lifecycle teardown is performed only by explicit `wt destroy` or `wt refresh`, both retaining their unconditional confirmation gate. Other declaration-refresh, run, and probe sites follow the ordinary §10.1–10.5 rules.

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
wt new <label>/<name> [--branch B] [--from REF] [--detach] [--meta k=v]… [--no-sync] [--verify] [--no-fetch] [--no-open] [--no-attach] [--no-build]
REF := <local branch> | <remote>/<branch> | pr:N | <PR URL> | <rev>
```
`--from` bare `X`: `refs/heads/X` → `refs/remotes/origin/X` → rev; both present and different → local wins with `FROM_LOCAL_SHADOWS_REMOTE`; `origin/X` forces remote. Default start `origin/<default>` after a bounded fetch (`--no-fetch` skips; unpushed local default-branch commits need `--from main`). A fetch covers only the branches the creation consumes — the requested start when it can name an origin branch, plus the default branch. Any narrow-fetch failure other than a timeout falls back once to the full `refs/heads/*` fetch (a wanted name may be a tag, a raw revision, or a renamed default), so anything resolvable before stays resolvable; when the fallback also fails, the narrow fetch's error is reported because it names the refs the creation asked for, and a timeout propagates without a second attempt. Default branch: `origin/HEAD` → `main`/`master`/`trunk` → HEAD, cached, refreshed on fetch. PR refspec by origin host: github `refs/pull/N/head`; gitlab `refs/merge-requests/N/head`; bitbucket `refs/pull-requests/N/from`; unknown: pull then merge-requests; fetched as `refs/wt/pr/N`, local branch `pr/N`, default name `pr-N`. A PR URL selects the label whose normalised origin (https or scp-style ssh, `.git` stripped) matches host and `owner/repo`; zero/many → error with remedy. `B` defaults to `<name>`; `--branch feature/x` without a name → `feature-x`. `AddSpec`: existing branch · `-b B <start>` with `--no-track` unless start is `refs/remotes/*` · `--detach`.

Each repeatable `--meta k=v` initializes one §4.1 metadata entry. Repeated keys
take their last value. The option affects a newly allocated or fresh
incarnation; resuming an existing incarnation retains its recorded map.

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
| S3 | `copy` (§11.7); record `materialized`; exclude | 6, 5 |
| S4 | `PORTS` (§7); assemble + declaration refresh (§10.5) + render (§8.3) | 5, 6 |
| S5 | `sync` through §10.1 under the held lock (unless `--no-sync`) | 2,3,4,6 |
| S6 | state: `sync.inputs`, phase `ready`, `op = null`, `verify_pending = --verify` | 6 |
| V | `--verify`: run `verify` through §10.1; state `verify = {at, ok, log}`, `verify_pending = false` | 2,3,4,6 |
| B | start the effective automatic `build` task (A69), unless `--no-build`, `--no-open`, or no `build` task exists; record `build = {started, log, pid}` in the state file | 6 |
| F | release | — |

**Automatic build (A69).** Step B runs after the tree is `ready`. In human mode the summary is printed before `build started` and its log path. On either backend wt launches a setsid/double-fork supervisor that outlives the parent CLI, records that supervisor's pid, invokes `wt build <target>` through the ordinary §10.1 path, tees through the existing task log, and atomically replaces the status file from `running` to `ok` or `failed`. `new --json` returns without waiting and includes `build = {started, log, pid}`. A later foreground `wt build` resets the same status first and records its own pid and start time, so the live run is never read against the finished supervisor's. The status file and recorded pid are one slot owned by the most recent build starter: a foreground build takes that ownership even while a supervisor still runs, and liveness is judged against the owner alone. A `running` status whose recorded pid is dead is normalised to `abandoned`: `doctor` warns with `BUILD_ABANDONED` and a `wt build <target>` remedy, while §9.2a treats it as not running and gives the ordinary `COMMAND_NOT_BUILT` remedy. Build failure is surfaced by `status`, `doctor`, and §9.2a shims, not by the originating `new`; there is no setup-window state. The task's `needs` still run first and in order. Setup that is more than compilation is expressed by giving `build` a `needs` list; there is no separate creation-hook mechanism.

After F, `new` prints its phase-1 human summary, then applies §9.4: with backend `tmux`, it ensures the session and attaches when the attachment predicate holds. `--no-open` skips both creation and attachment. `--no-attach` still creates the session. Backend `none` leaves the ready tree without a session. Agent selection comes only from `session.agent`; `new` has no agent flag. Session provisioning is additional to the completed tree: if it fails, `new` emits warning notice `SESSION_CREATE_FAILED` naming `wt open <target>` as the retry, exits 0, and retains the complete `NewData` payload in JSON as well as text mode.

Failures: G → class 7/5/4, entry stays (`claimed`, resumable); S4/S5 → `failed`, tree remains (R3); V → `VERIFY_FAILED` (6), tree `ready` with `verify_pending` cleared. Never `created:false` without `ready`.

### 11.3 `sync`
`wt sync [target] [--force]`: tree lock exclusive (1, deadline; `TREE_IN_USE`) → identity check → state phase `bootstrapping`, `op{sync}` (6) → tracked check for rendered paths (§8.3) → declaration refresh (§10.5) → run `sync` through §10.1 → record input hashes (`git hash-object` of adapter ∪ config `sync_inputs`) → `ready`/`failed`, `op = null` (6) → release. Unchanged inputs ∧ `ok` ⇒ no-op unless `--force`.

### 11.4 `remove`
Classification (pure over git results): "unpushed" = upstream present ∧ `rev-list --count @{u}..HEAD > 0`; upstream `[gone]` ⇒ yes; no upstream or detached ⇒ `git branch -r --contains HEAD` empty; tags ignored. `dirty` = `git status --porcelain=v1 --untracked-files=normal` non-empty.

| Step | Action | Lock | Mutates |
|---|---|---|---|
| 1 | resolve the address by §3.2; canonical → `USE_UNREGISTER` (2) (this refusal applies to `wt remove` only; `unregister` skips it, §11.5) | — | no |
| 2 | observe: dir exists?, identity (§4.1), dirty, unpushed, `has-session`, live door holders, tree-tied records with fresh probes | 3 | no |
| 3 | build `RemovePlan`, resolving step 10's branch decision; a work-losing plan (dirty, or unpushed commits on a branch this run deletes) without `--force` → TTY: consent at step 4; non-TTY: `TREE_DIRTY` (5) (A54) | — | no |
| 4 | consent for a work-losing plan on a TTY without `--force`: prompt with the plan (it lists the session and the door holders); `n` → exit 0, `removed: false`, info notice `REMOVE_DECLINED` naming the target and saying nothing changed. Clean, forced, missing and replaced removals do not prompt, and `--yes` does not unlock a work-losing one (A54) | — | no |
| 5 | `kill-session` if `has-session` (consented); then tree lock exclusive with deadline `locks.tree_exclusive` (`--wait d`); timeout → `TREE_IN_USE` (4) naming the holders, remedy "wait for or stop them" | 1 | session only |
| 6 | revalidate under the lock: identity check (§4.1) (mismatch → `TREE_REPLACED`, nothing destroyed); dirty/unpushed re-observed (newly dirty without `--force` → `TREE_DIRTY`, nothing changed) | — | no |
| 7 | state `removing`, `op{remove}` | 6 | yes |
| 8 | declaration refresh if the dir exists (§10.5); every tree-tied record: §10.4 `Destroy{teardown}`, newest record first; all attempted; an exclusive non-holder record is dropped without probe/destroy, while successful teardown of the holder clears its arena entry in this pass; the unconditional `exclusive = null` consult honours only a same-label holder because it protects checkouts of the same repository, while cross-label machine exclusivity is enforced through the declaring configuration (A60) | 3, 6 | yes |
| 9 | any record not dropped → without `--keep-orphans`: `DESTROY_FAILED` (6), tree stays `removing` → `remove-interrupted` with its records, nothing below runs (remedy: fix, then `wt remove` again, or `--keep-orphans`, or `wt prune --records <target>`); with `--keep-orphans`: state `op = null`, continue; the entry stays live after step 10 (derived phase `missing` with records; `TREE_MISSING_PENDING` protects the name) and step 11 is skipped | — | |
| 10 | `git worktree remove --force <entry.path>`; the branch per step 3's decision — deleted when its commits are on a remote, kept when they are not, kept for an adopted tree, always deleted with `--delete-branch` (`-D`), never with `--keep-branch` (A54); missing dir → `git worktree prune` | 2 | yes |
| 11 | registry: entry → tombstone (record-free; carries `materialized` paths); delete the state file; exclude block; delete the tree's build cache (the door's rendered `CARGO_BUILD_BUILD_DIR`, else the adapter scheme's path) when it lies under `$WT_HOME/cache` and ends in the tree's `name_short` — an override elsewhere keeps its own lifecycle; failure → warn notice `CACHE_DELETE_FAILED`, reaped later by `prune` (A64) | 5 | yes |
| 12 | release | — | |

**Missing directory:** steps 2 (records only), 3–5, 7–8 (records only, `execute` with `tree_missing = true`), 9, 10 (`git worktree prune`), 11. **Replaced directory** (phase `replaced`): steps 2 (records only), 4–5, 7–9 with `tree_missing = true`; no git step (the directory is not ours; doctor reports it as `UNMANAGED_WORKTREE`/`STALE_GIT_WORKTREE`); then 11 (A31). **Already absent:** only an explicit `label/name` address with no live tree and a tombstone for that address ⇒ `removed: false`, exit 0, info notice `ALREADY_REMOVED` saying that no live tree exists and the tombstone records its removal. Every other unresolvable address is §3.2 `NOT_FOUND` (3), including bare names outside their label context; its candidate remedy is preserved. Human output for each no-op or refusal states the reason (A65).

Step 8 excludes machine-tied declarations and records completely: `remove`
does not refresh, probe, destroy, or rewrite `_machine.json` (A61).

### 11.5 `unregister`
`wt unregister <label> [--yes] [--force]`: refuses while non-canonical trees exist (`TREES_EXIST` 5) unless `--force` (removes them first via §11.4, one consent prompt listing all). For the canonical tree the teardown is performed inline: §11.4 step 1 **without** its canonical refusal, then steps 2–8 exactly as written (step 8 is the **only** tree-tied teardown pass), then **every repo-tied record** with `Destroy{teardown}`; **failure barrier**: if any tree-tied or repo-tied record is not dropped → `DESTROY_FAILED` (6), the canonical tree stays `removing` → `remove-interrupted`, and nothing below runs; otherwise artefact cleanup — hash-owned rendered files deleted via §5.7, `.wt/` deleted (consented; it may hold application data), anything else `ARTIFACT_KEPT` with its exclusion retained; exclude block removed if nothing kept; registry and state records deleted. The checkout is never deleted. Machine-tied declarations and records are excluded from both refresh and teardown, and `_machine.json` is left byte-for-byte untouched (A61).

### 11.5a `forget`
`wt forget <target> [--yes]` is records-only unregister for one live non-canonical tree; canonical → `USE_UNREGISTER` (2). It refuses while the tree has instantiated resources — a record carrying an instance whose exclusive arena no other live tree holds, the same test `destroy` applies (A72); the refusal names them and its remedy names `wt destroy` and `wt rm` — while its session is live (remedy `wt close`), or while door holders exist (remedy: wait). After unregister-shaped consent it deletes hash-owned rendered files and `.wt/`, removes this tree's exclude entries, deletes its state file, and moves the registry entry to a tombstone with `reason = "forgotten"` and no materialised paths. It never removes the directory, branch, git worktree registration, or per-tree build cache. A mis-adoption is recovered with `forget`, then `adopt --name` using the correct name; tombstones, unlike live entries, do not participate in path uniqueness.

### 11.6 `register` and `adopt`
`wt register [path] [--label L] [--move-to PATH] [--repair]`, `wt adopt <path> [--label L] [--name N] [--agent X] [--meta k=v]…`:

| Step | Action | Lock |
|---|---|---|
| S0 | tree lock exclusive for the address | 1 |
| R | write the state file `{initialising, op}` (6), then registry txn (5): path/gitdir uniqueness (§4.1); label (register) and tree entry; §7 allocation incl. `ports` from the checkout's config; identical existing registration with no `op` ⇒ `registered: false` (the pre-written file is deleted); `init-interrupted` ⇒ resume | 6, 5 |
| I1 | write `.wt/tree_id` | — |
| I2 | exclude block; print the declared summary incl. the keys resources will persist (R10) | 5 |
| I3 | assemble + declaration refresh (§10.5) + render (§8.3) | 6, 5 |
| I4 | state `ready`, `sync: null`, `op = null` | 6 |

`adopt` requires the path to be listed by `git worktree list` of that gitdir (`NOT_A_WORKTREE` 5). `register --move-to` updates the canonical path and runs `git worktree repair`.
`adopt --agent` names a declared agent (including either built-in) and records it without starting a session, so the first `open` uses its resume recipe. `adopt --meta` has the same validation, duplicate-key, and last-value-wins semantics as `new --meta`.

`register <path> --label L --repair` recovers a canonical checkout in derived phase `replaced` because its `.wt/tree_id` marker is absent or wrong. It succeeds only when `path` is label `L`'s recorded canonical path, its common gitdir still matches the label, and the phase is `replaced`; otherwise `REPAIR_REFUSED` (5). Under the exclusive tree lock it rewrites the marker from the registry entry, recomputes the exclude block, and re-renders hash-owned files. It does not allocate or append coordinates, refresh resource declarations, alter resource/sync/verify state, or touch tombstones. `doctor`'s `TREE_REPLACED` remedy for a canonical tree names this command.

### 11.7 `copy`
Run exactly once per incarnation at `new` S3 (never for canonical or adopted trees). Source root = the canonical checkout. Per entry: source absent → `COPY_ABSENT` info; tracked by git → `COPY_TRACKED` (5), `new` aborts at S3 (`incomplete`, resumable); destination exists → `COPY_EXISTS` info, never overwritten; otherwise copied via §5.7 (files byte-for-byte with mode, directories recursively, symlinks recreated); record `materialized {kind: copied, hash: null}`. Copied paths are not hash-owned and are never re-rendered or individually deleted; they are excluded while the tree or its tombstone exists. Task side effects outside the tree are never tracked; a side effect that occupies a future tree path surfaces as `PATH_OCCUPIED` at that tree's `new` (§11.2 G).

## 12. Truth: `list`, `status`, `doctor`, `prune`, logs
`wt ls [label] [--probe] [--fast] [--disk] [--meta key]`, `wt status [target] [--probe]`: address, phase (§11.1), branch/detached, dirty counts, upstream ahead/behind, behind default, sync state (`ok | stale (<files>) | failed | never`, `behind <default> by N`, **`drift (<files>)`** = sync inputs changed on the default branch since the merge-base: one bounded `git diff --name-only HEAD...origin/<default> -- <sync_inputs>` per tree, S3/A31; `--fast` skips it), session `yes|no|unknown`, agent, tree-, repo-, and machine-tied resources `{scope, task, tied_to, state, external, undeclared, last_probe, last_error}`, slot/ports (+`bound` from one bind probe per declared port, skipped by `--fast`), path; `--disk` also sizes the tree's per-tree build cache (`cache_kb`, null when none exists). Human `ls --meta key` inserts a column named `key`, empty for trees without that value; JSON always carries the complete `meta` map and is unchanged by this option. `--probe` refreshes declarations (§10.5) and runs `exists` under each displayed record's resource lock. For `ls`, each distinct `ResourceKey` is probed once per pass: tree-tied keys remain per tree, while a repo- or machine-tied record shared across displayed trees is probed once and its one result is displayed for every tree that declares it. `status` probes each displayed record once. Doctor's state-orphan scan treats `_machine.json` as the machine store, never as an orphaned tree state file.

Truth is always freshly observed — wt never presents a cached fact as
current. The scan behind the exact dirty counts is kept cheap through git's
own self-invalidating caches instead: creation enables the untracked cache
(and the built-in filesystem monitor where git supports it) as per-worktree
configuration on each tree wt creates, so no other checkout's effective
status behaviour changes (the shared `extensions.worktreeConfig` switch is
enabled once, and never when `core.bare`/`core.worktree` would need
relocating first). `doctor` reports an existing tree without the untracked
cache — that key alone, since the monitor is platform-gated — as
`STATUS_CACHE_OFF`. Tree observations are independent read-only scans, so a
fleet `ls` may take them concurrently; concurrency changes neither output
order nor which error is reported relative to the sequential
implementation — the up-front shared resource-state read and the `--probe`
prewalk surface their errors first, as they always have, and per-tree
observation errors then surface in tree order, earliest tree first.

Exclusive resource rows additionally show `holder|null` in JSON and the live
holder in human output. A non-holder `--probe` skips the recipe and leaves its
record declared. `RESOURCE_HELD` is the run conflict in §10.4 and names both
the holder and the `--take` remedy. A teardown or destroy/refresh skip caused
by another live arena holder emits the info notice `RESOURCE_HELD_BY`, naming
the holder as the target to which the resource was left.

`wt doctor [label] [--probe]` findings `{severity, code, subject, message, remedy}`:

| Code | Condition (owner) |
|---|---|
| `STATE_ORPHAN` (info: a state file whose address has no live entry; deleted by `prune`); `CACHE_ORPHAN` (info: a `cache/cargo-build` entry that is no registered label's live or tombstoned `name_short`; deleted by `prune`); `REPO_PATH_MISSING`, `TREE_REPLACED` | §4.1, §4.3–4.4, §5.4, §6.1 |
| `TREE_MISSING`, `TREE_INCOMPLETE`, `TREE_INTERRUPTED`, `INIT_INTERRUPTED`, `REMOVE_INTERRUPTED`, `TREE_CLAIMED`, `VERIFY_PENDING`; `UNMANAGED_WORKTREE`, `STALE_GIT_WORKTREE`, `BRANCH_MERGED` (`merge-base --is-ancestor` ∧ not equal), `UPSTREAM_GONE` (`%(upstream:track) == [gone]`) | §11.1; git vs registry |
| `RESOURCE_ORPHANED`, `RESOURCE_GONE`, `RESOURCE_UNDECLARED`, `RESOURCE_PROBE_FAILED`, `REFRESH_SKIPPED`, `NAME_MAY_COLLIDE` (info: a `name` template uses `name()`/`name_snake()` but none of `name_short()`/`target()`/`root()`) | §10.3–10.5 |
| `TREE_MISSING_PENDING`, `GEOMETRY_CHANGED` (info), `SLOT_SQUATTED`, `PORT_SQUATTED` (warn: bound with no session and no running task), `PORTS_EXHAUSTED` | §7 |
| `ADAPTER_TOOL_MISSING`, `ACCELERATOR_*`, `NO_LOCKFILE`, `NO_ADAPTER`, `NO_VERIFY` | §6 |
| `NO_COORDINATION` (info: the label's effective root config declares no `ports`, no `env` alias and no resource, so parallel trees share the application's default coordinates; remedy "declare `ports`/`env` in `.wt.toml` or `$WT_HOME/config.toml [repos.<label>]`"; A13); `SESSION_BACKEND` (info: the effective session backend); `SHELL_INIT_MISSING` (info: no rc file wt knows installs §14.6's guard while a label claims commands or declares `bin`; remedy `wt setup`; A76) | §12, §5.4 |
| `BIN_DIR_MISSING` (doctor only, A50), `PATH_NOT_SHADOWED`, `PORT_BOUND`, `SHIM_SHADOWED` (info); `SHIM_BROKEN` (warn); `EXCLUDE_MISSING`, `EXCLUDE_REPAIRED`, `ACTIVATION_IGNORED` | §9, §12, §4.2, §8.1 |
| `IDENTIFIER_LONG` (resource name > 63); `TREE_IN_USE` (info, holders), `GIT_TOO_OLD` (< 2.31); `STATUS_CACHE_OFF` (info: an existing tree without git's untracked cache pays a full working-tree scan per status; wt enables the caches on trees it creates) | §5, §13, tooling, §12 |

`wt prune [label] [--yes] [--merged] [--gone] [--records <target>]`: retries orphaned destroys (`Destroy` on `orphaned`); runs §11.4's missing-directory path for `missing` trees (ending in a tombstone); `git worktree prune`; deletes `STATE_ORPHAN` files; deletes `CACHE_ORPHAN` paths (contained delete under `$WT_HOME/cache`, no symlink following; a tombstoned address's cache is kept because recreation reuses its coordinates, A64); deletes each `exclusive.<ScopedTask>` arena entry whose holder is not a live registry tree under that arena store's RMW lock and reports the deletion as a prune item; `--merged`/`--gone` remove clean trees so classified (dirty ⇒ `keep`). Before any step that creates a tombstone, `prune` `kill-session`s the address's session if tmux reports it (consented by the same prompt). `--records <target>` applies to **live entries** in phase `missing`, `replaced` or `remove-interrupted`: it drives that entry's records with `Destroy{teardown}` from their own snapshots (§10.3, `tree_missing = true` for `missing`/`replaced`) and never acts on any directory; it creates no tombstone (the entry stays live until `wt remove`/`wt new`). **Tombstone collection**: for each tombstone of the label, after the session check, delete the tombstone and recompute the exclude block in one registry RMW (5). Consent: TTY without `--yes` → prompt; **non-TTY without `--yes` → print the plan, exit 0 with `data.applied = false` and notice `CONFIRM_REQUIRED`** (prune is a report-then-act verb; §14.2).

**Log retention (A31).** `<tree>/.wt/logs/` keeps the newest `logs.keep` (default 20) logs per `(scope, task)`; older ones are deleted by the `run` that creates a new log (§10.1). `--no-log` writes none.

**Timing log (A75).** With `[logs] trace = true` (off by default), every invocation appends JSON Lines to `$WT_HOME/logs/wt.jsonl`. Records carry `{v, t, run, seq, pid, cmd, kind, name, ms}` and, per kind: `child` (one subprocess) adds `op` for a wt-composed argument list and `code` or an `outcome` of `timeout`/`failed`/`detached`; `lock` (an acquisition that blocked) and `span` (internal work) add `subject`; `cmd` closes the invocation with `code`, or with `outcome: "exec"` and the program named in `exec` for a passthrough door. A record is one append of at most 4096 bytes, truncated if longer, so parallel doors need no lock; the file rotates to `wt.jsonl.1` past 8 MiB, checked once per invocation. Task recipe text is never recorded. A failed write never fails the command.

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
| named lock | `locks.<name>.wait`, then `task.lock_wait` | 0s | `--wait d|forever` | `LOCK_HELD` (4), with `n/N in use`, per-slot holders, and the `--wait` / raise-`slots` remedy |
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
| `wt new <label>/<name> [--branch B] [--from REF] [--detach] [--meta k=v]… [--no-sync] [--verify] [--no-fetch] [--no-open] [--no-attach] [--no-build]` | yes (phase-aware) | §11.2 |
| `wt adopt <path> [--label L] [--name N] [--agent X] [--meta k=v]…` ★ | yes | §11.6 |
| `wt setup [path]… [--dry-run] [--shell S]` ★ | yes (a re-run adds repositories) | §14.7; composes §11.6, §5.4 and the configuration writes named there (A76) |
| `wt rm <target> [--yes] [--force] [--delete-branch] [--keep-branch] [--keep-orphans] [--wait d]` (`remove` alias) | yes | §11.4 |
| `wt forget <target> [--yes]` ★ | yes | §11.5a |
| `wt sync [target] [--force]` | yes | §11.3 |
| `wt ls [label] [--probe] [--fast] [--disk] [--meta key]` (`list` alias), `wt status [target] [--probe]` ★ | — | §12 |
| `wt meta <target> [k=v\|k=]…` ★ | yes; prompt-free (destroys nothing) | §4.1 |
| `wt prune [label] [--yes] [--merged] [--gone] [--records T]` | yes | §12 |
| `wt run <task> [target] … [--take] [-- <args…>]` (aliases `test lint fmt build`); `wt destroy <ScopedTask> [target]`, `wt refresh <ScopedTask> [target]` | per task / per §10.4 | §10.1, §10.4 |
| `wt exec [target] [--no-gate] -- <cmd…>` | — | §9.1–9.2; `--help`: "passthrough door; not a task (see `wt run`); no `--json` (A20)" |
| `wt edit [target]` ★ | — | §9.1–9.3 |
| `wt shell [target]` | — | §9.3 |
| `wt env [target] …` | — | §8.5 |
| `wt open [target] [--agent X] [--no-attach] [--all]`, `wt close [target\|--all]` | yes | §9.4 |
| `wt path [target]`, `wt which [target] <cmd>` ★, `wt tasks [target] [--private]` ★, `wt config [target] [--origin]` ★, `wt locks [label]` ★ | — | the root (one line); one executable under the door PATH; effective tasks; effective config per key with layer; lock table |
| `wt doctor [label] [--probe]` | — | §12 |
| `wt shell-init <zsh\|bash\|fish>` ★, `wt completions <shell>` ★ | — | §14.6 |

Global: `--json`, `--yes`, `--quiet`, `--verbose`, `--color auto|always|never`, `--home DIR`. Unknown subcommand → exit 2 with the three closest names; every `--help` carries one example. Top-level help groups commands in order as Everyday, Setup, Working inside a tree, and Upkeep. `rm` and `ls` are the primary documented spellings; `remove` and `list` remain visible aliases (A73 supersedes A55's hidden-alias presentation).

Text mode is the default and every verb has an intentional human rendering; JSON is emitted only with `--json` (passthrough exceptions: A20). There is no generic JSON-to-text fallback. Summaries use:

```
<headline: what happened>
  <key>  <value>
  next   <action, when one is needed>
```

The headline comes first. Fact keys are lower case and aligned within the block. Empty optional sections are omitted, but failures, orphaned resources and pending verification remain visible. `status`, `doctor` and `config` summarise rather than restating their JSON payloads. `path` prints only the root and `which` prints only the resolved executable (or `not found`). `list`, `tasks`, `config` and `locks` use aligned columns with a header where it aids reading; `tasks` is the effective task table, `config` shows effective keys with scope and layer, and `locks` is the coordination lock table. Output is plain ASCII apart from optional ANSI colour on existing diagnostic codes.

### 14.2 TTY and bounded-runtime rules (A14)
Control-plane deadlines per §13.3; user children run as long as they run. Idempotent re-run applies to `register`, `unregister`, `clone`, `new`, `adopt`, `sync`, `rm`, `forget`, `prune`, `open --no-attach`, `close`. stdin not a TTY ⇒ never prompt. Human stdout has the same format when redirected as it has on a terminal; only ANSI colour is omitted according to `--color`. `--json` selects the envelope instead. `unregister`, `forget`, `destroy` and `refresh` prompt on a TTY without `--yes` and require `--yes` otherwise (`CONFIRM_REQUIRED` 2); a declined prompt exits 0 with `*: false` and mutates nothing. **`rm` is gated on loss, not on the verb** (A54): it prompts only when the plan discards uncommitted work or deletes a branch carrying unpushed commits, `--force` both permits such a removal and consents to it, `--yes` never unlocks one, and without a TTY it is refused with `TREE_DIRTY` (5) rather than `CONFIRM_REQUIRED`. **Exception**: `prune` is a report-then-act verb; without `--yes` on a non-TTY it prints its plan and exits 0 with `data.applied = false` and the notice `CONFIRM_REQUIRED` (§12). **`setup` is the sole TTY-primary verb** (A76): without a terminal on stdin it refuses with `CONFIRM_REQUIRED` (2) and a remedy naming `wt register` and `--dry-run`, it refuses `--json` as `exec` does (A20), and quitting before its single consent exits 0 having mutated nothing. `--dry-run` asks nothing: it takes the default answer to every card and prints the plan, so it needs no terminal.

### 14.3 Exit classes and error type
| Code | Class | Examples |
|---|---|---|
| 0 | ok | incl. idempotent no-ops |
| 1 | internal | bug |
| 2 | usage | `CONFIRM_REQUIRED`, `JSON_UNSUPPORTED`, `USE_UNREGISTER`, `NO_GATE_REFUSED` |
| 3 | not found | `NOT_FOUND` |
| 4 | conflict | `NAME_TAKEN`, `BRANCH_IN_USE`, `LOCK_HELD`, `TREE_BUSY`, `TREE_IN_USE`, `SLOTS_EXHAUSTED`, `NAME_SHADOWS_LABEL`, `PATH_REGISTERED`, `GITDIR_REGISTERED`, `GEOMETRY_CONFLICT`, `PORTS_EXHAUSTED`, `IDENTITY_COLLISION`, `TREE_MISSING_PENDING` |
| 5 | state | `TREE_DIRTY`, `CONFIG_INVALID` (+ subcodes), `SETTINGS_INVALID`, `OPEN_ALL_REQUIRES_TMUX`, `ENV_UNDEFINED`, `TOOL_MISSING`, `COPY_TRACKED`, `RENDER_ONTO_*`, `PATH_OCCUPIED`, `HOME_OLD_FORMAT`, `*_CORRUPT`, `NOT_A_WORKTREE`, `VERIFY_PENDING`, `ROOT_IS_SYMLINK`, `TREE_REPLACED`, `TREES_EXIST`, `CWD_MISSING`, `FILE_SOURCE_MISSING` |
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
              sync: {state, at|null, changed: [RelPath], drift: [RelPath]}, verify: {ok, at}|null, build: {state: "running"|"abandoned"|"ok"|"failed"|"unknown", started, log}|null,
              session: "yes"|"no"|"unknown", session_name, agent|null, meta: Map<String,String>, resources: [Resource], ports: [ {name, port, bound|null} ], disk_kb|null, cache_kb|null }
Resource := { scope, task, tied_to, name, state, reason|null, external, undeclared, has_instance, holder|null, last_probe: {at, result}|null, last_error: {at, event, message}|null }
StepRep  := { id, scope, status, child|null, duration_ms }
```

`ok:false` does not imply `data:null`: batch operations may retain partial data,
as `open --all` does. `SessionsData.sessions` is one tagged union with three
shapes — open, closed, and failed — even though `open` emits open/failed and
`close` emits closed.

| Verb | `data` |
|---|---|
| `register` | `{ label, path, gitdir_id, registered, resumed, tree: Tree, declared: { tasks: [ScopedTask], resources: [ {scope, task, tied_to, snapshot_keys: [EnvKey]} ], env: [EnvKey], files: [RelPath], bin: [RelPath], ports: [PortName], copy: [RelPath] }, config_errors: [ {path, line, col, message} ] }` |
| `clone` | `{ url, path, cloned } & register.data` |
| `unregister` | `{ label, unregistered, destroyed: [ {scope, task, state, child|null} ], artifacts: [ {path, action: "deleted"|"kept"} ] }` |
| `forget` | `{ target, forgotten, artifacts: [ {path, action: "deleted"|"kept"} ] }` |
| `new` / `adopt` | `{ tree, created, resumed, sync: StepRep[]|null, verify: {ok, steps: StepRep[]}|null, build: {started, log, pid}|null }` / `{ tree, adopted, resumed }` |
| `remove` | `{ target, removed, destroyed: [ {scope, task, state, child|null} ], orphans_kept: [ScopedTask], branch_deleted, branch_kept: string|null, session_closed }` |
| `sync` | `{ target, ran, steps: StepRep[], inputs: [ {path, hash} ] }` |
| `run` | `{ target, task, args: [String], args_target: string|null, child|null, log|null, displaced: target|null, steps: StepRep[] }`; `--dry-run`: `{ task, args: [String], args_target: string|null, steps: [ {id, scope, origin, cwd, run, exists, lock, sys_locks, resource, tied_to} ] }` |
| `destroy` / `refresh` | `{ target, scope, task, before, after, child|null }` |
| `open` (non-attaching) / `close` | shared `{ sessions: [ {target, name, created, existing, agent|null, foreground} | {target, session, closed} | {target, name, failed:true, code, message, remedy} ] }`; `open` emits open/failed, `close` emits closed, and only `open --all` may return `ok:false` with this data retained |
| `env` | `{ target, set, overrode, restored, missing_bins, rendered, bins: [ {dir, exists, executables} ], ports: [ {name, port} ], env: Map, activation: Activation }` |
| `list` | `{ trees: [Tree], locks: [ {name, label, holder: {pid, target, verb, since}} ] }`; `status` | `Tree & { tasks: [TaskInfo], config_errors }` |
| `meta` | `{ target, meta: Map<String,String> }` |
| `path` / `which` | `{ target, path }` / `{ target, cmd, path|null, in_bin }` |
| `tasks` / `config` / `locks` | `{ target, tasks: [ {id, scope, origin, cwd, needs, resource, tied_to|null, lock|null, description|null} ] }` / `{ target, entries: [ {key, scope, layer, value} ] }` (env values shown as keys only) / `{ locks: [ {level, name, path, held, holder|null, held_slots?:u16, slots?:u16, holders?:[ {slot, path, holder|null} ]} ] }`; `held_slots` and `slots` are present for named locks, `holders` lists the held slots in ascending order and is omitted when empty, and human output renders `held n/N` with per-slot holders |
| `prune` | `{ applied: bool, items: [ {target, reasons: [String], action, result|null} ] }` |
| `doctor` / `shell-init` / `completions` | `{ findings: [ {severity, code, subject, message, remedy} ], counts: {error, warn, info} }` / `{ shell, script }` |

Redaction: environment values appear only in `env` output; `ResourceSnapshot.env` never appears anywhere.

### 14.5 Stable ordering
| Array | Order |
|---|---|
| `list.trees` | `(label, canonical first, name)` |
| `Tree.resources`, `*.destroyed`, `remove.orphans_kept` | `(tied_to: tree, repo, machine; scope; task)` |
| `Tree.ports` | recorded index (semantic) |
| `Tree.sync.changed`, `Tree.sync.drift`, `sync.inputs`, `register.declared.*`, `env.*` string arrays, `unregister.artifacts`, `env.bins[].executables` | lexical |
| `*.steps`, `tasks.tasks`, `run --dry-run.steps` | plan order (topological, ties `(scope, id)`) / `(scope, id)` |
| `tasks.tasks[].needs`, `run.args`, `run --dry-run.args`, `prune.items[].reasons`, arrays inside `config.entries[].value` | declaration/invocation order (semantic) |
| `notices` / `doctor.findings` | `(level: warn, info; code; subject; message)` / `(severity: error, warn, info; code; subject)` |
| `open.sessions`, `prune.items` / `list.locks`, `locks.locks` / `config.entries` / `*.config_errors` | `(target)` / `(level, name)` / `(key, scope, layer precedence)` / `(path, line, col, message)` |
| any other array | sorted lexically by canonical JSON of its elements |
| maps, including `Tree.meta` and `meta.meta` | sorted keys |

Byte stability is claimed only after normalising the declared nondeterministic fields: `wt.version`, every `at`/`since`/`started`/`recorded_at`/`removed_at`, `duration_ms`, `log`, `pid`, `tree_id`, `disk_kb`, `cache_kb`, `holder.since`, `last_probe.at`, `last_error.at`.

### 14.6 `shell-init`, the PATH guard, and prompt marker
`wt shell-init <shell>` prints:
```sh
# zsh/bash
if [ -n "$WT_PATH_PREFIX" ] && [ "${PATH#"$WT_PATH_PREFIX:"}" = "$PATH" ]; then PATH="$WT_PATH_PREFIX:$PATH"; export PATH; echo "wt: PATH_NOT_SHADOWED — re-prepended $WT_PATH_PREFIX" >&2; fi
if [ -n "${WT_TARGET:-}" ]; then case "${PS1:-}" in "($WT_TARGET) "*) ;; *) PS1="($WT_TARGET) ${PS1:-}" ;; esac; fi
```
```fish
if set -q WT_PATH_PREFIX
    set -l wt_prefix (string split : -- $WT_PATH_PREFIX)
    set -l n (count $wt_prefix)
    if test (count $PATH) -lt $n; or test "$(string join : -- $PATH[1..$n])" != "$(string join : -- $wt_prefix)"
        set -gx PATH $wt_prefix $PATH
        echo "wt: PATH_NOT_SHADOWED — re-prepended $WT_PATH_PREFIX" >&2
    end
end
if set -q WT_TARGET; and functions -q fish_prompt; and not functions -q __wt_original_fish_prompt
    functions -c fish_prompt __wt_original_fish_prompt
    function fish_prompt
        printf '(%s) ' "$WT_TARGET"
        __wt_original_fish_prompt
    end
end
```
The prompt prefix is inert outside a door because `WT_TARGET` is unset and is guarded against duplicate installation. `wt completions <shell>` completes targets from `wt list --json`; `shell-init` defines no directory-changing or environment-eval helper functions.

### 14.7 `setup` (A76)
`wt setup [path]… [--dry-run] [--shell S]`: a terminal-primary front end that composes `register`, `adopt`, `$WT_HOME/config.toml` writes, and appends to shell and tmux configuration. Those are its whole mutation set; `--dry-run` takes the default answer to every card without asking, ticking every repository found, and prints the run as the commands that would produce it (a settings write, which no shell line reproduces, as a comment naming the key and value), which needs no terminal.

**Discovery.** Walk `$HOME` plus any `path` arguments, breadth-first to depth 6, one readdir per directory, recognising a checkout by a `.git` entry in the parent's readdir and never descending past one. Skip every entry whose name begins with `.` other than `.git` itself, and the names `node_modules`, `target`, `vendor`, `build`, `dist`, `Library`, `Applications`, `Pods`; do not follow symlinks; do not descend past the device of the root a directory was reached from; permission errors are silent. The walk carries a wall-clock budget; expiring it truncates the sweep, which is reported rather than presented as a completed survey. A `.git` directory is a checkout; a `.git` file names its common gitdir, which is a submodule when its path contains `.git/modules` as adjacent components and a linked worktree otherwise; a directory carrying `HEAD`, `objects`, `refs` and `config` without a `.git` entry is a bare or mirror checkout, which is neither offered nor descended. A common-gitdir group holding no checkout offers nothing: a linked worktree whose own checkout lies outside the walk cannot be registered in its place. Candidates group by common gitdir into checkouts, and checkouts group by normalised origin (`user@` and a `.git` suffix stripped, lowercased to `host/org/name`); an originless checkout buckets alone. One parallel batch of `git config --get remote.origin.url` and `git worktree list --porcelain` runs per checkout, never per candidate. Recency is `max(mtime)` over `.git/index` and `.git/logs/HEAD` (a linked worktree's own, under `.git/worktrees/<n>/`).

**Proposals.** A label is proposed from the origin's repository name, disambiguated with its organisation on collision with another proposal or a registered label; an origin that is a local path proposes the checkout's directory name instead. A worktree's name is proposed from its branch, else its directory name. Every proposal is editable in place; an edit is sanitised to what a label or name may be spelled as. Nothing is ticked on the reader's behalf: registering and adopting are opt in. Recency orders the list, most recently touched first (ties by shortest path, then lexically), and collapses everything untouched for 28 days behind one line. Worktrees of an unregistered checkout are shown with the reason they are unavailable rather than omitted.

**Cards** run in order — shell integration, tmux, agent, `trees_dir`, repositories — then one plan and one consent. The repositories card is one line per decision: each unregistered checkout with its linked worktrees beneath it, a label shown only where it differs from the directory name, and checkouts already registered counted rather than listed. A worktree row is enabled exactly while its checkout is ticked or already registered, since it can only be adopted under that label (§11.6). The consent lists the plan as sentences; the shell lines belong to `--dry-run`. Discovery runs concurrently with the cards that do not depend on it. The shell card writes §14.6's `shell-init` line and `completions` into one guarded block in `~/.zshrc`, `~/.bashrc` (bash doors are interactive and non-login, §9.3) or `~/.config/fish/config.fish`, appending only, after a backup, idempotent by its guard, having resolved any symlink and named the real path in its consent. The tmux card reuses §5.4's version resolution, may install tmux through a detected package manager by handing over the terminal, generates a configuration when none exists — naming the terminal wt is running under, whether or not tmux is installed yet — and when one exists reports only the differences wt requires — colour, `extended-keys`, mouse — read from a throwaway `tmux -L` server that resolves the user's own configuration, under a deadline and killed unconditionally. A configuration already on disk when tmux is absent is left for the installed tmux to read; a tmux older than 3.2 is reported, not upgraded. The agent card offers only agents present on `PATH` and is skipped when none is.

## 15. Crates (A18)
```
crates/wt-core   pure: model, config (grammar/merge/scopes/validate/template), adapters, address, coords, ports, env, render::decide,
                 task (graph/plan/lock_plan), resource::step, declarations::reconcile, lifecycle (derive_phase, new::decide, init::decide,
                 remove::plan/revalidate/classify, from_ref, drift), session::name, doctor, report
crates/wt-sys    effects as plain modules, each public function wrapping one syscall, subprocess or file format:
                 git, fsx (store protocol, no-follow open, recursive byte copy, exclude splice), lock (six levels, holders, deadlines), proc (execvp door,
                 `run` parent with tee and timeouts), net (bind+connect probe), tmux, snapshot
crates/wt        binary: cli/ + app/ (door, executor, commands)
```
`wt-core`: no `std::{fs,process,env,net,thread}`, no `SystemTime::now`; dependency allow-list `serde serde_json toml blake3 indexmap thiserror` (clippy disallowed lists + CI manifest check). The binary crate cannot name `std::process::Command` or `std::fs` write/rename/remove (clippy `disallowed_methods`, `-D warnings`) and depends only on `wt-core`, `wt-sys`, `clap`, `serde`, `serde_json`. Long-held locks are fd-owning tokens (`TreeToken`, `GitToken`).

### 15.1 Decision APIs (pure)
`derive_phase(TreeObs)`; `new::decide(EntryView, Request)`; `init::decide`; `render::decide`; `resource::step(Option<Record>, Event)`; `declarations::reconcile(records, declared)`; `remove::classify(GitObs)`, `remove::plan(Obs)`, `remove::revalidate(plan, Obs2)`; `exclude::block`, `exclude::splice`; `deactivate`, `assemble`; `task::plan`, `task::lock_plan`; `session::name(label, name)`; `coords::choose(allocated_ranges, squatted, settings, tombstone)`; `ports::append(map, cfg_ports, stride)`; `drift(diff_names, sync_inputs)`; `settings::validate`.

## 16. Acceptance configs (A9, A15, A22, A31)
All three inputs parse unchanged.
- **orbit**: `register ~/source/orbit` → canonical tree with a slot, `.wt/orbit/config.yaml` rendered (hash-owned, `host_name: <name_short>`), `target/debug` first on PATH inside every door, with `commands = ["orbit"]` shimmed so `orbit` refuses rather than reaching the installed release until the first build (§9.2a); `wt new orbit/feat` → same, own ports and config; `wt run daemon` → `build` → instance frozen with `bin_exes ∋ orbit` → `orbit server start` → probe `orbit list` → present; `wt remove orbit/feat` → `orbit server stop` under the tree's own PATH, dropped, tombstone; `rm -rf` then `prune` → `orphaned(exe_missing)`, installed `orbit` never invoked. The canonical daemon runs beside the installed release (A3).
- **orbitapp**: one port (`WT_PORT_METRO` → `RCT_METRO_PORT`); `wt run ios` → `orbit-src` probes `$WT_ROOT/../orbit/crates/orbit`, links the sibling when absent (a side effect outside the tree, untracked), then `npm run ios`; a later `wt new orbitapp/orbit` reports `PATH_OCCUPIED`.
- **orbitcloud**: seven aliases in `Section__Key` form from `WT_PORT_*`; `.mcp.json` and `.claude/settings.local.json` copied at `new` S3 and excluded; custom `sync` replaces the composite; `pgdata` (no `run`) per the §10.5 example — with Docker stopped its probe exits 2 (`docker info … || exit 2`), so `remove` reports `orphaned(probe_failed)` and stops, `remove --keep-orphans` removes the worktree and leaves the record for `prune --records`; with Docker running, `remove` destroys the Aspire containers/volumes/network named from the path hash and drops the record.

## 17. Test plan
Levels **U** (wt-core), **I** (wt-sys/app on temp repos with shims: tmux, docker, npm, cargo, installed-`orbit` recorder, sleeping git per class, probe shell, probe agent, fd-closing child), **C** (CLI contract). Failpoints (feature `failpoints`, A30.3): `WT_FAILPOINT=<name>[:exit|:sigkill|:pause=<ms>]` at exactly: `new.G` (after `worktree add`), `sync.mid` (between two sync nodes), `remove.8` (after a destroy, before `worktree remove`), `render.write` (after the file write, before the record), `resource.destroyed` (after `DestroyOk`, before the record drop), `resource.frozen` (after the instance freeze, before `run`). Tests reference the owning section and assert its stated outcome; they restate no algorithm.

| Area (owner) | Proof | Level |
|---|---|---|
| §0 | a `wt exec` on a ready tree: subprocess tracer shows ≤ 1 git query (plus `ls-files` on the first render only); no bind; no state write when nothing changed; two flocks; §13.3: each subprocess class with a sleeping shim hits its deadline, lock waits bounded, `list` in a non-ready phase never blocks a concurrent verb | I |
| §5.6, §6.2 (A57) | a needs-only aggregate is valid, runs its needs in plan order, spawns nothing itself, and exits 0; every inert task key is refused by `CONFIG_INVALID` naming the key and that it belongs on a task that runs | U, C |
| §8.1–8.4 | proptest over marker-free parents (incl. pre-set `WT_*`, PATH variants, pre-set aliases, force, task env, two trees) asserting L1, L2 and "effect ⊆ applied keys"; a corrupted marker → `ACTIVATION_IGNORED` and the door proceeds; a user-edited tool-set key is replaced by the next door; `--deactivate --sh` evaluated restores the parent | U, C |
| §8.3 | edited bytes with header+record → `RENDER_ONTO_USER_FILE`; a tracked path → `RENDER_ONTO_TRACKED` at first render and after `sync`; `render.write` failpoint → next door reports row 5 with the `rm` remedy | I |
| §9.1–9.2 | env identical across `env --dotenv`, `exec -- env`, `run` node, probe shell at spawn, tmux probe agent; door file names the exec'd child's pid; `remove` during `exec -- sleep` → `TREE_IN_USE` naming it; fd-closing child releases the lock (documented residual); `--no-gate` outside `$TMUX` → `NO_GATE_REFUSED`; an owned command refuses before its build and execs the tree's binary after it, in one persistent shell (§9.2a); adversarial `run` children (partial JSON, no newline, 10 MB, invalid UTF-8, signal death) → one envelope; passthrough refusals (§9.3) | I, C |
| §9.4 + §11.4 step 5 | attached `open` then `remove --yes` → session killed, lock acquired, removal completes; a `wt shell` in the tree → `TREE_IN_USE` naming the shell; two concurrent `open`s → one session | I |
| §9.4 (A62) | an agent start or resume exits → the pane remains as the configured or default interactive shell with the assembled environment (`WT_TARGET`, tree cwd); a later `open` reports the session already open, attaches when eligible, and starts no second agent; a shell-only session keeps its direct launch argv; a nonexistent agent command → `SESSION_CREATE_FAILED` (7) with pane output and no recorded agent; an agent that runs then exits nonzero → surviving shell | I |
| §10.1 | `lock_plan` order asserted by a lock-order tracer (out-of-order acquisition panics in test builds); task env not contributed; present resource env contributed; log retention keeps 20 per task | I |
| §10.1, §14.1, §14.4 (A58) | argv arguments append after templating and shell arguments appear at `"$@"`, only on the resolved target; an adapter-composed root forwards to the single adapter recipe; a two-scope composite refuses and names the public scoped tasks; a user aggregate refuses regardless of fan-out; no-parameter shell and resource refusals have their specified usage codes and remedies; absent arguments preserve behaviour; aliases accept `--`; log header, dry-run, and JSON carry the exact argument vector and resolved `args_target` (`null` without arguments); arguments without `--` name the delimiter | C |
| §4.1, §11.2, §14.1, §14.4–14.5 (A63) | `new --meta` and prompt-free `meta` set, list and idempotently unset metadata through a registry round trip; invalid keys, oversized values and missing `=` refuse before a write; status text shows the map and Tree/meta JSON carry it with sorted keys | U, C |
| §3.1 (A67) | `phase_3_identity_and_environment_surface_are_exact` asserts linked target derivation, `.` sanitisation, and an `IDENTITY_COLLISION` between display names that sanitise alike; `open_reports_canonical_and_linked_sessions_that_die_during_startup` exercises a `/`-bearing linked session through the capture path | U, I, C |
| §9.4 (A68) | `backend_none_never_invokes_tmux_for_truth_or_teardown` covers none-backend shell entry, `--no-attach`, `--agent`, `--all`, and no tmux effects; `agents_start_only_for_new_sessions_and_open_all_resumes_recorded_agents` proves the canonical anchor is excluded from `open --all` | I, C |
| §11.2 (A69) | `new_starts_detached_build_and_shims_report_progress_and_failure` gates the detached build after the CLI returns and observes live-progress then failure; `dead_build_supervisor_is_abandoned_for_status_doctor_and_shims` fakes a dead pid and proves list/status normalisation, `BUILD_ABANDONED`, and the ordinary shim remedy | I, C |
| §9.1–9.3 (A71) | `edit_is_a_root_cwd_passthrough_door_with_documented_resolution` asserts root cwd, full door env, templated settings command, verbatim `$VISUAL`/`$EDITOR` fallback (including literal `{{`), JSON refusal, and `EDITOR_UNSET` | C |
| §11.5a (A72) | `forget_removes_only_wt_records_and_artifacts_and_requires_consent` and `forget_refuses_resources_sessions_and_door_holders_with_specific_remedies` prove consent/decline, records-only cleanup, retained directory/branch/worktree, tombstoning, and all refusal gates | C, I |
| §14.1, §14.4 (A73) | top-level help asserts its intent groups and primary short spellings; a completeness test checks every clap subcommand appears in the override literal; `ls` and its alias emit canonical envelope command `list`, while JSON snapshots retain `list`/`remove` | C |
| §4, §5.2–5.3, §13.3, §14.4 (A59) | named-lock config parses, validates `slots`/`wait`, is root-only, and merges by name; two holders fill two ordered slot files, a third gets `LOCK_HELD 2/2` naming both holders and both remedies, and a bounded waiter proceeds after release; an absent entry means one slot and the §13.3 default wait (previously unimplemented, now honoured — the announced A59 behaviour change); `wt locks` reports `held n/N` and per-slot holders | U, I, C |
| §4, §5.2, §5.6, §10.2–10.5, §11.4–11.5, §12, §14.5 (A61) | machine-tied validation rejects tree- and repo-specific keys/functions and snapshot stripping removes both sets; two labels declaring one `ScopedTask` share one `_machine.json` record with `label: null`; machine RMW/resource lock paths are used; remove and unregister leave the machine store byte-identical; destroy prompts on a TTY and requires `--yes` without one; refresh works from either label; `list --probe` probes shared repo- and machine-tied `ResourceKey`s once per pass and reuses each result across trees; status/list JSON order resources tree, repo, machine | U, I, C |
| §5.2, §10.1–10.4, §11.4, §12, §14.1, §14.4 (A60) | exclusive grammar is restricted to tree-tied resources; repo and machine arenas serialise holder claims; a second tree gets `RESOURCE_HELD`; `--take` destroys through the holder's frozen snapshot without holding two resource locks, reports the displaced target in text/JSON, and flips the holder; non-holder remove/probe run no recipe, including when its checkout predates the exclusive declaration, and a skip names the holder in `RESOURCE_HELD_BY`; an unrelated label's same-named non-exclusive task is unaffected by another label's machine arena entry; holder teardown and confirmed absence clear ownership; a present unheld resource is adopted as external; stale holders are treated as absent and their arena entries are collected by `prune`; `--take` on a non-exclusive target is a usage error | U, I, C |
| §10.3 | snapshot env = exactly the minimised set (never `WT_ACTIVATION`; parent keys only via `snapshot_env`); teardown env = invoker's env overlaid; A25 scans only the recipe about to run; files 0600 under umask 000; repo-tied env has no tree-specific key; orbit daemon after `rm -rf`: `prune` → `orphaned(exe_missing)`, installed `orbit` never invoked; a recipe without tree words runs with bins removed from PATH; canonical root gone → `repo_root_missing` | I |
| §10.4 | pgdata sequence (declared → run notice → external present → refresh → declared → remove drops absent/destroys present); probe exit 2 → never runs/destroys, teardown → orphaned; `resource.frozen` failpoint → next `run` probes and settles; destroy failure → orphaned, others attempted; declaration deleted after creation → still destroyed from the instance; sibling-scope same-named resources distinct | I, C |
| §10.5 | instance frozen after `needs`, later config edit does not change it; no refresh on a plain door; repo-tied: the invoking tree's stripped declaration is used until an instance exists, then the instance governs | I, C |
| §11.1 | `derive_phase` exhaustive over the table incl. `replaced`, `claimed`, `missing` with records | U |
| §11.2 | `new.G` failpoint → `wt new` resumes once (`resumed: true`); two `new` same address → one `TREE_IN_USE`; crash during V → `--verify` resumes V; `remove` then `new` same name → inherits slot/ports/identities with a fresh `tree_id`; `rm -rf` a tree with a present resource then `new` → `TREE_MISSING_PENDING`; after `prune --records` → fresh incarnation; a foreign directory at the path → `PATH_OCCUPIED`; `sync.mid` failpoint → `interrupted`, `wt sync` resumes, unchanged inputs → no-op (§11.3) | I, C |
| §3.2, §11.4, §14.3–14.4 (A65) | bare name inside its label resolves and removes; the same name outside a tree gets `NOT_FOUND` (3) with fully-qualified candidates; an unknown address gets `NOT_FOUND`; an explicit tombstoned address returns `removed: false` with `ALREADY_REMOVED`; `n` returns `removed: false` with `REMOVE_DECLINED`; every human rendering states the reason; `n` / `TREE_DIRTY` leave phase, op, session untouched; clean, missing and replaced trees do not prompt; a dirty tree prompts on a TTY and is `TREE_DIRTY` without one; `--force` never prompts; a pushed branch is deleted and an unpushed one kept; `remove.8` failpoint → `remove-interrupted`, re-run completes; probe exit 2 → `DESTROY_FAILED`; `--keep-orphans` removes the worktree and leaves a `missing` entry with records; repo-tied instance survives tree removal | C, I |
| §11.5–11.6 | `unregister` runs the canonical teardown inline, closes the canonical session, one tree-tied pass then the repo-tied pass, stops at the failure barrier; `register` → `list` ready/`sync: never`; `register` → doors before any `new`; interrupted init resumes; duplicate path/gitdir refused; §11.7: `COPY_ABSENT`/`COPY_TRACKED`/`COPY_EXISTS`, copied file never re-rendered/deleted, excluded from `git status` | I, C |
| §7 | disjoint ranges (proptest over geometry incl. `stride 0`/overflow rejection); tombstone ranges avoided; `IDENTITY_COLLISION`; appended port seen by the allocating door's child; reordered `ports` changes nothing; removed name keeps its index; `PORTS_EXHAUSTED`; settings change leaves live `wt env` unchanged | U, I, C |
| §4.1–4.4 | invariants incl. path uniqueness and no live/tombstone coexistence; `register` twice same label → `registered: false`; other label → `PATH_REGISTERED`; store crash at the rename boundary leaves the old file; `HOME_OLD_FORMAT` before any write; `TREE_REPLACED` on every verb for a replaced directory; `STATE_ORPHAN` collected by `prune` | U, I, C |
| §12 | `list` reports `drift` when the default branch changed a sync input; `prune` tombstone collection; `prune --records` on `missing`/`replaced`/`remove-interrupted` never touches the directory (spawn tracer: no cwd or PATH inside it); non-TTY `prune` without `--yes` → exit 0, `applied: false`; `NO_COORDINATION` for a config without ports/env/resources; `close` idempotent JSON | I, C |
| §14.4–14.5 | every envelope-producing verb's `--json` validates; ordering on raw output (a test walks the schema for unlisted arrays); bytes compared after normalisation; §14.6: emitted zsh, bash, and fish shell-init strings are asserted to contain the PATH guard and target prompt machinery (no test sources the fish script) | C |
| R1–R13, A1–A63 | R12/A2: filesystem allowlist snapshot around `new` + doors (only `<tree>`, `<tree>/.wt`, declared materialisations, `$WT_HOME`, `<commondir>/{info/exclude,worktrees/*,refs/wt/*,FETCH_HEAD,objects/*}`), `git status` clean, tracked bytes unchanged, `unregister` leaves the checkout clean or reports `ARTIFACT_KEPT`; each requirement maps to the rows above by its owning section; A9: the three inputs parse byte-for-byte; golden `tasks --json`; orbitcloud recipes run verbatim against the docker shim asserting the path hash and the exit-2 reachability guard | U, I, C |

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
| §2, §12, §13.2 | §11.5a | `forget` records-only teardown, refusal gates, consent, and tombstoning without directory or branch removal |
| §7, §11.2 R, §11.4 step 11, §11.6 R, §12 | §4.3 | state-file rule: written at R, deleted at tombstoning, orphans collected |
| §0 | §9.1, §8.3, §10.5, §12, §13.3 | the door ceiling; mechanisms moved off the hot path; every wait class has a deadline row |
