//! Which cargo units a build directory can lose (SPEC §11.9).
//!
//! Cargo keeps one artifact set per build configuration and never deletes a
//! superseded one, and it writes no "last used" mark on a unit it reuses, so
//! neither the directory nor file ages can say what is still read. The
//! decision here instead compares each unit with what the workspace
//! currently resolves to — the crate roots `cargo metadata` lists — and, for
//! units that are the same thing built twice, keeps only the most recently
//! compiled one. Build-script runs, which carry no dep-info, live while a
//! live unit's recorded dependencies name them.

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
    /// and config. Two units with equal identity, kind, crate root and
    /// output are the same thing built twice.
    pub identity: String,
    pub output: OutputKind,
    /// Modification time of `invoked.timestamp`, seconds since the epoch.
    pub compiled_at: u64,
}

/// One `incremental/<crate>-<hash>` directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalObs {
    pub name: String,
    /// Modification time, seconds since the epoch. rustc rewrites the
    /// directory on every use, so this is a use record.
    pub modified_at: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub units: Vec<UnitObs>,
    /// Every target source path of every package in the current resolve.
    pub crate_roots: BTreeSet<String>,
    pub incremental: Vec<IncrementalObs>,
    pub now: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    /// Hashes of the units to delete, in observation order.
    pub dead_units: Vec<String>,
    pub dead_incremental: Vec<String>,
    pub live_units: usize,
    /// Set when nothing may be deleted: no unit matched the workspace at
    /// all — a path mismatch, a resolve with no roots, or a directory whose
    /// every unit lacks readable dep-info — which means the observation is
    /// not about this workspace rather than that everything is stale.
    pub refused: Option<&'static str>,
}

/// How long an incremental session directory may go unused.
pub const INCREMENTAL_MAX_AGE_SECS: u64 = 14 * 24 * 60 * 60;

