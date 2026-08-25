mod common;

use std::path::Path;

use common::Harness;

const HUMAN_FIXTURE: &str = r#"
bin = ["bin"]
sync_inputs = ["README.md"]
[task.sync]
run = "true"
[task.build]
run = "true"
[task.test]
run = "true"
[task.lint]
run = "true"
[task.fmt]
run = "true"
[task.hello]
run = "true"
[task.service]
run = "touch \"$WT_ROOT/.service\""
exists = "test -f \"$WT_ROOT/.service\""
destroy = "rm -f \"$WT_ROOT/.service\""
tied_to = "tree"
"#;

#[test]
fn human_command_transcripts() {
    let h = Harness::new();
    let repo = h.repo("repo", HUMAN_FIXTURE);

    transcript(&h, "register", &["register", repo.to_str().unwrap()]);
    transcript(&h, "new", &["new", "repo/work", "--no-sync"]);
    transcript(&h, "list", &["list"]);
    transcript(&h, "status", &["status", "repo/work"]);
    transcript(&h, "doctor", &["doctor", "repo"]);
    transcript(&h, "sync", &["sync", "repo/work"]);
    transcript(&h, "tasks", &["tasks", "repo/work"]);
    transcript(&h, "config", &["config", "repo/work", "--origin"]);
    transcript(&h, "which", &["which", "repo/work", "git"]);
    transcript(&h, "locks", &["locks", "repo"]);
    transcript(&h, "remove", &["remove", "repo/work", "--yes"]);
    transcript_failure(&h, "remove_refused", &["remove", "repo", "--yes"], 2);
}

#[test]
fn redirected_output_matches_the_terminal_text() {
    let h = Harness::new();
    let repo = h.repo("repo", HUMAN_FIXTURE);
    h.register(&repo);

    for args in [
        vec!["status", "repo"],
        vec!["tasks", "repo"],
        vec!["config", "repo", "--origin"],
        vec!["locks", "repo"],
    ] {
        let terminal = h.pty_output(&args, b"");
        assert_eq!(terminal.child.code, Some(0));
        let redirected = h.wt().args(&args).output().unwrap();
        assert!(redirected.status.success());
        assert!(redirected.stderr.is_empty());
        assert_eq!(terminal_text(&terminal.stdout), redirected.stdout);
    }

    let terminal_h = Harness::new();
    let terminal_repo = terminal_h.repo("repo", HUMAN_FIXTURE);
    terminal_h.register(&terminal_repo);
    let terminal = terminal_h.pty_output(&["new", "repo/work", "--no-sync"], b"");
    assert_eq!(terminal.child.code, Some(0));

    let redirected_h = Harness::new();
    let redirected_repo = redirected_h.repo("repo", HUMAN_FIXTURE);
    redirected_h.register(&redirected_repo);
    let redirected = redirected_h
        .wt()
        .args(["new", "repo/work", "--no-sync"])
        .output()
        .unwrap();
    assert!(redirected.status.success());
    assert_eq!(
        terminal_text(normalize(&terminal_h, &terminal.stdout).as_bytes()),
        normalize(&redirected_h, &redirected.stdout).into_bytes()
    );
    common::proof_capture(
        "C3",
        normalize(&terminal_h, &terminal.stdout).replace("\r\n", "\n"),
    );
}

#[test]
fn missing_bin_guidance_is_exclusive_to_doctor() {
    let h = Harness::new();
    let repo = h.repo("repo", HUMAN_FIXTURE);
    h.register(&repo);
    let output = h.pty_output(&["new", "repo/work", "--no-sync", "--quiet"], b"");
    assert_eq!(output.child.code, Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("wt: BIN_DIR_MISSING"));
    assert!(!text.contains("  next"));
    assert!(!text.contains("wt build repo/work"));
    let doctor = h.json(&["doctor", "repo"]);
    let finding = doctor["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "BIN_DIR_MISSING" && finding["subject"] == "repo/work")
        .unwrap();
    common::proof_capture(
        "F1",
        format!(
            "door output:\n{}\ndoctor finding:\n{}",
            text.trim_end(),
            serde_json::to_string_pretty(finding).unwrap()
        )
        .replace(
            &std::fs::canonicalize(&h.root)
                .unwrap_or_else(|_| h.root.clone())
                .to_string_lossy()
                .to_string(),
            "<ROOT>",
        )
        .replace(&h.root.to_string_lossy().to_string(), "<ROOT>"),
    );
}

#[test]
fn new_calls_skipped_sync_nodes_skipped() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[task.sync]\nrun='true'\nneeds=['noop']\n[task.noop]\nrun='true'\nexists='true'\n",
    );
    h.register(&repo);
    let output = h.wt().args(["new", "repo/work"]).output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("1 passed, 1 skipped"), "{text}");
    assert!(!text.contains("1/2 passed"), "{text}");
    common::proof_capture("G2", text.trim_end());
}

