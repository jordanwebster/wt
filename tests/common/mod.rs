#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use wt_sys::proc::{self, CommandRequest};

pub struct Harness {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub repos: PathBuf,
    pub shims: PathBuf,
    pub shim_state: PathBuf,
}

impl Harness {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let home = root.join("wt-home");
        let repos = root.join("repos");
        let shims = root.join("shims");
        let shim_state = root.join("shim-state");
        for path in [&home, &repos, &shims, &shim_state] {
            wt_sys::fsx::create_private_dir(path).unwrap();
        }
        let harness = Self {
            _tmp: tmp,
            root,
            home,
            repos,
            shims,
            shim_state,
        };
        harness.install_tmux();
        harness
    }

    pub fn wt(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wt"));
        command
            .env_clear()
            .env("WT_HOME", &self.home)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env("TERM", "xterm-256color")
            .env("WT_SHIM_STATE", &self.shim_state)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.shims.display()),
            )
            .env_remove("TMUX");
        command
    }

    #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
    pub fn wt_std(&self) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_wt"));
        command
            .env_clear()
            .env("WT_HOME", &self.home)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env("TERM", "xterm-256color")
            .env("WT_SHIM_STATE", &self.shim_state)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.shims.display()),
            )
            .env_remove("TMUX");
        command
    }

    pub fn pty_status(&self, args: &[&str], input: &[u8]) -> wt_core::resource::ChildStatus {
        wt_sys::proc::pty_status(&self.pty_request(args), input).unwrap()
    }

    pub fn pty_output(&self, args: &[&str], input: &[u8]) -> wt_sys::proc::ProcessOutput {
        wt_sys::proc::pty_capture(&self.pty_request(args), input, Duration::from_secs(10)).unwrap()
    }

    pub fn pty_request(&self, args: &[&str]) -> CommandRequest {
        let mut request = CommandRequest::new(env!("CARGO_BIN_EXE_wt"));
        request.args = proc::os_args(args);
        request.clear_env = true;
        request.env = [
            (
                "WT_HOME".to_owned(),
                self.home.to_string_lossy().into_owned(),
            ),
            ("HOME".to_owned(), self.root.to_string_lossy().into_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            (
                "WT_SHIM_STATE".to_owned(),
                self.shim_state.to_string_lossy().into_owned(),
            ),
            (
                "PATH".to_owned(),
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.shims.display()),
            ),
        ]
        .into_iter()
        .collect();
        request
    }

    pub fn repo(&self, name: &str, config: &str) -> PathBuf {
        let path = self.repos.join(name);
        wt_sys::fsx::create_private_dir(&path).unwrap();
        git(&path, &["init", "-q"]);
        write(&path.join("README.md"), "fixture\n");
        if !config.is_empty() {
            write(&path.join(".wt.toml"), config);
        }
        git(&path, &["add", "-A"]);
        git(&path, &["commit", "-qm", "fixture"]);
        let origin = self.repos.join(format!("{name}-origin.git"));
        wt_sys::fsx::create_private_dir(&origin).unwrap();
        git(&origin, &["init", "--bare", "-q"]);
        git(
            &path,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&path, &["push", "-qu", "origin", "main"]);
        path
    }

    pub fn push_pull_ref(&self, repo: &Path, number: u64, revision: &str) {
        self.push_ref(repo, revision, &format!("refs/pull/{number}/head"));
    }

    pub fn push_ref(&self, repo: &Path, revision: &str, reference: &str) {
        git(
            repo,
            &["push", "-q", "origin", &format!("{revision}:{reference}")],
        );
    }

    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.wt().args(args).arg("--json").output().unwrap();
        assert!(
            output.status.success(),
            "status: {:?}\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    pub fn register(&self, repo: &Path) -> serde_json::Value {
        self.json(&["register", repo.to_str().unwrap()])
    }

    fn install_tmux(&self) {
        write_executable(
            &self.shims.join("tmux"),
            r#"#!/bin/sh
s="$WT_SHIM_STATE/tmux"; mkdir -p "$s"
case "$1" in
  -V) echo "tmux 3.4" ;;
  has-session) n=${3#=}; [ -d "$s/$n" ] ;;
  new-session)
    shift; n=""; c="."
    while [ $# -gt 0 ]; do
      case "$1" in
        -d) shift ;;
        -s) n=$2; shift 2 ;;
        -c) c=$2; shift 2 ;;
        --) shift; break ;;
        *) shift ;;
      esac
    done
    mkdir -p "$s/$n"; printf '%s\n' "$c" > "$s/$n/cwd"; printf '%s\n' "$@" > "$s/$n/argv" ;;
  set-option) : ;;
  kill-session) n=${3#=}; rm -rf "$s/$n" ;;
  switch-client|attach-session) : ;;
esac
"#,
        );
    }
}

pub fn write_executable(path: &Path, body: &str) {
    let parent = path.parent().unwrap();
    wt_sys::fsx::create_private_dir(parent).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    // A file still held open for writing cannot be executed (ETXTBSY), and
    // these tests spawn processes from several threads at once: a sibling's
    // fork can inherit this write descriptor and make the shim briefly
    // unexecutable. Guard the body so a warm-up run has no side effect, then
    // wait the window out before any test relies on the shim.
    let (shebang, rest) = body.split_once('\n').expect("a shim needs a shebang");
    let guarded = format!("{shebang}\n[ -n \"${{WT_STUB_WARMUP:-}}\" ] && exit 0\n{rest}");
    wt_sys::fsx::write_nofollow(
        parent,
        &wt_core::model::RelPath::new(name).unwrap(),
        guarded.as_bytes(),
        0o755,
    )
    .unwrap();
    settle_executable(path);
}

/// Waits out the ETXTBSY window described in `write_executable`.
fn settle_executable(path: &Path) {
    let mut request = CommandRequest::new(path);
    request.env.insert("WT_STUB_WARMUP".into(), "1".into());
    for _ in 0..200 {
        match proc::capture(&request, Duration::from_secs(10)) {
            Err(error) if error.code.0 == "SPAWN_FAILED" => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => return,
        }
    }
}

pub fn write(path: &Path, body: &str) {
    wt_sys::fsx::write_store(path, body.as_bytes()).unwrap();
}

pub fn proof_capture(claim: &str, text: impl AsRef<str>) {
    let requested = std::env::var("WT_PROOF_CAPTURE").unwrap_or_default();
    if requested == "1" || requested.split(',').any(|value| value == claim) {
        println!("--- {claim} ---\n{}", text.as_ref().replace("\r\n", "\n"));
    }
}

pub fn git(path: &Path, args: &[&str]) {
    let mut request = CommandRequest::new("git");
    request.cwd = Some(path.to_path_buf());
    request.args = proc::os_args(&[
        "-c",
        "user.name=test",
        "-c",
        "user.email=t@example.test",
        "-c",
        "init.defaultBranch=main",
    ]);
    request.args.extend(proc::os_args(args));
    let output = proc::capture(&request, Duration::from_secs(10)).unwrap();
    assert!(
        output.success(),
        "git stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn branches(repo: &Path) -> Vec<String> {
    let mut request = CommandRequest::new("git");
    request.cwd = Some(repo.to_path_buf());
    request.args = proc::os_args(&["branch", "--format=%(refname:short)"]);
    let output = proc::capture(&request, Duration::from_secs(10)).unwrap();
    assert!(
        output.success(),
        "git stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}
