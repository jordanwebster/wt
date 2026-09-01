mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};

use common::Harness;
use predicates::prelude::*;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const BASIC: &str = r#"
ports = ["http"]
[env]
APP_PORT = "{{ports.http}}"
[files.".wt/generated"]
content = "port={{ports.http}}"
[task.hello]
run = "printf hello"
"#;

static NEXT_ISOLATED_PORT: AtomicU16 = AtomicU16::new(40_000);

fn configure_backend_none(h: &Harness) {
    let port = NEXT_ISOLATED_PORT.fetch_add(32, Ordering::Relaxed);
    common::write(
        &h.home.join("config.toml"),
        &format!("[ports]\nbase={port}\nstride=1\n[session]\nbackend='none'\n"),
    );
}

fn wait_for_text(path: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if wt_sys::fsx::read_string(path).unwrap().as_deref() == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} never became {expected:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

const RESOURCE: &str = r#"
[task.service]
run = "touch \"$WT_ROOT/.service\""
exists = "test -f \"$WT_ROOT/.service\""
destroy = "rm -f \"$WT_ROOT/.service\""
tied_to = "tree"
"#;

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
fn truth_and_inspection_verbs_match_the_registered_tree() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let list = h.json(&["ls"]);
    assert_eq!(list["command"], "list");
    assert_eq!(list["data"]["trees"][0]["target"], "repo");
    assert_eq!(h.json(&["status", "repo"])["data"]["phase"], "ready");
    assert_eq!(
        h.json(&["path", "repo"])["data"]["path"],
        std::fs::canonicalize(repo)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert!(h.json(&["tasks", "repo"])["data"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|task| task["id"] == "hello"));
    assert_eq!(h.json(&["config", "repo"])["data"]["target"], "repo");
    assert_eq!(h.json(&["locks", "repo"])["data"]["locks"][0]["level"], 1);
    assert_eq!(h.json(&["doctor", "repo"])["command"], "doctor");
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
fn forget_refuses_resources_sessions_and_door_holders_with_specific_remedies() {
    let resources = Harness::new();
    configure_backend_none(&resources);
    let repo = resources.repo("repo", RESOURCE);
    resources.register(&repo);
    resources.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    // Declaring a resource is not creating one: the tree that has only ever
    // been declared at is forgettable, and every tree of a repo with resources
    // carries such records.
    resources.json(&["new", "repo/declared", "--no-sync", "--no-open"]);
    let declared = resources.json(&["forget", "repo/declared", "--yes"]);
    assert_eq!(declared["data"]["forgotten"], true);

    resources.json(&["run", "service", "repo/work"]);
    resources
        .wt()
        .args(["forget", "repo/work", "--yes"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("RESOURCES_EXIST"))
        .stderr(predicate::str::contains("service"))
        .stderr(predicate::str::contains("wt destroy"))
        .stderr(predicate::str::contains("wt rm"));
    // And once it is torn down the refusal lifts.
    resources.json(&["destroy", "service", "repo/work", "--yes"]);
    let destroyed = resources.json(&["forget", "repo/work", "--yes"]);
    assert_eq!(destroyed["data"]["forgotten"], true);

    let sessions = Harness::new();
    let port = NEXT_ISOLATED_PORT.fetch_add(32, Ordering::Relaxed);
    common::write(
        &sessions.home.join("config.toml"),
        &format!("[ports]\nbase={port}\nstride=1\n[session]\nbackend='tmux'\n"),
    );
    let repo = sessions.repo("repo", BASIC);
    sessions.register(&repo);
    sessions.json(&["new", "repo/work", "--no-sync", "--no-attach"]);
    sessions
        .wt()
        .args(["forget", "repo/work", "--yes"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("SESSION_LIVE"))
        .stderr(predicate::str::contains("wt close"));

    let holders = Harness::new();
    configure_backend_none(&holders);
    let repo = holders.repo("repo", BASIC);
    holders.register(&repo);
    holders.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    let mut child = holders
        .wt_std()
        .args(["exec", "repo/work", "--", "sleep", "5"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    holders
        .wt()
        .args(["forget", "repo/work", "--yes"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("TREE_IN_USE"))
        .stderr(predicate::str::contains("wait for the door holders"));
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn top_level_help_uses_intent_groups_and_primary_short_spellings() {
    let h = Harness::new();
    let output = h.wt().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    let headings = ["Everyday:", "Setup:", "Working inside a tree:", "Upkeep:"];
    let positions = headings.map(|heading| help.find(heading).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(help.contains("ls           List registered trees [aliases: list]"));
    assert!(help.contains("rm           Tear down and remove a linked tree [aliases: remove]"));
    for alias in ["test", "lint", "fmt", "build"] {
        assert!(help.contains(&format!("  {alias}")));
    }
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
fn run_json_keeps_one_envelope_on_stdout() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let output = h
        .wt()
        .args(["run", "hello", "repo", "--json"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["task"], "hello");
    assert!(String::from_utf8_lossy(&output.stderr).contains("hello"));
    let log = value["data"]["log"].as_str().unwrap();
    assert_eq!(std::fs::read(log).unwrap(), b"hello");
}

#[test]
fn shell_and_argv_recipes_are_templates_while_dollars_stay_literal() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
ports = ["http"]
[vars]
private = "secret"
[env]
OLD_TEMPLATE = '${root()}'
[files."generated.sh"]
marker = ""
content = "printf '%s\\n' '$' '${HOME}' '$$' '${h%??}' '{{private}}' '{{ports.http}}'"
[files."verbatim.j2"]
marker = ""
template = false
content = "{{ jinja_value }} ${HOME} $$"
[files."source-copy.j2"]
marker = ""
template = false
source = "input.j2"
[task.shell]
run = '''h=abcdef; printf '%s|%s|%s|%s|%s|%s|%s|%s' '{{root()}}' '$' '$HOME' '${HOME}' '$$' "${h%??}" '{{private}}' '{{ports.http}}' '''
[task.argv]
run = ["printf", "%s|%s", "{{private}}", "{{ports.http}}"]
"#,
    );
    common::write(&repo.join("input.j2"), "{{ source_value }} ${HOME} $$");
    h.register(&repo);
    assert_eq!(
        h.json(&["env", "repo"])["data"]["env"]["OLD_TEMPLATE"],
        "${root()}"
    );

    h.wt()
        .args(["run", "shell", "repo"])
        .assert()
        .success()
        .stdout(predicate::eq(format!(
            "{}|$|$HOME|${{HOME}}|$$|abcd|secret|20000",
            std::fs::canonicalize(&repo).unwrap().display()
        )));
    h.wt()
        .args(["run", "argv", "repo"])
        .assert()
        .success()
        .stdout(predicate::eq("secret|20000"));
    assert_eq!(
        std::fs::read_to_string(repo.join("generated.sh")).unwrap(),
        "printf '%s\\n' '$' '${HOME}' '$$' '${h%??}' 'secret' '20000'"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("verbatim.j2")).unwrap(),
        "{{ jinja_value }} ${HOME} $$"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("source-copy.j2")).unwrap(),
        "{{ source_value }} ${HOME} $$"
    );
}

#[test]
fn shell_resource_run_exists_and_destroy_are_templated_before_sh_c() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
[vars]
leaf = ".service"
[task.service]
tied_to = "tree"
run = "touch '{{root()}}/{{leaf}}'"
exists = "test -f '{{root()}}/{{leaf}}'"
destroy = "rm -f '{{root()}}/{{leaf}}'"
"#,
    );
    h.register(&repo);
    h.wt().args(["run", "service", "repo"]).assert().success();
    assert!(repo.join(".service").exists());
    h.wt()
        .args(["destroy", "service", "repo", "--yes"])
        .assert()
        .success();
    assert!(!repo.join(".service").exists());
}

#[test]
fn ports_is_a_reserved_var_key() {
    let error = wt_core::config::parse("[vars]\nports='mine'", "repo/.wt.toml").unwrap_err();
    assert_eq!(error.code.0, "CONFIG_INVALID");
    assert!(error.message.contains("reserved"));
}

#[test]
fn new_run_and_remove_form_an_idempotent_lifecycle() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync"]);
    assert_eq!(created["data"]["created"], true);
    assert_eq!(h.json(&["run", "hello", "repo/work"])["ok"], true);
    let removed = h.json(&["remove", "repo/work", "--yes"]);
    assert_eq!(removed["data"]["removed"], true);
    assert!(!Path::new(created["data"]["tree"]["path"].as_str().unwrap()).exists());
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
fn backend_none_runs_build_plan_after_ready_and_respects_suppression() {
    let h = Harness::new();
    configure_backend_none(&h);
    let events = h.root.join("build-events");
    let config = format!(
        r#"
[task.prepare]
run = ["sh", "-c", "printf 'prepare\\n' >> \"$EVENTS\""]
[task.prepare.env]
EVENTS = "{}"
[task.build]
needs = ["prepare"]
run = ["sh", "-c", "printf 'build:%s\\n' \"$(cat \"$WT_ROOT/.wt/build.status\")\" >> \"$EVENTS\""]
[task.build.env]
EVENTS = "{}"
"#,
        events.display(),
        events.display(),
    );
    let repo = h.repo("repo", &config);
    h.register(&repo);

    let created = h.json(&["new", "repo/work", "--no-sync"]);
    let status =
        Path::new(created["data"]["tree"]["path"].as_str().unwrap()).join(".wt/build.status");
    wait_for_text(&status, "ok\n");
    assert_eq!(
        std::fs::read_to_string(&events).unwrap(),
        "prepare\nbuild:running\n"
    );
    let target = wt_core::model::Target::parse("repo/work").unwrap();
    let state = wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(
        &h.home.join(wt_core::model::tree_state_path(&target)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    let build = state.build.unwrap();
    assert!(Path::new(&build.log).ends_with(".wt/logs/wt-setup.log"));
    assert_eq!(created["data"]["tree"]["phase"], "ready");
    assert_eq!(created["data"]["build"]["log"], build.log);
    assert_eq!(std::fs::read_to_string(&status).unwrap(), "ok\n");

    common::write(&events, "");
    h.json(&["build", "repo/work"]);
    assert_eq!(
        std::fs::read_to_string(&events).unwrap(),
        "prepare\nbuild:running\n"
    );
    assert_eq!(std::fs::read_to_string(&status).unwrap(), "ok\n");

    common::write(&events, "");
    h.json(&["new", "repo/no-build", "--no-sync", "--no-build"]);
    assert_eq!(std::fs::read_to_string(&events).unwrap(), "");
    h.json(&["new", "repo/no-open", "--no-sync", "--no-open"]);
    assert_eq!(std::fs::read_to_string(&events).unwrap(), "");
}

#[test]
fn backend_none_build_failure_is_reported_after_new_returns() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        r#"
bin = ["bin"]
commands = ["orbit"]
[task.build]
run = ["sh", "-c", "cat \"$WT_ROOT/.wt/build.status\" > \"$WT_ROOT/seen-status\"; exit 9"]
"#,
    );
    common::write_executable(&h.shims.join("orbit"), "#!/bin/sh\nprintf 'INSTALLED\\n'\n");
    h.register(&repo);

    let output = h
        .wt()
        .args(["new", "repo/work", "--no-sync", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    assert!(envelope["data"]["build"]["started"].is_string());
    let root = PathBuf::from(envelope["data"]["tree"]["path"].as_str().unwrap());
    wait_for_text(&root.join(".wt/build.status"), "failed\n");
    assert_eq!(
        std::fs::read_to_string(root.join("seen-status")).unwrap(),
        "running\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".wt/build.status")).unwrap(),
        "failed\n"
    );
    let status = h.json(&["status", "repo/work"]);
    assert_eq!(status["data"]["build"]["state"], "failed");
    assert!(h.json(&["doctor", "repo"])["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "BUILD_FAILED"));

    let refusal = h
        .wt()
        .args(["exec", "repo/work", "--", "orbit"])
        .output()
        .unwrap();
    assert_eq!(refusal.status.code(), Some(5));
    let refusal = String::from_utf8_lossy(&refusal.stderr);
    assert!(refusal.contains("wt build repo/work"), "{refusal}");
    assert!(refusal.contains(&h.shims.join("orbit").to_string_lossy().to_string()));
    assert!(!refusal.contains("wt:setup"), "{refusal}");
    assert!(!refusal.contains("build is in progress"), "{refusal}");
    common::proof_capture(
        "A1",
        format!(
            "recorded-tree terminal refusal:\n{}",
            refusal
                .replace(
                    &std::fs::canonicalize(&h.root)
                        .unwrap_or_else(|_| h.root.clone())
                        .to_string_lossy()
                        .to_string(),
                    "<ROOT>",
                )
                .replace(&h.root.to_string_lossy().to_string(), "<ROOT>")
        ),
    );

    common::write(
        &root.join(".wt.toml"),
        r#"
bin = ["bin"]
commands = ["orbit"]
[task.build]
run = ["sh", "-c", "cat \"$WT_ROOT/.wt/build.status\" > \"$WT_ROOT/seen-status\""]
"#,
    );
    h.json(&["build", "repo/work"]);
    assert_eq!(
        std::fs::read_to_string(root.join("seen-status")).unwrap(),
        "running\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".wt/build.status")).unwrap(),
        "ok\n"
    );
    common::proof_capture(
        "D3",
        "backend none observed running inside the task\nfirst terminal status: failed\nretry observed running inside the task\nretry terminal status: ok",
    );
}

#[test]
fn dead_build_supervisor_is_abandoned_for_status_doctor_and_shims() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        r#"
bin = ["bin"]
commands = ["orbit"]
[task.build]
run = "true"
"#,
    );
    h.register(&repo);
    h.json(&["env", "repo"]);

    let target = wt_core::model::Target::parse("repo").unwrap();
    let state_path = h.home.join(wt_core::model::tree_state_path(&target));
    let mut state =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    let log = repo.join(".wt/logs/wt-setup.log");
    state.build = Some(wt_core::lifecycle::BuildState {
        started: wt_sys::fsx::timestamp().unwrap(),
        log: log.to_string_lossy().into_owned(),
        pid: u32::MAX,
    });
    wt_sys::fsx::write_json(&state_path, &state).unwrap();
    common::write(&repo.join(".wt/build.status"), "running\n");

    let listed = h.json(&["ls"]);
    assert_eq!(listed["data"]["trees"][0]["build"]["state"], "abandoned");
    assert_eq!(
        h.json(&["status", "repo"])["data"]["build"]["state"],
        "abandoned"
    );
    let doctor = h.json(&["doctor", "repo"]);
    let finding = doctor["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "BUILD_ABANDONED")
        .unwrap();
    assert_eq!(finding["severity"], "warn");
    assert!(finding["remedy"]
        .as_str()
        .unwrap()
        .contains("wt build repo"));

    let refusal = h
        .wt()
        .args(["exec", "repo", "--", "orbit"])
        .output()
        .unwrap();
    assert_eq!(refusal.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&refusal.stderr);
    assert!(stderr.contains("COMMAND_NOT_BUILT"), "{stderr}");
    assert!(stderr.contains("wt build repo"), "{stderr}");
    assert!(!stderr.contains("build is in progress"), "{stderr}");
}

#[test]
fn foreground_build_records_its_own_pid_over_a_dead_supervisor() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        r#"
[task.build]
run = "true"
"#,
    );
    h.register(&repo);
    h.json(&["env", "repo"]);

    let target = wt_core::model::Target::parse("repo").unwrap();
    let state_path = h.home.join(wt_core::model::tree_state_path(&target));
    let mut state =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    state.build = Some(wt_core::lifecycle::BuildState {
        started: wt_sys::fsx::timestamp().unwrap(),
        log: repo
            .join(".wt/logs/wt-setup.log")
            .to_string_lossy()
            .into_owned(),
        pid: u32::MAX,
    });
    wt_sys::fsx::write_json(&state_path, &state).unwrap();
    common::write(&repo.join(".wt/build.status"), "running\n");

    h.json(&["build", "repo"]);

    // The reset to `running` must carry the live pid, or the whole foreground
    // run reads as abandoned against the finished supervisor's dead pid.
    let state =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    assert_ne!(state.build.unwrap().pid, u32::MAX);
    assert_eq!(h.json(&["status", "repo"])["data"]["build"]["state"], "ok");
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
        let pr_url = match forge {
            "github" => "https://github.com/o/repo/pull/8",
            "gitlab" => "https://gitlab.com/o/repo/-/merge_requests/8",
            _ => "https://bitbucket.org/o/repo/pull-requests/8",
        };
        assert_eq!(
            h.json(&[
                "new",
                &format!("repo/{forge}"),
                "--from",
                pr_url,
                "--no-sync",
            ])["data"]["tree"]["branch"],
            "pr/8"
        );
    }
}

#[test]
fn aliases_use_the_run_task_set() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[task.test]\nrun='printf test'\n[task.lint]\nrun='printf lint'\n[task.fmt]\nrun='printf fmt'\n[task.build]\nrun='printf build'\n",
    );
    h.register(&repo);
    for alias in ["test", "lint", "fmt", "build"] {
        let value = h.json(&[alias, "repo"]);
        assert_eq!(value["command"], alias);
        assert_eq!(value["data"]["task"], alias);
    }
}

#[test]
fn section_6_2_aggregate_task_runs_needs_in_plan_order() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
[task.first]
run='printf first >> "$WT_ROOT/.aggregate-order"'
[task.second]
run='printf second >> "$WT_ROOT/.aggregate-order"'
needs=['first']
[task.setup]
needs=['second']
description='run all setup steps'
"#,
    );
    h.register(&repo);

    let result = h.json(&["run", "setup", "repo"]);
    assert_eq!(
        result["data"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "second", "setup"]
    );
    assert_eq!(result["data"]["steps"][2]["status"], "skipped");
    assert_eq!(result["data"]["steps"][2]["child"], serde_json::Value::Null);
    assert_eq!(
        std::fs::read_to_string(repo.join(".aggregate-order")).unwrap(),
        "firstsecond"
    );
    let tasks = h.json(&["tasks", "repo"]);
    let setup = tasks["data"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "setup")
        .unwrap();
    assert_eq!(setup["origin"], "repo");
}

