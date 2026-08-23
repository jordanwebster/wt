use crate::{CoreError, ExitClass};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefCandidates {
    pub local: Option<String>,
    pub local_oid: Option<String>,
    pub origin: Option<String>,
    pub origin_oid: Option<String>,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution {
    Local {
        reference: String,
        notice: Option<&'static str>,
    },
    Remote {
        reference: String,
    },
    Revision {
        reference: String,
    },
    PullRequest {
        number: u64,
    },
    PullRequestUrl {
        host: String,
        owner: String,
        repo: String,
        number: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddSpec {
    ExistingBranch(String),
    NewBranch {
        name: String,
        start: String,
        track: bool,
    },
    Detached {
        start: String,
    },
}

pub fn add_spec(
    branch: &str,
    start: &str,
    detach: bool,
    branch_exists: bool,
    branch_in_use: bool,
    start_is_remote: bool,
) -> Result<AddSpec, CoreError> {
    if detach {
        return Ok(AddSpec::Detached {
            start: start.to_owned(),
        });
    }
    if branch_in_use {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "BRANCH_IN_USE",
            format!("branch `{branch}` is checked out by another worktree"),
            "choose another branch or remove the worktree that holds it",
        ));
    }
    if branch_exists {
        Ok(AddSpec::ExistingBranch(branch.to_owned()))
    } else {
        Ok(AddSpec::NewBranch {
            name: branch.to_owned(),
            start: start.to_owned(),
            track: start_is_remote,
        })
    }
}

/// Returns the ordered forge refspec attempts for a pull request.
pub fn pr_refspec(host: &str, number: u64) -> Vec<(String, String)> {
    let sources = if host.contains("gitlab") {
        vec![format!("refs/merge-requests/{number}/head")]
    } else if host.contains("bitbucket") {
        vec![format!("refs/pull-requests/{number}/from")]
    } else if host.contains("github") {
        vec![format!("refs/pull/{number}/head")]
    } else {
        vec![
            format!("refs/pull/{number}/head"),
            format!("refs/merge-requests/{number}/head"),
        ]
    };
    let destination = format!("refs/wt/pr/{number}");
    sources
        .into_iter()
        .map(|source| (source, destination.clone()))
        .collect()
}

pub fn normalize_url(input: &str) -> Option<(String, String)> {
    if let Some(rest) = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
    {
        let (host, path) = rest.trim_end_matches('/').split_once('/')?;
        let mut parts = path.trim_end_matches(".git").split('/');
        return Some((
            host.to_owned(),
            format!("{}/{}", parts.next()?, parts.next()?),
        ));
    }
    let (_, host_and_path) = input.split_once('@')?;
    let (host, path) = host_and_path.split_once(':')?;
    let mut parts = path
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .split('/');
    Some((
        host.to_owned(),
        format!("{}/{}", parts.next()?, parts.next()?),
    ))
}

/// Chooses the meaning of `--from` after the effects layer has supplied the
/// candidate refs. A bare name deliberately gives a differing local branch
/// precedence over `origin/<name>` (SPEC §11.2).
pub fn decide(input: &str, candidates: &RefCandidates) -> Result<Resolution, CoreError> {
    if let Some(number) = input
        .strip_prefix("pr:")
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Ok(Resolution::PullRequest { number });
    }
    if let Some(url) = parse_pr_url(input) {
        return Ok(url);
    }
    if input.starts_with("origin/") {
        return candidates
            .origin
            .as_ref()
            .cloned()
            .map(|reference| Resolution::Remote { reference })
            .ok_or_else(|| not_found(input));
    }
    if let Some(reference) = &candidates.local {
        return Ok(Resolution::Local {
            reference: reference.clone(),
            notice: match (
                candidates.origin.as_ref(),
                candidates.local_oid.as_ref(),
                candidates.origin_oid.as_ref(),
            ) {
                (Some(_), Some(local), Some(origin)) if local != origin => {
                    Some("FROM_LOCAL_SHADOWS_REMOTE")
                }
                _ => None,
            },
        });
    }
    if let Some(reference) = &candidates.origin {
        return Ok(Resolution::Remote {
            reference: reference.clone(),
        });
    }
    candidates
        .revision
        .as_ref()
        .cloned()
        .map(|reference| Resolution::Revision { reference })
        .ok_or_else(|| not_found(input))
}

