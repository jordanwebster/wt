use std::ffi::OsString;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use wt::{CreateStatus, Hook, Init, NewTree, Remove, Tree, Wt};

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
        /// A value for the `branch` templates, e.g. --set ticket=PRO-1170
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_set)]
        set: Vec<(String, String)>,
        /// Start from the last fetched copy of the default branch
        #[arg(long)]
        no_fetch: bool,
        /// Don't run the create hook
        #[arg(long)]
        no_create: bool,
    },
    /// Run a tree's stop and delete hooks, then remove it (keeping its branch)
    #[command(alias = "remove")]
    Rm {
        tree: String,
        /// Remove despite uncommitted changes or failing hooks
        #[arg(long)]
        force: bool,
        /// Leave the worktree on disk; only stop managing it
        #[arg(long)]
        keep: bool,
    },
    /// Run a tree's start hook
    Start {
        tree: Option<String>,
        /// Every tree
        #[arg(long, conflicts_with = "tree")]
        all: bool,
    },
    /// Run a tree's stop hook
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
        } => {
            let (label, name) = match tree.split_once('/') {
                Some((label, name)) => (Some(label.to_owned()), name.to_owned()),
                None => (None, tree),
            };
            let request = NewTree {
                label,
                from,
                branch,
                vars: set.into_iter().collect(),
                fetch: !no_fetch,
                ..NewTree::named(name)
            };
            let created = wt.new_tree(&cwd, request)?;
            for warning in &created.warnings {
                eprintln!("wt: {warning}");
            }
            let tree = created.tree;
            eprintln!("wt: made {} at {}", tree.id(), tree.root.display());
            if !no_create {
                return run_create(&tree);
            }
        }
        Cmd::Rm { tree, force, keep } => {
            let tree = wt.find(Some(&tree), &cwd)?;
            let options = Remove { force, keep };
            wt.check_remove(&tree, options)?;
            let hooks: &[Hook] = if tree.root.is_dir() {
                &[Hook::Stop, Hook::Delete]
            } else {
                eprintln!(
                    "wt: {} is already gone from disk; not running its hooks",
                    tree.id()
                );
                &[]
            };
            for &hook in hooks {
                if let Err((problem, _)) = run_hook(&tree, hook) {
                    if !force {
                        eprintln!("wt: {problem}; fix it and retry, or pass --force");
                        return Ok(ExitCode::FAILURE);
                    }
                    eprintln!("wt: {problem}; carrying on because of --force");
                }
            }
            let removed = wt.remove(&tree, options)?;
            let kept = removed
                .branch
                .map(|branch| format!("; kept branch {branch}"))
                .unwrap_or_default();
            eprintln!("wt: removed {}{kept}", tree.id());
        }
        Cmd::Start { tree, all } => return run_everywhere(&wt, &cwd, tree, all, Hook::Start),
        Cmd::Stop { tree, all } => return run_everywhere(&wt, &cwd, tree, all, Hook::Stop),
        Cmd::Init {
            path,
            label,
            name,
            no_create,
        } => {
            let tree = wt.init(path.as_deref().unwrap_or(&cwd), Init { label, name })?;
            eprintln!("wt: {} is at {}", tree.id(), tree.root.display());
            if !no_create {
                return run_create(&tree);
            }
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

/// Runs a hook in the foreground; one the config doesn't declare succeeds.
/// A failure carries a message and the exit code to pass on.
fn run_hook(tree: &Tree, hook: Hook) -> Result<(), (String, u8)> {
    let what = format!("{}'s {} hook", tree.id(), hook.name());
    let command = tree
        .hook(hook)
        .map_err(|error| (format!("couldn't prepare {what}: {error}"), 1))?;
    let Some(mut command) = command else {
        return Ok(());
    };
    eprintln!("wt: running {what}");
    let status = command
        .status()
        .map_err(|error| (format!("couldn't run {what}: {error}"), 1))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err((
            format!("{what} failed (exit {code})"),
            code.clamp(1, 255) as u8,
        )),
        None => Err((format!("{what} was killed by a signal"), 1)),
    }
}

fn run_create(tree: &Tree) -> wt::Result<ExitCode> {
    match run_hook(tree, Hook::Create) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err((problem, code)) => {
            eprintln!(
                "wt: {problem}; its output is in {}",
                tree.create_log().display()
            );
            Ok(ExitCode::from(code))
        }
    }
}

/// Runs `hook` in one tree, or in every tree whose directory exists, and
/// fails if any run failed.
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
    let mut failed = false;
    for tree in &trees {
        if let Err((problem, _)) = run_hook(tree, hook) {
            eprintln!("wt: {problem}");
            failed = true;
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Replaces this process with `command`, run in the tree's environment.
/// Stays in the current directory when it is inside the tree, and starts at
/// the tree's root otherwise.
fn enter(tree: &Tree, cwd: &Path, mut command: Command) -> wt::Result<ExitCode> {
    let env = tree.env()?;
    env.apply(&mut command);
    let inside = std::fs::canonicalize(cwd).is_ok_and(|cwd| cwd.starts_with(&tree.root));
    command.current_dir(if inside { cwd } else { &tree.root });
    let error = command.exec();
    eprintln!(
        "wt: couldn't run {}: {error}",
        command.get_program().to_string_lossy()
    );
    Ok(ExitCode::from(127))
}

fn list(wt: &Wt) -> wt::Result<()> {
    let rows: Vec<[String; 5]> = wt
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
                match tree.create_status() {
                    CreateStatus::NotRun | CreateStatus::Succeeded => "ready".to_owned(),
                    CreateStatus::Running => "creating".to_owned(),
                    CreateStatus::Failed(code) => format!("create failed ({code})"),
                }
            };
            [
                tree.id(),
                status,
                branch,
                tree.root.display().to_string(),
                ports,
            ]
        })
        .collect();
    let header = ["TREE", "STATUS", "BRANCH", "PATH", "PORTS"].map(str::to_owned);
    let widths: Vec<usize> = (0..4)
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
        line.push_str(&row[4]);
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