#[test]
fn section_5_6_aggregate_inert_key_refusal_names_key() {
    let h = Harness::new();
    let repo = h.repo("repo", "[task.setup]\nneeds=[]\nlock='serial'\n");
    h.wt()
        .args(["register", repo.to_str().unwrap()])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("CONFIG_INVALID"))
        .stderr(predicate::str::contains("key `lock`"))
        .stderr(predicate::str::contains("would guard nothing"))
        .stderr(predicate::str::contains("task that runs"));
}

#[test]
fn section_10_1_trailing_args_append_to_argv_root_only_and_are_reported() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        r#"
[task.dependency]
run=['{{root()}}/record-args', 'dependency', 'declared']
[task.argv]
run=['{{root()}}/record-args', 'root', 'base']
needs=['dependency']
[task.test]
run=['{{root()}}/record-args', 'alias', 'declared']
"#,
    );
    common::write_executable(
        &repo.join("record-args"),
        r#"#!/bin/sh
label=$1
shift
printf '%s:' "$label" >> "$WT_ROOT/.recorded-args"
for arg in "$@"; do printf '<%s>' "$arg" >> "$WT_ROOT/.recorded-args"; done
printf '\n' >> "$WT_ROOT/.recorded-args"
"#,
    );
    h.register(&repo);

    let output = h
        .wt()
        .args(["run", "argv", "repo", "--json", "--", "a b", "*.rs"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["args"], serde_json::json!(["a b", "*.rs"]));
    assert_eq!(value["data"]["args_target"], "argv");
    assert_eq!(
        std::fs::read_to_string(repo.join(".recorded-args")).unwrap(),
        "dependency:<declared>\nroot:<base><a b><*.rs>\n"
    );
    let log = value["data"]["log"].as_str().unwrap();
    assert!(std::fs::read_to_string(log)
        .unwrap()
        .starts_with("wt args: [\"a b\",\"*.rs\"]\n"));

    let dry = h
        .wt()
        .args([
            "run",
            "argv",
            "repo",
            "--dry-run",
            "--json",
            "--",
            "dry arg",
        ])
        .output()
        .unwrap();
    assert!(dry.status.success());
    let dry: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(dry["data"]["args"], serde_json::json!(["dry arg"]));
    assert_eq!(dry["data"]["args_target"], "argv");
    h.wt()
        .args(["run", "argv", "repo", "--dry-run", "--", "visible arg"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Plan for argv -- \"visible arg\""));

    let alias = h
        .wt()
        .args(["test", "repo", "--json", "--", "alias arg"])
        .output()
        .unwrap();
    assert!(alias.status.success());
    let alias: serde_json::Value = serde_json::from_slice(&alias.stdout).unwrap();
    assert_eq!(alias["data"]["args"], serde_json::json!(["alias arg"]));
    assert_eq!(alias["data"]["args_target"], "test");
    assert!(std::fs::read_to_string(repo.join(".recorded-args"))
        .unwrap()
        .ends_with("alias:<declared><alias arg>\n"));
    assert_eq!(
        h.json(&["run", "argv", "repo", "--dry-run"])["data"]["args_target"],
        serde_json::Value::Null
    );
}

