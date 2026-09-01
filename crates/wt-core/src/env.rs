use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    config::{EffectiveScope, FileDef},
    error::CoreError,
    model::{name_snake, EnvMap, TreeIdentity},
    resource::ResourceKey,
    template, ExitClass,
};

pub const ACTIVATION_KEY: &str = "WT_ACTIVATION";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub v: u8,
    pub target: String,
    pub home: String,
    pub applied: BTreeMap<String, String>,
    pub prior: BTreeMap<String, Option<String>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeactivationReport {
    pub restored: Vec<String>,
    pub activation_ignored: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Deactivation {
    pub clean: EnvMap,
    pub prior: Option<Activation>,
    pub report: DeactivationReport,
}

pub fn deactivate(parent: &EnvMap) -> Result<Deactivation, CoreError> {
    let Some(raw) = parent.get(ACTIVATION_KEY) else {
        return Ok(Deactivation {
            clean: parent.clone(),
            prior: None,
            report: DeactivationReport::default(),
        });
    };
    let activation: Activation = match serde_json::from_str(raw) {
        Ok(activation) => activation,
        Err(_) => return Ok(ignored_activation(parent)),
    };
    if activation.v != 1
        || activation.applied.keys().ne(activation.prior.keys())
        || activation
            .applied
            .keys()
            .any(|key| !activation_key_valid(key))
        || activation.applied.contains_key(ACTIVATION_KEY)
    {
        return Ok(ignored_activation(parent));
    }
    let mut clean = parent.clone();
    clean.remove(ACTIVATION_KEY);
    let mut report = DeactivationReport::default();
    for key in activation.applied.keys() {
        match &activation.prior[key] {
            Some(prior) => {
                clean.insert(key.clone(), prior.clone());
            }
            None => {
                clean.remove(key);
            }
        }
        report.restored.push(key.clone());
    }
    Ok(Deactivation {
        clean,
        prior: Some(activation),
        report,
    })
}

fn activation_key_valid(key: &str) -> bool {
    key == "PATH" || (key.starts_with("WT_") && key != ACTIVATION_KEY) || {
        let mut chars = key.chars();
        matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }
}

fn ignored_activation(parent: &EnvMap) -> Deactivation {
    let mut clean = parent.clone();
    clean.remove(ACTIVATION_KEY);
    Deactivation {
        clean,
        prior: None,
        report: DeactivationReport {
            activation_ignored: true,
            ..DeactivationReport::default()
        },
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskContext {
    pub id: String,
    pub env: BTreeMap<String, String>,
    pub resource_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EnvInputs<'a> {
    pub cfg: &'a EffectiveScope,
    pub tree: &'a TreeIdentity,
    pub home: &'a str,
    pub contributed: Vec<(ResourceKey, EnvMap)>,
    pub task: Option<TaskContext>,
    pub parent: &'a EnvMap,
    pub existing_dirs: &'a BTreeSet<String>,
    pub file_sources: &'a BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Render {
    pub path: String,
    /// Complete bytes written to disk, including the provenance header when
    /// present. Writers must not prepend `header` a second time.
    pub content: String,
    pub mode: String,
    pub header: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvReport {
    pub set: Vec<String>,
    pub overrode: Vec<String>,
    pub missing_bins: Vec<String>,
    pub restored: Vec<String>,
    #[serde(skip)]
    pub activation_ignored: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvOutput {
    pub env: EnvMap,
    pub activation: Activation,
    pub activation_json: String,
    pub render: Vec<Render>,
    pub report: EnvReport,
    pub vars: BTreeMap<String, String>,
    pub functions: template::FunctionValues,
}

struct Assembly {
    clean: EnvMap,
    env: EnvMap,
    applied: BTreeMap<String, String>,
    prior: BTreeMap<String, Option<String>>,
    report: EnvReport,
}

impl Assembly {
    fn new(clean: EnvMap, report: EnvReport) -> Self {
        Self {
            env: clean.clone(),
            clean,
            applied: BTreeMap::new(),
            prior: BTreeMap::new(),
            report,
        }
    }

    fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        self.prior
            .entry(key.clone())
            .or_insert_with(|| self.clean.get(&key).cloned());
        self.applied.insert(key.clone(), value.clone());
        self.env.insert(key, value);
    }

    fn alias(&mut self, key: &str, value: String) {
        // PATH is a coordinate assembled in step 4; neither contributed
        // resource env nor aliases may erase its declared-bin prefix.
        if key == "PATH" {
            return;
        }
        self.set(key, value);
        if self.clean.contains_key(key) {
            self.report.overrode.push(key.to_owned());
        } else {
            self.report.set.push(key.to_owned());
        }
    }
}

pub fn assemble(input: EnvInputs<'_>) -> Result<EnvOutput, CoreError> {
    let deactivated = deactivate(input.parent)?;
    let report = EnvReport {
        restored: deactivated.report.restored,
        activation_ignored: deactivated.report.activation_ignored,
        ..EnvReport::default()
    };

    let mut state = Assembly::new(deactivated.clean, report);

    let target = input.tree.target().to_string();
    let tool = BTreeMap::from([
        ("WT_LABEL".to_owned(), input.tree.label.to_string()),
        ("WT_NAME".to_owned(), input.tree.name.clone()),
        ("WT_TARGET".to_owned(), target.clone()),
        (
            "WT_BRANCH".to_owned(),
            input.tree.branch.clone().unwrap_or_default(),
        ),
        ("WT_ROOT".to_owned(), input.tree.root.clone()),
        ("WT_REPO".to_owned(), input.tree.repo.clone()),
        ("WT_HOME".to_owned(), input.home.to_owned()),
    ]);
    for (key, value) in &tool {
        state.set(key, value);
    }
    let mut port_indices = BTreeSet::new();
    if input.tree.ports.len() > usize::from(input.tree.geometry.stride)
        || input
            .tree
            .ports
            .values()
            .any(|index| *index >= input.tree.geometry.stride || !port_indices.insert(*index))
    {
        return Err(CoreError::new(
            ExitClass::State,
            "REGISTRY_CORRUPT",
            "recorded port coordinates are outside the frozen geometry",
            "delete the corrupt registry and re-register the affected checkouts",
        ));
    }
    let mut port_values = BTreeMap::new();
    for (port_name, index) in &input.tree.ports {
        let port_value = u32::from(input.tree.geometry.port_base)
            .checked_add(u32::from(*index))
            .filter(|port| *port <= u32::from(u16::MAX))
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::State,
                    "REGISTRY_CORRUPT",
                    "recorded port coordinates overflow the port space",
                    "inspect the registry backup and run `wt doctor`",
                )
            })?;
        port_values.insert(port_name.as_str().to_owned(), port_value.to_string());
    }

    let bins: Vec<String> = input
        .cfg
        .bin
        .iter()
        .map(|path| {
            format!(
                "{}/{}",
                input.tree.root.trim_end_matches('/'),
                path.as_str()
            )
        })
        .collect();
    for bin in &bins {
        if !input.existing_dirs.contains(bin) {
            state.report.missing_bins.push(bin.clone());
        }
    }
    let parent_path = state.clean.get("PATH").cloned().unwrap_or_default();
    let shims = (!input.cfg.commands.is_empty())
        .then(|| format!("{}/.wt/shims", input.tree.root.trim_end_matches('/')));
    let path_prefix = shims
        .into_iter()
        .chain(bins.iter().cloned())
        .collect::<Vec<_>>()
        .join(":");
    let path = (!path_prefix.is_empty())
        .then_some(path_prefix.clone())
        .into_iter()
        .chain((!parent_path.is_empty()).then_some(parent_path))
        .collect::<Vec<_>>()
        .join(":");
    let wt_bin = bins.join(":");
    state.set("PATH", path);
    state.set("WT_BIN", wt_bin);
    state.set("WT_PATH_PREFIX", path_prefix);

    let mut contributed = input.contributed;
    contributed.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, values) in contributed {
        for (key, value) in values {
            state.alias(&key, value);
        }
    }

    let functions = template::FunctionValues {
        simple: BTreeMap::from([
            ("home".to_owned(), input.home.to_owned()),
            ("root".to_owned(), input.tree.root.clone()),
            ("repo".to_owned(), input.tree.repo.clone()),
            (
                "branch".to_owned(),
                input.tree.branch.clone().unwrap_or_default(),
            ),
            ("label".to_owned(), input.tree.label.to_string()),
            ("name".to_owned(), input.tree.name.clone()),
            ("name_snake".to_owned(), name_snake(&input.tree.name)),
            ("name_short".to_owned(), input.tree.name_short.clone()),
            ("target".to_owned(), target.clone()),
        ]),
        ports: port_values,
    };
    let vars = template::resolve_vars(&input.cfg.vars, &functions)?;
    let template_context = template::Context {
        vars: &vars,
        functions: &functions,
    };
    for (key, value) in &input.cfg.env {
        let value = template::expand(value, &template_context)?;
        state.alias(key, value);
    }

    if let Some(task) = input.task {
        state.set("WT_TASK", task.id);
        if let Some(name) = task.resource_name {
            let value = template::expand(&name, &template_context)?;
            state.set("WT_SELF", value);
        }
        for (key, value) in task.env {
            let value = template::expand(&value, &template_context)?;
            state.set(key, value);
        }
    }

    let Assembly {
        mut env,
        applied,
        prior,
        mut report,
        ..
    } = state;

    let activation = Activation {
        v: 1,
        target: target.clone(),
        home: input.home.to_owned(),
        applied,
        prior,
    };
    let activation_json = crate::report::canonical_json(&activation).map_err(|error| {
        CoreError::new(
            ExitClass::Internal,
            "ACTIVATION_SERIALIZE",
            error.to_string(),
            "report this internal error",
        )
    })?;
    env.insert(ACTIVATION_KEY.to_owned(), activation_json.clone());

    let mut render = Vec::new();
    for (path, file) in &input.cfg.files {
        let source = file_text(file, input.file_sources)?;
        let body = if file.template {
            template::expand(source, &template_context)?
        } else {
            source.to_owned()
        };
        let header = (!file.marker.is_empty()).then(|| {
            format!(
                "{} generated by wt for {target}. If you edit this file, wt stops re-rendering it; delete it to let wt regenerate it, or set files.\"{path}\" = false in .wt/config.toml",
                file.marker
            )
        });
        let content = header
            .as_ref()
            .map_or_else(|| body.clone(), |header| format!("{header}\n{body}"));
        render.push(Render {
            path: path.clone(),
            content,
            mode: file.mode.clone(),
            header,
        });
    }
    sort_report(&mut report);
    Ok(EnvOutput {
        env,
        activation,
        activation_json,
        render,
        report,
        vars,
        functions,
    })
}

