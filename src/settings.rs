//! The user's machine-level settings in `~/.wt/config.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml::Table;

use crate::config::{Errors, PoolConfig};
use crate::error::{Error, Result, io_at};

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub trees_dir: PathBuf,
    pub ports: PortGeometry,
    /// Machine sizing for pools: only `size`, `wait` and `idle` are set.
    pub pools: BTreeMap<String, PoolConfig>,
}

/// Port `p` of the tree in slot `s` is `base + s * stride + p`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PortGeometry {
    pub base: u16,
    pub stride: u16,
}

impl Default for PortGeometry {
    fn default() -> Self {
        PortGeometry {
            base: 20_000,
            stride: 16,
        }
    }
}

impl Settings {
    pub fn load(home: &Path) -> Result<Settings> {
        let path = home.join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(io_at("reading", &path)(error)),
        };
        Self::parse(&text, home)
            .map_err(|error| Error::Config(format!("{}: {error}", path.display())))
    }

    pub fn parse(text: &str, home: &Path) -> Result<Settings, String> {
        let table: Table = text.parse().map_err(|error| format!("{error}"))?;
        let mut errors = Errors::default();
        errors.keys(
            &table,
            "",
            &["trees_dir", "ports", "pool"],
            &[
                (
                    "session",
                    "removed: launching sessions and agents moved out of wt",
                ),
                (
                    "agents",
                    "removed: launching sessions and agents moved out of wt",
                ),
                ("logs", "removed"),
                ("waits", "removed"),
                ("git", "removed"),
                (
                    "repos",
                    "a repository's user-layer config now lives in ~/.wt/repos/<label>/wt.toml",
                ),
                (
                    "resources",
                    "machine pool sizing is now [pool.<name>] size/wait/idle",
                ),
            ],
        );
        let mut settings = Settings {
            trees_dir: home.join("trees"),
            ports: PortGeometry::default(),
            pools: BTreeMap::new(),
        };
        if let Some(value) = table.get("trees_dir")
            && let Some(path) = errors.string(value, "trees_dir")
        {
            settings.trees_dir = expand_home(&path);
        }
        if let Some(value) = table.get("ports")
            && let Some(ports) = errors.table(value, "ports")
        {
            errors.keys(ports, "ports.", &["base", "stride"], &[]);
            let number = |key: &str, errors: &mut Errors| {
                ports.get(key).and_then(|value| {
                    let parsed = value.as_integer().and_then(|n| u16::try_from(n).ok());
                    if parsed.is_none() {
                        errors.push(format!("`ports.{key}` must be a number below 65536"));
                    }
                    parsed
                })
            };
            if let Some(base) = number("base", &mut errors) {
                settings.ports.base = base;
            }
            if let Some(stride) = number("stride", &mut errors) {
                settings.ports.stride = stride;
            }
            if settings.ports.base < 1024 || settings.ports.stride == 0 {
                errors.push("`ports` needs base >= 1024 and a non-zero stride");
            }
        }
        if let Some(value) = table.get("pool") {
            settings.pools = errors.pools(value, "pool", true);
        }
        errors.finish(settings)
    }
}

/// Expands a leading `~/` to `$HOME`.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None => PathBuf::from(path),
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let settings = Settings::parse("", Path::new("/h")).unwrap();
        assert_eq!(settings.trees_dir, Path::new("/h/trees"));
        assert_eq!(settings.ports, PortGeometry::default());
    }

    #[test]
    fn old_keys_name_their_replacement() {
        let error = Settings::parse(
            "[session]\nbackend = \"tmux\"\n[resources.container]\ncount = 4\n",
            Path::new("/h"),
        )
        .unwrap_err();
        assert!(error.contains("moved out of wt"), "{error}");
        assert!(error.contains("[pool.<name>]"), "{error}");
    }

    #[test]
    fn pool_sizing_only() {
        let error = Settings::parse(
            "[pool.container]\nsize = 4\nacquire = \"x\"\n",
            Path::new("/h"),
        )
        .unwrap_err();
        assert!(error.contains("pool.container.acquire"), "{error}");
        let settings = Settings::parse("[pool.container]\nsize = 4\n", Path::new("/h")).unwrap();
        assert_eq!(settings.pools["container"].size, Some(4));
    }
}
