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
}

impl Default for LogSettings {
    fn default() -> Self {
        Self { keep: 20 }
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
            agents: BTreeMap::from([
                (
                    "claude".to_owned(),
                    Agent {
                        start: Command::Argv(vec!["claude".to_owned()]),
                        resume: Command::Argv(vec!["claude".to_owned(), "--continue".to_owned()]),
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
        CoreError::new(
            ExitClass::State,
            "SETTINGS_INVALID",
            error.to_string(),
            "fix `$WT_HOME/config.toml`",
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
    Ok(())
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
