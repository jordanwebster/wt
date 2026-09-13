mod common;

use std::path::Path;

use common::Harness;

const COMPLETE: &str = r#"
ports = ["http"]
sync_inputs = ["README.md"]
[env]
APP_PORT = "{{ports.http}}"
[files.".wt/generated"]
content = "port={{ports.http}}"
[hooks]
after_new = "true"
warm = "true"
[resource.service]
create = "touch \"$WT_ROOT/.service\""
exists = "test -f \"$WT_ROOT/.service\""
destroy = "rm -f \"$WT_ROOT/.service\""
tied_to = "tree"
"#;

#[test]
fn every_verb_has_a_normalized_json_contract_snapshot() {
    let h = Harness::new();
    let repo = h.repo("repo", COMPLETE);
    snapshot(&h, "register", &["register", repo.to_str().unwrap()]);
    snapshot(&h, "list", &["list", "--disk"]);
    snapshot(&h, "status", &["status", "repo"]);
    snapshot(&h, "path", &["path", "repo"]);
    snapshot(&h, "config", &["config", "repo", "--origin"]);
    snapshot(&h, "which", &["which", "repo", "git"]);
    snapshot(&h, "leases", &["leases", "repo"]);
    snapshot(&h, "doctor", &["doctor", "repo"]);
    snapshot(&h, "env", &["env", "repo"]);
    snapshot_at(&h, "with", &["with", "service"], &repo);
    snapshot(&h, "sync", &["sync", "repo"]);
    snapshot(&h, "anchor", &["anchor", "repo"]);

    assert!(h
        .wt()
        .current_dir(&repo)
        .args(["with", "service", "--json"])
        .output()
        .unwrap()
        .status
        .success());
    snapshot(&h, "destroy", &["destroy", "service", "repo", "--yes"]);
    assert!(h
        .wt()
        .current_dir(&repo)
        .args(["with", "service", "--json"])
        .output()
        .unwrap()
        .status
        .success());
    snapshot(&h, "refresh", &["refresh", "service", "repo", "--yes"]);

    snapshot(&h, "open", &["open", "repo", "--no-attach"]);
    snapshot(&h, "close", &["close", "repo"]);
    snapshot(
        &h,
        "new",
        &["new", "repo/work", "--no-sync", "--no-after-new"],
    );
    snapshot(&h, "meta", &["meta", "repo/work", "ticket=ABC-123"]);
    snapshot(&h, "remove", &["remove", "repo/work", "--yes"]);
    snapshot(&h, "prune", &["prune", "--yes"]);

    let adopted_path = h.root.join("adopted");
    common::git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "adopted",
            adopted_path.to_str().unwrap(),
        ],
    );
    snapshot(
        &h,
        "adopt",
        &[
            "adopt",
            adopted_path.to_str().unwrap(),
            "--label",
            "repo",
            "--name",
            "adopted",
        ],
    );
    snapshot(&h, "forget", &["forget", "repo/adopted", "--yes"]);

    let source = h.repo("source", "");
    let clone_path = h.root.join("clone");
    snapshot(
        &h,
        "clone",
        &[
            "clone",
            source.to_str().unwrap(),
            "--label",
            "clone",
            "--path",
            clone_path.to_str().unwrap(),
        ],
    );
    snapshot(&h, "shell-init", &["shell-init", "zsh"]);
    snapshot(&h, "completions", &["completions", "zsh"]);
    snapshot(&h, "exec", &["exec", "repo", "--", "true"]);
    snapshot(&h, "shell", &["shell", "repo"]);
    snapshot(
        &h,
        "unregister",
        &["unregister", "repo", "--force", "--yes"],
    );

    assert!(Path::new(&clone_path).exists());
}

fn snapshot(h: &Harness, name: &str, args: &[&str]) {
    snapshot_command(h, name, h.wt().arg("--json").args(args));
}

fn snapshot_at(h: &Harness, name: &str, args: &[&str], cwd: &Path) {
    snapshot_command(h, name, h.wt().current_dir(cwd).arg("--json").args(args));
}

fn snapshot_command(h: &Harness, name: &str, command: &mut assert_cmd::Command) {
    let output = command.output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{name} emitted invalid JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    insta::with_settings!({sort_maps => true}, {
        insta::assert_json_snapshot!(name, normalize(value, &h.root));
    });
}

fn normalize(mut value: serde_json::Value, root: &Path) -> serde_json::Value {
    fn walk(value: &mut serde_json::Value, root: &str, canonical_root: &str) {
        match value {
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    if matches!(
                        key.as_str(),
                        "version"
                            | "at"
                            | "since"
                            | "started"
                            | "recorded_at"
                            | "removed_at"
                            | "duration_ms"
                            | "log"
                            | "head"
                            | "detached_sha"
                            | "pid"
                            | "tree_id"
                            | "disk_kb"
                            | "build_kb"
                            | "gitdir_id"
                    ) && !value.is_null()
                    {
                        *value = serde_json::Value::String(format!("<{key}>"));
                    } else {
                        walk(value, root, canonical_root);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    walk(value, root, canonical_root);
                }
            }
            serde_json::Value::String(text) => {
                *text = text.replace(canonical_root, "$ROOT").replace(root, "$ROOT");
            }
            _ => {}
        }
    }

    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    walk(
        &mut value,
        &root.to_string_lossy(),
        &canonical_root.to_string_lossy(),
    );
    value
}
