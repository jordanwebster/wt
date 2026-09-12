//! Which cargo output a build directory can lose (SPEC §11.9).
//!
//! Cargo keeps one artifact set per build configuration and never deletes a
//! superseded one, and it writes no "last used" mark on a unit it reuses, so
//! neither the directory nor file ages can say what is still read. The
//! decision here starts from the workspace's own units — the ones compiled
//! from a crate root `cargo metadata` lists as a workspace member — keeps the
//! newest of each configuration, and follows their recorded dependency
//! fingerprints outward: everything reached is live, everything else is not.
//! A dependency is therefore never judged on its own: two variants of one
//! crate that differ only in what they were compiled against both survive
//! while something live names them, and a bumped dependency's old chain dies
//! once nothing does.
//!
//! Two kinds of file live beside units and outlast them. Codegen-unit object
//! files, which rustc leaves under `deps/` on macOS for the debugger, are
//! never removed by cargo, so a unit's stale objects and the objects of
//! units that no longer exist are the sweep's to reclaim. Incremental session
//! directories are kept per crate up to the number of live configurations of
//! that crate, newest first, with age as a backstop.

use std::collections::{BTreeMap, BTreeSet};

/// What a unit produced, read from the files beside it. Two units of one
/// crate root that differ only here are both live: `cargo check` and
/// `cargo build` keep separate metadata-only and library units and each
/// command reuses its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum OutputKind {
    /// Nothing under `deps/` or `build/` carries the hash (a build-script run).
    None,
    /// Only `.rmeta`: a check-mode unit.
    Metadata,
    /// An `.rlib` or a dynamic library.
    Library,
    /// An executable: a binary, a test harness, or a build script.
    Executable,
}

/// One `.fingerprint/<package>-<hash>` directory, as observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitObs {
    /// The `<hash>` suffix shared by everything the unit produced.
    pub hash: String,
    /// The fingerprint file's name: flavour, target kind and target name
    /// (`lib-serde`, `test-lib-wt_core`, `run-build-script-build-script-build`).
    pub kind: String,
    /// The fingerprint's own hash, as the short file records it.
    pub fingerprint: Option<u64>,
    /// Fingerprint hashes of the unit's recorded dependencies.
    pub deps: Vec<u64>,
    /// The crate root the unit was compiled from, from its dep-info, made
    /// absolute. `None` when the unit has no dep-info (a build-script run).
    pub crate_root: Option<String>,
    /// The fingerprint fields that say what the unit is rather than what it
    /// was built against: features, target, profile, compile kind, rustflags
    /// and config. Two workspace units with equal identity, kind, crate root
    /// and output are the same thing built twice.
    pub identity: String,
    pub output: OutputKind,
    /// Modification time of `invoked.timestamp`, seconds since the epoch:
    /// when cargo last started compiling the unit.
    pub compiled_at: u64,
}

/// One file or directory under `deps/` or `build/` that carries a unit
/// hash. Deleted with its unit; judged on its own when the unit is live.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileObs {
    /// Path relative to the profile directory (`deps/libx-<hash>.rlib`).
    pub path: String,
    pub hash: String,
    /// A codegen-unit object file (`*.o`), the one kind of file rustc
    /// rewrites per session and never removes.
    pub object: bool,
    /// Hard-link count: an object a current incremental session still
    /// links has two, one left behind by a finished session has one.
    pub links: u64,
    /// Modification time, seconds since the epoch.
    pub modified_at: u64,
}

/// One `incremental/<crate>-<hash>` directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalObs {
    pub name: String,
    /// The crate the session belongs to: the directory name without its
    /// hash suffix, as rustc spells crate names (underscores).
    pub crate_name: String,
    /// Modification time, seconds since the epoch. rustc rewrites the
    /// directory on every use, so this is a use record.
    pub modified_at: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub units: Vec<UnitObs>,
    /// Every target source path of every workspace member in the current
    /// resolve: the crate roots whose units are the sweep's starting set.
    pub workspace_roots: BTreeSet<String>,
    pub files: Vec<FileObs>,
    pub incremental: Vec<IncrementalObs>,
    pub now: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    /// Hashes of the units to delete, in observation order.
    pub dead_units: Vec<String>,
    /// Files to delete on their own: those whose unit no longer exists, and
    /// stale objects of live units. Files of dead units are not repeated.
    pub dead_files: Vec<String>,
    pub dead_incremental: Vec<String>,
    pub live_units: usize,
    /// Set when nothing may be deleted: no unit matched the workspace at
    /// all — a path mismatch, a resolve with no members, or a directory
    /// whose every unit lacks readable dep-info — which means the
    /// observation is not about this workspace rather than that everything
    /// is stale.
    pub refused: Option<&'static str>,
}

