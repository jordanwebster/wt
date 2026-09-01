//! Subprocess-shape contracts for the hot paths: the observations list and
//! doctor are allowed to buy, asserted from wt's own timing log rather than
//! wall clocks, so they hold on any machine.

mod common;

use std::path::Path;
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
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|value| Record {
            cmd: value["cmd"].as_str().unwrap_or_default().to_owned(),
            name: value["name"].as_str().unwrap_or_default().to_owned(),
            op: value["op"].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

#[test]
fn list_statuses_each_tree_once_and_doctor_never() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    h.json(&["list"]);
    h.json(&["doctor"]);
    let records = trace_records(&h);
    let statuses = |cmd: &str| {
        records
            .iter()
            .filter(|record| record.cmd == cmd && record.name == "git" && record.op == "status")
            .count()
    };
    // One exact status per tree: the canonical checkout and repo/work.
    assert_eq!(statuses("list"), 2, "list runs one status per tree");
    // No doctor finding classifies dirtiness or remote containment, so
    // doctor buys neither a status scan nor a branch --contains walk.
    assert_eq!(statuses("doctor"), 0, "doctor never runs git status");
    assert!(
        !records
            .iter()
            .any(|record| record.cmd == "doctor" && record.name == "git" && record.op == "branch"),
        "doctor never walks remote branch containment"
    );
}

fn config_value(tree: &Path, args: &[&str]) -> Option<String> {
    let mut request = CommandRequest::new("git");
    request.cwd = Some(tree.to_path_buf());
    request.args = proc::os_args(args);
    let output = proc::capture(&request, Duration::from_secs(10)).unwrap();
    output
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[test]
fn created_trees_get_git_status_caches_scoped_per_worktree() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    let tree = created["data"]["tree"]["path"].as_str().unwrap().to_owned();
    assert_eq!(
        config_value(
            Path::new(&tree),
            &["config", "--get", "core.untrackedCache"]
        )
        .as_deref(),
        Some("true"),
        "a wt-created worktree keeps git's untracked cache on"
    );
    // The scoping matters as much as the cache: the canonical checkout wt
    // did not create keeps its configuration untouched.
    assert_eq!(
        config_value(
            &repo,
            &["config", "--worktree", "--get", "core.untrackedCache"]
        ),
        None,
        "the canonical checkout is not reconfigured"
    );
}

#[test]
fn new_fetches_a_branch_known_only_to_origin() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    h.register(&repo);
    // The branch exists only on origin: no local branch, no remote-tracking
    // ref, so creation must fetch it before it can resolve.
    h.push_ref(&repo, "HEAD", "refs/heads/remoteonly");
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
}
