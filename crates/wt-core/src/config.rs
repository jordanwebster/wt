use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    error::CoreError,
    model::{valid_duration, EnvKey, PortName, RelDir, RelPath, TaskId},
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDef {
    pub content: Option<String>,
    pub source: Option<RelPath>,
    #[serde(default = "default_marker")]
    pub marker: String,
    #[serde(default = "default_mode")]
    pub mode: String,
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
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Task {
    pub run: Option<Command>,
    pub exists: Option<Command>,
    pub destroy: Option<Command>,
    pub needs: Vec<String>,
    pub lock: Option<String>,
    pub name: Option<String>,
    pub tied_to: Option<TiedTo>,
    pub env: IndexMap<String, String>,
    pub cwd: Option<RelPath>,
    pub timeout: Option<String>,
    pub description: Option<String>,
    pub ready_within: Option<String>,
    pub snapshot_env: Vec<String>,
    #[serde(skip)]
    pub sys_locks: Vec<String>,
}

impl Task {
    pub fn is_resource(&self) -> bool {
        self.destroy.is_some()
    }
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
    pub commands: Vec<String>,
    pub vars: IndexMap<String, ValueOrFalse<String>>,
    pub env: IndexMap<String, ValueOrFalse<String>>,
    pub copy: Vec<RelPath>,
    pub files: IndexMap<String, ValueOrFalse<FileDef>>,
    pub task: IndexMap<String, ValueOrFalse<Task>>,
    pub adapters: IndexMap<String, AdapterChoice>,
    #[serde(skip)]
    pub locations: BTreeMap<String, SourceLocation>,
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
pub struct Config {
    #[serde(flatten)]
    pub root: Scope,
    pub ports: Vec<PortName>,
    pub dirs: IndexMap<String, Scope>,
    pub seed: Vec<RelPath>,
    /// Subset of `seed` contributed by built-in adapters. Executors use this
    /// identity to apply the reflink-only fallback rule from SPEC §6.1.
    #[serde(skip)]
    pub adapter_seed: Vec<RelPath>,
    pub sync_inputs: Vec<RelPath>,
    pub detect: Detect,
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
    pub tasks: BTreeMap<String, Task>,
    pub adapters: BTreeMap<String, AdapterChoice>,
    pub locations: BTreeMap<String, SourceLocation>,
}

pub fn parse(source: &str, path: &str) -> Result<Config, CoreError> {
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
            [kind] if kind == "vars" => Some(format!("var:{key}")),
            [kind] if kind == "env" => Some(format!("env:{key}")),
            [kind, file] if kind == "files" && key == "content" => Some(format!("file:{file}")),
            [kind, task]
                if kind == "task"
                    && matches!(key.as_str(), "name" | "run" | "exists" | "destroy") =>
            {
                Some(format!("task:{task}:{key}"))
            }
            [kind, task, env] if kind == "task" && env == "env" => {
                Some(format!("task:{task}:env:{key}"))
            }
            _ => None,
        };
        if let Some(logical) = logical {
            let expression_col = line
                .find("${")
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
    for (dir, scope) in std::iter::once((".", &config.root))
        .chain(config.dirs.iter().map(|(dir, scope)| (dir.as_str(), scope)))
    {
        RelDir::new(dir)?;
        validate_scope(scope)?;
    }
    validate_materialized_overlap(&config.root, &config.seed)?;
    for scope in config.dirs.values() {
        validate_materialized_overlap(scope, &[])?;
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
    };
    for dir in std::iter::once(".").chain(config.dirs.keys().map(String::as_str)) {
        let effective = effective_scope(config, dir)?;
        validate_var_graph(&effective.vars, &ports, &effective.locations)?;
        template::resolve_vars(&effective.vars, &functions)?;
        for (key, value) in &effective.env {
            validate_template_value(
                value,
                &effective.vars,
                &ports,
                effective.locations.get(&format!("env:{key}")),
            )?;
        }
        for (path, file) in &effective.files {
            if let Some(content) = &file.content {
                validate_template_value(
                    content,
                    &effective.vars,
                    &ports,
                    effective.locations.get(&format!("file:{path}")),
                )?;
            }
        }
        for (task_id, task) in &effective.tasks {
            if let Some(name) = &task.name {
                validate_template_value(
                    name,
                    &effective.vars,
                    &ports,
                    effective.locations.get(&format!("task:{task_id}:name")),
                )?;
            }
            for (key, value) in &task.env {
                validate_template_value(
                    value,
                    &effective.vars,
                    &ports,
                    effective
                        .locations
                        .get(&format!("task:{task_id}:env:{key}")),
                )?;
            }
            for command in [
                task.run.as_ref(),
                task.exists.as_ref(),
                task.destroy.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                match command {
                    Command::Shell(_) => {}
                    Command::Argv(argv) => {
                        for value in argv {
                            validate_template_value(value, &effective.vars, &ports, None)?;
                        }
                    }
                }
            }
        }
        if effective
            .copy
            .iter()
            .chain(&config.seed)
            .any(|path| effective.files.contains_key(path.as_str()))
        {
            return Err(invalid(
                "copy or seed entries may not also be rendered files",
            ));
        }
        for task in effective.tasks.values() {
            if task.tied_to == Some(TiedTo::Repo) {
                let mut calls = BTreeSet::new();
                let mut legacy_references = BTreeSet::new();
                for command in [
                    task.run.as_ref(),
                    task.exists.as_ref(),
                    task.destroy.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    match command {
                        Command::Shell(shell) => {
                            legacy_references.extend(template::shell_references(shell));
                        }
                        Command::Argv(argv) => {
                            for value in argv {
                                calls.extend(template::calls(value)?);
                            }
                        }
                    }
                }
                for text in task.name.iter().chain(task.env.values()) {
                    calls.extend(template::calls(text)?);
                }
                let tree_call = calls.iter().any(|call| {
                    matches!(
                        call.name(),
                        "root" | "branch" | "name" | "name_snake" | "name_short" | "target"
                    )
                }) || task.name.iter().chain(task.env.values()).any(|text| {
                    template::port_references(text).is_ok_and(|ports| !ports.is_empty())
                });
                let legacy_tree_reference = legacy_references.iter().any(|name| {
                    matches!(
                        name.as_str(),
                        "WT_ROOT"
                            | "WT_TARGET"
                            | "WT_NAME"
                            | "WT_NAME_SNAKE"
                            | "WT_NAME_SHORT"
                            | "WT_SLOT"
                            | "WT_SESSION"
                            | "WT_BIN"
                            | "PATH"
                    ) || name.starts_with("WT_PORT_")
                });
                if tree_call || legacy_tree_reference {
                    return Err(invalid(
                        "repo-tied resource references a tree-specific template function",
                    ));
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
                    "VARS_UNKNOWN",
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
    let mut commands = BTreeSet::new();
    for command in &scope.commands {
        if command.is_empty()
            || command.contains('/')
            || command.contains('\0')
            || !commands.insert(command)
        {
            return Err(invalid(
                "commands entries must be unique, non-empty basenames without NUL",
            ));
        }
    }
    for (key, value) in &scope.vars {
        if !valid_var_key(key) {
            return Err(invalid(format!(
                "vars key `{key}` must match [a-z_][a-z0-9_]*"
            )));
        }
        if key == "ports" || template::FUNCTIONS.contains(&key.as_str()) {
            return Err(invalid(format!(
                "vars key `{key}` is reserved by the template language"
            )));
        }
        if let Some(value) = value.value() {
            template::validate(value)?;
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
    for value in scope.env.values() {
        if let Some(template_value) = value.value() {
            template::validate(template_value)?;
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
            if let Some(content) = &file.content {
                template::validate(content)?;
            }
        }
    }
    for (id, task) in &scope.task {
        TaskId::new(id)?;
        if let Some(task) = task.value() {
            validate_task(task)?;
        }
    }
    Ok(())
}

fn valid_var_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_lowercase() || ch == '_')
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn validate_task(task: &Task) -> Result<(), CoreError> {
    if task.run.is_none() && task.destroy.is_none() {
        return Err(invalid("a task needs run or destroy"));
    }
    if task.destroy.is_some() && (task.exists.is_none() || task.tied_to.is_none()) {
        return Err(invalid("a resource needs exists and tied_to"));
    }
    if task.ready_within.is_some() && task.exists.is_none() {
        return Err(invalid("ready_within requires exists"));
    }
    for duration in [task.timeout.as_deref(), task.ready_within.as_deref()]
        .into_iter()
        .flatten()
    {
        if !valid_duration(duration) {
            return Err(invalid(
                "duration must be digits followed by ms, s, m, or h",
            ));
        }
    }
    if let Some(name) = &task.name {
        template::validate(name)?;
    }
    for key in &task.snapshot_env {
        EnvKey::new(key)?;
    }
    for (key, value) in &task.env {
        EnvKey::new(key)?;
        if key == "PATH" {
            return Err(invalid(
                "PATH is owned by wt and cannot be task environment",
            ));
        }
        template::validate(value)?;
    }
    for command in [
        task.run.as_ref(),
        task.exists.as_ref(),
        task.destroy.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if let Command::Argv(argv) = command {
            for text in argv {
                template::validate(text)?;
            }
        }
    }
    Ok(())
}

fn validate_materialized_overlap(scope: &Scope, seed: &[RelPath]) -> Result<(), CoreError> {
    let files: BTreeSet<_> = scope
        .files
        .iter()
        .filter(|(_, value)| value.value().is_some())
        .map(|(path, _)| path.as_str())
        .collect();
    if scope
        .copy
        .iter()
        .chain(seed)
        .any(|path| files.contains(path.as_str()))
    {
        return Err(invalid(
            "copy or seed entries may not also be rendered files",
        ));
    }
    if scope.copy.iter().any(|copy| seed.contains(copy)) {
        return Err(invalid("copy and seed entries must be disjoint"));
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
    for (kind, layer) in layers {
        merge_scope(&mut output.root, &layer.root);
        for (dir, scope) in &layer.dirs {
            merge_scope(output.dirs.entry(dir.clone()).or_default(), scope);
        }
        if !layer.ports.is_empty() {
            output.ports.clone_from(&layer.ports);
        }
        append_unique(&mut output.seed, &layer.seed);
        append_unique(&mut output.adapter_seed, &layer.adapter_seed);
        if *kind != Layer::Adapter {
            output
                .adapter_seed
                .retain(|path| !layer.seed.contains(path));
        }
        append_unique(&mut output.sync_inputs, &layer.sync_inputs);
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
    append_unique(&mut target.commands, &source.commands);
    target.commands.sort();
    append_unique(&mut target.copy, &source.copy);
    merge_deletable(&mut target.vars, &source.vars);
    merge_deletable(&mut target.env, &source.env);
    merge_deletable(&mut target.files, &source.files);
    merge_deletable(&mut target.task, &source.task);
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
            .flat_map(|scope| scope.commands.iter().cloned())
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
        apply_values(&mut output.tasks, &scope.task);
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
    fn parses_all_acceptance_files_unchanged() {
        let orbit = parse(
            include_str!("../../../spec/acceptance/orbit.wt.toml"),
            "orbit",
        )
        .unwrap();
        assert_eq!(orbit.root.bin[0].as_str(), "target/debug");
        assert!(orbit.root.task["daemon"].value().unwrap().is_resource());
        validate_resolved(&orbit, 16).unwrap();

        let app = parse(
            include_str!("../../../spec/acceptance/orbitapp.wt.toml"),
            "orbitapp",
        )
        .unwrap();
        assert_eq!(app.ports[0].as_str(), "metro");
        assert_eq!(app.root.task["ios"].value().unwrap().needs, ["orbit-src"]);
        validate_resolved(&app, 16).unwrap();

        let cloud = parse(
            include_str!("../../../spec/acceptance/orbitcloud.wt.toml"),
            "orbitcloud",
        )
        .unwrap();
        assert_eq!(cloud.ports.len(), 7);
        let pgdata = cloud.root.task["pgdata"].value().unwrap();
        assert!(pgdata.run.is_none());
        assert!(pgdata.destroy.is_some());
        assert!(matches!(
            pgdata.exists.as_ref(),
            Some(Command::Shell(command))
                if command.starts_with("docker info >/dev/null 2>&1 || exit 2;")
        ));
        validate_resolved(&cloud, 16).unwrap();
    }

    #[test]
    fn merge_deletes_entries_and_scopes_accumulate() {
        let low = parse(
            "bin=['root']\n[env]\nA='a'\n[task.test]\nrun='one'\n[dirs.sub.env]\nB='b'",
            "low",
        )
        .unwrap();
        let high = parse(
            "bin=['high']\n[env]\nA=false\n[task.test]\nrun='two'",
            "high",
        )
        .unwrap();
        let merged = merge(&[(Layer::Repo, low), (Layer::Tree, high)]);
        let scope = effective_scope(&merged, "sub").unwrap();
        assert!(!scope.env.contains_key("A"));
        assert_eq!(scope.env["B"], "b");
        assert_eq!(
            scope.tasks["test"].run,
            Some(Command::Shell("two".to_owned()))
        );
        assert_eq!(
            scope.bin.iter().map(RelPath::as_str).collect::<Vec<_>>(),
            ["high", "root"]
        );
    }

    #[test]
    fn nested_scope_deletions_survive_layer_merge_until_scope_resolution() {
        let repo = parse(
            "[env]\nA='root'\n[files.generated]\ncontent='root'\n[task.test]\nrun='root'",
            "repo",
        )
        .unwrap();
        let tree = parse(
            "[dirs.sub.env]\nA=false\n[dirs.sub.files]\ngenerated=false\n[dirs.sub.task]\ntest=false",
            "tree",
        )
        .unwrap();
        let merged = merge(&[(Layer::Repo, repo), (Layer::Tree, tree)]);

        assert!(matches!(merged.dirs["sub"].env["A"], ValueOrFalse::False));
        let root = effective_scope(&merged, ".").unwrap();
        assert_eq!(root.env["A"], "root");
        assert!(root.files.contains_key("generated"));
        assert!(root.tasks.contains_key("test"));
        let nested = effective_scope(&merged, "sub").unwrap();
        assert!(!nested.env.contains_key("A"));
        assert!(!nested.files.contains_key("generated"));
        assert!(!nested.tasks.contains_key("test"));
    }

    #[test]
    fn validation_rejects_resource_without_probe_or_scope() {
        let error = parse("[task.db]\ndestroy='drop'", "x").unwrap_err();
        assert_eq!(error.code.0, "CONFIG_INVALID");
    }

    #[test]
    fn rejects_unknown_keys_with_location() {
        let error = parse("mystery = true", "repo/.wt.toml").unwrap_err();
        assert!(error.message.starts_with("repo/.wt.toml:1:"));
    }

    #[test]
    fn resolved_validation_treats_env_names_as_vars_and_rejects_repo_tree_functions() {
        let chained = parse("[env]\nA='${B}'\nB='value'", "x").unwrap();
        assert_eq!(
            validate_resolved(&chained, 16).unwrap_err().code.0,
            "VARS_UNKNOWN"
        );
        let repo = parse(
            "[task.db]\ntied_to='repo'\nexists='test -e $WT_ROOT'\ndestroy='drop'",
            "x",
        )
        .unwrap();
        assert!(validate_resolved(&repo, 16)
            .unwrap_err()
            .message
            .contains("tree-specific"));

        let opaque = parse(
            "[task.db]\ntied_to='repo'\nexists='test -e ${root()}'\ndestroy='drop'",
            "x",
        )
        .unwrap();
        assert!(validate_resolved(&opaque, 16).is_ok());
    }

    #[test]
    fn only_false_is_a_deletion_value() {
        assert!(parse("[env]\nA=false", "x").is_ok());
        assert!(parse("[env]\nA=true", "x")
            .unwrap_err()
            .message
            .contains("only `false` deletes"));
        assert!(parse("[task.test]\nrun='true'\n[task]\ntest=true", "x").is_err());
    }

    #[test]
    fn resolved_validation_applies_every_scope_sensitive_row() {
        let task_cycle = parse(
            "[dirs.sub.task.t]\nrun='true'\n[dirs.sub.task.t.env]\nA='${missing}'\nB='one'",
            "x",
        )
        .unwrap();
        assert_eq!(
            validate_resolved(&task_cycle, 16).unwrap_err().code.0,
            "VARS_UNKNOWN"
        );

        let alias = parse("[dirs.sub.env]\nA='${B}'\nB='one'", "x").unwrap();
        assert_eq!(
            validate_resolved(&alias, 16).unwrap_err().code.0,
            "VARS_UNKNOWN"
        );

        let unknown = parse("[dirs.sub.env]\nA='${nonsense}'", "x").unwrap();
        assert!(validate_resolved(&unknown, 16)
            .unwrap_err()
            .message
            .contains("nonsense"));

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
        assert!(parse("[task.t]\nrun='true'\ntimeout='soon'", "x").is_err());
        assert!(parse("[task.t]\nrun='true'\nready_within='1s'", "x").is_err());
    }

    #[test]
    fn copy_and_seed_are_disjoint() {
        let config = parse("copy=['cache']\nseed=['cache']", "repo/.wt.toml").unwrap_err();
        assert_eq!(config.code.0, "CONFIG_INVALID");
        assert!(config.message.contains("disjoint"));
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
    fn vars_failures_name_every_involved_key_with_source_locations() {
        let cycle = parse(
            "[vars]\nfirst='${second}'\nsecond='${first}'",
            "repo/.wt.toml",
        )
        .unwrap();
        let error = validate_resolved(&cycle, 16).unwrap_err();
        assert_eq!(error.code.0, "VARS_CYCLE");
        assert!(error.message.contains("`first` at repo/.wt.toml:2:"));
        assert!(error.message.contains("`second` at repo/.wt.toml:3:"));

        let unknown = parse("[vars]\nknown='${missing}'", "repo/.wt.toml").unwrap();
        let error = validate_resolved(&unknown, 16).unwrap_err();
        assert_eq!(error.code.0, "VARS_UNKNOWN");
        assert!(error.message.contains("missing"));
        assert!(error.message.contains("repo/.wt.toml:2:"));
    }

    #[test]
    fn unknown_functions_and_ports_name_the_call_and_location() {
        for (source, call) in [
            ("[env]\nA='${mystery()}'", "mystery()"),
            ("ports=['http']\n[env]\nA=\"${ports.admin}\"", "ports.admin"),
        ] {
            let config = parse(source, "repo/.wt.toml").unwrap();
            let error = validate_resolved(&config, 16).unwrap_err();
            assert_eq!(error.code.0, "CONFIG_INVALID");
            assert!(error.message.contains(call));
            assert!(error.message.contains("repo/.wt.toml:"));
        }
    }
}
