# Cookbook

These patterns cover integrations that need a little policy around the normal
door model.

## Guard unsafe repository tools

If a repository command is unsafe in linked worktrees, claim its name and
render a guard into a declared binary directory:

```toml
commands = ["deploy"]
bin = [".wt/policy-bin"]

[files.".wt/policy-bin/deploy"]
mode = "0755"
content = '''#!/bin/sh
if [ "$WT_ROOT" != "$WT_REPO" ]; then
  echo "deploy is allowed only from the canonical checkout" >&2
  exit 2
fi
exec /usr/local/bin/deploy "$@"
'''
```

The recipe must call the real tool by absolute path, or it will recurse into
the claimed name. This guard applies inside wt doors; it cannot cover a bare
terminal opened directly in a checkout.

## Wrap an agent

An agent wrapper can add flags, logging, or pane-aware behavior while keeping
the agent declaration small:

```sh
#!/bin/sh
printf 'starting in pane %s\n' "${TMUX_PANE:-outside-tmux}" >&2
exec /opt/tools/my-agent "$@"
```

Declare the wrapper as an agent's `start` and `resume` command in
`$WT_HOME/config.toml`. Use tmux's `$TMUX_PANE` when the wrapper must address
its own pane; `WT_SESSION` no longer exists. Prefer an absolute path for a
machine-installed wrapper, or render a per-tree wrapper with `files` and use a
templated `{{root()}}` path.

```toml
[agents.team]
start = ["/opt/tools/wt-agent-wrapper", "start"]
resume = ["/opt/tools/wt-agent-wrapper", "resume"]
```

## Editors

Set a string or argv command in `$WT_HOME/config.toml`; `wt edit` falls back to
`$VISUAL` and then `$EDITOR` when this key is absent:

```toml
editor = ["code", "--new-window", "{{root()}}"]
```

`wt edit project/feature` launches at the tree root with the complete door
environment. A cold GUI launch inherits it, but a GUI CLI that forwards to an
already-running application cannot replace that application's environment.
Use `wt exec <target> -- ...` in editor run configurations and a `wt shell`
terminal profile for integrated terminals. Repository-owned editor files can
be rendered per tree with `files`, for example a `.vscode/settings.json` whose
content uses `{{root()}}`, private vars, or declared ports.

The stable and mechanism environment tiers are listed in
[Environment rules](../README.md#environment-rules).