#[test]
fn section_10_1_shell_trailing_args_use_positional_parameters() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        r#"
[task.shell]
run='"$WT_ROOT/record-args" shell ruff "$@" && "$WT_ROOT/record-args" shell mypy .'
[task.plain]
run='printf unchanged > "$WT_ROOT/.plain"'
[task.comment]
run='''# "$@" is intentionally only a lexical match
printf ignored > "$WT_ROOT/.comment"'''
"#,
    );
    common::write_executable(
        &repo.join("record-args"),
        r#"#!/bin/sh
label=$1
shift
printf '%s:' "$label" >> "$WT_ROOT/.shell-args"
for arg in "$@"; do printf '<%s>' "$arg" >> "$WT_ROOT/.shell-args"; done
printf '\n' >> "$WT_ROOT/.shell-args"
"#,
    );
    h.register(&repo);

    h.wt()
        .args(["run", "shell", "repo", "--", "only file.py", "second"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.join(".shell-args")).unwrap(),
        "shell:<ruff><only file.py><second>\nshell:<mypy><.>\n"
    );
    h.wt().args(["run", "plain", "repo"]).assert().success();
    assert_eq!(
        std::fs::read_to_string(repo.join(".plain")).unwrap(),
        "unchanged"
    );
    h.wt()
        .args(["run", "comment", "repo", "--", "ignored"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.join(".comment")).unwrap(),
        "ignored"
    );
}

