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
    about = "Worktree manager for humans and coding agents",
    override_help = "Worktree manager for humans and coding agents\n\nUsage: wt [OPTIONS] <COMMAND>\n\nEveryday:\n  new          Create or resume a linked worktree\n  open         Open or attach to an agent session\n  edit         Open a tree in the configured editor\n  with         Ensure resources and lease pools for one command\n  leases       List pool instances, slots, and holders\n  ls           List registered trees [aliases: list]\n  status       Report one tree's state\n  rm           Tear down and remove a linked tree [aliases: remove]\n\nSetup:\n  setup        Find repositories and settle wt's environment\n  clone        Clone a repository into a hub wt owns and register it\n  register     Register a canonical checkout\n  adopt        Adopt an existing git worktree\n  shell-init   Print shell helper initialisation\n  completions  Print dynamic shell completions\n\nWorking inside a tree:\n  exec         Run a one-shot command through a passthrough door\n  shell        Start an interactive shell door\n  env          Print a tree's assembled environment\n  path         Print a tree root\n  which        Resolve a command through a tree door's PATH\n  config       Show effective configuration origins\n  meta         Show or edit a tree's user metadata\n\nUpkeep:\n  sync         Synchronise a tree's dependencies\n  anchor       Refresh and warm a repository's canonical checkout\n  doctor       Diagnose registered state and tooling\n  prune        Report or clean stale state\n  close        Close agent sessions\n  forget       Forget wt's records for a linked tree without removing it\n  destroy      Destroy a declared resource\n  refresh      Destroy and recreate a declared resource\n  unregister   Tear down and forget a registered repository\n\nOptions:\n      --json           Emit one stable JSON envelope\n      --yes            Consent to destructive operations without prompting\n      --quiet          Suppress optional notices\n      --verbose        Show notices even when stderr is not a terminal\n      --color <COLOR>  Control coloured terminal output [default: auto] [possible values: auto, always, never]\n      --home <DIR>     Use an alternate wt state directory\n  -h, --help           Print help\n  -V, --version        Print version\n\nExample: wt register . && wt new repo/feature"
)]
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
    /// Run a lifecycle hook in a prepared tree.
    #[command(name = "__hook", hide = true)]
    Hook(HookRun),
    /// Create or resume a linked worktree.
    New(New),
    /// Open or attach to an agent session.
    Open(Open),
    /// Open a tree in the configured editor.
    Edit(Edit),
    /// Ensure resources and optionally lease pools for one command.
    With(With),
    /// List registered trees.
    #[command(name = "ls", visible_alias = "list")]
    List(List),
    /// Report one tree's state and resources.
    Status(Status),
    /// Tear down and remove a linked tree.
    #[command(name = "rm", visible_alias = "remove")]
    Remove(Remove),

    /// Find repositories and settle wt's environment.
    Setup(Setup),
    /// Clone a repository into a hub wt owns and register it.
    Clone(CloneRepo),
    /// Register a canonical checkout.
    Register(Register),
    /// Adopt an existing git worktree.
    Adopt(Adopt),
    /// Print shell helper initialisation.
    ShellInit(Script),
    /// Print dynamic shell completions.
    #[command(after_help = "Example: wt completions zsh")]
    Completions(Script),

    /// Run a one-shot command through a passthrough door.
    #[command(
        override_help = "Run a one-shot command through a passthrough door.\n\nUsage: wt exec [OPTIONS] [TARGET] -- <CMD>...\n\nArguments:\n  [TARGET]  Registered tree target\n  <CMD>...  Command and arguments to execute\n\nOptions:\n      --yes            Consent without prompting\n      --quiet          Suppress optional notices\n      --verbose        Show notices when stderr is not a terminal\n      --color <COLOR>  Control coloured output [default: auto] [possible values: auto, always, never]\n      --home <DIR>     Use an alternate wt state directory\n  -h, --help           Print help\n\nPassthrough door; no `--json`.\n\nExample: wt exec project/feature -- env"
    )]
    Exec(Exec),
    /// Start an interactive shell door.
    Shell(ShellDoor),
    /// Print a tree's assembled environment.
    Env(Env),
    /// Print a tree root.
    Path(TargetArg),
    /// Resolve a command through a tree door's PATH.
    Which(Which),
    /// Show effective configuration origins.
    Config(Config),
    /// Show or edit a tree's user metadata.
    Meta(Meta),

    /// Synchronise a tree's dependencies.
    Sync(Sync),
    /// Refresh and warm a repository's canonical checkout.
    Anchor(Anchor),
    /// Diagnose registered state and tooling.
    Doctor(Doctor),
    /// Report or clean stale tree records.
    Prune(Prune),
    /// Close agent sessions.
    Close(Close),
    /// Forget wt's records for a linked tree without removing it.
    Forget(Forget),
    /// Destroy a declared resource.
    Destroy(ResourceAction),
    /// Destroy and recreate a declared resource.
    #[command(after_help = "Example: wt refresh daemon project/feature --yes")]
    Refresh(ResourceAction),
    /// List resource-pool leases.
    Leases(Leases),
    /// Tear down and forget a registered repository.
    Unregister(Unregister),
}

