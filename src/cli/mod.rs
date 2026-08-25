use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Color {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "wt",
    version,
    about = "Worktree manager for humans and coding agents"
)]
#[command(after_help = "Example: wt register . && wt new repo/feature")]
pub struct Cli {
    /// Emit one stable JSON envelope.
    #[arg(long, global = true)]
    pub json: bool,
    /// Consent to destructive operations without prompting.
    #[arg(long, global = true)]
    pub yes: bool,
    /// Suppress optional notices.
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Show notices even when stderr is not a terminal.
    #[arg(long, global = true)]
    pub verbose: bool,
    /// Control coloured terminal output.
    #[arg(long, global = true, value_enum, default_value_t = Color::Auto)]
    pub color: Color,
    /// Use an alternate wt state directory.
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Register a canonical checkout.
    Register(Register),
    /// Tear down and forget a registered repository.
    Unregister(Unregister),
    /// Clone and register a repository.
    Clone(CloneRepo),
    /// Create or resume a linked worktree.
    New(New),
    /// Adopt an existing git worktree.
    Adopt(Adopt),
    /// List registered trees.
    List(List),
    /// Report one tree's state and tasks.
    Status(Status),
    /// Print a tree root.
    Path(TargetArg),
    /// Run a declared task.
    Run(Run),
    /// Synchronise a tree's dependencies.
    Sync(Sync),
    /// Run the test task.
    #[command(after_help = "Example: wt test project/feature")]
    Test(AliasRun),
    /// Run the lint task.
    #[command(after_help = "Example: wt lint project/feature")]
    Lint(AliasRun),
    /// Run the format task.
    #[command(after_help = "Example: wt fmt project/feature")]
    Fmt(AliasRun),
    /// Run the build task.
    #[command(after_help = "Example: wt build project/feature")]
    Build(AliasRun),
    /// Run a one-shot command through a passthrough door.
    #[command(
        override_help = "Run a one-shot command through a passthrough door.\n\nUsage: wt exec [OPTIONS] [TARGET] -- <CMD>...\n\nArguments:\n  [TARGET]  Registered tree target\n  <CMD>...  Command and arguments to execute\n\nOptions:\n      --yes            Consent without prompting\n      --quiet          Suppress optional notices\n      --verbose        Show notices when stderr is not a terminal\n      --color <COLOR>  Control coloured output [default: auto] [possible values: auto, always, never]\n      --home <DIR>     Use an alternate wt state directory\n  -h, --help           Print help\n\nPassthrough door; not a task (see `wt run`); no `--json` (A20).\n\nExample: wt exec project/feature -- env"
    )]
    Exec(Exec),
    /// Start an interactive shell door.
    Shell(ShellDoor),
    /// Print a tree's assembled environment.
    Env(Env),
    /// Open or attach to an agent session.
    Open(Open),
    /// Close agent sessions.
    Close(Close),
    /// Tear down and remove a linked tree.
    Remove(Remove),
    /// Report or clean stale tree records.
    Prune(Prune),
    /// Destroy a declared resource.
    Destroy(ResourceAction),
    /// Destroy and recreate a declared resource.
    #[command(after_help = "Example: wt refresh daemon project/feature --yes")]
    Refresh(ResourceAction),
    /// Diagnose registered state and tooling.
    Doctor(Doctor),
    /// List effective tasks.
    Tasks(Tasks),
    /// Show effective configuration origins.
    Config(Config),
    /// Resolve a command through a tree door's PATH.
    Which(Which),
    /// List wt coordination locks.
    Locks(Locks),
    /// Print shell helper initialisation.
    ShellInit(Script),
    /// Print dynamic shell completions.
    #[command(after_help = "Example: wt completions zsh")]
    Completions(Script),
}

