//! Forge command-line clients. A pull request's head branch is not in the
//! git protocol — `refs/pull/N/head` carries commits, not the name of the
//! branch they came from — so the forge's own CLI answers for it.

use std::path::Path;
use std::time::Duration;

use wt_core::from_ref::PullRequestHead;
use wt_core::{CoreError, ExitClass};

use crate::proc::{self, CommandRequest};
use crate::Result;

/// The exit status `gh` uses for a missing or expired login.
const GH_AUTH_EXIT: i32 = 4;

/// Asks `gh` for the branch a pull request of the repository at `repo` was
/// opened from. `gh` resolves the repository from the checkout's remotes,
/// so a GitHub Enterprise host works exactly as github.com does.
pub fn github_pull_request_head(
    repo: &Path,
    number: u64,
    timeout: Duration,
) -> Result<PullRequestHead> {
    let escape = "pass `--from origin/<branch>` if you already know the pull request's branch";
    if proc::on_path("gh").is_none() {
        return Err(CoreError::new(
            ExitClass::External,
            "FORGE_CLI_MISSING",
            format!("pull request {number} needs the GitHub CLI to find its branch, and `gh` is not on PATH"),
            format!("install gh (https://cli.github.com), run `gh auth login`, and retry; or {escape}"),
        ));
    }
    let mut request = CommandRequest::new("gh");
    request.args = proc::os_args(&[
        "pr",
        "view",
        &number.to_string(),
        "--json",
        "headRefName,isCrossRepository,headRepositoryOwner",
    ]);
    request.cwd = Some(repo.to_path_buf());
    request
        .env
        .insert("GH_PROMPT_DISABLED".to_owned(), "1".to_owned());
    request
        .env
        .insert("GH_NO_UPDATE_NOTIFIER".to_owned(), "1".to_owned());
    let output = proc::capture_op(&request, timeout, Some("gh pr view"))?;
    if output.timed_out {
        return Err(CoreError::new(
            ExitClass::Timeout,
            "TIMEOUT",
            format!("gh timed out looking up pull request {number}"),
            "retry the operation or raise git.timeouts.fetch",
        ));
    }
    if !output.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("no error output")
            .to_owned();
        let remedy = if output.child.code == Some(GH_AUTH_EXIT) {
            format!("run `gh auth login` and retry; or {escape}")
        } else {
            format!(
                "run `gh auth status` in {} to check access to the repository, then retry; or {escape}",
                repo.display()
            )
        };
        return Err(CoreError::new(
            ExitClass::External,
            "FORGE_CLI_FAILED",
            format!("gh could not look up pull request {number}: {reason}"),
            remedy,
        ));
    }
    parse_head(&output.stdout).ok_or_else(|| {
        CoreError::new(
            ExitClass::External,
            "FORGE_CLI_FAILED",
            format!("gh answered for pull request {number} without a head branch"),
            format!("upgrade gh and retry; or {escape}"),
        )
    })
}

fn parse_head(stdout: &[u8]) -> Option<PullRequestHead> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let branch = value["headRefName"].as_str()?.trim();
    if branch.is_empty() {
        return None;
    }
    Some(PullRequestHead {
        branch: branch.to_owned(),
        cross_repository: value["isCrossRepository"].as_bool().unwrap_or(false),
        owner: value["headRepositoryOwner"]["login"]
            .as_str()
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_parses_gh_json_and_rejects_empty_branches() {
        let head = parse_head(
            br#"{"headRefName":"feature/x","isCrossRepository":true,"headRepositoryOwner":{"login":"alice"}}"#,
        )
        .unwrap();
        assert_eq!(head.branch, "feature/x");
        assert!(head.cross_repository);
        assert_eq!(head.owner.as_deref(), Some("alice"));
        assert!(parse_head(br#"{"headRefName":""}"#).is_none());
        assert!(parse_head(b"not json").is_none());
    }
}
