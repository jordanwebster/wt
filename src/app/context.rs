use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use wt_core::address::LiveTree;
use wt_core::config::{self, Config, Layer};
use wt_core::lifecycle::{DerivedPhase, StateView, TreeObs, TreeState};
use wt_core::model::{Registry, Target, Tombstone, TreeIdentity, TreeRec};
use wt_core::report::Notice;
use wt_core::settings::{self, Settings};
use wt_core::task::{Node, Origin};
use wt_core::{CoreError, ExitClass};
use wt_sys::fsx::{self, PathKind};
use wt_sys::git::{Deadlines as GitDeadlines, Git};
use wt_sys::lock::{self, Holder};

use crate::cli::Cli;

pub(crate) struct Context {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub registry: Registry,
    pub settings: Settings,
    pub parent_env: BTreeMap<String, String>,
    pub json: bool,
    pub yes: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub tty: wt_sys::snapshot::Tty,
    pub pending_notices: Vec<Notice>,
}

impl Context {
    pub fn open(cli: &Cli) -> Result<Self, CoreError> {
        let parent_env = wt_sys::snapshot::capture_env();
        let home = cli
            .home
            .clone()
            .or_else(|| parent_env.get("WT_HOME").map(PathBuf::from))
            .or_else(|| {
                parent_env
                    .get("HOME")
                    .map(|home| PathBuf::from(home).join(".wt"))
            })
            .ok_or_else(|| {
                CoreError::new(
                    ExitClass::State,
                    "HOME_UNRESOLVED",
                    "WT_HOME could not be resolved",
                    "pass `--home DIR`, set WT_HOME, or set HOME",
                )
            })?;
        Self::check_format(&home)?;
        let settings = fsx::read_string(&home.join("config.toml"))?
            .map(|source| settings::parse(&source))
            .transpose()?
            .unwrap_or_default();
        wt_sys::trace::open(&home, cli.command.name(), settings.logs.trace);
        let registry = Self::read_registry_at(&home)?;
        let cwd = fsx::canonicalize(&std::env::current_dir().map_err(|error| {
            CoreError::new(
                ExitClass::State,
                "CWD_MISSING",
                format!("cannot read the current directory: {error}"),
                "change to an existing directory and retry",
            )
        })?)?;
        Ok(Self {
            home,
            cwd,
            registry,
            settings,
            parent_env,
            json: cli.json,
            yes: cli.yes,
            quiet: cli.quiet,
            verbose: cli.verbose,
            tty: wt_sys::snapshot::tty(),
            pending_notices: Vec::new(),
        })
    }

    fn check_format(home: &Path) -> Result<(), CoreError> {
        let json = !matches!(
            fsx::path_kind(&home.join("registry.json"))?,
            PathKind::Missing
        );
        let old = ["registry.toml", "state.toml"].iter().any(|name| {
            !matches!(
                fsx::path_kind(&home.join(name)).unwrap_or(PathKind::Missing),
                PathKind::Missing
            )
        });
        if old && json {
            return Err(CoreError::new(
                ExitClass::State,
                "HOME_MIXED",
                "WT_HOME contains both old and current formats",
                "move or delete WT_HOME, then re-register",
            ));
        }
        if old {
            return Err(CoreError::new(
                ExitClass::State,
                "HOME_OLD_FORMAT",
                "WT_HOME contains the unsupported old format",
                "move or delete WT_HOME, then re-register",
            ));
        }
        Ok(())
    }

    fn read_registry_at(home: &Path) -> Result<Registry, CoreError> {
        fsx::trace_budget("registry_read", Some(&home.join("registry.json")))?;
        let registry = fsx::read_json::<Registry>(&home.join("registry.json"), "REGISTRY_CORRUPT")?
            .unwrap_or_else(|| Registry {
                schema: 1,
                labels: BTreeMap::new(),
                trees: Vec::new(),
                tombstones: Vec::new(),
            });
        registry.validate()?;
        Ok(registry)
    }

    pub fn reload_registry(&mut self) -> Result<(), CoreError> {
        self.registry = Self::read_registry_at(&self.home)?;
        Ok(())
    }

