use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    config::{Command, Config},
    error::CoreError,
    model::{valid_duration, AbsPath, Geometry, Label},
    ExitClass,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub start: Command,
    pub resume: Command,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitTimeouts {
    pub query: Option<String>,
    pub fetch: Option<String>,
    pub clone: Option<String>,
    pub worktree: Option<String>,
    pub submodule: Option<String>,
}

impl Default for GitTimeouts {
    fn default() -> Self {
        Self {
            query: Some("30s".to_owned()),
            fetch: Some("120s".to_owned()),
            clone: Some("600s".to_owned()),
            worktree: Some("60s".to_owned()),
            submodule: Some("600s".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitSettings {
    pub timeouts: GitTimeouts,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskDefaults {
    pub probe_timeout: Option<String>,
    pub destroy_timeout: Option<String>,
    pub timeout: Option<String>,
    pub lock_wait: Option<String>,
}

impl Default for TaskDefaults {
    fn default() -> Self {
        Self {
            probe_timeout: Some("10s".to_owned()),
            destroy_timeout: Some("60s".to_owned()),
            timeout: None,
            lock_wait: Some("0s".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LockWaits {
    pub tree_exclusive: Option<String>,
    pub repo_git: Option<String>,
    pub resource: Option<String>,
    pub rmw: Option<String>,
}

impl Default for LockWaits {
    fn default() -> Self {
        Self {
            tree_exclusive: Some("30s".to_owned()),
            repo_git: Some("60s".to_owned()),
            resource: Some("120s".to_owned()),
            rmw: Some("5s".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogSettings {
    pub keep: u16,
    /// Whether wt appends its own timing log. Off by default: the log is a
    /// diagnostic for investigating slow commands, not something every home
    /// should accumulate; `[logs] trace = true` opts in.
    pub trace: bool,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            keep: 20,
            trace: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionSettings {
    pub backend: SessionBackend,
    pub attach: bool,
    pub agent: Option<String>,
    pub tmux_timeout: String,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            backend: SessionBackend::None,
            attach: true,
            agent: None,
            tmux_timeout: "10s".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionBackend {
    Tmux,
    None,
}

impl SessionBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShellSettings {
    pub program: Option<AbsPath>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub schema: u8,
    pub trees_dir: Option<String>,
    pub editor: Option<Command>,
    pub agents: BTreeMap<String, Agent>,
    pub ports: PortSettings,
    pub git: GitSettings,
    pub task: TaskDefaults,
    pub locks: LockWaits,
    pub session: SessionSettings,
    pub logs: LogSettings,
    pub shell: ShellSettings,
    pub repos: BTreeMap<Label, Config>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: 1,
            trees_dir: None,
            editor: None,
            agents: BTreeMap::from([
                (
                    "claude".to_owned(),
                    Agent {
                        start: Command::Argv(vec![
                            "claude".to_owned(),
                            "--name".to_owned(),
                            "{{name()}}".to_owned(),
                        ]),
                        resume: Command::Argv(vec![
                            "claude".to_owned(),
                            "--resume".to_owned(),
                            "{{name()}}".to_owned(),
                        ]),
                    },
                ),
                (
                    "codex".to_owned(),
                    Agent {
                        start: Command::Argv(vec!["codex".to_owned()]),
                        resume: Command::Argv(vec![
                            "codex".to_owned(),
                            "resume".to_owned(),
                            "--last".to_owned(),
                        ]),
                    },
                ),
            ]),
            ports: PortSettings::default(),
            git: GitSettings::default(),
            task: TaskDefaults::default(),
            locks: LockWaits::default(),
            session: SessionSettings::default(),
            logs: LogSettings::default(),
            shell: ShellSettings::default(),
            repos: BTreeMap::new(),
        }
    }
}

pub fn parse(source: &str) -> Result<Settings, CoreError> {
    let mut settings: Settings = toml::from_str(source).map_err(|error| {
        // A misplaced repo key parses as an unknown settings field, so serde
        // reports it against the settings schema and lists alternatives that
        // are all wrong for it. Say what is actually the matter instead of
        // leaving a message and a remedy that disagree.
        misplaced_repo_key(source).map_or_else(
            || {
                CoreError::new(
                    ExitClass::State,
                    "SETTINGS_INVALID",
                    error.to_string(),
                    "fix `$WT_HOME/config.toml`",
                )
            },
            |key| {
                CoreError::new(
                    ExitClass::State,
                    "SETTINGS_INVALID",
                    format!("`{key}` is a repo-scope key, not a settings key"),
                    format!("put `{key}` under `[repos.<label>]` in `$WT_HOME/config.toml`"),
                )
            },
        )
    })?;
    let defaults = Settings::default();
    for (name, agent) in defaults.agents {
        settings.agents.entry(name).or_insert(agent);
    }
    validate_settings(&settings)?;
    Ok(settings)
}

pub fn validate_settings(settings: &Settings) -> Result<(), CoreError> {
    if settings.schema != 1 {
        return Err(settings_error("settings schema must be 1"));
    }
    validate(settings.ports)?;
    let durations = [settings.session.tmux_timeout.as_str()].into_iter().chain(
        [
            settings.git.timeouts.query.as_deref(),
            settings.git.timeouts.fetch.as_deref(),
            settings.git.timeouts.clone.as_deref(),
            settings.git.timeouts.worktree.as_deref(),
            settings.git.timeouts.submodule.as_deref(),
            settings.task.probe_timeout.as_deref(),
            settings.task.destroy_timeout.as_deref(),
            settings.task.timeout.as_deref(),
            settings.task.lock_wait.as_deref(),
            settings.locks.tree_exclusive.as_deref(),
            settings.locks.repo_git.as_deref(),
            settings.locks.resource.as_deref(),
            settings.locks.rmw.as_deref(),
        ]
        .into_iter()
        .flatten(),
    );
    if durations
        .into_iter()
        .any(|duration| !valid_duration(duration))
    {
        return Err(settings_error("settings contain an invalid duration"));
    }
    if let Some(agent) = &settings.session.agent {
        if !settings.agents.contains_key(agent) {
            return Err(settings_error("session.agent is not declared"));
        }
    }
    for (label, repo) in &settings.repos {
        let Some(branch) = &repo.branch else {
            continue;
        };
        if branch.candidates().is_empty() {
            return Err(settings_error(&format!(
                "[repos.{label}] branch must declare at least one template"
            )));
        }
        for candidate in branch.candidates() {
            if let Err(error) = crate::template::validate_branch(candidate) {
                return Err(settings_error(&format!(
                    "[repos.{label}] branch template is invalid: {}",
                    error.message
                )));
            }
        }
    }
    if let Some(editor) = &settings.editor {
        if editor.texts().is_empty() || editor.texts().iter().any(String::is_empty) {
            return Err(settings_error("editor command must not be empty"));
        }
        for value in editor.texts() {
            if let Err(error) = crate::template::validate(value) {
                return Err(settings_error(&format!(
                    "editor command template is invalid: {}",
                    error.message
                )));
            }
        }
    }
    Ok(())
}

fn misplaced_repo_key(source: &str) -> Option<String> {
    const REPO_KEYS: &[&str] = &[
        "adapters",
        "bin",
        "branch",
        "commands",
        "copy",
        "detect",
        "dirs",
        "env",
        "files",
        "sync_inputs",
        "vars",
    ];
    let table = source.parse::<toml::Table>().ok()?;
    REPO_KEYS
        .iter()
        .find(|key| table.contains_key(**key))
        .map(|key| (*key).to_owned())
        .or_else(|| {
            table
                .get("ports")
                .is_some_and(toml::Value::is_array)
                .then(|| "ports".to_owned())
        })
        .or_else(|| {
            repo_map_key(
                &table,
                "locks",
                &["tree_exclusive", "repo_git", "resource", "rmw"],
            )
        })
        .or_else(|| {
            repo_map_key(
                &table,
                "task",
                &["probe_timeout", "destroy_timeout", "timeout", "lock_wait"],
            )
        })
}

/// Names the misplaced entry itself — `locks.integration` rather than `locks` —
/// because the settings table of the same name is legitimate and only the one
/// entry belongs elsewhere.
fn repo_map_key(table: &toml::Table, key: &str, settings_keys: &[&str]) -> Option<String> {
    let values = table.get(key)?.as_table()?;
    values
        .iter()
        .find(|(name, value)| {
            !settings_keys.contains(&name.as_str())
                && (value.is_table() || matches!(value, toml::Value::Boolean(false)))
        })
        .map(|(name, _)| format!("{key}.{name}"))
}

pub fn backend_is_declared(source: &str) -> Result<bool, CoreError> {
    let value = source.parse::<toml::Table>().map_err(|error| {
        CoreError::new(
            ExitClass::State,
            "SETTINGS_INVALID",
            error.to_string(),
            "fix `$WT_HOME/config.toml`",
        )
    })?;
    Ok(value
        .get("session")
        .and_then(toml::Value::as_table)
        .is_some_and(|session| session.contains_key("backend")))
}

pub fn declare_backend(source: &str, backend: SessionBackend) -> Result<String, CoreError> {
    parse(source)?;
    if backend_is_declared(source)? {
        return Ok(source.to_owned());
    }
    let table = source.parse::<toml::Table>().map_err(|error| {
        CoreError::new(
            ExitClass::State,
            "SETTINGS_INVALID",
            error.to_string(),
            "fix `$WT_HOME/config.toml`",
        )
    })?;
    if table.contains_key("session") && session_header_end(source).is_none() {
        return Err(CoreError::new(
            ExitClass::State,
            "SETTINGS_INVALID",
            "cannot add session.backend to a non-table session declaration",
            "rewrite `session = { ... }` as a `[session]` table, then retry",
        ));
    }
    let mut output = source.to_owned();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    let declaration = format!("backend = \"{}\"\n", backend.as_str());
    if let Some(offset) = session_header_end(&output) {
        output.insert_str(offset, &declaration);
    } else {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("[session]\n");
        output.push_str(&declaration);
    }
    parse(&output)?;
    Ok(output)
}

fn session_header_end(source: &str) -> Option<usize> {
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        offset += line.len();
        if line.trim() == "[session]" {
            return Some(offset);
        }
    }
    None
}

fn settings_error(message: &str) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "SETTINGS_INVALID",
        message,
        "fix `$WT_HOME/config.toml`",
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PortSettings {
    pub base: u16,
    pub stride: u8,
}

impl Default for PortSettings {
    fn default() -> Self {
        Self {
            base: 20_000,
            stride: 16,
        }
    }
}

impl PortSettings {
    pub fn max_slots(self) -> Result<u32, CoreError> {
        validate(self)?;
        Ok((65_536_u32 - u32::from(self.base)) / u32::from(self.stride))
    }

    pub fn geometry(self, slot: u32) -> Result<Geometry, CoreError> {
        let max_slots = self.max_slots()?;
        if slot >= max_slots {
            return Err(CoreError::new(
                ExitClass::Conflict,
                "SLOTS_EXHAUSTED",
                "port slot is outside the configured geometry",
                "increase the available port geometry",
            ));
        }
        let port_base = u32::from(self.base) + slot * u32::from(self.stride);
        Ok(Geometry {
            base: self.base,
            stride: self.stride,
            port_base: port_base as u16,
        })
    }
}

pub fn validate(settings: PortSettings) -> Result<(), CoreError> {
    let max_slots = if settings.stride == 0 {
        0
    } else {
        (65_536_u32 - u32::from(settings.base)) / u32::from(settings.stride)
    };
    if settings.base < 1024 || settings.stride == 0 || max_slots == 0 {
        return Err(CoreError::new(
            ExitClass::State,
            "SETTINGS_INVALID",
            "ports must have base >= 1024 and at least one complete non-zero stride",
            "set ports.base and ports.stride to a valid geometry",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_low_base_zero_stride_and_incomplete_geometry() {
        assert!(validate(PortSettings {
            base: 1023,
            stride: 1
        })
        .is_err());
        assert!(validate(PortSettings {
            base: 20000,
            stride: 0
        })
        .is_err());
        assert!(validate(PortSettings {
            base: 65535,
            stride: 2
        })
        .is_err());
        assert_eq!(PortSettings::default().max_slots().unwrap(), 2846);
    }

    #[test]
    fn settings_parser_applies_defaults_and_rejects_unknown_keys() {
        let settings = parse("[ports]\nbase=30000\nstride=8").unwrap();
        assert_eq!(
            settings.ports,
            PortSettings {
                base: 30000,
                stride: 8
            }
        );
        assert!(settings.agents.contains_key("claude"));
        assert_eq!(
            settings.agents["claude"].start,
            Command::Argv(vec![
                "claude".to_owned(),
                "--name".to_owned(),
                "{{name()}}".to_owned(),
            ])
        );
        assert_eq!(
            settings.agents["claude"].resume,
            Command::Argv(vec![
                "claude".to_owned(),
                "--resume".to_owned(),
                "{{name()}}".to_owned(),
            ])
        );
        assert_eq!(settings.logs.keep, 20);
        assert_eq!(settings.git.timeouts.query.as_deref(), Some("30s"));
        assert_eq!(settings.task.probe_timeout.as_deref(), Some("10s"));
        assert_eq!(settings.locks.rmw.as_deref(), Some("5s"));
        assert!(parse("mystery=true").is_err());
        let removed = parse("default_agent='codex'").unwrap_err();
        assert_eq!(removed.code.0, "SETTINGS_INVALID");
        assert!(removed.message.contains("unknown field `default_agent`"));
        assert!(!removed.message.contains("session.agent"));
        assert!(!removed.remedy.contains("session.agent"));
        assert_eq!(settings.session.backend, SessionBackend::None);
        assert!(settings.session.attach);
        assert_eq!(settings.session.agent, None);
        assert_eq!(settings.editor, None);
    }

    #[test]
    fn editor_accepts_both_command_forms_and_repo_keys_get_a_scoped_remedy() {
        assert_eq!(
            parse("editor=['code', '{{root()}}']").unwrap().editor,
            Some(Command::Argv(vec![
                "code".to_owned(),
                "{{root()}}".to_owned()
            ]))
        );
        assert_eq!(
            parse("editor='vim'").unwrap().editor,
            Some(Command::Shell("vim".to_owned()))
        );
        for source in ["editor=[]", "editor='{{root()}'"] {
            let error = parse(source).unwrap_err();
            assert_eq!(error.code.0, "SETTINGS_INVALID");
        }
        let error = parse("env={ FOO='bar' }").unwrap_err();
        assert_eq!(
            error.message,
            "`env` is a repo-scope key, not a settings key"
        );
        assert!(error.remedy.contains("[repos.<label>]"));
        assert!(error.remedy.contains("$WT_HOME/config.toml"));
        for (source, key) in [
            ("ports=['http']", "ports"),
            ("[locks.integration]\nslots=2", "locks.integration"),
            ("[task.test]\nrun='cargo test'", "task.test"),
        ] {
            let error = parse(source).unwrap_err();
            assert_eq!(
                error.message,
                format!("`{key}` is a repo-scope key, not a settings key"),
                "{source}"
            );
        }
        // A settings-shaped mistake keeps serde's own account of it.
        let error = parse("[locks]\ntree_exclusive=5").unwrap_err();
        assert!(
            error.message.contains("tree_exclusive"),
            "{}",
            error.message
        );
        assert_eq!(error.remedy, "fix `$WT_HOME/config.toml`");
    }

    #[test]
    fn backend_declaration_preserves_existing_settings() {
        let source = "trees_dir='/tmp/trees'\n[session]\nattach=false\n";
        let updated = declare_backend(source, SessionBackend::Tmux).unwrap();
        assert_eq!(
            updated,
            "trees_dir='/tmp/trees'\n[session]\nbackend = \"tmux\"\nattach=false\n"
        );
        let settings = parse(&updated).unwrap();
        assert_eq!(settings.session.backend, SessionBackend::Tmux);
        assert!(!settings.session.attach);
    }

    #[test]
    fn backend_declaration_explains_how_to_rewrite_an_inline_session_table() {
        let source = "session = { attach = false }\n";
        let error = declare_backend(source, SessionBackend::Tmux).unwrap_err();
        assert_eq!(error.code.0, "SETTINGS_INVALID");
        assert!(error.remedy.contains("[session]"));
        assert!(error.remedy.contains("retry"));
    }
}

/// A value as a TOML string literal, so a path with a quote or a backslash
/// in it is written rather than corrupting the file.
pub fn string_literal(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

/// Whether a settings key is already written.
pub fn declared(source: &str, table: Option<&str>, key: &str) -> Result<bool, CoreError> {
    let value = source.parse::<toml::Table>().map_err(settings_invalid)?;
    Ok(match table {
        Some(table) => value
            .get(table)
            .and_then(toml::Value::as_table)
            .is_some_and(|table| table.contains_key(key)),
        None => value.contains_key(key),
    })
}

/// Writes a settings key that is absent, leaving everything already there —
/// including comments and ordering — exactly as it was.
///
/// `value` is TOML, so a string arrives already quoted.
pub fn declare(
    source: &str,
    table: Option<&str>,
    key: &str,
    value: &str,
) -> Result<String, CoreError> {
    parse(source)?;
    if declared(source, table, key)? {
        return Ok(source.to_owned());
    }
    let parsed = source.parse::<toml::Table>().map_err(settings_invalid)?;
    if let Some(name) = table {
        if parsed.contains_key(name) && header_end(source, name).is_none() {
            return Err(CoreError::new(
                ExitClass::State,
                "SETTINGS_INVALID",
                format!("cannot add {name}.{key} to a non-table {name} declaration"),
                format!("rewrite `{name} = {{ ... }}` as a `[{name}]` table, then retry"),
            ));
        }
    }
    let mut output = source.to_owned();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    let line = format!("{key} = {value}\n");
    match table {
        Some(name) => match header_end(&output, name) {
            Some(offset) => output.insert_str(offset, &line),
            None => {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&format!("[{name}]\n"));
                output.push_str(&line);
            }
        },
        // A top-level key appended to the end of the file would land inside
        // whatever table header came last, so it goes above the first one.
        None => {
            let offset = first_header_start(&output).unwrap_or(output.len());
            output.insert_str(offset, &line);
        }
    }
    parse(&output)?;
    Ok(output)
}

/// Writes a settings key, replacing a value already there.
///
/// [`declare`] deliberately leaves an existing key alone, which is right when
/// wt is filling in a default it invented. It is wrong when a person chose the
/// value: silently keeping the old one and reporting success is the failure
/// mode this exists to avoid.
pub fn set(source: &str, table: Option<&str>, key: &str, value: &str) -> Result<String, CoreError> {
    parse(source)?;
    if !declared(source, table, key)? {
        return declare(source, table, key, value);
    }
    let updated = rewrite(source, table, key, |line| {
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        let comment = line
            .split_once('#')
            .map(|(_, comment)| format!("  #{comment}"))
            .unwrap_or_default();
        let comment = comment.trim_end_matches(['\n', '\r']).to_owned();
        Some(format!("{indent}{key} = {value}{comment}\n"))
    });
    parse(&updated)?;
    Ok(updated)
}

/// Removes a settings key, leaving the file untouched when it is absent.
pub fn unset(source: &str, table: Option<&str>, key: &str) -> Result<String, CoreError> {
    parse(source)?;
    if !declared(source, table, key)? {
        return Ok(source.to_owned());
    }
    let updated = rewrite(source, table, key, |_| None);
    parse(&updated)?;
    Ok(updated)
}

/// Replaces or deletes the line declaring `key` inside `table`.
///
/// Line-based rather than a parse-and-serialise round trip, which would
/// discard every comment and reorder the file.
fn rewrite(
    source: &str,
    table: Option<&str>,
    key: &str,
    replacement: impl Fn(&str) -> Option<String>,
) -> String {
    let mut output = String::with_capacity(source.len());
    let mut current: Option<String> = None;
    let mut done = false;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(name) = header_name(trimmed) {
            current = Some(name.to_owned());
            output.push_str(line);
            continue;
        }
        let in_table = match table {
            Some(name) => current.as_deref() == Some(name),
            None => current.is_none(),
        };
        let declares = trimmed
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == key)
            && !trimmed.starts_with('#');
        if !done && in_table && declares {
            done = true;
            if let Some(text) = replacement(line) {
                output.push_str(&text);
            }
            continue;
        }
        output.push_str(line);
    }
    output
}

/// The table a `[name]` line opens, allowing a trailing comment; `None` for
/// any other line.
fn header_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let (name, after) = rest.split_once(']')?;
    let after = after.trim();
    (after.is_empty() || after.starts_with('#')).then(|| name.trim())
}

fn header_end(source: &str, name: &str) -> Option<usize> {
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        offset += line.len();
        if header_name(line.trim()) == Some(name) {
            return Some(offset);
        }
    }
    None
}

fn first_header_start(source: &str) -> Option<usize> {
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn settings_invalid(error: toml::de::Error) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "SETTINGS_INVALID",
        error.to_string(),
        "fix `$WT_HOME/config.toml`",
    )
}

#[cfg(test)]
mod declare_tests {
    use super::*;

    #[test]
    fn a_table_key_is_inserted_under_its_header() {
        let source = "[session]\nbackend = \"tmux\"\n";
        let updated = declare(source, Some("session"), "agent", "\"claude\"").unwrap();
        assert_eq!(
            updated,
            "[session]\nagent = \"claude\"\nbackend = \"tmux\"\n"
        );
        assert_eq!(
            parse(&updated).unwrap().session.agent.as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn a_missing_table_is_created() {
        let updated = declare("", Some("session"), "agent", "\"codex\"").unwrap();
        assert_eq!(updated, "[session]\nagent = \"codex\"\n");
    }

    #[test]
    fn a_top_level_key_lands_above_the_first_table() {
        let source = "[session]\nbackend = \"tmux\"\n";
        let updated = declare(source, None, "trees_dir", "\"/w\"").unwrap();
        assert_eq!(
            updated, "trees_dir = \"/w\"\n[session]\nbackend = \"tmux\"\n",
            "a top-level key appended at the end would belong to [session]"
        );
        assert_eq!(parse(&updated).unwrap().trees_dir.as_deref(), Some("/w"));
    }

    #[test]
    fn a_top_level_key_in_a_table_free_file_is_appended() {
        let updated = declare("# a note\n", None, "trees_dir", "\"/w\"").unwrap();
        assert_eq!(updated, "# a note\ntrees_dir = \"/w\"\n");
    }

    #[test]
    fn an_existing_key_is_left_exactly_as_written() {
        let source = "# keep me\n[session]\nagent = \"codex\"  # and me\n";
        assert_eq!(
            declare(source, Some("session"), "agent", "\"claude\"").unwrap(),
            source
        );
    }

    #[test]
    fn comments_and_ordering_survive() {
        let source = "# top\n\n[ports]\nbase = 20000\n\n[session]\n# why\nbackend = \"tmux\"\n";
        let updated = declare(source, Some("session"), "agent", "\"claude\"").unwrap();
        assert!(updated.contains("# top"));
        assert!(updated.contains("# why"));
        assert!(updated.contains("base = 20000"));
    }

    #[test]
    fn setting_replaces_a_value_a_person_chose() {
        let source = "# top\ntrees_dir = \"/old\"\n\n[session]\nagent = \"claude\"  # why\n";
        let updated = set(source, None, "trees_dir", "\"/new\"").unwrap();
        assert!(updated.contains("trees_dir = \"/new\""));
        assert!(!updated.contains("/old"));
        assert!(updated.contains("# top"), "comments survive");
        assert_eq!(parse(&updated).unwrap().trees_dir.as_deref(), Some("/new"));

        let agent = set(&updated, Some("session"), "agent", "\"codex\"").unwrap();
        assert!(agent.contains("agent = \"codex\"  # why"), "{agent:?}");
        assert_eq!(
            parse(&agent).unwrap().session.agent.as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn setting_an_absent_key_declares_it() {
        let updated = set("", Some("session"), "agent", "\"codex\"").unwrap();
        assert_eq!(updated, "[session]\nagent = \"codex\"\n");
    }

    #[test]
    fn unsetting_removes_only_the_named_key() {
        let source = "trees_dir = \"/w\"\n[session]\nagent = \"claude\"\nattach = false\n";
        let updated = unset(source, Some("session"), "agent").unwrap();
        assert_eq!(updated, "trees_dir = \"/w\"\n[session]\nattach = false\n");
        let parsed = parse(&updated).unwrap();
        assert_eq!(parsed.session.agent, None);
        assert_eq!(parsed.trees_dir.as_deref(), Some("/w"));
    }

    #[test]
    fn unsetting_an_absent_key_changes_nothing() {
        let source = "[session]\nattach = false\n";
        assert_eq!(unset(source, Some("session"), "agent").unwrap(), source);
    }

    #[test]
    fn a_rewrite_stays_inside_the_table_it_names() {
        let source = "[ports]\nbase = 20000\n\n[session]\nattach = false\n";
        let updated = set(source, Some("ports"), "base", "21000").unwrap();
        assert!(updated.contains("base = 21000"));
        assert!(
            updated.contains("[session]\nattach = false"),
            "another table is untouched: {updated:?}"
        );
        assert_eq!(parse(&updated).unwrap().ports.base, 21000);
    }

    #[test]
    fn a_string_literal_round_trips_quotes_and_backslashes() {
        for value in ["/w/\"odd\"\\dir", "it's", "plain", "a\nb"] {
            let literal = string_literal(value);
            let updated = set("", None, "trees_dir", &literal).unwrap();
            assert_eq!(
                parse(&updated).unwrap().trees_dir.as_deref(),
                Some(value),
                "{literal}"
            );
        }
    }

    #[test]
    fn a_header_with_a_trailing_comment_still_names_its_table() {
        let source = "[session] # sessions\nagent = \"claude\"\n";
        let updated = set(source, Some("session"), "agent", "\"codex\"").unwrap();
        assert_eq!(
            parse(&updated).unwrap().session.agent.as_deref(),
            Some("codex")
        );
        let removed = unset(source, Some("session"), "agent").unwrap();
        assert_eq!(parse(&removed).unwrap().session.agent, None);
        let added = declare(source, Some("session"), "attach", "false").unwrap();
        assert!(!parse(&added).unwrap().session.attach);
    }

    #[test]
    fn an_inline_table_is_refused_rather_than_rewritten() {
        let source = "session = { backend = \"tmux\" }\n";
        let error = declare(source, Some("session"), "agent", "\"claude\"").unwrap_err();
        assert_eq!(error.code.0, "SETTINGS_INVALID");
    }

    #[test]
    fn a_value_that_would_not_parse_is_refused_before_it_is_written() {
        assert!(declare("", Some("session"), "agent", "not-toml").is_err());
    }
}
