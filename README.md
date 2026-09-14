# wt

`wt` creates isolated Git worktrees and owns the state that must be prepared,
shared, or kept alive around them. Your repository's own command runner remains
the place where work is named. `wt with` supplies durable resources and bounded
pool leases while that work executes.

```sh
wt register ~/source/sample
wt new sample/fix-scroll
wt with database -- just check
wt rm sample/fix-scroll
```

Inside a tree, `wt exec`, `wt shell`, `wt open`, and the shell integration expose
the same rendered environment, ports, files, copied inputs, and tree-local
binaries. A leased value exists only for its child and is exported as
`WT_LEASE_<POOL>`.

## Configuration

Put repository defaults in `.wt.toml`. User overrides live below
`[repos.<label>]` in `$WT_HOME/config.toml`; a tree may add `.wt/config.toml`.

```toml
ports = ["http"]
bin = ["bin"]
commands = ["samplectl"]
output = [
  { path = ".cache/typecheck" },
  { path = "node_modules" },
]

[env]
APP_PORT = "{{ports.http}}"

[files.".wt/application.env"]
marker = "#"
content = "PORT={{ports.http}}"

[hooks]
after_new = "just compile"
warm = "just warm"
```

Configured output roots are opaque directories. A new tree starts with a
copy-on-write clone when the filesystem supports it, `wt ls --disk` includes
their size, and wt never inspects or sweeps their contents. Adapter-owned roots
may have an ecosystem-specific sweeper.

`after_new` starts detached in each new tree, writes `.wt/logs/wt-setup.log`, and
gates claimed command shims until it succeeds or fails. `warm` runs in the
canonical checkout when the anchor refreshes it. The anchor is triggered by
`new`, `rm`, and `sync` when the effective configuration has output roots.

## Resources

A resource has an owner and a lifecycle:

```toml
[resource.database]
tied_to = "tree"
needs = ["network"]
name = "sample_{{name_snake()}}"
exists = 'test -f ".wt/$WT_SELF.ready"'
create = 'touch ".wt/$WT_SELF.ready"'
destroy = 'rm -f ".wt/$WT_SELF.ready"'
```

`tied_to` is `tree`, `repo`, or `machine`. Tree instances are destroyed by
`wt rm`; repository instances by `wt unregister`; machine instances survive
until explicitly destroyed or pruned after their declaration disappears.
Lifecycle recipes receive the frozen instance name in `$WT_SELF`, and they
run from a checkout, so a recipe may be one of the repository's own scripts:
a tree resource's from its tree, a repo resource's from the registered
checkout, and a machine resource's from the checkout that invoked it.

```sh
wt with database                 # ensure durable state
wt destroy database --yes
wt refresh database --yes
```

`needs` may name other resources. A resource can occupy a bounded arena while
it exists:

```toml
[resource.web]
tied_to = "tree"
arena = "repo"
capacity = 1
name = "sample-web-{{name_short()}}"
exists = 'test -f ".wt/$WT_SELF.ready"'
create = 'touch ".wt/$WT_SELF.ready"'
destroy = 'rm -f ".wt/$WT_SELF.ready"'
```

When an arena is full, `--take` may evict persistent tree- or resource-held
state through its frozen destroy snapshot. A running child is never evicted.

## Pools and leases

A recipe-free counted resource is a token pool:

```toml
[resource.container]
tied_to = "machine"
count = 4
wait = "2m"
```

Use it only with a command:

```sh
wt with container -- just integration
```

For several pools, wt acquires one slot from each in sorted pool-name order.
The child's output is tee'd to `.wt/logs/`; all slots are released on success,
failure, or termination. Naming one pool twice is an error.

A counted resource with recipes is an instance pool:

```toml
[resource.device]
tied_to = "machine"
count = 2
name = "orbit-device-{{index()}}"
exists = 'scripts/device exists "$WT_SELF"'
create = 'scripts/device create "$WT_SELF"'
acquire = 'scripts/device clean-and-start "$WT_SELF"'
destroy = 'scripts/device delete "$WT_SELF"'
keep = "2m"
idle = "30m"
wait = "10m"
```

`acquire` runs for every lease. After a command exits, its tree keeps the
instance for `keep`, so its next command gets the same instance without
queueing. Other trees wait for that reservation to expire. A free instance is
destroyed after `idle`; a later lease creates it again. Instances are addressed
from one, for example `wt destroy device#1 --yes`. `wt rm` ends that tree's
keeps. `wt leases --json` reports each instance as free, kept with its tree and
deadline, or leased with pid and process start time.

Machine pool recipes must agree across repositories. Machine sizing belongs in
the user settings and overrides repository suggestions:

```toml
[resources.device]
count = 3
keep = "45m"
idle = "30m"
wait = "15m"
```

## Everyday commands

```sh
wt ls --disk
wt status sample/fix-scroll
wt config sample/fix-scroll --origin
wt env sample/fix-scroll --sh
wt exec sample/fix-scroll -- just check
wt shell sample/fix-scroll
wt open sample/fix-scroll
wt doctor
wt prune --yes
```

All control-plane commands support the stable error classes and JSON envelopes
described in the specification. Passthrough doors retain their direct stdio
contract and therefore do not emit envelopes.

See [the cookbook](docs/cookbook.md) for complete patterns and
[the specification](spec/SPEC.md) for lifecycle and failure semantics.
