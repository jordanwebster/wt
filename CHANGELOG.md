# Changelog

## 0.6.0

wt is rewritten as a small library with a CLI over it, so other programs
can make trees and run things in them without shelling out.

**Upgrading from 0.5:** run `wt migrate` once to import your trees, keeping
their names and ports, then convert each repository's `.wt.toml` to the
new format. A key that was removed names what replaces it.

- **Trees:** `wt new` makes a worktree from origin's default branch (fetched
  first, falling back to the copy fetched last) and returns as soon as it
  exists; copying inputs and the create hook carry on in the background.
  Each tree gets named ports, an environment, rendered files, copied inputs
  and `[bin]` wrappers that run the tree's own build.
- **Pools:** `wt with pool -- cmd` leases one of a few shared things, such
  as simulators, and `wt leases` shows who has what. Idle slots are retired.
- **In use:** `wt shell` and `wt exec` hold the tree while their command
  runs, as can a program using the library. `wt rm` refuses a tree in use
  without `--force`, and a program can stop a tree once nothing holds it.
- **Pinned:** `wt start` pins a tree running; `wt stop` unpins it and stops
  it at once.
- **One hook at a time:** a tree's start, stop and removal wait for a create
  hook still building, even after `wt new` has returned.
- **Time limits:** start, stop and delete hooks are killed with their
  process group after 5 minutes, or `[lifecycle] start_timeout`,
  `stop_timeout` and `delete_timeout`.
- **Removal:** a failing stop or delete hook is reported and removal goes
  on. A tree already stopped isn't stopped again.
- **Owners:** a tree made by another program records which, and `wt ls`
  shows the owner, and whether a tree is pinned or in use.
- **Kept files:** `[files."<path>"] keep = true` writes a file only when it
  is missing, for a file that is also state the tree edits.
- **Unknown keys:** a config key wt doesn't know is ignored with a warning
  rather than refused, so a config using a key from a newer wt still works.
  (wt 0.5 still refuses one: upgrade every copy of wt to 0.6.)
