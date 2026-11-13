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
    let output = fx.cli(&fx.repo, &["new", "t", "--wait"]);
    assert!(stdout(&output).contains("made app/t"));
    let tree = fx.wt().find(Some("app/t"), &fx.root).unwrap();
    assert_eq!(tree.create_status(), CreateStatus::Succeeded);
    let log = fs::read_to_string(tree.create_log()).unwrap();
    assert!(log.contains("made app/t") && log.contains("oops"), "{log}");

    fx.config("[lifecycle]\ncreate = \"exit 7\"\n[bin]\ntool = \"x\"\n");
    let output = fx.cli(&fx.repo, &["new", "broken", "--wait"]);
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

#[test]
fn rm_checks_before_running_any_hook() {
    let fx = Fixture::new();
    let log = fx.root.join("hooks.log");
    fx.config(&format!(
        "[lifecycle]\nstop = \"echo stop >> {0}\"\ndelete = \"echo delete >> {0}\"\n",
        log.display()
    ));
    let wt = fx.wt();
    let tree = wt.new_tree(&fx.repo, NewTree::named("t")).unwrap().tree;
    wt.init(&fx.repo, Default::default()).unwrap();

    fs::write(tree.root.join("unsaved.txt"), "x").unwrap();
    assert!(!fx.cli(&fx.repo, &["rm", "t"]).status.success());
    assert!(!fx.cli(&fx.repo, &["rm", "app/main"]).status.success());
    assert!(!log.exists(), "a refused rm ran hooks");
    assert!(tree.root.exists());
}

#[test]
fn rm_sees_untracked_files_whatever_git_is_configured_to_show() {
    let fx = Fixture::new();
    git(&fx.repo, &["config", "status.showUntrackedFiles", "no"]);
    let tree = fx
        .wt()
        .new_tree(&fx.repo, NewTree::named("t"))
        .unwrap()
        .tree;
    fs::write(tree.root.join("notes.txt"), "mine").unwrap();
    let error = fx.wt().remove(&tree, Remove::default()).unwrap_err();
    assert!(error.to_string().contains("untracked"), "{error}");
    assert!(tree.root.join("notes.txt").exists());
}

#[test]
fn a_failed_new_gives_back_its_name() {
    let fx = Fixture::new();
    fx.config("[env]\nX = \"{{nope}}\"\n");
    let wt = fx.wt();
    assert!(wt.new_tree(&fx.repo, NewTree::named("t")).is_err());
    assert!(wt.list().unwrap().is_empty());
    fx.config("");
    wt.new_tree(&fx.repo, NewTree::named("t")).unwrap();
}

#[test]
fn a_tree_with_a_broken_config_can_still_be_forced_out() {
    let fx = Fixture::new();
    let log = fx.root.join("hooks.log");
    fx.config(&format!(
        "[lifecycle]\nstop = \"echo stop $WT_TREE >> {}\"\n",
        log.display()
    ));
    let wt = fx.wt();
    let broken = wt.new_tree(&fx.repo, NewTree::named("a")).unwrap().tree;
    wt.new_tree(&fx.repo, NewTree::named("b")).unwrap();
    fs::write(broken.root.join(".wt.toml"), "commands = [\"x\"]\n").unwrap();
    git(&broken.root, &["commit", "-qam", "old config"]);

    // One broken tree doesn't stop the others being stopped.
    assert!(!fx.cli(&fx.root, &["stop", "--all"]).status.success());
    assert_eq!(fs::read_to_string(&log).unwrap(), "stop app/b\n");

    assert!(!fx.cli(&fx.root, &["rm", "app/a"]).status.success());
    stdout(&fx.cli(&fx.root, &["rm", "app/a", "--force"]));
    assert!(!broken.root.exists());
}

#[test]
fn new_checks_out_a_branch_that_only_origin_has() {
    let fx = Fixture::new();
    let origin = fx.with_origin();
    let other = fx.root.join("colleague");
    git(
        &fx.root,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(&other, &["switch", "-q", "-c", "colleague"]);
    fs::write(other.join("work.txt"), "theirs").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-qm", "their work"]);
    git(&other, &["push", "-q", "origin", "colleague"]);
    let theirs = git(&other, &["rev-parse", "HEAD"]);

    let mut request = NewTree::named("review");
    request.branch = Some("colleague".into());
    let tree = fx.wt().new_tree(&fx.repo, request).unwrap().tree;
    assert_eq!(git(&tree.root, &["rev-parse", "HEAD"]), theirs);
    assert_eq!(
        git(
            &fx.repo,
            &[
                "for-each-ref",
                "--format=%(upstream:short)",
                "refs/heads/colleague"
            ]
        ),
        "origin/colleague"
    );
}

#[test]
fn new_returns_before_setup_finishes_and_wrappers_wait_for_it() {
    let fx = Fixture::new();
    let gate = fx.root.join("gate");
    fx.config(&format!(
        "copy = [\"build\"]\n[bin]\ntool = \"build/tool\"\n[lifecycle]\ncreate = \"while [ ! -e {0} ]; do sleep 0.05; done; echo built\"\n",
        gate.display()
    ));
    // The main checkout has a build of its own, which gets copied in.
    fs::create_dir_all(fx.repo.join("build")).unwrap();
    fs::write(fx.repo.join("build/tool"), "#!/bin/sh\necho main build\n").unwrap();
    Command::new("chmod")
        .arg("755")
        .arg(fx.repo.join("build/tool"))
        .status()
        .unwrap();

    let started = std::time::Instant::now();
    stdout(&fx.cli(&fx.repo, &["new", "t"]));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let tree = fx.wt().find(Some("app/t"), &fx.root).unwrap();
    assert_eq!(tree.create_status(), CreateStatus::Running);

    // Copied but not this tree's build yet: the wrapper refuses.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !tree.root.join("build/tool").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the copy never happened"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let early = Command::new(tree.root.join(".wt/bin/tool"))
        .output()
        .unwrap();
    assert_eq!(early.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&early.stderr).contains("still running"));

    fs::write(&gate, "").unwrap();
    while tree.create_status() == CreateStatus::Running {
        assert!(
            std::time::Instant::now() < deadline,
            "create never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(tree.create_status(), CreateStatus::Succeeded);
    assert!(
        fs::read_to_string(tree.create_log())
            .unwrap()
            .contains("built")
    );
    let ready = Command::new(tree.root.join(".wt/bin/tool"))
        .output()
        .unwrap();
    assert_eq!(stdout(&ready), "main build\n");
}
