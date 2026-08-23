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
    pub keep_orphans: bool,
    pub allow_canonical: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoveOptions {
    pub force: bool,
    pub keep_orphans: bool,
}

pub fn plan(obs: &Obs, force: bool) -> Result<RemovePlan, CoreError> {
    plan_with_options(
        obs,
        RemoveOptions {
            force,
            keep_orphans: false,
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
            keep_orphans: false,
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
    if (classification.dirty || classification.unpushed) && !options.force {
        return Err(CoreError::new(
            ExitClass::State,
            "TREE_DIRTY",
            "tree is dirty or has unpushed commits",
            "commit or push the work, or retry with `--force`",
        ));
    }
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
        keep_orphans: options.keep_orphans,
        allow_canonical,
    })
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
    if plan.keep_orphans {
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

pub fn revalidate(
    prior: &RemovePlan,
    observed: &Obs,
    force: bool,
) -> Result<Revalidation, CoreError> {
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
    let current = plan_for(
        observed,
        RemoveOptions {
            force,
            keep_orphans: prior.keep_orphans,
        },
        prior.allow_canonical,
    )?;
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
            session_live: false,
            door_holders: Vec::new(),
            resources: Vec::new(),
        }
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
            revalidate(&prior, &replaced, false).unwrap_err().code.0,
            "TREE_REPLACED"
        );

        let replaced_plan = plan(&replaced, false).unwrap();
        assert_eq!(
            revalidate(&replaced_plan, &replaced, false).unwrap(),
            Revalidation::Valid
        );

        let mut with_session = owned;
        with_session.session_live = true;
        let prior = plan(&with_session, false).unwrap();
        with_session.session_live = false;
        assert_eq!(
            revalidate(&prior, &with_session, false).unwrap(),
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
                force: false,
                keep_orphans: true,
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
            revalidate(&canonical, &observation, true).unwrap(),
            Revalidation::Valid
        );
    }
}
