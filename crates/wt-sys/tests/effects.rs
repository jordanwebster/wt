#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use tempfile::tempdir;
use wt_core::resource::ProbeResult;
use wt_sys::lock;
use wt_sys::proc::{self, CommandRequest};
use wt_sys::tmux::Tmux;

fn stubs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stubs")
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn committed_tmux_stub_records_argv_and_obeys_state() {
    let dir = tempdir().unwrap();
    let tmux_program = stubs().join("tmux");
    make_executable(&tmux_program);
    let log = dir.path().join("argv.log");
    let state = dir.path().join("state");
    fs::write(&state, "version=3.3\nsession=existing\n").unwrap();
    let tmux = Tmux::new(tmux_program, Duration::from_secs(1)).with_env(BTreeMap::from([
        ("WT_STUB_LOG".into(), log.to_string_lossy().into_owned()),
        ("WT_STUB_STATE".into(), state.to_string_lossy().into_owned()),
    ]));
    assert_eq!(tmux.check_version().unwrap(), (3, 3));
    assert!(tmux.has_session("existing").unwrap());
    assert!(!tmux.has_session("missing").unwrap());
    tmux.new_session(
        "new",
        dir.path(),
        dir.path(),
        &dir.path().join("capture"),
        &[OsString::from("wt"), OsString::from("exec")],
    )
    .unwrap();
    tmux.switch_client("new").unwrap();
    tmux.kill_session("new").unwrap();
    let record = fs::read_to_string(log).unwrap();
    assert!(record.contains("tmux\tnew-session\t-d\t-s\tnew\t-c"));
    assert!(record.contains("\t-c\t"));
    assert!(record.contains("\t--\t/bin/sh\t-c\t"));
    assert!(record.contains("\twt\texec\t;\tpipe-pane"));
    assert!(record.contains("\t-e\tWT_HOME="));
    assert!(record.contains("tmux\tswitch-client\t-t\tnew"));
    assert!(record.contains("tmux\tkill-session\t-t\tnew"));
}

#[test]
fn spawn_tracer_records_argv_cwd_and_exit_state() {
    let dir = tempdir().unwrap();
    let recorder = stubs().join("recorder");
    make_executable(&recorder);
    let log = dir.path().join("spawn.log");
    let state = dir.path().join("state");
    fs::write(&state, "exit=7\n").unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let request = CommandRequest {
        program: OsString::from("recorder"),
        args: proc::os_args(&["one", "two words"]),
        cwd: Some(dir.path().to_path_buf()),
        env: BTreeMap::from([
            ("WT_STUB_LOG".into(), log.to_string_lossy().into_owned()),
            ("WT_STUB_STATE".into(), state.to_string_lossy().into_owned()),
            (
                "PATH".into(),
                format!("{}:{inherited_path}", recorder.parent().unwrap().display()),
            ),
        ]),
        remove_env: Vec::new(),
        clear_env: false,
    };
    let output = proc::capture(&request, Duration::from_secs(1)).unwrap();
    assert_eq!(output.mapped_exit(), 7);
    let record = fs::read_to_string(log).unwrap();
    assert!(record.contains(&format!(
        "cwd={}",
        dir.path().canonicalize().unwrap().display()
    )));
    assert!(record.ends_with("\tone\ttwo words\n"));
}

#[test]
fn docker_like_path_stub_obeys_exit_two_probe_state() {
    let dir = tempdir().unwrap();
    let docker = stubs().join("docker");
    make_executable(&docker);
    let log = dir.path().join("docker.log");
    let state = dir.path().join("state");
    fs::write(&state, "exit=2\n").unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let request = CommandRequest {
        program: OsString::from("docker"),
        args: proc::os_args(&["info"]),
        cwd: Some(dir.path().to_path_buf()),
        env: BTreeMap::from([
            ("WT_STUB_LOG".into(), log.to_string_lossy().into_owned()),
            ("WT_STUB_STATE".into(), state.to_string_lossy().into_owned()),
            (
                "PATH".into(),
                format!("{}:{inherited_path}", docker.parent().unwrap().display()),
            ),
        ]),
        remove_env: Vec::new(),
        clear_env: false,
    };
    assert!(matches!(
        proc::probe(&request, Duration::from_secs(1), "now").result,
        ProbeResult::Failed { .. }
    ));
    assert_eq!(fs::read_to_string(log).unwrap(), "docker\tinfo\n");
}