fn not_found(input: &str) -> CoreError {
    CoreError::new(
        ExitClass::NotFound,
        "NOT_FOUND",
        format!("from reference `{input}` was not found"),
        "fetch the reference or choose an existing branch or revision",
    )
}

fn parse_pr_url(input: &str) -> Option<Resolution> {
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let parts: Vec<_> = path.trim_end_matches('/').split('/').collect();
    let (owner, repo, number) = match parts.as_slice() {
        [owner, repo, "pull", number]
        | [owner, repo, "merge_requests", number]
        | [owner, repo, "pull-requests", number] => (*owner, *repo, *number),
        [owner, repo, "-", "merge_requests", number] => (*owner, *repo, *number),
        _ => return None,
    };
    Some(Resolution::PullRequestUrl {
        host: host.to_owned(),
        owner: owner.to_owned(),
        repo: repo.trim_end_matches(".git").to_owned(),
        number: number.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_remote_revision_and_pr_precedence_match_new() {
        let both = RefCandidates {
            local: Some("refs/heads/main".to_owned()),
            local_oid: Some("aaa".to_owned()),
            origin: Some("refs/remotes/origin/main".to_owned()),
            origin_oid: Some("bbb".to_owned()),
            revision: Some("abc".to_owned()),
        };
        assert!(matches!(
            decide("main", &both).unwrap(),
            Resolution::Local {
                notice: Some("FROM_LOCAL_SHADOWS_REMOTE"),
                ..
            }
        ));
        assert_eq!(
            normalize_url("git@github.com:o/r.git"),
            Some(("github.com".to_owned(), "o/r".to_owned()))
        );
        assert_eq!(
            pr_refspec("gitlab.com", 7)[0].0,
            "refs/merge-requests/7/head"
        );
        assert_eq!(pr_refspec("forge.example", 7).len(), 2);
        assert_eq!(
            add_spec("work", "origin/main", false, false, false, true).unwrap(),
            AddSpec::NewBranch {
                name: "work".to_owned(),
                start: "origin/main".to_owned(),
                track: true,
            }
        );
        assert_eq!(
            decide("pr:42", &both).unwrap(),
            Resolution::PullRequest { number: 42 }
        );
        assert!(matches!(
            decide("https://github.com/o/r/pull/7", &both).unwrap(),
            Resolution::PullRequestUrl { number: 7, .. }
        ));
    }

    #[test]
    fn decision_table_covers_equal_refs_forcing_fallbacks_and_errors() {
        let same = RefCandidates {
            local: Some("refs/heads/main".to_owned()),
            local_oid: Some("aaa".to_owned()),
            origin: Some("refs/remotes/origin/main".to_owned()),
            origin_oid: Some("aaa".to_owned()),
            revision: Some("deadbeef".to_owned()),
        };
        assert_eq!(
            decide("main", &same).unwrap(),
            Resolution::Local {
                reference: "refs/heads/main".to_owned(),
                notice: None
            }
        );
        assert_eq!(
            decide("origin/main", &same).unwrap(),
            Resolution::Remote {
                reference: "refs/remotes/origin/main".to_owned()
            }
        );

        let remote = RefCandidates {
            local: None,
            local_oid: None,
            origin: Some("refs/remotes/origin/topic".to_owned()),
            origin_oid: Some("ccc".to_owned()),
            revision: Some("deadbeef".to_owned()),
        };
        assert!(matches!(
            decide("topic", &remote),
            Ok(Resolution::Remote { .. })
        ));

        let revision = RefCandidates {
            local: None,
            local_oid: None,
            origin: None,
            origin_oid: None,
            revision: Some("deadbeef".to_owned()),
        };
        assert!(matches!(
            decide("deadbeef", &revision),
            Ok(Resolution::Revision { .. })
        ));
        assert_eq!(
            decide("origin/main", &revision).unwrap_err().code.0,
            "NOT_FOUND"
        );

        let absent = RefCandidates {
            revision: None,
            ..revision
        };
        assert_eq!(decide("missing", &absent).unwrap_err().code.0, "NOT_FOUND");
        assert!(matches!(
            decide("https://bitbucket.org/o/r/pull-requests/9", &absent),
            Ok(Resolution::PullRequestUrl { number: 9, .. })
        ));
    }
}
