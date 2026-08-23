use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    model::{Label, TreeId},
    resource::ResourceRecord,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpVerb {
    New,
    Sync,
    Remove,
    Register,
    Adopt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub verb: OpVerb,
    pub pid: u32,
    pub started: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatePhase {
    Initialising,
    Bootstrapping,
    Ready,
    Failed,
    Removing,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncState {
    pub at: String,
    pub ok: bool,
    pub inputs: BTreeMap<String, String>,
    pub log: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifyState {
    pub at: String,
    pub ok: bool,
    pub log: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Materialized {
    pub path: String,
    pub kind: MaterializedKind,
    pub hash: Option<String>,
    pub tracked_checked_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializedKind {
    Rendered,
    Copied,
    Seeded,
}

/// The persisted state is keyed by `(label, name)` in its path and repeats that
/// address in the payload so a misplaced file is detectable (SPEC §4.3, §11.1).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TreeState {
    pub schema: u8,
    pub tree_id: TreeId,
    pub label: Label,
    pub name: String,
    pub phase: StatePhase,
    pub op: Option<Operation>,
    pub verify_pending: bool,
    pub sync: Option<SyncState>,
    pub verify: Option<VerifyState>,
    pub resources: BTreeMap<String, ResourceRecord>,
    pub materialized: Vec<Materialized>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepoState {
    pub schema: u8,
    pub label: Label,
    pub resources: BTreeMap<String, ResourceRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateView {
    pub phase: StatePhase,
    pub op: Option<OpVerb>,
    pub verify_pending: bool,
    pub has_records: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeObs {
    pub entry: bool,
    pub dir_exists: bool,
    pub identity_ok: bool,
    pub state: Option<StateView>,
    pub lock_held: bool,
    pub git_knows: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivedPhase {
    Ready,
    Verifying,
    Initialising,
    InitInterrupted,
    Bootstrapping,
    Interrupted,
    Failed,
    Removing,
    RemoveInterrupted,
    Creating,
    Claimed,
    Incomplete,
    Replaced,
    Missing,
    Unmanaged,
    StaleGit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseDecision {
    pub phase: DerivedPhase,
    pub finding: Option<&'static str>,
}

/// Applies the reduced tree-phase table in SPEC §11.1.
pub fn derive_phase(obs: &TreeObs) -> PhaseDecision {
    let result = |phase, finding| PhaseDecision { phase, finding };
    if !obs.entry {
        return if obs.git_knows {
            result(DerivedPhase::StaleGit, Some("STALE_GIT_WORKTREE"))
        } else {
            result(DerivedPhase::Unmanaged, Some("UNMANAGED_WORKTREE"))
        };
    }
    if obs.dir_exists
        && matches!(
            obs.state.as_ref(),
            Some(StateView {
                phase: StatePhase::Bootstrapping,
                op: Some(OpVerb::New),
                ..
            })
        )
    {
        return if obs.lock_held {
            result(DerivedPhase::Bootstrapping, None)
        } else {
            result(DerivedPhase::Interrupted, Some("TREE_INTERRUPTED"))
        };
    }
    if obs.dir_exists && !obs.identity_ok {
        return result(DerivedPhase::Replaced, Some("TREE_REPLACED"));
    }
    if !obs.dir_exists {
        return match &obs.state {
            Some(StateView {
                phase: StatePhase::Bootstrapping,
                op: Some(OpVerb::New),
                ..
            }) if obs.lock_held => result(DerivedPhase::Creating, None),
            Some(StateView {
                phase: StatePhase::Bootstrapping,
                op: Some(OpVerb::New),
                ..
            }) => result(DerivedPhase::Claimed, Some("TREE_CLAIMED")),
            _ => result(DerivedPhase::Missing, Some("TREE_MISSING")),
        };
    }
    let Some(state) = &obs.state else {
        return result(DerivedPhase::Incomplete, Some("TREE_INCOMPLETE"));
    };
    match state.phase {
        StatePhase::Ready if !state.verify_pending => result(DerivedPhase::Ready, None),
        // Ready trees never consult lock liveness. A shared door lock must not
        // make a pending verification appear to be actively running.
        StatePhase::Ready => result(DerivedPhase::Ready, Some("VERIFY_PENDING")),
        StatePhase::Initialising if obs.lock_held => result(DerivedPhase::Initialising, None),
        StatePhase::Initialising => result(DerivedPhase::InitInterrupted, Some("INIT_INTERRUPTED")),
        StatePhase::Bootstrapping if obs.lock_held => result(DerivedPhase::Bootstrapping, None),
        StatePhase::Bootstrapping => result(DerivedPhase::Interrupted, Some("TREE_INTERRUPTED")),
        StatePhase::Failed => result(DerivedPhase::Failed, None),
        StatePhase::Removing if obs.lock_held => result(DerivedPhase::Removing, None),
        StatePhase::Removing => result(DerivedPhase::RemoveInterrupted, Some("REMOVE_INTERRUPTED")),
    }
}

pub mod init {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Kind {
        Register,
        Adopt,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Ownership {
        Free,
        SameLabel,
        OtherLabel,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum EntryView {
        Absent,
        Initialising { lock_held: bool },
        Ready { identical_arguments: bool },
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Request {
        pub kind: Kind,
        pub entry: EntryView,
        pub path: Ownership,
        pub gitdir: Ownership,
        pub label_available: bool,
        pub adopt_is_worktree: bool,
        pub move_to: bool,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Decision {
        Start,
        Resume,
        AlreadyReady,
        Busy,
        Refuse(&'static str),
    }

    pub const fn decide(request: Request) -> Decision {
        if matches!(request.kind, Kind::Adopt) && !request.adopt_is_worktree {
            return Decision::Refuse("NOT_A_WORKTREE");
        }
        if matches!(request.path, Ownership::OtherLabel)
            || (!request.label_available && !request.move_to)
        {
            return Decision::Refuse("PATH_REGISTERED");
        }
        if matches!(request.gitdir, Ownership::OtherLabel) {
            return Decision::Refuse("GITDIR_REGISTERED");
        }
        match request.entry {
            EntryView::Absent => Decision::Start,
            EntryView::Initialising { lock_held: true } => Decision::Busy,
            EntryView::Initialising { lock_held: false } => Decision::Resume,
            EntryView::Ready {
                identical_arguments: true,
            } if !request.move_to => Decision::AlreadyReady,
            EntryView::Ready { .. } if request.move_to => Decision::Start,
            EntryView::Ready { .. } => Decision::Refuse("PATH_REGISTERED"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(state: Option<StatePhase>) -> TreeObs {
        TreeObs {
            entry: true,
            dir_exists: true,
            identity_ok: true,
            state: state.map(|phase| StateView {
                phase,
                op: None,
                verify_pending: false,
                has_records: false,
            }),
            lock_held: false,
            git_knows: true,
        }
    }

    #[test]
    fn derives_every_cell_of_the_reduced_phase_table() {
        let cases = [
            (obs(Some(StatePhase::Ready)), DerivedPhase::Ready),
            (
                obs(Some(StatePhase::Initialising)),
                DerivedPhase::InitInterrupted,
            ),
            (
                obs(Some(StatePhase::Bootstrapping)),
                DerivedPhase::Interrupted,
            ),
            (obs(Some(StatePhase::Failed)), DerivedPhase::Failed),
            (
                obs(Some(StatePhase::Removing)),
                DerivedPhase::RemoveInterrupted,
            ),
            (obs(None), DerivedPhase::Incomplete),
        ];
        for (observation, expected) in cases {
            assert_eq!(derive_phase(&observation).phase, expected);
        }

        for (phase, held, expected) in [
            (StatePhase::Initialising, true, DerivedPhase::Initialising),
            (StatePhase::Bootstrapping, true, DerivedPhase::Bootstrapping),
            (StatePhase::Removing, true, DerivedPhase::Removing),
        ] {
            let mut observation = obs(Some(phase));
            observation.lock_held = held;
            assert_eq!(derive_phase(&observation).phase, expected);
        }

        let mut verifying = obs(Some(StatePhase::Ready));
        verifying.state.as_mut().unwrap().verify_pending = true;
        verifying.lock_held = true;
        assert_eq!(derive_phase(&verifying).phase, DerivedPhase::Ready);
        assert_eq!(derive_phase(&verifying).finding, Some("VERIFY_PENDING"));
        verifying.lock_held = false;
        assert_eq!(derive_phase(&verifying).phase, DerivedPhase::Ready);
        assert_eq!(derive_phase(&verifying).finding, Some("VERIFY_PENDING"));

        let mut creating = obs(Some(StatePhase::Bootstrapping));
        creating.dir_exists = false;
        creating.state.as_mut().unwrap().op = Some(OpVerb::New);
        creating.lock_held = true;
        assert_eq!(derive_phase(&creating).phase, DerivedPhase::Creating);
        creating.lock_held = false;
        assert_eq!(derive_phase(&creating).phase, DerivedPhase::Claimed);

        let mut missing = obs(Some(StatePhase::Ready));
        missing.dir_exists = false;
        missing.state.as_mut().unwrap().has_records = true;
        assert_eq!(derive_phase(&missing).phase, DerivedPhase::Missing);

        let mut replaced = obs(Some(StatePhase::Ready));
        replaced.identity_ok = false;
        assert_eq!(derive_phase(&replaced).phase, DerivedPhase::Replaced);

        let mut unmanaged = obs(None);
        unmanaged.entry = false;
        unmanaged.git_knows = false;
        assert_eq!(derive_phase(&unmanaged).phase, DerivedPhase::Unmanaged);
        unmanaged.git_knows = true;
        assert_eq!(derive_phase(&unmanaged).phase, DerivedPhase::StaleGit);
    }

    #[test]
    fn init_resumes_only_an_unlocked_initialisation() {
        use init::{Decision, EntryView, Kind, Ownership, Request};
        let base = Request {
            kind: Kind::Register,
            entry: EntryView::Absent,
            path: Ownership::Free,
            gitdir: Ownership::Free,
            label_available: true,
            adopt_is_worktree: true,
            move_to: false,
        };
        assert_eq!(init::decide(base), Decision::Start);
        assert_eq!(
            init::decide(Request {
                entry: EntryView::Initialising { lock_held: false },
                ..base
            }),
            Decision::Resume
        );
        assert_eq!(
            init::decide(Request {
                entry: EntryView::Initialising { lock_held: true },
                ..base
            }),
            Decision::Busy
        );

        assert_eq!(
            init::decide(Request {
                kind: Kind::Adopt,
                adopt_is_worktree: false,
                ..base
            }),
            Decision::Refuse("NOT_A_WORKTREE")
        );
        assert_eq!(
            init::decide(Request {
                path: Ownership::OtherLabel,
                ..base
            }),
            Decision::Refuse("PATH_REGISTERED")
        );
        assert_eq!(
            init::decide(Request {
                gitdir: Ownership::OtherLabel,
                ..base
            }),
            Decision::Refuse("GITDIR_REGISTERED")
        );
        assert_eq!(
            init::decide(Request {
                entry: EntryView::Ready {
                    identical_arguments: true,
                },
                ..base
            }),
            Decision::AlreadyReady
        );
        assert_eq!(
            init::decide(Request {
                entry: EntryView::Ready {
                    identical_arguments: false,
                },
                ..base
            }),
            Decision::Refuse("PATH_REGISTERED")
        );
        assert_eq!(
            init::decide(Request {
                entry: EntryView::Ready {
                    identical_arguments: false,
                },
                move_to: true,
                ..base
            }),
            Decision::Start
        );
    }
}
