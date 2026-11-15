use std::ffi::OsString;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use wt::{
    CreateStatus, Hook, HookOutput, InUse, Init, NewTree, PlanRequest, Remove, RunHook, Tree, Wt,
};

#[derive(Parser)]
#[command(name = "wt", version, about = "Parameterised git worktrees")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Make a new tree: a worktree started from origin's default branch
    New {
        /// `name`, or `label/name` from outside the repository
        tree: String,
        /// Start from this instead of origin's default branch
        #[arg(long)]
        from: Option<String>,
        /// The branch name, instead of the config's `branch` templates
        #[arg(long)]
        branch: Option<String>,
        /// A value for the `branch` templates, e.g. --set ticket=ENG-42
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_set)]
        set: Vec<(String, String)>,
        /// Start from the last fetched copy of the default branch
        #[arg(long)]
        no_fetch: bool,
        /// Don't run the create hook
        #[arg(long)]
        no_create: bool,
        /// Copy inputs and run the create hook in the foreground, rather
        /// than in the background
        #[arg(long, conflicts_with = "no_create")]
        wait: bool,
    },
    /// Run a tree's stop and delete hooks, then remove it (keeping its branch)
    #[command(alias = "remove")]
    Rm {
        tree: String,
        /// Remove despite uncommitted changes, and while something holds it
        #[arg(long)]
        force: bool,
        /// Leave the worktree on disk; only stop managing it
        #[arg(long)]
        keep: bool,
    },
    /// Run a tree's start hook, and keep it running until `wt stop`
    Start {
        tree: Option<String>,
        /// Every tree
        #[arg(long, conflicts_with = "tree")]
        all: bool,
    },
    /// Run a tree's stop hook now, whatever holds it
    Stop {
        tree: Option<String>,
        /// Every tree
        #[arg(long, conflicts_with = "tree")]
        all: bool,
    },
    /// Make an existing worktree, or the main checkout, a tree
    Init {
        /// A path inside the worktree [default: the current directory]
        path: Option<PathBuf>,
        /// The repository's label, the first time it is seen
        #[arg(long)]
        label: Option<String>,
        /// The tree's name
        #[arg(long)]
        name: Option<String>,
        /// Don't run the create hook
        #[arg(long)]
        no_create: bool,
        /// Copy inputs and run the create hook in the foreground, rather
        /// than in the background
        #[arg(long, conflicts_with = "no_create")]
        wait: bool,
    },
    /// List trees
    #[command(alias = "list")]
    Ls,
    /// Print a tree's path
    Path { tree: Option<String> },
    /// Print a tree's environment as shell exports
    Env { tree: Option<String> },
    /// Run a command in a tree's environment
    Exec {
        tree: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Start a shell in a tree's environment
    Shell { tree: Option<String> },
    /// Import trees from wt 0.5, keeping their names and ports
    Migrate,
    /// Copy a tree's inputs, as the first step of its background setup
    #[command(name = "__copy-inputs", hide = true)]
    CopyInputs {
        path: PathBuf,
        /// Record a failure in create.status and create.log, as the create
        /// hook's wrapper does when it runs the copy
        #[arg(long)]
        record: bool,
    },
    /// Lease a slot in each pool and run a command while holding them
    With {
        #[arg(required = true)]
        pools: Vec<String>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Show every pool slot: who holds it, who used it last, when it retires
    Leases,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("wt: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Cmd) -> wt::Result<ExitCode> {
    let wt = Wt::open()?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if wt.needs_import() && !matches!(command, Cmd::Migrate) {
        return Err(wt::Error::State(format!(
            "{} has trees from wt 0.5; run `wt migrate` to import them first",
            wt.home().display()
        )));
    }
    match command {
        Cmd::CopyInputs { path, record } => {
            let tree = wt.find(None, &path)?;
            if let Err(error) = tree.copy_inputs() {
                let message = format!("wt: copying inputs failed: {error}\n");
                eprint!("{message}");
                if record {
                    record_failed_copy(&tree, &message);
                }
                return Ok(ExitCode::FAILURE);
            }
        }
        Cmd::Migrate => {
            let imported = wt.import_legacy()?;
            for warning in &imported.warnings {
                eprintln!("wt: {warning}");
            }
            for tree in &imported.trees {
                println!("imported {tree}");
            }
            eprintln!(
                "wt: convert each repository's .wt.toml to the new format; the next \
                 `wt init`, `env`, `exec` or `shell` in a tree renders it"
            );
        }
        Cmd::New {
            tree,
            from,
            branch,
            set,
            no_fetch,
            no_create,
            wait,
        } => {
            let (label, name) = match tree.split_once('/') {
                Some((label, name)) => (Some(label.to_owned()), name.to_owned()),
                None => (None, tree),
            };
            let plan = wt.plan_new(
                &cwd,
                &PlanRequest {
                    label,
                    branch,
                    vars: set.into_iter().collect(),
                    ..PlanRequest::named(name)
                },
            )?;
            let request = NewTree {
                from,
                fetch: !no_fetch,
                ..NewTree::default()
            };
            let created = wt.new_tree(&plan, request)?;
            for warning in &created.warnings {
                eprintln!("wt: {warning}");
            }
            let tree = created.tree;
            eprintln!("wt: made {} at {}", tree.id(), tree.root.display());
            return set_up(&tree, no_create, wait);
        }
        Cmd::Rm { tree, force, keep } => {
            let tree = wt.find(Some(&tree), &cwd)?;
            catch_for_hooks();
            let options = Remove {
                ignore_dirty: force,
                ignore_holders: force,
                keep,
                hooks: foreground(),
                ..Remove::default()
            };
            let removed = match wt.remove(&tree, options) {
                Err(wt::Error::Cancelled) => {
                    eprintln!("wt: gave up removing {}; it is still there", tree.id());
                    return Ok(interrupted());
                }
                removed => removed?,
            };
            for warning in &removed.warnings {
                eprintln!("wt: {warning}");
            }
            let kept = match (&removed.branch, removed.branch_deleted) {
                (Some(branch), false) => format!("; kept branch {branch}"),
                _ => String::new(),
            };
            eprintln!("wt: removed {}{kept}", tree.id());
        }
        Cmd::Start { tree, all } => return run_everywhere(&wt, &cwd, tree, all, Hook::Start),
        Cmd::Stop { tree, all } => return run_everywhere(&wt, &cwd, tree, all, Hook::Stop),
        Cmd::Init {
            path,
            label,
            name,
            no_create,
            wait,
        } => {
            let tree = wt.init(path.as_deref().unwrap_or(&cwd), Init { label, name })?;
            eprintln!("wt: {} is at {}", tree.id(), tree.root.display());
            return set_up(&tree, no_create, wait);
        }
        Cmd::Ls => list(&wt)?,
        Cmd::Path { tree } => println!("{}", wt.find(tree.as_deref(), &cwd)?.root.display()),
        Cmd::Env { tree } => print!("{}", wt.find(tree.as_deref(), &cwd)?.env()?.to_shell()),
        Cmd::Exec { tree, command } => {
            let tree = wt.find(tree.as_deref(), &cwd)?;
            let mut child = Command::new(&command[0]);
            child.args(&command[1..]);
            return enter(&tree, &cwd, child);
        }
        Cmd::Shell { tree } => {
            let tree = wt.find(tree.as_deref(), &cwd)?;
            let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
            return enter(&tree, &cwd, Command::new(shell));
        }
        Cmd::With { pools, command } => return with(&wt, &cwd, &pools, &command),
        Cmd::Leases => leases(&wt)?,
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_set(value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| format!("`{value}` should be KEY=VALUE"))
}

/// Hooks run from the command line: output on the terminal, and signals
/// passed on, since a hook in a process group of its own no longer gets the
/// terminal's.
fn foreground() -> RunHook {
    RunHook {
        output: HookOutput::Inherit,
        forward: Some(take_pending),
    }
}

/// Runs a hook in the foreground; one the config doesn't declare succeeds.
/// A failure carries a message and the exit code to pass on.
fn run_hook(tree: &Tree, hook: Hook) -> Result<(), (String, u8)> {
    let what = format!("{}'s {} hook", tree.id(), hook.name());
    if tree.config().is_ok_and(|config| match hook {
        Hook::Create => config.lifecycle.create.is_some(),
        Hook::Start => config.lifecycle.start.is_some(),
        Hook::Stop => config.lifecycle.stop.is_some(),
        Hook::Delete => config.lifecycle.delete.is_some(),
    }) {
        eprintln!("wt: running {what}");
    }
    match tree.run_hook(hook, foreground()) {
        Ok(ran) => match ran.problem() {
            None => Ok(()),
            Some(problem) => Err((problem, ran.exit_code())),
        },
        Err(wt::Error::Cancelled) => Err((format!("gave up waiting to run {what}"), 130)),
        Err(error) => Err((format!("couldn't run {what}: {error}"), 1)),
    }
}

/// Copies the tree's inputs and runs its create hook: in the foreground
/// with `wait`, otherwise in a detached process so that `wt new` returns as
/// soon as the worktree exists. Copying a build directory and building
/// take seconds to minutes; the [bin] wrappers refuse until it is done.
fn set_up(tree: &Tree, no_create: bool, wait: bool) -> wt::Result<ExitCode> {
    if no_create {
        tree.copy_inputs()?;
        return Ok(ExitCode::SUCCESS);
    }
    if wait {
        tree.copy_inputs()?;
        catch_for_hooks();
        return Ok(match run_hook(tree, Hook::Create) {
            Ok(()) => ExitCode::SUCCESS,
            Err((problem, code)) => {
                eprintln!("wt: {problem}");
                ExitCode::from(code)
            }
        });
    }
    let program = std::env::current_exe().map_err(|source| wt::Error::Io {
        context: "finding the wt binary".to_owned(),
        source,
    })?;
    let copy: Vec<OsString> = vec![
        program.into(),
        "__copy-inputs".into(),
        tree.root.clone().into(),
    ];
    // Copying goes first in the create hook's own wrapper, so that it holds
    // the tree's lifecycle lock from now until the build is done.
    if tree.spawn_create_after(&copy)?.is_none() {
        let mut setup = Command::new(&copy[0]);
        setup
            .args(&copy[1..])
            .arg("--record")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: setsid is async-signal-safe. A session of its own keeps the
        // copy going when the terminal that started it closes.
        unsafe {
            setup.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        setup.spawn().map_err(|source| wt::Error::Io {
            context: "starting the setup".to_owned(),
            source,
        })?;
        eprintln!("wt: copying {}'s inputs in the background", tree.id());
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!(
        "wt: setting {} up in the background; its output goes to {}",
        tree.id(),
        tree.create_log().display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Records a failed background copy where a failed create hook would be, so
/// `wt ls` and the `[bin]` wrappers report it. The status is written by a
/// rename, so a reader never finds it empty.
fn record_failed_copy(tree: &Tree, message: &str) {
    let directory = tree.root.join(".wt");
    let _ = std::fs::create_dir_all(&directory);
    let _ = std::fs::write(tree.create_log(), message);
    let status = directory.join("create.status");
    let new = directory.join("create.status.new");
    if std::fs::write(&new, "1").is_ok() {
        let _ = std::fs::rename(&new, &status);
    }
}

/// Runs `hook` in one tree, or in every tree whose directory exists, and
/// fails if any run failed. `start` pins each tree, so nothing stops it on
/// its own, and `stop` unpins it.
fn run_everywhere(
    wt: &Wt,
    cwd: &Path,
    tree: Option<String>,
    all: bool,
    hook: Hook,
) -> wt::Result<ExitCode> {
    let trees = if all {
        wt.list()?
            .into_iter()
            .filter(|tree| tree.root.is_dir())
            .collect()
    } else {
        vec![wt.find(tree.as_deref(), cwd)?]
    };
    catch_for_hooks();
    let mut failed = false;
    for tree in &trees {
        let pinning = match hook {
            Hook::Start => tree.pin(),
            Hook::Stop => tree.unpin(),
            _ => Ok(()),
        };
        if let Err(error) = pinning {
            eprintln!("wt: {error}");
            failed = true;
            continue;
        }
        if let Err((problem, code)) = run_hook(tree, hook) {
            eprintln!("wt: {problem}");
            if code == 130 || INTERRUPTED.load(Ordering::SeqCst) {
                return Ok(interrupted());
            }
            failed = true;
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Runs `command` in the tree's environment as this process's child,
/// holding the tree until it exits, unless run from a hook (`WT_IN_HOOK`),
/// which must never hold its own tree. Stays in the current directory when
/// it is inside the tree, and starts at the tree's root otherwise.
fn enter(tree: &Tree, cwd: &Path, mut command: Command) -> wt::Result<ExitCode> {
    let env = tree.env()?;
    env.apply(&mut command);
    let inside = std::fs::canonicalize(cwd).is_ok_and(|cwd| cwd.starts_with(&tree.root));
    command.current_dir(if inside { cwd } else { &tree.root });
    let hold = if std::env::var_os("WT_IN_HOOK").is_some() {
        None
    } else {
        Some(match tree.try_hold()? {
            Some(hold) => hold,
            None => {
                eprintln!("wt: waiting for {}'s stop hook to finish", tree.id());
                tree.hold()?
            }
        })
    };
    // The hold's file isn't inherited, so whatever the command leaves
    // running doesn't keep the tree held once this process is gone.
    forward_termination();
    let code = run_as_parent(command);
    drop(hold);
    Ok(code)
}

fn list(wt: &Wt) -> wt::Result<()> {
    let rows: Vec<[String; 6]> = wt
        .list()?
        .iter()
        .map(|tree| {
            let exists = tree.root.is_dir();
            let branch = if exists {
                tree.branch().unwrap_or_else(|| "(detached)".to_owned())
            } else {
                String::new()
            };
            let ports = tree
                .ports()
                .iter()
                .map(|(name, port)| format!("{name}={port}"))
                .collect::<Vec<_>>()
                .join(" ");
            let status = if !exists {
                "missing".to_owned()
            } else {
                let mut status = vec![match tree.create_status() {
                    CreateStatus::NotRun | CreateStatus::Succeeded => "ready".to_owned(),
                    CreateStatus::Running => "creating".to_owned(),
                    CreateStatus::Failed(code) => format!("create failed ({code})"),
                }];
                if tree.pinned() {
                    status.push("pinned".to_owned());
                }
                match tree.in_use() {
                    InUse::No => {}
                    InUse::Held => status.push("in use".to_owned()),
                    InUse::Stopping => status.push("stopping".to_owned()),
                }
                status.join(", ")
            };
            [
                tree.id(),
                status,
                tree.owner().unwrap_or_default(),
                branch,
                tree.root.display().to_string(),
                ports,
            ]
        })
        .collect();
    let header = ["TREE", "STATUS", "OWNER", "BRANCH", "PATH", "PORTS"].map(str::to_owned);
    let widths: Vec<usize> = (0..5)
        .map(|column| {
            std::iter::once(&header)
                .chain(&rows)
                .map(|row| row[column].len())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            line.push_str(&format!("{cell:width$}  "));
        }
        line.push_str(&row[5]);
        println!("{}", line.trim_end());
    }
    Ok(())
}

/// Leases the pools and runs `command` as this process's child, in the
/// current directory and, inside a tree, with the tree's environment.
fn with(wt: &Wt, cwd: &Path, pools: &[String], command: &[OsString]) -> wt::Result<ExitCode> {
    reap_idle(wt);
    let tree = match wt.find(None, cwd) {
        Ok(tree) => Some(tree),
        Err(wt::Error::NotFound(_)) => None,
        Err(error) => return Err(error),
    };
    let mut child = Command::new(&command[0]);
    child.args(&command[1..]).current_dir(cwd);
    if let Some(tree) = &tree {
        tree.env()?.apply(&mut child);
    }
    let pools: Vec<&str> = pools.iter().map(String::as_str).collect();
    // Caught from here on, not just once the command runs: a SIGTERM while
    // `acquire` runs would otherwise kill wt and free the slot while
    // `acquire` carries on, and the next lease would run a second one on it.
    forward_termination();
    let cancelled = || PENDING.load(Ordering::SeqCst) != 0;
    let lease = match wt.lease_unless(tree.as_ref(), &pools, &cancelled) {
        Err(wt::Error::Cancelled) => {
            return Ok(ExitCode::from(128 + PENDING.load(Ordering::SeqCst) as u8));
        }
        lease => lease?,
    };
    child.envs(lease.vars());
    let code = run_as_parent(child);
    drop(lease);
    Ok(code)
}

static CHILD: AtomicI32 = AtomicI32::new(0);
static PENDING: AtomicI32 = AtomicI32::new(0);
/// A signal was passed on to a hook, so `--all` stops at that tree.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// For [`wt::RunHook::forward`]: the termination signal that arrived since
/// it was last asked, if any.
fn take_pending() -> Option<i32> {
    let signal = PENDING.swap(0, Ordering::SeqCst);
    (signal != 0).then(|| {
        INTERRUPTED.store(true, Ordering::SeqCst);
        signal
    })
}

fn interrupted() -> ExitCode {
    ExitCode::from(130)
}

extern "C" fn record(signal: libc::c_int) {
    PENDING.store(signal, Ordering::SeqCst);
}

/// Records SIGINT, SIGTERM and SIGHUP for [`take_pending`] while hooks run.
/// Without SA_RESTART, so that a wait on a lock behind a long build is
/// interrupted and wt can give up. A signal the caller already ignored (as
/// under `nohup`) stays ignored.
fn catch_for_hooks() {
    // SAFETY: the dispositions are changed while this process has a single
    // thread, and the handler only stores to an atomic.
    unsafe {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = record as *const () as libc::sighandler_t;
            action.sa_flags = 0;
            libc::sigemptyset(&mut action.sa_mask);
            let mut previous: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, &action, &mut previous);
            if previous.sa_sigaction == libc::SIG_IGN {
                libc::sigaction(signal, &previous, std::ptr::null_mut());
            }
        }
    }
}

extern "C" fn forward(signal: libc::c_int) {
    let child = CHILD.load(Ordering::SeqCst);
    if child > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(child, signal) };
    } else {
        PENDING.store(signal, Ordering::SeqCst);
    }
}

/// Passes SIGTERM and SIGHUP on to the command once it runs, and remembers
/// them until then. A signal the caller already ignored (as under `nohup`)
/// stays ignored.
fn forward_termination() {
    // SAFETY: the dispositions are changed while this process has a single
    // thread, and the handler calls only async-signal-safe functions.
    unsafe {
        for signal in [libc::SIGTERM, libc::SIGHUP] {
            let previous = libc::signal(signal, forward as *const () as libc::sighandler_t);
            if previous == libc::SIG_IGN {
                libc::signal(signal, libc::SIG_IGN);
            }
        }
    }
}

/// Runs `command` and waits for it, rather than exec'ing it, because the
/// lease lasts exactly as long as this process. SIGTERM and SIGHUP are
/// passed on to the child. SIGINT is ignored here because the terminal
/// already delivers it to the child, which decides whether to exit. A
/// signal the caller already ignored (as under `nohup`) stays ignored for
/// both.
fn run_as_parent(mut command: Command) -> ExitCode {
    // A termination signal that arrived while the lease was being taken
    // means the command was never wanted.
    let pending = PENDING.swap(0, Ordering::SeqCst);
    if pending != 0 {
        return ExitCode::from(128 + pending as u8);
    }
    // SAFETY: the disposition is changed while this process has a single
    // thread, and the pre_exec closure calls only the async-signal-safe
    // signal().
    unsafe {
        let interrupt = libc::signal(libc::SIGINT, libc::SIG_IGN);
        command.pre_exec(move || {
            libc::signal(libc::SIGINT, interrupt);
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "wt: couldn't run {}: {error}",
                command.get_program().to_string_lossy()
            );
            return ExitCode::from(127);
        }
    };
    let pid = child.id() as i32;
    CHILD.store(pid, Ordering::SeqCst);
    // A signal that arrived before the child existed is passed on now.
    let pending = PENDING.swap(0, Ordering::SeqCst);
    if pending != 0 {
        // SAFETY: a plain syscall on our own child.
        unsafe { libc::kill(pid, pending) };
    }
    match child.wait() {
        Ok(status) => match (status.code(), status.signal()) {
            (Some(code), _) => ExitCode::from(code as u8),
            (None, Some(signal)) => ExitCode::from(128 + signal as u8),
            (None, None) => ExitCode::FAILURE,
        },
        Err(error) => {
            eprintln!("wt: waiting for the command: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Retires idle pool slots. Problems are only warnings: they must not stop
/// the command that was actually asked for.
fn reap_idle(wt: &Wt) {
    match wt.reap_idle() {
        Ok(retired) => {
            for slot in retired {
                if let Err(error) = slot.result {
                    eprintln!("wt: warning: retiring {}: {error}", slot.slot);
                }
            }
        }
        Err(error) => eprintln!("wt: warning: retiring idle pool slots: {error}"),
    }
}

fn leases(wt: &Wt) -> wt::Result<()> {
    reap_idle(wt);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let ago = |at: u64| span(now.saturating_sub(at));
    let mut rows = vec![["POOL", "SLOT", "STATE", "LAST USED BY", "RETIRES"].map(str::to_owned)];
    for slot in wt.leases()? {
        let state = match (&slot.holder, slot.leased) {
            (_, false) => "free".to_owned(),
            (None, true) => "leased".to_owned(),
            (Some(holder), true) => {
                let by = match (&holder.tree, holder.retiring) {
                    (_, true) => "retiring".to_owned(),
                    (Some(tree), false) => format!("leased by {tree}"),
                    (None, false) => "leased".to_owned(),
                };
                format!("{by} (pid {}, {})", holder.pid, ago(holder.since))
            }
        };
        let last = match (&slot.last_tree, slot.released_at) {
            (Some(tree), Some(at)) => format!("{tree}, {} ago", ago(at)),
            (None, Some(at)) => format!("outside a tree, {} ago", ago(at)),
            _ => String::new(),
        };
        let retires = match slot.retire_due {
            Some(due) if due > now => format!("in {}", span(due - now)),
            Some(_) => "now".to_owned(),
            None => String::new(),
        };
        rows.push([slot.pool, slot.name, state, last, retires]);
    }
    let widths: Vec<usize> = (0..4)
        .map(|column| rows.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    for row in &rows {
        let mut line = String::new();
        for (column, cell) in row.iter().enumerate() {
            let width = widths.get(column).copied().unwrap_or(0);
            line.push_str(&format!("{cell:width$}  "));
        }
        println!("{}", line.trim_end());
    }
    Ok(())
}

/// `45s`, `12m`, `3h`.
fn span(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}