impl Command {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Register(_) => "register",
            Self::Unregister(_) => "unregister",
            Self::Clone(_) => "clone",
            Self::New(_) => "new",
            Self::Adopt(_) => "adopt",
            Self::List(_) => "list",
            Self::Status(_) => "status",
            Self::Path(_) => "path",
            Self::Run(_) => "run",
            Self::Sync(_) => "sync",
            Self::Test(_) => "test",
            Self::Lint(_) => "lint",
            Self::Fmt(_) => "fmt",
            Self::Build(_) => "build",
            Self::Exec(_) => "exec",
            Self::Shell(_) => "shell",
            Self::Env(_) => "env",
            Self::Open(_) => "open",
            Self::Close(_) => "close",
            Self::Remove(_) => "remove",
            Self::Prune(_) => "prune",
            Self::Destroy(_) => "destroy",
            Self::Refresh(_) => "refresh",
            Self::Doctor(_) => "doctor",
            Self::Tasks(_) => "tasks",
            Self::Config(_) => "config",
            Self::Which(_) => "which",
            Self::Locks(_) => "locks",
            Self::ShellInit(_) => "shell-init",
            Self::Completions(_) => "completions",
        }
    }
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt register . --label project")]
pub struct Register {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    #[arg(long)]
    pub label: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub move_to: Option<PathBuf>,
    /// Restore a canonical checkout's missing wt identity marker.
    #[arg(long)]
    pub repair: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt unregister project --yes")]
pub struct Unregister {
    pub label: String,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt clone https://example.test/repo.git")]
pub struct CloneRepo {
    pub url: String,
    #[arg(long)]
    pub label: Option<String>,
    #[arg(long)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt new project/feature --from main")]
pub struct New {
    pub target: String,
    #[arg(long)]
    pub branch: Option<String>,
    #[arg(long = "from")]
    pub from_ref: Option<String>,
    #[arg(long)]
    pub detach: bool,
    #[arg(long)]
    pub no_sync: bool,
    #[arg(long)]
    pub verify: bool,
    #[arg(long)]
    pub no_fetch: bool,
    #[arg(long)]
    pub no_open: bool,
    #[arg(long)]
    pub no_attach: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt adopt ../existing --label project --name feature")]
pub struct Adopt {
    pub path: PathBuf,
    #[arg(long)]
    pub label: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt list --json")]
pub struct List {
    pub label: Option<String>,
    #[arg(long)]
    pub probe: bool,
    #[arg(long)]
    pub fast: bool,
    #[arg(long)]
    pub disk: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt status project/feature")]
pub struct Status {
    pub target: Option<String>,
    #[arg(long)]
    pub probe: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt path project/feature")]
pub struct TargetArg {
    pub target: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt run test project/feature --json")]
pub struct Run {
    pub task: String,
    pub target: Option<String>,
    #[arg(long)]
    pub wait: Option<String>,
    #[arg(long)]
    pub timeout: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub no_log: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt test project/feature")]
pub struct AliasRun {
    pub target: Option<String>,
    #[arg(long)]
    pub wait: Option<String>,
    #[arg(long)]
    pub timeout: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub no_log: bool,
}

impl AliasRun {
    pub fn into_run(self, task: &str) -> Run {
        Run {
            task: task.to_owned(),
            target: self.target,
            wait: self.wait,
            timeout: self.timeout,
            dry_run: self.dry_run,
            no_log: self.no_log,
        }
    }
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt sync project/feature")]
pub struct Sync {
    pub target: Option<String>,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt exec project/feature -- sh -c 'env'")]
pub struct Exec {
    pub target: Option<String>,
    #[arg(long, hide = true)]
    pub no_gate: bool,
    #[arg(last = true, required = true, num_args = 1..)]
    pub cmd: Vec<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt shell project/feature")]
pub struct ShellDoor {
    pub target: Option<String>,
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("format").args(["sh", "dotenv"]).multiple(false)))]
#[command(after_help = "Example: wt env project/feature --sh")]
pub struct Env {
    pub target: Option<String>,
    #[arg(long)]
    pub sh: bool,
    #[arg(long)]
    pub dotenv: bool,
    #[arg(long)]
    pub deactivate: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt open project/feature --agent codex --no-attach")]
pub struct Open {
    pub target: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub no_attach: bool,
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt close project/feature")]
pub struct Close {
    pub target: Option<String>,
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt remove project/feature --yes")]
pub struct Remove {
    pub target: String,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub delete_branch: bool,
    #[arg(long)]
    pub keep_orphans: bool,
    #[arg(long)]
    pub wait: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt prune --yes")]
pub struct Prune {
    pub label: Option<String>,
    #[arg(long)]
    pub merged: bool,
    #[arg(long)]
    pub gone: bool,
    #[arg(long)]
    pub records: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt destroy daemon project/feature --yes")]
pub struct ResourceAction {
    pub task: String,
    pub target: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt doctor project --json")]
pub struct Doctor {
    pub label: Option<String>,
    #[arg(long)]
    pub probe: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt tasks project/feature --private")]
pub struct Tasks {
    pub target: Option<String>,
    #[arg(long)]
    pub private: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt config project/feature --origin")]
pub struct Config {
    pub target: Option<String>,
    #[arg(long)]
    pub origin: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt which project/feature cargo")]
pub struct Which {
    #[arg(required = true, num_args = 1..=2)]
    pub values: Vec<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt locks project")]
pub struct Locks {
    pub label: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt shell-init zsh")]
pub struct Script {
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn parse() -> Cli {
    let names = [
        "register",
        "unregister",
        "clone",
        "new",
        "adopt",
        "list",
        "status",
        "path",
        "run",
        "sync",
        "test",
        "lint",
        "fmt",
        "build",
        "exec",
        "shell",
        "env",
        "open",
        "close",
        "remove",
        "prune",
        "destroy",
        "refresh",
        "doctor",
        "tasks",
        "config",
        "which",
        "locks",
        "shell-init",
        "completions",
    ];
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut skip = false;
    if let Some(candidate) = args.iter().find(|arg| {
        if skip {
            skip = false;
            return false;
        }
        if arg.as_str() == "--home" {
            skip = true;
            return false;
        }
        !arg.starts_with('-')
    }) {
        if !names.contains(&candidate.as_str()) {
            let mut nearest = names
                .iter()
                .map(|name| (*name, edit_distance(candidate, name)))
                .collect::<Vec<_>>();
            nearest.sort_by_key(|(name, distance)| (*distance, *name));
            eprintln!(
                "error: unrecognized subcommand '{candidate}'\n\n  tip: the three closest commands are: {}",
                nearest
                    .iter()
                    .take(3)
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            std::process::exit(2);
        }
    }
    Cli::parse()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut row = (0..=right.chars().count()).collect::<Vec<_>>();
    for (i, a) in left.chars().enumerate() {
        let mut prior = row[0];
        row[0] = i + 1;
        for (j, b) in right.chars().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(prior + usize::from(a != b));
            prior = above;
        }
    }
    row[right.chars().count()]
}
