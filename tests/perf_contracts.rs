//! Subprocess-shape contracts for the hot paths: the observations list and
//! doctor are allowed to buy, asserted from wt's own timing log rather than
//! wall clocks, so they hold on any machine — plus the observable semantics
//! the fast paths must preserve (per-tree default fallback, sequential
//! error selection, narrow fetch with wildcard fallback, session snapshot).

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::Harness;
use wt_sys::proc::{self, CommandRequest};

/// One timing-log record's fields that these contracts read.
#[derive(Debug)]
struct Record {
    cmd: String,
    name: String,
    op: String,
}

fn trace_records(h: &Harness) -> Vec<Record> {
    let text = std::fs::read_to_string(h.home.join("logs/wt.jsonl")).expect("timing log exists");
    text.lines()
        .map(|line| {
            // A malformed or field-stripped record must fail the test:
            // silently coercing one would let a "doctor never ran X"
            // assertion pass vacuously.
            let value = serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|error| panic!("malformed trace record {line:?}: {error}"));
            let field = |key: &str| {
                value[key]
                    .as_str()
                    .unwrap_or_else(|| panic!("trace record without string {key}: {line:?}"))
                    .to_owned()
            };
            Record {
                cmd: field("cmd"),
                name: field("name"),
                // Only wt-composed git argument lists carry an op.
                op: value["op"].as_str().unwrap_or_default().to_owned(),
            }
        })
        .collect()
}

#[test]
fn list_statuses_each_tree_once_and_doctor_never() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    // The timing log these contracts read is opt-in; register preserves
    // the existing settings when it records the session backend.
    common::write(&h.home.join("config.toml"), "[logs]\ntrace = true\n");
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    h.json(&["list"]);
    h.json(&["doctor"]);
    let records = trace_records(&h);
    let children = |cmd: &str, name: &str, op: &str| {
        records
            .iter()
            .filter(|record| record.cmd == cmd && record.name == name && record.op == op)
            .count()
    };
    // One exact status per tree: the canonical checkout and repo/work.
    assert_eq!(
        children("list", "git", "status"),
        2,
        "list runs one status per tree"
    );
    // One session snapshot for the whole fleet, not one tmux ask per tree.
    assert_eq!(
        records
            .iter()
            .filter(|record| record.cmd == "list" && record.name == "tmux")
            .count(),
        1,
        "list takes one tmux session snapshot"
    );
    // No doctor finding classifies dirtiness or remote containment, so
    // doctor buys neither a status scan nor a branch --contains walk.
    assert_eq!(
        children("doctor", "git", "status"),
        0,
        "doctor never runs git status"
    );
    assert_eq!(
        children("doctor", "git", "branch"),
        0,
        "doctor never walks remote branch containment"
    );
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut request = CommandRequest::new("git");
    request.cwd = Some(cwd.to_path_buf());
    request.args = proc::os_args(args);
    let output = proc::capture(&request, Duration::from_secs(10)).unwrap();
    output
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn ref_exists(cwd: &Path, reference: &str) -> bool {
    git_output(cwd, &["rev-parse", "--verify", "--quiet", reference]).is_some()
}

#[test]
fn created_trees_get_git_status_caches_scoped_per_worktree() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        git_output(&tree, &["config", "--get", "core.untrackedCache"]).as_deref(),
        Some("true"),
        "a wt-created worktree keeps git's untracked cache on"
    );
    // The scoping matters as much as the cache: the canonical checkout wt
    // did not create keeps its effective status configuration untouched.
    assert_eq!(
        git_output(
            &repo,
            &["config", "--worktree", "--get", "core.untrackedCache"]
        ),
        None,
        "the canonical checkout is not reconfigured"
    );
    // The built-in filesystem monitor is platform- and version-gated;
    // assert it exactly where the gate says it applies. The harness PATH
    // is shims:/usr/bin:/bin with no git shim, so wt's git is /usr/bin/git
    // — version the same executable, not whatever leads the outer PATH.
    if cfg!(target_os = "macos")
        && wt_sys::git::Git::version("/usr/bin/git", Duration::from_secs(10)).unwrap() >= (2, 37, 0)
    {
        assert_eq!(
            git_output(&tree, &["config", "--worktree", "--get", "core.fsmonitor"]).as_deref(),
            Some("true"),
            "supported platforms also get the filesystem monitor"
        );
    }
}