    pub fn holder(&self, target: impl Into<String>, verb: &str) -> Result<Holder, CoreError> {
        Ok(Holder::current(target, verb, fsx::timestamp()?))
    }

    pub fn rmw_wait(&self) -> Duration {
        duration(self.settings.locks.rmw.as_deref(), Duration::from_secs(5))
    }

    pub fn tree_wait(&self, override_value: Option<&str>) -> Duration {
        duration(
            override_value.or(self.settings.locks.tree_exclusive.as_deref()),
            Duration::from_secs(30),
        )
    }

    pub fn mutate_registry<T>(
        &mut self,
        holder: &Holder,
        mutate: impl FnOnce(&mut Registry) -> Result<T, CoreError>,
    ) -> Result<T, CoreError> {
        let _lock = lock::registry_rmw(&self.home.join("registry.lock"), holder, self.rmw_wait())?;
        self.registry = Self::read_registry_at(&self.home)?;
        let output = mutate(&mut self.registry)?;
        self.registry.validate()?;
        fsx::write_json(&self.home.join("registry.json"), &self.registry)?;
        Ok(output)
    }

    pub fn state_path(&self, target: &Target) -> PathBuf {
        self.home.join(wt_core::model::tree_state_path(target))
    }

    pub fn state_lock_path(&self, target: &Target) -> PathBuf {
        self.home
            .join(format!("locks/{}/{}.rmw.lock", target.label, target.name))
    }

    pub fn tree_lock_path(&self, target: &Target) -> PathBuf {
        self.home
            .join(format!("locks/{}/{}.lock", target.label, target.name))
    }

    pub fn door_lock_path(&self, target: &Target) -> PathBuf {
        self.home.join(format!(
            "locks/{}/{}.doors/{}.lock",
            target.label,
            target.name,
            std::process::id()
        ))
    }

    pub fn git_lock_path(&self, gitdir_id: &str) -> PathBuf {
        self.home.join(format!("locks/git/{gitdir_id}.lock"))
    }

    /// Root under which adapters key per-tree build caches.
    pub fn cache_root(&self) -> PathBuf {
        self.home.join("cache")
    }

    /// The per-tree build cache directory contributed by the Rust adapter
    /// (`cache/cargo-build/<label>/<name_short>`). wt owns this layout so
    /// that removal and prune can reap what the adapter's env value creates;
    /// a repo or user override that moves the cache elsewhere also takes
    /// over its lifecycle.
    pub fn tree_cache_dir(&self, label: &str, name_short: &str) -> PathBuf {
        self.cache_root()
            .join("cargo-build")
            .join(label)
            .join(name_short)
    }

    pub fn read_state(&self, target: &Target) -> Result<Option<TreeState>, CoreError> {
        fsx::trace_budget("state_read", Some(&self.state_path(target)))?;
        fsx::read_json(&self.state_path(target), "STATE_CORRUPT")
    }

    pub fn write_state(
        &self,
        target: &Target,
        state: &TreeState,
        holder: &Holder,
    ) -> Result<(), CoreError> {
        let _lock = lock::state_rmw(&self.state_lock_path(target), holder, self.rmw_wait())?;
        fsx::write_json(&self.state_path(target), state)
    }

    pub fn mutate_state<T>(
        &self,
        target: &Target,
        holder: &Holder,
        mutate: impl FnOnce(&mut TreeState) -> Result<T, CoreError>,
    ) -> Result<T, CoreError> {
        let _lock = lock::state_rmw(&self.state_lock_path(target), holder, self.rmw_wait())?;
        let mut state = fsx::read_json::<TreeState>(&self.state_path(target), "STATE_CORRUPT")?
            .ok_or_else(|| state_missing(target))?;
        let output = mutate(&mut state)?;
        fsx::write_json(&self.state_path(target), &state)?;
        Ok(output)
    }

    pub fn resolve(&self, address: Option<&str>) -> Result<Target, CoreError> {
        let trees = self
            .registry
            .trees
            .iter()
            .map(|tree| LiveTree {
                target: Target {
                    label: tree.label.clone(),
                    name: tree.name.clone(),
                },
                root: tree.path.as_str().to_owned(),
            })
            .collect::<Vec<_>>();
        let labels = self
            .registry
            .labels
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        wt_core::address::resolve(
            address.unwrap_or("."),
            &self.cwd.to_string_lossy(),
            &trees,
            &labels,
        )
    }

