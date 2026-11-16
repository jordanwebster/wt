//! Holding trees, stopping them once nothing holds them, pins, owners, the
//! lifecycle lock, hook time limits, planning, and removal that never
//! waits. Run against real processes and a scratch home.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::{Fixture, git, make, stdout};
use wt::{Error, Hook, InUse, NewTree, Outcome, PlanRequest, Remove, RunHook, Stopped, Tree, Wt};

const WT: &str = env!("CARGO_BIN_EXE_wt");

fn tree(fx: &Fixture, name: &str) -> Tree {
    make(&fx.wt(), &fx.repo, PlanRequest::named(name))
        .unwrap()
        .tree
}

/// `wt <args>` with the fixture's home, not yet started.
fn cli(fx: &Fixture, cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(WT);
    command
        .args(args)
        .current_dir(cwd)
        .env("WT_HOME", &fx.home)
        .env_remove("WT_IN_HOOK")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(20));
    }
}

/// Runs `work` on a thread, so a test can check that it hasn't finished yet
/// and then that it does.
struct Background<T> {
    receiver: mpsc::Receiver<T>,
}

impl<T: Send + 'static> Background<T> {
    fn start(work: impl FnOnce() -> T + Send + 'static) -> Background<T> {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        Background { receiver }
    }

    fn still_running(&self, for_at_least: Duration) -> bool {
        matches!(
            self.receiver.recv_timeout(for_at_least),
            Err(mpsc::RecvTimeoutError::Timeout)
        )
    }

    fn finish(self, what: &str) -> T {
        self.receiver
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("{what} never finished"))
    }
}

fn stop_in_background(tree: &Tree) -> Background<wt::Result<Stopped>> {
    let tree = tree.clone();
    Background::start(move || tree.stop_when_unheld(RunHook::default()))
}

