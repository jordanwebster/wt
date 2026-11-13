//! Making and removing trees, and their lifecycle hooks.

mod common;

use std::fs;
use std::process::Command;

use common::{Fixture, git, stdout};
use wt::{CreateStatus, Hook, NewTree, Remove};

#[test]
fn new_starts_from_origin_and_leaves_the_main_checkout_alone() {
    let fx = Fixture::new();
    let origin = fx.with_origin();
    let before = git(&fx.repo, &["rev-parse", "main"]);
    let upstream = fx.advance_origin(&origin);

    let created = fx
        .wt()
        .new_tree(&fx.repo, NewTree::named("feature"))
        .unwrap();
    assert!(created.warnings.is_empty(), "{:?}", created.warnings);
    let tree = created.tree;
    assert_eq!(tree.id(), "app/feature");
    assert_eq!(tree.root, fx.home.join("trees/app/feature"));
    assert_eq!(tree.branch().as_deref(), Some("feature"));
    assert_eq!(git(&tree.root, &["rev-parse", "HEAD"]), upstream);
    assert_eq!(git(&fx.repo, &["rev-parse", "main"]), before);
    let upstream_of_feature = git(
        &fx.repo,
        &["for-each-ref", "--format=%(upstream)", "refs/heads/feature"],
    );
    assert_eq!(upstream_of_feature, "");
}

#[test]
fn a_failed_fetch_starts_from_the_last_fetched_copy() {
    let fx = Fixture::new();
    fx.with_origin();
    let fetched = git(&fx.repo, &["rev-parse", "origin/main"]);
    git(
        &fx.repo,
        &["remote", "set-url", "origin", "/nonexistent/origin.git"],
    );

    let created = fx.wt().new_tree(&fx.repo, NewTree::named("t")).unwrap();
    assert_eq!(created.warnings.len(), 1);
    assert!(created.warnings[0].contains("couldn't fetch origin/main"));
    assert_eq!(git(&created.tree.root, &["rev-parse", "HEAD"]), fetched);
}

#[test]
fn branch_templates_use_the_first_complete_one() {
    let fx = Fixture::new();
    fx.config("branch = [\"{{ticket}}_{{name}}\", \"{{name}}\"]\n");
    let wt = fx.wt();
    let mut request = NewTree::named("vocab");
    request.vars.insert("ticket".into(), "PRO-1170".into());
    let tree = wt.new_tree(&fx.repo, request).unwrap().tree;
    assert_eq!(tree.branch().as_deref(), Some("PRO-1170_vocab"));
    let tree = wt.new_tree(&fx.repo, NewTree::named("plain")).unwrap().tree;
    assert_eq!(tree.branch().as_deref(), Some("plain"));
}

#[test]
fn new_checks_out_an_existing_branch() {
    let fx = Fixture::new();
    git(&fx.repo, &["branch", "existing"]);
    let mut request = NewTree::named("t");
    request.branch = Some("existing".into());
    let tree = fx.wt().new_tree(&fx.repo, request).unwrap().tree;
    assert_eq!(tree.branch().as_deref(), Some("existing"));
}

#[test]
fn new_refuses_duplicates_and_cleans_up_after_a_bad_config() {
    let fx = Fixture::new();
    let wt = fx.wt();
    wt.new_tree(&fx.repo, NewTree::named("t")).unwrap();
    assert!(wt.new_tree(&fx.repo, NewTree::named("t")).is_err());

    // A config that only fails once the tree exists (an unknown template
    // variable) leaves neither a worktree nor a branch behind.
    fx.config("[env]\nX = \"{{nope}}\"\n");
    assert!(wt.new_tree(&fx.repo, NewTree::named("bad")).is_err());
    assert!(!fx.home.join("trees/app/bad").exists());
    assert!(!git(&fx.repo, &["branch", "--list", "bad"]).contains("bad"));
}

#[test]
fn new_by_label_from_anywhere() {
    let fx = Fixture::new();
    let wt = fx.wt();
    wt.new_tree(&fx.repo, NewTree::named("first")).unwrap();
    let mut request = NewTree::named("second");
    request.label = Some("app".into());
    let tree = wt.new_tree(&fx.root, request).unwrap().tree;
    assert_eq!(tree.id(), "app/second");
    let mut unknown = NewTree::named("x");
    unknown.label = Some("nope".into());
    assert!(wt.new_tree(&fx.root, unknown).is_err());
}

