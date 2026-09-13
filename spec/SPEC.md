# wt specification

This document specifies the current resource-and-lease model. Historical
requirements remain in `requirements-addendum.md`; A83 supersedes A57–A61.

## 0. Purpose and boundaries

wt owns worktree identity, materialised environment, seed-derived output,
durable resources, and leases. A repository's command runner owns work. The
boundary is `wt with <resource>... [-- <command>...]`.

Control-plane operations are bounded. A child supplied after `--` keeps its
own runtime semantics. Passthrough doors preserve direct stdio.

## 1. Home and registry

`$WT_HOME` contains the registry, settings, state, advisory lease files, and
managed repositories. Per-child logs live below the operated tree's
`.wt/logs/` directory. Registry addresses are `<label>` for a canonical and
`<label>/<name>` for a linked tree. Paths and identifiers are validated before
mutation. Atomic writes preserve the last complete state.

## 2. Trees and identity

`register` records a canonical checkout. `new` creates or resumes a linked Git
worktree. `adopt` records an existing worktree. `rm` tears down tree-owned
resources, ends keeps owned by the tree, closes its session, removes the Git
worktree, and records a tombstone. `forget` removes wt records without deleting
the worktree. `unregister` additionally tears down repository-owned resources.

Tree identity includes a stable tree id, label, name, branch, slot, frozen port
geometry, allocated ports, source, and user metadata. The `.wt/tree_id`
marker must agree with the registry before a door or destructive operation
uses the path.

## 3. Configuration layers

Configuration merges in order: adapter, repository `.wt.toml`, user
`[repos.<label>]`, tree `.wt/config.toml`. Maps override by key and `false`
deletes supported map entries. Arrays use their documented merge rule.
`wt config --origin` reports the effective value and source layer.

Unknown keys are refused. Removed work-graph and coordination spellings are
also refused as `CONFIG_INVALID`, with a message naming resources, token pools,
hooks, `create`, `acquire`, or `arena` plus `capacity` as appropriate. There is
no compatibility mode. In the settings file, recipe timeouts use `[recipes]`
and internal coordination waits use `[waits]`.

## 4. Environment and materialisation

Every door assembles the same identity variables, configured environment,
ports, rendered files, copied inputs, and PATH. Per-tree facts should be
rendered into files when desktop processes need them. Per-lease facts exist
only in the leased child's environment.

Lifecycle recipes receive `WT_SELF`, the frozen instance name. Repo-owned
recipes cannot reference tree-specific values. Machine-owned recipes cannot
reference tree- or repository-specific values.

## 5. Configuration schema

Root and directory scopes support `bin`, `commands`, `vars`, `env`, `copy`,
`files`, `adapters`, and `resource`. Repository configuration also supports
`ports`, `sync_inputs`, `branch`, `detect`, `output`, and `hooks`.

### 5.1 Hooks

`hooks.after_new` is an optional command for the new tree. `hooks.warm` is an
optional command for the canonical anchor. Adapters may supply defaults only
when the effective configuration does not declare one.

### 5.2 Resources

`[resource.<name>]` accepts:

- ownership: `tied_to = "tree" | "repo" | "machine"`;
- lifecycle: `exists`, `create`, `destroy`, `acquire`;
- graph: resource-only `needs`;
- identity and execution: `name`, `cwd`, `env`, `timeout`, `ready_within`,
  `template`, `snapshot_env`, `description`;
- bounded ownership: `arena`, `capacity`;
- pooling: `count`, `wait`, `keep`, `idle`.

`destroy` requires `exists` and `tied_to`. `ready_within` requires `exists`.
`arena` and `capacity` appear together and capacity is positive. `count` is
positive. Durations use a non-negative integer plus `ms`, `s`, `m`, or `h`.

A declaration with `count` and no lifecycle recipes is a token pool. It may
carry `wait` and machine ownership, but not instance identity, recipes, `keep`,
or `idle`.

### 5.3 Machine sizing and agreement

Machine pools share one name across labels. All declarations of that name must
agree on `name`, `exists`, `create`, `destroy`, and `acquire`. Without
an operator override, their `count`, `keep`, `idle`, and `wait` also agree.
`[resources.<name>]` at settings root may override those four sizing values and
wins over every repository suggestion.

### 5.4 Templates

Resource names support the ordinary closed template functions. Counted
instances additionally support one-based `{{index()}}`. Recipes use `$WT_SELF`
for the resulting name rather than interpolating a separate name placeholder.

## 6. Adapters

### 6.1 Contributions