#[test]
fn acceleration_survives_a_repo_with_worktree_config_already_enabled() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    common::git(&repo, &["config", "extensions.worktreeConfig", "true"]);
    // The git-prescribed arrangement for such a repository: core.worktree
    // lives in the main worktree's own config.worktree. An effective-value
    // guard that cannot tell this from unmigrated shared config would skip
    // acceleration here.
    common::git(
        &repo,
        &[
            "config",
            "--worktree",
            "core.worktree",
            repo.to_str().unwrap(),
        ],
    );
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        git_output(&tree, &["config", "--get", "core.untrackedCache"]).as_deref(),
        Some("true"),
        "an already-enabled extension does not disable acceleration"
    );
}

#[test]
fn new_fetches_narrowly_and_falls_back_for_raw_revisions() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    h.register(&repo);
    // Two branches exist only on origin: no local branch, no
    // remote-tracking ref. Creation from one must fetch it; the other
    // proves the fetch was narrow rather than the old refs/heads/*.
    h.push_ref(&repo, "HEAD", "refs/heads/remoteonly");
    h.push_ref(&repo, "HEAD", "refs/heads/untouched");
    // Pushing updates the local remote-tracking refs; drop them so the
    // branches are genuinely known only to origin.
    common::git(
        &repo,
        &["update-ref", "-d", "refs/remotes/origin/remoteonly"],
    );
    common::git(
        &repo,
        &["update-ref", "-d", "refs/remotes/origin/untouched"],
    );
    let created = h.json(&[
        "new",
        "repo/r",
        "--from",
        "remoteonly",
        "--no-sync",
        "--no-open",
    ]);
    // h.json already asserted success; a remote start is tracked, so the
    // upstream field proves resolution went through origin/remoteonly.
    assert!(created["data"]["tree"]["upstream"].is_object());
    assert!(
        !ref_exists(&repo, "refs/remotes/origin/untouched"),
        "a narrow fetch leaves unrelated origin branches unfetched"
    );
    // A raw revision can never be a remote branch name, so the narrow
    // fetch fails and the wildcard fallback restores resolve-anything —
    // observable because the fallback fetches the unrelated branch too.
    let head = git_output(&repo, &["rev-parse", "HEAD"]).unwrap();
    h.json(&["new", "repo/rev", "--from", &head, "--no-sync", "--no-open"]);
    assert!(
        ref_exists(&repo, "refs/remotes/origin/untouched"),
        "the wildcard fallback fetched every origin branch"
    );
}

#[test]
fn default_branch_head_fallback_stays_per_tree() {
    let h = Harness::new();
    // A repository with no origin/HEAD (plain remote add, not a clone) and
    // no main/master/trunk anywhere: the default-branch chain ends at each
    // tree's own HEAD, and that fallback must not leak between trees.
    let repo = h.repos.join("odd");
    wt_sys::fsx::create_private_dir(&repo).unwrap();
    common::git(&repo, &["init", "-q"]);
    common::write(&repo.join("README.md"), "fixture\n");
    common::git(&repo, &["add", "-A"]);
    common::git(&repo, &["commit", "-qm", "fixture"]);
    common::git(&repo, &["branch", "-m", "alpha"]);
    let origin = h.repos.join("odd-origin.git");
    wt_sys::fsx::create_private_dir(&origin).unwrap();
    common::git(&origin, &["init", "--bare", "-q"]);
    common::git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    common::git(&repo, &["push", "-qu", "origin", "alpha"]);
    h.register(&repo);
    let created = h.json(&["new", "odd/beta", "--no-sync", "--no-open"]);
    let beta = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    common::git(&beta, &["push", "-qu", "origin", "beta"]);
    // Advance alpha past beta's tip so a leaked fallback would misreport.
    common::write(&repo.join("README.md"), "advanced\n");
    common::git(&repo, &["add", "-A"]);
    common::git(&repo, &["commit", "-qm", "advance"]);
    common::git(&repo, &["push", "-q", "origin", "alpha"]);
    let list = h.json(&["list"]);
    let tree = list["data"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["target"] == "odd/beta")
        .unwrap();
    // beta's default is beta — its own HEAD — never the canonical
    // checkout's alpha, so it is not behind.
    assert_eq!(tree["behind_default"], 0);
    let doctor = h.json(&["doctor", "odd"]);
    assert!(
        !doctor["data"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "BRANCH_MERGED" && finding["subject"] == "odd/beta"),
        "a tree equal to its own origin branch is not merged"
    );
}