    pub fn tree(&self, target: &Target) -> Result<TreeRec, CoreError> {
        self.registry
            .trees
            .iter()
            .find(|tree| tree.label == target.label && tree.name == target.name)
            .cloned()
            .ok_or_else(|| not_found(target))
    }

    pub fn identity_ok(&self, tree: &TreeRec) -> Result<bool, CoreError> {
        let path = Path::new(tree.path.as_str()).join(".wt/tree_id");
        fsx::trace_budget("identity_read", Some(&path))?;
        Ok(fsx::read_string(&path)?.as_deref() == Some(&format!("{}\n", tree.tree_id)))
    }

    pub fn require_identity(&self, tree: &TreeRec) -> Result<(), CoreError> {
        if self.identity_ok(tree)? {
            Ok(())
        } else {
            Err(CoreError::new(
                ExitClass::State,
                "TREE_REPLACED",
                format!(
                    "{} no longer carries its registered identity",
                    tree.path.as_str()
                ),
                // A canonical tree refuses both `remove` (USE_UNREGISTER) and
                // `adopt` (the name is reserved), so offering them here would
                // send the reader to two dead ends; `register --repair` is the
                // only route back for one. SPEC §11.6.
                if tree.canonical {
                    format!(
                        "run `wt register {} --label {} --repair`",
                        tree.path.as_str(),
                        tree.label.as_str()
                    )
                } else {
                    format!(
                        "run `wt prune --records {0}`, `wt remove {0}`, or `wt adopt {1}`",
                        target_of(tree),
                        tree.path.as_str()
                    )
                },
            ))
        }
    }

    pub fn phase(
        &self,
        tree: &TreeRec,
        state: Option<&TreeState>,
    ) -> Result<DerivedPhase, CoreError> {
        let dir_exists = matches!(
            fsx::path_kind(Path::new(tree.path.as_str()))?,
            PathKind::Directory
        );
        let identity_ok = dir_exists && self.identity_ok(tree)?;
        let state_view = state.map(|state| StateView {
            phase: state.phase,
            op: state.op.as_ref().map(|operation| operation.verb),
            verify_pending: state.verify_pending,
            has_records: !state.resources.is_empty(),
        });
        let consult_lock = state_view.as_ref().is_some_and(|state| {
            !matches!(
                state.phase,
                wt_core::lifecycle::StatePhase::Ready | wt_core::lifecycle::StatePhase::Failed
            )
        });
        let lock_held = consult_lock && lock::is_held(&self.tree_lock_path(&target_of(tree)))?;
        Ok(wt_core::derive_phase(&TreeObs {
            entry: true,
            dir_exists,
            identity_ok,
            state: state_view,
            lock_held,
            git_knows: true,
        })
        .phase)
    }

    pub fn git(&self, path: &Path) -> Result<Git, CoreError> {
        Git::open(
            path,
            GitDeadlines::from_settings(&self.settings.git.timeouts)?,
        )
    }

    pub fn tree_identity(&self, tree: &TreeRec) -> Result<TreeIdentity, CoreError> {
        let label = &self.registry.labels[&tree.label];
        let branch = self
            .git(Path::new(tree.path.as_str()))?
            .head_branch(Path::new(tree.path.as_str()))?;
        Ok(TreeIdentity {
            tree_id: tree.tree_id.clone(),
            label: tree.label.clone(),
            name: tree.name.clone(),
            canonical: tree.canonical,
            root: tree.path.as_str().to_owned(),
            repo: label.path.as_str().to_owned(),
            branch,
            slot: tree.slot,
            geometry: tree.geometry,
            ports: tree.ports.clone(),
            name_short: tree.name_short(),
        })
    }