pub fn plan(snapshot: &Snapshot) -> Plan {
    let mut plan = Plan::default();
    if !snapshot.units.is_empty() && snapshot.crate_roots.is_empty() {
        plan.refused = Some("the workspace resolves to no crate roots");
        return plan;
    }
    // Units compiled from a crate root the workspace still has.
    let identified = snapshot
        .units
        .iter()
        .enumerate()
        .filter(|(_, unit)| {
            unit.crate_root
                .as_ref()
                .is_some_and(|root| snapshot.crate_roots.contains(root))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if !snapshot.units.is_empty() && identified.is_empty() {
        plan.refused = Some("no unit was compiled from a crate root of this workspace");
        return plan;
    }
    // Among units that are the same thing built twice, the newest wins.
    let mut newest = BTreeMap::<(String, String, String, OutputKind), u64>::new();
    for &index in &identified {
        let unit = &snapshot.units[index];
        let key = slot_key(unit);
        let entry = newest.entry(key).or_insert(0);
        *entry = (*entry).max(unit.compiled_at);
    }
    let mut live = vec![false; snapshot.units.len()];
    for &index in &identified {
        let unit = &snapshot.units[index];
        if newest[&slot_key(unit)] == unit.compiled_at {
            live[index] = true;
        }
    }
    // Units without dep-info live while a live unit names them.
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
    let mut queue = live
        .iter()
        .enumerate()
        .filter(|(_, live)| **live)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    while let Some(index) = queue.pop() {
        for dep in &snapshot.units[index].deps {
            for &target in by_fingerprint.get(dep).map_or(&[][..], Vec::as_slice) {
                if !live[target] && snapshot.units[target].crate_root.is_none() {
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
    plan.dead_incremental = snapshot
        .incremental
        .iter()
        .filter(|dir| snapshot.now.saturating_sub(dir.modified_at) > INCREMENTAL_MAX_AGE_SECS)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(hash: &str, kind: &str, root: Option<&str>, identity: &str, at: u64) -> UnitObs {
        UnitObs {
            hash: hash.to_owned(),
            kind: kind.to_owned(),
            fingerprint: Some(hash.bytes().map(u64::from).sum()),
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

    #[test]
    fn units_of_crate_roots_the_workspace_lost_die() {
        let snapshot = Snapshot {
            units: vec![
                unit(
                    "aaaa",
                    "lib-serde",
                    Some("/reg/serde-1.0.2/src/lib.rs"),
                    "x",
                    10,
                ),
                unit(
                    "bbbb",
                    "lib-serde",
                    Some("/reg/serde-1.0.1/src/lib.rs"),
                    "x",
                    5,
                ),
                unit("cccc", "test-test-gone", Some("/ws/tests/gone.rs"), "x", 5),
            ],
            crate_roots: roots(&["/reg/serde-1.0.2/src/lib.rs", "/ws/src/lib.rs"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert_eq!(plan.dead_units, vec!["bbbb", "cccc"]);
        assert_eq!(plan.live_units, 1);
        assert!(plan.refused.is_none());
    }

    #[test]
    fn the_newest_of_identical_units_wins_and_output_kind_separates_them() {
        let mut check = unit("cc01", "lib-wt", Some("/ws/src/lib.rs"), "x", 3);
        check.output = OutputKind::Metadata;
        let snapshot = Snapshot {
            units: vec![
                unit("old1", "lib-wt", Some("/ws/src/lib.rs"), "x", 5),
                unit("new1", "lib-wt", Some("/ws/src/lib.rs"), "x", 9),
                unit("feat", "lib-wt", Some("/ws/src/lib.rs"), "features=[a]", 1),
                check,
            ],
            crate_roots: roots(&["/ws/src/lib.rs"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert_eq!(plan.dead_units, vec!["old1"]);
        assert_eq!(plan.live_units, 3);
    }

    #[test]
    fn build_script_runs_live_while_a_live_unit_names_them() {
        let mut lib = unit(
            "l111",
            "lib-blake3",
            Some("/reg/blake3-1.8.7/src/lib.rs"),
            "x",
            9,
        );
        let mut run = unit("r111", "run-build-script-build-script-build", None, "x", 8);
        run.fingerprint = Some(77);
        let mut orphan_run = unit("r222", "run-build-script-build-script-build", None, "x", 8);
        orphan_run.fingerprint = Some(78);
        let mut stale_lib = unit(
            "l000",
            "lib-blake3",
            Some("/reg/blake3-1.8.6/src/lib.rs"),
            "x",
            2,
        );
        let mut stale_run = unit("r000", "run-build-script-build-script-build", None, "x", 2);
        stale_run.fingerprint = Some(79);
        stale_lib.deps = vec![79];
        lib.deps = vec![77];
        let snapshot = Snapshot {
            units: vec![lib, run, orphan_run, stale_lib, stale_run],
            crate_roots: roots(&["/reg/blake3-1.8.7/src/lib.rs"]),
            ..Snapshot::default()
        };
        let plan = plan(&snapshot);
        assert_eq!(plan.dead_units, vec!["r222", "l000", "r000"]);
        assert_eq!(plan.live_units, 2);
    }

    #[test]
    fn nothing_matching_the_workspace_refuses_rather_than_deleting_everything() {
        let snapshot = Snapshot {
            units: vec![unit("aaaa", "lib-x", Some("/elsewhere/src/lib.rs"), "x", 1)],
            crate_roots: roots(&["/ws/src/lib.rs"]),
            ..Snapshot::default()
        };
        let decision = plan(&snapshot);
        assert!(decision.refused.is_some());
        assert!(decision.dead_units.is_empty());
        let empty_roots = Snapshot {
            units: vec![unit("aaaa", "lib-x", Some("/ws/src/lib.rs"), "x", 1)],
            crate_roots: BTreeSet::new(),
            ..Snapshot::default()
        };
        assert!(plan(&empty_roots).refused.is_some());
    }

    #[test]
    fn incremental_directories_die_by_age_alone() {
        let now = 100 * 24 * 60 * 60;
        let snapshot = Snapshot {
            units: Vec::new(),
            crate_roots: BTreeSet::new(),
            incremental: vec![
                IncrementalObs {
                    name: "wt-abc".to_owned(),
                    modified_at: now - INCREMENTAL_MAX_AGE_SECS - 1,
                },
                IncrementalObs {
                    name: "wt-def".to_owned(),
                    modified_at: now - 60,
                },
            ],
            now,
        };
        let plan = plan(&snapshot);
        assert_eq!(plan.dead_incremental, vec!["wt-abc"]);
        assert!(plan.refused.is_none());
    }
}