#[test]
fn execvp_fd_helper() {
    if std::env::var_os("WT_FD_HELPER").is_none() {
        return;
    }
    let tree_path = PathBuf::from(std::env::var_os("WT_FD_TREE").unwrap());
    let door_path = PathBuf::from(std::env::var_os("WT_FD_DOOR").unwrap());
    let rmw_path = PathBuf::from(std::env::var_os("WT_FD_RMW").unwrap());
    let metadata_path = PathBuf::from(std::env::var_os("WT_FD_META").unwrap());
    let output_path = PathBuf::from(std::env::var_os("WT_FD_OUTPUT").unwrap());
    let ready_path = PathBuf::from(std::env::var_os("WT_FD_READY").unwrap());
    let holder = lock::Holder::current("repo/tree", "exec", "now");
    let tree = lock::tree(&tree_path, lock::Mode::Shared, &holder, Duration::ZERO).unwrap();
    let door = lock::door(&door_path, &holder).unwrap();
    let rmw = lock::registry_rmw(&rmw_path, &holder, Duration::ZERO).unwrap();
    fs::write(
        &metadata_path,
        format!("{} {} {}", tree.raw_fd(), door.raw_fd(), rmw.raw_fd()),
    )
    .unwrap();

    let mut request = CommandRequest::new("sh");
    request.args = vec![
        "-c".into(),
        "output=$1; ready=$2; : > \"$output\"; fd=3; while [ \"$fd\" -lt 64 ]; do if [ -e \"/dev/fd/$fd\" ]; then printf '%s\\n' \"$fd\" >> \"$output\"; fi; fd=$((fd + 1)); done; : > \"$ready\"; sleep 1".into(),
        "wt-fd-probe".into(),
        output_path.into_os_string(),
        ready_path.into_os_string(),
    ];
    proc::execvp_inheriting(&request, &[tree.raw_fd(), door.raw_fd()]).unwrap();
}

#[test]
fn execvp_inherits_only_selected_lock_fds_and_preserves_flock() {
    let dir = tempdir().unwrap();
    let tree = dir.path().join("tree.lock");
    let door = dir.path().join("tree.doors/child.lock");
    let rmw = dir.path().join("registry.lock");
    let metadata = dir.path().join("fds.meta");
    let output = dir.path().join("fds.out");
    let ready = dir.path().join("ready");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "execvp_fd_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("WT_FD_HELPER", "1")
        .env("WT_FD_TREE", &tree)
        .env("WT_FD_DOOR", &door)
        .env("WT_FD_RMW", &rmw)
        .env("WT_FD_META", &metadata)
        .env("WT_FD_OUTPUT", &output)
        .env("WT_FD_READY", &ready)
        .spawn()
        .unwrap();

    let until = Instant::now() + Duration::from_secs(2);
    while !ready.exists() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "exec fd helper did not become ready");
    let expected = fs::read_to_string(metadata)
        .unwrap()
        .split_whitespace()
        .map(|value| value.parse::<i32>().unwrap())
        .collect::<Vec<_>>();
    let actual = fs::read_to_string(output)
        .unwrap()
        .lines()
        .map(|value| value.parse::<i32>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected[..2]);
    assert!(lock::is_held(&tree).unwrap());
    assert!(child.wait().unwrap().success());
    assert!(!lock::is_held(&tree).unwrap());
}
