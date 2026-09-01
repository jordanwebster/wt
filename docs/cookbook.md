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

## Environment tiers

| Tier | Variables | Change policy |
| --- | --- | --- |
| Interface | `WT_TARGET`, `WT_LABEL`, `WT_NAME`, `WT_ROOT`, `WT_REPO`, `WT_HOME`, `WT_BRANCH`, and `WT_SELF` inside a resource task | Stable scripting interface; changes are announced deliberately. |
| Mechanism | `WT_ACTIVATION`, `WT_PATH_PREFIX`, `WT_BIN`, `WT_TASK` | Internal door/task plumbing; may change as the mechanism evolves. |

Name transformations belong in template functions such as `{{name_snake()}}`;
`WT_SESSION`, `WT_NAME_SNAKE`, `WT_NAME_SHORT`, and `WT_SLOT` are not
exported. See [Environment rules](../README.md#environment-rules) for the
full rules, including why ports are configuration inputs rather than
environment.

## Recipes that contain `{{ }}`

`{{` always begins a wt template expression, so a Go template written straight
into a recipe — the kind `docker`, `kubectl`, and `gh` take after `--format` —
is a configuration error. Set `template = false` on the task to hand the whole
recipe to the shell verbatim:

```toml
[task.serve]
tied_to = "tree"
template = false
run = 'docker run -d --name "$WT_SELF" myapp'
exists = 'docker inspect -f "{{.State.Running}}" "$WT_SELF" 2>/dev/null | grep -q true'
destroy = 'docker rm -f "$WT_SELF"'
```

The opt-out covers `run`, `exists`, and `destroy`, in string or argv form. It
does not cover the task's `name` or `env`, which is how an untemplated recipe
still receives wt's values: `$WT_SELF` is the resolved resource name, and a
declared port reaches the recipe through an `env` entry.

```toml
[task.serve.env]
APP_PORT = "{{ports.http}}"
```

A recipe that needs no braces of its own needs none of this — keep templating
on and write `{{ports.http}}` directly.

## Find out where the time went

With the timing log enabled, wt appends a line per event to
`$WT_HOME/logs/wt.jsonl`: one for each child process it runs, one for each
lock that made it wait, and one closing each command with its total.
Everything carries a duration in `ms` and the `run` id of the invocation that
produced it, so once the log is on, a slow command can be taken apart after
the fact — no flag to remember per command. Turn it on in
`$WT_HOME/config.toml`:

```toml
[logs]
trace = true
```

```sh
# where the last `wt ls` spent its time
run=$(jq -r 'select(.kind=="cmd" and .name=="list").run' ~/.wt/logs/wt.jsonl | tail -1)
jq -r --arg run "$run" \
  'select(.run==$run and .kind=="child")|"\(.ms)\t\(.name) \(.op // "")"' \
  ~/.wt/logs/wt.jsonl | sort -rn | head
```

Recipe text never appears in the log; a task is named by its id. The log is
off by default; `trace = false` turns it back off.