Adapters detect ecosystems and may contribute environment, claimed commands,
synchronisation inputs and a private synchronisation command, requirements,
nudges, output roots, and hook defaults. They do not contribute repository work
commands or a public work graph.

Configured output roots default to kind `opaque`. Adapter roots may name an
ecosystem strategy such as Cargo. An adapter root is relative to its detected
scope; a configured root is relative to the tree.

## 7. Ports and slots

Ports are stable per tree incarnation. Settings define base and stride; each
tree freezes its geometry and allocates named offsets append-only within the
stride. Allocation refuses collisions with registered trees, tombstones, and
ports already bound on the machine.

## 8. Assembly

Assembly evaluates templates, renders owned files atomically, refreshes copied
inputs, maintains Git excludes for generated material, and constructs PATH.
Claimed command shims live ahead of external executables and never silently
fall through while `after_new` is running or failed.

## 9. Doors and hooks

`exec`, `shell`, `open`, `edit`, and environment inspection enter a tree door.
Doors validate identity, assemble material, and hold a shared tree lock for
their necessary lifetime.

After `new` has made the tree ready, `after_new` starts detached in that tree.
It writes `.wt/build.status` and `.wt/logs/wt-setup.log`; claimed shims wait for
the running status and refuse after failure. The hook runs even when session
creation is skipped. The `--no-after-new` option suppresses it for that one
creation request.

The anchor runs `warm` in the canonical. Its status covers the canonical HEAD
used to decide whether seed output is current.

## 10. Resource lifecycle

Declarations and instance snapshots are distinct. Before first creation, wt
stores the declaration. Immediately before an effect it freezes the expanded
name, commands, relevant environment, roots, and binary inventory. Destruction
uses that snapshot even after configuration changes.

Ensure probes `exists`. Present state succeeds without creation. Absent state
runs `create`, optionally polls until `ready_within`, and confirms presence.
Probe infrastructure failure never triggers a lifecycle effect. Dependencies
are ensured in stable topological order.

Tree records live in tree state. Repository and machine records live in
`_repo.json` and `_machine.json`. The state schema is 2. Reading schema 1
rewrites it to schema 2, preserves every resource instance snapshot, and
serialises former single-slot holders as arena holders.

## 11. Lifecycle commands

### 11.1 Registration and removal

Lifecycle mutations revalidate their observations after consent and locks.
Teardown is idempotent. A missing executable required by a frozen destroy
snapshot leaves an orphan record rather than invoking an unrelated binary.

### 11.2 New

`new` resolves its source, creates the Git worktree, records identity,
materialises configuration, clones output roots, optionally synchronises, marks
the tree ready, starts `after_new`, provisions a session when requested, and
triggers the anchor when effective output exists.

### 11.3 Sync

`sync` reruns only the adapter synchronisation commands whose tracked
`sync_inputs` changed, then rematerialises and triggers the anchor.

### 11.4 Remove

`rm` refuses unapproved loss, closes sessions, destroys tree-owned resources
from snapshots, ends every keep owned by the tree, removes the Git worktree,
deletes tree state, and records a tombstone. `--keep-orphans` permits removal
when a resource cannot be destroyed while retaining its record for repair.

### 11.5 Resource commands

`wt with <resource>...` with no child ensures only durable resources. A token
pool requires a child. `destroy` accepts an indexed name such as `device#1`;
`refresh` destroys and ensures again. Destructive resource commands require
consent and support JSON envelopes.

### 11.6 Leased command

`wt with <resource>... -- <command>...` resolves every name once and rejects
duplicates. Durable dependencies are ensured. Pools are sorted by name and one
slot from each is acquired in that order, each bounded by its own `wait`.
`WT_LEASE_<NORMALIZED_POOL>` exports the chosen instance name. Output is tee'd
to `.wt/logs/`. Leases are released when the child succeeds, fails, cannot be
spawned, or is terminated. JSON retains the stable error class and child
status.

### 11.7 Instance selection, keeping, and idle teardown

Each counted instance has a one-based resource key and its own snapshot. Lease
selection is: an instance kept for this tree, a free present instance, a new
instance when fewer than `count` exist, then wait. `create` runs when another
instance is needed and `acquire` runs at the start of every lease.

Release records its wall time and reserves the instance for that tree until
`keep` expires. Other trees wait for the reservation just as they wait for
running work, bounded by the pool's `wait`; `LEASE_HELD` distinguishes work
from a reservation and names the keeping tree. Removing a tree ends its keeps.
A free present instance older than `idle` is destroyed from its frozen snapshot.
Ordinary invocations schedule that teardown detached; `doctor` and `prune`
await it. An instance beyond the current count or absent from declarations is a
doctor finding and `prune --yes` destroys it. A dead process record is identified
by pid plus process start time, reported, and treated as released into a keep.
There is no dormant instance state and no explicit tree-hold command.

