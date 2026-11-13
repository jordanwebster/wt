use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

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
    match command {
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
                let code = run_hook(&tree, hook)?;
                if code != 0 && !force {
                    eprintln!(
                        "wt: {}'s {} hook failed (exit {code}); fix it and retry, or pass --force",
                        tree.id(),
                        hook.name()
                    );
                    return Ok(ExitCode::FAILURE);
                }
            }
            let removed = wt.remove(&tree, Remove { force, keep })?;
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
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_set(value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| format!("`{value}` should be KEY=VALUE"))
}

/// Runs a hook in the foreground and returns its exit code; 0 when the
/// config doesn't declare it.
fn run_hook(tree: &Tree, hook: Hook) -> wt::Result<i32> {
    let Some(mut command) = tree.hook(hook)? else {
        return Ok(0);
    };
    eprintln!("wt: running {}'s {} hook", tree.id(), hook.name());
    let status = command.status().map_err(|source| wt::Error::Io {
        context: format!("running {}'s {} hook", tree.id(), hook.name()),
        source,
    })?;
    Ok(status.code().unwrap_or(1))
}

fn run_create(tree: &Tree) -> wt::Result<ExitCode> {
    let code = run_hook(tree, Hook::Create)?;
    if code == 0 {
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!(
        "wt: {}'s create hook failed (exit {code}); its output is in {}",
        tree.id(),
        tree.create_log().display()
    );
    Ok(ExitCode::from(code.clamp(1, 255) as u8))
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
        let code = run_hook(tree, hook)?;
        if code != 0 {
            eprintln!(
                "wt: {}'s {} hook failed (exit {code})",
                tree.id(),
                hook.name()
            );
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