fn file_text<'a>(
    file: &'a FileDef,
    sources: &'a BTreeMap<String, String>,
) -> Result<&'a str, CoreError> {
    if let Some(content) = &file.content {
        return Ok(content);
    }
    let Some(path) = file.source.as_ref() else {
        return Err(CoreError::new(
            ExitClass::State,
            "CONFIG_INVALID",
            "rendered file has neither content nor source",
            "fix the rendered file declaration",
        ));
    };
    sources
        .get(path.as_str())
        .map(String::as_str)
        .ok_or_else(|| {
            CoreError::new(
                ExitClass::State,
                "FILE_SOURCE_MISSING",
                format!("file source `{path}` was not supplied"),
                "restore the source file or disable the rendered file",
            )
        })
}

fn sort_report(report: &mut EnvReport) {
    for values in [
        &mut report.set,
        &mut report.overrode,
        &mut report.missing_bins,
        &mut report.restored,
    ] {
        values.sort();
        values.dedup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{EffectiveScope, TiedTo},
        model::{Geometry, Label, PortMap, PortName, RelPath},
    };
    use proptest::prelude::*;

    fn tree(name: &str, port: u16) -> TreeIdentity {
        TreeIdentity {
            tree_id: "01".repeat(16),
            label: Label::new("repo").unwrap(),
            name: name.to_owned(),
            canonical: false,
            root: format!("/trees/{name}"),
            repo: "/repo".to_owned(),
            branch: Some(name.to_owned()),
            slot: 1,
            geometry: Geometry {
                base: 20_000,
                stride: 16,
                port_base: port,
            },
            ports: PortMap::from([(PortName::new("http").unwrap(), 0)]),
            name_short: format!("repo_{name}_12345678"),
        }
    }

    fn output(parent: &EnvMap, name: &str, task: Option<TaskContext>) -> EnvOutput {
        let mut cfg = EffectiveScope::default();
        cfg.env
            .insert("PORT".to_owned(), "{{ports.http}}".to_owned());
        cfg.bin.push(RelPath::new("bin").unwrap());
        let tree = tree(name, if name == "a" { 20_016 } else { 20_032 });
        assemble(EnvInputs {
            cfg: &cfg,
            tree: &tree,
            home: "/home",
            contributed: Vec::new(),
            task,
            parent,
            existing_dirs: &BTreeSet::new(),
            file_sources: &BTreeMap::new(),
        })
        .unwrap()
    }

    proptest! {
        #[test]
        fn l1_deactivation_restores_marker_free_parent(
            path in proptest::option::of(prop_oneof![Just("".to_owned()), "[a-z/:]{0,30}"]),
            preset_target in proptest::option::of("[a-z]{0,8}"),
            preset_alias in proptest::option::of("[0-9]{1,5}"),
            task_door in any::<bool>(),
        ) {
            let mut parent = EnvMap::new();
            if let Some(path) = path { parent.insert("PATH".to_owned(), path); }
            if let Some(value) = preset_target { parent.insert("WT_TARGET".to_owned(), value); }
            if let Some(value) = preset_alias { parent.insert("PORT".to_owned(), value); }
            let task = task_door.then(|| TaskContext {
                id: "test".to_owned(),
                env: BTreeMap::from([("TASK_KEY".to_owned(), "owned".to_owned())]),
                resource_name: None,
            });
            let activated = output(&parent, "a", task);
            prop_assert_eq!(deactivate(&activated.env).unwrap().clean, parent);
        }

        #[test]
        fn l2_nested_activation_equals_direct(
            parent_path in proptest::option::of("[a-z/:]{0,30}"),
            preset_alias in proptest::option::of("[0-9]{1,5}"),
            preset_coordinate in proptest::option::of("[a-z]{0,8}"),
            task_door in any::<bool>(),
        ) {
            let mut parent = EnvMap::new();
            if let Some(path) = parent_path { parent.insert("PATH".to_owned(), path); }
            if let Some(alias) = preset_alias { parent.insert("PORT".to_owned(), alias); }
            if let Some(coordinate) = preset_coordinate { parent.insert("WT_TARGET".to_owned(), coordinate); }
            let task = task_door.then(|| TaskContext {
                id: "test".to_owned(),
                env: BTreeMap::from([("TASK_KEY".to_owned(), "owned".to_owned())]),
                resource_name: None,
            });
            let first = output(&parent, "a", task.clone());
            prop_assert_eq!(
                output(&first.env, "b", task.clone()).env,
                output(&parent, "b", task).env
            );
        }

        #[test]
        fn deactivation_effect_is_bounded_to_applied_keys(
            edit_coordinate in any::<bool>(),
            edit_alias in any::<bool>(),
        ) {
            let parent = EnvMap::from([("UNRELATED".to_owned(), "leave".to_owned())]);
            let mut activated = output(&parent, "a", None).env;
            if edit_coordinate { activated.insert("WT_TARGET".to_owned(), "edited".to_owned()); }
            if edit_alias { activated.insert("PORT".to_owned(), "edited".to_owned()); }
            let metadata: Activation =
                serde_json::from_str(activated.get(ACTIVATION_KEY).unwrap()).unwrap();
            let before: EnvMap = activated
                .iter()
                .filter(|(key, _)| key.as_str() != ACTIVATION_KEY)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let result = deactivate(&activated).unwrap();
            for key in before.keys().filter(|key| !metadata.applied.contains_key(*key)) {
                prop_assert_eq!(result.clean.get(key), before.get(key));
            }
            prop_assert!(result
                .report
                .restored
                .iter()
                .all(|key| metadata.applied.contains_key(key)));
        }
    }

    #[test]
    fn corrupt_marker_is_ignored_and_the_door_proceeds() {
        for raw in [
            "not json",
            "{}",
            r#"{"v":2,"target":"repo/a","home":"/h","applied":{},"prior":{}}"#,
            r#"{"v":1,"target":"repo/a","home":"/h","applied":{"A":"x"},"prior":{}}"#,
        ] {
            let parent = EnvMap::from([
                (ACTIVATION_KEY.to_owned(), raw.to_owned()),
                ("KEEP".to_owned(), "yes".to_owned()),
            ]);
            let result = deactivate(&parent).unwrap();
            assert_eq!(result.clean.get("KEEP").map(String::as_str), Some("yes"));
            assert!(!result.clean.contains_key(ACTIVATION_KEY));
            assert!(result.report.activation_ignored);
        }
    }

    #[test]
    fn user_edits_to_tool_owned_keys_are_replaced_by_the_next_door() {
        let task = TaskContext {
            id: "test".to_owned(),
            env: BTreeMap::from([("TASK_KEY".to_owned(), "owned".to_owned())]),
            resource_name: None,
        };
        let mut activated = output(&EnvMap::new(), "a", Some(task.clone())).env;
        activated.insert("WT_TARGET".to_owned(), "edited".to_owned());
        activated.insert("TASK_KEY".to_owned(), "edited".to_owned());
        let next = output(&activated, "b", Some(task));
        assert_eq!(next.env["WT_TARGET"], "repo/b");
        assert_eq!(next.env["TASK_KEY"], "owned");
        assert!(next.report.restored.contains(&"WT_TARGET".to_owned()));
        assert!(next.report.restored.contains(&"TASK_KEY".to_owned()));
    }

    #[test]
    fn activation_json_is_compact_with_recursively_sorted_keys() {
        let output = output(&EnvMap::new(), "a", None);
        assert_eq!(
            output.activation_json,
            r#"{"applied":{"PATH":"/trees/a/bin","PORT":"20016","WT_BIN":"/trees/a/bin","WT_BRANCH":"a","WT_HOME":"/home","WT_LABEL":"repo","WT_NAME":"a","WT_PATH_PREFIX":"/trees/a/bin","WT_REPO":"/repo","WT_ROOT":"/trees/a","WT_TARGET":"repo/a"},"home":"/home","prior":{"PATH":null,"PORT":null,"WT_BIN":null,"WT_BRANCH":null,"WT_HOME":null,"WT_LABEL":null,"WT_NAME":null,"WT_PATH_PREFIX":null,"WT_REPO":null,"WT_ROOT":null,"WT_TARGET":null},"target":"repo/a","v":1}"#
        );
        assert_eq!(output.env[ACTIVATION_KEY], output.activation_json);
    }

    #[test]
    fn contributed_path_cannot_shadow_declared_bins() {
        let mut cfg = EffectiveScope::default();
        cfg.bin.push(RelPath::new("bin").unwrap());
        let tree = tree("a", 20_016);
        let key = ResourceKey {
            label: Some(Label::new("repo").unwrap()),
            tied_to: TiedTo::Tree,
            name: Some("a".to_owned()),
            scope: RelPath::new(".").unwrap(),
            task: "db".to_owned(),
        };
        let output = assemble(EnvInputs {
            cfg: &cfg,
            tree: &tree,
            home: "/home",
            contributed: vec![(
                key,
                EnvMap::from([("PATH".to_owned(), "/hijack".to_owned())]),
            )],
            task: None,
            parent: &EnvMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]),
            existing_dirs: &BTreeSet::new(),
            file_sources: &BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(output.env["PATH"], "/trees/a/bin:/usr/bin");
    }

    #[test]
    fn corrupt_recorded_port_geometry_is_rejected() {
        let mut tree = tree("a", u16::MAX);
        tree.ports.insert(PortName::new("admin").unwrap(), 1);
        let error = assemble(EnvInputs {
            cfg: &EffectiveScope::default(),
            tree: &tree,
            home: "/home",
            contributed: Vec::new(),
            task: None,
            parent: &EnvMap::new(),
            existing_dirs: &BTreeSet::new(),
            file_sources: &BTreeMap::new(),
        })
        .unwrap_err();
        assert_eq!(error.code.0, "REGISTRY_CORRUPT");
    }

    #[test]
    fn vars_feed_aliases_and_files_without_becoming_environment_keys() {
        let mut cfg = EffectiveScope::default();
        cfg.vars.insert(
            "composed".to_owned(),
            "{{root()}}/{{leaf}}/{{ports.http}}".to_owned(),
        );
        cfg.vars.insert("leaf".to_owned(), "private".to_owned());
        cfg.env
            .insert("VALUE".to_owned(), "{{composed}}".to_owned());
        cfg.files.insert(
            "generated".to_owned(),
            FileDef {
                content: Some("{{composed}}".to_owned()),
                source: None,
                template: true,
                marker: String::new(),
                mode: "0644".to_owned(),
            },
        );
        let output = assemble(EnvInputs {
            cfg: &cfg,
            tree: &tree("a", 20_016),
            home: "/home",
            contributed: Vec::new(),
            task: None,
            parent: &EnvMap::from([("VALUE".to_owned(), "production".to_owned())]),
            existing_dirs: &BTreeSet::new(),
            file_sources: &BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(output.env["VALUE"], "/trees/a/private/20016");
        assert_eq!(output.render[0].content, "/trees/a/private/20016");
        assert!(!output.env.contains_key("composed"));
        assert!(!output.env.contains_key("leaf"));
        assert!(!output.env.contains_key("WT_PORT_HTTP"));
        assert!(!output.env.contains_key("WT_PORT_BASE"));
        assert_eq!(output.report.overrode, ["VALUE"]);
        assert_eq!(
            output.activation.prior["VALUE"].as_deref(),
            Some("production")
        );
    }

    #[test]
    fn every_closed_function_resolves_to_its_typed_value() {
        let mut cfg = EffectiveScope::default();
        cfg.env.insert(
            "ALL".to_owned(),
            "{{home()}}|{{root()}}|{{repo()}}|{{branch()}}|{{label()}}|{{name()}}|{{name_snake()}}|{{name_short()}}|{{target()}}|{{ports.http}}".to_owned(),
        );
        let output = assemble(EnvInputs {
            cfg: &cfg,
            tree: &tree("feature-x", 20_016),
            home: "/home",
            contributed: Vec::new(),
            task: None,
            parent: &EnvMap::new(),
            existing_dirs: &BTreeSet::new(),
            file_sources: &BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(
            output.env["ALL"],
            "/home|/trees/feature-x|/repo|feature-x|repo|feature-x|feature_x|repo_feature-x_12345678|repo/feature-x|20016"
        );
    }
}
