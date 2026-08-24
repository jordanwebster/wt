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
}

#[test]
fn bin_directory_guidance_is_a_summary_fact_even_when_quiet() {
    let h = Harness::new();
    let repo = h.repo("repo", HUMAN_FIXTURE);
    h.register(&repo);
    let output = h.pty_output(&["new", "repo/work", "--no-sync", "--quiet"], b"");
    assert_eq!(output.child.code, Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("wt: BIN_DIR_MISSING"));
    assert!(text.contains("  next"));
    assert!(text.contains("wt build repo/work"));
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
    insta::assert_snapshot!(name, normalize(h, &output.stdout));
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
