mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use common::Harness;
use predicates::prelude::*;

const BASIC: &str = r#"
ports = ["http"]
[env]
APP_PORT = "{{ports.http}}"
[files.".wt/generated"]
content = "port={{ports.http}}"
"#;

static NEXT_ISOLATED_PORT: AtomicU16 = AtomicU16::new(40_000);

fn configure_backend_none(h: &Harness) {
    let port = NEXT_ISOLATED_PORT.fetch_add(32, Ordering::Relaxed);
    common::write(
        &h.home.join("config.toml"),
        &format!("[ports]\nbase={port}\nstride=1\n[session]\nbackend='none'\n"),
    );
}

fn advance_origin(h: &Harness, name: &str, file: &str) -> String {
    let origin = h.repos.join(format!("{name}-origin.git"));
    let other = h.root.join(format!("{name}-other-{file}"));
    common::git(
        &h.root,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    common::write(&other.join(file), "upstream\n");
    common::git(&other, &["add", "-A"]);
    common::git(&other, &["commit", "-qm", "upstream change"]);
    common::git(&other, &["push", "-q", "origin", "main"]);
    common::git_stdout(&other, &["rev-parse", "HEAD"])
        .trim()
        .to_owned()
}

fn quiet(h: &Harness) {
    static NEXT_BASE: AtomicU16 = AtomicU16::new(45_000);
    let base = NEXT_BASE.fetch_add(128, Ordering::Relaxed);
    common::write(
        &h.home.join("config.toml"),
        &format!("[ports]\nbase={base}\nstride=16\n[session]\nbackend='none'\n"),
    );
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} was not created",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_absence(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while path.exists() {
        assert!(Instant::now() < deadline, "{} still exists", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn removed_configuration_keys_name_their_replacements() {
    let cases = [
        (
            "task-run",
            "[task.old]\nrun='true'\n",
            "repository task runner",
        ),
        ("task-build", "[task.build]\nrun='true'\n", "after_new"),
        ("task-warm", "[task.warm]\nrun='true'\n", "[hooks] warm"),
        (
            "task-needs",
            "[task.old]\nneeds=['later']\n[task.later]\nrun='true'\n",
            "resource dependencies",
        ),
        (
            "task-lock",
            "[task.old]\nrun='true'\nlock='serial'\n[locks.serial]\nslots=1\n",
            "token pool",
        ),
        (
            "task-exclusive",
            "[task.old]\ntied_to='tree'\nexists='false'\ndestroy='true'\nexclusive='repo'\n",
            "arena",
        ),
        ("locks-table", "[locks.serial]\nslots=1\n", "token pools"),
        ("resource-run", "[resource.old]\nrun='true'\n", "create"),
        (
            "resource-reset",
            "[resource.old]\nreset='true'\n",
            "acquire",
        ),
        (
            "resource-lock",
            "[resource.old]\nlock='serial'\n",
            "token pool",
        ),
        (
            "resource-exclusive",
            "[resource.old]\nexclusive='repo'\n",
            "arena",
        ),
        (
            "token-keep",
            "[resource.old]\ncount=1\nkeep='1m'\n",
            "may not declare",
        ),
        (
            "token-idle",
            "[resource.old]\ncount=1\nidle='1m'\n",
            "may not declare",
        ),
    ];
    for (label, config, replacement) in cases {
        let h = Harness::new();
        quiet(&h);
        let repo = h.repo(label, config);
        h.wt()
            .args(["register", repo.to_str().unwrap(), "--json"])
            .assert()
            .code(5)
            .stdout(predicate::str::contains("CONFIG_INVALID"))
            .stdout(predicate::str::contains(replacement));
    }
}

#[test]
fn removed_work_and_inspection_verbs_are_not_commands() {
    let h = Harness::new();
    for verb in [
        "run", "test", "lint", "fmt", "build", "tasks", "locks", "lease",
    ] {
        h.wt()
            .arg(verb)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn current_state_file_is_migrated_on_the_first_upgraded_invocation() {
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo("repo", "");
    h.register(&repo);
    let label = wt_core::model::Label::new("repo").unwrap();
    let path = h.home.join(wt_core::model::repo_state_path(&label));
    common::write(&path, include_str!("fixtures/current-v1-repo-state.json"));

    h.wt().args(["config", "repo", "--json"]).assert().success();

    let migrated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(migrated["schema"], 2);
    assert_eq!(migrated["arenas"]["database"]["holder"], "repo/work");
    assert_eq!(
        migrated["resources"]["database"]["instance"]["env"]["DATABASE_URL"],
        "frozen"
    );
    assert!(migrated.get("exclusive").is_none());
}

#[test]
fn configured_output_is_rendered_cloned_sized_and_not_swept() {
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo("cache-fixture", "output=[{path='node_modules'}]\n");
    common::write(&repo.join("node_modules/cache/marker"), "warm\n");
    h.register(&repo);
    let config = h.json(&["config", "cache-fixture"]);
    assert_eq!(
        config["data"]["entries"][0]["value"]["output"][0]["kind"],
        "opaque"
    );
    let made = h.json(&[
        "new",
        "cache-fixture/work",
        "--no-sync",
        "--no-after-new",
        "--no-open",
    ]);
    let tree = PathBuf::from(made["data"]["tree"]["path"].as_str().unwrap());
    if cfg!(target_os = "macos") {
        assert_eq!(
            std::fs::read_to_string(tree.join("node_modules/cache/marker")).unwrap(),
            "warm\n"
        );
        let list = h.json(&["ls", "cache-fixture", "--disk"]);
        let row = list["data"]["trees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["target"] == "cache-fixture/work")
            .unwrap();
        assert!(row["build_kb"].as_u64().unwrap() > 0);
    }
    assert!(h.json(&["prune", "cache-fixture"])["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["action"] != "sweep"));
}

#[test]
fn after_new_is_detached_gates_claims_and_anchor_runs_warm() {
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo(
        "hook-fixture",
        r#"
bin=["bin"]
commands=["claimed-tool"]
output=[{path="cache"}]
[hooks]
after_new="sleep 0.15; touch .wt/after-new"
warm="touch .wt/warm"
"#,
    );
    common::write_executable(&repo.join("bin/claimed-tool"), "#!/bin/sh\nprintf ready\n");
    common::write(&repo.join("cache/seed"), "seed\n");
    common::git(&repo, &["add", "bin/claimed-tool", "cache/seed"]);
    common::git(&repo, &["commit", "-qm", "fixture tool"]);
    h.register(&repo);
    let made = h.json(&["new", "hook-fixture/work", "--no-sync", "--no-open"]);
    let tree = PathBuf::from(made["data"]["tree"]["path"].as_str().unwrap());
    h.wt()
        .current_dir(&tree)
        .args(["exec", "--", "claimed-tool"])
        .assert()
        .success()
        .stdout("ready");
    wait_for(&tree.join(".wt/after-new"));
    wait_for(&repo.join(".wt/warm"));
}

#[test]
fn durable_resources_cover_dependencies_scopes_and_arena_take() {
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo(
        "resource-fixture",
        r#"
[resource.base]
tied_to="tree"
name="{{name_short()}}-base"
exists="test -f .wt/base"
create="touch .wt/base"
destroy="rm -f .wt/base"

[resource.service]
tied_to="tree"
needs=["base"]
arena="repo"
capacity=2
name="{{name_short()}}-service"
exists="test -f .wt/service"
create="touch .wt/service"
destroy="rm -f .wt/service"

[resource.shared]
tied_to="repo"
name="shared-fixture"
exists="test -f $WT_HOME/shared-fixture"
create="touch $WT_HOME/shared-fixture"
destroy="rm -f $WT_HOME/shared-fixture"

[resource.machine]
tied_to="machine"
name="machine-fixture"
exists="test -f $WT_HOME/machine-fixture"
create="touch $WT_HOME/machine-fixture"
destroy="rm -f $WT_HOME/machine-fixture"
"#,
    );
    h.register(&repo);
    let mut roots = Vec::new();
    for name in ["one", "two", "three"] {
        let made = h.json(&[
            "new",
            &format!("resource-fixture/{name}"),
            "--no-sync",
            "--no-after-new",
            "--no-open",
        ]);
        roots.push(PathBuf::from(
            made["data"]["tree"]["path"].as_str().unwrap(),
        ));
    }
    for root in &roots[..2] {
        h.wt()
            .current_dir(root)
            .args(["with", "service", "shared", "machine", "--json"])
            .assert()
            .success();
        assert!(root.join(".wt/base").exists());
    }
    h.wt()
        .current_dir(&roots[2])
        .args(["with", "service", "--json"])
        .assert()
        .code(4)
        .stdout(predicate::str::contains("RESOURCE_HELD"));
    h.wt()
        .current_dir(&roots[2])
        .args(["with", "service", "--take", "--json"])
        .assert()
        .success();
    assert!(h.home.join("shared-fixture").exists());
    assert!(h.home.join("machine-fixture").exists());
}

#[test]
fn token_pools_order_export_log_release_and_refuse_invalid_uses() {
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo(
        "token-fixture",
        "[resource.zed]\ncount=1\nwait='100ms'\n[resource.alpha]\ncount=1\nwait='500ms'\n",
    );
    h.register(&repo);
    h.wt()
        .current_dir(&repo)
        .args(["with", "alpha", "--json"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("TOKEN_COMMAND_REQUIRED"));
    h.wt()
        .current_dir(&repo)
        .args(["--json", "with", "alpha", "alpha", "--", "true"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("RESOURCE_DUPLICATE"));
    let output = h
        .wt()
        .current_dir(&repo)
        .args([
            "--json",
            "with",
            "zed",
            "alpha",
            "--",
            "sh",
            "-c",
            "printf '%s|%s' \"$WT_LEASE_ALPHA\" \"$WT_LEASE_ZED\"",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["leases"]["WT_LEASE_ALPHA"], "alpha-1");
    assert_eq!(value["data"]["leases"]["WT_LEASE_ZED"], "zed-1");
    assert_eq!(
        std::fs::read_to_string(value["data"]["log"].as_str().unwrap()).unwrap(),
        "alpha-1|zed-1"
    );
    assert!(h.json(&["leases", "token-fixture"])["data"]["leases"]
        .as_array()
        .unwrap()
        .iter()
        .all(|slot| slot["hold"] == "free"));

    let mut child = h
        .wt_std()
        .current_dir(&repo)
        .args(["with", "alpha", "--", "sleep", "2"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let listed = h.json(&["leases", "token-fixture"]);
    let held = listed["data"]["leases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|slot| slot["pool"] == "alpha")
        .unwrap();
    assert_eq!(held["hold"], "leased");
    assert!(held["holder"]["pid"].as_u64().unwrap() > 0);
    assert!(!held["holder"]["process_start"].as_str().unwrap().is_empty());
    h.wt()
        .current_dir(&repo)
        .args(["--json", "with", "alpha", "--take", "--", "true"])
        .assert()
        .code(4)
        .stdout(predicate::str::contains("LEASE_HELD"))
        .stdout(predicate::str::contains("\"waiting_on\":\"work\""));
    let _ = child.kill();
    let _ = child.wait();
    let doctor = h.json(&["doctor", "token-fixture"]);
    assert!(doctor["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "LEASE_HOLDER_DEAD"));
    h.wt()
        .current_dir(&repo)
        .args(["--json", "with", "alpha", "--", "true"])
        .assert()
        .success();
    let mut finishing = h
        .wt_std()
        .current_dir(&repo)
        .args(["with", "alpha", "--", "sleep", "0.15"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    h.wt()
        .current_dir(&repo)
        .args(["--json", "with", "alpha", "--", "true"])
        .assert()
        .success();
    assert!(finishing.wait().unwrap().success());
    h.wt()
        .current_dir(&repo)
        .args(["--json", "with", "alpha", "--", "sh", "-c", "kill -9 $$"])
        .assert()
        .code(6);
    h.wt()
        .current_dir(&repo)
        .args(["--json", "with", "alpha", "--", "true"])
        .assert()
        .success();
}

#[test]
fn a_later_pool_failure_releases_an_earlier_instance() {
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo(
        "partial-lease-fixture",
        r#"
[resource.alpha]
tied_to="repo"
count=1
name="alpha-{{index()}}"
exists="test -f $WT_HOME/$WT_SELF"
create="touch $WT_HOME/$WT_SELF"
acquire="printf acquire >> $WT_HOME/acquire-events"
destroy="rm -f $WT_HOME/$WT_SELF"
keep="0ms"
wait="100ms"

[resource.zed]
count=1
wait="100ms"
"#,
    );
    h.register(&repo);
    let mut blocker = h
        .wt_std()
        .current_dir(&repo)
        .args(["with", "zed", "--", "sleep", "2"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    h.wt()
        .current_dir(&repo)
        .args(["with", "alpha", "zed", "--", "true"])
        .assert()
        .code(4);
    let label = wt_core::model::Label::new("partial-lease-fixture").unwrap();
    let state: wt_core::lifecycle::RepoState = wt_sys::fsx::read_json(
        &h.home.join(wt_core::model::repo_state_path(&label)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        state.resources["alpha#1"].hold,
        wt_core::resource::ResourceHold::Free
    );
    h.wt()
        .current_dir(&repo)
        .args(["with", "alpha", "--", "true"])
        .assert()
        .success();
    let _ = blocker.kill();
    let _ = blocker.wait();
}

#[test]
fn indexed_instances_create_acquire_keep_and_destroy() {
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo(
        "pool-fixture",
        r#"
[resource.sim]
tied_to="tree"
count=2
name="sim-{{index()}}"
exists="test -f .wt/$WT_SELF"
create="touch .wt/$WT_SELF; printf create >> .wt/events"
acquire="printf ':acquire:%s' \"$WT_SELF\" >> .wt/events"
destroy="rm -f .wt/$WT_SELF"
keep="0ms"
wait="100ms"
"#,
    );
    h.register(&repo);
    let made = h.json(&[
        "new",
        "pool-fixture/work",
        "--no-sync",
        "--no-after-new",
        "--no-open",
    ]);
    let tree = PathBuf::from(made["data"]["tree"]["path"].as_str().unwrap());
    for _ in 0..2 {
        h.wt()
            .current_dir(&tree)
            .args(["--json", "with", "sim", "--", "true"])
            .assert()
            .success();
    }
    assert_eq!(
        std::fs::read_to_string(tree.join(".wt/events")).unwrap(),
        "create:acquire:sim-1:acquire:sim-1"
    );
    h.wt()
        .current_dir(&tree)
        .args(["destroy", "sim#1", "--yes", "--json"])
        .assert()
        .success();
    assert!(!tree.join(".wt/sim-1").exists());

    h.wt()
        .args(["rm", "pool-fixture/work", "--force", "--yes", "--json"])
        .assert()
        .success();
    assert!(!h
        .home
        .join("leases/tree/pool-fixture/work/sim/0.keep.json")
        .exists());
}

#[test]
fn machine_pool_declarations_agree_and_operator_sizing_wins() {
    let h = Harness::new();
    quiet(&h);
    let declaration = r#"
[resource.shared]
tied_to="machine"
count=2
name="shared-{{index()}}"
exists="test -f $WT_HOME/$WT_SELF"
create="touch $WT_HOME/$WT_SELF"
acquire="true"
destroy="rm -f $WT_HOME/$WT_SELF"
"#;
    let first = h.repo("agreement-one", declaration);
    let second = h.repo("agreement-two", declaration);
    h.register(&first);
    h.register(&second);

    common::write(
        &second.join(".wt.toml"),
        &declaration.replace("count=2", "count=3"),
    );
    h.wt()
        .args(["config", "agreement-two", "--json"])
        .assert()
        .code(5)
        .stdout(predicate::str::contains(
            "disagree on machine resource `shared` count",
        ));

    common::write(
        &h.home.join("config.toml"),
        "[session]\nbackend='none'\n[resources.shared]\ncount=4\n",
    );
    let rendered = h.json(&["config", "agreement-two"]);
    let text = rendered.to_string();
    assert!(text.contains("\"count\":4"));

    common::write(
        &second.join(".wt.toml"),
        &declaration.replace("acquire=\"true\"", "acquire=\"false\""),
    );
    h.wt()
        .args(["config", "agreement-two", "--json"])
        .assert()
        .code(5)
        .stdout(predicate::str::contains("lifecycle recipes"));
}

#[test]
fn keeps_are_hard_reservations_and_idle_destroys_free_instances() {
    let h = Harness::new();
    quiet(&h);
    let config = r#"
[resource.worker]
tied_to="repo"
count=1
name="worker-{{index()}}"
exists="test -f $WT_HOME/$WT_SELF"
create="touch $WT_HOME/$WT_SELF"
acquire="true"
destroy="rm -f $WT_HOME/$WT_SELF"
keep="2s"
idle="0ms"
wait="100ms"
"#;
    let repo = h.repo("cleanup-fixture", config);
    h.register(&repo);
    let first = h.json(&[
        "new",
        "cleanup-fixture/first",
        "--no-sync",
        "--no-after-new",
        "--no-open",
    ]);
    let second = h.json(&[
        "new",
        "cleanup-fixture/second",
        "--no-sync",
        "--no-after-new",
        "--no-open",
    ]);
    let first = PathBuf::from(first["data"]["tree"]["path"].as_str().unwrap());
    let second = PathBuf::from(second["data"]["tree"]["path"].as_str().unwrap());
    h.wt()
        .current_dir(&first)
        .args(["with", "worker", "--", "true"])
        .assert()
        .success();
    let label = wt_core::model::Label::new("cleanup-fixture").unwrap();
    let state: wt_core::lifecycle::RepoState = wt_sys::fsx::read_json(
        &h.home.join(wt_core::model::repo_state_path(&label)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        &state.resources["worker#1"].hold,
        wt_core::resource::ResourceHold::Kept { tree, .. }
            if tree == "cleanup-fixture/first"
    ));
    h.wt()
        .current_dir(&second)
        .args(["with", "worker", "--", "true"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains(
            "reserved for tree `cleanup-fixture/first`",
        ));
    assert_eq!(
        h.json(&["leases", "cleanup-fixture"])["data"]["leases"][0]["hold"],
        "kept"
    );
    h.wt()
        .current_dir(&first)
        .args(["with", "worker", "--", "true"])
        .assert()
        .success();
    std::thread::sleep(Duration::from_millis(2100));
    h.wt()
        .current_dir(&second)
        .args(["with", "worker", "--", "true"])
        .assert()
        .success();
    assert!(h.home.join("worker-1").exists());
    std::thread::sleep(Duration::from_millis(2100));
    h.json(&["config", "cleanup-fixture"]);
    wait_for_absence(&h.home.join("worker-1"));

    let no_keep = config.replace("keep=\"2s\"", "keep=\"0ms\"");
    common::write(&repo.join(".wt.toml"), &no_keep);
    common::write(&second.join(".wt.toml"), &no_keep);
    h.wt()
        .current_dir(&second)
        .args(["with", "worker", "--", "true"])
        .assert()
        .success();
    h.json(&["doctor", "cleanup-fixture"]);
    assert!(!h.home.join("worker-1").exists());
    h.wt()
        .current_dir(&second)
        .args(["with", "worker", "--", "true"])
        .assert()
        .success();
    h.json(&["prune", "cleanup-fixture"]);
    assert!(!h.home.join("worker-1").exists());
}

#[test]
fn doctor_reports_and_prune_destroys_an_instance_beyond_the_new_count() {
    let h = Harness::new();
    quiet(&h);
    let config = r#"
[resource.worker]
tied_to="repo"
count=2
name="worker-{{index()}}"
exists="test -f $WT_HOME/$WT_SELF"
create="touch $WT_HOME/$WT_SELF"
acquire="true"
destroy="rm -f $WT_HOME/$WT_SELF"
keep="0ms"
idle="1h"
wait="100ms"
"#;
    let repo = h.repo("undeclared-fixture", config);
    h.register(&repo);
    let mut first = h
        .wt_std()
        .current_dir(&repo)
        .args(["with", "worker", "--", "sleep", "2"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    h.wt()
        .current_dir(&repo)
        .args(["with", "worker", "--", "true"])
        .assert()
        .success();
    let _ = first.kill();
    let _ = first.wait();
    assert!(h.home.join("worker-2").exists());
    common::write(
        &repo.join(".wt.toml"),
        &config.replace("count=2", "count=1"),
    );
    let doctor = h.json(&["doctor", "undeclared-fixture"]);
    assert!(doctor["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "RESOURCE_UNDECLARED"));
    let pruned = h.json(&["prune", "undeclared-fixture", "--yes"]);
    assert!(pruned["data"]["items"]
        .to_string()
        .contains("undeclared-resource"));
    assert!(!h.home.join("worker-2").exists());
}

#[test]
fn register_and_idempotence_have_envelopes() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    let first = h.register(&repo);
    assert_eq!(first["command"], "register");
    assert_eq!(first["data"]["registered"], true);
    assert_eq!(first["data"]["tree"]["phase"], "ready");
    assert_eq!(first["data"]["tree"]["session_name"], "repo");
    let second = h.register(&repo);
    assert_eq!(second["data"]["registered"], false);
}

#[test]
fn phase_3_identity_and_environment_surface_are_exact() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo("repo", "");
    h.register(&repo);
    let created = h.json(&["new", "repo/feature.one", "--no-sync", "--no-open"]);
    assert_eq!(created["data"]["tree"]["session_name"], "repo/feature_one");

    let env = h.json(&["env", "repo/feature.one"])["data"]["env"]
        .as_object()
        .unwrap()
        .clone();
    for key in [
        "WT_TARGET",
        "WT_LABEL",
        "WT_NAME",
        "WT_ROOT",
        "WT_REPO",
        "WT_HOME",
        "WT_BRANCH",
        "WT_ACTIVATION",
        "WT_PATH_PREFIX",
        "WT_BIN",
    ] {
        assert!(env.contains_key(key), "missing {key}");
    }
    for key in ["WT_SESSION", "WT_NAME_SNAKE", "WT_NAME_SHORT", "WT_SLOT"] {
        assert!(!env.contains_key(key), "deleted export {key} is present");
    }

    let collision = Harness::new();
    configure_backend_none(&collision);
    let dotted = collision.repo("dotted", "");
    collision
        .wt()
        .args([
            "register",
            dotted.to_str().unwrap(),
            "--label",
            "a.b",
            "--json",
        ])
        .assert()
        .success();
    let underscored = collision.repo("underscored", "");
    let output = collision
        .wt()
        .args([
            "register",
            underscored.to_str().unwrap(),
            "--label",
            "a_b",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let refusal: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(refusal["error"]["code"], "IDENTITY_COLLISION");
}

#[test]
fn section_4_1_tree_metadata_sets_lists_unsets_and_round_trips() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo("repo", "");
    h.register(&repo);

    let created = h.json(&[
        "new",
        "repo/work",
        "--no-sync",
        "--no-open",
        "--meta",
        "ticket=ABC-123",
        "--meta",
        "owner=alice",
    ]);
    assert_eq!(
        created["data"]["tree"]["meta"],
        serde_json::json!({"owner": "alice", "ticket": "ABC-123"})
    );
    h.wt()
        .args(["meta", "repo/work"])
        .assert()
        .success()
        .stdout("owner=alice\nticket=ABC-123\n");

    let updated = h.json(&[
        "meta",
        "repo/work",
        "ticket=ABC-124",
        "reviewer=bob",
        "owner=",
    ]);
    assert_eq!(updated["data"]["target"], "repo/work");
    assert_eq!(
        updated["data"]["meta"],
        serde_json::json!({"reviewer": "bob", "ticket": "ABC-124"})
    );
    let repeated = h.json(&["meta", "repo/work", "owner="]);
    assert_eq!(repeated["data"]["meta"], updated["data"]["meta"]);

    let status = h.json(&["status", "repo/work"]);
    assert_eq!(status["data"]["meta"], updated["data"]["meta"]);
    let list = h.json(&["list"]);
    let work = list["data"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["target"] == "repo/work")
        .unwrap();
    assert_eq!(work["meta"], updated["data"]["meta"]);
    h.wt()
        .args(["ls", "--meta", "ticket"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ticket"))
        .stdout(predicate::str::contains("ABC-124"));
    h.wt()
        .args(["status", "repo/work"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "meta     reviewer=bob, ticket=ABC-124",
        ));

    let registry: wt_core::model::Registry =
        serde_json::from_str(&std::fs::read_to_string(h.home.join("registry.json")).unwrap())
            .unwrap();
    let stored = registry
        .trees
        .iter()
        .find(|tree| tree.name == "work")
        .unwrap();
    assert_eq!(
        stored.meta,
        serde_json::from_value(updated["data"]["meta"].clone()).unwrap()
    );
}

#[test]
fn edit_is_a_root_cwd_passthrough_door_with_documented_resolution() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    let port = NEXT_ISOLATED_PORT.fetch_add(32, Ordering::Relaxed);
    common::write(
        &h.home.join("config.toml"),
        &format!("editor=['sh','-c','printf \"%s|%s|%s\" \"$PWD\" \"$WT_TARGET\" \"{{{{root()}}}}\"']\n[ports]\nbase={port}\nstride=1\n[session]\nbackend='none'\n"),
    );
    h.register(&repo);
    let expected_root = std::fs::canonicalize(&repo).unwrap();
    h.wt()
        .env("VISUAL", "false")
        .env("EDITOR", "false")
        .args(["edit", "repo"])
        .assert()
        .success()
        .stdout(format!(
            "{}|repo|{}",
            expected_root.display(),
            expected_root.display()
        ));

    let output = h.wt().args(["edit", "repo", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let refusal: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(refusal["error"]["code"], "JSON_UNSUPPORTED");

    common::write(
        &h.home.join("config.toml"),
        &format!("[ports]\nbase={port}\nstride=1\n[session]\nbackend='none'\n"),
    );
    h.wt()
        .env("VISUAL", "printf visual")
        .env("EDITOR", "printf editor")
        .args(["edit", "repo"])
        .assert()
        .success()
        .stdout("visual");
    h.wt()
        .env("EDITOR", "printf editor")
        .args(["edit", "repo"])
        .assert()
        .success()
        .stdout("editor");
    h.wt()
        .env("EDITOR", "printf '{{literal}}'")
        .args(["edit", "repo"])
        .assert()
        .success()
        .stdout("{{literal}}");
    h.wt()
        .args(["edit", "repo"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("EDITOR_UNSET"))
        .stderr(predicate::str::contains("`editor` settings key"));
}

#[test]
fn adopt_records_metadata_and_a_validated_agent_for_first_open_resume() {
    let h = Harness::new();
    let port = NEXT_ISOLATED_PORT.fetch_add(32, Ordering::Relaxed);
    common::write(
        &h.home.join("config.toml"),
        &format!("[ports]\nbase={port}\nstride=1\n[session]\nbackend='tmux'\n"),
    );
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let adopted = h.root.join("adopted-with-agent");
    common::git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "adopted-with-agent",
            adopted.to_str().unwrap(),
        ],
    );
    let value = h.json(&[
        "adopt",
        adopted.to_str().unwrap(),
        "--label",
        "repo",
        "--name",
        "adopted",
        "--agent",
        "codex",
        "--meta",
        "ticket=ABC-9",
    ]);
    assert_eq!(value["data"]["tree"]["agent"], "codex");
    assert_eq!(
        value["data"]["tree"]["meta"],
        serde_json::json!({"ticket": "ABC-9"})
    );
    let opened = h.json(&["open", "repo/adopted", "--no-attach"]);
    let session = opened["data"]["sessions"][0]["name"].as_str().unwrap();
    let argv =
        std::fs::read_to_string(h.shim_state.join("tmux").join(session).join("argv")).unwrap();
    assert!(argv.lines().any(|line| line == "resume"));
    assert!(argv.lines().any(|line| line == "--last"));

    let other = h.root.join("adopted-invalid-agent");
    common::git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "adopted-invalid-agent",
            other.to_str().unwrap(),
        ],
    );
    h.wt()
        .args([
            "adopt",
            other.to_str().unwrap(),
            "--label",
            "repo",
            "--agent",
            "missing",
        ])
        .assert()
        .code(5)
        .stderr(predicate::str::contains(
            "agent `missing` is not configured",
        ));
}

#[test]
fn forget_removes_only_wt_records_and_artifacts_and_requires_consent() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo("repo", "");
    h.register(&repo);
    h.wt()
        .args(["forget", "repo", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("USE_UNREGISTER"));
    let created = h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    common::write(
        &tree.join(".wt/config.toml"),
        "[files.\"editor.local\"]\ncontent='tree={{name()}}'\n",
    );
    h.json(&["env", "repo/work"]);
    assert!(tree.join("editor.local").exists());
    assert!(std::fs::read_to_string(repo.join(".git/info/exclude"))
        .unwrap()
        .contains("/editor.local"));
    let registry_before = std::fs::read(h.home.join("registry.json")).unwrap();

    h.wt()
        .args(["forget", "repo/work"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("CONFIRM_REQUIRED"));
    assert_eq!(
        std::fs::read(h.home.join("registry.json")).unwrap(),
        registry_before
    );
    let declined = h.pty_output(&["forget", "repo/work"], b"n\n");
    assert_eq!(declined.child.code, Some(0));
    assert!(String::from_utf8_lossy(&declined.stdout).contains("Did not forget repo/work"));
    assert_eq!(
        std::fs::read(h.home.join("registry.json")).unwrap(),
        registry_before
    );

    let forgotten = h.json(&["forget", "repo/work", "--yes"]);
    assert_eq!(forgotten["data"]["forgotten"], true);
    assert!(tree.exists());
    assert!(!tree.join(".wt").exists());
    assert!(!tree.join("editor.local").exists());
    assert!(!std::fs::read_to_string(repo.join(".git/info/exclude"))
        .unwrap()
        .contains("/editor.local"));
    assert!(tree.join(".git").exists());
    assert!(common::branches(&repo).contains(&"work".to_owned()));
    let registry: wt_core::model::Registry =
        serde_json::from_slice(&std::fs::read(h.home.join("registry.json")).unwrap()).unwrap();
    assert!(!registry.trees.iter().any(|entry| entry.name == "work"));
    assert!(registry
        .tombstones
        .iter()
        .any(|entry| entry.name == "work" && entry.reason == "forgotten"));
    assert!(!h
        .home
        .join(wt_core::model::tree_state_path(
            &wt_core::model::Target::parse("repo/work").unwrap()
        ))
        .exists());
}

#[test]
fn section_4_1_tree_metadata_validation_refuses_before_registry_write() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo("repo", "");
    h.register(&repo);
    let registry_path = h.home.join("registry.json");
    let before = std::fs::read(&registry_path).unwrap();
    let oversized = format!("note={}", "x".repeat(1025));

    h.wt()
        .args(["new", "repo/bad", "--meta", "Bad=value"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("META_INVALID"))
        .stderr(predicate::str::contains("[a-z_][a-z0-9_]*"));
    h.wt()
        .args(["meta", "repo", "missing_equals"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("META_INVALID"))
        .stderr(predicate::str::contains("missing `=`"));
    h.wt()
        .args(["meta", "repo", &oversized])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("META_INVALID"))
        .stderr(predicate::str::contains("1024 bytes"));
    assert_eq!(std::fs::read(registry_path).unwrap(), before);
}

#[test]
fn env_and_exec_transport_the_same_coordinates() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let env = h.json(&["env", "repo"]);
    let expected = env["data"]["env"]["APP_PORT"].as_str().unwrap().to_owned();
    h.wt()
        .args(["exec", "repo", "--", "sh", "-c", "printf %s \"$APP_PORT\""])
        .assert()
        .success()
        .stdout(predicate::eq(expected));
}

#[test]
fn passthrough_doors_refuse_json_with_usage_exit() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let output = h
        .wt()
        .args(["exec", "repo", "--json", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "JSON_UNSUPPORTED");
    h.wt()
        .args(["exec", "repo", "--no-gate", "--", "true"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("NO_GATE_REFUSED"));
}

#[test]
fn ports_is_a_reserved_var_key() {
    let error = wt_core::config::parse("[vars]\nports='mine'", "repo/.wt.toml").unwrap_err();
    assert_eq!(error.code.0, "CONFIG_INVALID");
    assert!(error.message.contains("reserved"));
}

#[test]
fn section_5_1_user_layer_branch_convention_overrides_the_repository() {
    let h = Harness::new();
    let port = 21600;
    common::write(
        &h.home.join("config.toml"),
        &format!(
            "[ports]\nbase={port}\nstride=1\n[session]\nbackend='none'\n\n[repos.repo]\nbranch = \"mine/{{{{name()}}}}\"\n"
        ),
    );
    let repo = h.repo("repo", "branch = \"theirs/{{name()}}\"\n");
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    assert_eq!(created["data"]["tree"]["branch"], "mine/work");
}

#[test]
fn remove_resolves_bare_names_and_reports_unresolved_addresses() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);

    h.json(&["new", "repo/inside", "--no-sync"]);
    let output = h
        .wt()
        .current_dir(std::fs::canonicalize(&repo).unwrap())
        .args(["remove", "inside", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let removed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(removed["data"]["target"], "repo/inside");
    assert_eq!(removed["data"]["removed"], true);

    h.json(&["new", "repo/outside", "--no-sync"]);
    let output = h
        .wt()
        .current_dir(&h.root)
        .args(["remove", "outside", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let unresolved: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(unresolved["error"]["code"], "NOT_FOUND");
    assert_eq!(
        unresolved["error"]["details"]["candidates"],
        serde_json::json!(["repo/outside"])
    );
    assert!(unresolved["error"]["remedy"]
        .as_str()
        .unwrap()
        .contains("repo/outside"));

    let output = h
        .wt()
        .current_dir(&h.root)
        .args(["remove", "repo/unknown", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let unknown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(unknown["error"]["code"], "NOT_FOUND");
    assert_eq!(
        unknown["error"]["details"]["candidates"],
        serde_json::json!([])
    );
    h.wt()
        .current_dir(&h.root)
        .args(["remove", "repo/unknown"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("NOT_FOUND — target is not live"));
}

#[test]
fn from_resolution_fetches_prs_and_names_branch_holders() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    common::git(&repo, &["branch", "topic"]);
    common::write(&repo.join("README.md"), "remote topic\n");
    common::git(&repo, &["add", "README.md"]);
    common::git(&repo, &["commit", "-qm", "remote topic"]);
    common::git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/topic"]);
    common::git(&repo, &["reset", "--hard", "HEAD^"]);
    let local = h.json(&["new", "repo/local", "--from", "topic", "--no-sync"]);
    assert_eq!(local["notices"][0]["code"], "FROM_LOCAL_SHADOWS_REMOTE");

    h.push_pull_ref(&repo, 7, "HEAD");
    h.wt()
        .args([
            "new",
            "repo/no-fetch",
            "--from",
            "pr:7",
            "--no-fetch",
            "--no-sync",
        ])
        .assert()
        .code(3);
    let pr = h.json(&["new", "repo/pr-7", "--from", "pr:7", "--no-sync"]);
    assert_eq!(pr["data"]["tree"]["branch"], "pr/7");

    let holder = pr["data"]["tree"]["path"].as_str().unwrap();
    h.wt()
        .args(["new", "repo/duplicate", "--branch", "pr/7", "--no-sync"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains(holder));
}

#[test]
fn forge_pr_urls_use_the_matching_origin_and_refspec() {
    for (forge, url, reference) in [
        (
            "github",
            "https://github.com/o/repo.git",
            "refs/pull/8/head",
        ),
        (
            "gitlab",
            "https://gitlab.com/o/repo.git",
            "refs/merge-requests/8/head",
        ),
        (
            "bitbucket",
            "https://bitbucket.org/o/repo.git",
            "refs/pull-requests/8/from",
        ),
    ] {
        let h = Harness::new();
        let repo = h.repo("repo", BASIC);
        let origin = repo.parent().unwrap().join("repo-origin.git");
        common::git(
            &repo,
            &[
                "config",
                &format!("url.{}.insteadOf", origin.display()),
                url,
            ],
        );
        common::git(&repo, &["remote", "set-url", "origin", url]);
        h.push_ref(&repo, "HEAD", reference);
        h.register(&repo);
        // GitHub is asked for the head; a fork answer keeps the mirror path
        // this contract exercises. The other forges are not asked at all.
        h.install_gh(
            r#"{"headRefName":"topic","isCrossRepository":true,"headRepositoryOwner":{"login":"alice"}}"#,
            0,
        );
        let pr_url = match forge {
            "github" => "https://github.com/o/repo/pull/8",
            "gitlab" => "https://gitlab.com/o/repo/-/merge_requests/8",
            _ => "https://bitbucket.org/o/repo/pull-requests/8",
        };
        let report = h.json(&[
            "new",
            &format!("repo/{forge}"),
            "--from",
            pr_url,
            "--no-sync",
        ]);
        assert_eq!(report["data"]["tree"]["branch"], "pr/8");
        let notice = &report["notices"][0];
        assert_eq!(notice["code"], "PR_BRANCH_UNTRACKED", "{report}");
        let message = notice["message"].as_str().unwrap();
        if forge == "github" {
            assert!(message.contains("alice's fork"), "{message}");
            assert!(h.gh_calls().is_some());
        } else {
            assert!(message.contains("--from origin/<branch>"), "{message}");
            assert!(h.gh_calls().is_none());
        }
    }
}

#[test]
fn session_transport_uses_inner_no_gate_door_without_tmux_e() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    common::write(
        &h.home.join("config.toml"),
        "[session]\nbackend='tmux'\nagent='probe'\n[agents.probe]\nstart=['true']\nresume=['true']\n",
    );
    let opened = h.json(&["open", "repo", "--no-attach"]);
    assert_eq!(opened["data"]["sessions"][0]["created"], true);
    let argv = std::fs::read_to_string(
        h.shim_state
            .join("tmux")
            .join(opened["data"]["sessions"][0]["name"].as_str().unwrap())
            .join("argv"),
    )
    .unwrap();
    assert!(argv.contains("--no-gate"));
    assert!(!argv.lines().any(|line| line == "-e"));
    assert!(!argv.lines().any(|line| line == "true"));
    assert_eq!(
        h.json(&["close", "repo"])["data"]["sessions"][0]["closed"],
        true
    );
}

#[test]
fn shell_init_and_completions_are_script_envelopes() {
    let h = Harness::new();
    for shell in ["zsh", "bash", "fish"] {
        let init = h.json(&["shell-init", shell]);
        let script = init["data"]["script"].as_str().unwrap();
        assert!(!script.contains("wtcd"));
        assert!(!script.contains("wtsh"));
        assert!(script.contains("WT_PATH_PREFIX"));
        assert!(script.contains("WT_TARGET"));
    }
    let completions = h.json(&["completions", "fish"]);
    assert!(completions["data"]["script"]
        .as_str()
        .unwrap()
        .contains("complete -c wt"));
}

#[test]
fn old_format_is_rejected_before_current_state_is_written() {
    let h = Harness::new();
    common::write(&h.home.join("registry.toml"), "old=true");
    h.wt()
        .arg("list")
        .assert()
        .code(5)
        .stderr(predicate::str::contains("HOME_OLD_FORMAT"));
    assert!(!h.home.join("registry.json").exists());
}

#[test]
fn register_declares_the_session_backend_once() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    let registered = h.register(&repo);
    assert!(registered["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| {
            notice["code"] == "SESSION_BACKEND_SELECTED"
                && notice["message"] == "sessions: tmux 3.4 (set session.backend to change)"
        }));
    let config = wt_sys::fsx::read_string(&h.home.join("config.toml"))
        .unwrap()
        .unwrap();
    assert!(config.contains("backend = \"tmux\""));
    assert!(!config.contains("agent ="));
    let doctor = h.json(&["doctor"]);
    assert!(doctor["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "SESSION_BACKEND"
            && finding["message"] == "session backend is tmux"));

    let record = h.shim_state.join("backend-resolution.log");
    common::write_executable(
        &h.shims.join("tmux"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\ncase \"$1\" in has-session) exit 1;; *) exit 0;; esac\n",
            record.display()
        ),
    );
    h.register(&repo);
    let calls = wt_sys::fsx::read_string(&record)
        .unwrap()
        .unwrap_or_default();
    assert!(!calls.lines().any(|line| line == "-V"));

    let unavailable = Harness::new();
    common::write_executable(
        &unavailable.shims.join("tmux"),
        "#!/bin/sh\nif [ \"$1\" = -V ]; then echo 'tmux 3.1'; exit 0; fi\nexit 1\n",
    );
    let unavailable_repo = unavailable.repo("unavailable", BASIC);
    let registered = unavailable.register(&unavailable_repo);
    assert!(registered["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| { notice["message"] == "sessions: none (set session.backend to change)" }));
    let config = wt_sys::fsx::read_string(&unavailable.home.join("config.toml"))
        .unwrap()
        .unwrap();
    assert!(config.contains("backend = \"none\""));
    common::proof_capture(
        "B8",
        format!(
            "selected: tmux 3.4\nconfig: backend = \"tmux\"\nsecond -V probes: {}\nunavailable selection: none\nsession.agent present by default: false",
            calls.lines().filter(|line| *line == "-V").count()
        ),
    );
}

#[test]
fn session_verbs_resolve_a_backend_for_preexisting_homes_once() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    wt_sys::fsx::remove_path(&h.home.join("config.toml")).unwrap();

    let first = h
        .wt()
        .args(["new", "repo/work", "--no-sync", "--no-open", "--json"])
        .output()
        .unwrap();
    assert!(first.status.success());
    assert_eq!(
        String::from_utf8_lossy(&first.stderr),
        "sessions: tmux 3.4 (set session.backend to change)\n"
    );
    let config = wt_sys::fsx::read_string(&h.home.join("config.toml"))
        .unwrap()
        .unwrap();
    assert!(config.contains("backend = \"tmux\""));

    let second = h
        .wt()
        .args(["new", "repo/work", "--no-sync", "--no-open", "--json"])
        .output()
        .unwrap();
    assert!(second.status.success());
    assert!(second.stderr.is_empty());
}

#[test]
fn register_explains_how_to_rewrite_an_inline_session_table() {
    let h = Harness::new();
    common::write(
        &h.home.join("config.toml"),
        "session = { attach = false }\n",
    );
    let repo = h.repo("repo", BASIC);
    let output = h
        .wt()
        .args(["register", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&output.stderr).contains("rewrite `session = { ... }`"));
    let config = wt_sys::fsx::read_string(&h.home.join("config.toml"))
        .unwrap()
        .unwrap();
    assert_eq!(config, "session = { attach = false }\n");
}

#[test]
fn new_keeps_its_payload_when_session_creation_fails() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    common::write_executable(
        &h.shims.join("tmux"),
        "#!/bin/sh\ncase \"$1\" in has-session) exit 1;; new-session) echo unavailable >&2; exit 9;; esac\n",
    );

    let output = h
        .wt()
        .args(["new", "repo/work", "--no-sync", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["tree"]["target"], "repo/work");
    assert_eq!(envelope["data"]["tree"]["phase"], "ready");
    assert!(envelope["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| {
            notice["code"] == "SESSION_CREATE_FAILED"
                && notice["subject"] == "repo/work"
                && notice["message"]
                    .as_str()
                    .unwrap()
                    .contains("wt open repo/work")
        }));
    assert_eq!(h.json(&["status", "repo/work"])["data"]["phase"], "ready");

    let terminal = Harness::new();
    let repo = terminal.repo("repo", BASIC);
    terminal.register(&repo);
    common::write_executable(
        &terminal.shims.join("tmux"),
        "#!/bin/sh\ncase \"$1\" in has-session) exit 1;; new-session) echo unavailable >&2; exit 9;; esac\n",
    );
    let output = terminal.pty_output(&["new", "repo/work", "--no-sync"], b"");
    assert_eq!(output.child.code, Some(0));
    let transcript = String::from_utf8_lossy(&output.stdout);
    assert!(transcript.contains("Created repo/work"));
    assert!(transcript.contains("SESSION_CREATE_FAILED"));
    assert!(transcript.contains("wt open repo/work"));
}

#[test]
fn open_all_reports_each_tree_and_continues_after_a_failure() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.json(&["new", "repo/one", "--no-sync", "--no-open"]);
    h.json(&["new", "repo/two", "--no-sync", "--no-open"]);
    wt_sys::fsx::remove_path(&h.home.join("trees/repo/one")).unwrap();

    let output = h.wt().args(["open", "--all", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(5));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["exit"], 5);
    let sessions = envelope["data"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions.iter().any(|session| {
            session["target"] == "repo/one"
                && session["failed"] == true
                && session["code"] == "TREE_REPLACED"
        }),
        "{envelope}"
    );
    assert!(sessions
        .iter()
        .any(|session| { session["target"] == "repo/two" && session["created"] == true }));
    common::proof_capture(
        "B6-partial",
        serde_json::to_string_pretty(&envelope)
            .unwrap()
            .replace(&h.root.to_string_lossy().to_string(), "<ROOT>"),
    );

    let redirected = h.wt().args(["open", "--all"]).output().unwrap();
    assert_eq!(redirected.status.code(), Some(5));
    assert!(
        redirected.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&redirected.stderr)
    );
    assert!(String::from_utf8_lossy(&redirected.stdout).contains("failed (TREE_REPLACED)"));
    common::proof_capture(
        "G1",
        format!(
            "exit: {:?}\nstderr bytes: {}\nstdout:\n{}",
            redirected.status.code(),
            redirected.stderr.len(),
            String::from_utf8_lossy(&redirected.stdout)
                .replace(&h.root.to_string_lossy().to_string(), "<ROOT>")
                .trim_end()
        ),
    );
}

#[test]
fn inline_session_tables_refuse_backend_insertion_with_a_rewrite_remedy() {
    let mut refusals = Vec::new();
    for verb in ["register", "new", "open", "close"] {
        let h = Harness::new();
        let repo = h.repo("repo", BASIC);
        if verb != "register" {
            h.register(&repo);
        }
        common::write(
            &h.home.join("config.toml"),
            "session = { attach = false }\n",
        );
        let mut command = h.wt();
        match verb {
            "register" => {
                command.args(["register", repo.to_str().unwrap()]);
            }
            "new" => {
                command.args(["new", "repo/work", "--no-sync"]);
            }
            "open" | "close" => {
                command.args([verb, "repo"]);
            }
            _ => unreachable!(),
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(5), "{verb}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("rewrite `session = { ... }`"),
            "{verb}: {stderr}"
        );
        assert!(stderr.contains("[session]"), "{verb}: {stderr}");
        refusals.push(format!("{verb}:\n{}", stderr.trim_end()));
    }
    common::proof_capture("G3", refusals.join("\n"));
}

#[test]
fn non_tty_destruction_requires_confirmation() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.wt()
        .args(["unregister", "repo"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("CONFIRM_REQUIRED"));
}

#[test]
fn remove_asks_only_where_work_dies() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);

    // Clean and pushed: nothing is lost, so nothing is asked, on a terminal or
    // off one, with or without --yes.
    let clean = h.json(&["new", "repo/clean", "--no-sync"]);
    let clean_path = Path::new(clean["data"]["tree"]["path"].as_str().unwrap()).to_owned();
    h.wt().args(["remove", "repo/clean"]).assert().code(0);
    assert!(!clean_path.exists());

    // Uncommitted work: refused without a terminal, named by its own remedy.
    let dirty = h.json(&["new", "repo/dirty", "--no-sync"]);
    let dirty_path = Path::new(dirty["data"]["tree"]["path"].as_str().unwrap()).to_owned();
    common::write(&dirty_path.join("untracked"), "keep\n");
    h.wt()
        .args(["remove", "repo/dirty"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("TREE_DIRTY"));
    assert!(dirty_path.exists());

    // --yes is not consent to lose work; --force is.
    h.wt()
        .args(["remove", "repo/dirty", "--yes"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("TREE_DIRTY"));
    assert!(dirty_path.exists());
    assert_eq!(
        h.json(&["remove", "repo/dirty", "--force"])["data"]["removed"],
        true
    );
    assert!(!dirty_path.exists());
}

#[test]
fn tty_consent_accepts_and_declines_through_a_pseudoterminal() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync"]);
    let path = Path::new(created["data"]["tree"]["path"].as_str().unwrap()).to_owned();
    common::write(&path.join("untracked"), "keep\n");

    let output = h.pty_output(&["remove", "repo/work"], b"n\n");
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(shown.contains("discards uncommitted work"), "{shown}");
    assert!(shown.contains("1 file (1 untracked)"), "{shown}");
    assert!(shown.contains("Remove it? [y/N]"), "{shown}");
    assert!(shown.contains("REMOVE_DECLINED"), "{shown}");
    assert!(
        shown.contains("Removal of `repo/work` was declined; nothing changed"),
        "{shown}"
    );
    assert!(path.exists());

    let output = h.pty_output(&["remove", "repo/work", "--json"], b"n\n");
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(shown.contains("\"code\":\"REMOVE_DECLINED\""), "{shown}");
    assert!(shown.contains("\"removed\":false"), "{shown}");
    assert!(path.exists());

    assert_eq!(h.pty_status(&["remove", "repo/work"], b"y\n").code, Some(0));
    assert!(!path.exists());

    // --force answers the question in advance and never asks it.
    let second = h.json(&["new", "repo/second", "--no-sync"]);
    let second_path = Path::new(second["data"]["tree"]["path"].as_str().unwrap()).to_owned();
    common::write(&second_path.join("untracked"), "keep\n");
    assert_eq!(
        h.pty_status(&["remove", "repo/second", "--force"], b"")
            .code,
        Some(0)
    );
    assert!(!second_path.exists());
}

#[test]
fn removal_deletes_a_branch_a_remote_can_restore_and_keeps_one_it_cannot() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);

    h.json(&["new", "repo/pushed", "--no-sync"]);
    let removed = h.json(&["remove", "repo/pushed"]);
    assert_eq!(removed["data"]["branch_deleted"], true);
    assert_eq!(removed["data"]["branch_kept"], serde_json::Value::Null);
    assert!(!common::branches(&repo).contains(&"pushed".to_owned()));

    let ahead = h.json(&["new", "repo/ahead", "--no-sync"]);
    let ahead_path = Path::new(ahead["data"]["tree"]["path"].as_str().unwrap()).to_owned();
    common::write(&ahead_path.join("local.txt"), "local\n");
    common::git(&ahead_path, &["add", "-A"]);
    common::git(&ahead_path, &["commit", "-qm", "local only"]);
    let kept = h.json(&["remove", "repo/ahead"]);
    assert_eq!(kept["data"]["branch_deleted"], false);
    assert_eq!(kept["data"]["branch_kept"], "ahead");
    assert!(common::branches(&repo).contains(&"ahead".to_owned()));

    // The commits are unreachable only once the branch goes too, so that is the
    // removal that has to ask.
    let asked = h.json(&["new", "repo/asked", "--no-sync"]);
    let asked_path = Path::new(asked["data"]["tree"]["path"].as_str().unwrap()).to_owned();
    common::write(&asked_path.join("local.txt"), "local\n");
    common::git(&asked_path, &["add", "-A"]);
    common::git(&asked_path, &["commit", "-qm", "local only"]);
    h.wt()
        .args(["remove", "repo/asked", "--delete-branch"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("TREE_DIRTY"));
    assert_eq!(
        h.json(&["remove", "repo/asked", "--delete-branch", "--force"])["data"]["branch_deleted"],
        true
    );
    assert!(!common::branches(&repo).contains(&"asked".to_owned()));

    // --keep-branch keeps what the default would have deleted.
    h.json(&["new", "repo/held", "--no-sync"]);
    let held = h.json(&["remove", "repo/held", "--keep-branch"]);
    assert_eq!(held["data"]["branch_kept"], "held");
    assert!(common::branches(&repo).contains(&"held".to_owned()));
}

#[test]
fn rm_and_ls_are_accepted_spellings() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync"]);
    assert!(h.json(&["ls"])["data"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tree| tree["target"] == "repo/work"));
    let removed = h.json(&["rm", "repo/work"]);
    assert_eq!(removed["command"], "remove");
    assert_eq!(removed["data"]["removed"], true);
}

#[test]
fn prune_without_yes_is_report_only() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync"]);
    h.json(&["remove", "repo/work", "--yes"]);
    let value = h.json(&["prune"]);
    assert_eq!(value["data"]["applied"], false);
    assert_eq!(value["notices"][0]["code"], "CONFIRM_REQUIRED");
}

#[test]
fn prune_collects_tombstones_and_missing_trees() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let first = h.json(&["new", "repo/first", "--no-sync"]);
    h.json(&["remove", "repo/first", "--yes"]);
    let collected = h.json(&["prune", "--yes"]);
    assert_eq!(collected["data"]["items"][0]["action"], "collect");

    let second = h.json(&["new", "repo/second", "--no-sync"]);
    wt_sys::fsx::remove_path(Path::new(second["data"]["tree"]["path"].as_str().unwrap())).unwrap();
    let pruned = h.json(&["prune", "--yes"]);
    assert!(pruned["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["target"] == "repo/second" && item["action"] == "remove"));
    assert!(
        h.wt()
            .args(["status", "repo/second"])
            .output()
            .unwrap()
            .status
            .code()
            == Some(3)
    );
    let _ = first;
}

#[test]
fn prune_merged_and_gone_remove_only_clean_classified_trees() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let merged = h.json(&["new", "repo/merged", "--no-sync"]);
    common::write(&repo.join("README.md"), "advance\n");
    common::git(&repo, &["add", "README.md"]);
    common::git(&repo, &["commit", "-qm", "advance"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);
    let applied = h.json(&["prune", "--merged", "--yes"]);
    assert!(applied["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["target"] == "repo/merged" && item["action"] == "remove"));
    assert!(!Path::new(merged["data"]["tree"]["path"].as_str().unwrap()).exists());

    common::git(&repo, &["branch", "topic"]);
    common::git(&repo, &["push", "-qu", "origin", "topic"]);
    let gone = h.json(&["new", "repo/gone", "--from", "origin/topic", "--no-sync"]);
    common::git(&repo, &["push", "-q", "origin", "--delete", "topic"]);
    common::git(&repo, &["fetch", "-q", "--prune", "origin"]);
    let applied = h.json(&["prune", "--gone", "--yes"]);
    assert!(applied["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["target"] == "repo/gone" && item["action"] == "remove"));
    assert!(!Path::new(gone["data"]["tree"]["path"].as_str().unwrap()).exists());

    let dirty = h.json(&["new", "repo/dirty", "--no-sync"]);
    common::write(
        &Path::new(dirty["data"]["tree"]["path"].as_str().unwrap()).join("untracked"),
        "keep\n",
    );
    common::write(&repo.join("README.md"), "advance again\n");
    common::git(&repo, &["add", "README.md"]);
    common::git(&repo, &["commit", "-qm", "advance again"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);
    let report = h.json(&["prune", "--merged", "--yes"]);
    assert!(report["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["target"] == "repo/dirty" && item["action"] == "keep"));
}

#[test]
fn doctor_reports_and_prune_deletes_state_orphans() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let orphan = h.home.join("state/repo/orphan.json");
    common::write(&orphan, "{\"schema\":1}");
    assert!(h.json(&["doctor", "repo"])["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "STATE_ORPHAN"));
    h.json(&["prune", "repo", "--yes"]);
    assert!(!orphan.exists());
}

#[test]
fn register_move_to_repairs_and_rechecks_the_checkout() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let moved = h.repos.join("repo-moved");
    wt_sys::fsx::rename_path(&repo, &moved).unwrap();
    let value = h.json(&[
        "register",
        "--label",
        "repo",
        "--move-to",
        moved.to_str().unwrap(),
    ]);
    assert_eq!(
        value["data"]["path"],
        moved.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(h.json(&["status", "repo"])["data"]["phase"], "ready");
}

#[test]
fn unregister_leaves_the_checkout() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    assert_eq!(
        h.json(&["unregister", "repo", "--yes"])["data"]["unregistered"],
        true
    );
    assert!(repo.exists());
}

#[test]
fn clone_registers_the_new_checkout() {
    let h = Harness::new();
    let source = h.repo("source", BASIC);
    let destination = h.root.join("cloned");
    let cloned = h.json(&[
        "clone",
        source.to_str().unwrap(),
        "--label",
        "cloned",
        "--path",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(cloned["data"]["cloned"], true);
    assert_eq!(cloned["data"]["label"], "cloned");
}

#[test]
fn adopt_registers_an_existing_linked_worktree() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let adopted = h.root.join("adopted");
    common::git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "adopted",
            adopted.to_str().unwrap(),
        ],
    );
    let value = h.json(&[
        "adopt",
        adopted.to_str().unwrap(),
        "--label",
        "repo",
        "--name",
        "adopted",
    ]);
    assert_eq!(value["data"]["adopted"], true);
}

#[test]
fn prepare_initialises_and_release_tears_down_an_external_worktree() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
copy = [".env"]
output = [{ path = ".cache/build" }]
[hooks]
after_new = "touch \"$WT_ROOT/.after-new\""
[files."generated.txt"]
content = "managed={{name()}}"
[resource.service]
create = "touch \"$WT_ROOT/.service\""
exists = "test -f \"$WT_ROOT/.service\""
destroy = "rm -f \"$WT_ROOT/.service\""
tied_to = "tree"
"#,
    );
    common::write(&repo.join(".env"), "LOCAL=1\n");
    common::write(&repo.join(".cache/build/seed"), "warm\n");
    h.register(&repo);
    let external = h.root.join("external");
    common::git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "external",
            external.to_str().unwrap(),
        ],
    );

    let prepared = h.json(&["prepare", external.to_str().unwrap(), "--name", "desktop"]);
    assert_eq!(prepared["data"]["adopted"], true);
    assert_eq!(prepared["data"]["tree"]["target"], "repo/desktop");
    assert!(!prepared["data"]["after_new"].is_null());
    wait_for(&external.join(".after-new"));
    assert_eq!(
        std::fs::read_to_string(external.join(".env")).unwrap(),
        "LOCAL=1\n"
    );
    assert_eq!(
        std::fs::read_to_string(external.join(".cache/build/seed")).unwrap(),
        "warm\n"
    );
    assert!(external.join("generated.txt").exists());
    let prepared_again = h.json(&[
        "prepare",
        external.to_str().unwrap(),
        "--name",
        "desktop",
        "--no-after-new",
    ]);
    assert_eq!(prepared_again["data"]["adopted"], false);
    assert_eq!(prepared_again["data"]["tree"]["phase"], "ready");

    h.wt()
        .current_dir(&external)
        .args(["with", "service"])
        .assert()
        .success();
    assert!(external.join(".service").exists());
    common::write(&external.join("user.txt"), "keep\n");
    let output = h
        .wt()
        .args(["release", external.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let released: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(released["data"]["released"], true);
    assert_eq!(released["data"]["target"], "repo/desktop");
    assert!(external.exists());
    assert!(external.join("user.txt").exists());
    assert!(external.join(".after-new").exists());
    assert!(external.join(".env").exists());
    assert!(external.join(".cache/build/seed").exists());
    assert!(!external.join(".service").exists());
    assert!(external.join("generated.txt").exists());
    assert!(external.join(".wt").exists());
    assert!(!external.join(".wt/tree_id").exists());
    assert!(
        common::git_stdout(&repo, &["worktree", "list", "--porcelain"])
            .contains(external.to_str().unwrap())
    );
    let released_again: serde_json::Value = serde_json::from_slice(
        &h.wt()
            .current_dir(&external)
            .args(["release", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(released_again["data"]["released"], false);
    assert_eq!(released_again["notices"][0]["code"], "ALREADY_RELEASED");
}

#[test]
fn release_failure_keeps_the_worktree_managed_for_retry() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
[resource.daemon]
create = "touch \"$WT_ROOT/.daemon\""
exists = "test -f \"$WT_ROOT/.daemon\""
destroy = "false"
tied_to = "tree"
"#,
    );
    h.register(&repo);
    let external = h.root.join("external");
    common::git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "external",
            external.to_str().unwrap(),
        ],
    );
    h.json(&[
        "prepare",
        external.to_str().unwrap(),
        "--name",
        "desktop",
        "--no-sync",
        "--no-after-new",
    ]);
    h.wt()
        .current_dir(&external)
        .args(["with", "daemon"])
        .assert()
        .success();

    h.wt()
        .current_dir(&external)
        .args(["release", "--json"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("DESTROY_FAILED"));
    assert!(external.join(".daemon").exists());
    assert!(external.join(".wt/tree_id").exists());
    assert_eq!(
        h.json(&["status", "repo/desktop"])["data"]["phase"],
        "remove-interrupted"
    );
    assert!(
        common::git_stdout(&repo, &["worktree", "list", "--porcelain"])
            .contains(external.to_str().unwrap())
    );
}

#[test]
fn shell_refuses_json_while_open_provisions_without_attaching() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.wt()
        .args(["shell", "repo", "--json"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("JSON_UNSUPPORTED"));
    let opened = h.json(&["open", "repo"]);
    assert_eq!(opened["data"]["sessions"][0]["created"], true);
}

#[test]
fn env_sh_unsets_keys_restored_to_absence() {
    let h = Harness::new();
    let first = h.repo("first", "[env]\nFIRST_ONLY='one'\n");
    let second = h.repo("second", "");
    h.register(&first);
    h.register(&second);
    let activated = h.json(&["env", "first"]);
    let marker = serde_json::to_string(&activated["data"]["activation"]).unwrap();
    h.wt()
        .env("WT_ACTIVATION", marker)
        .env("FIRST_ONLY", "one")
        .args(["env", "second", "--sh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("unset FIRST_ONLY"));
}

#[test]
fn shell_scripts_do_not_depend_on_a_healthy_home() {
    let h = Harness::new();
    common::write(&h.home.join("registry.toml"), "old=true");
    h.wt()
        .args(["shell-init", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("WT_PATH_PREFIX"))
        .stdout(predicate::str::contains("($WT_TARGET) "))
        .stdout(predicate::str::contains("wtcd").not());
    h.wt()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_wt_targets"));
}

#[test]
fn doctor_manufactures_repository_capacity_lock_and_tooling_conditions() {
    fn finding_codes(value: &serde_json::Value) -> BTreeSet<String> {
        value["data"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|finding| finding["code"].as_str().map(str::to_owned))
            .collect()
    }

    let mut codes = BTreeSet::new();
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.json(&["new", "repo/merged", "--no-sync"]);
    common::write(&repo.join("README.md"), "advanced\n");
    common::git(&repo, &["add", "README.md"]);
    common::git(&repo, &["commit", "-qm", "advance"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);

    common::git(&repo, &["branch", "topic"]);
    common::git(&repo, &["push", "-qu", "origin", "topic"]);
    h.json(&["new", "repo/gone", "--from", "origin/topic", "--no-sync"]);
    common::git(&repo, &["push", "-q", "origin", "--delete", "topic"]);
    common::git(&repo, &["fetch", "-q", "--prune", "origin"]);
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));

    let mut child = h
        .wt_std()
        .args(["exec", "repo", "--", "sleep", "5"])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));
    let _ = child.kill();
    let _ = child.wait();

    common::write_executable(
        &h.shims.join("git"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'git version 2.30.0'; else exec /usr/bin/git \"$@\"; fi\n",
    );
    common::write_executable(
        &h.shims.join("tmux"),
        "#!/bin/sh\ncase \"$1\" in -V) echo 'tmux 3.1';; has-session) exit 1;; *) exit 0;; esac\n",
    );
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));

    let h_missing = Harness::new();
    let missing = h_missing.repo("missing", "");
    h_missing.register(&missing);
    let moved = h_missing.root.join("repo-moved-away");
    wt_sys::fsx::rename_path(&missing, &moved).unwrap();
    codes.extend(finding_codes(&h_missing.json(&["doctor", "missing"])));

    let h_capacity = Harness::new();
    common::write(
        &h_capacity.home.join("config.toml"),
        "[ports]\nbase=30000\nstride=1\n",
    );
    let capacity = h_capacity.repo("capacity", "ports=['only']\n");
    h_capacity.register(&capacity);
    codes.extend(finding_codes(&h_capacity.json(&["doctor", "capacity"])));

    for code in [
        "BRANCH_MERGED",
        "UPSTREAM_GONE",
        "TREE_IN_USE",
        "GIT_TOO_OLD",
        "REPO_PATH_MISSING",
        "PORTS_EXHAUSTED",
    ] {
        assert!(
            codes.contains(code),
            "missing manufactured doctor code {code}"
        );
    }
}

#[test]
fn doctor_condition_contracts_cover_every_documented_code() {
    let covered = BTreeSet::from([
        "STATE_ORPHAN",
        "REPO_PATH_MISSING",
        "TREE_REPLACED",
        "TREE_MISSING",
        "TREE_INCOMPLETE",
        "TREE_INTERRUPTED",
        "INIT_INTERRUPTED",
        "REMOVE_INTERRUPTED",
        "TREE_CLAIMED",
        "UNMANAGED_WORKTREE",
        "STALE_GIT_WORKTREE",
        "BRANCH_MERGED",
        "UPSTREAM_GONE",
        "RESOURCE_ORPHANED",
        "RESOURCE_GONE",
        "RESOURCE_UNDECLARED",
        "RESOURCE_PROBE_FAILED",
        "REFRESH_SKIPPED",
        "NAME_MAY_COLLIDE",
        "TREE_MISSING_PENDING",
        "GEOMETRY_CHANGED",
        "SLOT_SQUATTED",
        "PORT_SQUATTED",
        "PORTS_EXHAUSTED",
        "ADAPTER_TOOL_MISSING",
        "ACCELERATOR_INACTIVE",
        "ACCELERATOR_AVAILABLE",
        "ACCELERATOR_MISSING",
        "NO_LOCKFILE",
        "NO_ADAPTER",
        "NO_COORDINATION",
        "SESSION_BACKEND",
        "BIN_DIR_MISSING",
        "SHIM_BROKEN",
        "SHIM_SHADOWED",
        "PATH_NOT_SHADOWED",
        "PORT_BOUND",
        "EXCLUDE_MISSING",
        "EXCLUDE_REPAIRED",
        "ACTIVATION_IGNORED",
        "IDENTIFIER_LONG",
        "TREE_IN_USE",
        "GIT_TOO_OLD",
        "SHELL_INIT_MISSING",
    ]);
    assert_eq!(
        covered,
        wt_core::doctor::CODES.iter().copied().collect(),
        "every documented doctor code must have a manufactured condition contract",
    );
}

#[test]
fn doctor_reports_a_broken_path_prefix_once() {
    let h = Harness::new();
    let repo = h.repo("repo", "bin=['bin-a','bin-b']\ncommands=['orbit']\n");
    common::write(&repo.join("bin-a/.keep"), "");
    common::write(&repo.join("bin-b/.keep"), "");
    h.register(&repo);

    // Reported only from inside a door (A76): outside one the prefix is
    // expected to be absent and the finding would be noise on every label.
    let outside = h.json(&["doctor", "repo"]);
    assert!(
        !outside["data"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "PATH_NOT_SHADOWED"),
        "outside a door the missing prefix is not a finding"
    );

    let inside = h
        .wt()
        .env("WT_TARGET", "repo")
        .args(["doctor", "repo", "--json"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&inside.stdout).unwrap();
    let findings = report["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "PATH_NOT_SHADOWED")
        .count();
    assert_eq!(findings, 1);

    let door = h.json(&["env", "repo"]);
    let assembled_path = door["data"]["env"]["PATH"].as_str().unwrap();
    let healthy = h
        .wt()
        .env("PATH", assembled_path)
        .args(["doctor", "repo", "--json"])
        .output()
        .unwrap();
    assert!(healthy.status.success());
    let healthy: serde_json::Value = serde_json::from_slice(&healthy.stdout).unwrap();
    assert!(healthy["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|finding| finding["code"] != "PATH_NOT_SHADOWED"));
}

#[test]
fn doctor_accepts_a_registered_worktree_path_alias() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo("repo", "");
    h.register(&repo);
    let alias_parent = h.root.join("aliased-repos");
    wt_sys::fsx::replace_symlink(&alias_parent, &h.repos).unwrap();
    let alias = alias_parent.join("repo");
    let registry_path = h.home.join("registry.json");
    let mut registry =
        wt_sys::fsx::read_json::<wt_core::model::Registry>(&registry_path, "REGISTRY_CORRUPT")
            .unwrap()
            .unwrap();
    let alias = wt_core::model::AbsPath::new(alias.to_str().unwrap()).unwrap();
    registry.trees[0].path = alias.clone();
    registry
        .labels
        .get_mut(&wt_core::model::Label::new("repo").unwrap())
        .unwrap()
        .path = alias;
    wt_sys::fsx::write_json(&registry_path, &registry).unwrap();

    let report = h.json(&["doctor", "repo"]);
    let unmanaged = report["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "UNMANAGED_WORKTREE");
    assert!(!unmanaged);
    common::proof_capture(
        "G4",
        format!(
            "registered path uses alias: {}\nUNMANAGED_WORKTREE present: {unmanaged}",
            registry.trees[0].path.as_str() != repo.to_string_lossy()
        ),
    );
}

#[cfg(feature = "failpoints")]
#[test]
fn new_g_failpoint_resumes() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.wt()
        .env("WT_FAILPOINT", "new.G:exit")
        .args(["new", "repo/work", "--no-sync"])
        .assert()
        .code(86);
    assert_eq!(
        h.json(&["new", "repo/work", "--no-sync"])["data"]["resumed"],
        true
    );
}

#[cfg(feature = "failpoints")]
#[test]
fn render_write_failpoint_preserves_user_file_refusal() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.wt()
        .env("WT_FAILPOINT", "render.write:exit")
        .args(["register", repo.to_str().unwrap()])
        .assert()
        .code(86);
    h.wt()
        .args(["register", repo.to_str().unwrap()])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("RENDER_ONTO_USER_FILE"));
}

#[cfg(feature = "failpoints")]
#[test]
fn remove_8_failpoint_reruns_to_tombstone() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync"]);
    h.wt()
        .env("WT_FAILPOINT", "remove.8:exit")
        .args(["remove", "repo/work", "--yes"])
        .assert()
        .code(86);
    assert_eq!(
        h.json(&["remove", "repo/work", "--yes"])["data"]["removed"],
        true
    );
}

#[cfg(feature = "failpoints")]
#[test]
fn doctor_reports_a_missing_shell_guard_until_one_is_installed() {
    let h = Harness::new();
    let repo = h.repo("repo", "commands=['orbit']\nbin=['target/debug']\n");
    h.register(&repo);

    let codes = |value: &serde_json::Value| -> BTreeSet<String> {
        value["data"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|finding| finding["code"].as_str().map(str::to_owned))
            .collect()
    };
    assert!(codes(&h.json(&["doctor", "repo"])).contains("SHELL_INIT_MISSING"));

    common::write(
        &h.root.join(".zshrc"),
        "export PATH=/usr/local/bin:$PATH\neval \"$(wt shell-init zsh)\"\n",
    );
    assert!(
        !codes(&h.json(&["doctor", "repo"])).contains("SHELL_INIT_MISSING"),
        "an installed guard must silence the finding"
    );
}

/// A76: a label that claims nothing has no prefix for an rc file to displace,
/// so the finding would be advice about a problem the reader cannot have.

#[test]
fn clone_makes_a_hub_and_a_detached_canonical_that_new_branches_from() {
    let h = Harness::new();
    configure_backend_none(&h);
    let source = h.repo("source", "");
    let origin = h.repos.join("source-origin.git");
    let base = h.root.join("hub");
    let cloned = h.json(&[
        "clone",
        origin.to_str().unwrap(),
        "--label",
        "hub",
        "--path",
        base.to_str().unwrap(),
    ]);
    assert_eq!(cloned["data"]["cloned"], true);
    let hub = base.join("hub.git");
    let canonical = base.join("canonical");
    assert_eq!(
        cloned["data"]["hub"].as_str().unwrap(),
        wt_sys::fsx::canonicalize(&hub).unwrap().to_string_lossy()
    );
    assert_eq!(
        cloned["data"]["path"].as_str().unwrap(),
        wt_sys::fsx::canonicalize(&canonical)
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(
        common::git_stdout(&hub, &["rev-parse", "--is-bare-repository"]).trim(),
        "true"
    );
    // The canonical is detached at the default branch; the branch is a ref
    // nothing holds, kept at origin's tip.
    assert_eq!(cloned["data"]["tree"]["branch"], serde_json::Value::Null);
    let tip = common::git_stdout(&source, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    assert_eq!(
        common::git_stdout(&canonical, &["rev-parse", "HEAD"]).trim(),
        tip
    );
    assert_eq!(
        common::git_stdout(&hub, &["rev-parse", "refs/heads/main"]).trim(),
        tip
    );
    assert_eq!(
        common::git_stdout(&hub, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    assert_eq!(
        common::git_stdout(&hub, &["branch", "--list"])
            .trim()
            .trim_start_matches('*')
            .trim(),
        "main"
    );
    let registry: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.home.join("registry.json")).unwrap())
            .unwrap();
    assert_eq!(registry["labels"]["hub"]["owner"], "wt");
    // A second clone is idempotent.
    let again = h.json(&[
        "clone",
        origin.to_str().unwrap(),
        "--label",
        "hub",
        "--path",
        base.to_str().unwrap(),
    ]);
    assert_eq!(again["data"]["cloned"], false);
    assert_eq!(again["data"]["registered"], false);

    // No tree checks the default branch out.
    let refused = h
        .wt()
        .args(["new", "hub/main", "--no-sync", "--json"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "DEFAULT_BRANCH_RESERVED");

    // A tree branches from the local default branch after `new` brought it
    // to origin's tip, and tracks it.
    let tip2 = advance_origin(&h, "source", "two.txt");
    let created = h.json(&["new", "hub/feat", "--no-sync"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        common::git_stdout(&tree, &["rev-parse", "HEAD"]).trim(),
        tip2
    );
    assert_eq!(
        common::git_stdout(&hub, &["rev-parse", "refs/heads/main"]).trim(),
        tip2
    );
    assert_eq!(
        common::git_stdout(&tree, &["rev-parse", "--abbrev-ref", "@{u}"]).trim(),
        "main"
    );
    assert_eq!(created["data"]["tree"]["upstream"]["behind"], 0);
    // The canonical itself has not moved yet: that is the refresh's job.
    assert_eq!(
        common::git_stdout(&canonical, &["rev-parse", "HEAD"]).trim(),
        tip
    );
    let refreshed = h.json(&["anchor", "hub"]);
    assert_eq!(refreshed["data"]["moved"]["from"], tip);
    assert_eq!(refreshed["data"]["moved"]["to"], tip2);
    assert_eq!(
        common::git_stdout(&canonical, &["rev-parse", "HEAD"]).trim(),
        tip2
    );
    assert!(canonical.join("two.txt").exists());
    assert_eq!(
        common::git_stdout(&canonical, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD",
        "the canonical stays detached"
    );
    // A modified tracked file in the canonical keeps it where it is; the
    // ref still moves.
    let tip3 = advance_origin(&h, "source", "three.txt");
    common::write(&canonical.join("README.md"), "edited\n");
    let dirty = h.json(&["anchor", "hub"]);
    assert!(dirty["data"]["moved"].is_null());
    assert!(dirty["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["code"] == "ANCHOR_DIRTY"));
    assert_eq!(
        common::git_stdout(&hub, &["rev-parse", "refs/heads/main"]).trim(),
        tip3
    );
    assert_eq!(
        common::git_stdout(&canonical, &["rev-parse", "HEAD"]).trim(),
        tip2
    );
    common::git(&canonical, &["checkout", "--", "README.md"]);

    // `ls` measures trees against the local default branch.
    let listed = h.json(&["ls", "hub"]);
    let feat = listed["data"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["target"] == "hub/feat")
        .unwrap();
    assert_eq!(feat["behind_default"], 1);

    // A relative --path and a repository named by a relative path resolve
    // against the caller's directory, not the hub git runs in.
    let relative = h
        .wt()
        .current_dir(&h.root)
        .args([
            "clone",
            "repos/source-origin.git",
            "--label",
            "rel",
            "--path",
            "relative-hub",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        relative.status.success(),
        "{}",
        String::from_utf8_lossy(&relative.stdout)
    );
    assert!(h.root.join("relative-hub/hub.git").is_dir());
    assert!(h.root.join("relative-hub/canonical/README.md").exists());
    assert!(!h.root.join("relative-hub/hub.git/relative-hub").exists());

    // A remote that cannot be reached leaves no half-made hub behind, so
    // the retry the error asks for is a clean clone.
    let missing = h.root.join("nowhere.git");
    let failed = h
        .wt()
        .args([
            "clone",
            missing.to_str().unwrap(),
            "--label",
            "gone",
            "--path",
            h.root.join("gone").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(!h.root.join("gone/hub.git").exists());

    // Unregistering leaves the hub and canonical, and says where they are.
    h.json(&["rm", "hub/feat", "--yes"]);
    let unregistered = h.json(&["unregister", "hub", "--yes"]);
    let kept = unregistered["data"]["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|artifact| artifact["action"] == "kept")
        .map(|artifact| artifact["path"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        kept.iter().any(|path| path.ends_with("hub.git")),
        "{kept:?}"
    );
    assert!(
        kept.iter().any(|path| path.ends_with("canonical")),
        "{kept:?}"
    );
    assert!(hub.exists() && canonical.exists());
}

#[test]
fn section_11_2_branch_convention_names_the_branch_a_creation_does_not() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        "branch = [\"{{meta.ticket}}_{{name()}}\", \"wip/{{name()}}\"]\n",
    );
    h.register(&repo);

    let ticketed = h.json(&[
        "new",
        "repo/fix-scroll",
        "--no-sync",
        "--no-open",
        "--meta",
        "ticket=ABC-42",
    ]);
    assert_eq!(ticketed["data"]["tree"]["branch"], "ABC-42_fix-scroll");
    assert_eq!(ticketed["data"]["tree"]["name"], "fix-scroll");

    // No ticket: the next candidate decides, and nothing refuses.
    let adhoc = h.json(&["new", "repo/poke", "--no-sync", "--no-open"]);
    assert_eq!(adhoc["data"]["tree"]["branch"], "wip/poke");

    // Naming the branch outright still wins, and re-running an address is
    // idempotent against the recorded branch rather than the convention.
    let named = h.json(&[
        "new",
        "repo/audit",
        "--no-sync",
        "--no-open",
        "--branch",
        "audit-only",
        "--meta",
        "ticket=ABC-43",
    ]);
    assert_eq!(named["data"]["tree"]["branch"], "audit-only");
    let again = h.json(&[
        "new",
        "repo/fix-scroll",
        "--no-sync",
        "--no-open",
        "--meta",
        "ticket=ABC-42",
    ]);
    assert_eq!(again["data"]["created"], false);
    // The branch was decided once, so a re-run that restates nothing is the
    // same source — even after the metadata it was derived from changes.
    let bare = h.json(&["new", "repo/fix-scroll", "--no-sync", "--no-open"]);
    assert_eq!(bare["data"]["created"], false);
    assert_eq!(bare["data"]["tree"]["branch"], "ABC-42_fix-scroll");
    h.json(&["meta", "repo/fix-scroll", "ticket=ABC-99"]);
    let edited = h.json(&["new", "repo/fix-scroll", "--no-sync", "--no-open"]);
    assert_eq!(edited["data"]["created"], false);
    assert_eq!(edited["data"]["tree"]["branch"], "ABC-42_fix-scroll");

    // A metadata value git cannot spell as a branch is refused before the
    // worktree is touched.
    let output = h
        .wt()
        .args([
            "new",
            "repo/spacey",
            "--no-sync",
            "--no-open",
            "--meta",
            "ticket=ABC 44",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let refusal: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(refusal["error"]["code"], "BRANCH_INVALID");
    assert!(refusal["error"]["remedy"]
        .as_str()
        .unwrap()
        .contains("--branch"));
    assert_eq!(
        h.json(&["list"])["data"]["trees"].as_array().unwrap().len(),
        4
    );
}

#[test]
fn github_pull_requests_check_out_their_own_branch_and_track_it() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    let origin = repo.parent().unwrap().join("repo-origin.git");
    let url = "https://github.com/o/repo.git";
    common::git(
        &repo,
        &[
            "config",
            &format!("url.{}.insteadOf", origin.display()),
            url,
        ],
    );
    common::git(&repo, &["remote", "set-url", "origin", url]);
    common::write(&repo.join("README.md"), "feature\n");
    common::git(&repo, &["add", "README.md"]);
    common::git(&repo, &["commit", "-qm", "feature"]);
    common::git(
        &repo,
        &["push", "-q", "origin", "HEAD:refs/heads/feature/x"],
    );
    h.push_pull_ref(&repo, 8, "HEAD");
    common::git(&repo, &["reset", "-q", "--hard", "HEAD^"]);
    h.register(&repo);

    // gh names the branch, so the worktree is that branch and tracks it:
    // a plain `git push` from inside updates the pull request.
    h.install_gh(
        r#"{"headRefName":"feature/x","isCrossRepository":false,"headRepositoryOwner":{"login":"o"}}"#,
        0,
    );
    let report = h.json(&[
        "new",
        "repo/pr-8",
        "--from",
        "https://github.com/o/repo/pull/8",
        "--no-sync",
    ]);
    assert_eq!(report["data"]["tree"]["branch"], "feature/x");
    assert_eq!(
        report["notices"][0]["code"], "PR_BRANCH_TRACKED",
        "{report}"
    );
    let path = PathBuf::from(report["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        common::git_stdout(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/feature/x"
    );
    assert_eq!(
        common::git_stdout(&path, &["rev-parse", "HEAD"]),
        common::git_stdout(&repo, &["rev-parse", "refs/remotes/origin/feature/x"])
    );
    let (cwd, argv) = h.gh_calls().unwrap();
    assert_eq!(
        argv,
        "pr view 8 --json headRefName,isCrossRepository,headRepositoryOwner"
    );
    assert_eq!(
        std::fs::canonicalize(cwd).unwrap(),
        std::fs::canonicalize(&repo).unwrap()
    );
    // The bare `pr:N` spelling asks the same question of origin's forge,
    // and lands on the same branch: one pull request, one worktree.
    h.wt()
        .args(["new", "repo/pr-8-again", "--from", "pr:8", "--no-sync"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("feature/x"));

    // A detached checkout of the same pull request tracks nothing and is
    // told where a push has to go.
    let detached = h.json(&[
        "new",
        "repo/pr-8-detached",
        "--from",
        "pr:8",
        "--detach",
        "--no-sync",
    ]);
    assert!(detached["data"]["tree"]["branch"].is_null(), "{detached}");
    assert_eq!(detached["notices"][0]["code"], "PR_BRANCH_UNTRACKED");
    let message = detached["notices"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("this detached worktree mirrors"),
        "{message}"
    );
    assert!(
        message.contains("git push origin HEAD:feature/x"),
        "{message}"
    );

    // A local branch already named like the head is checked out as it is,
    // and the creation says both that it is behind and that it tracks
    // nothing, rather than promising a push it cannot deliver.
    common::git(
        &repo,
        &[
            "push",
            "-q",
            "origin",
            "refs/remotes/origin/feature/x:refs/heads/feature/z",
        ],
    );
    h.push_pull_ref(&repo, 6, "refs/remotes/origin/feature/x");
    common::git(&repo, &["branch", "feature/z", "HEAD"]);
    h.install_gh(
        r#"{"headRefName":"feature/z","isCrossRepository":false,"headRepositoryOwner":{"login":"o"}}"#,
        0,
    );
    let stale = h.json(&["new", "repo/pr-6", "--from", "pr:6", "--no-sync"]);
    assert_eq!(stale["data"]["tree"]["branch"], "feature/z");
    let notices = stale["notices"].as_array().unwrap();
    let codes: BTreeSet<_> = notices
        .iter()
        .map(|notice| notice["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        BTreeSet::from(["FROM_LOCAL_SHADOWS_REMOTE", "PR_BRANCH_UNTRACKED"]),
        "{stale}"
    );
    assert!(notices.iter().any(|notice| notice["message"]
        .as_str()
        .unwrap()
        .contains("git push origin HEAD:feature/z")));

    // `--branch` naming an unrelated branch that tracks its own origin
    // twin is not the pull request's branch, however well it tracks.
    common::git(&repo, &["branch", "other", "HEAD"]);
    common::git(&repo, &["push", "-q", "-u", "origin", "other"]);
    h.install_gh(
        r#"{"headRefName":"feature/x","isCrossRepository":false,"headRepositoryOwner":{"login":"o"}}"#,
        0,
    );
    let other = h.json(&[
        "new",
        "repo/pr-8-other",
        "--from",
        "pr:8",
        "--branch",
        "other",
        "--no-sync",
    ]);
    assert_eq!(other["data"]["tree"]["branch"], "other");
    let notices = other["notices"].as_array().unwrap();
    assert!(
        notices
            .iter()
            .all(|notice| notice["code"] != "PR_BRANCH_TRACKED"),
        "{other}"
    );
    assert!(
        notices
            .iter()
            .any(|notice| notice["code"] == "PR_BRANCH_UNTRACKED"
                && notice["message"]
                    .as_str()
                    .unwrap()
                    .contains("branch other tracks origin/other, which is not")
                && notice["message"]
                    .as_str()
                    .unwrap()
                    .contains("git push origin HEAD:feature/x")),
        "{other}"
    );

    // A re-run for an address that records the pull request never asks
    // gh, so a broken gh cannot fail it.
    h.install_gh("gh: command failed", 1);
    h.forget_gh_calls();
    let rerun = h.json(&[
        "new",
        "repo/pr-8",
        "--from",
        "https://github.com/o/repo/pull/8",
        "--no-sync",
    ]);
    assert_eq!(rerun["data"]["tree"]["branch"], "feature/x");
    assert!(h.gh_calls().is_none());

    // An offline re-run keeps the recorded branch without asking gh.
    h.forget_gh_calls();
    let again = h.json(&[
        "new",
        "repo/pr-8",
        "--from",
        "https://github.com/o/repo/pull/8",
        "--no-fetch",
        "--no-sync",
    ]);
    assert_eq!(again["data"]["tree"]["branch"], "feature/x");
    assert!(h.gh_calls().is_none());

    // A fork's branch is not on origin, so the PR ref is mirrored and the
    // creation says where a push would have to go instead.
    h.push_pull_ref(&repo, 7, "HEAD");
    h.install_gh(
        r#"{"headRefName":"feature/y","isCrossRepository":true,"headRepositoryOwner":{"login":"alice"}}"#,
        0,
    );
    let fork = h.json(&["new", "repo/pr-7", "--from", "pr:7", "--no-sync"]);
    assert_eq!(fork["data"]["tree"]["branch"], "pr/7");
    assert_eq!(fork["notices"][0]["code"], "PR_BRANCH_UNTRACKED", "{fork}");
    let message = fork["notices"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("branch pr/7 mirrors pull request 7"),
        "{message}"
    );
    assert!(
        message.contains("push to that fork's `feature/y` branch"),
        "{message}"
    );

    // --no-fetch on a new address mirrors the already-fetched PR ref
    // without asking gh, and names the flag that would track it.
    h.forget_gh_calls();
    let offline = h.json(&[
        "new",
        "repo/pr-7-offline",
        "--from",
        "pr:7",
        "--branch",
        "offline-7",
        "--no-fetch",
        "--no-sync",
    ]);
    assert_eq!(offline["data"]["tree"]["branch"], "offline-7");
    assert_eq!(offline["notices"][0]["code"], "PR_BRANCH_UNTRACKED");
    let message = offline["notices"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("branch offline-7 mirrors pull request 7"),
        "{message}"
    );
    assert!(message.contains("retry without --no-fetch"), "{message}");
    assert!(h.gh_calls().is_none());

    // gh refusing is the creation refusing, with gh's reason and a way in.
    h.install_gh(
        "To get started with GitHub CLI, please run:  gh auth login",
        4,
    );
    h.wt()
        .args(["new", "repo/pr-9", "--from", "pr:9", "--no-sync"])
        .assert()
        .code(7)
        .stderr(predicate::str::contains(
            "gh could not look up pull request 9",
        ))
        .stderr(predicate::str::contains("run `gh auth login`"));
    h.install_gh("GraphQL: Could not resolve to a PullRequest", 1);
    h.wt()
        .args(["new", "repo/pr-9", "--from", "pr:9", "--no-sync"])
        .assert()
        .code(7)
        .stderr(predicate::str::contains("Could not resolve"))
        .stderr(predicate::str::contains("gh auth status"));
    assert!(h.json(&["ls"])["data"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .all(|tree| tree["name"] != "pr-9"));
}

#[test]
fn copy_populates_declared_paths_and_refuses_tracked_sources() {
    let copied = Harness::new();
    let repo = copied.repo("repo", "copy=['.env']\n");
    common::write(&repo.join(".env"), "TOKEN=local-only\n");
    copied.register(&repo);
    let created = copied.json(&["new", "repo/work", "--no-sync", "--no-after-new"]);
    let root = Path::new(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        wt_sys::fsx::read_string(&root.join(".env")).unwrap(),
        Some("TOKEN=local-only\n".to_owned())
    );

    let absent = Harness::new();
    let repo = absent.repo("repo", "copy=['missing.secret']\n");
    absent.register(&repo);
    let created = absent.json(&["new", "repo/work", "--no-sync"]);
    assert!(created["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["code"] == "COPY_ABSENT"));

    let tracked = Harness::new();
    let repo = tracked.repo("repo", "copy=['README.md']\n");
    tracked.register(&repo);
    tracked
        .wt()
        .args(["new", "repo/work", "--no-sync", "--json"])
        .assert()
        .code(5)
        .stdout(predicate::str::contains("COPY_TRACKED"));
    assert_eq!(
        tracked.json(&["status", "repo/work"])["data"]["phase"],
        "incomplete"
    );
}

#[test]
#[allow(clippy::disallowed_types)]
fn cargo_sets_no_build_directory_and_seeds_a_tree_from_the_canonical_build() {
    let h = Harness::new();
    common::write_executable(&h.shims.join("cargo"), "#!/bin/sh\nexit 0\n");
    let repo = h.repo("repo", "");
    common::write(
        &repo.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\nedition='2021'\n",
    );
    common::write(&repo.join("Cargo.lock"), "# generated fixture lockfile\n");
    common::git(&repo, &["add", "Cargo.toml", "Cargo.lock"]);
    common::git(&repo, &["commit", "-qm", "add cargo fixture"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);
    h.register(&repo);
    // A built canonical: compiled dependencies, incremental state, and the
    // uplifted binary — the whole output root travels with the snapshot.
    let rlib = "target/debug/deps/libfixture-0123456789abcdef.rlib";
    common::write(&repo.join(rlib), "rlib\n");
    common::write(
        &repo.join("target/debug/.fingerprint/fixture-0123456789abcdef/lib-fixture"),
        "fingerprint\n",
    );
    common::write(&repo.join("target/debug/incremental/fixture-abc/s-1/x"), "");
    common::write(&repo.join("target/debug/fixture"), "binary\n");
    // A source file whose modification time is what cargo's freshness rule
    // reads, and one with uncommitted changes the snapshot must not carry.
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_876_400);
    std::fs::File::options()
        .write(true)
        .open(repo.join("Cargo.toml"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    common::write(&repo.join("Cargo.lock"), "# edited, not committed\n");

    let created = h.json(&["new", "repo/work", "--no-sync", "--no-after-new"]);
    let canonical_env = h.json(&["env", "repo"])["data"]["env"].clone();
    let linked_env = h.json(&["env", "repo/work"])["data"]["env"].clone();
    // The whole build lives in the tree's own `target/`: no cargo directory
    // variable of any kind, so a plain `cargo build` anywhere does the right
    // thing and no two trees can address one another's units.
    for env in [&canonical_env, &linked_env] {
        assert!(env.get("CARGO_BUILD_BUILD_DIR").is_none());
        assert!(env.get("CARGO_TARGET_DIR").is_none());
    }
    let linked = PathBuf::from(linked_env["WT_ROOT"].as_str().unwrap());
    let codes = created["notices"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|notice| notice["code"].as_str())
        .collect::<Vec<_>>();
    // Whatever the filesystem, the tree is the committed checkout: the
    // canonical's uncommitted edit stays there.
    assert_eq!(
        std::fs::read_to_string(linked.join("Cargo.lock")).unwrap(),
        "# generated fixture lockfile\n"
    );
    assert!(
        common::git_stdout(&linked, &["status", "--porcelain", "--untracked-files=no"]).is_empty()
    );
    let seeded = if cfg!(target_os = "macos") {
        assert!(codes.contains(&"SNAPSHOT_CLONED"), "{codes:?}");
        assert_eq!(
            std::fs::read_to_string(linked.join(rlib)).unwrap(),
            "rlib\n"
        );
        assert!(linked
            .join("target/debug/.fingerprint/fixture-0123456789abcdef/lib-fixture")
            .exists());
        assert!(linked
            .join("target/debug/incremental/fixture-abc/s-1/x")
            .exists());
        assert!(linked.join("target/debug/fixture").exists());
        // A clean tracked file keeps the canonical's modification time, so
        // cargo finds the snapshot's units as fresh as the canonical's.
        assert_eq!(
            std::fs::metadata(linked.join("Cargo.toml"))
                .unwrap()
                .modified()
                .unwrap(),
            old
        );
        // Copy-on-write: a rebuild in the tree leaves the canonical as it was.
        common::write(&linked.join(rlib), "rebuilt\n");
        assert_eq!(std::fs::read_to_string(repo.join(rlib)).unwrap(), "rlib\n");
        true
    } else {
        assert!(codes.contains(&"SNAPSHOT_SKIPPED"), "{codes:?}");
        assert!(!linked.join("target").exists());
        false
    };
    // Sizing follows the build output into the tree.
    let listed = h.json(&["list", "repo", "--disk"]);
    let work = listed["data"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["target"] == "repo/work")
        .unwrap()
        .clone();
    if seeded {
        assert!(work["build_kb"].as_u64().unwrap() > 0);
    } else {
        assert!(work["build_kb"].is_null());
    }

    let synced = h.json(&["sync", "repo/work"]);
    let inputs = synced["data"]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|input| input["path"].as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(inputs, BTreeSet::from(["Cargo.lock", "Cargo.toml"]));

    common::proof_capture(
        "D1",
        format!(
            "CARGO_BUILD_BUILD_DIR set: false\nCARGO_TARGET_DIR set: false\nseeded from canonical: {seeded}\nlinked target: {}\nsync inputs: {}",
            linked.join("target").display(),
            inputs.iter().copied().collect::<Vec<_>>().join(", ")
        ),
    );
}

#[test]
fn doctor_manufactures_git_environment_port_and_adapter_conditions() {
    fn finding_codes(value: &serde_json::Value) -> BTreeSet<String> {
        value["data"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|finding| finding["code"].as_str().map(str::to_owned))
            .collect()
    }

    let mut codes = BTreeSet::new();
    let h = Harness::new();
    let repo = h.repo("repo", "ports=['http']\nbin=['missing-bin']\n");
    let registered = h.register(&repo);
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));
    wt_sys::fsx::create_private_dir(&repo.join("missing-bin")).unwrap();
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));

    // PATH_NOT_SHADOWED is only observable from inside a door (A76), and only
    // where a bin directory or a claimed command gives it a prefix to miss.
    let inside_door = h
        .wt()
        .env("WT_TARGET", "repo")
        .args(["doctor", "repo", "--json"])
        .output()
        .unwrap();
    codes.extend(finding_codes(
        &serde_json::from_slice(&inside_door.stdout).unwrap(),
    ));

    let exclude = repo.join(".git/info/exclude");
    common::write(&exclude, "");
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));
    common::write(&exclude, "# >>> wt managed >>>\n/.wt/\n");
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));

    let invalid_activation = h
        .wt()
        .env("WT_ACTIVATION", "not-json")
        .args(["doctor", "repo", "--json"])
        .output()
        .unwrap();
    assert!(invalid_activation.status.success());
    codes.extend(finding_codes(
        &serde_json::from_slice(&invalid_activation.stdout).unwrap(),
    ));

    common::write(
        &h.home.join("config.toml"),
        "[ports]\nbase=21000\nstride=16\n",
    );
    let port = registered["data"]["tree"]["ports"][0]["port"]
        .as_u64()
        .unwrap() as u16;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));
    drop(listener);

    let unmanaged = h.root.join("unmanaged");
    common::git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "unmanaged",
            unmanaged.to_str().unwrap(),
        ],
    );
    let stale = h.root.join("stale");
    common::git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "stale",
            stale.to_str().unwrap(),
        ],
    );
    wt_sys::fsx::remove_path(&stale).unwrap();
    codes.extend(finding_codes(&h.json(&["doctor", "repo"])));

    let h_empty = Harness::new();
    let empty = h_empty.repo("empty", "");
    h_empty.register(&empty);
    codes.extend(finding_codes(&h_empty.json(&["doctor", "empty"])));

    let h_rust = Harness::new();
    let rust = h_rust.repo("rust", "");
    common::write(
        &rust.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    );
    h_rust.register(&rust);
    codes.extend(finding_codes(&h_rust.json(&["doctor", "rust"])));
    common::write_executable(&h_rust.shims.join("cargo"), "#!/bin/sh\nexit 0\n");
    codes.extend(finding_codes(&h_rust.json(&["doctor", "rust"])));

    let h_node = Harness::new();
    let node = h_node.repo("node", "");
    common::write(&node.join("package.json"), "{\"name\":\"fixture\"}\n");
    common::write(&node.join("package-lock.json"), "{}\n");
    common::write_executable(&h_node.shims.join("npm"), "#!/bin/sh\nexit 0\n");
    h_node.register(&node);
    // The nudged tool absent, then installed.
    codes.extend(finding_codes(&h_node.json(&["doctor", "node"])));
    common::write_executable(&h_node.shims.join("pnpm"), "#!/bin/sh\nexit 0\n");
    codes.extend(finding_codes(&h_node.json(&["doctor", "node"])));

    for code in [
        "BIN_DIR_MISSING",
        "PATH_NOT_SHADOWED",
        "EXCLUDE_MISSING",
        "EXCLUDE_REPAIRED",
        "ACTIVATION_IGNORED",
        "GEOMETRY_CHANGED",
        "SLOT_SQUATTED",
        "PORT_SQUATTED",
        "PORT_BOUND",
        "UNMANAGED_WORKTREE",
        "STALE_GIT_WORKTREE",
        "NO_ADAPTER",
        "NO_COORDINATION",
        "ADAPTER_TOOL_MISSING",
        "ACCELERATOR_MISSING",
        "ACCELERATOR_AVAILABLE",
        "NO_LOCKFILE",
    ] {
        assert!(
            codes.contains(code),
            "missing manufactured doctor code {code}"
        );
    }
}

#[test]
fn setup_refuses_json_and_a_terminal_free_invocation() {
    let h = Harness::new();

    let json = h.wt().args(["setup", "--json"]).output().unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "JSON_UNSUPPORTED");
    assert_eq!(json.status.code(), Some(2));

    // Tests never have a terminal on stdin, which is exactly the condition.
    let plain = h.wt().arg("setup").output().unwrap();
    assert_eq!(plain.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&plain.stderr);
    assert!(stderr.contains("CONFIRM_REQUIRED"), "{stderr}");
    assert!(
        stderr.contains("wt register") && stderr.contains("--dry-run"),
        "the remedy must name what an agent can run instead: {stderr}"
    );

    let dry = h
        .wt()
        .args(["setup", "--dry-run", "--shell", "zsh"])
        .output()
        .unwrap();
    assert_eq!(
        dry.status.code(),
        Some(0),
        "a dry run asks nothing and needs no terminal: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
}

#[test]
fn register_repair_restores_only_a_replaced_canonical_checkout() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[files.'.wt/generated']\ncontent='restored {{target()}}'\n[resource.service]\ncreate='true'\nexists='false'\ndestroy='true'\ntied_to='tree'\n",
    );
    let registered = h.register(&repo);
    let tree_id = registered["data"]["tree"]["tree_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let registry_before = wt_sys::fsx::read_string(&h.home.join("registry.json"))
        .unwrap()
        .unwrap();
    let target = wt_core::model::Target::canonical(wt_core::model::Label::new("repo").unwrap());
    let state_path = h.home.join(wt_core::model::tree_state_path(&target));
    let state_before =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();

    wt_sys::fsx::remove_path(&repo.join(".wt/tree_id")).unwrap();
    wt_sys::fsx::remove_path(&repo.join(".wt/generated")).unwrap();
    let doctor = h.json(&["doctor", "repo"]);
    let replaced = doctor["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "TREE_REPLACED")
        .unwrap();
    assert!(replaced["remedy"].as_str().unwrap().contains("wt register"));
    assert!(replaced["remedy"].as_str().unwrap().contains("--repair"));

    // Reaching the same condition through an ordinary command must point at
    // the same escape. `remove` and `adopt` both refuse a canonical tree, so
    // naming them here would send the reader to two dead ends.
    let refused = h.wt().args(["env", "repo"]).output().unwrap();
    assert_eq!(refused.status.code(), Some(5));
    let remedy = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(remedy.contains("--repair"), "door remedy was: {remedy}");
    assert!(!remedy.contains("wt adopt"), "door remedy was: {remedy}");

    let other = h.repo("other", "");
    h.wt()
        .args([
            "register",
            other.to_str().unwrap(),
            "--label",
            "repo",
            "--repair",
            "--json",
        ])
        .assert()
        .code(5)
        .stdout(predicate::str::contains("REPAIR_REFUSED"))
        .stdout(predicate::str::contains(
            "not repo's registered canonical checkout",
        ));

    let repaired = h.json(&[
        "register",
        repo.to_str().unwrap(),
        "--label",
        "repo",
        "--repair",
    ]);
    assert_eq!(repaired["data"]["registered"], false);
    assert_eq!(
        wt_sys::fsx::read_string(&repo.join(".wt/tree_id")).unwrap(),
        Some(format!("{tree_id}\n"))
    );
    assert_eq!(
        wt_sys::fsx::read_string(&repo.join(".wt/generated")).unwrap(),
        Some("# generated by wt for repo. If you edit this file, wt stops re-rendering it; delete it to let wt regenerate it, or set files.\".wt/generated\" = false in .wt/config.toml\nrestored repo".to_owned())
    );
    assert_eq!(
        wt_sys::fsx::read_string(&h.home.join("registry.json"))
            .unwrap()
            .unwrap(),
        registry_before
    );
    let state_after =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    assert_eq!(state_after.tree_id, state_before.tree_id);
    assert_eq!(state_after.phase, state_before.phase);
    assert_eq!(state_after.sync, state_before.sync);
    assert_eq!(state_after.resources, state_before.resources);

    h.wt()
        .args([
            "register",
            repo.to_str().unwrap(),
            "--label",
            "repo",
            "--repair",
            "--json",
        ])
        .assert()
        .code(5)
        .stdout(predicate::str::contains("is not replaced"));
}

#[test]
fn ready_exec_stays_inside_the_door_subprocess_budget() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    let registered = h.register(&repo);
    let trace = h.root.join("spawn.jsonl");
    let lock_trace = h.root.join("locks.jsonl");
    let budget_trace = h.root.join("budget.jsonl");
    let label = registered["data"]["tree"]["label"].as_str().unwrap();
    let name = registered["data"]["tree"]["name"].as_str().unwrap();
    let state = h.home.join(format!("state/{label}/{name}.json"));
    let state_before = std::fs::read(&state).unwrap();
    h.wt()
        .env("WT_SPAWN_TRACE", &trace)
        .env("WT_LOCK_TRACE_FILE", &lock_trace)
        .env("WT_BUDGET_TRACE", &budget_trace)
        .args(["exec", "repo", "--", "true"])
        .assert()
        .success();
    let records = std::fs::read_to_string(trace).unwrap();
    let values = records
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        values
            .iter()
            .filter(|value| value["program"] == "git")
            .count(),
        1
    );
    assert_eq!(values.len(), 2, "one git query plus the requested child");
    let locks = std::fs::read_to_string(lock_trace).unwrap();
    let locks = locks
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(locks.len(), 2, "tree shared plus the door-holder flock");
    assert_eq!(locks[0]["mode"], "shared");
    assert_eq!(locks[1]["mode"], "exclusive");
    let budget = std::fs::read_to_string(budget_trace).unwrap();
    let kinds = budget
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "registry_read",
            "identity_read",
            "identity_read",
            "state_read",
            "render_hash_compare",
        ]
    );
    assert_eq!(std::fs::read(state).unwrap(), state_before);
}

#[test]
fn sync_and_which_have_stable_data_shapes() {
    let h = Harness::new();
    let repo = h.repo("repo", "sync_inputs=['README.md']\n");
    h.register(&repo);
    let synced = h.json(&["sync", "repo"]);
    assert_eq!(synced["data"]["ran"], false);
    assert!(synced["data"]["steps"].as_array().unwrap().is_empty());
    let which = h.json(&["which", "repo", "true"]);
    assert!(which["data"]["path"].as_str().is_some());
}

#[test]
fn sync_without_adapter_work_never_bricks_the_canonical_tree() {
    let h = Harness::new();
    let repo = h.repo("repo", "");
    h.register(&repo);
    h.wt().args(["sync", "repo", "--json"]).assert().success();
    assert_eq!(h.json(&["status", "repo"])["data"]["phase"], "ready");
    h.wt()
        .args(["exec", "repo", "--", "true"])
        .assert()
        .success();
}

#[test]
fn prune_records_then_new_creates_a_fresh_incarnation() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
[resource.service]
tied_to="tree"
exists="test -f .wt/service"
create="touch .wt/service"
destroy="rm -f .wt/service"
"#,
    );
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    h.wt()
        .current_dir(&tree)
        .args(["with", "service", "--json"])
        .assert()
        .success();
    let old_id = created["data"]["tree"]["tree_id"].as_str().unwrap();
    let path = created["data"]["tree"]["path"].as_str().unwrap();
    wt_sys::fsx::remove_path(Path::new(path)).unwrap();
    let pruned = h.json(&["prune", "--records", "repo/work", "--yes"]);
    assert_eq!(pruned["data"]["items"][0]["result"]["remaining"], 0);
    let recreated = h.json(&["new", "repo/work", "--no-sync"]);
    assert_ne!(recreated["data"]["tree"]["tree_id"], old_id);
}

#[test]
fn sync_rechecks_tracked_render_paths_even_when_they_have_records() {
    let h = Harness::new();
    let repo = h.repo("repo", "[files.'.wt/generated']\ncontent='generated'\n");
    h.register(&repo);
    common::git(&repo, &["add", "-f", ".wt/generated"]);
    common::git(&repo, &["commit", "-qm", "track generated path"]);
    h.wt()
        .args(["sync", "repo", "--json"])
        .assert()
        .code(5)
        .stdout(predicate::str::contains("RENDER_ONTO_TRACKED"));
    assert_eq!(h.json(&["status", "repo"])["data"]["phase"], "failed");
}

#[test]
fn missing_bins_are_silent_at_doors_and_reported_by_doctor() {
    let h = Harness::new();
    let repo = h.repo("repo", "bin=['missing-bin']\n");
    h.register(&repo);
    h.wt()
        .args(["exec", "repo", "--", "true"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
    assert!(h.json(&["env", "repo"])["notices"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(h.json(&["doctor", "repo"])["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "BIN_DIR_MISSING"));
}

#[test]
fn doctor_stays_quiet_about_the_shell_guard_when_nothing_is_claimed() {
    let h = Harness::new();
    let repo = h.repo("plain", "");
    h.register(&repo);
    let report = h.json(&["doctor", "plain"]);
    assert!(!report["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "SHELL_INIT_MISSING"));
}

#[test]
fn new_brings_the_local_default_branch_to_origin_before_branching() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo("repo", "");
    h.register(&repo);

    // A clean canonical on the default branch is fast-forwarded in place.
    let tip = advance_origin(&h, "repo", "one.txt");
    let created = h.json(&["new", "repo/a", "--no-sync"]);
    let tree_a = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        common::git_stdout(&tree_a, &["rev-parse", "HEAD"]).trim(),
        tip
    );
    assert_eq!(
        common::git_stdout(&repo, &["rev-parse", "HEAD"]).trim(),
        tip
    );
    assert!(repo.join("one.txt").exists());
    assert_eq!(
        common::git_stdout(&tree_a, &["rev-parse", "--abbrev-ref", "@{u}"]).trim(),
        "main"
    );

    // Local changes in the canonical: it stays, and the tree starts from
    // origin's tip instead, saying so.
    let tip2 = advance_origin(&h, "repo", "two.txt");
    common::write(&repo.join("README.md"), "edited\n");
    let created = h.json(&["new", "repo/b", "--no-sync"]);
    let tree_b = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        common::git_stdout(&tree_b, &["rev-parse", "HEAD"]).trim(),
        tip2
    );
    assert_eq!(
        common::git_stdout(&repo, &["rev-parse", "HEAD"]).trim(),
        tip
    );
    assert!(created["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["code"] == "DEFAULT_NOT_UPDATED"));
    common::git(&repo, &["checkout", "--", "README.md"]);

    // Local commits origin lacks are part of the start, never reset.
    common::git(&repo, &["merge", "-q", "--ff-only", "origin/main"]);
    common::write(&repo.join("local.txt"), "local\n");
    common::git(&repo, &["add", "-A"]);
    common::git(&repo, &["commit", "-qm", "local only"]);
    let local = common::git_stdout(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let created = h.json(&["new", "repo/c", "--no-sync"]);
    let tree_c = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        common::git_stdout(&tree_c, &["rev-parse", "HEAD"]).trim(),
        local
    );
    assert!(created["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["code"] == "DEFAULT_DIVERGED"));
    assert_eq!(
        common::git_stdout(&repo, &["rev-parse", "HEAD"]).trim(),
        local
    );

    // `--no-fetch` branches from the local default branch as it stands.
    let created = h.json(&["new", "repo/d", "--no-sync", "--no-fetch"]);
    let tree_d = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        common::git_stdout(&tree_d, &["rev-parse", "HEAD"]).trim(),
        local
    );
}

#[test]
fn a_door_that_became_its_child_can_open_another_door() {
    // `wt exec` replaces itself with the child and hands it the door's lock
    // fds, so a verb reached through an exec chain runs under the pid of the
    // door that spawned it. Its own door entry must not collide with the one
    // it inherited, and the inherited entry, which no token ever drops, is
    // swept by the next door once its process is gone.
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo("door-fixture", "[resource.slot]\ncount=1\nwait='500ms'\n");
    h.register(&repo);
    let doors = h.home.join("locks/door-fixture/canonical.doors");
    let output = h
        .wt()
        .current_dir(&repo)
        .args([
            "exec",
            "door-fixture",
            "--",
            env!("CARGO_BIN_EXE_wt"),
            "with",
            "slot",
            "--",
            "sh",
            "-c",
            "printf '%s' \"$WT_LEASE_SLOT\"",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("slot-1"));
    let inherited = wt_sys::fsx::read_dir_paths(&doors).unwrap();
    assert_eq!(
        inherited.len(),
        1,
        "only the exec'd door's own entry outlives the chain: {inherited:?}"
    );
    h.wt()
        .args(["exec", "door-fixture", "--", "true"])
        .assert()
        .success();
    let after = wt_sys::fsx::read_dir_paths(&doors).unwrap();
    assert!(
        !after.contains(&inherited[0]),
        "the next door sweeps the dead entry: {after:?}"
    );
    assert_eq!(after.len(), 1, "{after:?}");
}

#[test]
fn machine_recipes_run_from_the_invoking_checkout() {
    // A machine pool's recipes are the repository's own scripts. They run
    // from the checkout whose door invoked them, and a frozen instance
    // reached without a door, as prune does, still finds a checkout.
    let h = Harness::new();
    quiet(&h);
    let repo = h.repo(
        "gadget-fixture",
        "[resource.gadget]\ntied_to=\"machine\"\nname=\"gadget-fixture\"\nexists=\"test -f gadget.ready\"\ncreate=\"sh scripts/make-gadget.sh\"\ndestroy=\"rm -f gadget.ready\"\n",
    );
    common::write(
        &repo.join("scripts/make-gadget.sh"),
        "#!/bin/sh\ntouch gadget.ready\n",
    );
    common::git(&repo, &["add", "-A"]);
    common::git(&repo, &["commit", "-qm", "the gadget script"]);
    h.register(&repo);
    let made = h.json(&[
        "new",
        "gadget-fixture/work",
        "--no-sync",
        "--no-after-new",
        "--no-open",
    ]);
    let tree = PathBuf::from(made["data"]["tree"]["path"].as_str().unwrap());
    h.wt()
        .current_dir(&tree)
        .args(["with", "gadget", "--json"])
        .assert()
        .success();
    assert!(
        tree.join("gadget.ready").exists(),
        "the create recipe ran from the invoking tree"
    );
    assert!(!repo.join("gadget.ready").exists());
    h.wt()
        .args(["rm", "gadget-fixture/work", "--force", "--json"])
        .assert()
        .success();
    // The creating tree is gone; destroy falls back to the registered
    // checkout rather than failing to find a directory.
    h.wt()
        .args(["destroy", "gadget", "gadget-fixture", "--yes", "--json"])
        .assert()
        .success();
}