    pub fn load_config(&self, tree: &TreeRec) -> Result<Config, CoreError> {
        let repo_path = Path::new(tree.path.as_str()).join(".wt.toml");
        let repo = fsx::read_string(&repo_path)?
            .map(|source| config::parse(&source, &repo_path.to_string_lossy()))
            .transpose()?
            .unwrap_or_default();
        let user = self
            .settings
            .repos
            .get(&tree.label)
            .cloned()
            .unwrap_or_default();
        let overlay_path = Path::new(tree.path.as_str()).join(".wt/config.toml");
        let overlay = fsx::read_string(&overlay_path)?
            .map(|source| config::parse(&source, &overlay_path.to_string_lossy()))
            .transpose()?
            .unwrap_or_default();
        let preliminary = config::merge(&[
            (Layer::Repo, repo.clone()),
            (Layer::User, user.clone()),
            (Layer::Tree, overlay.clone()),
        ]);
        let mut adapter = Config::default();
        let mut hits = Vec::new();
        for scope in std::iter::once(".").chain(preliminary.dirs.keys().map(String::as_str)) {
            let relative = wt_core::model::RelPath::new(scope)?;
            let snapshot = fsx::capture_dir_snapshot(
                Path::new(tree.path.as_str()),
                &relative,
                &[
                    "package.json".to_owned(),
                    "rustfmt.toml".to_owned(),
                    ".rustfmt.toml".to_owned(),
                ],
            )?;
            let effective = config::effective_scope(&preliminary, scope)?;
            hits.extend(wt_core::adapters::detect(&snapshot, &effective.adapters)?);
        }
        wt_core::adapters::apply_contribution(
            &mut adapter,
            &wt_core::adapters::contribution(&hits)?,
        )?;
        let merged = config::merge(&[
            (Layer::Adapter, adapter),
            (Layer::Repo, repo),
            (Layer::User, user),
            (Layer::Tree, overlay),
        ]);
        config::validate_resolved(&merged, tree.geometry.stride)?;
        Ok(merged)
    }

    pub fn task_catalog(
        &self,
        tree: &TreeRec,
        config: &Config,
    ) -> Result<BTreeMap<String, Node>, CoreError> {
        let mut hits = Vec::new();
        let mut scripts = BTreeMap::new();
        for scope in std::iter::once(".").chain(config.dirs.keys().map(String::as_str)) {
            let relative = wt_core::model::RelPath::new(scope)?;
            let snapshot = fsx::capture_dir_snapshot(
                Path::new(tree.path.as_str()),
                &relative,
                &[
                    "package.json".to_owned(),
                    "rustfmt.toml".to_owned(),
                    ".rustfmt.toml".to_owned(),
                ],
            )?;
            let effective = config::effective_scope(config, scope)?;
            hits.extend(wt_core::adapters::detect(&snapshot, &effective.adapters)?);
            if let Some(package) = snapshot.contents.get("package.json") {
                let declared = serde_json::from_str::<serde_json::Value>(package)
                    .ok()
                    .and_then(|value| value.get("scripts").cloned())
                    .and_then(|value| value.as_object().cloned())
                    .map(|values| values.keys().cloned().collect())
                    .unwrap_or_default();
                scripts.insert(scope.to_owned(), declared);
            }
        }
        let mut tasks = BTreeMap::new();
        for (scope, values) in std::iter::once((".", &config.root.task)).chain(
            config
                .dirs
                .iter()
                .map(|(scope, values)| (scope.as_str(), &values.task)),
        ) {
            for (id, task) in values {
                if let Some(task) = task.value() {
                    tasks.insert((scope.to_owned(), id.clone()), (Origin::Repo, task.clone()));
                }
            }
        }
        wt_core::adapters::compose(&hits, &tasks, &scripts)
    }

    pub fn confirm(&self, action: &str) -> Result<bool, CoreError> {
        if self.yes {
            return Ok(true);
        }
        if !self.tty.stdin {
            return Err(CoreError::new(
                ExitClass::Usage,
                "CONFIRM_REQUIRED",
                format!("{action} requires confirmation"),
                "re-run with --yes after reviewing the plan",
            ));
        }
        self.ask(None, &format!("{action}?"))
    }

    /// Consent for an operation that destroys work. `--yes` does not answer it:
    /// it is a global flag that lands in aliases and agent command lines where
    /// this tree was never examined, and only `--force` says otherwise (A54).
    pub fn confirm_loss(&self, plan: &str, question: &str) -> Result<bool, CoreError> {
        self.ask(Some(plan), question)
    }

