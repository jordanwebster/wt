use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};
use wt::{Init, Tree, Wt};

#[derive(Parser)]
#[command(name = "wt", version, about = "Parameterised git worktrees")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
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
        Cmd::Init { path, label, name } => {
            let tree = wt.init(path.as_deref().unwrap_or(&cwd), Init { label, name })?;
            println!("{}  {}", tree.id(), tree.root.display());
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
    let rows: Vec<[String; 4]> = wt
        .list()?
        .iter()
        .map(|tree| {
            let exists = tree.root.is_dir();
            let branch = if exists {
                tree.branch().unwrap_or_else(|| "(detached)".to_owned())
            } else {
                "(missing)".to_owned()
            };
            let ports = tree
                .ports()
                .iter()
                .map(|(name, port)| format!("{name}={port}"))
                .collect::<Vec<_>>()
                .join(" ");
            [tree.id(), branch, tree.root.display().to_string(), ports]
        })
        .collect();
    let header = ["TREE", "BRANCH", "PATH", "PORTS"].map(str::to_owned);
    let widths: Vec<usize> = (0..3)
        .map(|column| {
            std::iter::once(&header)
                .chain(&rows)
                .map(|row| row[column].len())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let line = format!(
            "{:w0$}  {:w1$}  {:w2$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2]
        );
        println!("{}", line.trim_end());
    }
    Ok(())
}