fn ran(stopped: wt::Result<Stopped>) -> wt::Ran {
    match stopped.unwrap() {
        Stopped::Ran(ran) => ran,
        other => panic!("expected the stop hook to run, got {other:?}"),
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn alive(pid_file: &Path) -> bool {
    let pid: i32 = read(pid_file).trim().parse().expect("a pid");
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn kill(pid_file: &Path) {
    if let Ok(pid) = read(pid_file).trim().parse::<i32>() {
        // SAFETY: a plain syscall on a process the test started.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// A shell snippet that blocks until `gate` exists.
fn wait_for(gate: &Path) -> String {
    format!("while [ ! -e {} ]; do sleep 0.05; done", gate.display())
}

fn stop_logging_to(log: &Path) -> String {
    format!(
        "[lifecycle]\nstop = \"echo stopped >> {}\"\n",
        log.display()
    )
}

#[test]
fn a_killed_holder_releases_the_tree_and_its_command_never_held_it() {
    let fx = Fixture::new();
    let t = tree(&fx, "t");
    let pid = fx.root.join("sleep.pid");
    let script = format!("echo $$ > {}; exec sleep 60", pid.display());
    let mut exec = cli(&fx, &fx.root, &["exec", "app/t", "--", "sh", "-c", &script])
        .spawn()
        .unwrap();
    wait_until("the command to start", || pid.exists());
    assert_eq!(t.in_use(), InUse::Held);

    exec.kill().unwrap();
    exec.wait().unwrap();
    wait_until("the hold to go", || t.in_use() == InUse::No);
    // The command outlived wt, and doesn't hold the tree.
    assert!(alive(&pid));
    kill(&pid);
}

#[test]
fn a_stopper_waits_for_a_shell_and_stops_when_it_exits() {
    let fx = Fixture::new();
    let log = fx.root.join("stop.log");
    fx.config(&stop_logging_to(&log));
    let t = tree(&fx, "t");
    let mut shell = cli(&fx, &fx.root, &["shell", "app/t"])
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("the shell to hold the tree", || t.in_use() == InUse::Held);

    let stopper = stop_in_background(&t);
    assert!(stopper.still_running(Duration::from_millis(300)));
    assert_eq!(read(&log), "");

    drop(shell.stdin.take());
    assert!(shell.wait().unwrap().success());
    assert!(ran(stopper.finish("the stopper")).succeeded());
    assert_eq!(read(&log), "stopped\n");
}

#[test]
fn a_hold_can_be_taken_while_a_stopper_waits() {
    let fx = Fixture::new();
    let log = fx.root.join("stop.log");
    fx.config(&stop_logging_to(&log));
    let t = tree(&fx, "t");
    let first = t.hold().unwrap();
    let stopper = stop_in_background(&t);
    assert!(stopper.still_running(Duration::from_millis(200)));

    // A waiting exclusive lock has no priority over new shared ones.
    let again = t.clone();
    let second = Background::start(move || again.hold()).finish("the second hold");
    let second = second.unwrap();
    drop(first);
    assert!(stopper.still_running(Duration::from_millis(200)));
    drop(second);
    assert!(ran(stopper.finish("the stopper")).succeeded());
    assert_eq!(read(&log), "stopped\n");
}

#[test]
fn a_hold_taken_during_a_stop_waits_for_it() {
    let fx = Fixture::new();
    let started = fx.root.join("started");
    let gate = fx.root.join("gate");
    let log = fx.root.join("stop.log");
    fx.config(&format!(
        "[lifecycle]\nstop = \"touch {}; {}; echo stopped >> {}\"\n",
        started.display(),
        wait_for(&gate),
        log.display()
    ));
    let t = tree(&fx, "t");
    let stopper = stop_in_background(&t);
    wait_until("the stop hook to start", || started.exists());
    assert_eq!(t.in_use(), InUse::Stopping);
    assert!(t.try_hold().unwrap().is_none());

    let holder = t.clone();
    let log_seen = log.clone();
    let hold = Background::start(move || {
        let hold = holder.hold();
        (hold, read(&log_seen))
    });
    assert!(hold.still_running(Duration::from_millis(300)));
    fs::write(&gate, "").unwrap();
    let (hold, seen) = hold.finish("the hold");
    hold.unwrap();
    assert_eq!(
        seen, "stopped\n",
        "the hold was granted before the stop ended"
    );
    assert!(ran(stopper.finish("the stopper")).succeeded());
}

#[test]
fn a_stopper_backs_off_from_a_pinned_tree() {
    let fx = Fixture::new();
    let log = fx.root.join("stop.log");
    fx.config(&stop_logging_to(&log));
    let t = tree(&fx, "t");
    stdout(&fx.cli(&fx.root, &["start", "app/t"]));
    assert!(t.pinned());
    assert!(matches!(
        t.stop_when_unheld(RunHook::default()).unwrap(),
        Stopped::Pinned
    ));
    assert_eq!(read(&log), "");

    // `wt stop` unpins and stops, whatever holds the tree.
    let _hold = t.hold().unwrap();
    stdout(&fx.cli(&fx.root, &["stop", "app/t"]));
    assert!(!t.pinned());
    assert_eq!(read(&log), "stopped\n");
}

#[test]
fn a_stopper_waiting_on_a_removed_tree_gives_up_even_when_it_is_made_again() {
    let fx = Fixture::new();
    let log = fx.root.join("stop.log");
    fx.config(&stop_logging_to(&log));
    let wt = fx.wt();
    let old = tree(&fx, "t");
    let hold = old.hold().unwrap();
    let stopper = stop_in_background(&old);
    assert!(stopper.still_running(Duration::from_millis(200)));

    let force = Remove {
        ignore_holders: true,
        ..Remove::default()
    };
    wt.remove(&old, force).unwrap();
    fs::remove_file(&log).unwrap();
    let new = tree(&fx, "t");
    assert_eq!(new.root, old.root);
    new.hold().unwrap();
    drop(hold);
    assert!(matches!(
        stopper.finish("the stopper").unwrap(),
        Stopped::Gone
    ));
    assert_eq!(read(&log), "", "the new tree was stopped");
}

#[test]
fn start_stop_and_removal_wait_for_a_create_hook_that_outlives_wt_new() {
    let fx = Fixture::new();
    let gate = fx.root.join("gate");
    let order = fx.root.join("order.log");
    let server = fx.root.join("server.pid");
    // A build server left running, holding the hook's stdout.
    fx.config(&format!(
        "[lifecycle]\ncreate = \"sh -c 'echo $$ > {server}; exec sleep 60' & {wait}; echo built >> {order}\"\nstart = \"echo start >> {order}\"\nstop = \"echo stop >> {order}\"\ndelete = \"echo delete >> {order}\"\n",
        server = server.display(),
        wait = wait_for(&gate),
        order = order.display(),
    ));
    stdout(&fx.cli(&fx.repo, &["new", "t"]));
    let t = fx.wt().find(Some("app/t"), &fx.root).unwrap();
    wait_until("the build server", || server.exists());

    let mut start = cli(&fx, &fx.root, &["start", "app/t"]).spawn().unwrap();
    let (remover, home) = (t.clone(), fx.home.clone());
    let removal = Background::start(move || {
        Wt::with_home(&home)
            .unwrap()
            .remove(&remover, Remove::default())
    });
    sleep(Duration::from_millis(400));
    assert!(
        start.try_wait().unwrap().is_none(),
        "start ran during the build"
    );
    assert!(removal.still_running(Duration::ZERO));
    assert_eq!(read(&order), "");

    fs::write(&gate, "").unwrap();
    assert!(start.wait().unwrap().success());
    let removed = removal.finish("the removal").unwrap();
    assert!(removed.warnings.is_empty(), "{:?}", removed.warnings);
    // The server still holds the hook's stdout, and kept nothing waiting.
    assert!(alive(&server));
    let lines: Vec<String> = read(&order).lines().map(str::to_owned).collect();
    assert_eq!(lines[0], "built");
    assert_eq!(lines.len(), 4, "{lines:?}");
    kill(&server);
}

#[test]
fn spawn_create_holds_the_lifecycle_lock_but_a_build_server_does_not() {
    let fx = Fixture::new();
    let gate = fx.root.join("gate");
    let log = fx.root.join("hooks.log");
    let server = fx.root.join("server.pid");
    fx.config(&format!(
        "[lifecycle]\ncreate = \"sh -c 'echo $$ > {server}; exec sleep 60' & {wait}; echo built >> {log}\"\nstart = \"echo start >> {log}\"\n",
        server = server.display(),
        wait = wait_for(&gate),
        log = log.display(),
    ));
    let t = tree(&fx, "t");
    let mut create = t.spawn_create().unwrap().expect("a create hook");
    assert_eq!(t.create_status(), wt::CreateStatus::Running);

    let starter = t.clone();
    let start = Background::start(move || starter.run_hook(Hook::Start, RunHook::default()));
    assert!(start.still_running(Duration::from_millis(300)));
    fs::write(&gate, "").unwrap();
    assert!(create.wait().unwrap().success());
    assert!(start.finish("the start hook").unwrap().succeeded());
    assert_eq!(read(&log), "built\nstart\n");
    assert_eq!(t.create_status(), wt::CreateStatus::Succeeded);

    // The server lives on without the lock.
    assert!(alive(&server));
    let started = Instant::now();
    t.run_hook(Hook::Start, RunHook::default()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    kill(&server);
}

#[test]
fn a_start_hook_that_leaves_a_process_running_does_not_hold_the_tree() {
    let fx = Fixture::new();
    let pid = fx.root.join("service.pid");
    fx.config(&format!(
        "[lifecycle]\nstart = \"sh -c 'echo $$ > {}; exec sleep 60' > /dev/null 2>&1 &\"\nstop = \"true\"\n",
        pid.display()
    ));
    let t = tree(&fx, "t");
    assert!(
        t.run_hook(Hook::Start, RunHook::default())
            .unwrap()
            .succeeded()
    );
    wait_until("the service", || pid.exists());
    assert_eq!(t.in_use(), InUse::No);
    let stopped = Background::start({
        let t = t.clone();
        move || t.stop_when_unheld(RunHook::default())
    });
    assert!(ran(stopped.finish("the stopper")).succeeded());
    assert!(alive(&pid));
    kill(&pid);
}

#[test]
fn hooks_past_their_time_limit_are_killed_with_their_group_and_waiters_go_on() {
    let fx = Fixture::new();
    let started = fx.root.join("started");
    let child = fx.root.join("child.pid");
    fx.config(&format!(
        "[lifecycle]\nstop_timeout = \"1s\"\nstart_timeout = \"500ms\"\nstop = \"touch {}; sleep 30\"\nstart = \"sleep 30 & echo $! > {}; wait\"\n",
        started.display(),
        child.display()
    ));
    let t = tree(&fx, "t");
    let began = Instant::now();
    let stopper = stop_in_background(&t);
    wait_until("the stop hook", || started.exists());
    let holder = t.clone();
    let hold = Background::start(move || holder.hold());
    assert!(hold.still_running(Duration::from_millis(200)));
    hold.finish("the hold").unwrap();
    let ran = ran(stopper.finish("the stopper"));
    assert_eq!(ran.outcome, Outcome::TimedOut(Duration::from_secs(1)));
    assert!(ran.problem().unwrap().contains("time limit of 1s"));
    assert!(began.elapsed() < Duration::from_secs(10));

    let began = Instant::now();
    let output = fx.cli(&fx.root, &["start", "app/t"]);
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("time limit of 500ms"), "{message}");
    assert!(began.elapsed() < Duration::from_secs(10));
    wait_until("the hook's child to be killed too", || !alive(&child));
}

#[test]
fn a_stopper_lets_go_while_create_builds_so_holders_need_not_wait() {
    let fx = Fixture::new();
    let gate = fx.root.join("gate");
    let log = fx.root.join("hooks.log");
    fx.config(&format!(
        "[lifecycle]\ncreate = \"{}; echo built >> {log}\"\nstop = \"echo stopped >> {log}\"\n",
        wait_for(&gate),
        log = log.display(),
    ));
    let t = tree(&fx, "t");
    let mut create = t.spawn_create().unwrap().unwrap();
    let stopper = stop_in_background(&t);
    assert!(stopper.still_running(Duration::from_millis(300)));

    let holder = t.clone();
    let hold = Background::start(move || holder.hold())
        .finish("a hold during the build")
        .unwrap();
    fs::write(&gate, "").unwrap();
    assert!(create.wait().unwrap().success());
    // The build is done, but the tree is held again: no stop yet.
    assert!(stopper.still_running(Duration::from_millis(300)));
    assert_eq!(read(&log), "built\n");
    drop(hold);
    assert!(ran(stopper.finish("the stopper")).succeeded());
    assert_eq!(read(&log), "built\nstopped\n");
}

#[test]
fn a_stop_hook_can_wt_exec_in_its_own_tree() {
    let fx = Fixture::new();
    let log = fx.root.join("hooks.log");
    fx.config(&format!(
        "[lifecycle]\nstop_timeout = \"20s\"\nstop = \"{WT} exec app/t -- sh -c 'echo in $WT_IN_HOOK >> {}'\"\n",
        log.display()
    ));
    let t = tree(&fx, "t");
    let began = Instant::now();
    let ran = ran(stop_in_background(&t).finish("the stopper"));
    assert!(ran.succeeded(), "{:?}", ran.problem());
    assert!(began.elapsed() < Duration::from_secs(10));
    assert_eq!(read(&log), "in stop\n");
}

#[test]
fn removal_of_a_held_tree_is_refused_at_once_unless_told_to_ignore_holders() {
    let fx = Fixture::new();
    let wt = fx.wt();
    let t = tree(&fx, "t");
    let hold = t.hold().unwrap();
    let began = Instant::now();
    assert!(matches!(
        wt.remove(&t, Remove::default()),
        Err(Error::Held(_))
    ));
    assert!(matches!(
        wt.check_remove(&t, Remove::default()),
        Err(Error::Held(_))
    ));
    let output = fx.cli(&fx.root, &["rm", "app/t"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("in use"));
    assert!(began.elapsed() < Duration::from_secs(5));
    assert!(t.root.exists());

    // A dirty tree is still refused for being dirty.
    fs::write(t.root.join("scratch"), "x").unwrap();
    let ignore_holders = Remove {
        ignore_holders: true,
        ..Remove::default()
    };
    assert!(matches!(
        wt.remove(&t, ignore_holders),
        Err(Error::Conflict(_))
    ));
    fs::remove_file(t.root.join("scratch")).unwrap();
    wt.remove(&t, ignore_holders).unwrap();
    assert!(!t.root.exists());
    drop(hold);

    let forced = tree(&fx, "forced");
    let _hold = forced.hold().unwrap();
    fs::write(forced.root.join("scratch"), "x").unwrap();
    stdout(&fx.cli(&fx.root, &["rm", "app/forced", "--force"]));
    assert!(!forced.root.exists());
}

#[test]
fn plan_says_whether_wt_is_configured_and_new_tree_follows_it() {
    let fx = Fixture::new();
    let wt = fx.wt();
    assert!(wt.plan(&fx.repo, "a").unwrap().is_none());

    fx.config("branch = [\"wt/{{name}}\"]\n");
    let plan = wt.plan(&fx.repo, "a").unwrap().expect("configured");
    assert_eq!(plan.label, "app");
    assert_eq!(plan.name, "a");
    assert_eq!(plan.path, fx.home.join("trees/app/a"));
    assert_eq!(plan.branch, "wt/a");
    assert!(!plan.path.exists(), "planning made something");
    let options = NewTree {
        owner: Some("amux:host-1".into()),
        ..NewTree::default()
    };
    let t = wt.new_tree(&plan, options).unwrap().tree;
    assert_eq!(t.root, plan.path);
    assert_eq!(t.branch().as_deref(), Some("wt/a"));
    assert_eq!(t.owner().as_deref(), Some("amux:host-1"));

    // Taken names, paths and branches are told apart from other failures.
    assert!(matches!(wt.plan(&fx.repo, "a"), Err(Error::Taken(_))));
    assert!(matches!(
        wt.new_tree(&plan, NewTree::default()),
        Err(Error::Taken(_))
    ));
    git(&fx.repo, &["branch", "wt/b"]);
    assert!(matches!(wt.plan(&fx.repo, "b"), Err(Error::Taken(_))));
    fs::create_dir_all(fx.home.join("trees/app/c")).unwrap();
    assert!(matches!(wt.plan(&fx.repo, "c"), Err(Error::Taken(_))));
    assert!(matches!(
        wt.plan(&fx.repo, "no/slash"),
        Err(Error::Conflict(_))
    ));

    // The user layer counts as configuring the repository.
    let other = Fixture::new();
    fs::create_dir_all(other.home.join("repos/app")).unwrap();
    fs::write(other.home.join("repos/app/wt.toml"), "").unwrap();
    assert!(other.wt().plan(&other.repo, "a").unwrap().is_some());

    // `wt ls` shows the owner, the pin and holders.
    t.pin().unwrap();
    let _hold = t.hold().unwrap();
    let listed = stdout(&fx.cli(&fx.root, &["ls"]));
    let row = listed
        .lines()
        .find(|line| line.starts_with("app/a"))
        .unwrap();
    assert!(row.contains("ready, pinned, in use"), "{listed}");
    assert!(row.contains("amux:host-1"), "{listed}");
    t.disown().unwrap();
    assert_eq!(t.owner(), None);
}

#[test]
fn fresh_branch_refuses_an_existing_branch_locally_or_on_origin() {
    let fx = Fixture::new();
    let origin = fx.with_origin();
    let wt = fx.wt();
    let fresh = NewTree {
        fresh_branch: true,
        ..NewTree::default()
    };
    git(&fx.repo, &["branch", "local"]);
    let plan = wt.plan_new(&fx.repo, &PlanRequest::named("local")).unwrap();
    assert!(matches!(
        wt.new_tree(&plan, fresh.clone()),
        Err(Error::Taken(_))
    ));

    // Pushed by someone else, and not fetched here yet.
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
    git(&other, &["push", "-q", "origin", "HEAD:refs/heads/theirs"]);
    let plan = wt
        .plan_new(&fx.repo, &PlanRequest::named("theirs"))
        .unwrap();
    let error = wt.new_tree(&plan, fresh.clone()).unwrap_err();
    assert!(matches!(error, Error::Taken(_)), "{error}");
    assert!(error.to_string().contains("on origin"), "{error}");
    assert!(!plan.path.exists());

    let plan = wt.plan_new(&fx.repo, &PlanRequest::named("new")).unwrap();
    let t = wt.new_tree(&plan, fresh).unwrap().tree;
    assert_eq!(t.branch().as_deref(), Some("new"));
}

#[test]
fn delete_empty_branch_deletes_only_a_branch_without_commits_of_its_own() {
    let fx = Fixture::new();
    fx.with_origin();
    let wt = fx.wt();
    let options = Remove {
        delete_empty_branch: true,
        ..Remove::default()
    };
    let branch_exists = |name: &str| !git(&fx.repo, &["branch", "--list", name]).is_empty();

    let empty = tree(&fx, "empty");
    let removed = wt.remove(&empty, options).unwrap();
    assert!(removed.branch_deleted, "{:?}", removed.warnings);
    assert!(!branch_exists("empty"));

    let worked = tree(&fx, "worked");
    fs::write(worked.root.join("work.txt"), "mine").unwrap();
    git(&worked.root, &["add", "."]);
    git(&worked.root, &["commit", "-qm", "work"]);
    let removed = wt.remove(&worked, options).unwrap();
    assert!(!removed.branch_deleted);
    assert!(branch_exists("worked"));

    // Commits only on the branch and its own upstream are still its own.
    let pushed = tree(&fx, "pushed");
    fs::write(pushed.root.join("pushed.txt"), "mine").unwrap();
    git(&pushed.root, &["add", "."]);
    git(&pushed.root, &["commit", "-qm", "pushed"]);
    git(&pushed.root, &["push", "-q", "-u", "origin", "pushed"]);
    let removed = wt.remove(&pushed, options).unwrap();
    assert!(!removed.branch_deleted);
    assert!(branch_exists("pushed"));

    // Without the option, an empty branch is kept as always.
    let kept = tree(&fx, "kept");
    assert!(!wt.remove(&kept, Remove::default()).unwrap().branch_deleted);
    assert!(branch_exists("kept"));
}

/// An older wt (one without holds, pins or owners) lists and removes a tree
/// that has all the new files. Needs that wt's binary in `WT_OLDER_BIN`:
/// `just older-wt` builds one and runs this.
#[test]
fn an_older_wt_lists_and_removes_a_tree_with_the_new_files() {
    let Some(older) = std::env::var_os("WT_OLDER_BIN").map(PathBuf::from) else {
        eprintln!("skipped: set WT_OLDER_BIN to an older wt, or run `just older-wt`");
        return;
    };
    let fx = Fixture::new();
    fx.config("[lifecycle]\nstart = \"true\"\n");
    let wt = fx.wt();
    let plan = wt.plan(&fx.repo, "t").unwrap().unwrap();
    let options = NewTree {
        owner: Some("amux:host".into()),
        ..NewTree::default()
    };
    let t = wt.new_tree(&plan, options).unwrap().tree;
    t.pin().unwrap();
    t.run_hook(Hook::Start, RunHook::default()).unwrap();
    drop(t.hold().unwrap());
    for file in [
        "owner",
        "pinned",
        "holds.lock",
        "lifecycle.lock",
        "start.log",
    ] {
        assert!(t.root.join(".wt").join(file).exists(), "{file}");
    }
    let older_cli = |args: &[&str]| {
        Command::new(&older)
            .args(args)
            .current_dir(&fx.root)
            .env("WT_HOME", &fx.home)
            .output()
            .unwrap()
    };
    assert!(stdout(&older_cli(&["ls"])).contains("app/t"));
    stdout(&older_cli(&["rm", "app/t"]));
    assert!(!t.root.exists());
    assert!(wt.list().unwrap().is_empty());
}

#[test]
fn removal_refuses_work_that_appeared_while_its_hooks_ran() {
    let fx = Fixture::new();
    fx.config("[lifecycle]\ndelete = \"echo late > late.txt\"\n");
    let wt = fx.wt();
    let t = tree(&fx, "t");
    let error = wt.remove(&t, Remove::default()).unwrap_err();
    assert!(matches!(error, Error::Conflict(_)), "{error}");
    assert!(t.root.join("late.txt").exists(), "the new work was deleted");
    assert_eq!(wt.list().unwrap().len(), 1);

    let ignore_dirty = Remove {
        ignore_dirty: true,
        ..Remove::default()
    };
    wt.remove(&t, ignore_dirty).unwrap();
    assert!(!t.root.exists());
}

#[test]
fn a_signal_during_removal_stops_the_teardown_and_keeps_the_tree() {
    let fx = Fixture::new();
    let started = fx.root.join("started");
    let deleted = fx.root.join("deleted");
    fx.config(&format!(
        "[lifecycle]\nstop = \"touch {}; sleep 30\"\ndelete = \"touch {}\"\n",
        started.display(),
        deleted.display()
    ));
    let t = tree(&fx, "t");
    let mut rm = cli(&fx, &fx.root, &["rm", "app/t"]).spawn().unwrap();
    wait_until("the stop hook", || started.exists());
    // SAFETY: a plain syscall on the test's own child.
    unsafe { libc::kill(rm.id() as i32, libc::SIGTERM) };
    let status = rm.wait().unwrap();
    assert!(!status.success(), "wt rm exited 0 after being interrupted");
    assert!(
        !deleted.exists(),
        "the delete hook ran after the interruption"
    );
    assert!(t.root.exists());
    assert_eq!(fx.wt().list().unwrap().len(), 1);
}

#[test]
fn a_stopper_that_waited_for_a_hook_gives_up_on_a_tree_made_again_meanwhile() {
    let fx = Fixture::new();
    let log = fx.root.join("stop.log");
    fx.config(&stop_logging_to(&log));
    let wt = fx.wt();
    let old = tree(&fx, "t");
    // Stands in for a hook that is running, on the old tree's lock file.
    fs::create_dir_all(old.root.join(".wt")).unwrap();
    let lifecycle = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(old.root.join(".wt/lifecycle.lock"))
        .unwrap();
    lifecycle.lock().unwrap();
    let stopper = stop_in_background(&old);
    assert!(stopper.still_running(Duration::from_millis(300)));
    // The stopper let go of the tree while it waits for the hook.
    drop(old.try_hold().unwrap().expect("the stopper let go"));

    let moved = fx.root.join("moved-away");
    fs::rename(&old.root, &moved).unwrap();
    wt.remove(&old, Remove::default()).unwrap();
    let new = tree(&fx, "t");
    assert_eq!(new.root, old.root);
    drop(lifecycle);
    assert!(matches!(
        stopper.finish("the stopper").unwrap(),
        Stopped::Gone
    ));
    assert_eq!(read(&log), "", "the new tree was stopped");
}

#[test]
fn a_hook_keeps_its_locks_when_whatever_ran_it_is_killed() {
    let fx = Fixture::new();
    let started = fx.root.join("started");
    let gate = fx.root.join("gate");
    let log = fx.root.join("hooks.log");
    fx.config(&format!(
        "[lifecycle]\nstop = \"touch {}; {}; echo stopped >> {log}\"\nstart = \"echo start >> {log}\"\n",
        started.display(),
        wait_for(&gate),
        log = log.display(),
    ));
    let t = tree(&fx, "t");
    let mut rm = cli(&fx, &fx.root, &["rm", "app/t"]).spawn().unwrap();
    wait_until("the stop hook", || started.exists());
    rm.kill().unwrap();
    rm.wait().unwrap();

    // The stop hook still runs, so the tree is still stopping, and no other
    // hook may start.
    assert!(t.try_hold().unwrap().is_none(), "the hold went with wt");
    let starter = t.clone();
    let start = Background::start(move || starter.run_hook(Hook::Start, RunHook::default()));
    assert!(start.still_running(Duration::from_millis(300)));
    fs::write(&gate, "").unwrap();
    assert!(start.finish("the start hook").unwrap().succeeded());
    assert_eq!(read(&log), "stopped\nstart\n");
    wait_until("the stop to end", || t.try_hold().unwrap().is_some());
    assert!(t.root.exists());
}

#[test]
fn a_service_a_start_hook_leaves_running_still_holds_through_wt_exec() {
    let fx = Fixture::new();
    let gate = fx.root.join("gate");
    let pid = fx.root.join("service.pid");
    // The service runs `wt exec` in its own tree once the hook has ended,
    // with WT_IN_HOOK inherited from the hook.
    fx.config(&format!(
        "[lifecycle]\nstart = \"sh -c '{wait}; exec {WT} exec app/t -- sh -c \\\"echo \\\\$\\\\$ > {pid}; exec sleep 60\\\"' > /dev/null 2>&1 &\"\n",
        wait = wait_for(&gate),
        pid = pid.display(),
    ));
    let t = tree(&fx, "t");
    assert!(
        t.run_hook(Hook::Start, RunHook::default())
            .unwrap()
            .succeeded()
    );
    fs::write(&gate, "").unwrap();
    wait_until("the service's command", || pid.exists());
    assert_eq!(
        t.in_use(),
        InUse::Held,
        "the service's wt exec skipped the hold"
    );
    kill(&pid);
}

#[test]
fn a_hook_past_its_time_limit_is_killed_even_when_whatever_ran_it_is_gone() {
    let fx = Fixture::new();
    let started = fx.root.join("started");
    fx.config(&format!(
        "[lifecycle]\nstop_timeout = \"1s\"\nstop = \"touch {}; sleep 60\"\nstart = \"true\"\n",
        started.display()
    ));
    let t = tree(&fx, "t");
    let mut rm = cli(&fx, &fx.root, &["rm", "app/t"]).spawn().unwrap();
    wait_until("the stop hook", || started.exists());
    rm.kill().unwrap();
    rm.wait().unwrap();

    let starter = t.clone();
    let start = Background::start(move || starter.run_hook(Hook::Start, RunHook::default()));
    assert!(
        start
            .finish("a start after the hung stop")
            .unwrap()
            .succeeded()
    );
    assert!(
        t.try_hold().unwrap().is_some(),
        "the hold outlived the limit"
    );
}

#[test]
fn removal_waits_out_a_running_stop_hook_but_not_a_real_holder() {
    let fx = Fixture::new();
    let started = fx.root.join("started");
    let gate = fx.root.join("gate");
    fx.config(&format!(
        "[lifecycle]\nstop = \"touch {s}; {w}\"\nstart = \"touch {s}; {w}\"\n",
        s = started.display(),
        w = wait_for(&gate),
    ));

    // The stop hook has the tree to itself, but nobody else holds it.
    let t = tree(&fx, "t");
    let stopper = stop_in_background(&t);
    wait_until("the stop hook", || started.exists());
    let (remover, home) = (t.clone(), fx.home.clone());
    let removal = Background::start(move || {
        Wt::with_home(&home)
            .unwrap()
            .remove(&remover, Remove::default())
    });
    assert!(
        removal.still_running(Duration::from_millis(300)),
        "removal refused a tree only its stop hook had"
    );
    fs::write(&gate, "").unwrap();
    assert!(ran(stopper.finish("the stopper")).succeeded());
    removal.finish("the removal").unwrap();
    assert!(!t.root.exists());

    // A real holder is still refused, once the running hook is done.
    fs::remove_file(&gate).unwrap();
    fs::remove_file(&started).unwrap();
    let held = tree(&fx, "held");
    let _hold = held.hold().unwrap();
    let starter = held.clone();
    let start = Background::start(move || starter.run_hook(Hook::Start, RunHook::default()));
    wait_until("the start hook", || started.exists());
    let (remover, home) = (held.clone(), fx.home.clone());
    let removal = Background::start(move || {
        Wt::with_home(&home)
            .unwrap()
            .remove(&remover, Remove::default())
    });
    assert!(removal.still_running(Duration::from_millis(300)));
    fs::write(&gate, "").unwrap();
    start.finish("the start hook").unwrap();
    assert!(matches!(removal.finish("the removal"), Err(Error::Held(_))));
    assert!(held.root.exists());
}

#[test]
fn removal_skips_the_stop_hook_of_a_tree_already_stopped() {
    let fx = Fixture::new();
    let log = fx.root.join("hooks.log");
    let fail = fx.root.join("fail");
    fx.config(&format!(
        "[lifecycle]\nstart = \"echo start $WT_NAME >> {log}\"\nstop = \"test ! -e {fail} && echo stop $WT_NAME >> {log}\"\ndelete = \"echo delete $WT_NAME >> {log}\"\n",
        log = log.display(),
        fail = fail.display(),
    ));
    let wt = fx.wt();
    let marker = |t: &Tree| t.root.join(".wt/stopped").exists();

    let t = tree(&fx, "a");
    assert!(ran(stop_in_background(&t).finish("the stopper")).succeeded());
    assert!(marker(&t));
    wt.remove(&t, Remove::default()).unwrap();
    assert_eq!(read(&log), "stop a\ndelete a\n", "the stop hook ran twice");

    // A hold may start things by hand, so it clears the marker.
    fs::remove_file(&log).unwrap();
    let held = tree(&fx, "b");
    t_stop(&held);
    drop(held.hold().unwrap());
    assert!(!marker(&held));
    wt.remove(&held, Remove::default()).unwrap();
    assert_eq!(read(&log), "stop b\nstop b\ndelete b\n");

    // So does a start, and a stop that fails leaves none.
    fs::remove_file(&log).unwrap();
    let started = tree(&fx, "c");
    t_stop(&started);
    assert!(
        started
            .run_hook(Hook::Start, RunHook::default())
            .unwrap()
            .succeeded()
    );
    assert!(!marker(&started));
    t_stop(&started);
    fs::write(&fail, "").unwrap();
    assert!(
        !started
            .run_hook(Hook::Stop, RunHook::default())
            .unwrap()
            .succeeded()
    );
    assert!(!marker(&started));
    assert!(!started.stopped());
}

fn t_stop(t: &Tree) {
    assert!(
        t.run_hook(Hook::Stop, RunHook::default())
            .unwrap()
            .succeeded()
    );
    assert!(t.stopped());
}

#[test]
fn a_hard_stop_under_a_holder_does_not_mark_the_tree_stopped() {
    let fx = Fixture::new();
    fx.config("[lifecycle]\nstop = \"true\"\n");
    let t = tree(&fx, "t");
    let hold = t.hold().unwrap();
    stdout(&fx.cli(&fx.root, &["stop", "app/t"]));
    assert!(
        !t.stopped(),
        "marked stopped while a holder could restart things"
    );
    drop(hold);
    stdout(&fx.cli(&fx.root, &["stop", "app/t"]));
    assert!(t.stopped());
}

#[test]
fn a_create_hook_clears_the_stopped_mark() {
    let fx = Fixture::new();
    fx.config("[lifecycle]\nstop = \"true\"\ncreate = \"true\"\n");
    let t = tree(&fx, "t");
    t_stop(&t);
    assert!(
        t.run_hook(Hook::Create, RunHook::default())
            .unwrap()
            .succeeded()
    );
    assert!(!t.stopped(), "a foreground create left the mark");
    t_stop(&t);
    t.spawn_create().unwrap().unwrap().wait().unwrap();
    assert!(!t.stopped(), "a detached create left the mark");
}
