use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    config::{Command, Task, TiedTo},
    error::CoreError,
    model::{EnvMap, RelDir},
    resource::ResourceKey,
    ExitClass,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Adapter,
    Repo,
    User,
    Tree,
    Composite,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub scope: RelDir,
    pub origin: Origin,
    pub cwd: RelDir,
    pub needs: Vec<String>,
    pub run: Option<Command>,
    pub exists: Option<Command>,
    pub destroy: Option<Command>,
    pub tied_to: Option<TiedTo>,
    pub name: Option<String>,
    pub env: EnvMap,
    pub lock: Option<String>,
    pub timeout: Option<String>,
    pub ready_within: Option<String>,
    pub description: Option<String>,
    pub snapshot_env: Vec<String>,
    pub sys_locks: Vec<String>,
    pub resource: Option<ResourceKey>,
}

impl Node {
    pub fn from_task(id: String, scope: RelDir, origin: Origin, task: &Task) -> Self {
        let cwd = task.cwd.clone().unwrap_or_else(|| scope.clone());
        Self {
            id,
            scope,
            origin,
            cwd,
            needs: task.needs.clone(),
            run: task.run.clone(),
            exists: task.exists.clone(),
            destroy: task.destroy.clone(),
            tied_to: task.tied_to,
            name: task.name.clone(),
            env: task.env.clone().into_iter().collect(),
            lock: task.lock.clone(),
            timeout: task.timeout.clone(),
            ready_within: task.ready_within.clone(),
            description: task.description.clone(),
            snapshot_env: task.snapshot_env.clone(),
            sys_locks: task.sys_locks.clone(),
            resource: None,
        }
    }

