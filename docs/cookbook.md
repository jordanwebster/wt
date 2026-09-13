# wt cookbook

These examples use neutral fixture names. Recipes are ordinary shell commands;
replace them with the repository's own scripts.

## Prepare a tree-owned service

```toml
[resource.service]
tied_to = "tree"
name = "fixture-{{name_short()}}"
exists = 'test -S ".wt/$WT_SELF.sock"'
create = 'scripts/service start "$WT_SELF"'
destroy = 'scripts/service stop "$WT_SELF"'
```

```sh
wt with service
just develop
```

The ensure form is idempotent. Removing the tree executes the frozen destroy
recipe even if the declaration later changes.

## Wrap one command with a semaphore

```toml
[resource.worker]
tied_to = "machine"
count = 4
wait = "5m"
```

```sh
wt with worker -- just integration
```

The child receives a value such as `worker-2` in `WT_LEASE_WORKER`. Its stdout
and stderr remain visible and are also captured below `.wt/logs/`.

## Lease several pools without deadlock

```sh
wt with browser database-slot worker -- just end-to-end
```

The names may be written in any order. wt sorts them before acquiring, takes
one slot per distinct pool, and applies each pool's own wait bound.

## Keep reusable instances warm

```toml
[resource.browser]
tied_to = "machine"
count = 3
name = "fixture-browser-{{index()}}"
exists = 'scripts/browser exists "$WT_SELF"'
create = 'scripts/browser create "$WT_SELF"'
acquire = 'scripts/browser clean-and-start "$WT_SELF"'
destroy = 'scripts/browser delete "$WT_SELF"'
keep = "2m"
idle = "30m"
wait = "8m"
```

```sh
wt with browser -- just screenshots
wt leases
wt destroy browser#2 --yes
```

The clean-and-start operation belongs in `acquire` because it runs on every
lease, including consecutive uses from the same tree. A short `keep` is an
automatic reservation for that tree; no explicit hold command is needed.
After it expires the instance is free, and after `idle` it is destroyed.

## Bound a tree-owned singleton

```toml
[resource.frontend]
tied_to = "tree"
arena = "repo"
capacity = 1
name = "fixture-front-{{name_short()}}"
exists = 'test -f ".wt/$WT_SELF.ready"'
create = 'scripts/front start "$WT_SELF"'
destroy = 'scripts/front stop "$WT_SELF"'
```

```sh
wt with frontend
wt with frontend --take
```

The second form may destroy a persistent holder. It will not take a slot from
a running process.

## Share repository and machine state

Use `tied_to = "repo"` for one dependency cache or database shared by all trees
of a repository. Use `tied_to = "machine"` for named instances shared across
repositories. Shared recipes must not depend on tree-specific template values.

For a machine pool, put operator sizing in `$WT_HOME/config.toml`:

```toml
[resources.browser]
count = 5
keep = "1h"
idle = "30m"
wait = "15m"
```

Repository declarations still own lifecycle recipes. If two repositories
disagree about those recipes, configuration loading fails and names both.

## Warm cloned output

```toml
output = [
  { path = ".cache/analyser" },
  { path = "vendor/generated" },
]

[hooks]
after_new = "just compile"
warm = "just prepare-seed"
```

These output directories are cloned into a new tree and counted by `wt ls
--disk`. Their layout is opaque, so wt never deletes entries within them.
`after_new` runs detached in the new tree; `warm` runs in the canonical through
the anchor.

## Portable wrappers

A repository can keep CI independent of wt while requiring leases locally:

```sh
#!/bin/sh
if command -v wt >/dev/null 2>&1 && test -n "$WT_TARGET"; then
  exec wt with "$@"
fi
while test "$1" != "--"; do shift; done
shift
exec "$@"
```

Scripts that require a pool should fail loudly when `WT_TARGET` is set but the
corresponding `WT_LEASE_*` value is absent. Outside a managed tree they may use
a deterministic CI instance name and the same preparation routine.