/// How long an incremental session directory may go unused whatever its
/// crate still has live.
pub const INCREMENTAL_MAX_AGE_SECS: u64 = 14 * 24 * 60 * 60;

pub fn plan(snapshot: &Snapshot) -> Plan {
    let mut plan = Plan::default();
    if !snapshot.units.is_empty() && snapshot.workspace_roots.is_empty() {
        plan.refused = Some("the workspace resolves to no crate roots");
        return plan;
    }
    // The workspace's own units, newest of each configuration: the roots
    // everything live is reached from.
    let members = snapshot
        .units
        .iter()
        .enumerate()
        .filter(|(_, unit)| {
            unit.crate_root
                .as_ref()
                .is_some_and(|root| snapshot.workspace_roots.contains(root))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if !snapshot.units.is_empty() && members.is_empty() {
        plan.refused = Some("no unit was compiled from a crate root of this workspace");
        return plan;
    }
    let mut newest = BTreeMap::<(String, String, String, OutputKind), u64>::new();
    for &index in &members {
        let unit = &snapshot.units[index];
        let entry = newest.entry(slot_key(unit)).or_insert(0);
        *entry = (*entry).max(unit.compiled_at);
    }
    let mut live = vec![false; snapshot.units.len()];
    let mut queue = Vec::new();
    for &index in &members {
        let unit = &snapshot.units[index];
        if newest[&slot_key(unit)] == unit.compiled_at {
            live[index] = true;
            queue.push(index);
        }
    }
    // Everything a live unit's recorded dependencies name is live too,
    // transitively — a dependency variant, a build-script run, an older
    // workspace twin some live harness still links against.
    let by_fingerprint = snapshot
        .units
        .iter()
        .enumerate()
        .filter_map(|(index, unit)| unit.fingerprint.map(|hash| (hash, index)))
        .fold(
            BTreeMap::<u64, Vec<usize>>::new(),
            |mut map, (hash, index)| {
                map.entry(hash).or_default().push(index);
                map
            },
        );
    while let Some(index) = queue.pop() {
        for dep in &snapshot.units[index].deps {
            for &target in by_fingerprint.get(dep).map_or(&[][..], Vec::as_slice) {
                if !live[target] {
                    live[target] = true;
                    queue.push(target);
                }
            }
        }
    }
    plan.live_units = live.iter().filter(|live| **live).count();
    plan.dead_units = snapshot
        .units
        .iter()
        .zip(&live)
        .filter(|(_, live)| !**live)
        .map(|(unit, _)| unit.hash.clone())
        .collect();
    // Files: orphans die, and a live unit's objects die when no current
    // session links them and they predate the unit's last compile — the
    // leftovers of earlier incremental sessions. Objects the last compile
    // wrote, or reused by hard link from the session before, stay.
    let by_hash = snapshot
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| (unit.hash.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    for file in &snapshot.files {
        match by_hash.get(file.hash.as_str()) {
            None => plan.dead_files.push(file.path.clone()),
            Some(&index) if live[index] => {
                let unit = &snapshot.units[index];
                if file.object && file.links <= 1 && file.modified_at < unit.compiled_at {
                    plan.dead_files.push(file.path.clone());
                }
            }
            Some(_) => {}
        }
    }
    // Incremental: per crate, as many session directories as the crate has
    // live configurations, newest first; anything beyond that belongs to a
    // configuration nothing builds any more. Age retires the rest.
    let mut allowed = BTreeMap::<String, usize>::new();
    for (unit, live) in snapshot.units.iter().zip(&live) {
        if *live {
            if let Some(name) = crate_name_of(&unit.kind) {
                *allowed.entry(name).or_default() += 1;
            }
        }
    }
    let mut by_crate = BTreeMap::<&str, Vec<&IncrementalObs>>::new();
    for dir in &snapshot.incremental {
        by_crate
            .entry(dir.crate_name.as_str())
            .or_default()
            .push(dir);
    }
    let mut dead_incremental = BTreeSet::new();
    for (crate_name, mut dirs) in by_crate {
        dirs.sort_by(|a, b| b.modified_at.cmp(&a.modified_at).then(a.name.cmp(&b.name)));
        let keep = allowed.get(crate_name).copied().unwrap_or(0);
        for (rank, dir) in dirs.iter().enumerate() {
            let aged = snapshot.now.saturating_sub(dir.modified_at) > INCREMENTAL_MAX_AGE_SECS;
            if rank >= keep || aged {
                dead_incremental.insert(dir.name.clone());
            }
        }
    }
    plan.dead_incremental = snapshot
        .incremental
        .iter()
        .filter(|dir| dead_incremental.contains(&dir.name))
        .map(|dir| dir.name.clone())
        .collect();
    plan
}

fn slot_key(unit: &UnitObs) -> (String, String, String, OutputKind) {
    (
        unit.kind.clone(),
        unit.crate_root.clone().unwrap_or_default(),
        unit.identity.clone(),
        unit.output,
    )
}

/// The crate a unit's incremental session directory is named after, from
/// the fingerprint file's name: `test-lib-wt_core` → `wt_core`,
/// `bin-claude-hook` → `claude_hook`, `test-integration-test-spec` →
/// `spec`, a build script → `build_script_build`. A build-script run has
/// no session directory.
pub fn crate_name_of(kind: &str) -> Option<String> {
    if kind.starts_with("run-") {
        return None;
    }
    let rest = kind.strip_prefix("test-").unwrap_or(kind);
    if rest.starts_with("build-script-") {
        return Some("build_script_build".to_owned());
    }
    let name = [
        "integration-test-",
        "lib-",
        "bin-",
        "example-",
        "bench-",
        "test-",
    ]
    .iter()
    .find_map(|prefix| rest.strip_prefix(prefix))
    .unwrap_or(rest);
    Some(name.replace('-', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(hash: &str, kind: &str, root: Option<&str>, identity: &str, at: u64) -> UnitObs {
        UnitObs {
            hash: hash.to_owned(),
            kind: kind.to_owned(),
            fingerprint: Some(hash.bytes().fold(0_u64, |acc, byte| {
                acc.wrapping_mul(131).wrapping_add(u64::from(byte))
            })),
            deps: Vec::new(),
            crate_root: root.map(str::to_owned),
            identity: identity.to_owned(),
            output: OutputKind::Library,
            compiled_at: at,
        }
    }

    fn roots(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    fn file(path: &str, hash: &str, object: bool, links: u64, at: u64) -> FileObs {
        FileObs {
            path: path.to_owned(),
            hash: hash.to_owned(),
            object,
            links,
            modified_at: at,
        }
    }

    #[test]
    fn dependencies_live_only_while_a_live_workspace_unit_reaches_them() {
        let mut lib = unit("aaaa", "lib-app", Some("/ws/src/lib.rs"), "x", 10);
        let mut serde_new = unit(
            "s001",
            "lib-serde",
            Some("/reg/serde-1.0.2/src/lib.rs"),
            "x",
            10,
        );
        let serde_old = unit(
            "s000",
            "lib-serde",
            Some("/reg/serde-1.0.1/src/lib.rs"),
            "x",
            5,
        );
        let run = unit("r001", "run-build-script-build-script-build", None, "x", 8);
        let orphan_run = unit("r002", "run-build-script-build-script-build", None, "x", 8);
        let gone_test = unit(
            "t000",
            "test-integration-test-gone",
            Some("/ws/tests/gone.rs"),
            "x",
            5,
        );
        lib.deps = vec![serde_new.fingerprint.unwrap()];
        serde_new.deps = vec![run.fingerprint.unwrap()];
        let snapshot = Snapshot {
            units: vec![lib, serde_new, serde_old, run, orphan_run, gone_test],
            workspace_roots: roots(&["/ws/src/lib.rs"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert_eq!(plan.dead_units, vec!["s000", "r002", "t000"]);
        assert_eq!(plan.live_units, 3);
        assert!(plan.refused.is_none());
    }

    #[test]
    fn dependency_variants_named_by_live_consumers_both_survive() {
        // Two `getrandom` units agree on every identity field and differ
        // only in the `libc` they were compiled against; a live workspace
        // unit reaches each through a different consumer.
        let mut lib = unit("aaaa", "lib-app", Some("/ws/src/lib.rs"), "x", 10);
        let mut uuid = unit("u001", "lib-uuid", Some("/reg/uuid/src/lib.rs"), "x", 9);
        let mut tempfile = unit(
            "f001",
            "lib-tempfile",
            Some("/reg/tempfile/src/lib.rs"),
            "x",
            9,
        );
        let getrandom_a = unit(
            "g001",
            "lib-getrandom",
            Some("/reg/getrandom/src/lib.rs"),
            "x",
            9,
        );
        let getrandom_b = unit(
            "g002",
            "lib-getrandom",
            Some("/reg/getrandom/src/lib.rs"),
            "x",
            3,
        );
        uuid.deps = vec![getrandom_a.fingerprint.unwrap()];
        tempfile.deps = vec![getrandom_b.fingerprint.unwrap()];
        lib.deps = vec![uuid.fingerprint.unwrap(), tempfile.fingerprint.unwrap()];
        let snapshot = Snapshot {
            units: vec![lib, uuid, tempfile, getrandom_a, getrandom_b],
            workspace_roots: roots(&["/ws/src/lib.rs"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert!(plan.dead_units.is_empty(), "{plan:?}");
        assert_eq!(plan.live_units, 5);
    }

    #[test]
    fn a_dependency_bump_retires_the_old_chain() {
        // The workspace library built twice: once against `dep` 1.0.0 and,
        // after the lockfile moved, against 1.0.1. The older twin and
        // everything only it reached go.
        let mut lib_old = unit("l000", "lib-app", Some("/ws/src/lib.rs"), "x", 5);
        let mut lib_new = unit("l001", "lib-app", Some("/ws/src/lib.rs"), "x", 10);
        let dep_old = unit("d000", "lib-dep", Some("/reg/dep-1.0.0/src/lib.rs"), "x", 5);
        let dep_new = unit(
            "d001",
            "lib-dep",
            Some("/reg/dep-1.0.1/src/lib.rs"),
            "x",
            10,
        );
        // A leaf both versions of `dep` used, unchanged by the bump.
        let leaf = unit("e001", "lib-leaf", Some("/reg/leaf/src/lib.rs"), "x", 5);
        let mut dep_old = dep_old;
        let mut dep_new = dep_new;
        dep_old.deps = vec![leaf.fingerprint.unwrap()];
        dep_new.deps = vec![leaf.fingerprint.unwrap()];
        lib_old.deps = vec![dep_old.fingerprint.unwrap()];
        lib_new.deps = vec![dep_new.fingerprint.unwrap()];
        let snapshot = Snapshot {
            units: vec![lib_old, lib_new, dep_old, dep_new, leaf],
            workspace_roots: roots(&["/ws/src/lib.rs"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert_eq!(plan.dead_units, vec!["l000", "d000"]);
        assert_eq!(plan.live_units, 3);
    }

    #[test]
    fn an_older_workspace_twin_lives_while_a_live_harness_links_it() {
        let lib_old = unit("l000", "lib-app", Some("/ws/src/lib.rs"), "x", 5);
        let lib_new = unit("l001", "lib-app", Some("/ws/src/lib.rs"), "x", 10);
        let mut harness = unit(
            "h001",
            "test-integration-test-spec",
            Some("/ws/tests/spec.rs"),
            "x",
            6,
        );
        harness.output = OutputKind::Executable;
        harness.deps = vec![lib_old.fingerprint.unwrap()];
        let snapshot = Snapshot {
            units: vec![lib_old, lib_new, harness],
            workspace_roots: roots(&["/ws/src/lib.rs", "/ws/tests/spec.rs"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert!(plan.dead_units.is_empty(), "{plan:?}");
    }

    #[test]
    fn feature_variants_and_check_mode_twins_are_distinct_configurations() {
        let mut check = unit("cc01", "lib-app", Some("/ws/src/lib.rs"), "x", 3);
        check.output = OutputKind::Metadata;
        let snapshot = Snapshot {
            units: vec![
                unit("old1", "lib-app", Some("/ws/src/lib.rs"), "x", 5),
                unit("new1", "lib-app", Some("/ws/src/lib.rs"), "x", 9),
                unit("feat", "lib-app", Some("/ws/src/lib.rs"), "features=[a]", 1),
                check,
            ],
            workspace_roots: roots(&["/ws/src/lib.rs"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert_eq!(plan.dead_units, vec!["old1"]);
        assert_eq!(plan.live_units, 3);
    }

    #[test]
    fn orphan_files_and_stale_objects_of_live_units_die() {
        let lib = unit("l001", "lib-app", Some("/ws/src/lib.rs"), "x", 100);
        let dead = unit("l000", "lib-app", Some("/ws/src/lib.rs"), "x", 50);
        let snapshot = Snapshot {
            units: vec![lib, dead],
            workspace_roots: roots(&["/ws/src/lib.rs"]),
            files: vec![
                // The unit's own artifact and current objects.
                file("deps/libapp-l001.rlib", "l001", false, 1, 100),
                file("deps/app-l001.cgu0.rcgu.o", "l001", true, 1, 101),
                // Reused by hard link from the previous session: older,
                // but still linked.
                file("deps/app-l001.cgu1.rcgu.o", "l001", true, 2, 40),
                // Left by a finished session: unlinked and older.
                file("deps/app-l001.cgu2.rcgu.o", "l001", true, 1, 40),
                // A file of the dead unit: deleted with it, not listed twice.
                file("deps/app-l000.cgu0.rcgu.o", "l000", true, 1, 50),
                // A file whose unit no longer exists at all.
                file("deps/app-9999.cgu0.rcgu.o", "9999", true, 1, 1),
                file("build/app-8888", "8888", false, 1, 1),
            ],
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert_eq!(plan.dead_units, vec!["l000"]);
        assert_eq!(
            plan.dead_files,
            vec![
                "deps/app-l001.cgu2.rcgu.o",
                "deps/app-9999.cgu0.rcgu.o",
                "build/app-8888"
            ]
        );
    }

    #[test]
    fn incremental_directories_beyond_a_crates_live_configurations_die() {
        let now = 100 * 24 * 60 * 60;
        let mut test = unit("t001", "test-lib-app", Some("/ws/src/lib.rs"), "test", now);
        test.output = OutputKind::Executable;
        let run = unit(
            "r001",
            "run-build-script-build-script-build",
            None,
            "dev",
            now,
        );
        let mut lib = unit("l001", "lib-app", Some("/ws/src/lib.rs"), "dev", now);
        lib.deps = vec![run.fingerprint.unwrap()];
        let snapshot = Snapshot {
            units: vec![
                lib,
                test,
                unit(
                    "b001",
                    "build-script-build-script-build",
                    Some("/ws/build.rs"),
                    "dev",
                    now,
                ),
                run,
            ],
            workspace_roots: roots(&["/ws/src/lib.rs", "/ws/build.rs"]),
            incremental: vec![
                // Three sessions for two live configurations: the oldest goes.
                IncrementalObs {
                    name: "app-aaa".to_owned(),
                    crate_name: "app".to_owned(),
                    modified_at: now - 30,
                },
                IncrementalObs {
                    name: "app-bbb".to_owned(),
                    crate_name: "app".to_owned(),
                    modified_at: now - 20,
                },
                IncrementalObs {
                    name: "app-ccc".to_owned(),
                    crate_name: "app".to_owned(),
                    modified_at: now - 10,
                },
                // The build script's one session stays.
                IncrementalObs {
                    name: "build_script_build-ddd".to_owned(),
                    crate_name: "build_script_build".to_owned(),
                    modified_at: now - 10,
                },
                // A crate nothing builds any more.
                IncrementalObs {
                    name: "gone-eee".to_owned(),
                    crate_name: "gone".to_owned(),
                    modified_at: now - 10,
                },
                // Within its crate's allowance, but unused for a month.
                IncrementalObs {
                    name: "app-fff".to_owned(),
                    crate_name: "app".to_owned(),
                    modified_at: now - INCREMENTAL_MAX_AGE_SECS - 1,
                },
            ],
            now,
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert_eq!(
            plan.dead_incremental,
            vec!["app-aaa", "gone-eee", "app-fff"]
        );
        assert!(plan.dead_units.is_empty());
    }

    #[test]
    fn crate_names_follow_rustcs_spelling() {
        assert_eq!(crate_name_of("lib-serde").as_deref(), Some("serde"));
        assert_eq!(
            crate_name_of("test-lib-wt_core").as_deref(),
            Some("wt_core")
        );
        assert_eq!(
            crate_name_of("bin-claude-hook").as_deref(),
            Some("claude_hook")
        );
        assert_eq!(
            crate_name_of("test-integration-test-spec").as_deref(),
            Some("spec")
        );
        assert_eq!(
            crate_name_of("build-script-build-script-build").as_deref(),
            Some("build_script_build")
        );
        assert_eq!(crate_name_of("run-build-script-build-script-build"), None);
    }

    #[test]
    fn nothing_matching_the_workspace_refuses_rather_than_deleting_everything() {
        let snapshot = Snapshot {
            units: vec![unit("aaaa", "lib-x", Some("/elsewhere/src/lib.rs"), "x", 1)],
            workspace_roots: roots(&["/ws/src/lib.rs"]),
            ..Snapshot::default()
        };
        let decision = plan(&snapshot);
        assert!(decision.refused.is_some());
        assert!(decision.dead_units.is_empty());
        let empty_roots = Snapshot {
            units: vec![unit("aaaa", "lib-x", Some("/ws/src/lib.rs"), "x", 1)],
            workspace_roots: BTreeSet::new(),
            ..Snapshot::default()
        };
        assert!(plan(&empty_roots).refused.is_some());
    }
}
