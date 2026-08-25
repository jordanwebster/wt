use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wt_core::doctor::{Finding, Severity};
use wt_core::report::DoctorData;
use wt_core::resource::{ProbeResult, ResourceState};
use wt_core::CoreError;

use crate::cli::Doctor;

use super::{list, remove, Context, Output};

pub(crate) fn run(context: &mut Context, args: Doctor) -> Result<Output, CoreError> {
    let mut findings = vec![finding(
        Severity::Info,
        "SESSION_BACKEND",
        "sessions",
        format!(
            "session backend is {}",
            context.settings.session.backend.as_str()
        ),
        "set `session.backend` in `$WT_HOME/config.toml` to change it",
    )];
    tooling_findings(context, &mut findings);
    if wt_core::deactivate(&context.parent_env)?
        .report
        .activation_ignored
    {
        findings.push(finding(
            Severity::Warn,
            "ACTIVATION_IGNORED",
            "WT_ACTIVATION",
            "invalid activation metadata was ignored",
            "remove WT_ACTIVATION or enter through a fresh wt door",
        ));
    }
    state_orphan_findings(context, args.label.as_deref(), &mut findings)?;
    for (label, record) in context.registry.labels.clone() {
        if args
            .label
            .as_deref()
            .is_some_and(|filter| label.as_str() != filter)
        {
            continue;
        }
        let repo_path = Path::new(record.path.as_str());
        if !matches!(
            wt_sys::fsx::path_kind(repo_path)?,
            wt_sys::fsx::PathKind::Directory
        ) {
            findings.push(finding(
                Severity::Error,
                "REPO_PATH_MISSING",
                label.to_string(),
                "registered repository path is missing",
                "move the checkout back or run `wt register --label LABEL --move-to PATH`",
            ));
            continue;
        }
        git_registry_findings(context, &label, &record, &mut findings)?;
    }
    for tree in context.registry.trees.clone() {
        if args
            .label
            .as_deref()
            .is_some_and(|label| tree.label.as_str() != label)
        {
            continue;
        }
        tree_findings(context, &tree, args.probe, &mut findings)?;
    }
    let counts = wt_core::doctor::sort_and_count(&mut findings);
    Output::data(DoctorData { findings, counts })
}

