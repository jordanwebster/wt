mod common;

use std::path::Path;

use common::Harness;
use predicates::prelude::*;
use std::collections::BTreeSet;

const BASIC: &str = r#"
ports = ["http"]
[env]
APP_PORT = "${port('http')}"
[files.".wt/generated"]
content = "port=${port('http')}"
[task.hello]
run = "printf hello"
"#;

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
    let second = h.register(&repo);
    assert_eq!(second["data"]["registered"], false);
}

#[test]
fn truth_and_inspection_verbs_match_the_registered_tree() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    assert_eq!(h.json(&["list"])["data"]["trees"][0]["target"], "repo");
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
    assert_eq!(
        h.json(&["close", "repo"])["data"]["sessions"][0]["closed"],
        true
    );
}

#[test]
fn shell_init_and_completions_are_script_envelopes() {
    let h = Harness::new();
    let init = h.json(&["shell-init", "zsh"]);
    assert!(init["data"]["script"].as_str().unwrap().contains("wtcd"));
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
    assert_eq!(sessions.len(), 3);
    assert!(
        sessions.iter().any(|session| {
            session["target"] == "repo/one"
                && session["failed"] == true
                && session["code"] == "TREE_REPLACED"
        }),
        "{envelope}"
    );
    for target in ["repo", "repo/two"] {
        assert!(sessions
            .iter()
            .any(|session| { session["target"] == target && session["created"] == true }));
    }
    common::proof_capture(
        "B6-partial",
        serde_json::to_string_pretty(&envelope)
            .unwrap()
            .replace(&h.root.to_string_lossy().to_string(), "<ROOT>"),
    );
}

#[test]
fn non_tty_destruction_requires_confirmation() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    h.json(&["new", "repo/work", "--no-sync"]);
    h.wt()
        .args(["remove", "repo/work"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("CONFIRM_REQUIRED"));
}

#[test]
fn tty_consent_accepts_and_declines_through_a_pseudoterminal() {
    let h = Harness::new();
    let repo = h.repo("repo", BASIC);
    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync"]);
    let path = Path::new(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(h.pty_status(&["remove", "repo/work"], b"n\n").code, Some(0));
    assert!(path.exists());
    assert_eq!(h.pty_status(&["remove", "repo/work"], b"y\n").code, Some(0));
    assert!(!path.exists());
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
        "[files.'.wt/generated']\ncontent='restored ${target()}'\n[task.service]\nrun='true'\nexists='false'\ndestroy='true'\ntied_to='tree'\n",
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
app_port="${port('http')}"
[env]
APP_PORT='${app_port}'
[files.".wt/app.conf"]
content='port=${app_port}'
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
fn copy_reporting_distinguishes_absent_and_tracked_sources() {
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
fn cargo_adapter_seeds_new_trees_and_tracks_adapter_sync_inputs() {
    let h = Harness::new();
    common::write_executable(&h.shims.join("cargo"), "#!/bin/sh\nexit 0\n");
    let repo = h.repo("repo", "");
    common::write(
        &repo.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\nedition='2021'\n",
    );
    common::write(&repo.join("Cargo.lock"), "# generated fixture lockfile\n");
    common::write(&repo.join(".gitignore"), "/target/\n");
    wt_sys::fsx::create_private_dir(&repo.join("target")).unwrap();
    common::write(&repo.join("target/cache.bin"), "warm cache\n");
    common::git(&repo, &["add", "Cargo.toml", "Cargo.lock", ".gitignore"]);
    common::git(&repo, &["commit", "-qm", "add cargo fixture"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);

    let probe_root = h.root.join("reflink-probe");
    wt_sys::fsx::create_private_dir(&probe_root).unwrap();
    let probe = wt_sys::fsx::copy_contained(
        &repo,
        &probe_root,
        &wt_core::model::RelPath::new("target").unwrap(),
        wt_sys::fsx::CopyPolicy::PreferReflink,
    )
    .unwrap();
    let reflink_supported = probe.files.iter().all(|file| file.reflinked);

    h.register(&repo);
    let created = h.json(&["new", "repo/work", "--no-sync"]);
    let tree_root = Path::new(created["data"]["tree"]["path"].as_str().unwrap());
    let skipped = created["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["code"] == "SEED_SKIPPED_NO_REFLINK");
    assert_eq!(skipped, !reflink_supported);
    assert_eq!(
        wt_sys::fsx::read_string(&tree_root.join("target/cache.bin")).unwrap(),
        reflink_supported.then(|| "warm cache\n".to_owned())
    );
    assert!(!created["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["code"] == "SEED_COPIED_NOT_CLONED"));

    let target = wt_core::model::Target::parse("repo/work").unwrap();
    let state = wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(
        &h.home.join(wt_core::model::tree_state_path(&target)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        state.materialized.iter().any(|entry| {
            entry.path == "target" && entry.kind == wt_core::lifecycle::MaterializedKind::Seeded
        }),
        reflink_supported
    );

    let synced = h.json(&["sync", "repo/work"]);
    let inputs = synced["data"]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|input| input["path"].as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(inputs, BTreeSet::from(["Cargo.lock", "Cargo.toml"]));
    common::proof_capture(
        "A1",
        format!(
            "reflink supported: {reflink_supported}\nadapter seed present: {}\nSEED_SKIPPED_NO_REFLINK: {skipped}\nsync inputs: {}",
            tree_root.join("target/cache.bin").exists(),
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
    assert_eq!(
        h.json(&["remove", "repo/work", "--yes"])["data"]["removed"],
        false
    );
}

#[test]
fn shell_scripts_do_not_depend_on_a_healthy_home() {
    let h = Harness::new();
    common::write(&h.home.join("registry.toml"), "old=true");
    h.wt()
        .args(["shell-init", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("wtcd"));
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
name='${{name()}}'
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