    fn ask(&self, plan: Option<&str>, question: &str) -> Result<bool, CoreError> {
        if let Some(plan) = plan {
            eprintln!("{plan}");
        }
        eprint!("{question} [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).map_err(|error| {
            CoreError::new(
                ExitClass::Internal,
                "INPUT_FAILED",
                error.to_string(),
                "retry with --force",
            )
        })?;
        Ok(answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes"))
    }

    pub fn allocate(
        &self,
        target: &Target,
        configured_ports: &[wt_core::model::PortName],
    ) -> Result<wt_core::coords::Coordinates, CoreError> {
        let allocated_ranges = self
            .registry
            .trees
            .iter()
            .map(|tree| {
                let first = u32::from(tree.geometry.port_base);
                (first, first + u32::from(tree.geometry.stride) - 1)
            })
            .chain(self.registry.tombstones.iter().map(|tree| {
                let first = u32::from(tree.geometry.port_base);
                (first, first + u32::from(tree.geometry.stride) - 1)
            }))
            .collect::<Vec<_>>();
        let allocated_slots = self
            .registry
            .trees
            .iter()
            .map(|tree| tree.slot)
            .chain(self.registry.tombstones.iter().map(|tree| tree.slot))
            .collect::<BTreeSet<_>>();
        let taken_name_shorts = self
            .registry
            .trees
            .iter()
            .map(TreeRec::name_short)
            .chain(self.registry.tombstones.iter().map(Tombstone::name_short))
            .collect::<BTreeSet<_>>();
        let taken_sessions = self
            .registry
            .trees
            .iter()
            .map(TreeRec::session_name)
            .chain(self.registry.tombstones.iter().map(Tombstone::session_name))
            .collect::<BTreeSet<_>>();
        let tombstone = self
            .registry
            .tombstones
            .iter()
            .find(|record| record.label == target.label && record.name == target.name)
            .map(|record| wt_core::coords::TombstoneCoordinates {
                target: target.clone(),
                coordinates: wt_core::coords::Coordinates {
                    slot: record.slot,
                    geometry: record.geometry,
                    ports: record.ports.clone(),
                },
            });
        let initial_ports = wt_core::ports::append(
            &tombstone
                .as_ref()
                .map(|record| record.coordinates.ports.clone())
                .unwrap_or_default(),
            configured_ports,
            tombstone
                .as_ref()
                .map_or(self.settings.ports.stride, |record| {
                    record.coordinates.geometry.stride
                }),
        )?
        .ports;
        let mut squatted = BTreeSet::new();
        if tombstone.is_none() {
            for slot in 0..self.settings.ports.max_slots()? {
                if allocated_slots.contains(&slot) {
                    continue;
                }
                let geometry = self.settings.ports.geometry(slot)?;
                if (0..geometry.stride).any(|offset| {
                    wt_sys::net::squat_probe(
                        geometry.port_base + u16::from(offset),
                        Duration::from_millis(50),
                    )
                    .unwrap_or(true)
                }) {
                    squatted.insert(slot);
                } else {
                    break;
                }
            }
        }
        Ok(wt_core::coords::choose(wt_core::coords::ChooseInput {
            target,
            allocated_ranges: &allocated_ranges,
            allocated_slots: &allocated_slots,
            squatted: &squatted,
            settings: self.settings.ports,
            tombstone: tombstone.as_ref(),
            ports: initial_ports,
            taken_name_shorts: &taken_name_shorts,
            taken_sessions: &taken_sessions,
        })?
        .coordinates)
    }
}

pub(crate) fn target_of(tree: &TreeRec) -> Target {
    Target {
        label: tree.label.clone(),
        name: tree.name.clone(),
    }
}

pub(crate) fn duration(value: Option<&str>, default: Duration) -> Duration {
    value
        .and_then(wt_core::model::duration_millis)
        .map(Duration::from_millis)
        .unwrap_or(default)
}

pub(crate) fn not_found(target: &Target) -> CoreError {
    CoreError::new(
        ExitClass::NotFound,
        "NOT_FOUND",
        format!("target `{target}` was not found"),
        "run `wt list` to list live targets",
    )
}

fn state_missing(target: &Target) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "STATE_CORRUPT",
        format!("state for `{target}` is missing"),
        "run `wt doctor` and restore or prune the affected record",
    )
}