### 11.8 Output cloning

Every effective output root is a directory cloned whole from the canonical at
the output-copy stage of `new`. Copy-on-write is required when supported; wt
does not fall back to a potentially huge byte copy. A missing source leaves the
tree cold. Configured roots are opaque and may contain absolute paths wt cannot
repair.

### 11.9 Output sweeping

Opaque roots are never swept. Adapter strategies may sweep only layouts they
understand. Cargo sweeping derives liveness from metadata, fingerprints,
dep-info, and live dependency edges, holds Cargo's directory lock, and refuses
to delete when it cannot identify workspace member units. `prune` may schedule
these strategy-specific sweeps. Hook and arbitrary child completion do not
grant permission to inspect opaque output.

### 11.10 Anchor

The canonical anchor is triggered after `new`, `rm`, and `sync` when effective
output roots exist. One non-blocking anchor lock exists per label. The anchor
may fetch and fast-forward only a safe default branch, then invokes `warm` and
records the HEAD it warmed. `doctor` reports `ANCHOR_COLD` when output exists,
`warm` exists, and the recorded successful HEAD is not current.

## 12. Truth, pruning, and JSON

`ls`, `status`, `config`, `leases`, `doctor`, and `prune` are truth surfaces.
`ls --disk` reports the whole tree and effective output roots using `ls --disk`
semantics so clone-shared extents are represented consistently with the host.
`leases` reports every instance name, presence, release time, and hold: free,
kept with its tree and deadline, or leased with pid and process start time.

`doctor` reports missing or replaced trees, corrupt state, orphaned or
undeclared resources, idle teardown failures, dead lease holders, broken
shims, cold anchors, and relevant tooling findings. It does not invent findings
for a healthy migrated state. Idle expiry is eligible on every invocation;
`doctor` and `prune` await that cleanup so failure is part of their result.
`prune` reports other destructive work before acting; with `--yes` it collects
stale tree records, undeclared instances, and safe adapter sweep candidates.

All non-passthrough commands accept `--json` and produce one envelope with
`ok`, `command`, `wt`, `data`, `notices`, and on failure `error`. `with` and
`leases` use the same envelope and exit classes.

| Exit | Class | Typical codes |
|---:|---|---|
| 0 | ok | success and idempotent no-op |
| 1 | internal | invariant or local I/O failure |
| 2 | usage | `CONFIRM_REQUIRED`, `TOKEN_COMMAND_REQUIRED`, duplicate input |
| 3 | not found | unknown tree, resource, or pool |
| 4 | conflict | held tree, arena, or lease; occupied path |
| 5 | state | `CONFIG_INVALID`, corrupt state, missing tool or identity |
| 6 | child failed | creation, acquisition, hook, leased child, or teardown failed |
| 7 | external | Git or session backend failed |
| 8 | timeout | subprocess or state-lock deadline expired |

## 13. Locks, arenas, and leases

Lock order is anchor, tree, lease, repository Git, resource, named leaf, registry
RMW, state RMW. Multiple pool leases share the lease level and are acquired in
sorted pool-name order. No path acquires a lower level while holding a higher
one.

A hold is one of:

- process: a live flock held around one child;
- tree: a temporary reservation created when a child exits and removed when
  its `keep` deadline expires or the tree is removed;
- resource: an arena slot held while a material instance exists.

An arena is repository- or machine-scoped and admits `capacity` holders of the
same resource declaration. Contention waits, bounded by `wait`, when the hold
ends on its own: either running work or a keep. A timeout distinguishes work
from a reservation and names the reserving tree. A resource-held slot does not
end on its own, so it conflicts immediately; `--take` may destroy that resource
from its frozen snapshot. A process flock is never taken.

Default waits are bounded. Pool `wait` applies per pool; sorted acquisition
prevents cycles. Resource, Git, tree, and RMW waits use settings defaults.
Only an explicit `forever` pool wait is unbounded.

## 14. Command surface

Primary commands are `register`, `unregister`, `clone`, `new`, `adopt`, `setup`,
`rm`, `forget`, `sync`, `anchor`, `ls`, `status`, `meta`, `prune`, `with`,
`destroy`, `refresh`, `leases`, `exec`, `edit`, `shell`, `env`, `open`, `close`,
`path`, `which`, `config`, `doctor`, `shell-init`, and `completions`. `list`
aliases `ls`; `remove` aliases `rm`. Unknown subcommands exit with usage status
and nearest current names.
