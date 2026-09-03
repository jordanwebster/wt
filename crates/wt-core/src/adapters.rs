use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::{
    config::{AdapterChoice, Command, Config, Task, ValueOrFalse},
    error::CoreError,
    model::{RelDir, RelPath},
    task::{Node, Origin},
    ExitClass,
};

const TABLES: [(&str, &str); 6] = [
    ("cargo", include_str!("../adapters/rust.toml")),
    ("node", include_str!("../adapters/node.toml")),
    ("dotnet", include_str!("../adapters/dotnet.toml")),
    ("python", include_str!("../adapters/python.toml")),
    ("go", include_str!("../adapters/go.toml")),
    ("submodules", include_str!("../adapters/submodules.toml")),
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adapter {
    pub name: String,
    pub detect: Vec<String>,
    pub default_tool: String,
    #[serde(default)]
    pub nudge: Vec<Nudge>,
    pub tools: IndexMap<String, Tool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nudge {
    pub if_tool: Option<String>,
    pub want: String,
    pub hint: String,
    #[serde(default)]
    pub used_if_env: Vec<String>,
    /// Machine-level config files that also activate the accelerator, for
    /// tools that read their own config rather than an environment variable
    /// (cargo's `rustc-wrapper` in `~/.cargo/config.toml`). Same rule shape
    /// as tool sniffs; the app layer supplies the file contents.
    #[serde(default)]
    pub used_if_file: Vec<Sniff>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tool {
    pub lockfile: Vec<String>,
    pub sniff: Vec<Sniff>,
    pub requires: Option<String>,
    pub sync_inputs: Vec<String>,
    /// Directories a new tree clones copy-on-write from the canonical
    /// checkout, when the filesystem can (§11.8).
    pub seed: Vec<String>,
    pub env: IndexMap<String, String>,
    pub commands: Vec<String>,
    pub task: IndexMap<String, Task>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sniff {
    pub file: String,
    pub toml_key: Option<String>,
    pub contains: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DirSnapshot {
    pub dir: String,
    pub names: BTreeSet<String>,
    pub contents: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterHit {
    pub adapter: String,
    pub tool: String,
    pub dir: String,
    pub notice: Option<String>,
    pub sync_override: Option<Command>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AdapterContribution {
    pub env: IndexMap<String, String>,
    pub commands: Vec<String>,
    pub sync_inputs: Vec<String>,
    pub seed: Vec<String>,
    pub requirements: Vec<String>,
    pub nudges: Vec<Nudge>,
    pub selected_tools: BTreeSet<String>,
}

pub fn builtins() -> Result<Vec<Adapter>, CoreError> {
    TABLES
        .iter()
        .map(|(id, source)| {
            toml::from_str(source).map_err(|error| {
                CoreError::new(
                    ExitClass::Internal,
                    "ADAPTER_INVALID",
                    format!("built-in adapter `{id}` is invalid: {error}"),
                    "reinstall a valid wt build",
                )
            })
        })
        .collect()
}

pub fn detect(
    snapshot: &DirSnapshot,
    overrides: &BTreeMap<String, AdapterChoice>,
) -> Result<Vec<AdapterHit>, CoreError> {
    let mut hits = Vec::new();
    let adapters = builtins()?;
    for adapter in adapters
        .iter()
        .filter(|adapter| adapter.name != "submodules")
    {
        if overrides
            .get(&adapter.name)
            .is_some_and(|choice| choice.disabled == Some(true))
        {
            continue;
        }
        if !adapter
            .detect
            .iter()
            .any(|pattern| matches_name(pattern, &snapshot.names))
        {
            continue;
        }
        let tool = overrides
            .get(&adapter.name)
            .and_then(|choice| choice.tool.clone())
            .or_else(|| select_lockfile(adapter, snapshot))
            .or_else(|| select_sniff(adapter, snapshot))
            .unwrap_or_else(|| adapter.default_tool.clone());
        if !adapter.tools.contains_key(&tool) {
            return Err(CoreError::new(
                ExitClass::State,
                "CONFIG_INVALID",
                format!("adapter `{}` has no tool `{tool}`", adapter.name),
                "choose a tool listed by `wt config`",
            ));
        }
        let notice = (adapter.name == "node"
            && tool == "npm"
            && !snapshot.names.contains("package-lock.json")
            && !snapshot.names.contains("npm-shrinkwrap.json"))
        .then(|| "NO_LOCKFILE".to_owned());
        let sync_override = if adapter.name == "node" && tool == "npm" && notice.is_some() {
            Some(Command::Shell("npm install".to_owned()))
        } else if adapter.name == "node" && tool == "yarn" {
            Some(Command::Shell(
                if snapshot.names.contains(".yarnrc.yml") {
                    "yarn install --immutable"
                } else {
                    "yarn install --frozen-lockfile"
                }
                .to_owned(),
            ))
        } else {
            None
        };
        hits.push(AdapterHit {
            adapter: adapter.name.clone(),
            tool,
            dir: snapshot.dir.clone(),
            notice,
            sync_override,
        });
        break;
    }
    if snapshot.dir == "." && snapshot.names.contains(".gitmodules") {
        hits.push(AdapterHit {
            adapter: "submodules".to_owned(),
            tool: "git".to_owned(),
            dir: ".".to_owned(),
            notice: None,
            sync_override: None,
        });
    }
    Ok(hits)
}

pub fn contribution(hits: &[AdapterHit]) -> Result<AdapterContribution, CoreError> {
    let adapters: BTreeMap<_, _> = builtins()?
        .into_iter()
        .map(|adapter| (adapter.name.clone(), adapter))
        .collect();
    let mut output = AdapterContribution::default();
    for hit in hits {
        let adapter = &adapters[&hit.adapter];
        let tool = &adapter.tools[&hit.tool];
        output.selected_tools.insert(hit.tool.clone());
        for (key, value) in &tool.env {
            output.env.insert(key.clone(), value.clone());
        }
        append_unique(&mut output.commands, &tool.commands);
        append_unique(&mut output.sync_inputs, &tool.sync_inputs);
        // A tool detected under a configured scope seeds relative to that
        // scope: `backend/target/...`, not the repository's `target/`.
        let seed = tool
            .seed
            .iter()
            .map(|path| {
                if hit.dir == "." {
                    path.clone()
                } else {
                    format!("{}/{}", hit.dir.trim_end_matches('/'), path)
                }
            })
            .collect::<Vec<_>>();
        append_unique(&mut output.seed, &seed);
        if let Some(requirement) = &tool.requires {
            append_unique(&mut output.requirements, std::slice::from_ref(requirement));
        }
        append_unique(&mut output.nudges, &adapter.nudge);
    }
    Ok(output)
}

pub fn apply_contribution(
    config: &mut Config,
    contribution: &AdapterContribution,
) -> Result<(), CoreError> {
    for (key, value) in &contribution.env {
        config
            .root
            .env
            .insert(key.clone(), ValueOrFalse::Value(value.clone()));
    }
    for command in &contribution.commands {
        config.root.commands.insert(command.clone(), true);
    }
    for path in &contribution.sync_inputs {
        let path = RelPath::new(path)?;
        append_unique(&mut config.sync_inputs, std::slice::from_ref(&path));
    }
    for path in &contribution.seed {
        let path = RelPath::new(path)?;
        append_unique(&mut config.seed, std::slice::from_ref(&path));
    }
    Ok(())
}

fn append_unique<T: Clone + Eq>(target: &mut Vec<T>, values: &[T]) {
    for value in values {
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
}

fn select_lockfile(adapter: &Adapter, snapshot: &DirSnapshot) -> Option<String> {
    adapter.tools.iter().find_map(|(id, tool)| {
        tool.lockfile
            .iter()
            .any(|name| matches_name(name, &snapshot.names))
            .then(|| id.clone())
    })
}

fn select_sniff(adapter: &Adapter, snapshot: &DirSnapshot) -> Option<String> {
    adapter.tools.iter().find_map(|(id, tool)| {
        tool.sniff
            .iter()
            .any(|sniff| sniff_matches(sniff, snapshot))
            .then(|| id.clone())
    })
}

fn sniff_matches(sniff: &Sniff, snapshot: &DirSnapshot) -> bool {
    sniff_content_matches(sniff, &snapshot.contents)
}

/// Applies one sniff rule to a map of file contents keyed by the rule's
/// `file` string. `toml_key` walks dot-separated tables; with `contains` as
/// well, the key must exist *and* its value's text must contain the needle.
pub fn sniff_content_matches(sniff: &Sniff, contents: &BTreeMap<String, String>) -> bool {
    let Some(content) = contents.get(&sniff.file) else {
        return false;
    };
    if let Some(key) = &sniff.toml_key {
        let Ok(table) = content.parse::<toml::Table>() else {
            return false;
        };
        let mut value = &toml::Value::Table(table);
        for segment in key.split('.') {
            match value.as_table().and_then(|table| table.get(segment)) {
                Some(next) => value = next,
                None => return false,
            }
        }
        return sniff.contains.as_ref().is_none_or(|needle| {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
                .contains(needle)
        });
    }
    sniff
        .contains
        .as_ref()
        .is_none_or(|needle| content.contains(needle))
}

fn matches_name(pattern: &str, names: &BTreeSet<String>) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        names.iter().any(|name| name.ends_with(suffix))
    } else {
        names.contains(pattern)
    }
}

pub fn private_id(adapter: &str, tool: &str, dir: &str, task: &str) -> String {
    format!("@{adapter}/{tool}@{dir}/{task}")
}

pub fn compose(
    hits: &[AdapterHit],
    layer_tasks: &BTreeMap<(String, String), (Origin, Task)>,
    package_scripts: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeMap<String, Node>, CoreError> {
    let adapters: BTreeMap<String, Adapter> = builtins()?
        .into_iter()
        .map(|adapter| (adapter.name.clone(), adapter))
        .collect();
    let mut catalog = BTreeMap::new();
    let mut scoped_public = BTreeSet::new();
    let mut public_task_ids = BTreeSet::new();
    for hit in hits {
        let tool = &adapters[&hit.adapter].tools[&hit.tool];
        for (task_id, task) in &tool.task {
            if hit.adapter == "node"
                && task_id != "sync"
                && !(hit.tool == "npm" && task_id == "test")
                && !package_scripts
                    .get(&hit.dir)
                    .is_some_and(|scripts| scripts.contains(task_id))
            {
                continue;
            }
            let mut task = task.clone();
            if hit.adapter == "submodules" && task_id == "sync" {
                task.sys_locks.push("RepoGit".to_owned());
            }
            if task_id == "sync" && hit.sync_override.is_some() {
                task.run.clone_from(&hit.sync_override);
            }
            let private = private_id(&hit.adapter, &hit.tool, &hit.dir, task_id);
            let node = Node::from_task(
                private.clone(),
                RelDir::new(&hit.dir)?,
                Origin::Adapter,
                &task,
            );
            catalog.insert(private.clone(), node);
            public_task_ids.insert(task_id.clone());
            if hit.dir != "." {
                let public = format!("{}/{task_id}", hit.dir);
                if !layer_tasks.contains_key(&(hit.dir.clone(), task_id.clone())) {
                    let alias = Node::from_task(
                        public.clone(),
                        RelDir::new(&hit.dir)?,
                        Origin::Composite,
                        &Task {
                            needs: Some(vec![private]),
                            ..Task::default()
                        },
                    );
                    catalog.insert(public.clone(), alias);
                }
                scoped_public.insert(public);
            }
        }
    }
    for ((scope, id), (origin, task)) in layer_tasks {
        let public = if scope == "." {
            id.clone()
        } else {
            format!("{scope}/{id}")
        };
        catalog.insert(
            public.clone(),
            Node::from_task(public.clone(), RelDir::new(scope)?, *origin, task),
        );
        public_task_ids.insert(id.clone());
        if scope != "." {
            scoped_public.insert(public);
        }
    }
    for task_id in public_task_ids {
        if layer_tasks.contains_key(&(".".to_owned(), task_id.clone())) {
            continue;
        }
        let submodule = hits
            .iter()
            .find(|hit| hit.dir == "." && hit.adapter == "submodules")
            .map(|hit| private_id(&hit.adapter, &hit.tool, ".", &task_id))
            .filter(|id| catalog.contains_key(id));
        let roots = hits
            .iter()
            .filter(|hit| hit.dir == "." && hit.adapter != "submodules")
            .map(|hit| private_id(&hit.adapter, &hit.tool, ".", &task_id))
            .filter(|id| catalog.contains_key(id));
        let scopes = scoped_public
            .iter()
            .filter(|id| id.rsplit_once('/').is_some_and(|(_, id)| id == task_id))
            .cloned();
        let mut needs = Vec::new();
        needs.extend(submodule);
        needs.extend(roots);
        needs.extend(scopes);
        let mut seen = BTreeSet::new();
        needs.retain(|need| seen.insert(need.clone()));
        if !needs.is_empty() {
            let composite = Node::from_task(
                task_id.clone(),
                RelDir::new(".")?,
                Origin::Composite,
                &Task {
                    needs: Some(needs),
                    ..Task::default()
                },
            );
            catalog.insert(task_id, composite);
        }
    }
    if !catalog.contains_key("verify") {
        let selected = ["test", "build"]
            .into_iter()
            .find(|id| catalog.contains_key(*id));
        if let Some(selected) = selected {
            let verify = Node::from_task(
                "verify".to_owned(),
                RelDir::new(".")?,
                Origin::Composite,
                &Task {
                    needs: Some(vec![selected.to_owned()]),
                    ..Task::default()
                },
            );
            catalog.insert("verify".to_owned(), verify);
        }
    }
    Ok(catalog)
}

pub fn composition_notices(catalog: &BTreeMap<String, Node>) -> Vec<String> {
    if catalog.contains_key("verify") {
        Vec::new()
    } else {
        vec!["NO_VERIFY".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nightly_rustfmt_sniff_selects_nightly_tool() {
        let snapshot = DirSnapshot {
            dir: ".".to_owned(),
            names: BTreeSet::from(["Cargo.toml".to_owned(), "rustfmt.toml".to_owned()]),
            contents: BTreeMap::from([(
                "rustfmt.toml".to_owned(),
                "group_imports = 'StdExternalCrate'".to_owned(),
            )]),
        };
        assert_eq!(
            detect(&snapshot, &BTreeMap::new()).unwrap()[0].tool,
            "cargo-nightly-fmt"
        );
    }

    #[test]
    fn lockfiles_choose_tools_and_override_wins() {
        let snapshot = DirSnapshot {
            dir: ".".to_owned(),
            names: BTreeSet::from(["package.json".to_owned(), "pnpm-lock.yaml".to_owned()]),
            contents: BTreeMap::new(),
        };
        assert_eq!(detect(&snapshot, &BTreeMap::new()).unwrap()[0].tool, "pnpm");
        assert_eq!(
            detect(
                &snapshot,
                &BTreeMap::from([(
                    "node".to_owned(),
                    AdapterChoice {
                        tool: Some("npm".to_owned()),
                        disabled: None,
                    },
                )])
            )
            .unwrap()[0]
                .tool,
            "npm"
        );
        let no_lock = DirSnapshot {
            dir: ".".to_owned(),
            names: BTreeSet::from(["package.json".to_owned()]),
            contents: BTreeMap::new(),
        };
        let hit = detect(&no_lock, &BTreeMap::new()).unwrap().remove(0);
        assert_eq!(hit.notice.as_deref(), Some("NO_LOCKFILE"));
        assert_eq!(
            hit.sync_override,
            Some(Command::Shell("npm install".to_owned()))
        );
    }

    #[test]
    fn orbitcloud_composition_is_overridden_at_root() {
        let hits = vec![
            AdapterHit {
                adapter: "dotnet".to_owned(),
                tool: "dotnet".to_owned(),
                dir: ".".to_owned(),
                notice: None,
                sync_override: None,
            },
            AdapterHit {
                adapter: "node".to_owned(),
                tool: "npm".to_owned(),
                dir: "frontend".to_owned(),
                notice: None,
                sync_override: None,
            },
            AdapterHit {
                adapter: "node".to_owned(),
                tool: "npm".to_owned(),
                dir: "website".to_owned(),
                notice: None,
                sync_override: None,
            },
        ];
        let scripts = BTreeMap::from([
            ("frontend".to_owned(), BTreeSet::from(["build".to_owned()])),
            ("website".to_owned(), BTreeSet::from(["build".to_owned()])),
        ]);
        let before = compose(&hits, &BTreeMap::new(), &scripts).unwrap();
        assert_eq!(before["sync"].needs.len(), 3);
        let custom = Task {
            run: Some(Command::Shell("custom".to_owned())),
            ..Task::default()
        };
        let after = compose(
            &hits,
            &BTreeMap::from([((".".to_owned(), "sync".to_owned()), (Origin::Repo, custom))]),
            &scripts,
        )
        .unwrap();
        assert_eq!(after["sync"].run, Some(Command::Shell("custom".to_owned())));
        assert!(after.contains_key("frontend/sync"));
    }

    #[test]
    fn disabled_adapter_is_skipped_and_tool_metadata_is_contributed() {
        let snapshot = DirSnapshot {
            dir: ".".to_owned(),
            names: BTreeSet::from(["Cargo.toml".to_owned()]),
            contents: BTreeMap::new(),
        };
        let disabled = BTreeMap::from([(
            "cargo".to_owned(),
            AdapterChoice {
                tool: None,
                disabled: Some(true),
            },
        )]);
        assert!(detect(&snapshot, &disabled).unwrap().is_empty());

        let hits = detect(&snapshot, &BTreeMap::new()).unwrap();
        let contribution = contribution(&hits).unwrap();
        assert!(contribution.sync_inputs.contains(&"Cargo.toml".to_owned()));
        // The tree's build lives in its own `target/`: no cargo directory
        // variable of any kind, and the compiled dependencies arrive by seed.
        assert!(!contribution.env.keys().any(|key| key.starts_with("CARGO")));
        assert_eq!(
            contribution.seed,
            vec![
                "target/debug/.fingerprint".to_owned(),
                "target/debug/build".to_owned(),
                "target/debug/deps".to_owned(),
                "target/debug/incremental".to_owned(),
            ]
        );
        assert!(contribution
            .nudges
            .iter()
            .any(|nudge| nudge.want == "sccache"));
        let mut config = Config::default();
        apply_contribution(&mut config, &contribution).unwrap();
        assert!(config
            .sync_inputs
            .iter()
            .any(|path| path.as_str() == "Cargo.toml"));
        assert!(config
            .seed
            .iter()
            .any(|path| path.as_str() == "target/debug/deps"));
        // `seed` is the adapter's to declare, never the configuration's.
        assert!(crate::config::parse("seed = ['target']", "repo").is_err());
        // Under a configured scope the seed follows the scope.
        let scoped = contribution(&[AdapterHit {
            adapter: "cargo".to_owned(),
            tool: "cargo".to_owned(),
            dir: "backend".to_owned(),
            notice: None,
            sync_override: None,
        }])
        .unwrap();
        assert!(scoped
            .seed
            .iter()
            .all(|path| path.starts_with("backend/target/debug/")));
    }

    #[test]
    fn composition_supports_private_tasks_dynamic_composites_and_no_verify() {
        let hit = AdapterHit {
            adapter: "node".to_owned(),
            tool: "npm".to_owned(),
            dir: "frontend".to_owned(),
            notice: None,
            sync_override: None,
        };
        let scripts =
            BTreeMap::from([("frontend".to_owned(), BTreeSet::from(["build".to_owned()]))]);
        let layer_tasks = BTreeMap::from([(
            ("frontend".to_owned(), "e2e".to_owned()),
            (
                Origin::Repo,
                Task {
                    run: Some(Command::Shell("npm run e2e".to_owned())),
                    ..Task::default()
                },
            ),
        )]);
        let catalog = compose(&[hit], &layer_tasks, &scripts).unwrap();
        assert!(catalog.contains_key("@node/npm@frontend/build"));
        assert_eq!(catalog["e2e"].needs, ["frontend/e2e"]);
        let no_verify = compose(&[], &layer_tasks, &BTreeMap::new()).unwrap();
        assert_eq!(composition_notices(&no_verify), ["NO_VERIFY"]);
    }

    #[test]
    fn composite_needs_keep_submodule_root_and_scope_order() {
        let hits = vec![
            AdapterHit {
                adapter: "cargo".to_owned(),
                tool: "cargo".to_owned(),
                dir: ".".to_owned(),
                notice: None,
                sync_override: None,
            },
            AdapterHit {
                adapter: "node".to_owned(),
                tool: "npm".to_owned(),
                dir: "frontend".to_owned(),
                notice: None,
                sync_override: None,
            },
            AdapterHit {
                adapter: "submodules".to_owned(),
                tool: "git".to_owned(),
                dir: ".".to_owned(),
                notice: None,
                sync_override: None,
            },
        ];
        let catalog = compose(&hits, &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert_eq!(
            catalog["sync"].needs,
            [
                "@submodules/git@./sync",
                "@cargo/cargo@./sync",
                "frontend/sync",
            ]
        );
    }

    #[test]
    fn npm_test_is_unconditional_but_other_node_scripts_are_detected() {
        let hit = AdapterHit {
            adapter: "node".to_owned(),
            tool: "npm".to_owned(),
            dir: ".".to_owned(),
            notice: None,
            sync_override: None,
        };
        let catalog = compose(&[hit], &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert!(catalog.contains_key("@node/npm@./test"));
        assert!(!catalog.contains_key("@node/npm@./build"));
    }

    #[test]
    fn built_in_test_recipes_forward_only_where_argument_placement_is_unambiguous() {
        let adapters = builtins()
            .unwrap()
            .into_iter()
            .map(|adapter| (adapter.name.clone(), adapter))
            .collect::<BTreeMap<_, _>>();
        let cases = [
            ("cargo", "cargo", "cargo test", "cargo test"),
            ("cargo", "cargo-nightly-fmt", "cargo test", "cargo test"),
            ("node", "npm", "npm test --", "npm test"),
            ("node", "pnpm", "pnpm test", "pnpm test"),
            ("node", "yarn", "yarn test", "yarn test"),
            ("node", "bun", "bun test", "bun test"),
            ("dotnet", "dotnet", "dotnet test", "dotnet test"),
            ("python", "uv", "uv run pytest", "uv run pytest"),
            ("python", "poetry", "poetry run pytest", "poetry run pytest"),
            ("python", "pip", ".venv/bin/pytest", ".venv/bin/pytest"),
        ];
        for (adapter, tool, with_args, without_args) in cases {
            let Some(Command::Shell(recipe)) =
                adapters[adapter].tools[tool].task["test"].run.as_ref()
            else {
                panic!("{adapter}/{tool} test must be a shell recipe");
            };
            assert_eq!(
                recipe,
                &format!("if [ \"$#\" -gt 0 ]; then {with_args} \"$@\"; else {without_args}; fi"),
                "{adapter}/{tool}"
            );
        }
        assert_eq!(
            adapters["go"].tools["go"].task["test"].run,
            Some(Command::Shell("go test ./...".to_owned()))
        );
    }
}