fn tree_findings(
    context: &mut Context,
    tree: &wt_core::model::TreeRec,
    probe: bool,
    findings: &mut Vec<Finding>,
) -> Result<(), CoreError> {
    let target = super::context::target_of(tree);
    let state = context.read_state(&target)?;
    let report = list::tree_report(context, tree, true, false, probe)?;
    let phase = context.phase(tree, state.as_ref())?;
    if let Some((severity, code, remedy)) = phase_finding(phase) {
        let remedy = if phase == wt_core::lifecycle::DerivedPhase::Replaced && tree.canonical {
            format!(
                "run `wt register {} --label {} --repair`",
                tree.path.as_str(),
                tree.label
            )
        } else {
            remedy.to_owned()
        };
        findings.push(finding(
            severity,
            code,
            report.target.clone(),
            format!("tree phase is {}", report.phase),
            &remedy,
        ));
    }
    if state.as_ref().is_some_and(|state| state.verify_pending) {
        findings.push(finding(
            Severity::Warn,
            "VERIFY_PENDING",
            &report.target,
            "verification is pending",
            "re-run `wt new --verify` for this tree",
        ));
    }
    if report.phase == "missing"
        && state
            .as_ref()
            .is_some_and(|state| !state.resources.is_empty())
    {
        findings.push(finding(
            Severity::Warn,
            "TREE_MISSING_PENDING",
            report.target.clone(),
            "missing tree still has resource records",
            "run `wt prune --records` before recreating the name",
        ));
    }
    if let Some(skipped) = state
        .as_ref()
        .and_then(|state| state.last_error.as_deref())
        .and_then(|error| error.strip_prefix("REFRESH_SKIPPED:"))
    {
        findings.push(finding(
            Severity::Warn,
            "REFRESH_SKIPPED",
            format!("{}:{skipped}", report.target),
            "resource declaration refresh was skipped because an environment value was undefined",
            "define the missing environment value and run `wt sync`",
        ));
    }
    for record in state
        .as_ref()
        .into_iter()
        .flat_map(|state| state.resources.values())
    {
        let subject = format!("{}:{}", report.target, record.key.task);
        if record.state == ResourceState::Orphaned {
            findings.push(finding(
                Severity::Warn,
                "RESOURCE_ORPHANED",
                &subject,
                "resource teardown is orphaned",
                "restore the dependency and run `wt prune --records`",
            ));
        }
        if record.undeclared {
            findings.push(finding(
                Severity::Warn,
                "RESOURCE_UNDECLARED",
                &subject,
                "resource record is no longer declared",
                "destroy the resource or restore its declaration",
            ));
        }
        if record
            .last_probe
            .as_ref()
            .is_some_and(|probe| matches!(probe.result, ProbeResult::Failed { .. }))
        {
            findings.push(finding(
                Severity::Warn,
                "RESOURCE_PROBE_FAILED",
                &subject,
                "resource probe failed",
                "restore the probe dependency and retry with --probe",
            ));
        }
        if record.state == ResourceState::Declared
            && record.last_probe.as_ref().is_some_and(|probe| {
                matches!(probe.result, ProbeResult::Absent) && record.instance.is_some()
            })
        {
            findings.push(finding(
                Severity::Info,
                "RESOURCE_GONE",
                &subject,
                "previously instantiated resource is absent",
                "run its task to recreate it or remove the stale record",
            ));
        }
    }
    if tree.geometry.base != context.settings.ports.base
        || tree.geometry.stride != context.settings.ports.stride
    {
        findings.push(finding(
            Severity::Info,
            "GEOMETRY_CHANGED",
            &report.target,
            "current settings differ from this tree's frozen port geometry",
            "remove and recreate the tree to adopt the new geometry",
        ));
    }
    if tree.ports.len() >= usize::from(tree.geometry.stride) {
        findings.push(finding(
            Severity::Warn,
            "PORTS_EXHAUSTED",
            &report.target,
            "tree has no unallocated port index",
            "remove and recreate the tree or raise stride for future trees",
        ));
    }
    if wt_sys::lock::is_held(&context.tree_lock_path(&target))? {
        let holders = super::remove::door_holders(context, &target)?;
        findings.push(finding(
            Severity::Info,
            "TREE_IN_USE",
            &report.target,
            if holders.is_empty() {
                "tree lock is currently held".to_owned()
            } else {
                format!(
                    "tree lock is held by {}",
                    holders
                        .iter()
                        .map(|holder| format!("pid {} ({})", holder.pid, holder.verb))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            "wait for the holder or stop the process",
        ));
    }
    let running_task = super::remove::door_holders(context, &target)?
        .iter()
        .any(|holder| holder.verb == "run");
    let mut any_bound = false;
    for port in &report.ports {
        if wt_sys::net::squat_probe(port.port, Duration::from_millis(50)).unwrap_or(false) {
            any_bound = true;
            if report.session != "yes" && !running_task {
                findings.push(finding(
                    Severity::Warn,
                    "PORT_SQUATTED",
                    format!("{}:{}", report.target, port.name),
                    format!("port {} is bound without a wt session", port.port),
                    "stop the unrelated listener or recreate the tree in another slot",
                ));
            }
            findings.push(finding(
                Severity::Info,
                "PORT_BOUND",
                format!("{}:{}", report.target, port.name),
                format!("port {} is bound", port.port),
                "inspect the listener if the binding is unexpected",
            ));
        }
    }
    if any_bound && report.session != "yes" && !running_task {
        findings.push(finding(
            Severity::Info,
            "SLOT_SQUATTED",
            &report.target,
            "one or more ports in this tree's allocated slot are occupied",
            "stop the unrelated listener or recreate the tree in another slot",
        ));
    }
    let root = Path::new(tree.path.as_str());
    if !matches!(
        wt_sys::fsx::path_kind(root)?,
        wt_sys::fsx::PathKind::Directory
    ) || phase == wt_core::lifecycle::DerivedPhase::Replaced
    {
        return Ok(());
    }
    let config = context.load_config(tree)?;
    config_findings(context, tree, &config, findings)?;
    for record in state
        .as_ref()
        .into_iter()
        .flat_map(|state| state.resources.values())
    {
        let effective = wt_core::config::effective_scope(&config, record.key.scope.as_str())?;
        let template = effective
            .tasks
            .get(&record.key.task)
            .and_then(|task| task.name.as_deref());
        findings.extend(wt_core::doctor::resource_name_findings(
            &format!("{}:{}", report.target, record.key.task),
            template,
            record.name(),
        )?);
    }
    if let Some(observation) = remove::observe_git(context, tree, true)? {
        if observation.merged && !tree.canonical {
            findings.push(finding(
                Severity::Info,
                "BRANCH_MERGED",
                &report.target,
                "branch is merged into the default branch",
                "run `wt prune --merged` after reviewing the tree",
            ));
        }
        if observation.upstream == wt_core::remove::Upstream::Gone {
            findings.push(finding(
                Severity::Warn,
                "UPSTREAM_GONE",
                &report.target,
                "configured upstream no longer exists",
                "push a replacement upstream or run `wt prune --gone`",
            ));
        }
    }
    Ok(())
}

fn config_findings(
    context: &Context,
    tree: &wt_core::model::TreeRec,
    config: &wt_core::config::Config,
    findings: &mut Vec<Finding>,
) -> Result<(), CoreError> {
    let subject = super::context::target_of(tree).to_string();
    let has_env_aliases = config
        .root
        .env
        .values()
        .any(|value| value.value().is_some());
    let has_resources = config
        .root
        .task
        .values()
        .any(|task| task.value().is_some_and(|task| task.is_resource()));
    if tree.canonical {
        if let Some(finding) = wt_core::doctor::no_coordination(
            tree.label.as_str(),
            !config.ports.is_empty(),
            has_env_aliases,
            has_resources,
        ) {
            findings.push(finding);
        }
    }
    let root = Path::new(tree.path.as_str());
    for bin in &config.root.bin {
        let path = root.join(bin.as_str());
        if !matches!(
            wt_sys::fsx::path_kind(&path)?,
            wt_sys::fsx::PathKind::Directory
        ) {
            findings.push(finding(
                Severity::Warn,
                "BIN_DIR_MISSING",
                &subject,
                format!("declared bin directory {} is missing", path.display()),
                "create the directory by running the build task",
            ));
        } else if context
            .parent_env
            .get("PATH")
            .and_then(|path_env| path_env.split(':').next())
            != Some(path.to_string_lossy().as_ref())
        {
            findings.push(finding(
                Severity::Warn,
                "PATH_NOT_SHADOWED",
                &subject,
                "declared bin directory is not first on PATH",
                "enter through a wt door or install the shell-init PATH guard",
            ));
        }
    }
    if config
        .root
        .task
        .get("verify")
        .is_none_or(|task| task.value().is_none())
    {
        findings.push(finding(
            Severity::Info,
            "NO_VERIFY",
            &subject,
            "no verify task is declared",
            "declare task.verify when lifecycle verification is desired",
        ));
    }
    let snapshot = wt_sys::fsx::capture_dir_snapshot(
        root,
        &wt_core::model::RelPath::new(".")?,
        &[
            "Cargo.toml",
            "Cargo.lock",
            "package.json",
            "package-lock.json",
            "npm-shrinkwrap.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
            "pyproject.toml",
            "requirements.txt",
            "setup.py",
            "uv.lock",
            "poetry.lock",
            "go.mod",
            "go.sum",
            ".gitmodules",
            "rustfmt.toml",
            ".rustfmt.toml",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    )?;
    let effective = wt_core::config::effective_scope(config, ".")?;
    let hits = wt_core::adapters::detect(&snapshot, &effective.adapters)?;
    if hits.is_empty() {
        findings.push(finding(
            Severity::Info,
            "NO_ADAPTER",
            &subject,
            "no built-in adapter matched this checkout",
            "configure tasks explicitly or add a supported project manifest",
        ));
    } else {
        let contribution = wt_core::adapters::contribution(&hits)?;
        let available = available_binaries(&context.parent_env);
        findings.extend(wt_core::doctor::adapter_findings(
            &subject,
            &contribution,
            &available,
            &context.parent_env,
        ));
        let has_lockfile = snapshot.names.iter().any(|name| {
            [
                "Cargo.lock",
                "package-lock.json",
                "pnpm-lock.yaml",
                "yarn.lock",
                "uv.lock",
                "poetry.lock",
                "go.sum",
            ]
            .contains(&name.as_str())
        });
        if !has_lockfile {
            findings.push(finding(
                Severity::Info,
                "NO_LOCKFILE",
                &subject,
                "adapter matched without a lockfile",
                "commit the ecosystem lockfile for reproducible sync",
            ));
        }
    }
    let exclude =
        Path::new(context.registry.labels[&tree.label].common_gitdir.as_str()).join("info/exclude");
    let exclude_text = wt_sys::fsx::read_string(&exclude)?.unwrap_or_default();
    if !exclude_text.contains("# >>> wt managed >>>") {
        findings.push(finding(
            Severity::Warn,
            "EXCLUDE_MISSING",
            &subject,
            "managed git exclude block is missing",
            "run `wt sync` or re-register the checkout",
        ));
    }
    if exclude_text.matches("# >>> wt managed >>>").count()
        > exclude_text.matches("# <<< wt managed <<<").count()
    {
        findings.push(finding(
            Severity::Info,
            "EXCLUDE_REPAIRED",
            &subject,
            "the managed exclude block is unclosed and will be repaired",
            "run `wt sync` to repair the managed block",
        ));
    }
    Ok(())
}

fn git_registry_findings(
    context: &Context,
    label: &wt_core::model::Label,
    record: &wt_core::model::LabelRec,
    findings: &mut Vec<Finding>,
) -> Result<(), CoreError> {
    let git = context.git(Path::new(record.path.as_str()))?;
    let listed = git.worktrees()?;
    let registered = context
        .registry
        .trees
        .iter()
        .filter(|tree| &tree.label == label)
        .map(|tree| PathBuf::from(tree.path.as_str()))
        .collect::<BTreeSet<_>>();
    for worktree in listed {
        if registered.contains(&worktree.path) {
            continue;
        }
        let exists = matches!(
            wt_sys::fsx::path_kind(&worktree.path)?,
            wt_sys::fsx::PathKind::Directory
        );
        findings.push(finding(
            Severity::Warn,
            if exists {
                "UNMANAGED_WORKTREE"
            } else {
                "STALE_GIT_WORKTREE"
            },
            worktree.path.to_string_lossy(),
            if exists {
                "git worktree is not registered with wt"
            } else {
                "git retains a missing worktree record"
            },
            if exists {
                "run `wt adopt` for the checkout"
            } else {
                "run `wt prune --yes`"
            },
        ));
    }
    Ok(())
}

fn state_orphan_findings(
    context: &Context,
    label_filter: Option<&str>,
    findings: &mut Vec<Finding>,
) -> Result<(), CoreError> {
    for label_dir in wt_sys::fsx::read_dir_paths(&context.home.join("state"))? {
        let Some(label) = label_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if label_filter.is_some_and(|filter| filter != label) {
            continue;
        }
        for path in wt_sys::fsx::read_dir_paths(&label_dir)? {
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == "_repo" {
                continue;
            }
            if !context
                .registry
                .trees
                .iter()
                .any(|tree| tree.label.as_str() == label && tree.name == name)
            {
                findings.push(wt_core::doctor::state_orphan(
                    &format!("{label}/{name}"),
                    &path.to_string_lossy(),
                ));
            }
        }
    }
    Ok(())
}

fn tooling_findings(_context: &Context, findings: &mut Vec<Finding>) {
    match wt_sys::git::Git::version("git", Duration::from_secs(5)) {
        Ok(version) if version < (2, 31, 0) => findings.push(finding(
            Severity::Warn,
            "GIT_TOO_OLD",
            "git",
            format!(
                "git {}.{}.{} is older than 2.31",
                version.0, version.1, version.2
            ),
            "upgrade git to 2.31 or newer",
        )),
        _ => {}
    }
}

fn available_binaries(env: &std::collections::BTreeMap<String, String>) -> BTreeSet<String> {
    let mut output = BTreeSet::new();
    let Some(path) = env.get("PATH") else {
        return output;
    };
    for directory in path.split(':') {
        let directory = Path::new(directory);
        for entry in wt_sys::fsx::read_dir_paths(directory).unwrap_or_default() {
            if wt_sys::fsx::is_executable_file(&entry).unwrap_or(false) {
                if let Some(name) = entry.file_name().and_then(|name| name.to_str()) {
                    output.insert(name.to_owned());
                }
            }
        }
    }
    output
}

fn phase_finding(
    phase: wt_core::lifecycle::DerivedPhase,
) -> Option<(Severity, &'static str, &'static str)> {
    use wt_core::lifecycle::DerivedPhase;
    match phase {
        DerivedPhase::Missing => Some((
            Severity::Warn,
            "TREE_MISSING",
            "run `wt prune` or `wt remove`",
        )),
        DerivedPhase::Replaced => Some((
            Severity::Error,
            "TREE_REPLACED",
            "do not touch the replacement; run `wt prune --records` or `wt adopt`",
        )),
        DerivedPhase::Incomplete => Some((
            Severity::Warn,
            "TREE_INCOMPLETE",
            "re-run `wt new` to resume or run `wt remove`",
        )),
        DerivedPhase::Interrupted => Some((
            Severity::Warn,
            "TREE_INTERRUPTED",
            "re-run `wt new` or `wt sync`",
        )),
        DerivedPhase::InitInterrupted => Some((
            Severity::Warn,
            "INIT_INTERRUPTED",
            "re-run `wt register` or `wt adopt`",
        )),
        DerivedPhase::RemoveInterrupted => {
            Some((Severity::Warn, "REMOVE_INTERRUPTED", "re-run `wt remove`"))
        }
        DerivedPhase::Claimed => Some((Severity::Warn, "TREE_CLAIMED", "re-run `wt new`")),
        DerivedPhase::Ready => None,
        DerivedPhase::Verifying => None,
        DerivedPhase::Initialising | DerivedPhase::Bootstrapping | DerivedPhase::Removing => None,
        DerivedPhase::Failed => None,
        DerivedPhase::Creating => None,
        DerivedPhase::Unmanaged | DerivedPhase::StaleGit => None,
    }
}

fn finding(
    severity: Severity,
    code: impl Into<String>,
    subject: impl Into<String>,
    message: impl Into<String>,
    remedy: impl Into<String>,
) -> Finding {
    Finding::new(severity, code, subject, message, remedy)
}

trait ResourceName {
    fn name(&self) -> &str;
}

impl ResourceName for wt_core::resource::ResourceRecord {
    fn name(&self) -> &str {
        self.effective_snapshot().name.as_str()
    }
}