    fn order_key(&self) -> (&str, &str) {
        (self.scope.as_str(), &self.id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskPlan {
    pub root: String,
    pub nodes: Vec<Node>,
}

pub fn plan(root: &str, catalog: &BTreeMap<String, Node>) -> Result<TaskPlan, CoreError> {
    if !catalog.contains_key(root) {
        return Err(CoreError::new(
            ExitClass::NotFound,
            "NOT_FOUND",
            format!("task `{root}` was not found"),
            "run `wt tasks` to list tasks",
        ));
    }
    let mut required = BTreeSet::new();
    collect(root, catalog, &mut required, &mut BTreeSet::new())?;
    let mut emitted = BTreeSet::new();
    let mut nodes = Vec::new();
    while emitted.len() < required.len() {
        let mut ready: Vec<&Node> = required
            .iter()
            .filter(|id| !emitted.contains(*id))
            .filter_map(|id| catalog.get(id))
            .filter(|node| node.needs.iter().all(|need| emitted.contains(need)))
            .collect();
        ready.sort_by_key(|node| node.order_key());
        if ready.is_empty() {
            return Err(invalid_graph("task graph contains a cycle"));
        }
        for node in ready {
            emitted.insert(node.id.clone());
            nodes.push(node.clone());
        }
    }
    Ok(TaskPlan {
        root: root.to_owned(),
        nodes,
    })
}

fn collect(
    id: &str,
    catalog: &BTreeMap<String, Node>,
    required: &mut BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
) -> Result<(), CoreError> {
    if required.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id.to_owned()) {
        return Err(invalid_graph("task graph contains a cycle"));
    }
    let node = catalog
        .get(id)
        .ok_or_else(|| invalid_graph(format!("task `{id}` has an unresolved dependency")))?;
    for need in &node.needs {
        collect(need, catalog, required, visiting)?;
    }
    visiting.remove(id);
    required.insert(id.to_owned());
    Ok(())
}

fn invalid_graph(message: impl Into<String>) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "CONFIG_INVALID",
        message,
        "fix task needs so every dependency exists and the graph is acyclic",
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeldLocks {
    pub tree: bool,
    pub repo_git: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[repr(u8)]
pub enum LockLevel {
    Tree = 1,
    RepoGit = 2,
    Resource = 3,
    Named = 4,
    RegistryRmw = 5,
    StateRmw = 6,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlannedLock {
    TreeShared,
    RepoGit,
    Resource { key: ResourceKey },
    Named { name: String },
}

impl PlannedLock {
    pub const fn level(&self) -> LockLevel {
        match self {
            Self::TreeShared => LockLevel::Tree,
            Self::RepoGit => LockLevel::RepoGit,
            Self::Resource { .. } => LockLevel::Resource,
            Self::Named { .. } => LockLevel::Named,
        }
    }
}

pub fn lock_plan(node: &Node, held: HeldLocks) -> Vec<PlannedLock> {
    let mut locks = Vec::new();
    if !held.tree {
        locks.push(PlannedLock::TreeShared);
    }
    if node.sys_locks.iter().any(|lock| lock == "RepoGit") && !held.repo_git {
        locks.push(PlannedLock::RepoGit);
    }
    if let Some(key) = &node.resource {
        locks.push(PlannedLock::Resource { key: key.clone() });
    }
    if let Some(name) = &node.lock {
        locks.push(PlannedLock::Named { name: name.clone() });
    }
    locks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::TiedTo, model::Label};

    fn node(id: &str, scope: &str, needs: &[&str]) -> Node {
        Node {
            id: id.to_owned(),
            scope: RelDir::new(scope).unwrap(),
            origin: Origin::Repo,
            cwd: RelDir::new(scope).unwrap(),
            needs: needs.iter().map(|value| (*value).to_owned()).collect(),
            run: Some(Command::Shell("true".to_owned())),
            exists: None,
            destroy: None,
            tied_to: None,
            name: None,
            env: EnvMap::new(),
            lock: None,
            timeout: None,
            ready_within: None,
            description: None,
            snapshot_env: Vec::new(),
            sys_locks: Vec::new(),
            resource: None,
        }
    }

    #[test]
    fn plan_is_topological_with_stable_ties() {
        let catalog = BTreeMap::from([
            ("root".to_owned(), node("root", ".", &["z", "a"])),
            ("z".to_owned(), node("z", "z", &[])),
            ("a".to_owned(), node("a", "a", &[])),
        ]);
        let ids: Vec<_> = plan("root", &catalog)
            .unwrap()
            .nodes
            .into_iter()
            .map(|node| node.id)
            .collect();
        assert_eq!(ids, ["a", "z", "root"]);
    }

    #[test]
    fn lock_plan_obeys_levels_and_held_tokens() {
        assert_eq!(
            [
                LockLevel::Tree as u8,
                LockLevel::RepoGit as u8,
                LockLevel::Resource as u8,
                LockLevel::Named as u8,
                LockLevel::RegistryRmw as u8,
                LockLevel::StateRmw as u8,
            ],
            [1, 2, 3, 4, 5, 6]
        );
        let mut node = node("db", ".", &[]);
        node.sys_locks.push("RepoGit".to_owned());
        node.lock = Some("serial".to_owned());
        node.resource = Some(ResourceKey {
            label: Label::new("repo").unwrap(),
            tied_to: TiedTo::Tree,
            name: Some("work".to_owned()),
            scope: RelDir::new(".").unwrap(),
            task: "db".to_owned(),
        });
        let locks = lock_plan(&node, HeldLocks::default());
        assert_eq!(
            locks.iter().map(PlannedLock::level).collect::<Vec<_>>(),
            [
                LockLevel::Tree,
                LockLevel::RepoGit,
                LockLevel::Resource,
                LockLevel::Named
            ]
        );
        assert_eq!(
            lock_plan(
                &node,
                HeldLocks {
                    tree: true,
                    repo_git: true,
                }
            )
            .iter()
            .map(PlannedLock::level)
            .collect::<Vec<_>>(),
            [LockLevel::Resource, LockLevel::Named]
        );
    }

    #[test]
    fn plan_rejects_cycles_and_unresolved_dependencies() {
        let cycle = BTreeMap::from([
            ("a".to_owned(), node("a", ".", &["b"])),
            ("b".to_owned(), node("b", ".", &["a"])),
        ]);
        assert!(plan("a", &cycle).unwrap_err().message.contains("cycle"));

        let unresolved = BTreeMap::from([("a".to_owned(), node("a", ".", &["missing"]))]);
        assert!(plan("a", &unresolved)
            .unwrap_err()
            .message
            .contains("unresolved"));
    }
}
