//! Pools and leases: `wt with`, `wt leases`, and the library calls behind
//! them, run against real processes and a scratch home.

mod common;

use std::fs;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::{Fixture, stdout};
use wt::{Error, Init, Tree};

/// A fixture whose repository has `config`, with its main checkout as the
/// tree `app/main`.
fn setup(config: &str) -> (Fixture, Tree) {
    let fx = Fixture::new();
    fx.config(config);
    let tree = fx.wt().init(&fx.repo, Init::default()).unwrap();
    (fx, tree)
}

fn settings(fx: &Fixture, text: &str) {
    fs::create_dir_all(&fx.home).unwrap();
    fs::write(fx.home.join("config.toml"), text).unwrap();
}

/// `wt with <args>`, not yet started.
fn with(fx: &Fixture, cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wt"));
    command
        .arg("with")
        .args(args)
        .current_dir(cwd)
        .env("WT_HOME", &fx.home)
        .stdout(Stdio::piped())
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

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn finish(child: Child) -> Output {
    child.wait_with_output().unwrap()
}

#[test]
fn a_second_with_waits_for_the_only_slot() {
    let (fx, _) = setup("[pool.p]\nsize = 1\nwait = \"10s\"\n");
    let log = fx.root.join("log");
    let first = with(
        &fx,
        &fx.repo,
        &[
            "p",
            "--",
            "sh",
            "-c",
            r#"echo "first $WT_LEASE_P" >> "$1"; sleep 0.4; echo first-done >> "$1""#,
            "sh",
            log.to_str().unwrap(),
        ],
    )
    .spawn()
    .unwrap();
    wait_until("the first lease", || read(&log).contains("first"));
    let second = with(
        &fx,
        &fx.repo,
        &[
            "p",
            "--",
            "sh",
            "-c",
            r#"echo "second $WT_LEASE_P" >> "$1""#,
            "sh",
            log.to_str().unwrap(),
        ],
    )
    .spawn()
    .unwrap();
    stdout(&finish(first));
    stdout(&finish(second));
    assert_eq!(read(&log), "first p-1\nfirst-done\nsecond p-1\n");
}

#[test]
fn a_full_pool_times_out_saying_who_holds_it() {
    let (fx, tree) = setup("[pool.p]\nsize = 1\nwait = \"300ms\"\n");
    let held = fx.wt().lease(Some(&tree), &["p"]).unwrap();
    assert_eq!(held.slots().collect::<Vec<_>>(), [("p", "p-1")]);

    let started = Instant::now();
    let output = with(&fx, &fx.repo, &["p", "--", "true"]).output().unwrap();
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(output.status.code(), Some(1));
    let message = stderr(&output);
    assert!(
        message.contains("no free slot in pool `p` after waiting 300ms"),
        "{message}"
    );
    let holder = format!("p-1 leased by app/main (pid {}", std::process::id());
    assert!(message.contains(&holder), "{message}");

    let table = stdout(&fx.cli(&fx.root, &["leases"]));
    let lines: Vec<&str> = table.lines().collect();
    assert!(lines[0].starts_with("POOL  SLOT  STATE"), "{table}");
    assert!(
        lines[1].starts_with("p     p-1   leased by app/main (pid"),
        "{table}"
    );

    drop(held);
    let table = stdout(&fx.cli(&fx.root, &["leases"]));
    assert!(table.contains("free"), "{table}");
    assert!(table.contains("app/main, 0s ago"), "{table}");
}

#[test]
fn a_tree_gets_back_the_slot_it_used_last() {
    let (fx, one) = setup("[pool.p]\nsize = 3\nwait = \"1s\"\n");
    let wt = fx.wt();
    let two = wt.init(&fx.worktree("two"), Init::default()).unwrap();
    let slot = |lease: &wt::Lease| lease.slots().next().unwrap().1.to_owned();

    let first = wt.lease(Some(&one), &["p"]).unwrap();
    let second = wt.lease(Some(&two), &["p"]).unwrap();
    assert_eq!((slot(&first), slot(&second)), ("p-1".into(), "p-2".into()));
    drop(first);
    drop(second);

    // Taken in the other order, each tree still gets the slot it had.
    let second = wt.lease(Some(&two), &["p"]).unwrap();
    let first = wt.lease(Some(&one), &["p"]).unwrap();
    assert_eq!((slot(&first), slot(&second)), ("p-1".into(), "p-2".into()));
}

#[test]
fn nested_with_reuses_the_slot_the_caller_holds() {
    let (fx, _) = setup("[pool.p]\nsize = 1\nwait = \"200ms\"\n");
    let output = with(
        &fx,
        &fx.repo,
        &[
            "p",
            "--",
            "sh",
            "-c",
            r#""$WT" with p -- sh -c 'echo inner $WT_LEASE_P' && echo outer $WT_LEASE_P"#,
        ],
    )
    .env("WT", env!("CARGO_BIN_EXE_wt"))
    .output()
    .unwrap();
    assert_eq!(stdout(&output), "inner p-1\nouter p-1\n");
}

#[test]
fn a_holder_killed_outright_frees_its_slot() {
    let (fx, tree) = setup("[pool.p]\nsize = 1\nwait = \"300ms\"\n");
    let wt = fx.wt();
    let pid_file = fx.root.join("pid");
    let mut holder = with(
        &fx,
        &fx.repo,
        &[
            "p",
            "--",
            "sh",
            "-c",
            r#"echo $$ > "$1"; exec sleep 30"#,
            "sh",
            pid_file.to_str().unwrap(),
        ],
    )
    .spawn()
    .unwrap();
    wait_until("the holder to start", || read(&pid_file).ends_with('\n'));
    let slots = wt.leases().unwrap();
    assert!(slots[0].leased);
    assert_eq!(slots[0].holder.as_ref().unwrap().pid, holder.id());

    holder.kill().unwrap();
    holder.wait().unwrap();
    let grandchild = read(&pid_file).trim().to_owned();
    Command::new("kill").arg(&grandchild).status().unwrap();

    // The dead holder's record is still on disk, but the free lock wins.
    assert!(read(&fx.home.join("pools/p/1.json")).contains("\"holder\""));
    let slots = wt.leases().unwrap();
    assert!(!slots[0].leased);
    assert_eq!(slots[0].holder, None);
    let lease = wt.lease(Some(&tree), &["p"]).unwrap();
    assert_eq!(lease.vars()["WT_LEASE_P"], "p-1");
}

#[test]
fn idle_slots_are_retired_once_and_never_while_leased() {
    let fx = Fixture::new();
    let log = fx.root.join("log");
    fx.config(&format!(
        r#"
[pool.p]
size = 2
wait = "1s"
acquire = 'echo "acquire $WT_LEASE" >> {log}'
idle = "1s"
retire = 'echo "retire $WT_LEASE $(pwd -P)" >> {log}'
"#,
        log = log.display()
    ));
    let wt = fx.wt();
    let one = wt.init(&fx.repo, Init::default()).unwrap();
    let two = wt.init(&fx.worktree("two"), Init::default()).unwrap();
    let three = wt.init(&fx.worktree("three"), Init::default()).unwrap();

    drop((
        wt.lease(Some(&one), &["p"]).unwrap(),
        wt.lease(Some(&two), &["p"]).unwrap(),
    ));
    let held = wt.lease(Some(&two), &["p"]).unwrap();
    assert_eq!(held.vars()["WT_LEASE_P"], "p-2");
    assert!(wt.reap_idle().unwrap().is_empty(), "nothing is idle yet");

    sleep(Duration::from_millis(2100));
    let retired = wt.reap_idle().unwrap();
    assert_eq!(retired.len(), 1, "{retired:?}");
    assert_eq!(
        (retired[0].pool.as_str(), retired[0].slot.as_str()),
        ("p", "p-1")
    );
    retired[0].result.as_ref().unwrap();
    assert!(wt.reap_idle().unwrap().is_empty(), "p-1 is cold now");
    drop(held);

    // A tree with no slot of its own prefers the warm one.
    let lease = wt.lease(Some(&three), &["p"]).unwrap();
    assert_eq!(lease.vars()["WT_LEASE_P"], "p-2");
    assert!(wt.reap_idle().unwrap().is_empty());
    drop(lease);

    assert_eq!(
        read(&log),
        format!(
            "acquire p-1\nacquire p-2\nacquire p-2\nretire p-1 {}\nacquire p-2\n",
            fx.repo.display()
        )
    );
    let slots = wt.leases().unwrap();
    assert_eq!(slots[0].retire_due, None);
    assert!(slots[1].retire_due.is_some());
}

#[test]
fn a_failed_acquire_releases_every_slot_taken() {
    let (fx, tree) = setup(
        "[pool.a]\nsize = 1\nwait = \"200ms\"\n\n[pool.b]\nsize = 1\nwait = \"200ms\"\nacquire = \"exit 3\"\n",
    );
    let wt = fx.wt();
    let error = wt.lease(Some(&tree), &["b", "a"]).unwrap_err();
    assert!(matches!(error, Error::Command(_)), "{error:?}");
    let message = error.to_string();
    assert!(message.contains("`acquire` failed for b-1"), "{message}");
    assert!(message.contains('3'), "{message}");

    let slots = wt.leases().unwrap();
    assert_eq!(slots.len(), 2);
    assert!(slots.iter().all(|slot| !slot.leased), "{slots:?}");
    wt.lease(Some(&tree), &["a"]).unwrap();
}

#[test]
fn with_passes_the_exit_status_through() {
    let fx = Fixture::new();
    settings(&fx, "[pool.tok]\nsize = 1\n");
    // Outside any tree, only the machine's pools are known.
    let code = |script: &str| {
        let output = with(&fx, &fx.root, &["tok", "--", "sh", "-c", script])
            .output()
            .unwrap();
        output.status.code()
    };
    assert_eq!(code("exit 7"), Some(7));
    assert_eq!(code("kill -TERM $$"), Some(128 + 15));
    assert_eq!(code("exit 0"), Some(0));

    let output = with(&fx, &fx.root, &["nope", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no pool `nope`"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn several_pools_are_leased_together() {
    let (fx, _) = setup(
        "[pool.a]\nsize = 1\n\n[pool.b]\nsize = 2\nname = \"bee-{{n}}\"\n\n[pool.c-d]\nsize = 1\n",
    );
    let output = with(
        &fx,
        &fx.repo,
        &[
            "b",
            "c-d",
            "a",
            "--",
            "sh",
            "-c",
            "echo $WT_LEASE_A $WT_LEASE_B $WT_LEASE_C_D $WT_TREE; pwd -P",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(
        stdout(&output),
        format!("a-1 bee-1 c-d-1 app/main\n{}\n", fx.repo.display())
    );

    let output = with(&fx, &fx.repo, &["a", "a", "--", "true"])
        .output()
        .unwrap();
    assert!(
        stderr(&output).contains("pool `a` is named twice"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn sigterm_reaches_the_child_and_sigint_is_left_to_it() {
    let fx = Fixture::new();
    settings(&fx, "[pool.tok]\nsize = 1\n");
    let ready = fx.root.join("ready");
    let out = fx.root.join("out");
    let child = with(
        &fx,
        &fx.root,
        &[
            "tok",
            "--",
            "sh",
            "-c",
            r#"trap 'echo term > "$2"; exit 0' TERM; echo > "$1"; while :; do sleep 0.05; done"#,
            "sh",
            ready.to_str().unwrap(),
            out.to_str().unwrap(),
        ],
    )
    .spawn()
    .unwrap();
    wait_until("the child to start", || ready.exists());
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let output = finish(child);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(read(&out), "term\n");

    // An interrupt sent to wt alone doesn't stop it: the child carries on
    // and its status comes back.
    fs::remove_file(&ready).unwrap();
    let child = with(
        &fx,
        &fx.root,
        &[
            "tok",
            "--",
            "sh",
            "-c",
            r#"echo > "$1"; sleep 0.3; exit 5"#,
            "sh",
            ready.to_str().unwrap(),
        ],
    )
    .spawn()
    .unwrap();
    wait_until("the child to start", || ready.exists());
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert_eq!(finish(child).status.code(), Some(5));
}

#[test]
fn a_pool_is_the_trees_config_with_machine_sizing() {
    let (fx, tree) = setup(
        r#"
[pool.p]
size = 1
name = "x-{{n}}"
acquire = "true"

[pool.unsized]
wait = "1s"

[pool.bad]
size = 1
name = "{{nope}}"
"#,
    );
    settings(
        &fx,
        "[pool.p]\nsize = 5\n\n[pool.tok]\nsize = 2\nidle = \"1m\"\n",
    );
    let wt = fx.wt();

    let pool = wt.pool(Some(&tree), "p").unwrap();
    assert_eq!(pool.size, 5);
    assert_eq!(pool.acquire.as_deref(), Some("true"));
    assert_eq!(pool.wait, Duration::from_secs(30 * 60));
    assert_eq!(pool.slot(2).unwrap(), "x-2");

    // Outside a tree, the machine's sizing is a pool of plain tokens.
    let pool = wt.pool(None, "p").unwrap();
    assert_eq!((pool.size, pool.acquire.as_deref()), (5, None));
    assert_eq!(pool.slot(1).unwrap(), "p-1");
    assert_eq!(
        wt.pool(None, "tok").unwrap().idle,
        Some(Duration::from_secs(60))
    );

    assert!(matches!(wt.pool(None, "missing"), Err(Error::NotFound(_))));
    assert!(matches!(
        wt.pool(Some(&tree), "missing"),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        wt.pool(Some(&tree), "unsized"),
        Err(Error::Config(_))
    ));
    let error = wt.pool(Some(&tree), "bad").unwrap_err().to_string();
    assert!(error.contains("nope"), "{error}");
}

#[test]
fn sigterm_during_acquire_lets_it_finish_before_the_slot_is_free() {
    let (fx, _tree) = setup("");
    let log = fx.root.join("acquire.log");
    fx.config(&format!(
        "[pool.sim]\nsize = 1\nacquire = \"echo start >> {0}; sleep 1; echo end >> {0}\"\n",
        log.display()
    ));
    let first = with(&fx, &fx.repo, &["sim", "--", "true"]).spawn().unwrap();
    wait_until("acquire to start", || read(&log) == "start\n");
    // SAFETY: signalling our own child.
    unsafe { libc::kill(first.id() as i32, libc::SIGTERM) };
    let second = with(&fx, &fx.repo, &["sim", "--", "true"]).spawn().unwrap();
    let first = finish(first);
    assert_eq!(first.status.code(), Some(143), "{}", stderr(&first));
    let second = finish(second);
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(read(&log), "start\nend\nstart\nend\n");
}

#[test]
fn sigterm_while_waiting_gives_up_at_once() {
    let (fx, _tree) = setup("[pool.sim]\nsize = 1\nwait = \"1m\"\n");
    let holder = with(&fx, &fx.repo, &["sim", "--", "sleep", "5"])
        .spawn()
        .unwrap();
    wait_until("the slot to be held", || {
        fx.wt().leases().unwrap().iter().any(|slot| slot.leased)
    });
    let waiter = with(&fx, &fx.repo, &["sim", "--", "true"]).spawn().unwrap();
    sleep(Duration::from_millis(300));
    let started = Instant::now();
    // SAFETY: signalling our own child.
    unsafe { libc::kill(waiter.id() as i32, libc::SIGTERM) };
    let waited = finish(waiter);
    assert_eq!(waited.status.code(), Some(143));
    assert!(started.elapsed() < Duration::from_secs(2));
    // SAFETY: signalling our own child.
    unsafe { libc::kill(holder.id() as i32, libc::SIGTERM) };
    finish(holder);
}
