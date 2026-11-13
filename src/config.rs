//! A repository's `.wt.toml`, or its user-layer replacement at
//! `~/.wt/repos/<label>/wt.toml`.
//!
//! Parsing goes through `toml::Table` by hand rather than serde so that every
//! unknown key in a file is reported at once, and keys from older versions of
//! wt name their replacement.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use toml::{Table, Value};

use crate::error::{Error, Result, io_at};
use crate::names;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Branch templates for `wt new` without `--branch`; the first whose
    /// variables are all set wins.
    pub branch: Vec<String>,
    /// Named ports, in declaration order.
    pub ports: Vec<String>,
    pub copy: Vec<CopyEntry>,
    /// Wrapper name → executable path relative to the tree root.
    pub bin: BTreeMap<String, String>,
    /// Variable → template.
    pub env: BTreeMap<String, String>,
    /// Path relative to the tree root → file.
    pub files: BTreeMap<String, FileEntry>,
    pub lifecycle: Lifecycle,
    pub pools: BTreeMap<String, PoolConfig>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CopyEntry {
    /// As written: `~/…`, absolute, or relative to the main checkout.
    pub from: String,
    /// Relative to the tree root.
    pub to: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub content: String,
    pub mode: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lifecycle {
    pub create: Option<String>,
    pub start: Option<String>,
    pub stop: Option<String>,
    pub delete: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoolConfig {
    pub size: Option<u32>,
    pub name: Option<String>,
    pub wait: Option<Duration>,
    pub acquire: Option<String>,
    pub idle: Option<Duration>,
    pub retire: Option<String>,
}

impl Config {
    /// The config in effect for a tree of `label` rooted at `root`: the user
    /// layer if it exists, otherwise the tree's own `.wt.toml`, otherwise
    /// nothing.
    pub fn load(home: &Path, label: &str, root: &Path) -> Result<Config> {
        match Self::source(home, label, root) {
            Some(path) => Self::from_file(&path),
            None => Ok(Config::default()),
        }
    }

    /// Which file `load` reads, if any.
    pub fn source(home: &Path, label: &str, root: &Path) -> Option<PathBuf> {
        [
            home.join("repos").join(label).join("wt.toml"),
            root.join(".wt.toml"),
        ]
        .into_iter()
        .find(|path| path.is_file())
    }

    pub fn from_file(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).map_err(io_at("reading", path))?;
        Self::parse(&text).map_err(|error| Error::Config(format!("{}: {error}", path.display())))
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let table: Table = text.parse().map_err(|error| format!("{error}"))?;
        let mut errors = Errors::default();
        errors.keys(
            &table,
            "",
            &[
                "branch",
                "ports",
                "copy",
                "bin",
                "env",
                "files",
                "lifecycle",
                "pool",
            ],
            REMOVED_REPO_KEYS,
        );
        let mut config = Config::default();
        if let Some(value) = table.get("branch") {
            config.branch = errors.strings(value, "branch");
        }
        if let Some(value) = table.get("ports") {
            config.ports = errors.strings(value, "ports");
            for port in &config.ports {
                errors.name(port, "a port");
            }
        }
        if let Some(value) = table.get("copy") {
            config.copy = errors.copy(value);
        }
        if let Some(value) = table.get("bin") {
            match value {
                Value::Array(_) => errors.push(
                    "`bin` is now a table naming each executable: [bin] amux = \"target/debug/amux\"",
                ),
                _ => {
                    for (name, path) in errors.string_map(value, "bin") {
                        errors.name(&name, "a [bin] entry");
                        errors.relative(&path, &format!("bin.{name}"));
                        config.bin.insert(name, path);
                    }
                }
            }
        }
        if let Some(value) = table.get("env") {
            for (key, template) in errors.string_map(value, "env") {
                if !valid_env_name(&key) {
                    errors.push(format!("env.{key}: not a valid environment variable name"));
                }
                config.env.insert(key, template);
            }
        }
        if let Some(value) = table.get("files") {
            config.files = errors.files(value);
        }
        if let Some(value) = table.get("lifecycle") {
            config.lifecycle = errors.lifecycle(value);
        }
        if let Some(value) = table.get("pool") {
            config.pools = errors.pools(value, "pool", false);
        }
        errors.finish(config)
    }
}

/// Keys from wt 0.5 and what replaces them.
const REMOVED_REPO_KEYS: &[(&str, &str)] = &[
    (
        "hooks",
        "use [lifecycle]: hooks.after_new is now lifecycle.create; hooks.warm has no replacement",
    ),
    (
        "resource",
        "a per-tree resource becomes [lifecycle] start/stop/delete hooks; a counted resource becomes [pool.<name>]",
    ),
    (
        "commands",
        "name each executable in [bin] instead: [bin] amux = \"target/debug/amux\"",
    ),
    ("output", "list the directory in `copy` instead"),
    (
        "vars",
        "put the value in [env] and refer to it as {{env.NAME}} in files",
    ),
    ("sync_inputs", "run the synchronisation in lifecycle.create"),
    ("adapters", "removed: wt has no language-specific behaviour"),
    ("detect", "removed: wt has no language-specific behaviour"),
    ("dirs", "removed"),
];

pub(crate) fn valid_env_name(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|ch: char| ch.is_ascii_digit())
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Parses `500ms`, `30s`, `10m` or `2h`.
pub(crate) fn duration(text: &str) -> Option<Duration> {
    let split = text.find(|ch: char| !ch.is_ascii_digit())?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    match unit {
        "ms" => Some(Duration::from_millis(number)),
        "s" => Some(Duration::from_secs(number)),
        "m" => Some(Duration::from_secs(number * 60)),
        "h" => Some(Duration::from_secs(number * 3600)),
        _ => None,
    }
}

/// Collects every problem in a file so they can be reported together.
#[derive(Default)]
pub(crate) struct Errors(Vec<String>);

impl Errors {
    pub(crate) fn push(&mut self, message: impl Into<String>) {
        self.0.push(message.into());
    }

    pub(crate) fn finish<T>(self, value: T) -> Result<T, String> {
        if self.0.is_empty() {
            Ok(value)
        } else {
            Err(self.0.join("\n  "))
        }
    }

    /// Reports keys of `table` outside `allowed`, naming the replacement of
    /// any in `removed`.
    pub(crate) fn keys(
        &mut self,
        table: &Table,
        prefix: &str,
        allowed: &[&str],
        removed: &[(&str, &str)],
    ) {
        for key in table.keys() {
            if allowed.contains(&key.as_str()) {
                continue;
            }
            let path = format!("{prefix}{key}");
            match removed.iter().find(|(old, _)| old == key) {
                Some((_, instead)) => self.push(format!("`{path}` was removed: {instead}")),
                None => self.push(format!(
                    "unknown key `{path}`; expected one of: {}",
                    allowed.join(", ")
                )),
            }
        }
    }

    pub(crate) fn table<'a>(&mut self, value: &'a Value, path: &str) -> Option<&'a Table> {
        let table = value.as_table();
        if table.is_none() {
            self.push(format!("`{path}` must be a table"));
        }
        table
    }

    pub(crate) fn string(&mut self, value: &Value, path: &str) -> Option<String> {
        let string = value.as_str().map(str::to_owned);
        if string.is_none() {
            self.push(format!("`{path}` must be a string"));
        }
        string
    }

    fn strings(&mut self, value: &Value, path: &str) -> Vec<String> {
        let Some(array) = value.as_array() else {
            self.push(format!("`{path}` must be an array of strings"));
            return Vec::new();
        };
        array
            .iter()
            .filter_map(|item| self.string(item, path))
            .collect()
    }

    fn string_map(&mut self, value: &Value, path: &str) -> Vec<(String, String)> {
        let Some(table) = self.table(value, path) else {
            return Vec::new();
        };
        table
            .iter()
            .filter_map(|(key, item)| {
                self.string(item, &format!("{path}.{key}"))
                    .map(|string| (key.clone(), string))
            })
            .collect()
    }

    pub(crate) fn duration(&mut self, value: &Value, path: &str) -> Option<Duration> {
        let text = self.string(value, path)?;
        let parsed = duration(&text);
        if parsed.is_none() {
            self.push(format!(
                "`{path}`: `{text}` is not a duration like 500ms, 30s, 10m or 2h"
            ));
        }
        parsed
    }

    pub(crate) fn size(&mut self, value: &Value, path: &str) -> Option<u32> {
        match value.as_integer() {
            Some(size) if size > 0 && size <= i64::from(u32::MAX) => Some(size as u32),
            _ => {
                self.push(format!("`{path}` must be a positive integer"));
                None
            }
        }
    }

    pub(crate) fn name(&mut self, value: &str, what: &str) {
        if !names::valid(value) {
            self.push(format!(
                "`{value}` is not a valid name for {what}: use letters, digits, `-`, `_` and `.`"
            ));
        }
    }

    fn relative(&mut self, value: &str, path: &str) {
        let candidate = Path::new(value);
        if value.is_empty()
            || candidate.is_absolute()
            || candidate
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            self.push(format!(
                "`{path}`: `{value}` must be a plain path inside the tree"
            ));
        }
    }

    fn copy(&mut self, value: &Value) -> Vec<CopyEntry> {
        let Some(array) = value.as_array() else {
            self.push("`copy` must be an array");
            return Vec::new();
        };
        let mut entries = Vec::new();
        for item in array {
            match item {
                Value::String(path) => {
                    self.relative(path, "copy");
                    entries.push(CopyEntry {
                        from: path.clone(),
                        to: path.clone(),
                    });
                }
                Value::Table(table) => {
                    self.keys(table, "copy.", &["from", "to"], &[]);
                    let from = table.get("from").and_then(|v| self.string(v, "copy.from"));
                    let to = table.get("to").and_then(|v| self.string(v, "copy.to"));
                    match (from, to) {
                        (Some(from), Some(to)) => {
                            self.relative(&to, "copy.to");
                            entries.push(CopyEntry { from, to });
                        }
                        _ => self.push("each `copy` table needs both `from` and `to`"),
                    }
                }
                _ => self.push("`copy` entries are paths or { from, to } tables"),
            }
        }
        entries
    }

    fn files(&mut self, value: &Value) -> BTreeMap<String, FileEntry> {
        let mut files = BTreeMap::new();
        let Some(table) = self.table(value, "files") else {
            return files;
        };
        for (path, entry) in table {
            let at = format!("files.\"{path}\"");
            self.relative(path, &at);
            let Some(entry) = self.table(entry, &at) else {
                continue;
            };
            self.keys(
                entry,
                &format!("{at}."),
                &["content", "mode"],
                &[
                    ("marker", "removed: delete it"),
                    (
                        "template",
                        "removed: files are always rendered; copy a file verbatim with copy = [{ from, to }]",
                    ),
                ],
            );
            let content = entry
                .get("content")
                .and_then(|v| self.string(v, &format!("{at}.content")));
            let mode = entry.get("mode").and_then(|v| {
                let text = self.string(v, &format!("{at}.mode"))?;
                let mode = u32::from_str_radix(&text, 8).ok().filter(|m| *m <= 0o7777);
                if mode.is_none() {
                    self.push(format!(
                        "`{at}.mode`: `{text}` is not an octal mode like \"0755\""
                    ));
                }
                mode
            });
            match content {
                Some(content) => {
                    files.insert(path.clone(), FileEntry { content, mode });
                }
                None => self.push(format!("`{at}` needs `content`")),
            }
        }
        files
    }

    fn lifecycle(&mut self, value: &Value) -> Lifecycle {
        let mut lifecycle = Lifecycle::default();
        let Some(table) = self.table(value, "lifecycle") else {
            return lifecycle;
        };
        self.keys(
            table,
            "lifecycle.",
            &["create", "start", "stop", "delete"],
            &[("after_new", "renamed to lifecycle.create")],
        );
        let mut hook = |key: &str| {
            table
                .get(key)
                .and_then(|v| self.string(v, &format!("lifecycle.{key}")))
        };
        lifecycle.create = hook("create");
        lifecycle.start = hook("start");
        lifecycle.stop = hook("stop");
        lifecycle.delete = hook("delete");
        lifecycle
    }

    /// `[pool.<name>]` tables. In the user settings only sizing is allowed.
    pub(crate) fn pools(
        &mut self,
        value: &Value,
        prefix: &str,
        sizing_only: bool,
    ) -> BTreeMap<String, PoolConfig> {
        let mut pools = BTreeMap::new();
        let Some(table) = self.table(value, prefix) else {
            return pools;
        };
        let allowed: &[&str] = if sizing_only {
            &["size", "wait", "idle"]
        } else {
            &["size", "name", "wait", "acquire", "idle", "retire"]
        };
        for (name, pool) in table {
            let at = format!("{prefix}.{name}");
            self.name(name, "a pool");
            let Some(pool) = self.table(pool, &at) else {
                continue;
            };
            self.keys(
                pool,
                &format!("{at}."),
                allowed,
                &[
                    ("count", "renamed to `size`"),
                    ("keep", "removed: a tree now prefers the slot it used last"),
                    (
                        "exists",
                        "removed: `acquire` creates the instance when it is missing",
                    ),
                    (
                        "create",
                        "removed: `acquire` creates the instance when it is missing",
                    ),
                    ("destroy", "renamed to `retire`, which runs after `idle`"),
                    (
                        "tied_to",
                        "removed: pools are shared by name across the machine",
                    ),
                ],
            );
            let get = |key: &str| pool.get(key);
            let config = PoolConfig {
                size: get("size").and_then(|v| self.size(v, &format!("{at}.size"))),
                name: get("name").and_then(|v| self.string(v, &format!("{at}.name"))),
                wait: get("wait").and_then(|v| self.duration(v, &format!("{at}.wait"))),
                acquire: get("acquire").and_then(|v| self.string(v, &format!("{at}.acquire"))),
                idle: get("idle").and_then(|v| self.duration(v, &format!("{at}.idle"))),
                retire: get("retire").and_then(|v| self.string(v, &format!("{at}.retire"))),
            };
            if config.idle.is_some() != config.retire.is_some() && !sizing_only {
                self.push(format!("`{at}`: `idle` and `retire` go together"));
            }
            pools.insert(name.clone(), config);
        }
        pools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_config() {
        let config = Config::parse(
            r#"
            branch = ["{{ticket}}_{{name}}", "{{name}}"]
            ports = ["server", "frontend"]
            copy = [".claude/settings.local.json", { from = "~/x/justfile", to = ".wt/justfile" }]

            [bin]
            amux = "target/debug/amux"

            [env]
            URL = "http://localhost:{{ports.server}}"

            [files.".mcp.json"]
            content = "{}"
            mode = "0644"

            [lifecycle]
            create = "just build"
            delete = "just clean"

            [pool.iphone]
            size = 2
            name = "amux-iphone-{{n}}"
            wait = "45m"
            acquire = "acquire"
            idle = "30m"
            retire = "retire"
            "#,
        )
        .unwrap();
        assert_eq!(config.ports, ["server", "frontend"]);
        assert_eq!(config.copy[1].to, ".wt/justfile");
        assert_eq!(config.bin["amux"], "target/debug/amux");
        assert_eq!(config.files[".mcp.json"].mode, Some(0o644));
        assert_eq!(config.lifecycle.create.as_deref(), Some("just build"));
        assert_eq!(
            config.pools["iphone"].wait,
            Some(Duration::from_secs(45 * 60))
        );
    }

    #[test]
    fn reports_every_problem_at_once_and_names_replacements() {
        let error = Config::parse(
            r#"
            bin = ["target/debug"]
            commands = ["amux"]
            unknown = 1
            [hooks]
            after_new = "x"
            [pool.p]
            count = 2
            "#,
        )
        .unwrap_err();
        assert!(error.contains("`bin` is now a table"), "{error}");
        assert!(error.contains("`commands` was removed"), "{error}");
        assert!(error.contains("unknown key `unknown`"), "{error}");
        assert!(error.contains("lifecycle.create"), "{error}");
        assert!(
            error.contains("`pool.p.count` was removed: renamed to `size`"),
            "{error}"
        );
    }

    #[test]
    fn rejects_paths_outside_the_tree() {
        let error = Config::parse("copy = [\"../x\"]\n[bin]\nx = \"/usr/bin/x\"").unwrap_err();
        assert!(error.contains("copy"), "{error}");
        assert!(error.contains("bin.x"), "{error}");
    }

    #[test]
    fn durations() {
        assert_eq!(duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(duration("2"), None);
        assert_eq!(duration("m"), None);
    }
}