#[test]
fn canonical_repair_transcript() {
    let h = Harness::new();
    let repo = h.repo(
        "repo",
        "[files.'.wt/generated']\ncontent='generated for ${target()}'\n",
    );
    h.register(&repo);
    wt_sys::fsx::remove_path(&repo.join(".wt/tree_id")).unwrap();
    wt_sys::fsx::remove_path(&repo.join(".wt/generated")).unwrap();

    transcript(&h, "doctor_repair", &["doctor", "repo"]);
    transcript(
        &h,
        "register_repair",
        &[
            "register",
            repo.to_str().unwrap(),
            "--label",
            "repo",
            "--repair",
        ],
    );
}

#[test]
fn every_successful_verb_uses_intentional_human_text() {
    let h = Harness::new();
    let repo = h.repo("repo", HUMAN_FIXTURE);
    human(&h, &["register", repo.to_str().unwrap()], b"");

    for args in [
        vec!["list"],
        vec!["status", "repo"],
        vec!["path", "repo"],
        vec!["tasks", "repo"],
        vec!["config", "repo", "--origin"],
        vec!["which", "repo", "git"],
        vec!["locks", "repo"],
        vec!["doctor", "repo"],
        vec!["env", "repo"],
        vec!["run", "hello", "repo"],
        vec!["run", "hello", "repo", "--dry-run"],
        vec!["test", "repo"],
        vec!["lint", "repo"],
        vec!["fmt", "repo"],
        vec!["build", "repo"],
        vec!["sync", "repo"],
    ] {
        human(&h, &args, b"");
    }

    human(&h, &["run", "service", "repo"], b"");
    human(&h, &["destroy", "service", "repo", "--yes"], b"");
    human(&h, &["run", "service", "repo"], b"");
    human(&h, &["refresh", "service", "repo", "--yes"], b"");
    human(
        &h,
        &["open", "repo", "--agent", "codex", "--no-attach"],
        b"",
    );
    human(&h, &["close", "repo"], b"");
    human(&h, &["new", "repo/work", "--no-sync"], b"");

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
    human(
        &h,
        &[
            "adopt",
            adopted.to_str().unwrap(),
            "--label",
            "repo",
            "--name",
            "adopted",
        ],
        b"",
    );
    human(&h, &["remove", "repo/work", "--yes"], b"");
    human(&h, &["remove", "repo/adopted", "--yes", "--force"], b"");
    human(&h, &["prune", "--yes"], b"");

    let clone_path = h.root.join("cloned");
    let origin = h.repos.join("repo-origin.git");
    human(
        &h,
        &[
            "clone",
            origin.to_str().unwrap(),
            "--label",
            "cloned",
            "--path",
            clone_path.to_str().unwrap(),
        ],
        b"",
    );
    human(&h, &["unregister", "cloned", "--yes"], b"");
    human(&h, &["shell-init", "zsh"], b"");
    human(&h, &["completions", "zsh"], b"");
    human(&h, &["exec", "repo", "--", "printf", "exec"], b"");
    human(&h, &["shell", "repo"], b"exit\n");
    human(&h, &["unregister", "repo", "--yes"], b"");
}

fn transcript(h: &Harness, name: &str, args: &[&str]) {
    let output = h.pty_output(args, b"");
    assert_eq!(
        output.child.code,
        Some(0),
        "{name}: {}",
        display(&output.stdout)
    );
    let normalized = normalize(h, &output.stdout);
    if name == "doctor_repair" {
        common::proof_capture("A4", &normalized);
    }
    insta::assert_snapshot!(name, normalized);
}

fn transcript_failure(h: &Harness, name: &str, args: &[&str], code: i32) {
    let output = h.pty_output(args, b"");
    assert_eq!(output.child.code, Some(code));
    insta::assert_snapshot!(name, normalize(h, &output.stdout));
}

fn human(h: &Harness, args: &[&str], input: &[u8]) {
    let output = h.pty_output(args, input);
    assert_eq!(
        output.child.code,
        Some(0),
        "{}: {}",
        args.join(" "),
        display(&output.stdout)
    );
    let text = display(&output.stdout);
    assert!(
        !text.trim_start().starts_with('{'),
        "{} emitted a JSON object: {text}",
        args.join(" ")
    );
    common::proof_capture(
        "C1",
        format!(
            "$ wt {} => {}",
            args.join(" "),
            normalize(h, &output.stdout)
                .lines()
                .next()
                .unwrap_or("<empty child output>")
        ),
    );
}

fn normalize(h: &Harness, bytes: &[u8]) -> String {
    let canonical_root = wt_sys::fsx::canonicalize(&h.root).unwrap();
    display(bytes)
        .replace(&canonical_root.to_string_lossy().to_string(), "<ROOT>")
        .replace(&h.root.to_string_lossy().to_string(), "<ROOT>")
        .replace(env!("CARGO_BIN_EXE_wt"), "<WT>")
}

fn terminal_text(bytes: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .into_bytes()
}

fn display(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn harness_never_uses_operator_state() {
    let h = Harness::new();
    assert!(h.home.starts_with(&h.root));
    assert!(!h.home.starts_with(Path::new("/Users/jlw/.wt")));
}