#[test]
fn create_hook_records_its_status_and_output() {
    let fx = Fixture::new();
    fx.config("[lifecycle]\ncreate = \"echo made $WT_TREE; echo oops >&2\"\n");
    let output = fx.cli(&fx.repo, &["new", "t"]);
    assert!(stdout(&output).contains("made app/t"));
    let tree = fx.wt().find(Some("app/t"), &fx.root).unwrap();
    assert_eq!(tree.create_status(), CreateStatus::Succeeded);
    let log = fs::read_to_string(tree.create_log()).unwrap();
    assert!(log.contains("made app/t") && log.contains("oops"), "{log}");

    fx.config("[lifecycle]\ncreate = \"exit 7\"\n[bin]\ntool = \"x\"\n");
    let output = fx.cli(&fx.repo, &["new", "broken"]);
    assert_eq!(output.status.code(), Some(7));
    let tree = fx.wt().find(Some("app/broken"), &fx.root).unwrap();
    assert_eq!(tree.create_status(), CreateStatus::Failed(7));
    let wrapper = Command::new(tree.root.join(".wt/bin/tool"))
        .output()
        .unwrap();
    let message = String::from_utf8_lossy(&wrapper.stderr);
    assert!(message.contains("create hook failed"), "{message}");
}

#[test]
fn rm_runs_stop_and_delete_then_removes_the_worktree_and_keeps_the_branch() {
    let fx = Fixture::new();
    let log = fx.root.join("hooks.log");
    fx.config(&format!(
        "[lifecycle]\nstop = \"echo stop $WT_TREE >> {0}\"\ndelete = \"echo delete $WT_TREE >> {0}\"\n",
        log.display()
    ));
    fx.wt().new_tree(&fx.repo, NewTree::named("t")).unwrap();
    stdout(&fx.cli(&fx.repo, &["rm", "t"]));
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "stop app/t\ndelete app/t\n"
    );
    assert!(!fx.home.join("trees/app/t").exists());
    assert!(fx.wt().list().unwrap().is_empty());
    assert!(git(&fx.repo, &["branch", "--list", "t"]).contains('t'));
}

#[test]
fn rm_refuses_unsaved_work_and_failing_hooks_unless_forced() {
    let fx = Fixture::new();
    fx.config("[lifecycle]\ndelete = \"exit 1\"\n");
    let wt = fx.wt();
    let tree = wt.new_tree(&fx.repo, NewTree::named("t")).unwrap().tree;

    let output = fx.cli(&fx.repo, &["rm", "t"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("delete hook failed"));
    assert!(tree.root.exists());

    fs::write(tree.root.join("scratch.txt"), "unsaved").unwrap();
    let error = wt.remove(&tree, Remove::default()).unwrap_err().to_string();
    assert!(error.contains("untracked"), "{error}");

    stdout(&fx.cli(&fx.repo, &["rm", "t", "--force"]));
    assert!(!tree.root.exists());
}

#[test]
fn the_main_checkout_is_only_forgotten() {
    let fx = Fixture::new();
    let wt = fx.wt();
    let main = wt.init(&fx.repo, Default::default()).unwrap();
    assert!(wt.remove(&main, Remove::default()).is_err());
    wt.remove(
        &main,
        Remove {
            keep: true,
            ..Remove::default()
        },
    )
    .unwrap();
    assert!(fx.repo.join("README").exists());
    assert!(wt.list().unwrap().is_empty());
}

#[test]
fn start_and_stop_run_in_one_tree_or_all() {
    let fx = Fixture::new();
    let log = fx.root.join("hooks.log");
    fx.config(&format!(
        "[lifecycle]\nstart = \"echo start $WT_TREE >> {0}\"\nstop = \"echo stop $WT_TREE >> {0}\"\n",
        log.display()
    ));
    let wt = fx.wt();
    let a = wt.new_tree(&fx.repo, NewTree::named("a")).unwrap().tree;
    wt.new_tree(&fx.repo, NewTree::named("b")).unwrap();
    stdout(&fx.cli(&a.root, &["start"]));
    stdout(&fx.cli(&fx.root, &["stop", "--all"]));
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "start app/a\nstop app/a\nstop app/b\n"
    );
    assert!(a.hook(Hook::Create).unwrap().is_none());
}

#[test]
fn a_tree_deleted_by_hand_is_not_recreated_and_can_be_removed() {
    let fx = Fixture::new();
    fx.config("[files.\"x.txt\"]\ncontent = \"x\"\n[lifecycle]\ndelete = \"exit 1\"\n");
    let tree = fx
        .wt()
        .new_tree(&fx.repo, NewTree::named("t"))
        .unwrap()
        .tree;
    fs::remove_dir_all(&tree.root).unwrap();
    assert!(tree.env().is_err());
    assert!(!tree.root.exists());
    stdout(&fx.cli(&fx.repo, &["rm", "t"]));
    assert!(fx.wt().list().unwrap().is_empty());
}