#[test]
fn section_10_1_trailing_arg_refusals_precede_dependencies() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        r#"
[task.dependency]
run='touch "$WT_ROOT/.dependency-ran"'
[task.no_parameters]
run='true'
needs=['dependency']
[task.leaf]
run='true'
[task.aggregate]
needs=['leaf']
[task.service]
run='touch "$WT_ROOT/.service"'
exists='test -f "$WT_ROOT/.service"'
destroy='rm -f "$WT_ROOT/.service"'
tied_to='tree'
"#,
    );
    h.register(&repo);

    h.wt()
        .args(["run", "no_parameters", "repo", "--", "arg"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ARGS_UNSUPPORTED"))
        .stderr(predicate::str::contains("\"$@\""));
    assert!(!repo.join(".dependency-ran").exists());

    h.wt()
        .args(["run", "aggregate", "repo", "--", "arg"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ARGS_ON_COMPOSITE"))
        .stderr(predicate::str::contains("leaf"))
        .stderr(predicate::str::contains("wt run leaf --"));

    h.wt()
        .args(["run", "service", "repo", "--", "arg"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ARGS_UNSUPPORTED"))
        .stderr(predicate::str::contains(
            "state transition replayed from snapshots",
        ));
    assert!(!repo.join(".service").exists());

    h.wt()
        .args(["run", "no_parameters", "repo"])
        .assert()
        .success();
    assert!(repo.join(".dependency-ran").exists());
}

#[test]
fn section_10_1_trailing_args_refuse_two_scope_composite_with_public_needs() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        r#"
[dirs."d1".task.check]
run=['true']
[dirs."d2".task.check]
run=['true']
"#,
    );
    wt_sys::fsx::create_private_dir(&repo.join("d1")).unwrap();
    wt_sys::fsx::create_private_dir(&repo.join("d2")).unwrap();
    h.register(&repo);

    let output = h
        .wt()
        .args(["run", "check", "repo", "--", "arg"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ARGS_ON_COMPOSITE"), "{stderr}");
    assert!(stderr.contains("d1/check"), "{stderr}");
    assert!(stderr.contains("d2/check"), "{stderr}");
    assert!(!stderr.contains('@'), "{stderr}");
}

#[test]
fn section_10_1_trailing_args_cross_single_scope_composite() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo(
        "repo",
        r#"
[dirs."d1".task.check]
run=['{{root()}}/record-args', 'check']
"#,
    );
    wt_sys::fsx::create_private_dir(&repo.join("d1")).unwrap();
    common::write_executable(
        &repo.join("record-args"),
        r#"#!/bin/sh
printf '%s' "$1" > "$WT_ROOT/.single-scope-args"
shift
for arg in "$@"; do printf '<%s>' "$arg" >> "$WT_ROOT/.single-scope-args"; done
"#,
    );
    h.register(&repo);

    h.wt()
        .args(["run", "check", "repo", "--", "forwarded arg"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.join(".single-scope-args")).unwrap(),
        "check<forwarded arg>"
    );
}

#[test]
fn section_10_1_trailing_args_cross_adapter_composite_and_alias() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo("repo", "[dirs.d1]\n");
    common::write(
        &repo.join("d1/package.json"),
        r#"{"name":"fixture","scripts":{"test":"fixture"}}"#,
    );
    common::write(&repo.join("d1/package-lock.json"), "{}\n");
    common::write_executable(
        &h.shims.join("npm"),
        r#"#!/bin/sh
for arg in "$@"; do printf '<%s>' "$arg" >> "$WT_ROOT/.adapter-args"; done
printf '\n' >> "$WT_ROOT/.adapter-args"
"#,
    );
    h.register(&repo);

    h.wt()
        .args(["test", "repo", "--", "-k", "foo"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.join(".adapter-args")).unwrap(),
        "<test><--><-k><foo>\n"
    );
}

#[test]
fn section_14_1_trailing_args_require_the_delimiter() {
    let h = Harness::new();
    h.wt()
        .args(["run", "task", "repo", "argument"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unexpected argument 'argument'"))
        .stderr(predicate::str::contains("[-- <ARGS>...]"));
}

#[test]
fn resource_run_and_destroy_follow_probe_truth() {
    let h = Harness::new();
    let repo = h.repo("repo", RESOURCE);
    h.register(&repo);
    assert_eq!(h.json(&["run", "service", "repo"])["ok"], true);
    assert!(repo.join(".service").exists());
    assert_eq!(
        h.json(&["destroy", "service", "repo", "--yes"])["data"]["after"],
        "declared"
    );
    assert!(!repo.join(".service").exists());
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
fn doctor_reports_and_prune_deletes_cache_orphans_and_remove_reaps_the_tree_cache() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync"]);
    let live = h
        .home
        .join("cache/cargo-build/repo")
        .join(wt_core::model::name_short("repo", "work"));
    wt_sys::fsx::create_private_dir(&live).unwrap();
    common::write(&live.join("marker"), "built\n");
    // Debris from the retired per-repository layout, and a label no longer
    // registered: both are orphans; the live tree's directory is not.
    let legacy = h.home.join("cache/cargo-build/repo/debug");
    wt_sys::fsx::create_private_dir(&legacy).unwrap();
    let foreign = h.home.join("cache/cargo-build/unregistered");
    wt_sys::fsx::create_private_dir(&foreign).unwrap();

    let findings = h.json(&["doctor"])["data"]["findings"].clone();
    let orphaned = findings
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "CACHE_ORPHAN")
        .filter_map(|finding| finding["subject"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        orphaned,
        BTreeSet::from([
            "cargo-build/repo/debug".to_owned(),
            "cargo-build/unregistered".to_owned(),
        ])
    );

    let applied = h.json(&["prune", "--yes"]);
    assert!(applied["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["action"] == "delete-cache" && item["result"]["deleted"] == true));
    assert!(!legacy.exists());
    assert!(!foreign.exists());
    assert!(live.exists());

    let removed = h.json(&["remove", "repo/work", "--yes", "--force"]);
    assert_eq!(removed["data"]["removed"], true);
    assert_eq!(
        removed["data"]["cache_deleted"],
        live.to_string_lossy().as_ref()
    );
    assert!(!live.exists());
}

#[test]
fn remove_keep_orphans_leaves_a_missing_live_entry() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[task.service]\nrun='true'\nexists='exit 2'\ndestroy='true'\ntied_to='tree'\n",
    );
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync"]);
    let removed = h.json(&["remove", "repo/work", "--yes", "--force", "--keep-orphans"]);
    assert_eq!(removed["data"]["removed"], true);
    assert!(!removed["data"]["orphans_kept"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(h.json(&["status", "repo/work"])["data"]["phase"], "missing");
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
fn register_repair_restores_only_a_replaced_canonical_checkout() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[files.'.wt/generated']\ncontent='restored {{target()}}'\n[task.service]\nrun='true'\nexists='false'\ndestroy='true'\ntied_to='tree'\n",
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
    assert_eq!(state_after.verify, state_before.verify);
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
fn truth_reports_upstream_default_drift_staleness_probe_and_disk() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "sync_inputs=['README.md']\n[task.sync]\nrun='true'\n[task.service]\nrun='touch \"$WT_ROOT/.service\"'\nexists='test -f \"$WT_ROOT/.service\"'\ndestroy='rm -f \"$WT_ROOT/.service\"'\ntied_to='tree'\n",
    );
    h.register(&repo);
    h.json(&["new", "repo/work"]);
    h.json(&["run", "service", "repo/work"]);
    common::write(&repo.join("README.md"), "default advanced\n");
    common::git(&repo, &["add", "README.md"]);
    common::git(&repo, &["commit", "-qm", "advance default"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);
    common::write(
        &Path::new(
            h.json(&["path", "repo/work"])["data"]["path"]
                .as_str()
                .unwrap(),
        )
        .join("README.md"),
        "locally stale\n",
    );
    let value = h.json(&["list", "--disk", "--probe"]);
    let tree = value["data"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["target"] == "repo/work")
        .unwrap();
    assert_eq!(tree["behind_default"], 1);
    assert_eq!(tree["sync"]["state"], "stale");
    assert_eq!(tree["sync"]["drift"][0], "README.md");
    assert!(tree["disk_kb"].as_u64().is_some());
    assert_eq!(tree["resources"][0]["last_probe"]["result"], "present");
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
            "state_read",
            "render_hash_compare",
        ]
    );
    assert_eq!(std::fs::read(state).unwrap(), state_before);
}

#[test]
fn sync_and_which_have_stable_data_shapes() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "sync_inputs=['README.md']\n[task.prep]\nrun='true'\n[task.sync]\nrun='true'\nneeds=['prep']\n",
    );
    h.register(&repo);
    let synced = h.json(&["sync", "repo"]);
    assert_eq!(synced["data"]["ran"], true);
    assert_eq!(synced["data"]["steps"].as_array().unwrap().len(), 2);
    let which = h.json(&["which", "repo", "true"]);
    assert!(which["data"]["path"].as_str().is_some());
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
fn refresh_recreates_a_present_resource() {
    let h = Harness::new();
    let repo = h.repo("repo", RESOURCE);
    h.register(&repo);
    h.json(&["run", "service", "repo"]);
    let refreshed = h.json(&["refresh", "service", "repo", "--yes"]);
    assert_eq!(refreshed["data"]["after"], "present");
    assert!(repo.join(".service").exists());
}

#[test]
fn sections_10_2_to_11_5_machine_scope_is_shared_and_lifecycle_excluded() {
    let h = Harness::new();
    let config = |source: &str| {
        format!(
            r#"
[task.setup]
run = 'printf %s "$SOURCE" > "$WT_HOME/machine-ready"'
exists = 'test -f "$WT_HOME/machine-ready"'
destroy = 'rm -f "$WT_HOME/machine-ready"'
tied_to = "machine"
[task.setup.env]
SOURCE = "{source}"
"#
        )
    };
    let first = h.repo("first", &config("first"));
    let second = h.repo("second", &config("second"));
    h.register(&first);
    h.register(&second);

    let machine_path = h.home.join(wt_core::model::machine_state_path());
    let machine =
        wt_sys::fsx::read_json::<wt_core::lifecycle::RepoState>(&machine_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    assert_eq!(machine.label, None);
    assert_eq!(machine.resources.len(), 1);
    let record = &machine.resources["setup"];
    assert_eq!(record.key.label, None);
    assert_eq!(record.key.name, None);
    assert_eq!(record.key.tied_to, wt_core::config::TiedTo::Machine);
    assert!(!record.declaration.env.contains_key("WT_LABEL"));
    assert!(!record.declaration.env.contains_key("WT_REPO"));
    assert!(!record.declaration.env.contains_key("WT_ROOT"));

    h.json(&["run", "setup", "first"]);
    assert_eq!(
        std::fs::read_to_string(h.home.join("machine-ready")).unwrap(),
        "first"
    );
    assert!(h.home.join("locks/_machine.rmw.lock").exists());
    assert!(h.home.join("locks/_machine/res/./setup.lock").exists());

    h.json(&["refresh", "setup", "second", "--yes"]);
    assert_eq!(
        std::fs::read_to_string(h.home.join("machine-ready")).unwrap(),
        "second"
    );
    h.json(&["refresh", "setup", "first", "--yes"]);
    assert_eq!(
        std::fs::read_to_string(h.home.join("machine-ready")).unwrap(),
        "first"
    );

    h.json(&["new", "first/work", "--no-sync", "--no-open"]);
    let before_remove = std::fs::read(&machine_path).unwrap();
    h.json(&["remove", "first/work", "--yes"]);
    assert_eq!(std::fs::read(&machine_path).unwrap(), before_remove);
    h.json(&["unregister", "first", "--yes"]);
    assert_eq!(std::fs::read(&machine_path).unwrap(), before_remove);

    h.wt()
        .args(["destroy", "setup", "second"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("CONFIRM_REQUIRED"));
    assert!(h.home.join("machine-ready").exists());
    let declined = h.pty_output(&["destroy", "setup", "second"], b"n\n");
    let transcript = String::from_utf8_lossy(&declined.stdout);
    assert!(transcript.contains("destroy resource setup? [y/N]"));
    assert!(h.home.join("machine-ready").exists());
    assert_eq!(
        h.pty_status(&["destroy", "setup", "second"], b"y\n").code,
        Some(0)
    );
    assert!(!h.home.join("machine-ready").exists());

    h.json(&["refresh", "setup", "second", "--yes"]);
    assert_eq!(
        std::fs::read_to_string(h.home.join("machine-ready")).unwrap(),
        "second"
    );
    h.json(&["destroy", "setup", "second", "--yes"]);
    assert!(!h.home.join("machine-ready").exists());
}

fn resource_config(exclusive: Option<&str>, live: &Path, events: &Path) -> String {
    let exclusive =
        exclusive.map_or_else(String::new, |arena| format!("exclusive = \"{arena}\"\n"));
    format!(
        r#"
[task.service]
run = 'printf %s "$WT_TARGET" > "{}"'
exists = 'test -f "{}"'
destroy = 'printf "destroy:%s\n" "$WT_TARGET" >> "{}"; rm -f "{}"'
tied_to = "tree"
{}
"#,
        live.display(),
        live.display(),
        events.display(),
        live.display(),
        exclusive,
    )
}

fn exclusive_config(arena: &str, live: &Path, events: &Path) -> String {
    resource_config(Some(arena), live, events)
}

fn repo_exclusive_holder(h: &Harness) -> Option<String> {
    let label = wt_core::model::Label::new("repo").unwrap();
    wt_sys::fsx::read_json::<wt_core::lifecycle::RepoState>(
        &h.home.join(wt_core::model::repo_state_path(&label)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .and_then(|state| state.exclusive.get("service").cloned())
    .map(|entry| entry.holder)
}

#[test]
fn sections_5_2_and_10_4_exclusive_grammar_names_the_tree_resource_rule() {
    for invalid in [
        r#"[task.x]
run = "true"
exclusive = "repo"
"#,
        r#"[task.x]
destroy = "true"
tied_to = "tree"
exclusive = "machine"
"#,
        r#"[task.x]
exists = "false"
destroy = "true"
tied_to = "repo"
exclusive = "repo"
"#,
    ] {
        let error = wt_core::config::parse(invalid, "repo/.wt.toml").unwrap_err();
        assert_eq!(error.code.0, "CONFIG_INVALID");
        assert!(error.message.contains(
            "exclusive is valid only on a tree-tied resource (destroy + exists + tied_to = \"tree\")"
        ));
    }
}

#[test]
fn sections_10_1_to_10_4_take_displaces_frozen_holder_and_flips_repo_arena() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &exclusive_config("repo", &live, &events));
    h.register(&repo);
    h.json(&["new", "repo/a", "--no-sync", "--no-open"]);
    h.json(&["new", "repo/b", "--no-sync", "--no-open"]);
    h.json(&["run", "service", "repo/a"]);

    let held = h
        .wt()
        .args(["run", "service", "repo/b", "--json"])
        .output()
        .unwrap();
    assert_eq!(held.status.code(), Some(4));
    let held_json: serde_json::Value = serde_json::from_slice(&held.stdout).unwrap();
    assert_eq!(held_json["error"]["code"], "RESOURCE_HELD");
    assert!(held_json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("repo/a"));
    assert!(held_json["error"]["remedy"]
        .as_str()
        .unwrap()
        .contains("--take"));

    h.wt()
        .args(["run", "service", "repo/b", "--take"])
        .assert()
        .success()
        .stdout(predicate::str::contains("displaced repo/a"));
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "repo/b");
    assert_eq!(
        std::fs::read_to_string(&events).unwrap(),
        "destroy:repo/a\n"
    );
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/b"));

    let displaced_json = h.json(&["run", "service", "repo/a", "--take"]);
    assert_eq!(displaced_json["data"]["displaced"], "repo/b");
    let final_json = h.json(&["run", "service", "repo/b", "--take"]);
    assert_eq!(final_json["data"]["displaced"], "repo/a");
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/b"));
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "repo/b");
}

#[test]
fn sections_10_4_and_11_4_live_declaration_protects_stale_non_holder_on_remove() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &resource_config(None, &live, &events));
    h.register(&repo);
    let created_a = h.json(&["new", "repo/a", "--no-sync", "--no-open"]);
    let created_b = h.json(&["new", "repo/b", "--no-sync", "--no-open"]);
    let tree_a = PathBuf::from(created_a["data"]["tree"]["path"].as_str().unwrap());
    let tree_b = PathBuf::from(created_b["data"]["tree"]["path"].as_str().unwrap());
    h.json(&["run", "service", "repo/a"]);

    let target_a = wt_core::model::Target::parse("repo/a").unwrap();
    let state_path_a = h.home.join(wt_core::model::tree_state_path(&target_a));
    let state_a =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path_a, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    assert_eq!(
        state_a.resources["service"]
            .instance
            .as_ref()
            .unwrap()
            .exclusive,
        None
    );

    let exclusive = exclusive_config("repo", &live, &events);
    common::write(&tree_a.join(".wt/config.toml"), &exclusive);
    common::write(&tree_b.join(".wt/config.toml"), &exclusive);
    h.json(&["run", "service", "repo/b"]);
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/b"));

    let removed = h.json(&["remove", "repo/a", "--yes"]);
    assert_eq!(removed["data"]["destroyed"][0]["state"], "dropped");
    assert!(!state_path_a.exists());
    assert!(live.exists());
    assert!(!events.exists());
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/b"));
}

#[test]
fn sections_10_4_and_11_4_arena_protects_pre_exclusive_checkout_on_remove() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &resource_config(None, &live, &events));
    h.register(&repo);
    h.json(&["new", "repo/a", "--no-sync", "--no-open"]);
    let created_b = h.json(&["new", "repo/b", "--no-sync", "--no-open"]);
    let tree_b = PathBuf::from(created_b["data"]["tree"]["path"].as_str().unwrap());

    common::write(
        &tree_b.join(".wt/config.toml"),
        &exclusive_config("repo", &live, &events),
    );
    h.json(&["run", "service", "repo/b"]);
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/b"));

    let target_a = wt_core::model::Target::parse("repo/a").unwrap();
    let state_path_a = h.home.join(wt_core::model::tree_state_path(&target_a));
    let state_a =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path_a, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    assert_eq!(state_a.resources["service"].declaration.exclusive, None);

    let removed = h.json(&["remove", "repo/a", "--yes"]);
    assert_eq!(removed["data"]["destroyed"][0]["state"], "dropped");
    let notice = removed["notices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|notice| notice["code"] == "RESOURCE_HELD_BY")
        .unwrap();
    assert_eq!(notice["subject"], "service");
    assert!(notice["message"]
        .as_str()
        .unwrap()
        .contains("left to repo/b"));
    assert!(live.exists());
    assert!(!events.exists());
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/b"));
}

#[test]
fn sections_10_4_and_11_4_unrelated_label_ignores_machine_divergence_guard() {
    let h = Harness::new();
    configure_backend_none(&h);
    let alpha_live = h.root.join("alpha-db");
    let beta_live = h.root.join("beta-db");
    let beta_destroyed = h.root.join("beta-db-destroyed");
    let alpha = h.repo(
        "alpha",
        &format!(
            r#"
[task.db]
run = 'touch "{}"'
exists = 'test -f "{}"'
destroy = 'rm -f "{}"'
tied_to = "tree"
exclusive = "machine"
"#,
            alpha_live.display(),
            alpha_live.display(),
            alpha_live.display(),
        ),
    );
    let beta = h.repo(
        "beta",
        &format!(
            r#"
[task.db]
run = 'touch "{}"'
exists = 'test -f "{}"'
destroy = 'touch "{}"; rm -f "{}"'
tied_to = "tree"
"#,
            beta_live.display(),
            beta_live.display(),
            beta_destroyed.display(),
            beta_live.display(),
        ),
    );
    h.register(&alpha);
    h.register(&beta);
    h.json(&["run", "db", "alpha"]);
    h.json(&["run", "db", "beta"]);
    assert!(alpha_live.exists());
    assert!(beta_live.exists());

    let destroyed = h.json(&["destroy", "db", "beta", "--yes"]);
    assert_eq!(destroyed["data"]["after"], "declared");
    assert!(!destroyed["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["code"] == "RESOURCE_HELD_BY"));
    assert!(alpha_live.exists());
    assert!(!beta_live.exists());
    assert!(beta_destroyed.exists());

    let machine = wt_sys::fsx::read_json::<wt_core::lifecycle::RepoState>(
        &h.home.join(wt_core::model::machine_state_path()),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    assert_eq!(machine.exclusive["db"].holder, "alpha");

    // The other branch of the guard, pinned so a future narrowing cannot
    // silently break it: a label that DECLARES exclusive = "machine" must
    // respect another label's live holder on the teardown path.
    let gamma_destroyed = h.root.join("gamma-db-destroyed");
    let gamma = h.repo(
        "gamma",
        &format!(
            r#"
[task.db]
run = 'true'
exists = 'test -f "{}"'
destroy = 'touch "{}"'
tied_to = "tree"
exclusive = "machine"
"#,
            alpha_live.display(),
            gamma_destroyed.display(),
        ),
    );
    h.register(&gamma);
    let held = h.json(&["destroy", "db", "gamma", "--yes"]);
    assert_eq!(held["data"]["after"], "declared");
    assert!(held["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["code"] == "RESOURCE_HELD_BY"
            && notice["message"].as_str().unwrap().contains("alpha")));
    assert!(!gamma_destroyed.exists());
    assert!(alpha_live.exists());
}

#[test]
fn section_12_prune_collects_stale_exclusive_arena_entries() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &exclusive_config("repo", &live, &events));
    h.register(&repo);
    let created = h.json(&["new", "repo/a", "--no-sync", "--no-open"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    h.json(&["run", "service", "repo/a"]);
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/a"));

    wt_sys::fsx::remove_path(&tree).unwrap();
    let registry_path = h.home.join("registry.json");
    let mut registry =
        wt_sys::fsx::read_json::<wt_core::model::Registry>(&registry_path, "REGISTRY_CORRUPT")
            .unwrap()
            .unwrap();
    registry
        .trees
        .retain(|tree| tree.label.as_str() != "repo" || tree.name != "a");
    wt_sys::fsx::write_json(&registry_path, &registry).unwrap();

    let pruned = h.json(&["prune", "--yes"]);
    let item = pruned["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["target"] == "repo:exclusive.service")
        .unwrap();
    assert_eq!(item["reasons"][0], "stale-exclusive-holder");
    assert_eq!(item["action"], "delete-exclusive");
    assert_eq!(item["result"]["deleted"], true);
    assert_eq!(repo_exclusive_holder(&h), None);
}

#[test]
fn section_10_4_live_declaration_claims_for_stale_pre_exclusive_instance() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &resource_config(None, &live, &events));
    h.register(&repo);
    let created = h.json(&["new", "repo/a", "--no-sync", "--no-open"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    h.json(&["run", "service", "repo/a"]);
    assert_eq!(repo_exclusive_holder(&h), None);

    common::write(
        &tree.join(".wt/config.toml"),
        &exclusive_config("repo", &live, &events),
    );
    h.json(&["run", "service", "repo/a"]);
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/a"));
}

#[test]
fn sections_10_1_and_14_4_take_without_displacement_is_a_plain_run() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &exclusive_config("repo", &live, &events));
    h.register(&repo);

    let no_holder = h.json(&["run", "service", "repo", "--take"]);
    assert_eq!(no_holder["data"]["displaced"], serde_json::Value::Null);
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo"));

    let current_holder = h.json(&["run", "service", "repo", "--take"]);
    assert_eq!(current_holder["data"]["displaced"], serde_json::Value::Null);
    h.wt()
        .args(["run", "service", "repo", "--take"])
        .assert()
        .success()
        .stdout(predicate::str::contains("displaced").not());
}

#[test]
fn sections_10_4_and_11_4_non_holder_remove_skips_and_holder_remove_clears() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &exclusive_config("repo", &live, &events));
    h.register(&repo);
    h.json(&["new", "repo/a", "--no-sync", "--no-open"]);
    h.json(&["new", "repo/b", "--no-sync", "--no-open"]);
    h.json(&["run", "service", "repo/a"]);

    h.json(&["remove", "repo/b", "--yes"]);
    assert!(live.exists());
    assert!(!events.exists());
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/a"));

    h.json(&["remove", "repo/a", "--yes"]);
    assert!(!live.exists());
    assert_eq!(
        std::fs::read_to_string(&events).unwrap(),
        "destroy:repo/a\n"
    );
    assert_eq!(repo_exclusive_holder(&h), None);
}

#[test]
fn sections_10_4_and_12_external_adoption_and_absent_probe_manage_holder() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &exclusive_config("repo", &live, &events));
    h.register(&repo);
    common::write(&live, "external");

    let adopted = h.json(&["run", "service", "repo"]);
    assert_eq!(adopted["data"]["steps"][0]["status"], "present");
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo"));
    let status = h.json(&["status", "repo"]);
    assert_eq!(status["data"]["resources"][0]["external"], true);
    assert_eq!(status["data"]["resources"][0]["holder"], "repo");
    h.wt()
        .args(["status", "repo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("holder repo"));
    h.wt()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("service:repo"));

    wt_sys::fsx::remove_path(&live).unwrap();
    let probed = h.json(&["status", "repo", "--probe"]);
    assert_eq!(probed["data"]["resources"][0]["state"], "declared");
    assert_eq!(probed["data"]["resources"][0]["has_instance"], false);
    assert_eq!(
        probed["data"]["resources"][0]["holder"],
        serde_json::Value::Null
    );
    assert_eq!(repo_exclusive_holder(&h), None);

    let label = wt_core::model::Label::new("repo").unwrap();
    let arena_path = h.home.join(wt_core::model::repo_state_path(&label));
    let mut arena =
        wt_sys::fsx::read_json::<wt_core::lifecycle::RepoState>(&arena_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap_or(wt_core::lifecycle::RepoState {
                schema: 1,
                label: Some(label),
                resources: std::collections::BTreeMap::new(),
                exclusive: std::collections::BTreeMap::new(),
            });
    arena.exclusive.insert(
        "service".to_owned(),
        wt_core::lifecycle::ExclusiveHolder {
            holder: "repo/gone".to_owned(),
            since: wt_sys::fsx::timestamp().unwrap(),
        },
    );
    wt_sys::fsx::write_json(&arena_path, &arena).unwrap();
    h.json(&["run", "service", "repo"]);
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo"));
}

#[test]
fn section_10_4_exclusive_frozen_crash_is_settled_by_the_next_probe() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let repo = h.repo("repo", &exclusive_config("repo", &live, &events));
    h.register(&repo);

    let target = wt_core::model::Target::parse("repo").unwrap();
    let state_path = h.home.join(wt_core::model::tree_state_path(&target));
    let mut state =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    let record = state.resources.get_mut("service").unwrap();
    record.instance = Some(record.declaration.clone());
    wt_sys::fsx::write_json(&state_path, &state).unwrap();

    let label = wt_core::model::Label::new("repo").unwrap();
    let arena_path = h.home.join(wt_core::model::repo_state_path(&label));
    let mut arena =
        wt_sys::fsx::read_json::<wt_core::lifecycle::RepoState>(&arena_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap_or(wt_core::lifecycle::RepoState {
                schema: 1,
                label: Some(label),
                resources: std::collections::BTreeMap::new(),
                exclusive: std::collections::BTreeMap::new(),
            });
    arena.exclusive.insert(
        "service".to_owned(),
        wt_core::lifecycle::ExclusiveHolder {
            holder: "repo".to_owned(),
            since: wt_sys::fsx::timestamp().unwrap(),
        },
    );
    wt_sys::fsx::write_json(&arena_path, &arena).unwrap();

    let probed = h.json(&["status", "repo", "--probe"]);
    assert_eq!(probed["data"]["resources"][0]["state"], "declared");
    assert_eq!(probed["data"]["resources"][0]["has_instance"], false);
    assert_eq!(repo_exclusive_holder(&h), None);
    assert!(!live.exists());
}

#[test]
fn sections_10_1_and_14_1_take_on_non_exclusive_is_usage_error() {
    let h = Harness::new();
    configure_backend_none(&h);
    let repo = h.repo("repo", RESOURCE);
    h.register(&repo);
    let output = h
        .wt()
        .args(["run", "service", "repo", "--take", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "TAKE_REQUIRES_EXCLUSIVE");
}

#[test]
fn section_10_1_take_destroy_failure_retains_holder_and_orphans_its_record() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let repo = h.repo(
        "repo",
        &format!(
            r#"
[task.service]
run = 'touch "{}"'
exists = 'test -f "{}"'
destroy = 'exit 9'
tied_to = "tree"
exclusive = "repo"
"#,
            live.display(),
            live.display(),
        ),
    );
    h.register(&repo);
    h.json(&["new", "repo/a", "--no-sync", "--no-open"]);
    h.json(&["new", "repo/b", "--no-sync", "--no-open"]);
    h.json(&["run", "service", "repo/a"]);

    let output = h
        .wt()
        .args(["run", "service", "repo/b", "--take", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "DESTROY_FAILED");
    assert_eq!(repo_exclusive_holder(&h).as_deref(), Some("repo/a"));
    let target = wt_core::model::Target::parse("repo/a").unwrap();
    let state = wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(
        &h.home.join(wt_core::model::tree_state_path(&target)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        state.resources["service"].state,
        wt_core::resource::ResourceState::Orphaned
    );
}

#[test]
fn sections_10_2_and_10_4_machine_exclusive_conflicts_across_labels() {
    let h = Harness::new();
    configure_backend_none(&h);
    let live = h.root.join("exclusive-live");
    let events = h.root.join("exclusive-events");
    let first = h.repo("first", &exclusive_config("machine", &live, &events));
    let second = h.repo("second", &exclusive_config("machine", &live, &events));
    h.register(&first);
    h.register(&second);
    h.json(&["run", "service", "first"]);

    let output = h
        .wt()
        .args(["run", "service", "second", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "RESOURCE_HELD");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("first"));
    let machine = wt_sys::fsx::read_json::<wt_core::lifecycle::RepoState>(
        &h.home.join(wt_core::model::machine_state_path()),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    assert_eq!(machine.exclusive["service"].holder, "first");
}

#[test]
fn list_probe_deduplicates_shared_resources_across_trees_and_status_probes_once() {
    let h = Harness::new();
    let probe_log = h.root.join("resource-probes");
    let repo = h.repo(
        "repo",
        &format!(
            r#"
[task.tree]
exists = 'printf "tree\n" >> "$PROBE_LOG"; false'
destroy = 'true'
tied_to = "tree"
[task.tree.env]
PROBE_LOG = "{}"
[task.repo]
exists = 'printf "repo\n" >> "$PROBE_LOG"; false'
destroy = 'true'
tied_to = "repo"
[task.repo.env]
PROBE_LOG = "{}"
[task.machine]
exists = 'printf "machine\n" >> "$PROBE_LOG"; false'
destroy = 'true'
tied_to = "machine"
[task.machine.env]
PROBE_LOG = "{}"
"#,
            probe_log.display(),
            probe_log.display(),
            probe_log.display(),
        ),
    );
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync", "--no-open"]);

    let listed = h.json(&["list", "--probe"]);
    let counts = || {
        let log = std::fs::read_to_string(&probe_log).unwrap();
        ["tree", "repo", "machine"]
            .map(|kind| log.lines().filter(|invocation| *invocation == kind).count())
    };
    assert_eq!(counts(), [2, 1, 1]);
    let trees = listed["data"]["trees"].as_array().unwrap();
    assert_eq!(trees.len(), 2);
    for tied_to in ["repo", "machine"] {
        let probes = trees
            .iter()
            .map(|tree| {
                tree["resources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|resource| resource["tied_to"] == tied_to)
                    .unwrap()["last_probe"]
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(probes[0]["result"], "absent");
        assert_eq!(probes[0], probes[1]);
    }

    common::write(&probe_log, "");
    h.json(&["status", "repo/work", "--probe"]);
    assert_eq!(counts(), [1, 1, 1]);
}

#[test]
fn section_14_5_status_and_list_order_resources_by_scope_axis() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
[task.tree]
exists = 'test -f "$WT_ROOT/.tree-resource"'
destroy = 'rm -f "$WT_ROOT/.tree-resource"'
tied_to = "tree"
[task.repo]
exists = 'test -f "$WT_REPO/.repo-resource"'
destroy = 'rm -f "$WT_REPO/.repo-resource"'
tied_to = "repo"
[task.machine]
exists = 'test -f "$WT_HOME/machine-resource"'
destroy = 'rm -f "$WT_HOME/machine-resource"'
tied_to = "machine"
"#,
    );
    h.register(&repo);

    for value in [
        h.json(&["status", "repo"])["data"].clone(),
        h.json(&["list"])["data"]["trees"][0].clone(),
    ] {
        assert_eq!(
            value["resources"]
                .as_array()
                .unwrap()
                .iter()
                .map(|resource| resource["tied_to"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["tree", "repo", "machine"]
        );
    }
}

#[test]
fn teardown_uses_frozen_instances_in_newest_first_order() {
    let h = Harness::new();
    let order = h.root.join("destroy-order");
    let sentinel = h.root.join("changed-destroy-ran");
    let config = format!(
        r#"
[task.older]
run = 'touch "$WT_ROOT/.older"'
exists = 'test -f "$WT_ROOT/.older"'
destroy = 'printf "older\\n" >> "$ORDER"; rm -f "$WT_ROOT/.older"'
tied_to = "tree"
[task.older.env]
ORDER = "{}"

[task.newer]
run = 'touch "$WT_ROOT/.newer"'
exists = 'test -f "$WT_ROOT/.newer"'
destroy = 'printf "newer\\n" >> "$ORDER"; rm -f "$WT_ROOT/.newer"'
tied_to = "tree"
[task.newer.env]
ORDER = "{}"
"#,
        order.display(),
        order.display(),
    );
    let repo = h.repo("repo", &config);
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync", "--no-open"]);
    let tree = Path::new(created["data"]["tree"]["path"].as_str().unwrap());
    h.json(&["run", "older", "repo/work"]);
    h.json(&["run", "newer", "repo/work"]);

    let target = wt_core::model::Target::parse("repo/work").unwrap();
    let state_path = h.home.join(wt_core::model::tree_state_path(&target));
    let mut state =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    let sequences = state
        .resources
        .values()
        .map(|record| {
            (
                record.key.task.clone(),
                record.instance.as_ref().unwrap().recorded_sequence,
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert!(sequences["older"] < sequences["newer"]);
    for record in state.resources.values_mut() {
        record.instance.as_mut().unwrap().recorded_at = "same-time".to_owned();
    }
    wt_sys::fsx::write_json(&state_path, &state).unwrap();

    let changed = format!(
        r#"
[task.older]
exists = 'test -f "$WT_ROOT/.older"'
destroy = 'touch "$SENTINEL"; rm -f "$WT_ROOT/.older"'
tied_to = "tree"
[task.older.env]
SENTINEL = "{}"

[task.newer]
exists = 'test -f "$WT_ROOT/.newer"'
destroy = 'touch "$SENTINEL"; rm -f "$WT_ROOT/.newer"'
tied_to = "tree"
[task.newer.env]
SENTINEL = "{}"
"#,
        sentinel.display(),
        sentinel.display(),
    );
    common::write(&tree.join(".wt.toml"), &changed);
    h.json(&["remove", "repo/work", "--yes", "--force"]);

    assert_eq!(std::fs::read_to_string(&order).unwrap(), "newer\nolder\n");
    assert!(!sentinel.exists());
    common::proof_capture(
        "D4",
        format!(
            "recorded sequence: older={} newer={}\ndestroy order:\n{}changed recipe sentinel exists: {}",
            sequences["older"],
            sequences["newer"],
            std::fs::read_to_string(&order).unwrap(),
            sentinel.exists()
        ),
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
fn acceptance_shaped_fixture_walks_register_new_run_open_remove() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
ports=['http']
bin=['bin']
copy=['secret.txt']
[vars]
app_port="{{ports.http}}"
[env]
APP_PORT='{{app_port}}'
[files.".wt/app.conf"]
content='port={{app_port}}'
[task.service]
run='touch "$WT_ROOT/.service"'
exists='test -f "$WT_ROOT/.service"'
destroy='rm -f "$WT_ROOT/.service"'
tied_to='tree'
"#,
    );
    wt_sys::fsx::create_private_dir(&repo.join("bin")).unwrap();
    common::write_executable(&repo.join("bin/tool"), "#!/bin/sh\nexit 0\n");
    common::write(&repo.join("secret.txt"), "copied\n");
    common::write(&repo.join(".git/info/exclude"), "secret.txt\nbin/tool\n");
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync"]);
    let tree = created["data"]["tree"]["path"].as_str().unwrap();
    assert!(Path::new(tree).join("secret.txt").exists());
    assert!(Path::new(tree).join(".wt/app.conf").exists());
    h.json(&["run", "service", "repo/work"]);
    common::write(
        &h.home.join("config.toml"),
        "[session]\nbackend='tmux'\nagent='probe'\n[agents.probe]\nstart=['true']\nresume=['true']\n",
    );
    h.json(&["open", "repo/work", "--no-attach"]);
    assert_eq!(
        h.json(&["remove", "repo/work", "--yes", "--force"])["data"]["removed"],
        true
    );
    assert!(!Path::new(tree).exists());
}

#[test]
fn sync_without_a_sync_task_never_bricks_the_canonical_tree() {
    let h = Harness::new();
    let repo = h.repo("repo", "[task.hello]\nrun='true'\n");
    h.register(&repo);
    h.wt()
        .args(["sync", "repo", "--json"])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("NOT_FOUND"));
    assert_eq!(h.json(&["status", "repo"])["data"]["phase"], "ready");
    h.wt()
        .args(["exec", "repo", "--", "true"])
        .assert()
        .success();
}

#[test]
fn new_sync_failure_is_failed_and_resumes_from_sync() {
    let h = Harness::new();
    let repo = h.repo("repo", "[task.sync]\nrun='exit 3'\n");
    h.register(&repo);
    h.wt()
        .args(["new", "repo/work", "--json"])
        .assert()
        .code(6)
        .stdout(predicate::str::contains("TASK_FAILED"));
    assert_eq!(h.json(&["status", "repo/work"])["data"]["phase"], "failed");
    let tree = h.json(&["path", "repo/work"])["data"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    common::write(
        &Path::new(&tree).join(".wt/config.toml"),
        "[task.sync]\nrun='true'\n",
    );
    let resumed = h.json(&["new", "repo/work"]);
    assert_eq!(resumed["data"]["resumed"], true);
    assert_eq!(resumed["data"]["tree"]["phase"], "ready");
}

#[test]
fn verify_failure_is_distinct_and_can_be_resumed() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[task.sync]\nrun='true'\n[task.verify]\nrun='exit 2'\n",
    );
    h.register(&repo);
    h.wt()
        .args(["new", "repo/work", "--verify", "--json"])
        .assert()
        .code(6)
        .stdout(predicate::str::contains("VERIFY_FAILED"));
    let status = h.json(&["status", "repo/work"]);
    assert_eq!(status["data"]["phase"], "ready");
    assert_eq!(status["data"]["verify"]["ok"], false);
    let tree = status["data"]["path"].as_str().unwrap();
    common::write(
        &Path::new(tree).join(".wt/config.toml"),
        "[task.verify]\nrun='true'\n",
    );
    let resumed = h.json(&["new", "repo/work", "--verify"]);
    assert_eq!(resumed["data"]["verify"]["ok"], true);
}

#[test]
fn present_resource_environment_is_identical_across_env_exec_and_run() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
[task.resource]
run='touch "$WT_ROOT/.resource"'
exists='test -f "$WT_ROOT/.resource"'
destroy='rm -f "$WT_ROOT/.resource"'
tied_to='tree'
[task.resource.env]
RES_ENV='from-resource'
[task.print]
run='printf %s "$RES_ENV"'
"#,
    );
    h.register(&repo);
    h.json(&["run", "resource", "repo"]);
    let env = h.json(&["env", "repo"])["data"]["env"]["RES_ENV"]
        .as_str()
        .unwrap()
        .to_owned();
    let exec = h
        .wt()
        .args(["exec", "repo", "--", "sh", "-c", "printf %s \"$RES_ENV\""])
        .output()
        .unwrap();
    let run = h.wt().args(["run", "print", "repo"]).output().unwrap();
    assert_eq!(env, "from-resource");
    assert_eq!(String::from_utf8(exec.stdout).unwrap(), env);
    assert_eq!(String::from_utf8(run.stdout).unwrap(), env);
}

#[test]
fn prune_records_then_new_creates_a_fresh_incarnation() {
    let h = Harness::new();
    let repo = h.repo("repo", RESOURCE);
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync"]);
    h.json(&["run", "service", "repo/work"]);
    let old_id = created["data"]["tree"]["tree_id"].as_str().unwrap();
    let path = created["data"]["tree"]["path"].as_str().unwrap();
    wt_sys::fsx::remove_path(Path::new(path)).unwrap();
    let pruned = h.json(&["prune", "--records", "repo/work", "--yes"]);
    assert_eq!(pruned["data"]["items"][0]["result"]["remaining"], 0);
    let recreated = h.json(&["new", "repo/work", "--no-sync"]);
    assert_ne!(recreated["data"]["tree"]["tree_id"], old_id);
}

#[test]
fn missing_tree_teardown_reports_exe_missing_and_the_actual_remaining_count() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "bin=['bin']\n[task.service]\nrun='mytool run'\nexists='mytool exists'\ndestroy='mytool destroy'\ntied_to='tree'\n",
    );
    wt_sys::fsx::create_private_dir(&repo.join("bin")).unwrap();
    common::write_executable(
        &repo.join("bin/mytool"),
        "#!/bin/sh\ncase \"$1\" in run) touch \"$WT_ROOT/.service\";; exists) test -f \"$WT_ROOT/.service\";; destroy) rm -f \"$WT_ROOT/.service\";; esac\n",
    );
    common::git(&repo, &["add", "bin/mytool"]);
    common::git(&repo, &["commit", "-qm", "fixture tool"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync"]);
    h.json(&["run", "service", "repo/work"]);
    let path = created["data"]["tree"]["path"].as_str().unwrap();
    wt_sys::fsx::remove_path(Path::new(path)).unwrap();
    let pruned = h.json(&["prune", "--records", "repo/work", "--yes"]);
    assert_eq!(pruned["data"]["items"][0]["result"]["remaining"], 1);
    let target = wt_core::model::Target::parse("repo/work").unwrap();
    let state = wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(
        &h.home.join(wt_core::model::tree_state_path(&target)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        state.resources.values().next().unwrap().reason.as_deref(),
        Some("exe_missing")
    );
}

#[test]
fn copy_populates_declared_paths_and_refuses_tracked_sources() {
    let copied = Harness::new();
    let repo = copied.repo("repo", "copy=['.env']\n");
    common::write(&repo.join(".env"), "TOKEN=local-only\n");
    copied.register(&repo);
    let created = copied.json(&["new", "repo/work", "--no-sync", "--no-build"]);
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
fn cargo_keys_intermediates_per_tree_and_keeps_tree_outputs_local() {
    let h = Harness::new();
    configure_backend_none(&h);
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
    h.json(&["new", "repo/work", "--no-sync", "--no-build"]);
    let canonical_env = h.json(&["env", "repo"])["data"]["env"].clone();
    let linked_env = h.json(&["env", "repo/work"])["data"]["env"].clone();
    // One build directory per tree, grouped under the repository: Cargo's
    // unit hashes ignore the workspace path, so trees sharing one directory
    // would overwrite each other's build-script output and freshness state.
    let repo_cache = h.home.join("cache/cargo-build/repo");
    let canonical_dir = repo_cache.join(wt_core::model::name_short("repo", "canonical"));
    let linked_dir = repo_cache.join(wt_core::model::name_short("repo", "work"));
    assert_ne!(canonical_dir, linked_dir);
    assert_eq!(
        canonical_env["CARGO_BUILD_BUILD_DIR"],
        canonical_dir.to_string_lossy().as_ref()
    );
    assert_eq!(
        linked_env["CARGO_BUILD_BUILD_DIR"],
        linked_dir.to_string_lossy().as_ref()
    );
    let overridden = h
        .wt()
        .env("CARGO_BUILD_BUILD_DIR", "/inherited/cargo-build")
        .args(["env", "repo", "--json"])
        .output()
        .unwrap();
    assert!(overridden.status.success());
    let overridden: serde_json::Value = serde_json::from_slice(&overridden.stdout).unwrap();
    assert_eq!(
        overridden["data"]["env"]["CARGO_BUILD_BUILD_DIR"],
        canonical_dir.to_string_lossy().as_ref()
    );
    assert!(overridden["data"]["overrode"]
        .as_array()
        .unwrap()
        .iter()
        .any(|key| key == "CARGO_BUILD_BUILD_DIR"));
    assert!(canonical_env.get("CARGO_TARGET_DIR").is_none());
    assert!(linked_env.get("CARGO_TARGET_DIR").is_none());
    let canonical_target = PathBuf::from(canonical_env["WT_ROOT"].as_str().unwrap()).join("target");
    let linked_target = PathBuf::from(linked_env["WT_ROOT"].as_str().unwrap()).join("target");
    assert_ne!(canonical_target, linked_target);

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
            "canonical build dir (per-tree): {}\nlinked build dir (per-tree): {}\nCARGO_TARGET_DIR set: false\ncanonical target: {}\nlinked target: {}\nsync inputs: {}",
            canonical_env["CARGO_BUILD_BUILD_DIR"],
            linked_env["CARGO_BUILD_BUILD_DIR"],
            canonical_target.display(),
            linked_target.display(),
            inputs.iter().copied().collect::<Vec<_>>().join(", ")
        ),
    );

    common::write(
        &repo.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.2.0'\nedition='2021'\n",
    );
    common::git(&repo, &["add", "Cargo.toml"]);
    common::git(&repo, &["commit", "-qm", "advance cargo input"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);
    let status = h.json(&["status", "repo/work"]);
    assert_eq!(
        status["data"]["sync"]["drift"],
        serde_json::json!(["Cargo.toml"])
    );
}

#[test]
fn sync_rechecks_tracked_render_paths_even_when_they_have_records() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[files.'.wt/generated']\ncontent='generated'\n[task.sync]\nrun='true'\n",
    );
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
fn truth_surfaces_report_descriptions_config_errors_and_live_locks() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[task.hello]\nrun='true'\ndescription='says hello'\n",
    );
    h.register(&repo);
    assert_eq!(
        h.json(&["status", "repo"])["data"]["tasks"][0]["description"],
        "says hello"
    );
    let mut child = h
        .wt_std()
        .args(["exec", "repo", "--", "sleep", "5"])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(!h.json(&["list"])["data"]["locks"]
        .as_array()
        .unwrap()
        .is_empty());
    let _ = child.kill();
    let _ = child.wait();

    common::write(&repo.join(".wt.toml"), "invalid = [\n");
    assert!(!h.json(&["status", "repo"])["data"]["config_errors"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        !h.json(&["register", repo.to_str().unwrap()])["data"]["config_errors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn sections_13_3_and_14_4_named_lock_capacity_reports_occupancy_and_waits() {
    fn wait_for(path: &Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out waiting for {}", path.display());
    }

    let h = Harness::new();
    let repo = h.repo(
        "repo",
        r#"
locks.serial = { slots = 2 }
[task.first]
run = 'touch "$WT_ROOT/.first"; while [ ! -e "$WT_ROOT/.release-first" ]; do sleep 0.01; done'
lock = "serial"
[task.second]
run = 'touch "$WT_ROOT/.second"; while [ ! -e "$WT_ROOT/.release-second" ]; do sleep 0.01; done'
lock = "serial"
[task.waiter]
run = 'touch "$WT_ROOT/.waiter"'
lock = "serial"
[task.legacy_first]
run = 'touch "$WT_ROOT/.legacy-first"; while [ ! -e "$WT_ROOT/.release-legacy" ]; do sleep 0.01; done'
lock = "legacy"
[task.legacy_second]
run = "true"
lock = "legacy"
"#,
    );
    h.register(&repo);

    let mut first = h
        .wt_std()
        .args(["run", "first", "repo", "--json"])
        .spawn()
        .unwrap();
    wait_for(&repo.join(".first"));
    let mut second = h
        .wt_std()
        .args(["run", "second", "repo", "--json"])
        .spawn()
        .unwrap();
    wait_for(&repo.join(".second"));

    let refused = h
        .wt()
        .args(["run", "waiter", "repo", "--json"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(4));
    let refused: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(refused["error"]["code"], "LOCK_HELD");
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("2/2 in use"));
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("slot 0: pid"));
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("slot 1: pid"));
    assert!(refused["error"]["remedy"]
        .as_str()
        .unwrap()
        .contains("locks.\"serial\".slots"));

    let locks = h.json(&["locks", "repo"]);
    let named = locks["data"]["locks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lock| lock["level"] == 4 && lock["name"] == "repo:serial")
        .unwrap();
    assert_eq!(named["held_slots"], 2);
    assert_eq!(named["slots"], 2);
    assert_eq!(named["holders"].as_array().unwrap().len(), 2);
    assert!(named["holders"][0]["path"]
        .as_str()
        .unwrap()
        .ends_with("/named/serial/0.lock"));
    h.wt()
        .args(["locks", "repo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("held 2/2"));

    let mut waiter = h
        .wt_std()
        .args(["run", "waiter", "repo", "--wait", "2s", "--json"])
        .spawn()
        .unwrap();
    common::write(&repo.join(".release-first"), "release\n");
    wait_for(&repo.join(".waiter"));
    assert!(waiter.wait().unwrap().success());
    assert!(first.wait().unwrap().success());
    common::write(&repo.join(".release-second"), "release\n");
    assert!(second.wait().unwrap().success());

    let mut legacy = h
        .wt_std()
        .args(["run", "legacy_first", "repo", "--json"])
        .spawn()
        .unwrap();
    wait_for(&repo.join(".legacy-first"));
    let legacy_refused = h
        .wt()
        .args(["run", "legacy_second", "repo", "--json"])
        .output()
        .unwrap();
    assert_eq!(legacy_refused.status.code(), Some(4));
    let legacy_refused: serde_json::Value = serde_json::from_slice(&legacy_refused.stdout).unwrap();
    assert!(legacy_refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("1/1 in use"));
    common::write(&repo.join(".release-legacy"), "release\n");
    assert!(legacy.wait().unwrap().success());
}

#[test]
fn missing_bins_are_silent_at_doors_and_reported_by_doctor() {
    let h = Harness::new();
    let repo = h.repo("repo", "bin=['missing-bin']\n[task.fail]\nrun='exit 2'\n");
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
    h.wt()
        .args(["run", "fail", "repo", "--json"])
        .assert()
        .code(6)
        .stdout(predicate::str::contains("BIN_DIR_MISSING").not());
}

#[test]
fn text_run_keeps_wt_guidance_off_the_child_stdout() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "bin=['missing-bin']\n[task.print-version]\nrun='printf 1.2.3'\n",
    );
    h.register(&repo);
    h.wt()
        .args(["run", "print-version", "repo"])
        .assert()
        .success()
        .stdout("1.2.3")
        .stderr(predicate::str::is_empty());
}

#[test]
fn remove_is_idempotent_and_text_run_preserves_child_status() {
    let h = Harness::new();
    let repo = h.repo("repo", "[task.fail]\nrun='exit 23'\n");
    h.register(&repo);
    h.wt().args(["run", "fail", "repo"]).assert().code(23);
    h.json(&["new", "repo/work", "--no-sync"]);
    assert_eq!(
        h.json(&["remove", "repo/work", "--yes"])["data"]["removed"],
        true
    );
    let repeated = h.json(&["remove", "repo/work", "--yes"]);
    assert_eq!(repeated["data"]["removed"], false);
    assert_eq!(repeated["notices"][0]["code"], "ALREADY_REMOVED");
    assert_eq!(repeated["notices"][0]["subject"], "repo/work");
    assert!(repeated["notices"][0]["message"]
        .as_str()
        .unwrap()
        .contains("No live tree exists"));

    h.wt()
        .args(["remove", "repo/work"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
        "No live tree exists for `repo/work`; its tombstone records that it was already removed",
    ));
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
fn doctor_manufactures_lifecycle_and_resource_conditions() {
    let h = Harness::new();
    let long_name = "x".repeat(64);
    let repo = h.repo(
        "repo",
        &format!(
            r#"
[task.service]
run='touch "$WT_ROOT/.service"'
exists='test -f "$WT_ROOT/.service"'
destroy='rm -f "$WT_ROOT/.service"'
tied_to='tree'
name='{{{{name()}}}}'
[task.long]
exists='false'
destroy='true'
tied_to='tree'
name='{long_name}'
"#
        ),
    );
    let registered = h.register(&repo);
    for name in [
        "interrupted",
        "initialising",
        "removing",
        "claimed",
        "missing",
        "incomplete",
        "replaced",
    ] {
        h.json(&["new", &format!("repo/{name}"), "--no-sync"]);
    }

    let mutate = |name: &str, phase: wt_core::lifecycle::StatePhase, verb| {
        let target = wt_core::model::Target::parse(&format!("repo/{name}")).unwrap();
        let path = h.home.join(wt_core::model::tree_state_path(&target));
        let mut state =
            wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&path, "STATE_CORRUPT")
                .unwrap()
                .unwrap();
        state.phase = phase;
        state.op = Some(wt_core::lifecycle::Operation {
            verb,
            pid: u32::MAX,
            started: wt_sys::fsx::timestamp().unwrap(),
        });
        wt_sys::fsx::write_json(&path, &state).unwrap();
    };
    mutate(
        "interrupted",
        wt_core::lifecycle::StatePhase::Bootstrapping,
        wt_core::lifecycle::OpVerb::Sync,
    );
    mutate(
        "initialising",
        wt_core::lifecycle::StatePhase::Initialising,
        wt_core::lifecycle::OpVerb::Adopt,
    );
    mutate(
        "removing",
        wt_core::lifecycle::StatePhase::Removing,
        wt_core::lifecycle::OpVerb::Remove,
    );
    mutate(
        "claimed",
        wt_core::lifecycle::StatePhase::Bootstrapping,
        wt_core::lifecycle::OpVerb::New,
    );
    for name in ["claimed", "missing"] {
        let path = h.json(&["path", &format!("repo/{name}")])["data"]["path"]
            .as_str()
            .unwrap()
            .to_owned();
        wt_sys::fsx::remove_path(Path::new(&path)).unwrap();
    }
    let incomplete = wt_core::model::Target::parse("repo/incomplete").unwrap();
    wt_sys::fsx::remove_path(&h.home.join(wt_core::model::tree_state_path(&incomplete))).unwrap();
    let replaced = h.json(&["path", "repo/replaced"])["data"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    common::write(&Path::new(&replaced).join(".wt/tree_id"), "replacement\n");

    let canonical = wt_core::model::Target::canonical(wt_core::model::Label::new("repo").unwrap());
    let state_path = h.home.join(wt_core::model::tree_state_path(&canonical));
    let mut state =
        wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(&state_path, "STATE_CORRUPT")
            .unwrap()
            .unwrap();
    state.verify_pending = true;
    state.last_error = Some("REFRESH_SKIPPED:.:service".to_owned());
    let mut records = state.resources.values_mut().collect::<Vec<_>>();
    records[0].state = wt_core::resource::ResourceState::Orphaned;
    records[0].undeclared = true;
    records[0].last_probe =
        Some(wt_core::resource::Probe::failed_exit(wt_sys::fsx::timestamp().unwrap(), 2).unwrap());
    records[1].state = wt_core::resource::ResourceState::Declared;
    records[1].instance = Some(records[1].declaration.clone());
    records[1].last_probe = Some(wt_core::resource::Probe::absent(
        wt_sys::fsx::timestamp().unwrap(),
    ));
    wt_sys::fsx::write_json(&state_path, &state).unwrap();

    let value = h.json(&["doctor", "repo"]);
    let codes = value["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|finding| finding["code"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    for code in [
        "TREE_INTERRUPTED",
        "INIT_INTERRUPTED",
        "REMOVE_INTERRUPTED",
        "TREE_CLAIMED",
        "TREE_MISSING",
        "TREE_MISSING_PENDING",
        "TREE_INCOMPLETE",
        "TREE_REPLACED",
        "VERIFY_PENDING",
        "RESOURCE_ORPHANED",
        "RESOURCE_GONE",
        "RESOURCE_UNDECLARED",
        "RESOURCE_PROBE_FAILED",
        "REFRESH_SKIPPED",
        "NAME_MAY_COLLIDE",
        "IDENTIFIER_LONG",
    ] {
        assert!(
            codes.contains(code),
            "missing manufactured doctor code {code}"
        );
    }
    assert_eq!(registered["data"]["tree"]["target"], "repo");
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
        "CACHE_ORPHAN",
        "REPO_PATH_MISSING",
        "TREE_REPLACED",
        "TREE_MISSING",
        "TREE_INCOMPLETE",
        "TREE_INTERRUPTED",
        "INIT_INTERRUPTED",
        "REMOVE_INTERRUPTED",
        "TREE_CLAIMED",
        "VERIFY_PENDING",
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
        "NO_VERIFY",
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
    common::write_executable(&h_rust.shims.join("sccache"), "#!/bin/sh\nexit 0\n");
    codes.extend(finding_codes(&h_rust.json(&["doctor", "rust"])));

    let h_node = Harness::new();
    let node = h_node.repo("node", "");
    common::write(&node.join("package.json"), "{\"name\":\"fixture\"}\n");
    common::write(&node.join("package-lock.json"), "{}\n");
    common::write_executable(&h_node.shims.join("npm"), "#!/bin/sh\nexit 0\n");
    common::write_executable(&h_node.shims.join("pnpm"), "#!/bin/sh\nexit 0\n");
    h_node.register(&node);
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
        "NO_VERIFY",
        "NO_COORDINATION",
        "ADAPTER_TOOL_MISSING",
        "ACCELERATOR_MISSING",
        "ACCELERATOR_INACTIVE",
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
fn sync_mid_failpoint_reruns_to_ready() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[task.prep]\nrun='true'\n[task.sync]\nrun='true'\nneeds=['prep']\n",
    );
    h.register(&repo);
    h.wt()
        .env("WT_FAILPOINT", "sync.mid:exit")
        .args(["sync", "repo"])
        .assert()
        .code(86);
    assert_eq!(h.json(&["sync", "repo"])["ok"], true);
    assert_eq!(h.json(&["status", "repo"])["data"]["phase"], "ready");
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
fn resource_frozen_failpoint_reruns_from_probe_truth() {
    let h = Harness::new();
    let repo = h.repo("repo", RESOURCE);
    h.register(&repo);
    h.wt()
        .env("WT_FAILPOINT", "resource.frozen:exit")
        .args(["run", "service", "repo"])
        .assert()
        .code(86);
    assert_eq!(h.json(&["run", "service", "repo"])["ok"], true);
}

#[cfg(feature = "failpoints")]
#[test]
fn resource_destroyed_failpoint_reruns_from_probe_truth() {
    let h = Harness::new();
    let repo = h.repo("repo", RESOURCE);
    h.register(&repo);
    h.json(&["run", "service", "repo"]);
    h.wt()
        .env("WT_FAILPOINT", "resource.destroyed:exit")
        .args(["destroy", "service", "repo", "--yes"])
        .assert()
        .code(86);
    assert_eq!(
        h.json(&["destroy", "service", "repo", "--yes"])["data"]["after"],
        "declared"
    );
}

/// A76 §14.7: `setup` is the sole terminal-primary verb, and it says so in the
/// two ways an automated caller can reach it — while `--dry-run`, which asks
/// nothing, works anywhere.
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

/// A76 §14.7, end to end without a terminal: the walk, the git batch, the
/// grouping, the proposals and the default answers, printed as the commands
/// that would produce them.
#[test]
fn setup_dry_run_plans_the_default_answers_as_commands() {
    let h = Harness::new();
    // `infocmp` decides the generated tmux file's `default-terminal`, and
    // machines differ; the shim makes the answer the same everywhere.
    common::write_executable(&h.shims.join("infocmp"), "#!/bin/sh\nexit 0\n");

    // A checkout with a linked worktree, both untouched by wt.
    let api = h.repo("api", "");
    common::git(
        &api,
        &["worktree", "add", "-q", "-b", "feature", "../api-feature"],
    );
    let feature = h.repos.join("api-feature");
    // A checkout wt already knows, whose own worktree is still adoptable.
    let old = h.repo("old", "");
    h.register(&old);
    common::git(&old, &["worktree", "add", "-q", "-b", "wip", "../old-wip"]);
    let wip = h.repos.join("old-wip");

    let output = h
        .wt()
        .args(["setup", "--dry-run", "--shell", "zsh"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let real = |path: &Path| {
        wt_sys::fsx::canonicalize(path)
            .unwrap()
            .display()
            .to_string()
    };
    let register = format!("wt register {} --label api", real(&api));
    let adopt = format!("wt adopt {} --label api --name feature", real(&feature));
    let adopt_wip = format!("wt adopt {} --label old --name wip", real(&wip));
    for line in [&register, &adopt, &adopt_wip] {
        assert!(
            stdout.contains(line.as_str()),
            "missing `{line}` in:\n{stdout}"
        );
    }
    assert!(
        stdout.find(&register).unwrap() < stdout.find(&adopt).unwrap(),
        "a register precedes the adopt that needs its label:\n{stdout}"
    );
    assert!(
        !stdout.contains(&format!("wt register {}", real(&old))),
        "a registered checkout is not registered again:\n{stdout}"
    );
    // The origins are bare mirrors beside the checkouts; they are neither
    // offered nor descended.
    assert!(!stdout.contains("api-origin"), "{stdout}");

    // The shell block is printed byte for byte, guard included, into the
    // file the detected shell reads.
    assert!(
        stdout.contains(&format!("cat >> {}/.zshrc <<'WT_SETUP'", h.root.display())),
        "{stdout}"
    );
    assert!(stdout.contains("eval \"$(wt shell-init zsh)\""), "{stdout}");
    assert!(
        stdout.contains("eval \"$(wt completions zsh)\""),
        "{stdout}"
    );
    assert!(stdout.contains("# >>> wt >>>"), "{stdout}");

    // tmux (the shim) is installed and unconfigured, so a configuration is
    // written, describing the terminal wt is running under.
    assert!(
        stdout.contains(&format!(
            "cat > {}/.config/tmux/tmux.conf <<'WT_SETUP'",
            h.root.display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("xterm-256color:extkeys"), "{stdout}");
    assert!(
        stdout.contains("default-terminal \"tmux-256color\""),
        "{stdout}"
    );
    // The bare mirror's HEAD/objects/refs must not have been taken for a
    // worktree, and nothing was written.
    assert!(!h.root.join(".zshrc").exists());
    assert!(!h.root.join(".config/tmux/tmux.conf").exists());
    assert!(
        !h.root.join("wt-home/config.toml").exists()
            || !std::fs::read_to_string(h.root.join("wt-home/config.toml"))
                .unwrap()
                .contains("trees_dir")
    );
}

/// A76: a second run offers only what is new, and an rc file that already
/// installs the guard is not appended to again.
#[test]
fn setup_dry_run_is_idempotent_once_everything_is_done() {
    let h = Harness::new();
    common::write_executable(&h.shims.join("infocmp"), "#!/bin/sh\nexit 0\n");
    let api = h.repo("api", "");
    h.register(&api);
    common::write(&h.root.join(".zshrc"), "eval \"$(wt shell-init zsh)\"\n");
    common::write(&h.root.join(".tmux.conf"), "set -g mouse on\n");

    let output = h
        .wt()
        .args(["setup", "--dry-run", "--shell", "zsh"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(!stdout.contains("wt register"), "{stdout}");
    assert!(
        !stdout.contains("shell-init"),
        "an installed guard is not installed twice:\n{stdout}"
    );
    assert!(
        !stdout.contains("cat > "),
        "an existing tmux configuration is never replaced:\n{stdout}"
    );
}

/// A76: the guard is invisible from inside the door it breaks, so `doctor`
/// reports its absence — and stops once any rc file installs it.
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
