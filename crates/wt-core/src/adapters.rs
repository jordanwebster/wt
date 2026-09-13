use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::{
    config::{AdapterChoice, Command, Config, Hooks, Recipe, ValueOrFalse},
    error::CoreError,
    model::{RelDir, RelPath},
    recipe_plan::{Node, Origin},
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
    /// The tool's build output roots: what a new tree's snapshot of the
    /// canonical clones (§11.8) and what the sweep reclaims from (§11.9),
    /// each with the strategy that knows its layout.
    pub output: Vec<OutputSpec>,
    pub env: IndexMap<String, String>,
    pub commands: Vec<String>,
    /// Private lifecycle command used only by `wt sync`.
    pub sync: Option<Command>,
    pub after_new: Option<Command>,
    pub warm: Option<Command>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSpec {
    pub path: String,
    pub kind: String,
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
    pub outputs: Vec<crate::config::Output>,
    pub requirements: Vec<String>,
    pub nudges: Vec<Nudge>,
    pub selected_tools: BTreeSet<String>,
    pub hooks: Hooks,
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
                    "reinstall a valid wt binary",
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
    let mut contributed = AdapterContribution::default();
    for hit in hits {
        let adapter = &adapters[&hit.adapter];
        let tool = &adapter.tools[&hit.tool];
        contributed.selected_tools.insert(hit.tool.clone());
        for (key, value) in &tool.env {
            contributed.env.insert(key.clone(), value.clone());
        }
        append_unique(&mut contributed.commands, &tool.commands);
        append_unique(&mut contributed.sync_inputs, &tool.sync_inputs);
        if contributed.hooks.after_new.is_none() {
            contributed.hooks.after_new.clone_from(&tool.after_new);
        }
        if contributed.hooks.warm.is_none() {
            contributed.hooks.warm.clone_from(&tool.warm);
        }
        // A tool detected under a configured scope has its output relative
        // to that scope: `backend/target`, not the repository's `target/`.
        for spec in &tool.output {
            let kind = match spec.kind.as_str() {
                "cargo" => crate::config::OutputKind::Cargo,
                other => {
                    return Err(CoreError::new(
                        ExitClass::Internal,
                        "ADAPTER_INVALID",
                        format!(
                            "built-in adapter `{}` names unknown output kind `{other}`",
                            hit.adapter
                        ),
                        "reinstall a valid wt binary",
                    ))
                }
            };
            let output = crate::config::Output {
                kind,
                workspace: hit.dir.trim_end_matches('/').to_owned(),
                path: RelPath::new(if hit.dir == "." {
                    spec.path.clone()
                } else {
                    format!("{}/{}", hit.dir.trim_end_matches('/'), spec.path)
                })?,
                adapter: true,
            };
            append_unique(&mut contributed.outputs, std::slice::from_ref(&output));
        }
        if let Some(requirement) = &tool.requires {
            append_unique(
                &mut contributed.requirements,
                std::slice::from_ref(requirement),
            );
        }
        append_unique(&mut contributed.nudges, &adapter.nudge);
    }
    Ok(contributed)
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
    append_unique(&mut config.outputs, &contribution.outputs);
    if config.hooks.after_new.is_none() {
        config
            .hooks
            .after_new
            .clone_from(&contribution.hooks.after_new);
    }
    if config.hooks.warm.is_none() {
        config.hooks.warm.clone_from(&contribution.hooks.warm);
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

pub fn private_id(adapter: &str, tool: &str, dir: &str, recipe: &str) -> String {
    format!("@{adapter}/{tool}@{dir}/{recipe}")
}

pub fn compose(
    hits: &[AdapterHit],
    layer_recipes: &BTreeMap<(String, String), (Origin, Recipe)>,
    _package_scripts: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeMap<String, Node>, CoreError> {
    let adapters: BTreeMap<String, Adapter> = builtins()?
        .into_iter()
        .map(|adapter| (adapter.name.clone(), adapter))
        .collect();
    let mut catalog = BTreeMap::new();
    let mut sync_nodes = Vec::new();
    for hit in hits {
        let tool = &adapters[&hit.adapter].tools[&hit.tool];
        if tool.sync.is_some() || hit.sync_override.is_some() {
            let mut recipe = Recipe {
                command: hit.sync_override.clone().or_else(|| tool.sync.clone()),
                ..Recipe::default()
            };
            if hit.adapter == "submodules" {
                recipe.sys_locks.push("RepoGit".to_owned());
            }
            let private = private_id(&hit.adapter, &hit.tool, &hit.dir, "sync");
            let node = Node::from_recipe(
                private.clone(),
                RelDir::new(&hit.dir)?,
                Origin::Adapter,
                &recipe,
            );
            catalog.insert(private.clone(), node);
            sync_nodes.push(private);
        }
    }
    for ((scope, id), (origin, recipe)) in layer_recipes {
        let public = if scope == "." {
            id.clone()
        } else {
            format!("{scope}/{id}")
        };
        catalog.insert(
            public.clone(),
            Node::from_recipe(public.clone(), RelDir::new(scope)?, *origin, recipe),
        );
    }
    // `wt sync` remains a lifecycle verb even when no adapter has dependency
    // work to do. Its private root therefore exists as a no-op aggregate.
    catalog.insert(
        "sync".to_owned(),
        Node::from_recipe(
            "sync".to_owned(),
            RelDir::new(".")?,
            Origin::Composite,
            &Recipe {
                needs: Some(sync_nodes),
                ..Recipe::default()
            },
        ),
    );
    Ok(catalog)
}

pub fn composition_notices(catalog: &BTreeMap<String, Node>) -> Vec<String> {
    let _ = catalog;
    Vec::new()
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
    fn adapter_sync_commands_compose_at_root() {
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
        assert!(before.contains_key("@dotnet/dotnet@./sync"));
        assert!(before.contains_key("@node/npm@frontend/sync"));
        assert!(before.contains_key("@node/npm@website/sync"));
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
        let contributed = contribution(&hits).unwrap();
        assert!(contributed.sync_inputs.contains(&"Cargo.toml".to_owned()));
        // The tree's build lives in its own `target/`: no cargo directory
        // variable of any kind, and the output root is what a new tree's
        // snapshot clones and the sweep reclaims from.
        assert!(!contributed.env.keys().any(|key| key.starts_with("CARGO")));
        assert_eq!(
            contributed.outputs,
            vec![crate::config::Output {
                kind: crate::config::OutputKind::Cargo,
                workspace: ".".to_owned(),
                path: RelPath::new("target").unwrap(),
                adapter: true,
            }]
        );
        assert!(!contributed
            .nudges
            .iter()
            .any(|nudge| nudge.want == "sccache"));
        let mut config = Config::default();
        apply_contribution(&mut config, &contributed).unwrap();
        assert!(config
            .sync_inputs
            .iter()
            .any(|path| path.as_str() == "Cargo.toml"));
        assert!(config
            .outputs
            .iter()
            .any(|output| output.path.as_str() == "target"));
        let configured =
            crate::config::parse("output = [{ path = 'node_modules' }]", "repo").unwrap();
        assert_eq!(
            configured.outputs[0].kind,
            crate::config::OutputKind::Opaque
        );
        assert!(!configured.outputs[0].adapter);
        assert!(
            crate::config::parse("output = [{ path = 'target', kind = 'cargo' }]", "repo")
                .unwrap_err()
                .message
                .contains("adapter-owned")
        );
        assert!(crate::config::parse("seed = ['target']", "repo").is_err());
        // Under a configured scope the output follows the scope.
        let scoped = contribution(&[AdapterHit {
            adapter: "cargo".to_owned(),
            tool: "cargo".to_owned(),
            dir: "backend".to_owned(),
            notice: None,
            sync_override: None,
        }])
        .unwrap();
        assert_eq!(scoped.outputs[0].workspace, "backend");
        assert_eq!(scoped.outputs[0].path.as_str(), "backend/target");
    }

    #[test]
    fn composition_contains_only_private_sync_and_declared_resources() {
        let hit = AdapterHit {
            adapter: "node".to_owned(),
            tool: "npm".to_owned(),
            dir: "frontend".to_owned(),
            notice: None,
            sync_override: None,
        };
        let scripts = BTreeMap::new();
        let layer_recipes = BTreeMap::from([(
            ("frontend".to_owned(), "e2e".to_owned()),
            (
                Origin::Repo,
                Recipe {
                    command: Some(Command::Shell("npm run e2e".to_owned())),
                    ..Recipe::default()
                },
            ),
        )]);
        let catalog = compose(&[hit], &layer_recipes, &scripts).unwrap();
        assert!(catalog.contains_key("@node/npm@frontend/sync"));
        assert!(catalog.contains_key("sync"));
        assert!(catalog.contains_key("frontend/e2e"));
        assert!(!catalog.contains_key("build"));
        assert!(composition_notices(&catalog).is_empty());
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
                "@cargo/cargo@./sync",
                "@node/npm@frontend/sync",
                "@submodules/git@./sync",
            ]
        );
    }

    #[test]
    fn adapters_do_not_contribute_work_commands() {
        let hit = AdapterHit {
            adapter: "node".to_owned(),
            tool: "npm".to_owned(),
            dir: ".".to_owned(),
            notice: None,
            sync_override: None,
        };
        let catalog = compose(&[hit], &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert!(catalog.contains_key("@node/npm@./sync"));
        assert!(!catalog.contains_key("@node/npm@./test"));
        assert!(!catalog.contains_key("@node/npm@./build"));
    }
}
