use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wt_core::config::TiedTo;
use wt_core::model::{duration_millis, EnvMap};
use wt_core::resource::{ExpandedCommand, Probe, ResourceSnapshot};
use wt_core::settings::TaskDefaults;
use wt_core::snapshot::{missing_tree_verdict, MissingTreeVerdict};
use wt_core::{CoreError, ExitClass};

use crate::proc::{self, CommandRequest, ProcessOutput, Tee};
use crate::Result;

pub use wt_core::snapshot::minimise_env;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Exists,
    Destroy,
    Run,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadlines {
    pub probe: Duration,
    pub destroy: Duration,
    pub run: Option<Duration>,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self::from_settings(&TaskDefaults::default())
            .expect("wt-core's built-in task deadlines are valid")
    }
}

impl Deadlines {
    pub fn from_settings(settings: &TaskDefaults) -> Result<Self> {
        Ok(Self {
            probe: resolved_duration(settings.probe_timeout.as_deref(), "task.probe_timeout")?,
            destroy: resolved_duration(
                settings.destroy_timeout.as_deref(),
                "task.destroy_timeout",
            )?,
            run: settings
                .timeout
                .as_deref()
                .map(|value| resolved_duration(Some(value), "task.timeout"))
                .transpose()?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecuteResult {
    Probe(Probe),
    Child(ProcessOutput),
    Orphaned {
        reason: String,
        remedy: String,
        recipe: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tty {
    pub stdin: bool,
    pub stdout: bool,
    pub stderr: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ExecutionOptions<'a> {
    pub tree_replaced: bool,
    pub deadlines: Deadlines,
    pub log: Option<&'a Path>,
    pub tee: Tee,
}

/// Executes one frozen snapshot recipe using the invoker environment as its base.
pub fn execute(
    snapshot: &ResourceSnapshot,
    action: Action,
    invoker_env: &EnvMap,
    current_repo_root: &Path,
    log: Option<&Path>,
    tee: Tee,
) -> Result<ExecuteResult> {
    execute_observed(
        snapshot,
        action,
        invoker_env,
        current_repo_root,
        ExecutionOptions {
            tree_replaced: false,
            deadlines: Deadlines::default(),
            log,
            tee,
        },
    )
}

/// Executes with caller observations that cannot be derived from the snapshot itself.
pub fn execute_observed(
    snapshot: &ResourceSnapshot,
    action: Action,
    invoker_env: &EnvMap,
    current_repo_root: &Path,
    options: ExecutionOptions<'_>,
) -> Result<ExecuteResult> {
    let command = command_for(snapshot, action)?;
    let recipe = recipe_text(command);
    let tree_root = Path::new(&snapshot.roots.tree);
    let tree_missing = options.tree_replaced || !tree_root.is_dir();
    let bin_missing = snapshot
        .bin_dirs
        .iter()
        .any(|directory| !Path::new(directory).is_dir());
    if (tree_missing || bin_missing)
        && matches!(
            missing_tree_verdict(command, &snapshot.bin_exes),
            MissingTreeVerdict::OrphanedExeMissing { .. }
        )
    {
        return Ok(ExecuteResult::Orphaned {
            reason: "exe_missing".to_owned(),
            remedy: format!("rebuild the tree's binaries, or destroy by hand: `{recipe}`"),
            recipe,
        });
    }

    let mut environment = invoker_env.clone();
    environment.extend(snapshot.env.clone());
    if tree_missing || bin_missing {
        strip_tree_paths(&mut environment, &snapshot.bin_dirs, tree_root)?;
    }

    let cwd = match snapshot.key.tied_to {
        TiedTo::Tree if !tree_missing => tree_root.join(snapshot.cwd_rel.as_str()),
        TiedTo::Tree => std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp")),
        TiedTo::Repo => {
            if !current_repo_root.is_dir() {
                return Ok(ExecuteResult::Orphaned {
                    reason: "repo_root_missing".to_owned(),
                    remedy: "`wt register … --move-to`, or destroy by hand".to_owned(),
                    recipe,
                });
            }
            current_repo_root.join(snapshot.cwd_rel.as_str())
        }
    };
    let request = CommandRequest::expanded(command, cwd, environment)?;
    let at = timestamp();
    match action {
        Action::Exists => Ok(ExecuteResult::Probe(proc::probe(
            &request,
            options.deadlines.probe,
            at,
        ))),
        Action::Destroy => {
            let output = proc::run(
                &request,
                options.log,
                Some(options.deadlines.destroy),
                options.tee,
            )?;
            if output.success() {
                crate::failpoint::resource_destroyed()?;
            }
            Ok(ExecuteResult::Child(output))
        }
        Action::Run => {
            crate::failpoint::resource_frozen()?;
            proc::run(&request, options.log, options.deadlines.run, options.tee)
                .map(ExecuteResult::Child)
        }
    }
}

/// Captures the invoking process's Unicode environment without panicking.
pub fn capture_env() -> EnvMap {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// Captures executable regular-file names from declared bin directories.
pub fn capture_bin_exes(bin_dirs: &[PathBuf]) -> Result<Vec<String>> {
    let mut executables = BTreeSet::new();
    for directory in bin_dirs {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(snapshot_io("read bin directory", directory, error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| snapshot_io("read bin entry", directory, error))?;
            if crate::fsx::is_executable_file(&entry.path())? {
                executables.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    Ok(executables.into_iter().collect())
}

/// Reports terminal attachment for the three standard streams.
pub fn tty() -> Tty {
    Tty {
        stdin: std::io::stdin().is_terminal(),
        stdout: std::io::stdout().is_terminal(),
        stderr: std::io::stderr().is_terminal(),
    }
}

fn command_for(snapshot: &ResourceSnapshot, action: Action) -> Result<&ExpandedCommand> {
    let command = match action {
        Action::Exists => snapshot.exists.as_ref(),
        Action::Destroy => Some(&snapshot.destroy),
        Action::Run => snapshot.run.as_ref(),
    };
    command.ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "CONFIG_INVALID",
            format!("snapshot has no {:?} recipe", action),
            "refresh the resource declaration from a valid task",
        )
    })
}

fn recipe_text(command: &ExpandedCommand) -> String {
    match command {
        ExpandedCommand::Shell { shell } => shell.clone(),
        ExpandedCommand::Argv { argv } => argv.join(" "),
    }
}

fn strip_tree_paths(
    environment: &mut BTreeMap<String, String>,
    bin_dirs: &[String],
    tree_root: &Path,
) -> Result<()> {
    let Some(path) = environment.get("PATH") else {
        return Ok(());
    };
    let mut bins = bin_dirs.iter().map(PathBuf::from).collect::<BTreeSet<_>>();
    bins.insert(tree_root.join(".wt/shims"));
    let kept = std::env::split_paths(path)
        .filter(|entry| !bins.contains(entry))
        .collect::<Vec<_>>();
    let joined = std::env::join_paths(kept).map_err(|error| {
        CoreError::new(
            ExitClass::State,
            "SNAPSHOT_ENV_INVALID",
            format!("snapshot PATH cannot be rebuilt safely: {error}"),
            "refresh the resource declaration with a valid PATH",
        )
    })?;
    environment.insert("PATH".to_owned(), joined.to_string_lossy().into_owned());
    Ok(())
}

fn timestamp() -> String {
    crate::fsx::timestamp().unwrap_or_else(|_| "1970-01-01T00:00:00.000000000Z".to_owned())
}

fn resolved_duration(value: Option<&str>, key: &str) -> Result<Duration> {
    let millis = value.and_then(duration_millis).ok_or_else(|| {
        CoreError::new(
            ExitClass::State,
            "SETTINGS_INVALID",
            format!("{key} has no resolved valid duration"),
            "fix `$WT_HOME/config.toml` and reload settings",
        )
    })?;
    Ok(Duration::from_millis(millis))
}

fn snapshot_io(action: &str, path: &Path, error: std::io::Error) -> CoreError {
    CoreError::new(
        ExitClass::Internal,
        "IO_FAILED",
        format!("{action} {}: {error}", path.display()),
        "retry the operation and inspect filesystem permissions if it repeats",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;
    use wt_core::config::TiedTo;
    use wt_core::model::{Label, RelPath};
    use wt_core::resource::{ResourceKey, SnapshotRoots};

    use super::*;

    fn snapshot(tree: &Path, command: ExpandedCommand) -> ResourceSnapshot {
        ResourceSnapshot {
            schema: 1,
            key: ResourceKey {
                label: Label::new("repo").unwrap(),
                tied_to: TiedTo::Tree,
                name: Some("tree".into()),
                scope: RelPath::new(".").unwrap(),
                task: "daemon".into(),
            },
            name: "repo_tree_daemon".into(),
            cwd_rel: RelPath::new(".").unwrap(),
            exists: Some(command.clone()),
            destroy: command.clone(),
            run: Some(command),
            env: BTreeMap::new(),
            bin_dirs: vec![tree.join("bin").to_string_lossy().into_owned()],
            bin_exes: vec!["tree-tool".into()],
            roots: SnapshotRoots {
                tree: tree.to_string_lossy().into_owned(),
                home: "/tmp/wt-home".into(),
            },
            recorded_at: "now".into(),
        }
    }

    #[test]
    fn missing_tree_blocks_only_the_recipe_that_names_a_recorded_executable() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("gone");
        let blocked = snapshot(
            &missing,
            ExpandedCommand::Shell {
                shell: "tree-tool stop".into(),
            },
        );
        assert!(matches!(
            execute(
                &blocked,
                Action::Destroy,
                &BTreeMap::new(),
                dir.path(),
                None,
                Tee::Quiet,
            )
            .unwrap(),
            ExecuteResult::Orphaned { ref reason, .. } if reason == "exe_missing"
        ));

        let record = dir.path().join("record");
        let safe = snapshot(
            &missing,
            ExpandedCommand::Shell {
                shell: format!("printf %s \"$PATH\" > {}", record.display()),
            },
        );
        let invoker = BTreeMap::from([(
            "PATH".into(),
            format!(
                "{}:{}:/bin:/usr/bin",
                missing.join(".wt/shims").display(),
                missing.join("bin").display()
            ),
        )]);
        let result = execute(
            &safe,
            Action::Destroy,
            &invoker,
            dir.path(),
            None,
            Tee::Quiet,
        )
        .unwrap();
        assert!(matches!(result, ExecuteResult::Child(ref output) if output.success()));
        assert_eq!(fs::read_to_string(record).unwrap(), "/bin:/usr/bin");
    }

    #[test]
    fn snapshot_environment_overlays_the_invoker() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        let mut value = snapshot(
            dir.path(),
            ExpandedCommand::Shell {
                shell: "test \"$KEY\" = snapshot".into(),
            },
        );
        value.env.insert("KEY".into(), "snapshot".into());
        let invoker = BTreeMap::from([("KEY".into(), "invoker".into())]);
        assert!(matches!(
            execute(
                &value,
                Action::Run,
                &invoker,
                dir.path(),
                None,
                Tee::Quiet,
            )
            .unwrap(),
            ExecuteResult::Child(ref output) if output.success()
        ));
    }

    #[test]
    fn repo_tied_execution_uses_current_canonical_root_and_tees_to_log() {
        let tree = tempdir().unwrap();
        let canonical = tempdir().unwrap();
        let mut value = snapshot(
            tree.path(),
            ExpandedCommand::Shell {
                shell: "pwd; printf resource-output >&2".into(),
            },
        );
        value.key.tied_to = TiedTo::Repo;
        value.key.name = None;
        let log = tree.path().join("resource.log");

        let result = execute(
            &value,
            Action::Destroy,
            &BTreeMap::new(),
            canonical.path(),
            Some(&log),
            Tee::Quiet,
        )
        .unwrap();
        let ExecuteResult::Child(output) = result else {
            panic!("repo-tied destroy should run");
        };
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            canonical.path().canonicalize().unwrap().to_string_lossy()
        );
        assert!(fs::read_to_string(log).unwrap().contains("resource-output"));

        let missing = tree.path().join("missing-canonical");
        assert!(matches!(
            execute(
                &value,
                Action::Destroy,
                &BTreeMap::new(),
                &missing,
                None,
                Tee::Quiet,
            )
            .unwrap(),
            ExecuteResult::Orphaned { ref reason, .. } if reason == "repo_root_missing"
        ));
    }

    #[test]
    fn executable_capture_is_sorted_and_ignores_non_executables() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("z"), []).unwrap();
        fs::write(dir.path().join("a"), []).unwrap();
        fs::set_permissions(dir.path().join("z"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            capture_bin_exes(&[dir.path().to_path_buf()]).unwrap(),
            ["z"]
        );
    }

    #[test]
    fn executable_capture_follows_symlinks_to_binaries() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real");
        fs::write(&real, []).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(&real, bin.join("linked")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), bin.join("dangling")).unwrap();
        std::os::unix::fs::symlink(dir.path(), bin.join("to-dir")).unwrap();
        assert_eq!(capture_bin_exes(&[bin]).unwrap(), ["linked"]);
    }

    #[test]
    fn tokenizer_scans_only_the_selected_recipe() {
        let dir = tempdir().unwrap();
        let mut value = snapshot(
            &dir.path().join("gone"),
            ExpandedCommand::Shell {
                shell: "true".into(),
            },
        );
        value.run = Some(ExpandedCommand::Shell {
            shell: "tree-tool start".into(),
        });
        assert!(matches!(
            execute(
                &value,
                Action::Exists,
                &capture_env(),
                dir.path(),
                None,
                Tee::Quiet,
            )
            .unwrap(),
            ExecuteResult::Probe(_)
        ));
        assert!(matches!(
            execute(
                &value,
                Action::Run,
                &capture_env(),
                dir.path(),
                None,
                Tee::Quiet,
            )
            .unwrap(),
            ExecuteResult::Orphaned { .. }
        ));
    }
}
