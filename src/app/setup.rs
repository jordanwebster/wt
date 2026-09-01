//! `wt setup` — the interactive onboarding verb (A76, §14.7).
//!
//! Every effect here is one an existing verb already performs. Discovery runs
//! on a background thread while the questions that do not depend on it are
//! answered, all answers are gathered before anything is written, and one
//! consent covers the whole plan. `--dry-run` takes the default answer to
//! every question and prints the plan without asking anything, which is also
//! how the whole pipeline is tested.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use serde::Serialize;
use wt_core::setup::{self as model, Bucket, CheckoutObs, Step, WorktreeObs};
use wt_core::tmuxconf::{self, Delta, TmuxObs};
use wt_core::tui::{self, Card, Mode, Outcome, Row};
use wt_core::{CoreError, ExitClass};

use crate::cli::{Setup, Shell};

use super::{Context, Output};

/// The frame interval while something is animating: fast enough to read, slow
/// enough that every frame over a slow link is not a wasted packet.
const FRAME: Duration = Duration::from_millis(66);
/// How long an idle interface waits for a key. Nothing is animating, so a long
/// wait costs nothing and keeps the process asleep.
const IDLE: Duration = Duration::from_secs(3600);
/// The bound on any one probe subprocess.
const PROBE: Duration = Duration::from_secs(5);
/// The bound on the whole walk.
const WALK_BUDGET: Duration = Duration::from_secs(20);

/// `setup` refuses `--json`, so this is only ever rendered as text; it exists
/// so the command's result is still structured data rather than a string.
#[derive(Clone, Debug, Serialize)]
struct SetupData {
    applied: bool,
    steps: Vec<StepReport>,
}

#[derive(Clone, Debug, Serialize)]
struct StepReport {
    command: String,
    status: &'static str,
    detail: String,
}

pub(crate) fn run(context: &mut Context, args: Setup) -> Result<Output, CoreError> {
    if context.json {
        // The envelope describes one operation's result, and a session of
        // questions is not one (A76, A20).
        return Err(CoreError::new(
            ExitClass::Usage,
            "JSON_UNSUPPORTED",
            "setup is a session of questions, not one operation with an envelope",
            "register repositories with `wt register --json`, or run `wt setup --dry-run`",
        ));
    }

    let environment = Environment::probe(context, args.shell)?;
    if args.dry_run {
        return dry_run(context, &args.paths, &environment);
    }
    if !context.tty.stdin {
        return Err(CoreError::new(
            ExitClass::Usage,
            "CONFIRM_REQUIRED",
            "setup asks questions and needs a terminal",
            "register repositories directly with `wt register <path>`, or print the default plan with `wt setup --dry-run`",
        ));
    }

    let scan = Scan::start(context, &args.paths);
    let mut state = tui::State::new(cards(context, &environment));
    let (quit, buckets) = drive(context, &environment, &mut state, scan)?;
    if quit {
        return Output::text(
            SetupData {
                applied: false,
                steps: Vec::new(),
            },
            "setup left; nothing was changed",
        );
    }

    let steps = compose(context, &state, &environment, &buckets);
    if steps.is_empty() {
        return Output::text(
            SetupData {
                applied: false,
                steps: Vec::new(),
            },
            "setup found nothing to change",
        );
    }

    // The consent reads as sentences: the shell lines are for `--dry-run`,
    // where a script or an agent wants them. A person wants to know what
    // will happen, in the words the cards used.
    let described = steps
        .iter()
        .map(|step| format!("  {}", sentence(step, &environment)))
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!("\nsetup will\n{described}\n");
    if !context.confirm("apply")? {
        return Output::text(
            SetupData {
                applied: false,
                steps: Vec::new(),
            },
            "setup declined; nothing was changed",
        );
    }
    apply(context, &steps, &environment)
}

/// The default answer to every question, with every repository found ticked,
/// as the commands it would run.
///
/// Nothing is asked, so no terminal is needed: this is what an agent, or a
/// test, runs to see what `setup` could do on this machine.
fn dry_run(
    context: &Context,
    extra: &[PathBuf],
    environment: &Environment,
) -> Result<Output, CoreError> {
    let now = wt_sys::fsx::epoch_seconds();
    let (checkouts, truncated) = Discovery::of(context, extra).run(&mut |_| {});
    let buckets = bucket(context, checkouts);
    let mut cards = cards(context, environment);
    if let Some(card) = cards.iter_mut().find(|card| card.key == "repos") {
        fill_repos(card, &buckets, &environment.home, now, truncated);
        // Interactively nothing is ticked; a dry run answers "what could
        // setup do here", so it ticks everything it found.
        card.set_all(true);
    }
    let state = tui::State::new(cards);
    let steps = compose(context, &state, environment, &buckets);
    let mut text = if steps.is_empty() {
        "setup would change nothing".to_owned()
    } else {
        format!("setup would run:\n{}", plan_text(&steps))
    };
    if truncated {
        text.push_str(&format!("\n\n{TRUNCATED}"));
    }
    Output::text(
        SetupData {
            applied: false,
            steps: steps
                .iter()
                .map(|step| report(step, "planned", ""))
                .collect(),
        },
        text,
    )
}

/// What a walk that ran out of time says about itself.
const TRUNCATED: &str =
    "the search ran out of time, so some repositories may be missing; re-run with the directory they are in";

