use std::collections::{BTreeMap, BTreeSet};

use crate::{
    config::TiedTo,
    resource::{ResourceRecord, ResourceSnapshot},
    snapshot::is_tree_specific_key,
};

/// Reconciles the effective declaration set without disturbing a frozen
/// instance or resource truth. Undeclared records remain until teardown
/// confirms absence (SPEC §10.5).
pub fn reconcile(
    records: &mut BTreeMap<String, ResourceRecord>,
    mut declared: BTreeMap<String, ResourceSnapshot>,
    refresh_skipped: &BTreeSet<String>,
) {
    let declared_keys: BTreeSet<_> = declared.keys().chain(refresh_skipped).cloned().collect();
    for (scoped_task, record) in records.iter_mut() {
        record.undeclared = !declared_keys.contains(scoped_task);
    }
    for (scoped_task, snapshot) in &mut declared {
        strip_tree_specific_env(snapshot);
        match records.get_mut(scoped_task) {
            Some(record) => {
                record.declaration = snapshot.clone();
                record.key = snapshot.key.clone();
                record.undeclared = false;
            }
            None => {
                records.insert(
                    scoped_task.clone(),
                    ResourceRecord::declared(snapshot.clone()),
                );
            }
        }
    }
}

/// Repo-tied declarations carry no tree coordinate environment. The invoking
/// tree's resulting stripped declaration is authoritative until an instance is
/// frozen; there is no cross-tree agreement comparison.
pub fn strip_tree_specific_env(snapshot: &mut ResourceSnapshot) {
    if snapshot.key.tied_to != TiedTo::Repo {
        return;
    }
    snapshot.env.retain(|key, _| !is_tree_specific_key(key));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{EnvMap, Label, RelDir},
        resource::{ExpandedCommand, ResourceKey, ResourceState, SnapshotRoots},
    };

    fn snapshot(task: &str, tied_to: TiedTo, tree: &str) -> ResourceSnapshot {
        ResourceSnapshot {
            schema: 1,
            key: ResourceKey {
                label: Label::new("repo").unwrap(),
                tied_to,
                name: (tied_to == TiedTo::Tree).then(|| tree.to_owned()),
                scope: RelDir::new(".").unwrap(),
                task: task.to_owned(),
            },
            name: format!("repo_{task}"),
            cwd_rel: RelDir::new(".").unwrap(),
            exists: None,
            destroy: ExpandedCommand::Shell {
                shell: "drop".to_owned(),
            },
            run: None,
            env: EnvMap::from([
                ("WT_ROOT".to_owned(), format!("/{tree}")),
                ("WT_PORT_HTTP".to_owned(), "20000".to_owned()),
                ("KEEP".to_owned(), "yes".to_owned()),
            ]),
            bin_dirs: Vec::new(),
            bin_exes: Vec::new(),
            roots: SnapshotRoots {
                tree: format!("/{tree}"),
                home: "/home".to_owned(),
            },
            recorded_at: "T".to_owned(),
        }
    }

    #[test]
    fn reconcile_replaces_only_declarations_and_marks_missing_tasks() {
        let old = snapshot("db", TiedTo::Tree, "old");
        let mut record = ResourceRecord::declared(old.clone());
        record.instance = Some(old);
        record.state = ResourceState::Present;
        let mut records = BTreeMap::from([("db".to_owned(), record)]);

        reconcile(
            &mut records,
            BTreeMap::from([("db".to_owned(), snapshot("db", TiedTo::Tree, "new"))]),
            &BTreeSet::new(),
        );
        assert_eq!(records["db"].declaration.roots.tree, "/new");
        assert_eq!(records["db"].instance.as_ref().unwrap().roots.tree, "/old");
        assert_eq!(records["db"].state, ResourceState::Present);

        reconcile(&mut records, BTreeMap::new(), &BTreeSet::new());
        assert!(records["db"].undeclared);
    }

    #[test]
    fn repo_declaration_is_stripped_without_agreement_or_provenance() {
        let mut records = BTreeMap::new();
        reconcile(
            &mut records,
            BTreeMap::from([("db".to_owned(), snapshot("db", TiedTo::Repo, "invoking"))]),
            &BTreeSet::new(),
        );
        assert_eq!(
            records["db"].declaration.env,
            EnvMap::from([("KEEP".to_owned(), "yes".to_owned())])
        );
    }

    #[test]
    fn refresh_skipped_preserves_the_previous_declaration_and_declared_status() {
        let old = snapshot("db", TiedTo::Tree, "old");
        let mut records =
            BTreeMap::from([("db".to_owned(), ResourceRecord::declared(old.clone()))]);
        reconcile(
            &mut records,
            BTreeMap::new(),
            &BTreeSet::from(["db".to_owned()]),
        );
        assert_eq!(records["db"].declaration, old);
        assert!(!records["db"].undeclared);
    }
}