#[test]
fn list_reports_the_earliest_trees_error_first() {
    let h = Harness::new();
    let first = h.repo("aaa", "");
    h.register(&first);
    let second = h.repo("bbb", "");
    h.register(&second);
    // The earlier tree has corrupt tree state; the later repository's
    // corrupt HEAD makes every git command fail, including the up-front
    // per-label fact queries (verified against the pre-fix build, which
    // reported that later GIT_FAILED instead). Error selection must match
    // the sequential read: the earlier tree's state error wins.
    let state_dir = h.home.join("state/aaa");
    let state_file = std::fs::read_dir(&state_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            // Exactly the tree's state file — never the label's shared
            // `_repo.json`, whose read happens up front by contract.
            path.extension().is_some_and(|ext| ext == "json")
                && path.file_stem().is_some_and(|stem| stem != "_repo")
        })
        .expect("aaa has a tree state file");
    common::write(&state_file, "not json");
    common::write(&second.join(".git/HEAD"), "garbage\n");
    let output = h.wt().args(["list", "--json"]).output().unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "STATE_CORRUPT");
    // Two files could produce STATE_CORRUPT; the message's path pins the
    // selection to the earlier tree's file.
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains(state_file.to_str().unwrap()),
        "the reported error names the earlier tree's state file: {}",
        value["error"]["message"]
    );
}

#[test]
fn a_failed_fallback_reports_the_narrow_fetch_error() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    h.register(&repo);
    // With origin unreachable, both the narrow fetch and the wildcard
    // fallback fail; the reported error must be the narrow one, which
    // names the refs the creation asked for.
    common::git(
        &repo,
        &["remote", "set-url", "origin", "/nonexistent/origin.git"],
    );
    let output = h
        .wt()
        .args([
            "new",
            "repo/x",
            "--from",
            "wanted",
            "--no-sync",
            "--no-open",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("refs/heads/wanted"),
        "the narrow refspec is named: {message}"
    );
    assert!(
        !message.contains("refs/heads/*"),
        "the wildcard attempt's error is not the one reported: {message}"
    );
}

#[test]
fn session_snapshot_keeps_the_three_session_answers() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    let sessions = |h: &Harness| {
        let list = h.json(&["list"]);
        let of = |target: &str| {
            list["data"]["trees"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tree| tree["target"] == target)
                .unwrap()["session"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        (of("repo"), of("repo/work"))
    };
    // No server, no sessions: everything is a definite no.
    assert_eq!(sessions(&h), ("no".into(), "no".into()));
    // One live session marks exactly its own tree, not every tree.
    h.json(&["open", "repo", "--agent", "codex", "--no-attach"]);
    assert_eq!(
        sessions(&h),
        ("yes".into(), "no".into()),
        "the snapshot discriminates by exact session name"
    );
    // A tmux that fails outright leaves every session unknown, never a
    // false no — whether it runs and fails or cannot be spawned at all.
    common::write_executable(&h.shims.join("tmux"), "#!/bin/sh\nexit 2\n");
    assert_eq!(sessions(&h), ("unknown".into(), "unknown".into()));
    // The unspawnable case must not delete the shim: PATH would then fall
    // through to a system tmux where one is installed (Ubuntu CI). An
    // executable whose interpreter does not exist fails to spawn on every
    // platform while still shadowing any real tmux.
    wt_sys::fsx::write_nofollow(
        &h.shims,
        &wt_core::model::RelPath::new("tmux").unwrap(),
        b"#!/nonexistent-interpreter\n",
        0o755,
    )
    .unwrap();
    assert_eq!(sessions(&h), ("unknown".into(), "unknown".into()));
    // Backend none answers without asking tmux at all.
    common::write(
        &h.home.join("config.toml"),
        "[session]\nbackend = \"none\"\n",
    );
    assert_eq!(sessions(&h), ("no".into(), "no".into()));
}