fn plan_text(steps: &[Step]) -> String {
    steps
        .iter()
        .map(|step| format!("  {}", step.command()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn report(step: &Step, status: &'static str, detail: impl Into<String>) -> StepReport {
    StepReport {
        command: step.command(),
        status,
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------- environment

/// One shell's rc file and what is already in it.
struct RcFile {
    shell: String,
    /// The file as named, before symlinks are resolved.
    path: PathBuf,
    /// The file actually written to, which a symlink may put elsewhere.
    real: PathBuf,
    installed: bool,
    exists: bool,
}

enum TmuxState {
    /// tmux is not installed.
    Absent {
        manager: Option<&'static model::PackageManager>,
        /// A configuration already on disk, which tmux will read once it is
        /// installed; nothing about it can be observed until then.
        existing: Option<PathBuf>,
    },
    /// tmux is installed but older than wt's session backend needs (§5.4).
    Outdated,
    /// tmux is installed and has no configuration to preserve.
    Unconfigured { path: PathBuf },
    /// tmux is installed and already configured.
    Configured {
        real: PathBuf,
        deltas: Vec<Delta>,
        /// Whether the configuration could actually be read.
        read: bool,
    },
}

struct Environment {
    home: PathBuf,
    /// `session.backend = "none"` is written to `config.toml`: an earlier
    /// `register` on a machine without tmux resolved it that way (§5.4), and
    /// nothing re-resolves a declared value, so installing tmux has to
    /// declare it back.
    backend_declared_none: bool,
    detected_shell: Option<String>,
    shells: Vec<RcFile>,
    tmux: TmuxState,
    /// What the terminal and terminfo say, which is what a generated tmux
    /// configuration has to describe. Observable whether or not tmux is
    /// installed, so it is read regardless.
    obs: TmuxObs,
    agents: Vec<String>,
    trees_dir: String,
}

impl Environment {
    fn probe(context: &Context, forced: Option<Shell>) -> Result<Self, CoreError> {
        let home = context
            .parent_env
            .get("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::State,
                    "HOME_UNRESOLVED",
                    "HOME is not set",
                    "set HOME, or register repositories with `wt register`",
                )
            })?;
        let detected_shell = match forced {
            Some(shell) => Some(shell.as_str().to_owned()),
            None => wt_sys::probe::detected_shell(PROBE),
        };

        let mut shells = Vec::new();
        for shell in model::SHELLS {
            let Some(relative) = model::rc_file(shell) else {
                continue;
            };
            let path = home.join(relative);
            let exists = !matches!(
                wt_sys::fsx::path_kind(&path)?,
                wt_sys::fsx::PathKind::Missing
            );
            // The consent names the file that is actually written, which a
            // symlink into a dotfiles repository puts somewhere else entirely.
            let real = if exists {
                wt_sys::fsx::canonicalize(&path).unwrap_or_else(|_| path.clone())
            } else {
                path.clone()
            };
            // An rc file that cannot be read is offered rather than fatal: the
            // append will say so if it cannot be written either.
            let installed = wt_sys::fsx::read_string(&real)
                .ok()
                .flatten()
                .is_some_and(|contents| model::block_installed(&contents));
            shells.push(RcFile {
                shell: (*shell).to_owned(),
                path,
                real,
                installed,
                exists,
            });
        }

        let obs = TmuxObs {
            term: context.parent_env.get("TERM").cloned(),
            tmux_256color: wt_sys::probe::has_terminfo("tmux-256color", PROBE),
        };
        let tmux = Self::probe_tmux(&home, &obs)?;
        let agents = context
            .settings
            .agents
            .keys()
            .filter(|agent| wt_sys::probe::installed(agent))
            .cloned()
            .collect();
        let trees_dir = context
            .settings
            .trees_dir
            .clone()
            .unwrap_or_else(|| context.home.join("trees").to_string_lossy().into_owned());

        let settings_source =
            wt_sys::fsx::read_string(&context.home.join("config.toml"))?.unwrap_or_default();
        let backend_declared_none = wt_core::settings::backend_is_declared(&settings_source)?
            && matches!(
                context.settings.session.backend,
                wt_core::settings::SessionBackend::None
            );

        Ok(Self {
            home,
            backend_declared_none,
            detected_shell,
            shells,
            tmux,
            obs,
            agents,
            trees_dir,
        })
    }

    /// The configuration file tmux would read, if there is one. tmux tries
    /// `~/.tmux.conf` first and the XDG location only when that is absent.
    fn existing_tmux_config(home: &Path) -> Result<Option<PathBuf>, CoreError> {
        for candidate in [home.join(".tmux.conf"), home.join(".config/tmux/tmux.conf")] {
            if !matches!(
                wt_sys::fsx::path_kind(&candidate)?,
                wt_sys::fsx::PathKind::Missing
            ) {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    fn probe_tmux(home: &Path, obs: &TmuxObs) -> Result<TmuxState, CoreError> {
        let existing = Self::existing_tmux_config(home)?;
        if !wt_sys::probe::installed("tmux") {
            return Ok(TmuxState::Absent {
                manager: wt_sys::probe::package_manager(),
                existing,
            });
        }
        // §5.4's gate, not a PATH check: tmux below 3.2 has neither
        // `terminal-features` nor the extended-keys handling the generated
        // configuration depends on, so it is not a configuration target.
        // Which package manages it is unknowable, so no upgrade is offered.
        if wt_sys::tmux::Tmux::new("tmux", PROBE)
            .check_version()
            .is_err()
        {
            return Ok(TmuxState::Outdated);
        }
        let Some(path) = existing else {
            return Ok(TmuxState::Unconfigured {
                path: home.join(".config/tmux/tmux.conf"),
            });
        };
        // A probe that could not run leaves the configuration alone. Treating
        // an unreadable configuration as an empty one would propose appending
        // every option wt wants to a file that may already set them all — but
        // it is reported as unread rather than as agreement.
        let (deltas, read) = match wt_sys::tmux::probe_effective("tmux", Some(&path), PROBE) {
            Ok(options) => (tmuxconf::deltas(obs, &effective(options)), true),
            Err(_) => (Vec::new(), false),
        };
        let real = wt_sys::fsx::canonicalize(&path).unwrap_or(path);
        Ok(TmuxState::Configured { real, deltas, read })
    }

    /// A path written the way a person reads it.
    fn short(&self, path: &Path) -> String {
        short_path(&self.home, path)
    }
}

/// A path written the way a person reads it: relative to home where it can be.
fn short_path(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn effective(options: wt_sys::tmux::EffectiveOptions) -> tmuxconf::Effective {
    tmuxconf::Effective {
        default_terminal: options.default_terminal,
        extended_keys: options.extended_keys,
        terminal_features: options.terminal_features,
        mouse: options.mouse,
    }
}

// ------------------------------------------------------------------- scanning

/// What discovery needs from the context, captured before a thread starts.
struct Discovery {
    roots: Vec<PathBuf>,
    known: KnownRegistry,
    deadlines: Option<wt_sys::git::Deadlines>,
}

impl Discovery {
    fn of(context: &Context, extra: &[PathBuf]) -> Self {
        let mut roots = Vec::new();
        if let Some(home) = context.parent_env.get("HOME") {
            roots.push(PathBuf::from(home));
        }
        roots.extend(extra.iter().cloned());
        Self {
            roots,
            known: KnownRegistry::of(context),
            deadlines: wt_sys::git::Deadlines::from_settings(&context.settings.git.timeouts).ok(),
        }
    }

    /// Walks the machine and describes what it finds. Runs on the background
    /// thread of an interactive session and inline for `--dry-run`.
    ///
    /// Returns the checkouts and whether the walk ran out of time.
    fn run(&self, report: &mut dyn FnMut(wt_sys::walk::Progress)) -> (Vec<CheckoutObs>, bool) {
        let Some(deadlines) = self.deadlines else {
            return (Vec::new(), false);
        };
        let mut truncated = false;
        let mut observe = |progress: wt_sys::walk::Progress| {
            truncated = progress.truncated;
            report(progress);
        };
        let found = wt_sys::walk::discover(
            &self.roots,
            wt_sys::walk::DEFAULT_DEPTH,
            WALK_BUDGET,
            &mut observe,
        )
        .unwrap_or_default();
        (describe(found, &self.known, deadlines), truncated)
    }
}

fn bucket(context: &Context, checkouts: Vec<CheckoutObs>) -> Vec<Bucket> {
    let labels = context
        .registry
        .labels
        .keys()
        .map(ToString::to_string)
        .collect();
    model::bucket(checkouts, &labels)
}

/// What the background thread sends back.
enum Found {
    Progress(wt_sys::walk::Progress),
    Done(Vec<CheckoutObs>, bool),
}

struct Scan {
    events: Receiver<Found>,
    progress: wt_sys::walk::Progress,
    result: Option<(Vec<CheckoutObs>, bool)>,
    /// Whether a result has already been produced. Without this, the closed
    /// channel keeps manufacturing an empty one after the real result was
    /// taken, and the second overwrites the repositories card with nothing.
    delivered: bool,
}

impl Scan {
    /// Starts discovery immediately, so it overlaps the questions that do not
    /// depend on it. On any ordinary machine it is finished before the reader
    /// reaches the repositories card.
    fn start(context: &Context, extra: &[PathBuf]) -> Self {
        let (sender, events) = mpsc::channel();
        let discovery = Discovery::of(context, extra);
        std::thread::spawn(move || {
            let progress = sender.clone();
            let mut report = |progress_now| {
                let _ = progress.send(Found::Progress(progress_now));
            };
            let (checkouts, truncated) = discovery.run(&mut report);
            let _ = sender.send(Found::Done(checkouts, truncated));
        });

        Self {
            events,
            progress: wt_sys::walk::Progress::default(),
            result: None,
            delivered: false,
        }
    }

    /// Takes the discovery result, exactly once.
    fn take(&mut self) -> Option<(Vec<CheckoutObs>, bool)> {
        self.result.take()
    }

    /// Drains whatever the thread has sent. Never blocks.
    fn poll(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(Found::Progress(progress)) => self.progress = progress,
                Ok(Found::Done(checkouts, truncated)) => {
                    self.result = Some((checkouts, truncated));
                    self.delivered = true;
                }
                Err(TryRecvError::Empty) => return,
                // The thread is gone. Only a thread that died before sending
                // leaves nothing to show; one whose result was already taken
                // must not be given a second, empty one.
                Err(TryRecvError::Disconnected) => {
                    if !self.delivered {
                        self.result = Some((Vec::new(), false));
                        self.delivered = true;
                    }
                    return;
                }
            }
        }
    }

    fn line(&self) -> String {
        format!(
            "searching · {} directories · {} repositories",
            self.progress.directories, self.progress.found
        )
    }
}

/// What the registry already knows, captured before the thread starts.
#[derive(Clone)]
struct KnownRegistry {
    /// Canonical checkout path to its label.
    canonical: std::collections::BTreeMap<PathBuf, String>,
    /// Every registered tree path.
    trees: BTreeSet<PathBuf>,
    /// Tree names already live under each label.
    names: std::collections::BTreeMap<String, Vec<String>>,
}

impl KnownRegistry {
    fn of(context: &Context) -> Self {
        let mut names: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for tree in &context.registry.trees {
            names
                .entry(tree.label.to_string())
                .or_default()
                .push(tree.name.clone());
        }
        Self {
            canonical: context
                .registry
                .trees
                .iter()
                .filter(|tree| tree.canonical)
                .map(|tree| (PathBuf::from(tree.path.as_str()), tree.label.to_string()))
                .collect(),
            trees: context
                .registry
                .trees
                .iter()
                .map(|tree| PathBuf::from(tree.path.as_str()))
                .collect(),
            names,
        }
    }
}

/// Turns walk results into the observations the model groups, running one
/// batch of git queries per checkout rather than per candidate.
fn describe(
    found: Vec<wt_sys::walk::Found>,
    known: &KnownRegistry,
    deadlines: wt_sys::git::Deadlines,
) -> Vec<CheckoutObs> {
    // Group by the gitdir a checkout shares with its linked worktrees: that
    // grouping is what turns N candidates into one git query each.
    let mut grouped: std::collections::BTreeMap<PathBuf, Vec<wt_sys::walk::Found>> =
        Default::default();
    for entry in found {
        grouped
            .entry(entry.common_gitdir.clone())
            .or_default()
            .push(entry);
    }

    let groups: Vec<_> = grouped.into_values().collect();
    let mut described = Vec::new();
    // Eight at a time: the queries are latency-bound, and a fleet of forty
    // checkouts should not cost forty round trips in series.
    for chunk in groups.chunks(8) {
        let results: Vec<Option<CheckoutObs>> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|group| scope.spawn(|| describe_one(group, known, deadlines)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap_or(None))
                .collect()
        });
        described.extend(results.into_iter().flatten());
    }
    described
}

fn describe_one(
    group: &[wt_sys::walk::Found],
    known: &KnownRegistry,
    deadlines: wt_sys::git::Deadlines,
) -> Option<CheckoutObs> {
    // Only a real checkout can be registered. Falling back to a linked
    // worktree would offer `register <path>` and `adopt <same path>` for one
    // directory, which happens whenever a worktree is inside the walk and its
    // checkout is not.
    let primary = group
        .iter()
        .find(|entry| entry.kind == model::CandidateKind::Checkout)?;
    let git = wt_sys::git::Git::open(&primary.path, deadlines).ok()?;
    let origin = git.origin_url().ok().flatten();
    let branches: std::collections::BTreeMap<PathBuf, Option<String>> = git
        .worktrees()
        .ok()
        .map(|worktrees| {
            worktrees
                .into_iter()
                .map(|worktree| {
                    (
                        wt_sys::fsx::canonicalize(&worktree.path)
                            .unwrap_or_else(|_| worktree.path.clone()),
                        worktree.branch,
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let worktrees = group
        .iter()
        .filter(|entry| entry.kind == model::CandidateKind::Linked)
        .map(|entry| WorktreeObs {
            path: entry.path.to_string_lossy().into_owned(),
            branch: branches.get(&entry.path).cloned().flatten(),
            touched: entry.touched,
            registered: known.trees.contains(&entry.path),
        })
        .collect();

    let registered = known.canonical.get(&primary.path).cloned();
    let taken_names = registered
        .as_deref()
        .and_then(|label| known.names.get(label))
        .cloned()
        .unwrap_or_default();
    Some(CheckoutObs {
        path: primary.path.to_string_lossy().into_owned(),
        common_gitdir: primary.common_gitdir.to_string_lossy().into_owned(),
        origin,
        touched: primary.touched,
        worktrees,
        registered,
        taken_names,
    })
}

// ---------------------------------------------------------------------- cards

fn cards(context: &Context, environment: &Environment) -> Vec<Card> {
    vec![
        shell_card(environment),
        tmux_card(environment),
        agent_card(context, environment),
        trees_card(environment),
        repos_card(),
    ]
}

fn shell_card(environment: &Environment) -> Card {
    let mut rows = Vec::new();
    for rc in &environment.shells {
        let detected = environment.detected_shell.as_deref() == Some(rc.shell.as_str());
        if !rc.exists && !detected {
            continue;
        }
        let mut row = Row::item(&rc.shell, &rc.shell).with_detail(environment.short(&rc.real));
        if rc.installed {
            row = row.with_note("already installed").disabled();
        } else {
            row = row.selected(detected);
            if rc.real != rc.path {
                row = row.with_note("symlink");
            }
        }
        rows.push(row);
    }
    let mut card = Card::new("shell", "shell", Mode::Multi)
        .with_verb("install")
        .with_rows(rows)
        .with_blurb(
            "adds wt's PATH guard and completions; without it an rc file that reorders PATH \
             silently defeats a worktree's own build",
        );
    if card.rows.iter().all(|row| !row.enabled) {
        card.skipped = true;
    }
    card
}

fn tmux_card(environment: &Environment) -> Card {
    let mut rows = Vec::new();
    let blurb;
    match &environment.tmux {
        TmuxState::Absent { manager: None, .. } => {
            blurb = "tmux is not installed and no package manager was found; wt runs in the \
                     foreground until it is"
                .to_owned();
            rows.push(Row::item("skip", "carry on without tmux").selected(true));
        }
        TmuxState::Absent {
            manager: Some(manager),
            existing,
        } => {
            blurb = "tmux gives each worktree its own session; without it wt runs in the \
                     foreground"
                .to_owned();
            rows.push(
                Row::item("install", "install tmux")
                    .with_detail(manager.install_command("tmux"))
                    .selected(true),
            );
            rows.push(Row::item("skip", "carry on without tmux"));
            if let Some(existing) = existing {
                rows.push(Row::note(format!(
                    "tmux will read your existing {}; re-run `wt setup` afterwards to check it",
                    environment.short(existing)
                )));
            }
        }
        TmuxState::Outdated => {
            blurb = "tmux is installed but older than 3.2, which wt's sessions need; upgrade it \
                     and re-run `wt setup`"
                .to_owned();
            rows.push(Row::item("skip", "carry on without tmux").selected(true));
        }
        TmuxState::Unconfigured { path, .. } => {
            blurb = "tmux is installed but has no configuration".to_owned();
            rows.push(
                Row::item("write", "write a configuration")
                    .with_detail(environment.short(path))
                    .selected(true),
            );
            rows.push(Row::item("skip", "leave tmux unconfigured"));
        }
        TmuxState::Configured {
            real, deltas, read, ..
        } => {
            if deltas.is_empty() {
                blurb = if *read {
                    "your tmux configuration already does everything wt needs".to_owned()
                } else {
                    "your tmux configuration could not be read, so it is left alone".to_owned()
                };
                rows.push(Row::item("skip", "leave it as it is").selected(true));
            } else {
                blurb = format!(
                    "{} option{} differ from what wt needs",
                    deltas.len(),
                    if deltas.len() == 1 { "" } else { "s" }
                );
                rows.push(
                    Row::item("append", "append what is missing")
                        .with_detail(environment.short(real))
                        .selected(true),
                );
                rows.push(Row::item("skip", "leave it as it is"));
                for delta in deltas {
                    rows.push(Row::note(format!(
                        "{} is {} — {}",
                        delta.option, delta.current, delta.consequence
                    )));
                }
            }
        }
    }
    Card::new("tmux", "tmux", Mode::Choice)
        .with_rows(rows)
        .with_blurb(blurb)
}

fn agent_card(context: &Context, environment: &Environment) -> Card {
    let mut card = Card::new("agent", "agent", Mode::Choice).with_blurb(
        "started by `wt open` in a new session; the repository's own checkout always gets a shell",
    );
    if environment.agents.is_empty() {
        card.skipped = true;
        return card;
    }
    let current = context.settings.session.agent.clone();
    let single = environment.agents.len() == 1;
    let mut rows: Vec<Row> = environment
        .agents
        .iter()
        .map(|agent| {
            let chosen =
                current.as_deref() == Some(agent.as_str()) || (single && current.is_none());
            Row::item(agent, agent).selected(chosen)
        })
        .collect();
    rows.push(Row::item("none", "none — just a shell").selected(current.is_none() && !single));
    card.with_rows(rows)
}

fn trees_card(environment: &Environment) -> Card {
    Card::new("trees", "trees dir", Mode::Text)
        .with_value(environment.trees_dir.clone())
        .with_blurb("where linked worktrees are created; awkward to change once trees exist")
}

fn repos_card() -> Card {
    let mut card = Card::new("repos", "repositories", Mode::Multi)
        .with_verb("register")
        .with_blurb("tick the checkouts to register; their worktrees are adopted under them");
    card.pending = true;
    card
}

/// Builds the repository rows once discovery has finished.
///
/// One line per decision, nothing ticked. A checkout already registered is
/// not a decision, so it is counted in the status line rather than listed;
/// its adoptable worktrees still are. A label equal to the directory name is
/// not shown — it is what the reader would assume — and a worktree's row
/// depends on its checkout's, since it can only be adopted under that label
/// (§11.6).
fn fill_repos(card: &mut Card, buckets: &[Bucket], home: &Path, now: u64, truncated: bool) {
    let short = |path: &str| short_path(home, Path::new(path));
    let mut rows = Vec::new();
    if truncated {
        rows.push(Row::note(TRUNCATED));
    }
    let mut first_stale = None;
    let mut found = 0;
    let mut registered = 0;
    for bucket in buckets {
        if bucket.stale(now, model::RECENT_DAYS) && first_stale.is_none() {
            first_stale = Some(rows.len());
        }
        let first_label = bucket
            .checkouts
            .first()
            .map(|checkout| checkout.label.clone());
        for checkout in &bucket.checkouts {
            found += 1;
            if checkout.registered.is_some() {
                registered += 1;
            } else {
                let mut row = Row::item(&checkout.path, short(&checkout.path))
                    .with_detail("as")
                    .with_value(&checkout.label)
                    .selected(checkout.selected);
                if model::basename_of(&checkout.path) == Some(checkout.label.as_str()) {
                    row = row.implicit();
                }
                if checkout.secondary {
                    row = row.with_note(format!(
                        "another checkout of {}",
                        first_label.as_deref().unwrap_or(&bucket.origin)
                    ));
                }
                rows.push(row);
            }
            for worktree in &checkout.worktrees {
                if worktree.registered {
                    continue;
                }
                let mut row = Row::item(&worktree.path, format!("  {}", short(&worktree.path)))
                    .with_detail(format!("adopt into {} as", checkout.label))
                    .with_value(&worktree.name)
                    .selected(worktree.selected);
                if checkout.registered.is_none() {
                    row = row.under(&checkout.path);
                }
                rows.push(row);
            }
        }
    }
    card.pending = false;
    card.rows = rows;
    card.settle_dependents();
    let mut status = format!("{found} found");
    if registered > 0 {
        status.push_str(&format!(" · {registered} already registered"));
    }
    card.status = status;
    // Every checkout already registered is a complete answer, not a question.
    card.skipped = !card.actionable();
    if card.skipped {
        return;
    }
    // Nothing recent at all would collapse the whole list, leaving a card with
    // no visible rows, so a tail that starts at the top is not collapsed.
    if let Some(index) = first_stale.filter(|index| *index > 0) {
        card.collapse_after = Some(index);
    }
    card.settle_cursor();
}

// --------------------------------------------------------------------- driver

/// Runs the interface. Returns true when the reader left without accepting,
/// along with the buckets the repositories card was built from.
fn drive(
    context: &Context,
    environment: &Environment,
    state: &mut tui::State,
    mut scan: Scan,
) -> Result<(bool, Vec<Bucket>), CoreError> {
    let color = !matches!(context_color(context), Coloring::Never);
    // Locale precedence is LC_ALL, then LC_CTYPE, then LANG.
    let unicode = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| context.parent_env.get(*name))
        .map(|value| {
            value.to_ascii_uppercase().contains("UTF-8")
                || value.to_ascii_uppercase().contains("UTF8")
        })
        .unwrap_or(false);
    let mut terminal = wt_sys::term::Terminal::enter()?;
    let now = wt_sys::fsx::epoch_seconds();
    let mut buckets: Vec<Bucket> = Vec::new();

    let quit = loop {
        scan.poll();
        if let Some((checkouts, truncated)) = scan.take() {
            buckets = bucket(context, checkouts);
            if let Some(card) = state.cards.iter_mut().find(|card| card.key == "repos") {
                fill_repos(card, &buckets, &environment.home, now, truncated);
            }
            // The card may have become one with nothing to ask while it was
            // the one on screen.
            state.skip_forward();
        }
        state.scan = state
            .card()
            .filter(|card| card.pending)
            .map(|_| scan.line());

        let committed = state.take_committed();
        terminal.commit(&committed)?;
        if state.finished() {
            break state.quit;
        }
        let viewport = terminal.viewport(color, unicode);
        terminal.draw(&state.view(viewport))?;

        let animating =
            state.scan.is_some() || state.card().is_some_and(|card| card.editing.is_some());
        let wait = if animating { FRAME } else { IDLE };
        match terminal.read_key(wait)? {
            None => state.frame = state.frame.wrapping_add(1),
            Some(key) => match state.update(key) {
                Outcome::Quit => break true,
                Outcome::Accepted => break false,
                Outcome::Continue => {}
            },
        }
    };
    let committed = state.take_committed();
    terminal.clear_region()?;
    terminal.commit(&committed)?;
    terminal.restore();
    Ok((quit, buckets))
}

enum Coloring {
    Never,
    Auto,
}

fn context_color(context: &Context) -> Coloring {
    if context.parent_env.contains_key("NO_COLOR")
        || context
            .parent_env
            .get("TERM")
            .is_some_and(|term| term == "dumb")
    {
        return Coloring::Never;
    }
    Coloring::Auto
}

// ------------------------------------------------------------- plan and apply

/// Turns the answered cards into the ordered step list.
fn compose(
    context: &Context,
    state: &tui::State,
    environment: &Environment,
    buckets: &[Bucket],
) -> Vec<Step> {
    let mut steps = Vec::new();

    for shell in state
        .answer("shell")
        .map(Card::selection)
        .unwrap_or_default()
    {
        if let Some(rc) = environment.shells.iter().find(|rc| rc.shell == shell) {
            steps.push(Step::ShellInit {
                file: rc.real.to_string_lossy().into_owned(),
                shell: shell.clone(),
            });
        }
    }

    if let Some(choice) = state
        .answer("tmux")
        .and_then(|card| card.selection().first().cloned())
    {
        steps.extend(tmux_steps(environment, &choice));
    }

    if let Some(agent) = state
        .answer("agent")
        .and_then(|card| card.selection().first().cloned())
    {
        let current = context.settings.session.agent.as_deref();
        if agent == "none" {
            if current.is_some() {
                steps.push(Step::Settings {
                    key: "session.agent".to_owned(),
                    value: String::new(),
                });
            }
        } else if current != Some(agent.as_str()) {
            steps.push(Step::Settings {
                key: "session.agent".to_owned(),
                value: wt_core::settings::string_literal(&agent),
            });
        }
    }

    if let Some(card) = state.answer("trees") {
        let value = card.value.trim();
        // Compared with the effective value: accepting the built-in default
        // is not a change, and writing it would be a step nobody asked for.
        if !value.is_empty() && environment.trees_dir != value {
            steps.push(Step::Settings {
                key: "trees_dir".to_owned(),
                value: wt_core::settings::string_literal(value),
            });
        }
    }

    match state.answer("repos") {
        Some(card) => model::plan(&chosen(card, buckets), steps),
        None => steps,
    }
}

/// The buckets as the reader answered them: selections and edited proposals
/// written back over the model's defaults, so the plan comes from the model
/// rather than from the rows.
fn chosen(card: &Card, buckets: &[Bucket]) -> Vec<Bucket> {
    let selected: BTreeSet<String> = card.selection().into_iter().collect();
    let mut buckets = buckets.to_vec();
    for bucket in &mut buckets {
        for checkout in &mut bucket.checkouts {
            if checkout.registered.is_none() {
                checkout.selected = selected.contains(&checkout.path);
                if let Some(label) = card
                    .value_of(&checkout.path)
                    .and_then(model::sanitise_label)
                {
                    checkout.label = label;
                }
            }
            for worktree in &mut checkout.worktrees {
                worktree.selected = selected.contains(&worktree.path);
                if let Some(name) = card.value_of(&worktree.path).and_then(model::sanitise_name) {
                    worktree.name = name;
                }
            }
        }
    }
    buckets
}

/// The steps behind one answer to the tmux card.
fn tmux_steps(environment: &Environment, choice: &str) -> Vec<Step> {
    let mut steps = tmux_file_steps(environment, choice);
    // tmux chosen, but sessions declared off: the declaration wins over any
    // later probe, so it is the one thing standing between the reader and
    // the sessions the card promised.
    if !steps.is_empty() && environment.backend_declared_none {
        steps.push(Step::Settings {
            key: "session.backend".to_owned(),
            value: wt_core::settings::string_literal("tmux"),
        });
    }
    steps
}

fn tmux_file_steps(environment: &Environment, choice: &str) -> Vec<Step> {
    match (&environment.tmux, choice) {
        (TmuxState::Absent { manager, existing }, "install") => {
            let mut steps = Vec::new();
            if let Some(manager) = manager {
                steps.push(Step::TmuxInstall {
                    command: manager.install_command("tmux"),
                });
            }
            // Installing tmux and leaving it unconfigured would end the run
            // having produced none of the behaviour the card described, so the
            // configuration belongs to the same answer rather than to a second
            // one the reader would have to know to give. A configuration
            // already on disk is tmux's to read; nothing can be learnt about
            // it until tmux exists, so it is left alone.
            if existing.is_none() {
                steps.push(Step::TmuxConfig {
                    file: environment
                        .home
                        .join(".config/tmux/tmux.conf")
                        .to_string_lossy()
                        .into_owned(),
                    created: true,
                    body: tmuxconf::render(&environment.obs),
                });
            }
            steps
        }
        (TmuxState::Unconfigured { path }, "write") => vec![Step::TmuxConfig {
            file: path.to_string_lossy().into_owned(),
            created: true,
            body: tmuxconf::render(&environment.obs),
        }],
        (TmuxState::Configured { real, deltas, .. }, "append") => vec![Step::TmuxConfig {
            file: real.to_string_lossy().into_owned(),
            created: false,
            body: delta_block(deltas),
        }],
        _ => Vec::new(),
    }
}

/// The guarded block appended to a tmux configuration that already exists.
/// The markers are what a later run reads to tell wt's lines from the
/// reader's own.
fn delta_block(deltas: &[tmuxconf::Delta]) -> String {
    let mut block = format!("{}\n", tmuxconf::BLOCK_OPEN);
    for delta in deltas {
        block.push_str(&format!("{}\n", delta.line));
    }
    block.push_str(&format!("{}\n", tmuxconf::BLOCK_CLOSE));
    block
}

fn apply(
    context: &mut Context,
    steps: &[Step],
    environment: &Environment,
) -> Result<Output, CoreError> {
    let mut reports = Vec::new();
    let mut lines = Vec::new();
    let mut failed_labels: BTreeSet<String> = BTreeSet::new();
    let mut tmux_failed = false;
    let mut failure: Option<CoreError> = None;
    for step in steps {
        // An adopt needs its label, so a register that failed cancels the
        // adopts that depended on it rather than failing again per worktree.
        if let Step::Adopt { label, name, .. } = step {
            if failed_labels.contains(label) {
                lines.push(format!(
                    "  skipped adopting {label}/{name} — {label} was not registered"
                ));
                reports.push(report(
                    step,
                    "skipped",
                    format!("{label} was not registered"),
                ));
                continue;
            }
        }
        // Declaring the tmux backend on a machine where the install failed
        // would send every later `open` to a program that is not there.
        if let Step::Settings { key, .. } = step {
            if key == "session.backend" && tmux_failed {
                lines.push(
                    "  skipped session.backend = \"tmux\" — tmux was not installed".to_owned(),
                );
                reports.push(report(step, "skipped", "tmux was not installed"));
                continue;
            }
        }
        match apply_one(context, step, environment) {
            Ok(detail) => {
                lines.push(format!(
                    "  ok      {}",
                    summarise(step, environment, &detail)
                ));
                reports.push(report(step, "done", detail));
            }
            Err(error) => {
                // The remaining independent steps still run: stopping would
                // leave a half-applied plan the reader was never shown. What
                // does not happen is calling the result a success.
                if let Step::Register { label, .. } = step {
                    failed_labels.insert(label.clone());
                }
                if let Step::TmuxInstall { .. } = step {
                    tmux_failed = true;
                }
                lines.push(format!(
                    "  failed  {} — {}",
                    summarise(step, environment, ""),
                    error.message
                ));
                reports.push(report(step, "failed", error.message.clone()));
                failure.get_or_insert(error);
            }
        }
    }
    let failures = reports
        .iter()
        .filter(|step| step.status == "failed")
        .count();
    let mut text = format!(
        "
{}",
        lines.join(
            "
"
        )
    );
    if failures == 0 {
        text.push_str("\n\nnext: wt ls, then wt new <label>/<branch>");
    } else {
        text.push_str(&format!(
            "\n\n{failures} step{} failed; re-run `wt setup` after fixing them",
            if failures == 1 { "" } else { "s" }
        ));
    }
    let output = Output::text(
        SetupData {
            applied: failures == 0,
            steps: reports,
        },
        text,
    )?;
    // A run that could not do what it was told is not a success, however much
    // of the plan happened to land.
    Ok(match failure {
        Some(error) => output.with_failure(error),
        None => output,
    })
}

/// One step as a sentence, before it runs.
fn sentence(step: &Step, environment: &Environment) -> String {
    let short = |file: &str| environment.short(Path::new(file));
    match step {
        Step::Register { path, label } => format!("register {} as {label}", short(path)),
        Step::Adopt { path, label, name } => format!("adopt {} as {label}/{name}", short(path)),
        Step::Settings { key, value } if value.is_empty() => format!("clear {key}"),
        Step::Settings { key, value } => format!("set {key} = {value}"),
        Step::ShellInit { file, .. } => format!("add the shell guard to {}", short(file)),
        Step::TmuxInstall { command } => format!("run `{command}`"),
        Step::TmuxConfig {
            file,
            created,
            body,
        } => {
            if *created {
                format!("write {}", short(file))
            } else {
                format!("append {} lines to {}", body.lines().count(), short(file))
            }
        }
    }
}

/// One step as a sentence, after it ran.
fn summarise(step: &Step, environment: &Environment, detail: &str) -> String {
    let short = |file: &str| environment.short(Path::new(file));
    let head = match step {
        Step::Register { path, label } => format!("registered {} as {label}", short(path)),
        Step::Adopt { path, label, name } => format!("adopted {} as {label}/{name}", short(path)),
        Step::Settings { key, value } if value.is_empty() => format!("cleared {key}"),
        Step::Settings { key, value } => format!("set {key} = {value}"),
        Step::ShellInit { file, .. } => format!("added the shell guard to {}", short(file)),
        Step::TmuxInstall { .. } => "installed tmux".to_owned(),
        Step::TmuxConfig { file, created, .. } => {
            if *created {
                format!("wrote {}", short(file))
            } else {
                format!("appended to {}", short(file))
            }
        }
    };
    if detail.is_empty() {
        head
    } else {
        format!("{head}  {detail}")
    }
}

fn apply_one(
    context: &mut Context,
    step: &Step,
    environment: &Environment,
) -> Result<String, CoreError> {
    match step {
        Step::Register { path, label } => {
            super::register::run(
                context,
                crate::cli::Register {
                    path: PathBuf::from(path),
                    label: Some(label.clone()),
                    move_to: None,
                    repair: false,
                },
            )?;
            Ok(String::new())
        }
        Step::Adopt { path, label, name } => {
            super::adopt::run(
                context,
                crate::cli::Adopt {
                    path: PathBuf::from(path),
                    label: Some(label.clone()),
                    name: Some(name.clone()),
                    agent: None,
                    meta: Vec::new(),
                },
            )?;
            Ok(String::new())
        }
        Step::Settings { key, value } => {
            let path = context.home.join("config.toml");
            let source = wt_sys::fsx::read_string(&path)?.unwrap_or_default();
            let (table, key) = match key.split_once('.') {
                Some((table, key)) => (Some(table), key),
                None => (None, key.as_str()),
            };
            // `set`, not `declare`: declare keeps an existing value, which
            // for a value the reader chose means reporting success while
            // changing nothing. An empty value clears the key.
            let updated = if value.is_empty() {
                wt_core::settings::unset(&source, table, key)?
            } else {
                wt_core::settings::set(&source, table, key, value)?
            };
            wt_sys::fsx::write_store(&path, updated.as_bytes())?;
            context.settings = wt_core::settings::parse(&updated)?;
            Ok(String::new())
        }
        Step::ShellInit { file, shell } => {
            let path = PathBuf::from(file);
            let block = model::shell_block(shell);
            let backup = wt_sys::fsx::append_with_backup(&path, &block)?;
            Ok(backup.map_or_else(String::new, |backup| {
                format!("(backup {})", environment.short(&backup))
            }))
        }
        Step::TmuxInstall { command } => {
            let mut argv = command.split_whitespace();
            let program = argv.next().ok_or_else(|| {
                CoreError::new(
                    ExitClass::Internal,
                    "CONFIG_INVALID",
                    "the install command is empty",
                    "install tmux by hand",
                )
            })?;
            let program = wt_sys::proc::on_path(program).ok_or_else(|| {
                CoreError::new(
                    ExitClass::State,
                    "TOOL_MISSING",
                    format!("{program} is not on PATH"),
                    "install tmux by hand",
                )
            })?;
            let mut request = wt_sys::proc::CommandRequest::new(program);
            request.args = argv.map(std::ffi::OsString::from).collect();
            eprintln!("running: {command}");
            // The terminal goes to the child whole: a package manager may ask
            // for a password and will redraw its own progress.
            let status = wt_sys::proc::handover(&request)?;
            if status.code == Some(0) {
                Ok(String::new())
            } else {
                Err(CoreError::new(
                    ExitClass::State,
                    "TASK_FAILED",
                    format!("{command} exited {}", status.code.unwrap_or(-1)),
                    "install tmux by hand, then re-run `wt setup`",
                ))
            }
        }
        Step::TmuxConfig {
            file,
            created,
            body,
        } => {
            let path = PathBuf::from(file);
            if *created {
                // "There is no configuration" was decided before the consent,
                // and a file may have appeared since. Backing it up rather
                // than replacing it keeps the promise that setup never
                // destroys what it did not write.
                return match wt_sys::fsx::path_kind(&path)? {
                    wt_sys::fsx::PathKind::Missing => {
                        wt_sys::fsx::write_store(&path, body.as_bytes())?;
                        Ok(String::new())
                    }
                    _ => Err(CoreError::new(
                        ExitClass::State,
                        "ARTIFACT_KEPT",
                        format!(
                            "{} appeared after the plan was made and was left alone",
                            path.display()
                        ),
                        "re-run `wt setup`, which will offer the differences instead",
                    )),
                };
            }
            wt_sys::fsx::append_with_backup(&path, body)?;
            // The probe, not the append, decides whether it worked: a plugin
            // line at the end of a configuration can overwrite what follows it.
            let remaining = wt_sys::tmux::probe_effective("tmux", Some(&path), PROBE)
                .map(|options| tmuxconf::deltas(&environment.obs, &effective(options)))
                .unwrap_or_default();
            if remaining.is_empty() {
                Ok(String::new())
            } else {
                Ok(format!(
                    "({} option{} still overridden later in the file)",
                    remaining.len(),
                    if remaining.len() == 1 { "" } else { "s" }
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wt_core::tui::Key;

    const DAY: u64 = 86_400;
    const NOW: u64 = 1_000 * DAY;

    fn environment(tmux: TmuxState) -> Environment {
        Environment {
            home: PathBuf::from("/home/u"),
            backend_declared_none: false,
            detected_shell: None,
            shells: Vec::new(),
            tmux,
            obs: TmuxObs {
                term: Some("xterm-ghostty".to_owned()),
                tmux_256color: true,
            },
            agents: Vec::new(),
            trees_dir: String::new(),
        }
    }

    fn brew() -> &'static model::PackageManager {
        model::PACKAGE_MANAGERS
            .iter()
            .find(|manager| manager.program == "brew")
            .unwrap()
    }

    #[test]
    fn installing_tmux_also_configures_it_for_this_terminal() {
        let steps = tmux_steps(
            &environment(TmuxState::Absent {
                manager: Some(brew()),
                existing: None,
            }),
            "install",
        );
        let [Step::TmuxInstall { .. }, Step::TmuxConfig { body, .. }] = steps.as_slice() else {
            panic!("the install is followed by the configuration: {steps:?}");
        };
        // The terminal is observable whether or not tmux is: a configuration
        // written for a generic terminal would be wrong on this machine.
        assert!(body.contains("xterm-ghostty:extkeys"), "{body}");
        assert!(
            body.contains("default-terminal \"tmux-256color\""),
            "{body}"
        );

        let alone = tmux_steps(
            &environment(TmuxState::Absent {
                manager: None,
                existing: None,
            }),
            "install",
        );
        assert!(
            matches!(alone.as_slice(), [Step::TmuxConfig { .. }]),
            "with no package manager the configuration still happens: {alone:?}"
        );
    }

    #[test]
    fn choosing_tmux_re_declares_a_backend_an_earlier_run_switched_off() {
        let mut environment = environment(TmuxState::Unconfigured {
            path: PathBuf::from("/home/u/.config/tmux/tmux.conf"),
        });
        environment.backend_declared_none = true;
        let steps = tmux_steps(&environment, "write");
        assert!(
            matches!(
                steps.last(),
                Some(Step::Settings { key, value }) if key == "session.backend" && value == "\"tmux\""
            ),
            "{steps:?}"
        );
        assert!(
            tmux_steps(&environment, "skip").is_empty(),
            "declining tmux declares nothing"
        );
        environment.backend_declared_none = false;
        assert!(
            !tmux_steps(&environment, "write")
                .iter()
                .any(|step| matches!(step, Step::Settings { .. })),
            "an undeclared backend is resolved by register, not by setup"
        );
    }

    #[test]
    fn an_existing_config_is_left_for_the_installed_tmux_to_read() {
        // tmux reads ~/.tmux.conf before the XDG file, so writing the latter
        // would produce a file that is never read.
        let steps = tmux_steps(
            &environment(TmuxState::Absent {
                manager: Some(brew()),
                existing: Some(PathBuf::from("/home/u/.tmux.conf")),
            }),
            "install",
        );
        assert!(
            matches!(steps.as_slice(), [Step::TmuxInstall { .. }]),
            "{steps:?}"
        );
        let card = tmux_card(&environment(TmuxState::Absent {
            manager: Some(brew()),
            existing: Some(PathBuf::from("/home/u/.tmux.conf")),
        }));
        assert!(card
            .rows
            .iter()
            .any(|row| row.text.contains("~/.tmux.conf")));
    }

    #[test]
    fn an_outdated_tmux_is_reported_and_nothing_is_run() {
        let environment = environment(TmuxState::Outdated);
        let card = tmux_card(&environment);
        assert!(!card.skipped, "the reader is told rather than passed over");
        assert!(card.blurb.contains("3.2"));
        assert!(tmux_steps(&environment, "skip").is_empty());
    }

    #[test]
    fn a_planned_tmux_config_prints_the_text_it_writes() {
        let steps = tmux_steps(
            &environment(TmuxState::Absent {
                manager: None,
                existing: None,
            }),
            "install",
        );
        let Some(Step::TmuxConfig { body, .. }) = steps.first() else {
            panic!("expected a configuration step: {steps:?}");
        };
        assert!(
            steps[0].command().contains(body.as_str()),
            "the printed command carries the body verbatim"
        );
    }

    fn checkout(path: &str, origin: &str, touched: u64) -> CheckoutObs {
        CheckoutObs {
            path: path.to_owned(),
            common_gitdir: format!("{path}/.git"),
            origin: Some(origin.to_owned()),
            touched,
            worktrees: Vec::new(),
            registered: None,
            taken_names: Vec::new(),
        }
    }

    fn worktree(path: &str, branch: &str, touched: u64) -> WorktreeObs {
        WorktreeObs {
            path: path.to_owned(),
            branch: Some(branch.to_owned()),
            touched,
            registered: false,
        }
    }

    fn buckets(checkouts: Vec<CheckoutObs>) -> Vec<Bucket> {
        model::bucket(checkouts, &BTreeSet::new())
    }

    fn filled(buckets: &[Bucket]) -> Card {
        let mut card = repos_card();
        fill_repos(&mut card, buckets, Path::new("/home/u"), NOW, false);
        card
    }

    #[test]
    fn a_refilled_card_recovers_from_an_earlier_empty_fill() {
        // The scan reports once, but a closed channel used to manufacture a
        // second, empty result. A card disabled by that must come back when a
        // real result arrives, or the repositories never appear at all.
        let mut card = repos_card();
        fill_repos(&mut card, &[], Path::new("/home/u"), NOW, false);
        assert!(card.skipped, "nothing found is nothing to ask about");

        fill_repos(
            &mut card,
            &buckets(vec![checkout("/src/api", "git@h:o/api.git", NOW)]),
            Path::new("/home/u"),
            NOW,
            false,
        );
        assert!(!card.skipped, "a later real result must re-enable the card");
        assert!(!card.pending);
        assert!(card.rows.iter().any(|row| row.id == "/src/api"));
        assert!(card.selection().is_empty(), "registering is opt in");
    }

    #[test]
    fn a_scan_that_finds_nothing_leaves_no_card_to_answer() {
        let card = filled(&buckets(Vec::new()));
        assert!(card.skipped);
        assert!(!card.pending, "a finished scan is never still pending");
    }

    #[test]
    fn a_label_equal_to_the_directory_name_is_implicit() {
        let card = filled(&buckets(vec![
            checkout("/src/api", "git@h:acme/api.git", NOW),
            checkout("/oss/api", "git@h:acme/api.git", NOW - 1),
        ]));
        let first = card.rows.iter().find(|row| row.id == "/src/api").unwrap();
        assert!(
            first.implicit,
            "`api` for ~/src/api is what the reader assumes"
        );
        let second = card.rows.iter().find(|row| row.id == "/oss/api").unwrap();
        assert!(!second.implicit, "`acme-api` is worth showing");
        assert_eq!(second.note, "another checkout of api");
        assert!(
            !card.rows.iter().any(|row| row.kind == tui::RowKind::Header),
            "no origin headers: one line per decision"
        );
    }

    #[test]
    fn the_plan_reads_as_sentences_with_short_paths() {
        let environment = environment(TmuxState::Outdated);
        let register = Step::Register {
            path: "/home/u/src/api".to_owned(),
            label: "api".to_owned(),
        };
        assert_eq!(
            sentence(&register, &environment),
            "register ~/src/api as api"
        );
        let shell = Step::ShellInit {
            file: "/home/u/.zshrc".to_owned(),
            shell: "zsh".to_owned(),
        };
        assert_eq!(
            sentence(&shell, &environment),
            "add the shell guard to ~/.zshrc"
        );
        assert!(
            !sentence(&shell, &environment).contains("cat"),
            "the shell line is for --dry-run, not for a person"
        );
    }

    #[test]
    fn a_scan_that_ran_out_of_time_says_so_on_the_card() {
        let mut card = repos_card();
        fill_repos(
            &mut card,
            &buckets(vec![checkout("/src/api", "git@h:o/api.git", NOW)]),
            Path::new("/home/u"),
            NOW,
            true,
        );
        assert!(
            card.rows[0].text.contains("ran out of time"),
            "{:?}",
            card.rows[0]
        );
    }

    #[test]
    fn a_stale_tail_collapses_but_never_the_whole_list() {
        let mut checkouts = vec![checkout("/src/fresh", "git@h:o/fresh.git", NOW)];
        for index in 0..4 {
            checkouts.push(checkout(
                &format!("/src/old{index}"),
                &format!("git@h:o/old{index}.git"),
                NOW - 90 * DAY,
            ));
        }
        let card = filled(&buckets(checkouts));
        assert!(card.collapse_after.is_some());
        assert!(card.visible_rows().len() < card.rows.len());

        // Everything stale would otherwise collapse to an empty card.
        let all_stale = filled(&buckets(vec![checkout(
            "/src/old",
            "git@h:o/old.git",
            NOW - 90 * DAY,
        )]));
        assert_eq!(all_stale.collapse_after, None);
        assert!(!all_stale.visible_rows().is_empty());
    }

    #[test]
    fn a_worktree_follows_its_checkout_on_the_same_card() {
        let mut observed = checkout("/src/api", "git@h:o/api.git", NOW);
        observed.worktrees = vec![worktree("/t/one", "one", NOW)];
        let buckets = buckets(vec![observed]);
        let card = filled(&buckets);
        assert!(
            card.selection().is_empty(),
            "nothing is ticked on the reader's behalf"
        );

        // The worktree cannot be ticked until its checkout is, and says so.
        let mut state = tui::State::new(vec![card]);
        let rendered = state.view(tui::Viewport::default()).join("\n");
        assert!(
            rendered.contains("its checkout is not selected"),
            "{rendered}"
        );
        assert!(rendered.contains("enter skip"), "{rendered}");
        state.update(Key::Space);
        state.update(Key::Down);
        state.update(Key::Space);
        let card = state.answer("repos").unwrap();
        assert_eq!(
            card.selection(),
            vec!["/src/api".to_owned(), "/t/one".to_owned()]
        );
        let rendered = state.view(tui::Viewport::default()).join("\n");
        assert!(rendered.contains("enter register 2"), "{rendered}");
        assert_eq!(card.summary(), "repositories   api, one");
    }

    #[test]
    fn the_plan_comes_from_the_model_with_edits_written_back() {
        let mut observed = checkout("/src/api", "git@h:o/api.git", NOW);
        observed.worktrees = vec![worktree("/t/one", "fix/scroll", NOW)];
        let buckets = buckets(vec![observed]);
        let mut card = filled(&buckets);
        card.set_all(true);
        // The reader renamed both proposals, one of them into something a
        // label cannot be spelled as.
        card.rows
            .iter_mut()
            .find(|row| row.id == "/src/api")
            .unwrap()
            .value = "Acme API".to_owned();
        card.rows
            .iter_mut()
            .find(|row| row.id == "/t/one")
            .unwrap()
            .value = "scrolling".to_owned();

        let steps = model::plan(&chosen(&card, &buckets), Vec::new());
        assert_eq!(
            steps,
            vec![
                Step::Register {
                    path: "/src/api".to_owned(),
                    label: "acme-api".to_owned()
                },
                Step::Adopt {
                    path: "/t/one".to_owned(),
                    label: "acme-api".to_owned(),
                    name: "scrolling".to_owned()
                },
            ],
            "a register precedes the adopt that needs its label, under the edited label"
        );
    }

    #[test]
    fn an_already_registered_checkout_is_shown_and_its_worktrees_are_still_offered() {
        let mut observed = checkout("/src/api", "git@h:o/api.git", NOW);
        observed.registered = Some("api".to_owned());
        observed.worktrees = vec![worktree("/t/one", "one", NOW)];
        let buckets = model::bucket(vec![observed], &BTreeSet::from(["api".to_owned()]));
        let card = filled(&buckets);
        assert!(
            !card.rows.iter().any(|row| row.id == "/src/api"),
            "a registered checkout is not a decision, so it is not a row"
        );
        assert_eq!(card.status, "1 found · 1 already registered");
        // The label exists, so the worktree depends on nothing and is the
        // whole point of a second run.
        assert!(!card.skipped);
        let row = card.rows.iter().find(|row| row.id == "/t/one").unwrap();
        assert_eq!(row.parent, None);
        assert_eq!(row.detail, "adopt into api as");
        let mut card = card;
        card.set_all(true);
        let steps = model::plan(&chosen(&card, &buckets), Vec::new());
        assert_eq!(
            steps,
            vec![Step::Adopt {
                path: "/t/one".to_owned(),
                label: "api".to_owned(),
                name: "one".to_owned()
            }]
        );
    }
}