impl Command {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Hook(_) => "__hook",
            Self::New(_) => "new",
            Self::Open(_) => "open",
            Self::Edit(_) => "edit",
            Self::With(_) => "with",
            Self::List(_) => "list",
            Self::Status(_) => "status",
            Self::Remove(_) => "remove",
            Self::Setup(_) => "setup",
            Self::Clone(_) => "clone",
            Self::Register(_) => "register",
            Self::Adopt(_) => "adopt",
            Self::ShellInit(_) => "shell-init",
            Self::Completions(_) => "completions",
            Self::Exec(_) => "exec",
            Self::Shell(_) => "shell",
            Self::Env(_) => "env",
            Self::Path(_) => "path",
            Self::Which(_) => "which",
            Self::Config(_) => "config",
            Self::Meta(_) => "meta",
            Self::Sync(_) => "sync",
            Self::Anchor(_) => "anchor",
            Self::Doctor(_) => "doctor",
            Self::Prune(_) => "prune",
            Self::Close(_) => "close",
            Self::Forget(_) => "forget",
            Self::Destroy(_) => "destroy",
            Self::Refresh(_) => "refresh",
            Self::Leases(_) => "leases",
            Self::Unregister(_) => "unregister",
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn override_help_names_every_subcommand() {
        let mut command = Cli::command();
        let names = command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(|subcommand| subcommand.get_name().to_owned())
            .collect::<Vec<_>>();
        let help = command.render_help().to_string();
        for name in names {
            assert!(
                help.lines().any(|line| {
                    line.trim_start()
                        .strip_prefix(&name)
                        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
                }),
                "override_help omits clap subcommand `{name}`"
            );
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
pub struct HookRun {
    pub kind: String,
    pub target: String,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt unregister project --yes")]
pub struct Unregister {
    pub label: String,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Interactive; needs a terminal. Composes `register`, `adopt` and configuration writes, so `--dry-run` prints the run as commands. No `--json` (A76).\n\nExample: wt setup ~/work"
)]
pub struct Setup {
    /// Extra directories to search, alongside the home directory.
    pub paths: Vec<PathBuf>,
    /// Print the plan as the commands that would produce it.
    #[arg(long)]
    pub dry_run: bool,
    /// Install into this shell rather than the detected one.
    #[arg(long, value_enum)]
    pub shell: Option<Shell>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Creates a bare hub and a canonical checkout wt owns, detached at the default branch; the branch itself is never checked out.\n\nExample: wt clone https://example.test/repo.git"
)]
pub struct CloneRepo {
    pub url: String,
    #[arg(long)]
    pub label: Option<String>,
    /// Directory to hold `hub.git` and `canonical` (default: `$WT_HOME/repos/<label>`).
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
    pub no_fetch: bool,
    #[arg(long)]
    pub no_open: bool,
    #[arg(long)]
    pub no_attach: bool,
    #[arg(long = "no-after-new")]
    pub no_after_new: bool,
    /// Attach opaque metadata to the new tree.
    #[arg(long = "meta", value_name = "k=v")]
    pub meta: Vec<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt adopt ../existing --label project --name feature")]
pub struct Adopt {
    pub path: PathBuf,
    #[arg(long)]
    pub label: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    /// Attach opaque metadata to the adopted tree.
    #[arg(long = "meta", value_name = "k=v")]
    pub meta: Vec<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt ls --json")]
pub struct List {
    pub label: Option<String>,
    #[arg(long)]
    pub probe: bool,
    #[arg(long)]
    pub fast: bool,
    #[arg(long)]
    pub disk: bool,
    /// Add one metadata value column to the human table.
    #[arg(long, value_name = "KEY")]
    pub meta: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt status project/feature")]
pub struct Status {
    pub target: Option<String>,
    #[arg(long)]
    pub probe: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt meta project/feature ticket=ABC-123")]
pub struct Meta {
    pub target: String,
    #[arg(value_name = "k=v")]
    pub edits: Vec<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt path project/feature")]
pub struct TargetArg {
    pub target: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt with database -- just test")]
pub struct With {
    #[arg(required = true, num_args = 1..)]
    pub resources: Vec<String>,
    #[arg(long)]
    pub take: bool,
    #[arg(last = true)]
    pub cmd: Vec<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Runs by itself after `wt new`, `wt rm` and `wt sync` for repositories whose output roots seed a new tree.\n\nExample: wt anchor project"
)]
pub struct Anchor {
    /// The registered repository whose canonical checkout to refresh.
    pub label: String,
    /// Skip the fetch: fast-forward to what origin's default branch already is, then warm.
    #[arg(long)]
    pub no_fetch: bool,
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
#[command(
    after_help = "Terminal editors and cold GUI launches inherit the full door environment; a GUI editor CLI that forwards to an already-running instance does not (use `wt exec` in run configs or a `wt shell` terminal profile — see the cookbook).\n\nExample: wt edit project/feature"
)]
pub struct Edit {
    pub target: Option<String>,
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
#[command(after_help = "Example: wt forget project/feature --yes")]
pub struct Forget {
    pub target: String,
}

#[derive(Debug, Args)]
#[command(after_help = "Example: wt rm project/feature")]
pub struct Remove {
    pub target: String,
    #[arg(long)]
    pub force: bool,
    /// Delete the tree's branch even when no remote carries its commits.
    #[arg(long, conflicts_with = "keep_branch")]
    pub delete_branch: bool,
    /// Keep the tree's branch that removal would otherwise delete.
    #[arg(long)]
    pub keep_branch: bool,
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
    pub resource: String,
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
#[command(after_help = "Example: wt leases repo")]
pub struct Leases {
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
        "__hook",
        "new",
        "open",
        "edit",
        "with",
        "ls",
        "list",
        "status",
        "rm",
        "remove",
        "setup",
        "clone",
        "register",
        "adopt",
        "shell-init",
        "completions",
        "exec",
        "shell",
        "env",
        "path",
        "which",
        "config",
        "meta",
        "sync",
        "anchor",
        "doctor",
        "prune",
        "close",
        "forget",
        "destroy",
        "refresh",
        "leases",
        "unregister",
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
