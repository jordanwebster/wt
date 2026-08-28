use serde::{Deserialize, Serialize};

use crate::{CoreError, ExitClass};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitObs {
    pub dirty_porcelain: String,
    pub upstream: Upstream,
    pub ahead: u32,
    pub remote_contains_head: bool,
    pub detached: bool,
    pub merged: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Upstream {
    Present,
    Gone,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    pub dirty: bool,
    pub unpushed: bool,
    pub merged: bool,
}

pub fn classify(obs: &GitObs) -> Classification {
    let unpushed = if obs.detached {
        !obs.remote_contains_head
    } else {
        match obs.upstream {
            Upstream::Present => obs.ahead > 0,
            Upstream::Gone => true,
            Upstream::None => !obs.remote_contains_head,
        }
    };
    Classification {
        dirty: !obs.dirty_porcelain.is_empty(),
        unpushed,
        merged: obs.merged,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Obs {
    pub target: String,
    pub canonical: bool,
    pub dir_exists: bool,
    pub identity_ok: bool,
    pub git: Option<GitObs>,
    pub branch: Option<String>,
    /// The tree came from `wt adopt`, so its branch predates wt.
    pub adopted: bool,
    pub session_live: bool,
    pub door_holders: Vec<DoorHolder>,
    pub resources: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DoorHolder {
    pub pid: u32,
    pub verb: String,
    pub since: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemovePlan {
    pub target: String,
    pub dir_exists: bool,
    pub identity_ok: bool,
    pub dirty: bool,
    pub unpushed: bool,
    pub merged: bool,
    pub session_live: bool,
    pub door_holders: Vec<DoorHolder>,
    pub resources: Vec<String>,
    pub branch: Option<String>,
    /// The resolved branch decision, not the flag that asked for it.
    pub delete_branch: bool,
    /// The plan destroys work that nothing can recover once it finishes.
    pub consent_required: bool,
    pub options: RemoveOptions,
    pub allow_canonical: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveOptions {
    pub force: bool,
    pub keep_orphans: bool,
    pub delete_branch: bool,
    pub keep_branch: bool,
}

pub fn plan(obs: &Obs, force: bool) -> Result<RemovePlan, CoreError> {
    plan_with_options(
        obs,
        RemoveOptions {
            force,
            ..RemoveOptions::default()
        },
    )
}

pub fn plan_with_options(obs: &Obs, options: RemoveOptions) -> Result<RemovePlan, CoreError> {
    plan_for(obs, options, false)
}

/// Canonical teardown differs only at the command boundary; all safety
/// observations and consent data remain identical (SPEC §11.5).
pub fn plan_unregister(obs: &Obs, force: bool) -> Result<RemovePlan, CoreError> {
    plan_for(
        obs,
        RemoveOptions {
            force,
            ..RemoveOptions::default()
        },
        true,
    )
}

fn plan_for(
    obs: &Obs,
    options: RemoveOptions,
    allow_canonical: bool,
) -> Result<RemovePlan, CoreError> {
    if obs.canonical && !allow_canonical {
        return Err(CoreError::new(
            ExitClass::Usage,
            "USE_UNREGISTER",
            "canonical checkouts are unregistered, not removed",
            "run `wt unregister <label>`",
        ));
    }
    let classification = obs.git.as_ref().map(classify).unwrap_or(Classification {
        dirty: false,
        unpushed: false,
        merged: false,
    });
    let delete_branch = obs.branch.is_some() && !options.keep_branch && {
        // A branch whose commits are on a remote is a name that `origin` can
        // restore; one that was never observed, carries unpushed commits, or
        // predates wt is kept unless the flag names it explicitly.
        options.delete_branch || (obs.git.is_some() && !classification.unpushed && !obs.adopted)
    };
    // Uncommitted changes die with the directory, and commits die with a branch
    // that no remote carries; everything else the removal touches can be made
    // again from the declarations or from `origin` (A54).
    let loses_work = classification.dirty || (classification.unpushed && delete_branch);
    Ok(RemovePlan {
        target: obs.target.clone(),
        dir_exists: obs.dir_exists,
        identity_ok: obs.identity_ok,
        dirty: classification.dirty,
        unpushed: classification.unpushed,
        merged: classification.merged,
        session_live: obs.session_live,
        door_holders: obs.door_holders.clone(),
        resources: obs.resources.clone(),
        branch: obs.branch.clone(),
        delete_branch,
        consent_required: loses_work && !options.force,
        options,
        allow_canonical,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Consent {
    NotNeeded,
    Prompt,
}

/// `--force` is the answer a caller gives in advance; without one, a removal
/// that loses work needs a terminal to ask, and is refused otherwise (A54).
pub fn gate(plan: &RemovePlan, can_prompt: bool) -> Result<Consent, CoreError> {
    if !plan.consent_required {
        return Ok(Consent::NotNeeded);
    }
    if !can_prompt {
        return Err(work_loss_refusal(plan));
    }
    Ok(Consent::Prompt)
}

fn work_loss_refusal(plan: &RemovePlan) -> CoreError {
    let message = if plan.dirty {
        "tree has uncommitted changes"
    } else {
        "branch has unpushed commits and would be deleted"
    };
    CoreError::new(
        ExitClass::State,
        "TREE_DIRTY",
        message,
        "commit or push the work, or retry with `--force`",
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AfterDestroy {
    ContinueToRemoval,
    KeepLiveEntry,
}

pub fn after_destroy(
    plan: &RemovePlan,
    records_remaining: bool,
) -> Result<AfterDestroy, CoreError> {
    if !records_remaining {
        return Ok(AfterDestroy::ContinueToRemoval);
    }
    if plan.options.keep_orphans {
        return Ok(AfterDestroy::KeepLiveEntry);
    }
    Err(CoreError::new(
        ExitClass::ChildFailed,
        "DESTROY_FAILED",
        "one or more resource records remain after teardown",
        "fix the resource and retry, pass `--keep-orphans`, or run `wt prune --records`",
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Revalidation {
    Valid,
    Changed(RemovePlan),
}

pub fn revalidate(prior: &RemovePlan, observed: &Obs) -> Result<Revalidation, CoreError> {
    // A path already known to be replaced follows the records-only path. A
    // change from owned to replaced after consent is the hostile-rename race
    // and must stop before any resource effect (SPEC §11.4 step 6).
    if prior.identity_ok && observed.dir_exists && !observed.identity_ok {
        return Err(CoreError::new(
            ExitClass::State,
            "TREE_REPLACED",
            "tree identity changed after consent",
            "do not remove the replacement; use `wt prune --records`",
        ));
    }
    let current = plan_for(observed, prior.options, prior.allow_canonical)?;
    // Consent covers the plan it was given. Work that appeared since is work
    // nobody agreed to lose, whether or not a prompt was shown.
    if current.consent_required && !prior.consent_required {
        return Err(work_loss_refusal(&current));
    }
    if current.target == prior.target
        && current.dir_exists == prior.dir_exists
        && current.identity_ok == prior.identity_ok
        && current.dirty == prior.dirty
        && current.unpushed == prior.unpushed
    {
        Ok(Revalidation::Valid)
    } else {
        Ok(Revalidation::Changed(current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> Obs {
        Obs {
            target: "repo/work".to_owned(),
            canonical: false,
            dir_exists: true,
            identity_ok: true,
            git: Some(GitObs {
                dirty_porcelain: String::new(),
                upstream: Upstream::Present,
                ahead: 0,
                remote_contains_head: true,
                detached: false,
                merged: false,
            }),
            branch: Some("work".to_owned()),
            adopted: false,
            session_live: false,
            door_holders: Vec::new(),
            resources: Vec::new(),
        }
    }

    fn dirty() -> Obs {
        let mut observation = clean();
        observation.git.as_mut().unwrap().dirty_porcelain = " M src/main.rs".to_owned();
        observation
    }

    #[test]
    fn classification_follows_every_upstream_rule() {
        let mut observation = clean().git.unwrap();
        observation.ahead = 1;
        assert!(classify(&observation).unpushed);
        observation.upstream = Upstream::Gone;
        observation.ahead = 0;
        assert!(classify(&observation).unpushed);
        observation.upstream = Upstream::None;
        observation.remote_contains_head = true;
        assert!(!classify(&observation).unpushed);
        observation.remote_contains_head = false;
        assert!(classify(&observation).unpushed);
        observation.detached = true;
        observation.upstream = Upstream::Present;
        observation.ahead = 0;
        assert!(classify(&observation).unpushed);
        observation.remote_contains_head = true;
        assert!(!classify(&observation).unpushed);
    }

    #[test]
    fn replaced_at_observation_is_plannable_but_replacement_after_consent_stops() {
        let owned = clean();
        let prior = plan(&owned, false).unwrap();
        let mut replaced = owned.clone();
        replaced.identity_ok = false;
        assert_eq!(
            revalidate(&prior, &replaced).unwrap_err().code.0,
            "TREE_REPLACED"
        );

        let replaced_plan = plan(&replaced, false).unwrap();
        assert_eq!(
            revalidate(&replaced_plan, &replaced).unwrap(),
            Revalidation::Valid
        );

        let mut with_session = owned;
        with_session.session_live = true;
        let prior = plan(&with_session, false).unwrap();
        with_session.session_live = false;
        assert_eq!(
            revalidate(&prior, &with_session).unwrap(),
            Revalidation::Valid
        );
    }

    #[test]
    fn unregister_uses_the_same_plan_without_the_canonical_redirect() {
        let mut observation = clean();
        observation.canonical = true;
        assert_eq!(
            plan(&observation, true).unwrap_err().code.0,
            "USE_UNREGISTER"
        );
        assert!(plan_unregister(&observation, true).is_ok());

        let remove_plan = plan_with_options(
            &clean(),
            RemoveOptions {
                keep_orphans: true,
                ..RemoveOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            after_destroy(&remove_plan, true).unwrap(),
            AfterDestroy::KeepLiveEntry
        );
        assert_eq!(
            after_destroy(&remove_plan, false).unwrap(),
            AfterDestroy::ContinueToRemoval
        );
        let strict = plan(&clean(), false).unwrap();
        assert_eq!(
            after_destroy(&strict, true).unwrap_err().code.0,
            "DESTROY_FAILED"
        );

        let canonical = plan_unregister(&observation, true).unwrap();
        assert_eq!(
            revalidate(&canonical, &observation).unwrap(),
            Revalidation::Valid
        );
    }

    #[test]
    fn consent_is_asked_only_where_work_dies() {
        let clean_plan = plan(&clean(), false).unwrap();
        assert!(!clean_plan.consent_required);
        assert_eq!(gate(&clean_plan, false).unwrap(), Consent::NotNeeded);

        let dirty_plan = plan(&dirty(), false).unwrap();
        assert!(dirty_plan.consent_required);
        assert_eq!(gate(&dirty_plan, true).unwrap(), Consent::Prompt);
        assert_eq!(gate(&dirty_plan, false).unwrap_err().code.0, "TREE_DIRTY");

        // `--force` is consent given in advance, with or without a terminal.
        let forced = plan(&dirty(), true).unwrap();
        assert!(!forced.consent_required);
        assert_eq!(gate(&forced, false).unwrap(), Consent::NotNeeded);
    }

    #[test]
    fn a_branch_is_deleted_only_where_a_remote_can_restore_it() {
        assert!(plan(&clean(), false).unwrap().delete_branch);

        let mut unpushed = clean();
        unpushed.git.as_mut().unwrap().remote_contains_head = false;
        unpushed.git.as_mut().unwrap().upstream = Upstream::None;
        let kept = plan(&unpushed, false).unwrap();
        assert!(!kept.delete_branch);
        // The branch survives, so the commits do, and nothing needs consent.
        assert!(!kept.consent_required);

        let asked = plan_with_options(
            &unpushed,
            RemoveOptions {
                delete_branch: true,
                ..RemoveOptions::default()
            },
        )
        .unwrap();
        assert!(asked.delete_branch);
        assert!(asked.consent_required);

        let mut adopted = clean();
        adopted.adopted = true;
        assert!(!plan(&adopted, false).unwrap().delete_branch);

        let held = plan_with_options(
            &clean(),
            RemoveOptions {
                keep_branch: true,
                ..RemoveOptions::default()
            },
        )
        .unwrap();
        assert!(!held.delete_branch);

        // A directory that was never observed cannot be shown to be pushed.
        let mut missing = clean();
        missing.dir_exists = false;
        missing.git = None;
        assert!(!plan(&missing, false).unwrap().delete_branch);

        let mut detached = clean();
        detached.branch = None;
        assert!(!plan(&detached, false).unwrap().delete_branch);
    }

    #[test]
    fn work_that_appears_after_consent_is_not_covered_by_it() {
        let prior = plan(&clean(), false).unwrap();
        assert_eq!(
            revalidate(&prior, &dirty()).unwrap_err().code.0,
            "TREE_DIRTY"
        );

        // Consent already covers a dirty tree that is still dirty.
        let consented = plan(&dirty(), false).unwrap();
        assert_eq!(
            revalidate(&consented, &dirty()).unwrap(),
            Revalidation::Valid
        );
    }
}
