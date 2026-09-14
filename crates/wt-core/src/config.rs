use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    error::CoreError,
    model::{valid_duration, EnvKey, PortName, RelDir, RelPath, ResourceId},
    template, ExitClass,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueOrFalse<T> {
    Value(T),
    False,
}

impl<T> ValueOrFalse<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::False => None,
        }
    }

    pub fn value_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Value(value) => Some(value),
            Self::False => None,
        }
    }
}

impl<T: Serialize> Serialize for ValueOrFalse<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(value) => value.serialize(serializer),
            Self::False => serializer.serialize_bool(false),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ValueOrFalse<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ValueOrBool<T> {
            Value(T),
            Bool(bool),
        }
        match ValueOrBool::deserialize(deserializer)? {
            ValueOrBool::Value(value) => Ok(Self::Value(value)),
            ValueOrBool::Bool(false) => Ok(Self::False),
            ValueOrBool::Bool(true) => Err(D::Error::custom("only `false` deletes an entry")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Command {
    Shell(String),
    Argv(Vec<String>),
}

impl Command {
    pub fn texts(&self) -> &[String] {
        match self {
            Self::Shell(shell) => std::slice::from_ref(shell),
            Self::Argv(argv) => argv,
        }
    }
}

/// How a creation that does not name a branch spells one: a single template,
/// or candidates tried in order until one has all its metadata (A77).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BranchTemplates {
    One(String),
    Many(Vec<String>),
}

impl BranchTemplates {
    pub fn candidates(&self) -> &[String] {
        match self {
            Self::One(template) => std::slice::from_ref(template),
            Self::Many(templates) => templates,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDef {
    pub content: Option<String>,
    pub source: Option<RelPath>,
    #[serde(default = "default_file_template", skip_serializing_if = "is_true")]
    pub template: bool,
    #[serde(default = "default_marker")]
    pub marker: String,
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_file_template() -> bool {
    true
}
fn is_true(value: &bool) -> bool {
    *value
}
fn default_marker() -> String {
    "#".to_owned()
}
fn default_mode() -> String {
    "0644".to_owned()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TiedTo {
    Tree,
    Repo,
    Machine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arena {
    Repo,
    Machine,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Resource {
    pub create: Option<Command>,
    pub exists: Option<Command>,
    pub destroy: Option<Command>,
    pub acquire: Option<Command>,
    #[serde(default = "default_recipe_template", skip_serializing_if = "is_true")]
    pub template: bool,
    #[serde(serialize_with = "serialize_needs")]
    pub needs: Option<Vec<String>>,
    pub name: Option<String>,
    pub tied_to: Option<TiedTo>,
    pub arena: Option<Arena>,
    pub capacity: Option<u16>,
    pub count: Option<u16>,
    pub wait: Option<String>,
    pub keep: Option<String>,
    pub idle: Option<String>,
    pub env: IndexMap<String, String>,
    pub cwd: Option<RelPath>,
    pub timeout: Option<String>,
    pub description: Option<String>,
    pub ready_within: Option<String>,
    pub snapshot_env: Vec<String>,
}

impl Default for Resource {
    fn default() -> Self {
        Self {
            create: None,
            exists: None,
            destroy: None,
            acquire: None,
            template: true,
            needs: None,
            name: None,
            tied_to: None,
            arena: None,
            capacity: None,
            count: None,
            wait: None,
            keep: None,
            idle: None,
            env: IndexMap::new(),
            cwd: None,
            timeout: None,
            description: None,
            ready_within: None,
            snapshot_env: Vec::new(),
        }
    }
}

impl Resource {
    pub fn token_pool(&self) -> bool {
        self.count.is_some()
            && self.exists.is_none()
            && self.create.is_none()
            && self.destroy.is_none()
            && self.acquire.is_none()
    }

    pub fn as_recipe(&self) -> Recipe {
        Recipe {
            command: self.create.clone(),
            exists: self.exists.clone(),
            destroy: self.destroy.clone(),
            template: self.template,
            needs: self.needs.clone(),
            name: self.name.clone(),
            tied_to: self.tied_to,
            arena: self.arena,
            capacity: self.capacity,
            env: self.env.clone(),
            cwd: self.cwd.clone(),
            timeout: self.timeout.clone(),
            description: self.description.clone(),
            ready_within: self.ready_within.clone(),
            snapshot_env: self.snapshot_env.clone(),
            ..Recipe::default()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recipe {
    pub command: Option<Command>,
    pub exists: Option<Command>,
    pub destroy: Option<Command>,
    /// Opts lifecycle commands out of templating so a recipe can
    /// carry text owned by another `{{`-using format, such as a Go template
    /// passed to `docker --format`. `name` and `env` stay templated: they hold
    /// wt's own values, and they are how such a recipe receives them.
    pub template: bool,
    pub needs: Option<Vec<String>>,
    pub name: Option<String>,
    pub tied_to: Option<TiedTo>,
    pub arena: Option<Arena>,
    pub capacity: Option<u16>,
    pub env: IndexMap<String, String>,
    pub cwd: Option<RelPath>,
    pub timeout: Option<String>,
    pub description: Option<String>,
    pub ready_within: Option<String>,
    pub snapshot_env: Vec<String>,
    pub sys_locks: Vec<String>,
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            command: None,
            exists: None,
            destroy: None,
            template: default_recipe_template(),
            needs: None,
            name: None,
            tied_to: None,
            arena: None,
            capacity: None,
            env: IndexMap::new(),
            cwd: None,
            timeout: None,
            description: None,
            ready_within: None,
            snapshot_env: Vec::new(),
            sys_locks: Vec::new(),
        }
    }
}

fn default_recipe_template() -> bool {
    true
}

impl Recipe {
    pub fn is_resource(&self) -> bool {
        self.destroy.is_some()
    }
}

fn serialize_needs<S: Serializer>(
    needs: &Option<Vec<String>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    needs.as_deref().unwrap_or_default().serialize(serializer)
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdapterChoice {
    pub tool: Option<String>,
    pub disabled: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scope {
    pub bin: Vec<RelPath>,
    #[serde(
        default,
        deserialize_with = "deserialize_commands",
        serialize_with = "serialize_commands"
    )]
    pub commands: IndexMap<String, bool>,
    pub vars: IndexMap<String, ValueOrFalse<String>>,
    pub env: IndexMap<String, ValueOrFalse<String>>,
    pub copy: Vec<RelPath>,
    pub files: IndexMap<String, ValueOrFalse<FileDef>>,
    pub resource: IndexMap<String, ValueOrFalse<Resource>>,
    pub adapters: IndexMap<String, AdapterChoice>,
    #[serde(skip)]
    pub locations: BTreeMap<String, SourceLocation>,
}

fn deserialize_commands<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<IndexMap<String, bool>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Commands {
        Names(Vec<String>),
        Overrides(IndexMap<String, bool>),
    }

    match Commands::deserialize(deserializer)? {
        Commands::Names(names) => {
            let mut commands = IndexMap::new();
            for name in names {
                if commands.insert(name.clone(), true).is_some() {
                    return Err(D::Error::custom(format!(
                        "duplicate commands entry `{name}`"
                    )));
                }
            }
            Ok(commands)
        }
        Commands::Overrides(commands) => Ok(commands),
    }
}

fn serialize_commands<S: Serializer>(
    commands: &IndexMap<String, bool>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if commands.values().all(|claimed| *claimed) {
        commands.keys().collect::<Vec<_>>().serialize(serializer)
    } else {
        commands.serialize(serializer)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceLocation {
    pub path: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for SourceLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}:{}", self.path, self.line, self.col)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Detect {
    pub depth: Option<u8>,
    pub ignore: Option<Vec<RelPath>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hooks {
    pub after_new: Option<Command>,
    pub warm: Option<Command>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(flatten)]
    pub root: Scope,
    pub ports: Vec<PortName>,
    pub dirs: IndexMap<String, Scope>,
    pub sync_inputs: Vec<RelPath>,
    pub hooks: Hooks,
    /// Output roots cloned from the canonical into a new tree. Configured
    /// roots are opaque; adapter roots may additionally carry a sweep
    /// strategy that understands their ecosystem.
    #[serde(rename = "output", default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<Output>,
    pub detect: Detect,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<BranchTemplates>,
}

/// One output root cloned whole into a new tree's snapshot (§11.8).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    #[serde(default)]
    pub kind: OutputKind,
    /// The scope the tool was detected in, `.` at the root: where the
    /// strategy asks the tool about the workspace, and what relative
    /// paths in build output resolve against.
    #[serde(default = "default_output_workspace", skip_serializing)]
    pub workspace: String,
    /// The output root, relative to the tree root.
    pub path: RelPath,
    /// Adapter contributions may select a sweep strategy; configuration
    /// roots may not claim ecosystem knowledge.
    #[serde(skip)]
    pub adapter: bool,
}

fn default_output_workspace() -> String {
    ".".to_owned()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    /// User-declared output: clone and size it, but never inspect or sweep it.
    #[default]
    Opaque,
    /// Cargo's `target/` layout: units under `.fingerprint`, `deps`,
    /// `build` and `incremental` per profile directory.
    Cargo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Adapter,
    Repo,
    User,
    Tree,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectiveScope {
    pub dir: String,
    pub bin: Vec<RelPath>,
    pub commands: Vec<String>,
    pub vars: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub copy: Vec<RelPath>,
    pub files: BTreeMap<String, FileDef>,
    pub resources: BTreeMap<String, Resource>,
    pub adapters: BTreeMap<String, AdapterChoice>,
    pub locations: BTreeMap<String, SourceLocation>,
}

pub fn parse(source: &str, path: &str) -> Result<Config, CoreError> {
    if let Ok(table) = source.parse::<toml::Table>() {
        if let Some(message) = removed_surface(&table) {
            return Err(invalid(message));
        }
    }
    let mut config: Config = toml::from_str(source).map_err(|error| {
        let span = error.span().unwrap_or(0..0);
        let before = &source[..span.start.min(source.len())];
        let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(1, |tail| tail.len() + 1);
        CoreError::new(
            ExitClass::State,
            "CONFIG_INVALID",
            format!("{path}:{line}:{col}: {error}"),
            "fix the configuration error",
        )
    })?;
    record_locations(&mut config, source, path);
    validate(&config)?;
    Ok(config)
}

pub fn parse_current(source: &str, path: &str) -> Result<Config, CoreError> {
    parse(source, path)
}

pub fn removed_surface(table: &toml::Table) -> Option<String> {
    fn in_scope(table: &toml::Table) -> Option<String> {
        if let Some(tasks) = table.get("task") {
            let tasks = tasks.as_table();
            let selected = tasks.and_then(|tasks| {
                ["build", "warm"]
                    .into_iter()
                    .find_map(|id| tasks.get(id).map(|task| (id, task.as_table())))
                    .or_else(|| {
                        ["lock", "needs", "exclusive", "run"]
                            .into_iter()
                            .find_map(|key| {
                                tasks.iter().find_map(|(id, value)| {
                                    value
                                        .as_table()
                                        .filter(|task| task.contains_key(key))
                                        .map(|task| (id.as_str(), Some(task)))
                                })
                            })
                    })
                    .or_else(|| {
                        tasks
                            .iter()
                            .next()
                            .map(|(id, task)| (id.as_str(), task.as_table()))
                    })
            });
            let (id, task) = selected.unwrap_or(("<name>", None));
            let message = if id == "build" {
                "`[task.build]` was replaced by `[hooks] after_new`".to_owned()
            } else if id == "warm" {
                "`[task.warm]` was replaced by `[hooks] warm`".to_owned()
            } else if task.is_some_and(|task| task.contains_key("lock")) {
                format!("task `{id}` key `lock` was replaced by a token pool in `[resource.{id}]`")
            } else if task.is_some_and(|task| task.contains_key("needs")) {
                format!("task `{id}` key `needs` was removed; task dependencies belong in the repository task runner and resource dependencies use `[resource.{id}] needs`")
            } else if task.is_some_and(|task| task.contains_key("exclusive")) {
                format!(
                    "task `{id}` key `exclusive` was replaced by resource `arena` and `capacity`"
                )
            } else if task.is_some_and(|task| task.contains_key("run")) {
                format!("task `{id}` key `run` was removed; use the repository task runner, or `[resource.{id}] create` for durable state")
            } else {
                format!("`[task.{id}]` was removed; use the repository task runner or `[resource.{id}]`")
            };
            return Some(message);
        }
        if table.contains_key("locks") {
            return Some(
            "`[locks]` was replaced by token pools: declare `[resource.<name>]` with `count` and `wait`".to_owned()
            );
        }
        table
            .get("resource")
            .and_then(toml::Value::as_table)
            .and_then(|resources| {
                ["run", "reset", "lock", "exclusive"]
                    .into_iter()
                    .find_map(|key| {
                        resources.iter().find_map(|(id, value)| {
                            value
                                .as_table()
                                .filter(|resource| resource.contains_key(key))
                                .map(|_| {
                                    let replacement = match key {
                                        "run" => "`create`",
                                        "reset" => "`acquire`",
                                        "lock" => "a token pool in `[resource]`",
                                        "exclusive" => "`arena` and `capacity`",
                                        _ => unreachable!(),
                                    };
                                    format!(
                                        "resource `{id}` key `{key}` was replaced by {replacement}"
                                    )
                                })
                        })
                    })
            })
    }

    in_scope(table).or_else(|| {
        table
            .get("dirs")
            .and_then(toml::Value::as_table)
            .and_then(|dirs| {
                dirs.values()
                    .filter_map(toml::Value::as_table)
                    .find_map(in_scope)
            })
    })
}

fn record_locations(config: &mut Config, source: &str, path: &str) {
    let mut section = Vec::<String>::new();
    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let body = trimmed.trim_matches('[').trim_matches(']');
            section = split_toml_path(body);
            continue;
        }
        let Some((raw_key, _)) = trimmed.split_once('=') else {
            continue;
        };
        let key = unquote(raw_key.trim());
        let (scope, table) =
            if section.first().map(String::as_str) == Some("dirs") && section.len() >= 2 {
                let Some(scope) = config.dirs.get_mut(&section[1]) else {
                    continue;
                };
                (scope, &section[2..])
            } else {
                (&mut config.root, section.as_slice())
            };
        let logical = match table {
            [] if key == "branch" => Some("branch".to_owned()),
            [kind] if kind == "vars" => Some(format!("var:{key}")),
            [kind] if kind == "env" => Some(format!("env:{key}")),
            [kind, file] if kind == "files" && key == "content" => Some(format!("file:{file}")),
            [kind, resource]
                if kind == "resource"
                    && matches!(
                        key.as_str(),
                        "name" | "create" | "exists" | "destroy" | "acquire"
                    ) =>
            {
                Some(format!("resource:{resource}:{key}"))
            }
            [kind, resource, env] if kind == "resource" && env == "env" => {
                Some(format!("resource:{resource}:env:{key}"))
            }
            _ => None,
        };
        if let Some(logical) = logical {
            let expression_col = first_template_dollar(line)
                .map_or_else(|| line.len() - trimmed.len() + 1, |index| index + 1);
            scope.locations.insert(
                logical,
                SourceLocation {
                    path: path.to_owned(),
                    line: line_index + 1,
                    col: expression_col,
                },
            );
        }
    }
}

fn first_template_dollar(line: &str) -> Option<usize> {
    let mut characters = line.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if character != '$' {
            continue;
        }
        if characters.peek().is_some_and(|(_, next)| *next == '$') {
            characters.next();
            continue;
        }
        if characters
            .peek()
            .is_some_and(|(_, next)| *next == '{' || next.is_ascii_alphabetic() || *next == '_')
        {
            return Some(index);
        }
    }
    None
}

fn split_toml_path(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in input.chars() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(active), value) if value == active => quote = None,
            (None, '.') => parts.push(std::mem::take(&mut current)),
            _ => current.push(character),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn unquote(input: &str) -> String {
    input
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            input
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(input)
        .to_owned()
}

pub fn validate(config: &Config) -> Result<(), CoreError> {
    if config.detect.depth.is_some_and(|depth| depth > 2) {
        return Err(invalid("detect.depth must be 0, 1, or 2"));
    }
    let mut ports = BTreeSet::new();
    if config.ports.iter().any(|port| !ports.insert(port)) {
        return Err(invalid("port names must be unique"));
    }
    for output in &config.outputs {
        if !output.adapter && output.kind != OutputKind::Opaque {
            return Err(invalid(
                "configured output kind must be `opaque`; sweep strategies are adapter-owned",
            ));
        }
    }
    for command in [config.hooks.after_new.as_ref(), config.hooks.warm.as_ref()]
        .into_iter()
        .flatten()
    {
        if command.texts().is_empty() || command.texts().iter().any(String::is_empty) {
            return Err(invalid("hook command must not be empty"));
        }
        for value in command.texts() {
            template::validate(value)?;
        }
    }
    if let Some(branch) = &config.branch {
        if branch.candidates().is_empty() {
            return Err(invalid("branch must declare at least one template"));
        }
        for candidate in branch.candidates() {
            template::validate_branch(candidate)
                .map_err(|error| located(error, config.root.locations.get("branch")))?;
        }
    }
    for (dir, scope) in std::iter::once((".", &config.root))
        .chain(config.dirs.iter().map(|(dir, scope)| (dir.as_str(), scope)))
    {
        RelDir::new(dir)?;
        validate_scope(scope)?;
    }
    validate_materialized_overlap(&config.root)?;
    for scope in config.dirs.values() {
        validate_materialized_overlap(scope)?;
    }
    Ok(())
}

/// Checks rules that require the effective scope or frozen geometry (§5.6).
pub fn validate_resolved(config: &Config, stride: u8) -> Result<(), CoreError> {
    validate(config)?;
    if config.ports.len() > usize::from(stride) {
        return Err(invalid("declared ports exceed the frozen geometry stride"));
    }
    let ports = config
        .ports
        .iter()
        .map(|port| port.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let functions = template::FunctionValues {
        simple: template::FUNCTIONS
            .iter()
            .map(|name| ((*name).to_owned(), String::new()))
            .collect(),
        ports: ports
            .iter()
            .map(|name| (name.clone(), String::new()))
            .collect(),
        meta: BTreeMap::new(),
    };
    for dir in std::iter::once(".").chain(config.dirs.keys().map(String::as_str)) {
        let effective = effective_scope(config, dir)?;
        validate_var_graph(&effective.vars, &ports, &effective.locations)?;
        template::resolve_vars(&effective.vars, &functions)?;
        if dir == "." {
            for command in [config.hooks.after_new.as_ref(), config.hooks.warm.as_ref()]
                .into_iter()
                .flatten()
            {
                for value in command.texts() {
                    validate_template_value(value, &effective.vars, &ports, None)?;
                }
            }
        }
        for (key, value) in &effective.env {
            validate_template_value(
                value,
                &effective.vars,
                &ports,
                effective.locations.get(&format!("env:{key}")),
            )?;
        }
        for (path, file) in &effective.files {
            if file.template {
                if let Some(content) = &file.content {
                    validate_template_value(
                        content,
                        &effective.vars,
                        &ports,
                        effective.locations.get(&format!("file:{path}")),
                    )?;
                }
            }
        }
        for (resource_id, resource) in &effective.resources {
            if let Some(name) = &resource.name {
                validate_template_value(
                    name,
                    &effective.vars,
                    &ports,
                    effective
                        .locations
                        .get(&format!("resource:{resource_id}:name")),
                )?;
            }
            for (key, value) in &resource.env {
                validate_template_value(
                    value,
                    &effective.vars,
                    &ports,
                    effective
                        .locations
                        .get(&format!("resource:{resource_id}:env:{key}")),
                )?;
            }
            for (field, command) in [
                ("create", resource.create.as_ref()),
                ("exists", resource.exists.as_ref()),
                ("destroy", resource.destroy.as_ref()),
                ("acquire", resource.acquire.as_ref()),
            ]
            .into_iter()
            .filter_map(|(field, command)| command.map(|command| (field, command)))
            {
                if resource.template {
                    for value in command.texts() {
                        validate_template_value(
                            value,
                            &effective.vars,
                            &ports,
                            effective
                                .locations
                                .get(&format!("resource:{resource_id}:{field}")),
                        )?;
                    }
                }
            }
        }
        if effective
            .copy
            .iter()
            .any(|path| effective.files.contains_key(path.as_str()))
        {
            return Err(invalid("copy entries may not also be rendered files"));
        }
        for resource in effective.resources.values() {
            if matches!(resource.tied_to, Some(TiedTo::Repo | TiedTo::Machine)) {
                let mut calls = BTreeSet::new();
                let mut shell_references = BTreeSet::new();
                let mut port_references = BTreeSet::new();
                // An untemplated recipe still reaches a shell, so its `$WT_*`
                // references are still evidence of tree specificity; its braces
                // belong to whatever format owns them and are not wt's to read.
                for command in [
                    resource.create.as_ref(),
                    resource.exists.as_ref(),
                    resource.destroy.as_ref(),
                    resource.acquire.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    if let Command::Shell(shell) = command {
                        shell_references.extend(template::shell_references(shell));
                    }
                }
                if resource.template {
                    for command in [
                        resource.create.as_ref(),
                        resource.exists.as_ref(),
                        resource.destroy.as_ref(),
                        resource.acquire.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        for value in command.texts() {
                            calls.extend(template::calls(value)?);
                            port_references.extend(template::port_references(value)?);
                        }
                    }
                }
                for text in resource.name.iter().chain(resource.env.values()) {
                    calls.extend(template::calls(text)?);
                    port_references.extend(template::port_references(text)?);
                }
                let tree_call = calls.iter().any(|call| {
                    matches!(
                        call.name(),
                        "root" | "branch" | "name" | "name_snake" | "name_short" | "target"
                    )
                }) || !port_references.is_empty();
                let shell_tree_reference = shell_references.iter().any(|name| {
                    matches!(
                        name.as_str(),
                        "WT_ROOT" | "WT_TARGET" | "WT_NAME" | "WT_BRANCH" | "WT_BIN" | "PATH"
                    ) || name.starts_with("WT_PORT_")
                });
                let machine = resource.tied_to == Some(TiedTo::Machine);
                let repo_call = machine
                    && calls
                        .iter()
                        .any(|call| matches!(call.name(), "label" | "repo"));
                let shell_repo_reference = machine
                    && shell_references
                        .iter()
                        .any(|name| matches!(name.as_str(), "WT_LABEL" | "WT_REPO"));
                if tree_call || shell_tree_reference || repo_call || shell_repo_reference {
                    let scope = if machine { "machine" } else { "repo" };
                    let specificity = if machine {
                        "tree- or repo-specific"
                    } else {
                        "tree-specific"
                    };
                    return Err(invalid(format!(
                        "{scope}-tied resource references a {specificity} template function"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_template_value(
    value: &str,
    vars: &BTreeMap<String, String>,
    ports: &BTreeSet<String>,
    location: Option<&SourceLocation>,
) -> Result<(), CoreError> {
    template::validate_calls(value, ports).map_err(|error| located(error, location))?;
    for reference in template::references(value)? {
        if !vars.contains_key(&reference) {
            return Err(located(
                CoreError::new(
                    ExitClass::State,
                    "CONFIG_INVALID",
                    format!("unknown vars name `{reference}`"),
                    "declare the variable in `vars` or correct the reference",
                ),
                location,
            ));
        }
    }
    Ok(())
}

fn validate_var_graph(
    vars: &BTreeMap<String, String>,
    ports: &BTreeSet<String>,
    locations: &BTreeMap<String, SourceLocation>,
) -> Result<(), CoreError> {
    fn visit(
        key: &str,
        vars: &BTreeMap<String, String>,
        ports: &BTreeSet<String>,
        locations: &BTreeMap<String, SourceLocation>,
        complete: &mut BTreeSet<String>,
        active: &mut Vec<String>,
    ) -> Result<(), CoreError> {
        if complete.contains(key) {
            return Ok(());
        }
        if let Some(start) = active.iter().position(|candidate| candidate == key) {
            let mut cycle = active[start..].to_vec();
            cycle.sort();
            cycle.dedup();
            let involved = cycle
                .iter()
                .map(|name| format_involved(name, locations.get(&format!("var:{name}"))))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CoreError::new(
                ExitClass::State,
                "VARS_CYCLE",
                format!("vars cycle involves {involved}"),
                "break the cycle in `vars`",
            ));
        }
        let value = &vars[key];
        let location = locations.get(&format!("var:{key}"));
        template::validate_calls(value, ports).map_err(|error| located(error, location))?;
        active.push(key.to_owned());
        for reference in template::references(value)? {
            if !vars.contains_key(&reference) {
                return Err(located(
                    CoreError::new(
                        ExitClass::State,
                        "VARS_UNKNOWN",
                        format!(
                            "vars `{key}` references unknown name `{reference}`; involved: {}",
                            format_involved(key, location)
                        ),
                        "declare the variable in `vars` or correct the reference",
                    ),
                    location,
                ));
            }
            visit(&reference, vars, ports, locations, complete, active)?;
        }
        active.pop();
        complete.insert(key.to_owned());
        Ok(())
    }

    let mut complete = BTreeSet::new();
    for key in vars.keys() {
        visit(key, vars, ports, locations, &mut complete, &mut Vec::new())?;
    }
    Ok(())
}

fn format_involved(name: &str, location: Option<&SourceLocation>) -> String {
    location.map_or_else(
        || format!("`{name}`"),
        |location| format!("`{name}` at {location}"),
    )
}

fn located(mut error: CoreError, location: Option<&SourceLocation>) -> CoreError {
    if let Some(location) = location {
        error.message = format!("{location}: {}", error.message);
    }
    error
}

fn validate_scope(scope: &Scope) -> Result<(), CoreError> {
    for command in scope.commands.keys() {
        if command.is_empty()
            || matches!(command.as_str(), "wt" | "." | "..")
            || command.contains('/')
            || command.contains('\0')
        {
            return Err(invalid(format!(
                "commands entry `{command}` is invalid: names must be non-empty basenames without `/` or NUL, and `wt`, `.`, and `..` are reserved"
            )));
        }
    }
    for (key, value) in &scope.vars {
        if !valid_var_key(key) {
            return Err(invalid(format!(
                "vars key `{key}` must match [a-z_][a-z0-9_]*"
            )));
        }
        if matches!(key.as_str(), "ports" | "meta") || template::FUNCTIONS.contains(&key.as_str()) {
            return Err(invalid(format!(
                "vars key `{key}` is reserved by the template language"
            )));
        }
        if let Some(value) = value.value() {
            template::validate(value)
                .map_err(|error| located(error, scope.locations.get(&format!("var:{key}"))))?;
        }
    }
    for key in scope.env.keys() {
        EnvKey::new(key)?;
        if key == "PATH" {
            return Err(invalid(
                "PATH is owned by wt and cannot be an environment alias",
            ));
        }
    }
    for (key, value) in &scope.env {
        if let Some(template_value) = value.value() {
            template::validate(template_value)
                .map_err(|error| located(error, scope.locations.get(&format!("env:{key}"))))?;
        }
    }
    for (path, file) in &scope.files {
        RelPath::new(path)?;
        if let Some(file) = file.value() {
            if file.content.is_some() == file.source.is_some() {
                return Err(invalid("a file needs exactly one of content or source"));
            }
            if !is_mode(&file.mode) {
                return Err(invalid("file mode must be a four-digit octal string"));
            }
            if file.template {
                if let Some(content) = &file.content {
                    template::validate(content).map_err(|error| {
                        located(error, scope.locations.get(&format!("file:{path}")))
                    })?;
                }
            }
        }
    }
    for (id, resource) in &scope.resource {
        ResourceId::new(id)?;
        if let Some(resource) = resource.value() {
            validate_resource(id, resource)?;
        }
    }
    Ok(())
}

fn validate_resource(id: &str, resource: &Resource) -> Result<(), CoreError> {
    for duration in [
        resource.timeout.as_deref(),
        resource.ready_within.as_deref(),
        resource.wait.as_deref(),
        resource.keep.as_deref(),
        resource.idle.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if duration != "forever" && !valid_duration(duration) {
            return Err(invalid(
                "duration must be digits followed by ms, s, m, or h",
            ));
        }
    }
    if resource.count == Some(0) || resource.capacity == Some(0) {
        return Err(invalid("resource count and capacity must be at least 1"));
    }
    if resource.token_pool() {
        if resource.name.is_some()
            || !resource.env.is_empty()
            || resource.keep.is_some()
            || resource.idle.is_some()
            || resource.needs.is_some()
            || resource.arena.is_some()
            || resource.capacity.is_some()
            || resource.cwd.is_some()
            || resource.timeout.is_some()
            || resource.description.is_some()
            || resource.ready_within.is_some()
            || !resource.snapshot_env.is_empty()
            || !resource.template
        {
            return Err(invalid(format!(
                "token pool `{id}` may not declare instance fields; only tied_to, count, and wait are valid"
            )));
        }
        return Ok(());
    }
    if resource.count.is_some()
        && resource
            .name
            .as_deref()
            .is_none_or(|name| !name.contains("{{index()}}"))
    {
        return Err(invalid(format!(
            "instance pool `{id}` needs a name containing `{{{{index()}}}}`"
        )));
    }
    if resource.destroy.is_none() || resource.exists.is_none() || resource.tied_to.is_none() {
        return Err(invalid(format!(
            "resource `{id}` needs exists, destroy, and tied_to"
        )));
    }
    if resource.arena.is_some() && resource.capacity.is_none() {
        return Err(invalid(format!(
            "resource `{id}` with arena needs capacity"
        )));
    }
    if resource.capacity.is_some() && resource.arena.is_none() {
        return Err(invalid(format!("resource `{id}` capacity requires arena")));
    }
    Ok(())
}

fn valid_var_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_lowercase() || ch == '_')
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn validate_materialized_overlap(scope: &Scope) -> Result<(), CoreError> {
    let files: BTreeSet<_> = scope
        .files
        .iter()
        .filter(|(_, value)| value.value().is_some())
        .map(|(path, _)| path.as_str())
        .collect();
    if scope.copy.iter().any(|path| files.contains(path.as_str())) {
        return Err(invalid("copy entries may not also be rendered files"));
    }
    Ok(())
}

fn is_mode(value: &str) -> bool {
    value.len() == 4 && value.starts_with('0') && value.chars().all(|ch| ('0'..='7').contains(&ch))
}

fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "CONFIG_INVALID",
        message,
        "fix the configuration",
    )
}

pub fn merge(layers: &[(Layer, Config)]) -> Config {
    let mut output = Config::default();
    let mut layers: Vec<_> = layers.iter().collect();
    layers.sort_by_key(|(layer, _)| *layer);
    for (_, layer) in layers {
        merge_scope(&mut output.root, &layer.root);
        for (dir, scope) in &layer.dirs {
            merge_scope(output.dirs.entry(dir.clone()).or_default(), scope);
        }
        if !layer.ports.is_empty() {
            output.ports.clone_from(&layer.ports);
        }
        append_unique(&mut output.sync_inputs, &layer.sync_inputs);
        append_unique(&mut output.outputs, &layer.outputs);
        if layer.hooks.after_new.is_some() {
            output.hooks.after_new.clone_from(&layer.hooks.after_new);
        }
        if layer.hooks.warm.is_some() {
            output.hooks.warm.clone_from(&layer.hooks.warm);
        }
        if layer.branch.is_some() {
            output.branch.clone_from(&layer.branch);
        }
        if layer.detect.depth.is_some() {
            output.detect.depth = layer.detect.depth;
        }
        if layer.detect.ignore.is_some() {
            output.detect.ignore.clone_from(&layer.detect.ignore);
        }
    }
    output
}

fn merge_scope(target: &mut Scope, source: &Scope) {
    merge_bins(&mut target.bin, &source.bin);
    for (name, claimed) in &source.commands {
        target.commands.insert(name.clone(), *claimed);
    }
    append_unique(&mut target.copy, &source.copy);
    merge_deletable(&mut target.vars, &source.vars);
    merge_deletable(&mut target.env, &source.env);
    merge_deletable(&mut target.files, &source.files);
    merge_deletable(&mut target.resource, &source.resource);
    for (id, choice) in &source.adapters {
        let current = target.adapters.entry(id.clone()).or_default();
        if choice.tool.is_some() {
            current.tool.clone_from(&choice.tool);
        }
        if choice.disabled.is_some() {
            current.disabled = choice.disabled;
        }
    }
    target.locations.extend(source.locations.clone());
}

fn merge_deletable<T: Clone>(
    target: &mut IndexMap<String, ValueOrFalse<T>>,
    source: &IndexMap<String, ValueOrFalse<T>>,
) {
    for (key, value) in source {
        // Keep deletion markers until directory scopes are accumulated. A
        // nearer scope may delete a value inherited from an outer scope even
        // when the deleted key was absent in this layer at the nearer scope.
        target.insert(key.clone(), value.clone());
    }
}

fn append_unique<T: Clone + Eq>(target: &mut Vec<T>, values: &[T]) {
    for value in values {
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
}

fn merge_bins(target: &mut Vec<RelPath>, source: &[RelPath]) {
    let mut bins = Vec::new();
    append_unique(&mut bins, source);
    target.retain(|path| !bins.contains(path));
    bins.append(target);
    *target = bins;
}

pub fn scope_chain(config: &Config, cwd: &str) -> Result<Vec<String>, CoreError> {
    let cwd = RelDir::new(cwd)?;
    let mut candidates = Vec::new();
    let mut current = cwd.as_str();
    loop {
        if current == "." || config.dirs.contains_key(current) {
            candidates.push(current.to_owned());
        }
        if current == "." {
            break;
        }
        current = current.rsplit_once('/').map_or(".", |(parent, _)| parent);
    }
    Ok(candidates)
}

pub fn effective_scope(config: &Config, cwd: &str) -> Result<EffectiveScope, CoreError> {
    let mut chain = scope_chain(config, cwd)?;
    chain.reverse();
    let mut output = EffectiveScope {
        dir: cwd.to_owned(),
        commands: std::iter::once(&config.root)
            .chain(config.dirs.values())
            .flat_map(|scope| {
                scope
                    .commands
                    .iter()
                    .filter(|(_, claimed)| **claimed)
                    .map(|(name, _)| name.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        ..EffectiveScope::default()
    };
    for dir in chain {
        let scope = if dir == "." {
            &config.root
        } else {
            &config.dirs[&dir]
        };
        merge_bins(&mut output.bin, &scope.bin);
        append_unique(&mut output.copy, &scope.copy);
        apply_values(&mut output.vars, &scope.vars);
        apply_values(&mut output.env, &scope.env);
        apply_values(&mut output.files, &scope.files);
        apply_values(&mut output.resources, &scope.resource);
        for (id, choice) in &scope.adapters {
            output.adapters.insert(id.clone(), choice.clone());
        }
        output.locations.extend(scope.locations.clone());
    }
    Ok(output)
}

fn apply_values<T: Clone>(
    target: &mut BTreeMap<String, T>,
    source: &IndexMap<String, ValueOrFalse<T>>,
) {
    for (key, value) in source {
        match value {
            ValueOrFalse::Value(value) => {
                target.insert(key.clone(), value.clone());
            }
            ValueOrFalse::False => {
                target.remove(key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_convention_layers_and_is_validated_where_it_is_written() {
        let repo = parse("branch = \"{{meta.ticket}}_{{name()}}\"\n", ".wt.toml").unwrap();
        assert_eq!(
            repo.branch.as_ref().unwrap().candidates(),
            ["{{meta.ticket}}_{{name()}}".to_owned()]
        );
        let user = parse(
            "branch = [\"{{meta.ticket}}/{{name()}}\", \"wip/{{name()}}\"]\n",
            "config.toml",
        )
        .unwrap();
        let merged = merge(&[(Layer::Repo, repo), (Layer::User, user)]);
        assert_eq!(
            merged.branch.unwrap().candidates(),
            [
                "{{meta.ticket}}/{{name()}}".to_owned(),
                "wip/{{name()}}".to_owned()
            ]
        );

        let error = parse("branch = \"{{ports.http}}\"\n", ".wt.toml").unwrap_err();
        assert_eq!(error.code.0, "CONFIG_INVALID");
        assert!(
            error.message.starts_with(".wt.toml:1:1:"),
            "{}",
            error.message
        );
        let error = parse("[vars]\nname_for = \"{{meta.ticket}}\"\n", ".wt.toml").unwrap_err();
        assert!(error.message.contains("`meta.ticket`"), "{}", error.message);
        assert!(parse("[vars]\nmeta = \"x\"\n", ".wt.toml").is_err());
        assert!(parse("branch = []\n", ".wt.toml").is_err());
    }

    #[test]
    fn parses_all_acceptance_files_unchanged() {
        for (name, source) in [
            (
                "orbit",
                include_str!("../../../spec/acceptance/orbit.wt.toml"),
            ),
            (
                "orbitcloud",
                include_str!("../../../spec/acceptance/orbitcloud.wt.toml"),
            ),
            (
                "orbittool",
                include_str!("../../../spec/acceptance/orbittool.wt.toml"),
            ),
            (
                "orbitapp",
                include_str!("../../../spec/acceptance/orbitapp.wt.toml"),
            ),
            (
                "orbitmono",
                include_str!("../../../spec/acceptance/orbitmono.wt.toml"),
            ),
        ] {
            let config = parse_current(source, name).unwrap();
            validate_resolved(&config, 16).unwrap();
        }
    }

    #[test]
    fn merge_deletes_entries_and_scopes_accumulate() {
        let low = parse("bin=['root']\n[env]\nA='a'\n[dirs.sub.env]\nB='b'", "low").unwrap();
        let high = parse("bin=['high']\n[env]\nA=false", "high").unwrap();
        let merged = merge(&[(Layer::Repo, low), (Layer::Tree, high)]);
        let scope = effective_scope(&merged, "sub").unwrap();
        assert!(!scope.env.contains_key("A"));
        assert_eq!(scope.env["B"], "b");
        assert_eq!(
            scope.bin.iter().map(RelPath::as_str).collect::<Vec<_>>(),
            ["high", "root"]
        );
    }

    #[test]
    fn nested_scope_deletions_survive_layer_merge_until_scope_resolution() {
        let repo = parse(
            "[env]\nA='root'\n[files.generated]\ncontent='root'\n[resource.cache]\ntied_to='tree'\nexists='true'\ndestroy='true'",
            "repo",
        )
        .unwrap();
        let tree = parse(
            "[dirs.sub.env]\nA=false\n[dirs.sub.files]\ngenerated=false\n[dirs.sub.resource]\ncache=false",
            "tree",
        )
        .unwrap();
        let merged = merge(&[(Layer::Repo, repo), (Layer::Tree, tree)]);

        assert!(matches!(merged.dirs["sub"].env["A"], ValueOrFalse::False));
        let root = effective_scope(&merged, ".").unwrap();
        assert_eq!(root.env["A"], "root");
        assert!(root.files.contains_key("generated"));
        assert!(root.resources.contains_key("cache"));
        let nested = effective_scope(&merged, "sub").unwrap();
        assert!(!nested.env.contains_key("A"));
        assert!(!nested.files.contains_key("generated"));
        assert!(!nested.resources.contains_key("cache"));
    }

    #[test]
    fn rejects_unknown_keys_with_location() {
        let error = parse("mystery = true", "repo/.wt.toml").unwrap_err();
        assert!(error.message.starts_with("repo/.wt.toml:1:"));
    }

    #[test]
    fn resolved_validation_treats_env_names_as_vars_and_rejects_repo_tree_functions() {
        let chained = parse("[env]\nA='{{B}}'\nB='value'", "x").unwrap();
        assert_eq!(
            validate_resolved(&chained, 16).unwrap_err().code.0,
            "CONFIG_INVALID"
        );
        let repo = parse(
            "[resource.db]\ntied_to='repo'\nexists='test -e $WT_ROOT'\ndestroy='drop'",
            "x",
        )
        .unwrap();
        assert!(validate_resolved(&repo, 16)
            .unwrap_err()
            .message
            .contains("tree-specific"));

        let opaque = parse(
            "[resource.db]\ntied_to='repo'\nexists='test -e ${root()}'\ndestroy='drop'",
            "x",
        )
        .unwrap();
        assert!(validate_resolved(&opaque, 16).is_ok());

        let templated = parse(
            "[resource.db]\ntied_to='repo'\nexists=\"test -e '{{root()}}'\"\ndestroy='drop'",
            "x",
        )
        .unwrap();
        assert!(validate_resolved(&templated, 16)
            .unwrap_err()
            .message
            .contains("tree-specific"));
    }

    #[test]
    fn section_5_6_machine_scope_rejects_tree_and_repo_specific_references() {
        for reference in [
            "$WT_ROOT",
            "$WT_LABEL",
            "$WT_REPO",
            "{{root()}}",
            "{{label()}}",
            "{{repo()}}",
        ] {
            let source = if reference.starts_with('$') {
                format!(
                    "[resource.setup]\ntied_to='machine'\nexists='test -n {reference}'\ndestroy='true'"
                )
            } else {
                format!(
                    "[resource.setup]\ntied_to='machine'\nexists='true'\ndestroy='true'\n[resource.setup.env]\nVALUE='{reference}'"
                )
            };
            let config = parse(&source, "x").unwrap();
            let error = validate_resolved(&config, 16).unwrap_err();
            assert!(
                error.message.contains("machine-tied"),
                "{reference}: {error:?}"
            );
            assert!(
                error.message.contains("tree- or repo-specific"),
                "{reference}: {error:?}"
            );
        }

        let portable = parse(
            "[resource.setup]\ntied_to='machine'\nexists='test -n $WT_HOME'\ndestroy='true'\n[resource.setup.env]\nVALUE='{{home()}}'",
            "x",
        )
        .unwrap();
        assert!(validate_resolved(&portable, 16).is_ok());
    }

    #[test]
    fn only_false_is_a_deletion_value() {
        assert!(parse("[env]\nA=false", "x").is_ok());
        assert!(parse("[env]\nA=true", "x")
            .unwrap_err()
            .message
            .contains("only `false` deletes"));
        assert!(parse("[resource.test]\ncreate=true", "x").is_err());
    }

    #[test]
    fn resolved_validation_applies_every_scope_sensitive_row() {
        let alias = parse("[dirs.sub.env]\nA='{{B}}'\nB='one'", "x").unwrap();
        assert_eq!(
            validate_resolved(&alias, 16).unwrap_err().code.0,
            "CONFIG_INVALID"
        );

        let unknown = parse("[dirs.sub.env]\nA='{{nonsense}}'", "x").unwrap();
        let error = validate_resolved(&unknown, 16).unwrap_err();
        assert_eq!(error.code.0, "CONFIG_INVALID");
        assert!(error.message.contains("nonsense"));

        assert!(parse(
            "[dirs.sub]\ncopy=['generated']\n[dirs.sub.files.generated]\ncontent='x'",
            "x",
        )
        .is_err());
    }

    #[test]
    fn detect_fields_merge_even_when_the_override_equals_the_default() {
        let low = parse("detect={depth=2,ignore=['vendor']}", "low").unwrap();
        let high = parse("detect={depth=1}", "high").unwrap();
        let merged = merge(&[(Layer::Repo, low), (Layer::Tree, high)]);
        assert_eq!(merged.detect.depth, Some(1));
        assert_eq!(merged.detect.ignore.unwrap()[0].as_str(), "vendor");
    }

    #[test]
    fn validation_covers_port_mode_duration_and_readiness_rows() {
        assert!(parse("ports=['http','http']", "x").is_err());
        let too_many = parse("ports=['a','b']", "x").unwrap();
        assert!(validate_resolved(&too_many, 1).is_err());
        assert!(parse("[files.generated]\ncontent='x'\nmode='644'", "x").is_err());
        assert!(parse(
            "[resource.t]\ntied_to='tree'\nexists='true'\ndestroy='true'\ntimeout='soon'",
            "x"
        )
        .is_err());
        assert!(parse(
            "[resource.t]\ntied_to='tree'\ndestroy='true'\nready_within='1s'",
            "x"
        )
        .is_err());
    }

    #[test]
    fn strict_template_validation_covers_shell_commands_and_file_opt_out() {
        let config = parse(
            "[resource.t]\ntied_to='tree'\nexists='true'\ncreate='echo {{ name}}'\ndestroy='true'",
            "repo/.wt.toml",
        )
        .unwrap();
        let error = validate_resolved(&config, 16).unwrap_err();
        assert_eq!(error.code.0, "CONFIG_INVALID");
        assert!(error.message.contains("repo/.wt.toml:4:"));
        assert!(error.message.contains("whitespace"));

        assert!(parse(
            "[files.generated]\ncontent='{{ jinja }}'\ntemplate=false",
            "repo/.wt.toml",
        )
        .is_ok());
    }

    #[test]
    fn an_untemplated_resource_carries_foreign_braces_but_still_shows_its_tree_specificity() {
        let serve = |extra: &str, tied_to: &str| {
            format!(
                "[resource.serve]\n{extra}tied_to='{tied_to}'\n\
                 exists='docker inspect -f \"{{{{.State.Running}}}}\" \"$WT_ROOT\"'\n\
                 destroy='docker rm -f x'"
            )
        };
        let templated = parse(&serve("", "tree"), "repo/.wt.toml").unwrap();
        let templated = validate_resolved(&templated, 16).unwrap_err();
        assert_eq!(templated.code.0, "CONFIG_INVALID");

        let source = serve("template=false\n", "tree");
        let config = parse(&source, "repo/.wt.toml").unwrap();
        assert!(validate_resolved(&config, 16).is_ok());

        // The opt-out covers the recipes, not the values wt supplies to them.
        let name = parse(&format!("{source}\nname='{{{{ bad }}}}'"), "repo/.wt.toml").unwrap();
        let name = validate_resolved(&name, 16).unwrap_err();
        assert_eq!(name.code.0, "CONFIG_INVALID");
        assert!(name.message.contains("whitespace"));

        // `$WT_ROOT` still reaches the shell, so it still betrays a repo-tied
        // resource reaching for one tree.
        let repo_tied = parse(&serve("template=false\n", "repo"), "repo/.wt.toml").unwrap();
        let error = validate_resolved(&repo_tied, 16).unwrap_err();
        assert!(error.message.contains("tree-specific"), "{}", error.message);
    }

    #[test]
    fn removed_seed_key_is_refused_as_unknown() {
        let config = parse("seed=['cache']", "repo/.wt.toml").unwrap_err();
        assert_eq!(config.code.0, "CONFIG_INVALID");
        assert!(config.message.contains("unknown field `seed`"));
    }

    #[test]
    fn commands_are_a_sorted_union_across_layers_and_all_scopes() {
        let repo = parse(
            "commands=['zeta','alpha']\n[dirs.sub]\ncommands=['nested']",
            "repo/.wt.toml",
        )
        .unwrap();
        let overlay = parse("commands=['alpha','beta']", "repo/.wt/config.toml").unwrap();
        let merged = merge(&[(Layer::Repo, repo), (Layer::Tree, overlay)]);
        assert_eq!(
            effective_scope(&merged, ".").unwrap().commands,
            ["alpha", "beta", "nested", "zeta"]
        );
        assert_eq!(
            effective_scope(&merged, "sub").unwrap().commands,
            ["alpha", "beta", "nested", "zeta"]
        );
    }

    #[test]
    fn commands_can_delete_adapter_claims_and_reserve_dispatch_names() {
        let adapter = parse("commands=['orbit','helper']", "adapter.toml").unwrap();
        let repo = parse("commands={orbit=false, local=true}", "repo/.wt.toml").unwrap();
        let merged = merge(&[(Layer::Adapter, adapter), (Layer::Repo, repo)]);
        assert_eq!(
            effective_scope(&merged, ".").unwrap().commands,
            ["helper", "local"]
        );

        for name in ["wt", ".", "..", "path/name"] {
            let error = parse(&format!("commands={{'{name}'=true}}"), "repo/.wt.toml").unwrap_err();
            assert_eq!(error.code.0, "CONFIG_INVALID");
            assert!(error.message.contains("reserved") || error.message.contains("basename"));
        }
    }

    #[test]
    fn vars_failures_name_every_involved_key_with_source_locations() {
        let cycle = parse(
            "[vars]\nfirst='{{second}}'\nsecond='{{first}}'",
            "repo/.wt.toml",
        )
        .unwrap();
        let error = validate_resolved(&cycle, 16).unwrap_err();
        assert_eq!(error.code.0, "VARS_CYCLE");
        assert!(error.message.contains("`first` at repo/.wt.toml:2:"));
        assert!(error.message.contains("`second` at repo/.wt.toml:3:"));

        let unknown = parse("[vars]\nknown='{{missing}}'", "repo/.wt.toml").unwrap();
        let error = validate_resolved(&unknown, 16).unwrap_err();
        assert_eq!(error.code.0, "VARS_UNKNOWN");
        assert!(error.message.contains("missing"));
        assert!(error.message.contains("repo/.wt.toml:2:"));
    }

    #[test]
    fn unknown_functions_and_ports_name_the_call_and_location() {
        for (source, call) in [
            ("[env]\nA='{{mystery()}}'", "mystery()"),
            (
                "ports=['http']\n[env]\nA=\"{{ports.admin}}\"",
                "ports.admin",
            ),
        ] {
            let config = parse(source, "repo/.wt.toml").unwrap();
            let error = validate_resolved(&config, 16).unwrap_err();
            assert_eq!(error.code.0, "CONFIG_INVALID");
            assert!(error.message.contains(call));
            assert!(error.message.contains("repo/.wt.toml:"));
        }
    }
}
