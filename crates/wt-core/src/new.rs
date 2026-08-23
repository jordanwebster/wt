use serde::{Deserialize, Serialize};

use crate::lifecycle::DerivedPhase;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryView {
    pub phase: DerivedPhase,
    pub source_identical: bool,
    pub verify_pending: bool,
    pub has_resource_records: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    pub verify: bool,
    pub name_shadows_label: bool,
    pub path_occupied: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartAt {
    Git,
    State,
    Bootstrap,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    Allocate {
        start: StartAt,
    },
    AlreadyReady,
    Verify {
        resumed: bool,
    },
    Resume {
        start: StartAt,
    },
    FreshIncarnation {
        start: StartAt,
    },
    Refuse {
        code: &'static str,
        remedy: &'static str,
    },
}

pub fn decide(entry: Option<&EntryView>, request: &Request) -> Decision {
    if request.name_shadows_label {
        return Decision::Refuse {
            code: "NAME_SHADOWS_LABEL",
            remedy: "choose a tree name that is not a registered label",
        };
    }
    if request.path_occupied {
        return Decision::Refuse {
            code: "PATH_OCCUPIED",
            remedy: "move or delete the non-wt object at the worktree path",
        };
    }
    let Some(entry) = entry else {
        return Decision::Allocate {
            start: StartAt::Git,
        };
    };
    match entry.phase {
        DerivedPhase::Replaced => {
            return Decision::Refuse {
                code: "TREE_REPLACED",
                remedy: "run `wt prune --records`, `wt remove`, or `wt adopt` as appropriate",
            };
        }
        DerivedPhase::InitInterrupted | DerivedPhase::Initialising => {
            return Decision::Refuse {
                code: "INIT_INTERRUPTED",
                remedy: "re-run `wt register` or `wt adopt` for this path",
            };
        }
        DerivedPhase::Verifying | DerivedPhase::Removing | DerivedPhase::RemoveInterrupted => {
            return Decision::Refuse {
                code: "TREE_BUSY",
                remedy: "wait for or resume the active lifecycle operation",
            };
        }
        _ => {}
    }
    if !entry.source_identical {
        return Decision::Refuse {
            code: "NAME_TAKEN",
            remedy: "choose another name or remove the existing tree",
        };
    }
    match entry.phase {
        DerivedPhase::Ready if entry.verify_pending => Decision::Verify { resumed: true },
        DerivedPhase::Ready if request.verify => Decision::Verify { resumed: false },
        DerivedPhase::Ready => Decision::AlreadyReady,
        DerivedPhase::Missing if entry.has_resource_records => Decision::Refuse {
            code: "TREE_MISSING_PENDING",
            remedy: "run `wt prune --records <target>` then `wt new`",
        },
        DerivedPhase::Missing => Decision::FreshIncarnation {
            start: StartAt::Git,
        },
        DerivedPhase::Claimed | DerivedPhase::Creating => Decision::Resume {
            start: StartAt::Git,
        },
        DerivedPhase::Incomplete => Decision::Resume {
            start: StartAt::State,
        },
        DerivedPhase::Bootstrapping | DerivedPhase::Interrupted | DerivedPhase::Failed => {
            Decision::Resume {
                start: StartAt::Bootstrap,
            }
        }
        DerivedPhase::Replaced
        | DerivedPhase::InitInterrupted
        | DerivedPhase::Initialising
        | DerivedPhase::Verifying
        | DerivedPhase::Removing
        | DerivedPhase::RemoveInterrupted => unreachable!("handled above"),
        DerivedPhase::Unmanaged | DerivedPhase::StaleGit => Decision::Refuse {
            code: "NAME_TAKEN",
            remedy: "finish or remove the existing tree",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            verify: false,
            name_shadows_label: false,
            path_occupied: false,
        }
    }

    fn entry(phase: DerivedPhase) -> EntryView {
        EntryView {
            phase,
            source_identical: true,
            verify_pending: false,
            has_resource_records: false,
        }
    }

    #[test]
    fn revised_decision_table_covers_ready_missing_and_resume() {
        assert_eq!(
            decide(None, &request()),
            Decision::Allocate {
                start: StartAt::Git
            }
        );
        assert_eq!(
            decide(Some(&entry(DerivedPhase::Ready)), &request()),
            Decision::AlreadyReady
        );

        let mut missing = entry(DerivedPhase::Missing);
        missing.has_resource_records = true;
        assert!(matches!(
            decide(Some(&missing), &request()),
            Decision::Refuse {
                code: "TREE_MISSING_PENDING",
                ..
            }
        ));
        missing.has_resource_records = false;
        assert_eq!(
            decide(Some(&missing), &request()),
            Decision::FreshIncarnation {
                start: StartAt::Git
            }
        );
        assert_eq!(
            decide(Some(&entry(DerivedPhase::Interrupted)), &request()),
            Decision::Resume {
                start: StartAt::Bootstrap
            }
        );
    }

    #[test]
    fn revised_conflicts_include_path_occupied() {
        for (request, code) in [
            (
                Request {
                    name_shadows_label: true,
                    ..request()
                },
                "NAME_SHADOWS_LABEL",
            ),
            (
                Request {
                    path_occupied: true,
                    ..request()
                },
                "PATH_OCCUPIED",
            ),
        ] {
            assert!(matches!(
                decide(None, &request),
                Decision::Refuse {
                    code: actual,
                    ..
                } if actual == code
            ));
        }
    }

    #[test]
    fn decision_table_covers_verify_conflicts_and_every_resume_point() {
        let mut verify_request = request();
        verify_request.verify = true;
        assert_eq!(
            decide(Some(&entry(DerivedPhase::Ready)), &verify_request),
            Decision::Verify { resumed: false }
        );
        let mut pending = entry(DerivedPhase::Ready);
        pending.verify_pending = true;
        assert_eq!(
            decide(Some(&pending), &request()),
            Decision::Verify { resumed: true }
        );

        let mut different = entry(DerivedPhase::Ready);
        different.source_identical = false;
        assert!(matches!(
            decide(Some(&different), &request()),
            Decision::Refuse {
                code: "NAME_TAKEN",
                ..
            }
        ));

        for (phase, start) in [
            (DerivedPhase::Claimed, StartAt::Git),
            (DerivedPhase::Creating, StartAt::Git),
            (DerivedPhase::Incomplete, StartAt::State),
            (DerivedPhase::Bootstrapping, StartAt::Bootstrap),
            (DerivedPhase::Interrupted, StartAt::Bootstrap),
            (DerivedPhase::Failed, StartAt::Bootstrap),
        ] {
            assert_eq!(
                decide(Some(&entry(phase)), &request()),
                Decision::Resume { start },
                "{phase:?}"
            );
        }
        for (phase, code) in [
            (DerivedPhase::Replaced, "TREE_REPLACED"),
            (DerivedPhase::Initialising, "INIT_INTERRUPTED"),
            (DerivedPhase::InitInterrupted, "INIT_INTERRUPTED"),
            (DerivedPhase::Verifying, "TREE_BUSY"),
            (DerivedPhase::Removing, "TREE_BUSY"),
            (DerivedPhase::RemoveInterrupted, "TREE_BUSY"),
        ] {
            assert!(matches!(
                decide(Some(&entry(phase)), &request()),
                Decision::Refuse { code: actual, .. } if actual == code
            ));
        }
    }
}
