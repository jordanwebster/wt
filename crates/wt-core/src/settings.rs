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
    pub status_bar: bool,
    pub tmux_timeout: String,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            status_bar: true,
            tmux_timeout: "10s".to_owned(),
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
    pub default_agent: Option<String>,
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
            default_agent: None,
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
    if let Some(default_agent) = &settings.default_agent {
        if !settings.agents.contains_key(default_agent) {
            return Err(settings_error("default_agent is not declared"));
        }
    }
    Ok(())
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
        assert!(parse("default_agent='missing'").is_err());
    }
}
