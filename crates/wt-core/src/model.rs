use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, ExitClass};

macro_rules! identifier {
    ($name:ident, $validate:ident, $what:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
                let value = value.into();
                if $validate(&value) {
                    Ok(Self(value))
                } else {
                    Err(CoreError::new(
                        ExitClass::State,
                        "CONFIG_INVALID",
                        format!("invalid {} `{value}`", $what),
                        format!("use a valid {}", $what),
                    ))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = CoreError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

fn common_id(value: &str, max: usize) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && value.len() <= max
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_label(value: &str) -> bool {
    common_id(value, 32)
}
fn valid_tree_name(value: &str) -> bool {
    common_id(value, 64) && value != "canonical"
}
fn valid_task_id(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && value.len() <= 64
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}
fn valid_port_name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
fn valid_env_key(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !value.starts_with("WT_")
}
fn valid_lock_name(value: &str) -> bool {
    valid_task_id(value)
}

identifier!(Label, valid_label, "label");
identifier!(TreeName, valid_tree_name, "tree name");
identifier!(TaskId, valid_task_id, "task id");
identifier!(PortName, valid_port_name, "port name");
identifier!(EnvKey, valid_env_key, "environment key");
identifier!(LockName, valid_lock_name, "lock name");

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct Target {
    pub label: Label,
    pub name: String,
}

impl Target {
    pub fn canonical(label: Label) -> Self {
        Self {
            label,
            name: "canonical".to_owned(),
        }
    }

    pub fn parse(value: &str) -> Result<Self, CoreError> {
        match value.split_once('/') {
            Some((label, name)) => {
                let label = Label::new(label)?;
                if name == "canonical" {
                    Ok(Self::canonical(label))
                } else {
                    Ok(Self {
                        label,
                        name: TreeName::new(name)?.to_string(),
                    })
                }
            }
            None => Ok(Self::canonical(Label::new(value)?)),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.name == "canonical" {
            write!(f, "{}", self.label)
        } else {
            write!(f, "{}/{}", self.label, self.name)
        }
    }
}

pub fn tree_state_path(target: &Target) -> String {
    format!("state/{}/{}.json", target.label, target.name)
}

pub fn repo_state_path(label: &Label) -> String {
    format!("state/{label}/_repo.json")
}

pub fn machine_state_path() -> &'static str {
    "state/_machine.json"
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelPath(String);

impl RelPath {
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        let valid = !value.is_empty()
            && !value.starts_with('/')
            && !value.contains('\0')
            && (value == "."
                || value
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."));
        if valid {
            Ok(Self(value))
        } else {
            Err(CoreError::new(
                ExitClass::State,
                "CONFIG_INVALID",
                format!("path `{value}` is not a contained relative path"),
                "use a non-empty relative path without `.` or `..` components",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RelPath {
    type Error = CoreError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RelPath> for String {
    fn from(value: RelPath) -> Self {
        value.0
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type RelDir = RelPath;
pub type TreeId = String;
pub type EnvMap = BTreeMap<String, String>;
pub type PortMap = BTreeMap<PortName, u8>;

pub fn scope_enc(scope: &RelDir) -> String {
    scope.as_str().replace('/', "%2F")
}

pub fn valid_tree_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AbsPath(String);

impl AbsPath {
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.starts_with('/') && !value.contains('\0') {
            Ok(Self(value))
        } else {
            Err(CoreError::new(
                ExitClass::State,
                "CONFIG_INVALID",
                format!("path `{value}` is not absolute"),
                "use an absolute path",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AbsPath {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AbsPath> for String {
    fn from(value: AbsPath) -> Self {
        value.0
    }
}

pub fn valid_duration(value: &str) -> bool {
    duration_millis(value).is_some()
}

/// Parses the settings duration grammar into milliseconds without effects.
pub fn duration_millis(value: &str) -> Option<u64> {
    let digits = value.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let number = value[..digits].parse::<u64>().ok()?;
    let multiplier = match &value[digits..] {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub base: u16,
    pub stride: u8,
    pub port_base: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TreeIdentity {
    pub tree_id: TreeId,
    pub label: Label,
    pub name: String,
    pub canonical: bool,
    pub root: String,
    pub repo: String,
    pub branch: Option<String>,
    pub slot: u32,
    pub geometry: Geometry,
    pub ports: PortMap,
    pub name_short: String,
    pub session_name: String,
}

impl TreeIdentity {
    pub fn target(&self) -> Target {
        Target {
            label: self.label.clone(),
            name: self.name.clone(),
        }
    }
}

pub fn name_snake(value: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            if separator && !out.is_empty() {
                out.push('_');
            }
            separator = false;
            out.push(ch);
        } else {
            separator = true;
        }
    }
    if out.is_empty() {
        "x".to_owned()
    } else {
        out
    }
}

pub fn name_short(label: &str, name: &str) -> String {
    let full = format!("{}_{}", name_snake(label), name_snake(name));
    // Hash the untruncated identity so equal display prefixes remain distinct.
    let hash = blake3::hash(full.as_bytes()).to_hex();
    format!(
        "{}_{}",
        full.chars().take(22).collect::<String>(),
        &hash[..8]
    )
}

/// Hashes the canonical common-gitdir path used as the repository identity.
pub fn gitdir_id(common_gitdir: &str) -> String {
    blake3::hash(common_gitdir.as_bytes()).to_hex().to_string()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub schema: u8,
    pub labels: BTreeMap<Label, LabelRec>,
    pub trees: Vec<TreeRec>,
    pub tombstones: Vec<Tombstone>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LabelRec {
    pub path: AbsPath,
    pub gitdir_id: String,
    pub common_gitdir: AbsPath,
    pub registered_at: String,
    pub trees_dir: Option<AbsPath>,
    pub default_branch: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Canonical,
    Branch,
    Pr,
    Ref,
    Adopted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TreeSource {
    pub kind: SourceKind,
    pub branch: Option<String>,
    pub pr: Option<u64>,
    pub start: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TreeRec {
    pub tree_id: TreeId,
    pub label: Label,
    pub name: String,
    pub canonical: bool,
    pub path: AbsPath,
    pub slot: u32,
    pub geometry: Geometry,
    pub ports: PortMap,
    pub name_short: String,
    pub session_name: String,
    pub created_at: String,
    pub agent: Option<String>,
    pub source: TreeSource,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub label: Label,
    pub name: String,
    pub slot: u32,
    pub geometry: Geometry,
    pub ports: PortMap,
    pub name_short: String,
    pub session_name: String,
    pub path: AbsPath,
    pub materialized: Vec<RelPath>,
    pub removed_at: String,
    pub reason: String,
}

impl Registry {
    pub fn invariant_view(&self) -> RegistryInvariantView {
        RegistryInvariantView {
            labels: self
                .labels
                .iter()
                .map(|(label, record)| {
                    (
                        label.clone(),
                        record.path.as_str().to_owned(),
                        record.gitdir_id.clone(),
                    )
                })
                .collect(),
            trees: self
                .trees
                .iter()
                .map(|tree| RegistryTreeView {
                    target: Target {
                        label: tree.label.clone(),
                        name: tree.name.clone(),
                    },
                    tree_id: tree.tree_id.clone(),
                    canonical: tree.canonical,
                    path: tree.path.as_str().to_owned(),
                    slot: tree.slot,
                    range: geometry_range(tree.geometry),
                    name_short: tree.name_short.clone(),
                    session_name: tree.session_name.clone(),
                })
                .collect(),
            tombstones: self
                .tombstones
                .iter()
                .map(|tombstone| RegistryTombstoneView {
                    target: Target {
                        label: tombstone.label.clone(),
                        name: tombstone.name.clone(),
                    },
                    path: tombstone.path.as_str().to_owned(),
                    slot: tombstone.slot,
                    range: geometry_range(tombstone.geometry),
                    name_short: tombstone.name_short.clone(),
                    session_name: tombstone.session_name.clone(),
                })
                .collect(),
        }
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        if self.schema != 1 {
            return Err(CoreError::new(
                ExitClass::State,
                "REGISTRY_CORRUPT",
                format!("unsupported registry schema {}", self.schema),
                "delete the corrupt registry and re-register the affected checkouts",
            ));
        }
        validate_registry(&self.invariant_view())
    }
}

fn geometry_range(geometry: Geometry) -> (u32, u32) {
    let first = u32::from(geometry.port_base);
    (first, first + u32::from(geometry.stride).saturating_sub(1))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryInvariantView {
    pub labels: Vec<(Label, String, String)>,
    pub trees: Vec<RegistryTreeView>,
    pub tombstones: Vec<RegistryTombstoneView>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryTreeView {
    pub target: Target,
    pub tree_id: TreeId,
    pub canonical: bool,
    pub path: String,
    pub slot: u32,
    pub range: (u32, u32),
    pub name_short: String,
    pub session_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryTombstoneView {
    pub target: Target,
    pub path: String,
    pub slot: u32,
    pub range: (u32, u32),
    pub name_short: String,
    pub session_name: String,
}

pub fn validate_registry(view: &RegistryInvariantView) -> Result<(), CoreError> {
    let corrupt = |why: String| {
        CoreError::new(
            ExitClass::State,
            "REGISTRY_CORRUPT",
            why,
            "delete the corrupt registry and re-register the affected checkouts",
        )
    };
    let mut gitdirs = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for (_, path, gitdir) in &view.labels {
        if !paths.insert(path) || !gitdirs.insert(gitdir) {
            return Err(corrupt("duplicate label path or common gitdir".to_owned()));
        }
    }
    let mut targets = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut slots = BTreeSet::new();
    let mut shorts = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for tree in &view.trees {
        let valid_target = if tree.canonical {
            tree.target.name == "canonical"
        } else {
            TreeName::new(&tree.target.name).is_ok()
        };
        if !valid_target {
            return Err(corrupt(
                "tree address does not match its canonical flag".to_owned(),
            ));
        }
        if !valid_tree_id(&tree.tree_id) {
            return Err(corrupt(
                "tree_id is not 32 lowercase hexadecimal characters".to_owned(),
            ));
        }
        if !targets.insert(tree.target.clone())
            || !ids.insert(&tree.tree_id)
            || !slots.insert(tree.slot)
            || !shorts.insert(&tree.name_short)
            || !sessions.insert(&tree.session_name)
        {
            return Err(corrupt("duplicate tree identity or coordinate".to_owned()));
        }
        if !tree.canonical && !paths.insert(&tree.path) {
            return Err(corrupt("duplicate tree path".to_owned()));
        }
        if ranges
            .iter()
            .any(|range| tree.range.0 <= range.1 && range.0 <= tree.range.1)
        {
            return Err(corrupt("overlapping port ranges".to_owned()));
        }
        ranges.push(tree.range);
    }
    for tombstone in &view.tombstones {
        if !targets.insert(tombstone.target.clone())
            || !slots.insert(tombstone.slot)
            || !shorts.insert(&tombstone.name_short)
            || !sessions.insert(&tombstone.session_name)
        {
            return Err(corrupt("duplicate tree address or coordinate".to_owned()));
        }
        if ranges
            .iter()
            .any(|range| tombstone.range.0 <= range.1 && range.0 <= tombstone.range.1)
        {
            return Err(corrupt("overlapping port ranges".to_owned()));
        }
        ranges.push(tombstone.range);
    }
    for (label, _, _) in &view.labels {
        let canonical = view
            .trees
            .iter()
            .filter(|tree| tree.target.label == *label && tree.canonical)
            .collect::<Vec<_>>();
        if canonical.len() != 1 {
            return Err(corrupt(format!(
                "label `{label}` does not have exactly one canonical tree"
            )));
        }
        let label_path = view
            .labels
            .iter()
            .find(|(candidate, _, _)| candidate == label)
            .map(|(_, path, _)| path);
        if label_path != Some(&canonical[0].path) {
            return Err(corrupt(format!(
                "label `{label}` canonical tree path differs from the label path"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_grammars_and_derivations_match_the_spec() {
        assert!(Label::new("a.b-c_1").is_ok());
        assert!(Label::new(".").is_err());
        assert!(TreeName::new("canonical").is_err());
        assert!(TaskId::new("Build").is_err());
        assert!(EnvKey::new("WT_BAD").is_err());
        assert_eq!(name_snake(" Feature//One "), "feature_one");
        let target = Target::parse("repo/work").unwrap();
        assert_eq!(tree_state_path(&target), "state/repo/work.json");
        assert_eq!(repo_state_path(&target.label), "state/repo/_repo.json");
        assert_eq!(machine_state_path(), "state/_machine.json");
        assert_eq!(
            scope_enc(&RelDir::new("services/api").unwrap()),
            "services%2Fapi"
        );
        assert_eq!(scope_enc(&RelDir::new(".").unwrap()), ".");
        let short = name_short("repo", "a very long feature branch name");
        assert!(short.len() <= 31);
        assert!(short
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'));
    }

    #[test]
    fn registry_allows_canonical_path_alias_but_rejects_other_duplicates() {
        let label = Label::new("repo").unwrap();
        let canonical = RegistryTreeView {
            target: Target::canonical(label.clone()),
            tree_id: "01".repeat(16),
            canonical: true,
            path: "/repo".to_owned(),
            slot: 0,
            range: (20000, 20015),
            name_short: "repo_c".to_owned(),
            session_name: "s1".to_owned(),
        };
        let work = RegistryTreeView {
            target: Target {
                label: label.clone(),
                name: "work".to_owned(),
            },
            tree_id: "02".repeat(16),
            canonical: false,
            path: "/trees/work".to_owned(),
            slot: 1,
            range: (20016, 20031),
            name_short: "repo_w".to_owned(),
            session_name: "s2".to_owned(),
        };
        let view = RegistryInvariantView {
            labels: vec![(label, "/repo".to_owned(), "/repo/.git".to_owned())],
            trees: vec![canonical, work.clone()],
            tombstones: vec![],
        };
        validate_registry(&view).unwrap();
        let mut bad = view;
        bad.tombstones.push(RegistryTombstoneView {
            target: work.target,
            path: work.path,
            slot: work.slot,
            range: work.range,
            name_short: work.name_short,
            session_name: work.session_name,
        });
        assert_eq!(
            validate_registry(&bad).unwrap_err().code.0,
            "REGISTRY_CORRUPT"
        );
    }

    #[test]
    fn registry_schema_round_trips_every_persisted_record() {
        let label = Label::new("repo").unwrap();
        let geometry = Geometry {
            base: 20_000,
            stride: 16,
            port_base: 20_000,
        };
        let registry = Registry {
            schema: 1,
            labels: BTreeMap::from([(
                label.clone(),
                LabelRec {
                    path: AbsPath::new("/repo").unwrap(),
                    gitdir_id: "gitdir".to_owned(),
                    common_gitdir: AbsPath::new("/repo/.git").unwrap(),
                    registered_at: "TIME".to_owned(),
                    trees_dir: Some(AbsPath::new("/trees/repo").unwrap()),
                    default_branch: Some("main".to_owned()),
                },
            )]),
            trees: vec![TreeRec {
                tree_id: "01".repeat(16),
                label: label.clone(),
                name: "canonical".to_owned(),
                canonical: true,
                path: AbsPath::new("/repo").unwrap(),
                slot: 0,
                geometry,
                ports: PortMap::from([(PortName::new("http").unwrap(), 0)]),
                name_short: "repo_canonical_deadbeef".to_owned(),
                session_name: "wt_repo_canonical_deadbeef".to_owned(),
                created_at: "TIME".to_owned(),
                agent: Some("codex".to_owned()),
                source: TreeSource {
                    kind: SourceKind::Canonical,
                    branch: Some("main".to_owned()),
                    pr: None,
                    start: None,
                },
            }],
            tombstones: vec![Tombstone {
                label,
                name: "old".to_owned(),
                slot: 1,
                geometry: Geometry {
                    port_base: 20_016,
                    ..geometry
                },
                ports: PortMap::new(),
                name_short: "repo_old_deadbeef".to_owned(),
                session_name: "wt_repo_old_deadbeef".to_owned(),
                path: AbsPath::new("/trees/repo/old").unwrap(),
                materialized: vec![RelPath::new("target").unwrap()],
                removed_at: "TIME".to_owned(),
                reason: "removed".to_owned(),
            }],
        };
        registry.validate().unwrap();
        let json = serde_json::to_string(&registry).unwrap();
        let decoded: Registry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, registry);
    }

    #[test]
    fn tombstone_paths_do_not_participate_in_live_path_uniqueness() {
        let label = Label::new("repo").unwrap();
        let canonical = RegistryTreeView {
            target: Target::canonical(label.clone()),
            tree_id: "01".repeat(16),
            canonical: true,
            path: "/repo".to_owned(),
            slot: 0,
            range: (20_000, 20_015),
            name_short: "canonical".to_owned(),
            session_name: "canonical".to_owned(),
        };
        let view = RegistryInvariantView {
            labels: vec![(label.clone(), "/repo".to_owned(), "/repo/.git".to_owned())],
            trees: vec![canonical],
            tombstones: vec![RegistryTombstoneView {
                target: Target {
                    label,
                    name: "old".to_owned(),
                },
                path: "/repo".to_owned(),
                slot: 1,
                range: (20_016, 20_031),
                name_short: "old".to_owned(),
                session_name: "old".to_owned(),
            }],
        };
        validate_registry(&view).unwrap();
    }
}
