use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wt_core::doctor::{Finding, Severity};
use wt_core::report::DoctorData;
use wt_core::resource::{ProbeResult, ResourceState};
use wt_core::CoreError;

use crate::cli::Doctor;

use super::{door, executor, list, Context, Output};

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
    cache_orphan_findings(context, args.label.as_deref(), &mut findings)?;
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
    let trees = context
        .registry
        .trees
        .clone()
        .into_iter()
        .filter(|tree| {
            args.label
                .as_deref()
                .is_none_or(|label| tree.label.as_str() == label)
        })
        .collect::<Vec<_>>();
    let env = list::ReportEnv::collect(context, &trees, list::SharedResourceRecords::default())?;
    for tree in trees {
        tree_findings(context, &tree, args.probe, &env, &mut findings)?;
    }
    let counts = wt_core::doctor::sort_and_count(&mut findings);
    Output::data(DoctorData { findings, counts })
}

/// Doctor observes only what its findings consume: phase, build, session,
/// ports, and the branch-health facts. It deliberately runs no `git status`
/// — no finding classifies dirtiness — so a fleet checkup does not pay for
/// full working-tree scans.
fn tree_findings(
    context: &mut Context,
    tree: &wt_core::model::TreeRec,
    probe: bool,
    env: &list::ReportEnv,
    findings: &mut Vec<Finding>,
) -> Result<(), CoreError> {
    let target = super::context::target_of(tree);
    let subject = target.to_string();
    let dir_exists = matches!(
        wt_sys::fsx::path_kind(Path::new(tree.path.as_str()))?,
        wt_sys::fsx::PathKind::Directory
    );
    if probe && dir_exists && context.identity_ok(tree)? {
        let mut entered = door::enter(context, Some(&subject), "probe")?;
        executor::refresh_all_declarations(context, &entered)?;
        executor::probe_all_resources(context, &entered)?;
        entered.release_gate();
    }
    let state = context.read_state(&target)?;
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
            subject.clone(),
            format!("tree phase is {}", list::phase_name(phase)),
            &remedy,
        ));
    }
    if state.as_ref().is_some_and(|state| state.verify_pending) {
        findings.push(finding(
            Severity::Warn,
            "VERIFY_PENDING",
            &subject,
            "verification is pending",
            "re-run `wt new --verify` for this tree",
        ));
    }
    if let Some(build) = &list::build_report(tree, state.as_ref())? {
        match build.state.as_str() {
            "running" => findings.push(finding(
                Severity::Info,
                "BUILD_RUNNING",
                &subject,
                format!("automatic build is running; log: {}", build.log),
                "wait for completion or inspect the build log",
            )),
            "abandoned" => findings.push(finding(
                Severity::Warn,
                "BUILD_ABANDONED",
                &subject,
                format!("automatic build was abandoned; log: {}", build.log),
                format!(
                    "inspect the log and run `wt build {}` to retry",
                    subject
                ),
            )),
            "failed" => findings.push(finding(
                Severity::Warn,
                "BUILD_FAILED",
                &subject,
                format!("automatic build failed; log: {}", build.log),
                "inspect the log and run `wt build` to retry",
            )),
            "unknown" => findings.push(finding(
                Severity::Warn,
                "BUILD_STATUS_MISSING",
                &subject,
                format!("automatic build status is unavailable; log: {}", build.log),
                "inspect the log and run `wt build` to establish a fresh status",
            )),
            "ok" => {}
            value => findings.push(finding(
                Severity::Warn,
                "BUILD_STATUS_UNKNOWN",
                &subject,
                format!(
                    "automatic build has unrecognised status `{value}`; log: {}",
                    build.log
                ),
                format!(
                    "inspect the log and run `wt build {}` to establish a fresh status",
                    subject
                ),
            )),
        }
    }
    if phase == wt_core::lifecycle::DerivedPhase::Missing
        && state
            .as_ref()
            .is_some_and(|state| !state.resources.is_empty())
    {
        findings.push(finding(
            Severity::Warn,
            "TREE_MISSING_PENDING",
            subject.clone(),
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
            format!("{}:{skipped}", subject),
            "resource declaration refresh was skipped because an environment value was undefined",
            "define the missing environment value and run `wt sync`",
        ));
    }
    for record in state
        .as_ref()
        .into_iter()
        .flat_map(|state| state.resources.values())
    {
        let subject = format!("{}:{}", subject, record.key.task);
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
            &subject,
            "current settings differ from this tree's frozen port geometry",
            "remove and recreate the tree to adopt the new geometry",
        ));
    }
    if tree.ports.len() >= usize::from(tree.geometry.stride) {
        findings.push(finding(
            Severity::Warn,
            "PORTS_EXHAUSTED",
            &subject,
            "tree has no unallocated port index",
            "remove and recreate the tree or raise stride for future trees",
        ));
    }
    let holders = super::remove::door_holders(context, &target)?;
    if wt_sys::lock::is_held(&context.tree_lock_path(&target))? {
        findings.push(finding(
            Severity::Info,
            "TREE_IN_USE",
            &subject,
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
    let running_task = holders.iter().any(|holder| holder.verb == "run");
    let session = env.session_state(tree);
    let mut ports = tree
        .ports
        .iter()
        .map(|(name, index)| (name.to_string(), tree.geometry.port_base + u16::from(*index)))
        .collect::<Vec<_>>();
    ports.sort_by_key(|(name, _)| {
        tree.ports[&wt_core::model::PortName::new(name).expect("stored port name is valid")]
    });
    let mut any_bound = false;
    for (name, port) in &ports {
        if wt_sys::net::squat_probe(*port, Duration::from_millis(50)).unwrap_or(false) {
            any_bound = true;
            if session != "yes" && !running_task {
                findings.push(finding(
                    Severity::Warn,
                    "PORT_SQUATTED",
                    format!("{subject}:{name}"),
                    format!("port {port} is bound without a wt session"),
                    "stop the unrelated listener or recreate the tree in another slot",
                ));
            }
            findings.push(finding(
                Severity::Info,
                "PORT_BOUND",
                format!("{subject}:{name}"),
                format!("port {port} is bound"),
                "inspect the listener if the binding is unexpected",
            ));
        }
    }
    if any_bound && session != "yes" && !running_task {
        findings.push(finding(
            Severity::Info,
            "SLOT_SQUATTED",
            &subject,
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
            &format!("{}:{}", subject, record.key.task),
            template,
            record.name(),
        )?);
    }
    // Branch health needs only ref-level facts — merged means HEAD is an
    // ancestor of (and not equal to) the default ref, gone means the
    // configured upstream no longer resolves. Deliberately narrower than
    // the removal observer, which also classifies dirtiness and remote
    // containment that no doctor finding consumes.
    let git = context.git(root)?;
    // wt turns these caches on for the worktrees it creates; a tree
    // without them (typically the canonical checkout, or one created
    // before wt did this) pays a full working-tree scan on every status.
    if git.config_bool_in(root, "core.untrackedCache")? != Some(true) {
        findings.push(finding(
            Severity::Info,
            "STATUS_CACHE_OFF",
            &subject,
            "git's untracked cache is off, so each status scans the whole tree",
            "run `git config core.untrackedCache true` in this checkout (on macOS or Windows also `git config core.fsmonitor true`)",
        ));
    }
    let branch = git.head_branch(root)?;
    let merged = if tree.canonical {
        false
    } else if let Some(default_ref) = env.default_ref(&tree.label) {
        git.is_ancestor("HEAD", default_ref)?
            && git.resolve_commit("HEAD")? != git.resolve_commit(default_ref)?
    } else {
        false
    };
    if merged {
        findings.push(finding(
            Severity::Info,
            "BRANCH_MERGED",
            &subject,
            "branch is merged into the default branch",
            "run `wt prune --merged` after reviewing the tree",
        ));
    }
    let upstream_gone = match branch.as_deref() {
        Some(branch) => git.upstream_info(branch)?.gone,
        None => false,
    };
    if upstream_gone {
        findings.push(finding(
            Severity::Warn,
            "UPSTREAM_GONE",
            &subject,
            "configured upstream no longer exists",
            "push a replacement upstream or run `wt prune --gone`",
        ));
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
    let effective = wt_core::config::effective_scope(config, ".")?;
    shim_findings(context, tree, &effective, findings)?;
    let mut has_existing_bin = false;
    for bin in &effective.bin {
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
        } else {
            has_existing_bin = true;
        }
    }
    if (has_existing_bin || !effective.commands.is_empty())
        && !path_prefix_is_assembled(context, root, &effective)
    {
        findings.push(finding(
            Severity::Warn,
            "PATH_NOT_SHADOWED",
            &subject,
            "the expected door prefix is not first on PATH",
            "enter through a wt door or install the shell-init PATH guard",
        ));
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
        let machine_files = nudge_file_contents(&contribution, &context.parent_env);
        findings.extend(wt_core::doctor::adapter_findings(
            &subject,
            &contribution,
            &available,
            &context.parent_env,
            &machine_files,
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

fn shim_findings(
    context: &Context,
    tree: &wt_core::model::TreeRec,
    effective: &wt_core::config::EffectiveScope,
    findings: &mut Vec<Finding>,
) -> Result<(), CoreError> {
    let root = Path::new(tree.path.as_str());
    let target = super::context::target_of(tree).to_string();
    let shadows = shell_shadows(&context.parent_env);
    for command in &effective.commands {
        let path = root.join(".wt/shims").join(command);
        let broken = match wt_sys::fsx::path_kind(&path)? {
            wt_sys::fsx::PathKind::Symlink => wt_sys::fsx::read_link(&path)?
                .is_none_or(|link| !wt_sys::fsx::is_executable_file(&link).unwrap_or(false)),
            _ => true,
        };
        if broken {
            findings.push(finding(
                Severity::Warn,
                "SHIM_BROKEN",
                format!("{target}:{command}"),
                format!("owned command shim {} is missing or broken", path.display()),
                format!("run `wt env {target}` to repair the shim"),
            ));
        }
        if shadows.contains(command) {
            findings.push(finding(
                Severity::Info,
                "SHIM_SHADOWED",
                format!("{target}:{command}"),
                format!("shell alias or function `{command}` outranks the owned command shim"),
                format!("remove the `{command}` alias or function to use the tree's command"),
            ));
        }
    }
    Ok(())
}

fn shell_shadows(environment: &std::collections::BTreeMap<String, String>) -> BTreeSet<String> {
    let mut shadows = environment
        .get("WT_SHELL_SHADOWS")
        .into_iter()
        .flat_map(|value| value.lines())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for key in environment.keys() {
        if let Some(name) = key
            .strip_prefix("BASH_FUNC_")
            .and_then(|name| name.strip_suffix("%%"))
        {
            shadows.insert(name.to_owned());
        }
    }
    shadows
}

fn path_prefix_is_assembled(
    context: &Context,
    root: &Path,
    effective: &wt_core::config::EffectiveScope,
) -> bool {
    let mut expected = Vec::new();
    if !effective.commands.is_empty() {
        expected.push(root.join(".wt/shims").to_string_lossy().into_owned());
    }
    expected.extend(
        effective
            .bin
            .iter()
            .map(|bin| root.join(bin.as_str()).to_string_lossy().into_owned()),
    );
    context
        .parent_env
        .get("PATH")
        .map(|path| {
            path.split(':')
                .take(expected.len())
                .eq(expected.iter().map(String::as_str))
        })
        .unwrap_or(expected.is_empty())
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
        .map(|tree| canonical_observed_path(Path::new(tree.path.as_str())))
        .collect::<BTreeSet<_>>();
    for worktree in listed {
        if registered.contains(&canonical_observed_path(&worktree.path)) {
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

fn canonical_observed_path(path: &Path) -> PathBuf {
    wt_sys::fsx::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn state_orphan_findings(
    context: &Context,
    label_filter: Option<&str>,
    findings: &mut Vec<Finding>,
) -> Result<(), CoreError> {
    for label_dir in wt_sys::fsx::read_dir_paths(&context.home.join("state"))? {
        if label_dir.file_name().and_then(|name| name.to_str()) == Some("_machine.json")
            || !matches!(
                wt_sys::fsx::path_kind(&label_dir)?,
                wt_sys::fsx::PathKind::Directory
            )
        {
            continue;
        }
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

/// Contents of the machine config files named by nudge `used_if_file`
/// rules, keyed by each rule's literal file string. `~/` resolves through
/// the parent environment's HOME; an unreadable file counts as absent.
fn nudge_file_contents(
    contribution: &wt_core::adapters::AdapterContribution,
    parent_env: &wt_core::model::EnvMap,
) -> std::collections::BTreeMap<String, String> {
    let mut contents = std::collections::BTreeMap::new();
    for sniff in contribution
        .nudges
        .iter()
        .flat_map(|nudge| &nudge.used_if_file)
    {
        if contents.contains_key(&sniff.file) {
            continue;
        }
        let path = sniff
            .file
            .strip_prefix("~/")
            .and_then(|rest| {
                parent_env
                    .get("HOME")
                    .map(|home| Path::new(home).join(rest))
            })
            .unwrap_or_else(|| PathBuf::from(&sniff.file));
        if let Ok(text) = std::fs::read_to_string(&path) {
            contents.insert(sniff.file.clone(), text);
        }
    }
    contents
}

fn cache_orphan_findings(
    context: &Context,
    label_filter: Option<&str>,
    findings: &mut Vec<Finding>,
) -> Result<(), CoreError> {
    for path in super::prune::cache_orphans(context, label_filter)? {
        let subject = path
            .strip_prefix(context.cache_root())
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        findings.push(wt_core::doctor::cache_orphan(
            &subject,
            &path.to_string_lossy(),
        ));
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn observed_worktree_paths_compare_by_canonical_identity() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("private/var/tree");
        wt_sys::fsx::create_private_dir(&real).unwrap();
        symlink(root.path().join("private/var"), root.path().join("var")).unwrap();
        let alias = root.path().join("var/tree");
        assert_ne!(alias, real);
        assert_eq!(
            canonical_observed_path(&alias),
            canonical_observed_path(&real)
        );
    }
}
