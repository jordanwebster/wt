use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wt_core::from_ref::{pr_origin_branch, PullRequestHead};
use wt_core::lifecycle::{Materialized, MaterializedKind, StatePhase, SyncState, VerifyState};
use wt_core::model::{AbsPath, SourceKind, Target, TreeRec, TreeSource};
use wt_core::report::{NewData, NewVerifyReport};
use wt_core::report::{Notice, NoticeLevel};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock::{self, Mode};

use crate::cli::New;

use super::context::target_of;
use super::{door, executor, list, open, register, AfterRender, Context, Output};

struct NewFinish {
    sync: Option<Vec<wt_core::report::StepReport>>,
    verify: Option<NewVerifyReport>,
    notices: Vec<Notice>,
}

pub(crate) fn run(context: &mut Context, args: New) -> Result<Output, CoreError> {
    let meta = super::meta::parse_creation(&args.meta)?;
    let backend_notice = register::resolve_session_backend(context)?;
    let target = args.target.clone();
    let no_open = args.no_open;
    let no_attach = args.no_attach;
    let no_build = args.no_build;
    let mut output = create_tree(context, args, meta)?;
    if let Some(notice) = backend_notice {
        output = output.with_notices([notice]);
    }
    if no_open {
        return Ok(output);
    }
    let build = !no_build && has_build_task(context, &target)?;
    if open::should_attach(context, no_attach) {
        return Ok(output.after_render(AfterRender::NewSession { target, build }));
    }
    match open::provision_new(context, &target) {
        // Creation already carries the same door notice set. Session
        // provisioning enters that door again only to launch tmux.
        Ok(_) => {}
        Err(error) => {
            output = output.with_notices([open::session_failure_notice(&target, &error)]);
            return Ok(output);
        }
    }
    if context.settings.session.backend == wt_core::settings::SessionBackend::Tmux {
        let resolved = context.resolve(Some(&target))?;
        let tree = context.tree(&resolved)?;
        output.data["tree"]["session"] = serde_json::Value::String("yes".to_owned());
        output.data["tree"]["agent"] = tree
            .agent
            .map_or(serde_json::Value::Null, serde_json::Value::String);
    }
    if build {
        output = output.after_render(AfterRender::Build { target });
    }
    Ok(output)
}

fn has_build_task(context: &Context, target: &str) -> Result<bool, CoreError> {
    let target = context.resolve(Some(target))?;
    let tree = context.tree(&target)?;
    let config = context.load_config(&tree)?;
    Ok(context.task_catalog(&tree, &config)?.contains_key("build"))
}

fn create_tree(
    context: &mut Context,
    args: New,
    meta: BTreeMap<String, String>,
) -> Result<Output, CoreError> {
    let target = Target::parse(&args.target)?;
    if target.name == "canonical" {
        return Err(CoreError::new(
            ExitClass::Usage,
            "NAME_REQUIRED",
            "new requires <label>/<name>",
            "choose a non-canonical tree name",
        ));
    }
    if context
        .registry
        .labels
        .contains_key(&wt_core::model::Label::new(&target.name)?)
    {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "NAME_SHADOWS_LABEL",
            "tree name shadows a registered label",
            "choose another tree name",
        ));
    }
    let label = context
        .registry
        .labels
        .get(&target.label)
        .cloned()
        .ok_or_else(|| {
            CoreError::new(
                ExitClass::NotFound,
                "NOT_FOUND",
                format!("label {} is not registered", target.label),
                "run `wt register` first",
            )
        })?;
    let canonical = context
        .registry
        .trees
        .iter()
        .find(|tree| tree.label == target.label && tree.canonical)
        .cloned()
        .ok_or_else(|| super::context::not_found(&Target::canonical(target.label.clone())))?;
    let existing = context
        .registry
        .trees
        .iter()
        .find(|tree| tree.label == target.label && tree.name == target.name)
        .cloned();
    // An existing entry's branch was decided when it was created and is not
    // re-derived: a resume keeps its recorded metadata too, so a convention
    // whose inputs changed since — a `wt meta` edit, an edited template —
    // must not turn a bare re-run into a different source. Only `--branch`
    // and a different pull request state a branch for an address that
    // already exists; the same pull request keeps its recorded branch, so
    // an offline re-run does not have to ask the forge again.
    let requested_pr = parse_pr(&args.from_ref);
    let same_pr = existing
        .as_ref()
        .is_some_and(|existing| requested_pr.is_some() && existing.source.pr == requested_pr);
    // The forge is asked only when its answer decides something: an
    // address that already records this pull request keeps its branch,
    // and a re-run must not fail on a missing or logged-out `gh`.
    let head = if same_pr {
        Head::Deferred
    } else {
        Head::Known(pull_request_head(context, &canonical, &args)?)
    };
    let requested_branch = match &existing {
        Some(existing) if args.branch.is_none() && (requested_pr.is_none() || same_pr) => existing
            .source
            .branch
            .clone()
            .unwrap_or_else(|| target.name.clone()),
        _ => creation_branch(context, &canonical, &target, &args, &meta, head.known())?,
    };
    let requested_start = args.from_ref.clone();
    if let Some(existing) = existing {
        let state = context.read_state(&target)?;
        let phase = context.phase(&existing, state.as_ref())?;
        let view = wt_core::new::EntryView {
            phase,
            source_identical: existing.source.branch.as_deref() == Some(&requested_branch)
                && (requested_start.is_none() || existing.source.start == requested_start),
            verify_pending: state.as_ref().is_some_and(|state| state.verify_pending),
            has_resource_records: state
                .as_ref()
                .is_some_and(|state| !state.resources.is_empty()),
        };
        let decision = wt_core::new::decide(
            Some(&view),
            &wt_core::new::Request {
                verify: args.verify,
                name_shadows_label: false,
                path_occupied: false,
            },
        );
        match decision {
            wt_core::new::Decision::AlreadyReady => {
                return ready_report(context, existing, false, false, None, None, Vec::new())
            }
            wt_core::new::Decision::Verify { resumed } => {
                return verify_ready(context, existing, args, resumed)
            }
            wt_core::new::Decision::FreshIncarnation { .. } => {
                return fresh_incarnation(context, existing, canonical, args, meta, head)
            }
            wt_core::new::Decision::Resume { start } => {
                return resume(context, existing, canonical, args, start, head)
            }
            wt_core::new::Decision::Refuse { code, remedy } => {
                return Err(CoreError::new(
                    ExitClass::Conflict,
                    code,
                    format!("cannot create {target} in phase {phase:?}"),
                    remedy,
                ))
            }
            wt_core::new::Decision::Allocate { .. } => unreachable!("existing entry was supplied"),
        }
    }

    let path = tree_path(context, &label, &target);
    let path_occupied = !matches!(
        wt_sys::fsx::path_kind(&path)?,
        wt_sys::fsx::PathKind::Missing
    );
    if let wt_core::new::Decision::Refuse { code, remedy } = wt_core::new::decide(
        None,
        &wt_core::new::Request {
            verify: args.verify,
            name_shadows_label: false,
            path_occupied,
        },
    ) {
        return Err(CoreError::new(
            ExitClass::State,
            code,
            format!("tree path {} is occupied", path.display()),
            remedy,
        ));
    }
    create(
        context,
        target,
        path,
        canonical,
        args,
        meta,
        requested_branch,
        head,
    )
}

#[allow(clippy::too_many_arguments)]
fn create(
    context: &mut Context,
    target: Target,
    path: PathBuf,
    canonical: TreeRec,
    args: New,
    meta: BTreeMap<String, String>,
    branch: String,
    head: Head,
) -> Result<Output, CoreError> {
    let holder = context.holder(target.to_string(), "new")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    let initial =
        register::initial_config(context, Path::new(canonical.path.as_str()), &target.label)?;
    let coordinates = context.allocate(&target, &initial.ports)?;
    let now = wt_sys::fsx::timestamp()?;
    let tree_id = wt_sys::fsx::random_tree_id()?;
    let state = register::new_state(&target, &tree_id, StatePhase::Bootstrapping, "new", &now)?;
    context.write_state(&target, &state, &holder)?;
    let pr = parse_pr(&args.from_ref);
    let tree = TreeRec {
        tree_id: tree_id.clone(),
        label: target.label.clone(),
        name: target.name.clone(),
        canonical: false,
        path: AbsPath::new(path.to_string_lossy().into_owned())?,
        slot: coordinates.slot,
        geometry: coordinates.geometry,
        ports: coordinates.ports,
        created_at: now,
        agent: None,
        meta,
        source: TreeSource {
            kind: if pr.is_some() {
                SourceKind::Pr
            } else if args.detach {
                SourceKind::Ref
            } else {
                SourceKind::Branch
            },
            branch: (!args.detach).then(|| branch.clone()),
            pr,
            start: args.from_ref.clone(),
        },
    };
    context.mutate_registry(&holder, |registry| {
        registry
            .tombstones
            .retain(|record| record.label != target.label || record.name != target.name);
        registry.trees.push(tree.clone());
        Ok(())
    })?;
    let notice = add_worktree(context, &canonical, &tree, &args, head)?;
    let finished = finish_under_lock(
        context,
        &tree,
        &canonical,
        &initial,
        &args,
        &holder,
        wt_core::new::StartAt::Git,
    )?;
    drop(token);
    ready_report(
        context,
        tree,
        true,
        false,
        finished.sync,
        finished.verify,
        source_notices(notice, finished.notices),
    )
}

fn resume(
    context: &mut Context,
    tree: TreeRec,
    canonical: TreeRec,
    args: New,
    start: wt_core::new::StartAt,
    head: Head,
) -> Result<Output, CoreError> {
    let target = super::context::target_of(&tree);
    let holder = context.holder(target.to_string(), "new")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    if start == wt_core::new::StartAt::State {
        let state = register::new_state(
            &target,
            &tree.tree_id,
            StatePhase::Bootstrapping,
            "new",
            &wt_sys::fsx::timestamp()?,
        )?;
        context.write_state(&target, &state, &holder)?;
    }
    let initial =
        register::initial_config(context, Path::new(canonical.path.as_str()), &tree.label)?;
    if matches!(
        wt_sys::fsx::path_kind(Path::new(tree.path.as_str()))?,
        wt_sys::fsx::PathKind::Missing
    ) {
        let notice = add_worktree(context, &canonical, &tree, &args, head)?;
        let finished =
            finish_under_lock(context, &tree, &canonical, &initial, &args, &holder, start)?;
        drop(token);
        return ready_report(
            context,
            tree,
            false,
            true,
            finished.sync,
            finished.verify,
            source_notices(notice, finished.notices),
        );
    }
    let finished = finish_under_lock(context, &tree, &canonical, &initial, &args, &holder, start)?;
    drop(token);
    ready_report(
        context,
        tree,
        false,
        true,
        finished.sync,
        finished.verify,
        finished.notices,
    )
}

fn add_worktree(
    context: &mut Context,
    canonical: &TreeRec,
    tree: &TreeRec,
    args: &New,
    head: Head,
) -> Result<Vec<Notice>, CoreError> {
    let git = context.git(Path::new(canonical.path.as_str()))?;
    let target = super::context::target_of(tree);
    let holder = context.holder(target.to_string(), "new")?;
    let _git_lock = lock::git(
        &context.git_lock_path(&context.registry.labels[&tree.label].gitdir_id),
        &holder,
        super::context::duration(
            context.settings.locks.repo_git.as_deref(),
            std::time::Duration::from_secs(60),
        ),
    )?;
    let origin = git.origin_url()?;
    let pr = requested_pull_request(context, canonical, args)?;
    // The branch this incarnation was created with, recorded when it was
    // allocated and unchanged by a later configuration edit. Only a detached
    // tree records none, and its name feeds the porcelain check alone.
    let branch = match tree.source.branch.clone() {
        Some(branch) => branch,
        None => creation_branch(context, canonical, &target, args, &tree.meta, head.known())?,
    };
    if let Some(path) = git.branch_holder(&branch)? {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "BRANCH_IN_USE",
            format!("branch `{branch}` is checked out at {}", path.display()),
            "choose another branch or remove the worktree that holds it",
        ));
    }
    let branch_exists = git.ref_exists(&format!("refs/heads/{branch}"))?;
    // A branch that already exists locally is checked out as it is, so it
    // needs no start; a pull request whose recorded branch survived needs
    // no forge question either.
    let reuse_local = branch_exists && !args.detach;
    let (head, asked) = match head {
        Head::Known(head) => (head, true),
        Head::Deferred if reuse_local => (None, false),
        Head::Deferred => (pull_request_head(context, canonical, args)?, true),
    };
    // A pull request whose head is a branch of origin is checked out as
    // that branch, tracking it, so a push updates the pull request. The
    // forge answers for the branch name; git's own PR refs carry only the
    // commits. Anything else — a fork, another forge, `--no-fetch` — falls
    // back to mirroring the PR ref as `pr/N`, and says so.
    let tracked_branch = pr_origin_branch(head.as_ref()).map(str::to_owned);
    // The default branch resolves through origin/HEAD and local refs, none
    // of which a fetch changes, so one discovery serves start resolution,
    // the fetch refspec, and the registry cache refresh below. The cache
    // substitutes for discovery only under --no-fetch, as before.
    let mut discovered = None;
    let default = match context.registry.labels[&tree.label]
        .default_branch
        .clone()
        .filter(|_| args.no_fetch)
    {
        Some(default) => default,
        None => {
            let value = git.default_branch()?;
            discovered = Some(value.clone());
            value
        }
    };
    if origin.is_some() && !args.no_fetch {
        // Fetch only the branches this creation consumes: the requested
        // start when it can name an origin branch, and the default branch
        // the tree will be measured against. A narrow fetch fails when a
        // wanted name is not a branch on the remote (a tag, a raw
        // revision, a renamed default); the wildcard fetch then restores
        // the resolve-anything behaviour at the old cost.
        let mut wanted = std::collections::BTreeSet::from([default.clone()]);
        if let Some(branch) = &tracked_branch {
            wanted.insert(branch.clone());
        } else if let Some(input) = args.from_ref.as_deref() {
            if pr.is_none() {
                wanted.insert(input.strip_prefix("origin/").unwrap_or(input).to_owned());
            }
        }
        match git.fetch_origin_named(&wanted) {
            Ok(()) => {}
            Err(error) if error.code.0 == "TIMEOUT" => return Err(error),
            // When the fallback fails too, report the narrow error: it
            // names the refs this creation actually asked for.
            Err(error) => {
                if git.fetch_origin_branches().is_err() {
                    return Err(error);
                }
            }
        }
    }
    let mut notices = Vec::new();
    let start = if let Some(pr) = &pr {
        if reuse_local {
            format!("refs/heads/{branch}")
        } else if let Some(head_branch) = &tracked_branch {
            let reference = format!("refs/remotes/origin/{head_branch}");
            if !git.ref_exists(&reference)? {
                return Err(CoreError::new(
                    ExitClass::NotFound,
                    "NOT_FOUND",
                    format!(
                        "pull request {} is branch `{head_branch}`, which origin does not have",
                        pr.number
                    ),
                    "verify the pull request is still open and its branch was not deleted",
                ));
            }
            reference
        } else {
            if !args.no_fetch {
                git.fetch_pull_request(&pr.host, pr.number)?;
            }
            ensure_pr_ref(&git, pr.number)?;
            format!("refs/wt/pr/{}", pr.number)
        }
    } else if let Some(input) = args.from_ref.as_deref() {
        match resolve_from(&git, input)? {
            wt_core::from_ref::Resolution::Local { reference, notice } => {
                notices.extend(notice.map(|code| Notice {
                    level: NoticeLevel::Warn,
                    code: code.to_owned(),
                    subject: Some(target.to_string()),
                    message:
                        "local branch takes precedence over a different origin branch".to_owned(),
                }));
                reference
            }
            wt_core::from_ref::Resolution::Remote { reference }
            | wt_core::from_ref::Resolution::Revision { reference } => reference,
            wt_core::from_ref::Resolution::PullRequest { .. }
            | wt_core::from_ref::Resolution::PullRequestUrl { .. } => {
                unreachable!("pull requests resolve before plain references")
            }
        }
    } else if git.ref_exists(&format!("refs/remotes/origin/{default}"))? {
        format!("refs/remotes/origin/{default}")
    } else {
        format!("refs/heads/{default}")
    };
    let default = match discovered {
        Some(value) => value,
        None => git.default_branch()?,
    };
    if context.registry.labels[&tree.label]
        .default_branch
        .as_deref()
        != Some(&default)
    {
        context.mutate_registry(&holder, |registry| {
            registry
                .labels
                .get_mut(&tree.label)
                .ok_or_else(|| super::context::not_found(&Target::canonical(tree.label.clone())))?
                .default_branch = Some(default);
            Ok(())
        })?;
    }
    let spec = wt_core::from_ref::add_spec(
        &branch,
        &start,
        args.detach,
        branch_exists,
        false,
        start.starts_with("refs/remotes/"),
    )?;
    git.worktree_add(Path::new(tree.path.as_str()), &spec)?;
    // Wt-created worktrees get git's own exact status caches. Best-effort:
    // a tree is never lost to a failed optimisation, and doctor reports
    // trees whose caches are off.
    let _ = git.accelerate_status(Path::new(tree.path.as_str()));
    if let Some(pr) = &pr {
        // The affirmative notice needs both halves: the forge named this
        // very branch as the pull request's, and git confirms the worktree
        // tracks it. A branch that already existed locally, `--branch`
        // naming an unrelated branch that tracks its own origin twin, or
        // `--detach` may satisfy neither or only one, and the notice must
        // not promise a push it cannot deliver. A recorded branch re-added
        // without asking the forge is neither praised nor warned about
        // while it tracks its origin twin.
        let upstream = if args.detach {
            None
        } else {
            git.upstream(&branch)?
        };
        let self_tracking = upstream.as_deref() == Some(&format!("origin/{branch}"));
        if self_tracking && tracked_branch.as_deref() == Some(branch.as_str()) {
            notices.push(Notice {
                level: NoticeLevel::Info,
                code: "PR_BRANCH_TRACKED".to_owned(),
                subject: Some(target.to_string()),
                message: format!(
                    "branch `{branch}` tracks origin/{branch}, so `git push` updates pull request {}",
                    pr.number
                ),
            });
        } else if asked || !self_tracking {
            notices.push(untracked_pr_notice(
                &target,
                &branch,
                upstream.as_deref(),
                pr,
                head.as_ref(),
                args,
            ));
        }
        if reuse_local && tracked_branch.as_deref() == Some(branch.as_str()) {
            let local = git.resolve_commit(&format!("refs/heads/{branch}"))?;
            let remote = git.resolve_commit(&format!("refs/remotes/origin/{branch}"))?;
            if remote.is_some() && local != remote {
                notices.push(Notice {
                    level: NoticeLevel::Warn,
                    code: "FROM_LOCAL_SHADOWS_REMOTE".to_owned(),
                    subject: Some(target.to_string()),
                    message: format!(
                        "local branch `{branch}` is not at origin/{branch}, and the worktree starts from the local branch"
                    ),
                });
            }
        }
    }
    Ok(notices)
}

fn resolve_from(
    git: &wt_sys::git::Git,
    input: &str,
) -> Result<wt_core::from_ref::Resolution, CoreError> {
    let local = format!("refs/heads/{input}");
    let origin = input.strip_prefix("origin/").map_or_else(
        || format!("refs/remotes/origin/{input}"),
        |branch| format!("refs/remotes/origin/{branch}"),
    );
    let local_exists = !input.starts_with("origin/") && git.ref_exists(&local)?;
    let origin_exists = git.ref_exists(&origin)?;
    let candidates = wt_core::from_ref::RefCandidates {
        local: local_exists.then_some(local.clone()),
        local_oid: if local_exists {
            git.resolve_commit(&local)?
        } else {
            None
        },
        origin: origin_exists.then_some(origin.clone()),
        origin_oid: if origin_exists {
            git.resolve_commit(&origin)?
        } else {
            None
        },
        revision: git.resolve_commit(input)?.map(|_| input.to_owned()),
    };
    wt_core::from_ref::decide(input, &candidates)
}

fn ensure_pr_ref(git: &wt_sys::git::Git, number: u64) -> Result<(), CoreError> {
    let reference = format!("refs/wt/pr/{number}");
    if git.ref_exists(&reference)? {
        Ok(())
    } else {
        Err(CoreError::new(
            ExitClass::NotFound,
            "NOT_FOUND",
            format!("pull request {number} is not available locally"),
            "retry without --no-fetch or verify the pull request number",
        ))
    }
}

fn ensure_pr_url_matches(
    context: &Context,
    tree: &TreeRec,
    host: &str,
    owner: &str,
    repo: &str,
) -> Result<(), CoreError> {
    let wanted = (host.to_ascii_lowercase(), format!("{owner}/{repo}"));
    let mut matches = Vec::new();
    for (label, record) in &context.registry.labels {
        let git = context.git(Path::new(record.path.as_str()))?;
        if git
            .origin_url()?
            .as_deref()
            .and_then(wt_core::from_ref::normalize_url)
            .is_some_and(|value| (value.0.to_ascii_lowercase(), value.1) == wanted)
        {
            matches.push(label.clone());
        }
    }
    if matches.len() == 1 && matches[0] == tree.label {
        Ok(())
    } else {
        Err(CoreError::new(
            ExitClass::State,
            "PR_REPO_UNRESOLVED",
            format!(
                "pull request URL matches {} registered labels",
                matches.len()
            ),
            "register exactly one checkout whose origin matches the pull request URL",
        ))
    }
}

/// The branch a creation targets when it is not resumed from a record:
/// `--branch`, then the pull request's own branch (or its `pr/N` mirror),
/// then the label's declared convention, then the tree's name (A77).
fn creation_branch(
    context: &Context,
    canonical: &TreeRec,
    target: &Target,
    args: &New,
    meta: &BTreeMap<String, String>,
    head: Option<&PullRequestHead>,
) -> Result<String, CoreError> {
    if let Some(branch) = args.branch.clone() {
        return Ok(branch);
    }
    if args.detach {
        // No branch is created, so neither the pull request's branch nor
        // the convention has anything to name here; the value only feeds
        // step G's branch-held-elsewhere check, and a detached checkout
        // of a pull request must not be refused because another worktree
        // holds the pull request's branch.
        return Ok(target.name.clone());
    }
    if let Some(number) = parse_pr(&args.from_ref) {
        return Ok(pr_origin_branch(head).map_or_else(|| format!("pr/{number}"), str::to_owned));
    }
    let declared =
        register::declared_branch(context, Path::new(canonical.path.as_str()), &target.label)?;
    let candidates = declared
        .as_ref()
        .map_or(&[][..], |declared| declared.candidates());
    Ok(
        wt_core::new::branch_from_templates(candidates, target.label.as_str(), &target.name, meta)?
            .unwrap_or_else(|| target.name.clone()),
    )
}

fn parse_pr(input: &Option<String>) -> Option<u64> {
    let input = input.as_deref()?;
    match wt_core::from_ref::decide(input, &empty_candidates()).ok()? {
        wt_core::from_ref::Resolution::PullRequest { number }
        | wt_core::from_ref::Resolution::PullRequestUrl { number, .. } => Some(number),
        _ => None,
    }
}

fn finish_under_lock(
    context: &mut Context,
    tree: &TreeRec,
    canonical: &TreeRec,
    config: &wt_core::config::Config,
    args: &New,
    holder: &wt_sys::lock::Holder,
    start: wt_core::new::StartAt,
) -> Result<NewFinish, CoreError> {
    let root = Path::new(tree.path.as_str());
    wt_sys::fsx::write_nofollow(
        root,
        &wt_core::model::RelPath::new(".wt/tree_id")?,
        format!("{}\n", tree.tree_id).as_bytes(),
        0o600,
    )?;
    let mut notices = Vec::new();
    let mut copied = Vec::new();
    if start != wt_core::new::StartAt::Bootstrap {
        let entries = config.root.copy.iter().collect::<Vec<_>>();
        let tracked = context
            .git(Path::new(canonical.path.as_str()))?
            .tracked_paths(
                Path::new(canonical.path.as_str()),
                &entries
                    .iter()
                    .map(|path| PathBuf::from(path.as_str()))
                    .collect::<Vec<_>>(),
            )?;
        for path in entries {
            let subject = Some(format!("{}:{}", target_of(tree), path));
            if matches!(
                wt_sys::fsx::path_kind(&Path::new(canonical.path.as_str()).join(path.as_str()))?,
                wt_sys::fsx::PathKind::Missing
            ) {
                notices.push(Notice {
                    level: NoticeLevel::Info,
                    code: "COPY_ABSENT".to_owned(),
                    subject,
                    message: format!("copy source {path} is absent"),
                });
                continue;
            }
            if tracked.contains(&PathBuf::from(path.as_str())) {
                wt_sys::fsx::remove_path(&context.state_path(&target_of(tree)))?;
                return Err(CoreError::new(
                    ExitClass::State,
                    "COPY_TRACKED",
                    format!("copy source `{path}` is tracked by git"),
                    "remove it from copy or stop tracking it before retrying",
                ));
            }
            if !matches!(
                wt_sys::fsx::path_kind(&root.join(path.as_str()))?,
                wt_sys::fsx::PathKind::Missing
            ) {
                notices.push(Notice {
                    level: NoticeLevel::Info,
                    code: "COPY_EXISTS".to_owned(),
                    subject,
                    message: format!("copy destination {path} already exists"),
                });
                continue;
            }
            wt_sys::fsx::copy_contained(Path::new(canonical.path.as_str()), root, path)?;
            copied.push(Materialized {
                path: path.to_string(),
                kind: MaterializedKind::Copied,
                hash: None,
                tracked_checked_at: wt_sys::fsx::timestamp()?,
            });
        }
    }
    let target = super::context::target_of(tree);
    context.mutate_state(&target, holder, |state| {
        state.materialized.extend(copied);
        state.phase = StatePhase::Bootstrapping;
        Ok(())
    })?;
    door::recompute_exclude(context, &tree.label)?;
    let door = match door::enter_held(context, tree.clone(), "new", true) {
        Ok(door) => door,
        Err(error) => {
            mark_new_failed(context, &target, holder, &error)?;
            return Err(error);
        }
    };
    notices.extend(door.notices.clone());
    if let Err(error) = executor::refresh_all_declarations(context, &door) {
        mark_new_failed(context, &target, holder, &error)?;
        return Err(error);
    }
    let sync = if args.no_sync {
        None
    } else if let Ok(plan) = executor::plan(context, &door, "sync") {
        let execution = match executor::execute_plan(
            context,
            &door,
            &plan,
            None,
            executor::ExecuteOptions::DEFAULT,
        ) {
            Ok(execution) => execution,
            Err(error) => {
                mark_new_failed(context, &target, holder, &error)?;
                return Err(error);
            }
        };
        notices.extend(execution.notices);
        let paths = door
            .config
            .sync_inputs
            .iter()
            .map(|path| PathBuf::from(path.as_str()))
            .collect::<Vec<_>>();
        let hashes = context
            .git(Path::new(door.tree.path.as_str()))?
            .hash_object(&paths)?;
        let inputs = paths
            .iter()
            .zip(hashes)
            .map(|(path, hash)| (path.to_string_lossy().into_owned(), hash))
            .collect();
        let holder = context.holder(target.to_string(), "new")?;
        context.mutate_state(&target, &holder, |state| {
            state.sync = Some(SyncState {
                at: wt_sys::fsx::timestamp()?,
                ok: true,
                inputs,
                log: None,
            });
            Ok(())
        })?;
        Some(execution.data.steps)
    } else {
        Some(Vec::new())
    };
    context.mutate_state(&target, holder, |state| {
        state.phase = StatePhase::Ready;
        state.op = None;
        state.verify_pending = args.verify;
        state.last_error = None;
        Ok(())
    })?;
    let verify = if args.verify {
        Some(run_verify(context, &door, holder, &mut notices)?)
    } else {
        None
    };
    Ok(NewFinish {
        sync,
        verify,
        notices,
    })
}

fn mark_new_failed(
    context: &Context,
    target: &Target,
    holder: &wt_sys::lock::Holder,
    error: &CoreError,
) -> Result<(), CoreError> {
    context.mutate_state(target, holder, |state| {
        state.phase = StatePhase::Failed;
        state.op = None;
        state.last_error = Some(error.to_string());
        Ok(())
    })
}

fn run_verify(
    context: &mut Context,
    door: &door::Door,
    holder: &wt_sys::lock::Holder,
    notices: &mut Vec<Notice>,
) -> Result<NewVerifyReport, CoreError> {
    let target = door.target.clone();
    let result = executor::plan(context, door, "verify").and_then(|plan| {
        executor::execute_plan(
            context,
            door,
            &plan,
            None,
            executor::ExecuteOptions::DEFAULT,
        )
    });
    match result {
        Ok(execution) => {
            notices.extend(execution.notices);
            context.mutate_state(&target, holder, |state| {
                state.verify = Some(VerifyState {
                    at: wt_sys::fsx::timestamp()?,
                    ok: true,
                    log: execution.data.log.clone(),
                });
                state.verify_pending = false;
                Ok(())
            })?;
            Ok(NewVerifyReport {
                ok: true,
                steps: execution.data.steps,
            })
        }
        Err(error) => {
            context.mutate_state(&target, holder, |state| {
                state.verify = Some(VerifyState {
                    at: wt_sys::fsx::timestamp()?,
                    ok: false,
                    log: None,
                });
                state.verify_pending = false;
                state.phase = StatePhase::Ready;
                state.op = None;
                state.last_error = Some(error.to_string());
                Ok(())
            })?;
            Err(CoreError::new(
                ExitClass::ChildFailed,
                "VERIFY_FAILED",
                format!("verification failed: {}", error.message),
                "fix the verify task and run `wt new --verify` again",
            )
            .with_details(error.details))
        }
    }
}

fn verify_ready(
    context: &mut Context,
    tree: TreeRec,
    _args: New,
    resumed: bool,
) -> Result<Output, CoreError> {
    let target = target_of(&tree);
    let holder = context.holder(target.to_string(), "new")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    context.require_identity(&tree)?;
    context.mutate_state(&target, &holder, |state| {
        state.verify_pending = true;
        Ok(())
    })?;
    let door = door::enter_held(context, tree.clone(), "new", true)?;
    let mut notices = door.notices.clone();
    let verify = run_verify(context, &door, &holder, &mut notices)?;
    drop(token);
    ready_report(context, tree, false, resumed, None, Some(verify), notices)
}

fn fresh_incarnation(
    context: &mut Context,
    mut tree: TreeRec,
    canonical: TreeRec,
    args: New,
    meta: BTreeMap<String, String>,
    head: Head,
) -> Result<Output, CoreError> {
    let target = target_of(&tree);
    let holder = context.holder(target.to_string(), "new")?;
    let token = lock::tree(
        &context.tree_lock_path(&target),
        Mode::Exclusive,
        &holder,
        context.tree_wait(None),
    )?;
    let tree_id = wt_sys::fsx::random_tree_id()?;
    let now = wt_sys::fsx::timestamp()?;
    let state = register::new_state(&target, &tree_id, StatePhase::Bootstrapping, "new", &now)?;
    context.write_state(&target, &state, &holder)?;
    tree.tree_id = tree_id;
    tree.created_at = now;
    tree.meta = meta;
    context.mutate_registry(&holder, |registry| {
        let record = registry
            .trees
            .iter_mut()
            .find(|record| record.label == target.label && record.name == target.name)
            .ok_or_else(|| super::context::not_found(&target))?;
        *record = tree.clone();
        Ok(())
    })?;
    let source_notice = add_worktree(context, &canonical, &tree, &args, head)?;
    let initial =
        register::initial_config(context, Path::new(canonical.path.as_str()), &tree.label)?;
    let finished = finish_under_lock(
        context,
        &tree,
        &canonical,
        &initial,
        &args,
        &holder,
        wt_core::new::StartAt::Git,
    )?;
    drop(token);
    ready_report(
        context,
        tree,
        true,
        false,
        finished.sync,
        finished.verify,
        source_notices(source_notice, finished.notices),
    )
}

fn source_notices(source: Vec<Notice>, mut notices: Vec<Notice>) -> Vec<Notice> {
    notices.extend(source);
    notices
}

/// Whether the forge has been asked about the requested pull request.
enum Head {
    /// Not asked: the address already records this pull request, and the
    /// worktree add asks only if it needs a start the record cannot give.
    Deferred,
    /// Asked, or not applicable; `None` when the creation is not a
    /// GitHub pull request or runs under `--no-fetch`.
    Known(Option<PullRequestHead>),
}

impl Head {
    fn known(&self) -> Option<&PullRequestHead> {
        match self {
            Self::Deferred => None,
            Self::Known(head) => head.as_ref(),
        }
    }
}

/// A requested pull request and the forge host it lives on.
struct PullRequest {
    number: u64,
    host: String,
}

/// The pull request `--from` names, if any. A pull request URL is matched
/// against the registered origins first, so the question goes to the
/// repository the URL names rather than to whichever checkout the address
/// happens to live in.
fn requested_pull_request(
    context: &Context,
    canonical: &TreeRec,
    args: &New,
) -> Result<Option<PullRequest>, CoreError> {
    let Some(number) = parse_pr(&args.from_ref) else {
        return Ok(None);
    };
    let host = match wt_core::from_ref::decide(
        args.from_ref.as_deref().unwrap_or_default(),
        &empty_candidates(),
    ) {
        Ok(wt_core::from_ref::Resolution::PullRequestUrl {
            host, owner, repo, ..
        }) => {
            ensure_pr_url_matches(context, canonical, &host, &owner, &repo)?;
            host
        }
        _ => context
            .git(Path::new(canonical.path.as_str()))?
            .origin_url()?
            .as_deref()
            .and_then(wt_core::from_ref::normalize_url)
            .map(|value| value.0)
            .unwrap_or_else(|| "unknown".to_owned()),
    };
    Ok(Some(PullRequest { number, host }))
}

/// Asks the forge which branch a requested pull request was opened from.
/// Only GitHub is asked today; `--no-fetch` keeps the creation offline.
/// A pull request URL is first matched against the registered origins, so
/// the question goes to the repository the URL names rather than to
/// whichever checkout the address happens to live in.
fn pull_request_head(
    context: &Context,
    canonical: &TreeRec,
    args: &New,
) -> Result<Option<PullRequestHead>, CoreError> {
    let Some(pr) = requested_pull_request(context, canonical, args)? else {
        return Ok(None);
    };
    if args.no_fetch || wt_core::from_ref::forge_of(&pr.host) != wt_core::from_ref::Forge::GitHub {
        return Ok(None);
    }
    let timeout = wt_sys::git::Deadlines::from_settings(&context.settings.git.timeouts)?.fetch;
    wt_sys::forge::github_pull_request_head(Path::new(canonical.path.as_str()), pr.number, timeout)
        .map(Some)
}

/// Why a pull request creation mirrors `pr/N` instead of tracking the
/// pull request's own branch, and what would get the tracked branch.
fn untracked_pr_notice(
    target: &Target,
    branch: &str,
    upstream: Option<&str>,
    pr: &PullRequest,
    head: Option<&PullRequestHead>,
    args: &New,
) -> Notice {
    let number = pr.number;
    let mirror = if args.detach {
        "this detached worktree".to_owned()
    } else {
        format!("branch {branch}")
    };
    let consequence = match upstream {
        Some(upstream) => format!(
            "{mirror} tracks {upstream}, which is not pull request {number}'s branch, so a plain `git push` updates {upstream} instead of the pull request"
        ),
        None => format!(
            "{mirror} mirrors pull request {number} but tracks nothing, so a plain `git push` creates a new branch on origin instead of updating the pull request"
        ),
    };
    let action = match head {
        Some(head) if head.cross_repository => match &head.owner {
            Some(owner) => format!(
                "the pull request was opened from {owner}'s fork; to update it, push to that fork's `{}` branch",
                head.branch
            ),
            None => format!(
                "the pull request was opened from a fork; to update it, push to the fork's `{}` branch",
                head.branch
            ),
        },
        Some(head) => format!(
            "the pull request is branch `{}`; to update it, push there: `git push origin HEAD:{}`",
            head.branch, head.branch
        ),
        None if args.no_fetch => {
            "retry without --no-fetch to check out the pull request's own branch".to_owned()
        }
        None if wt_core::from_ref::forge_of(&pr.host) == wt_core::from_ref::Forge::GitHub => {
            "set the branch's upstream to the pull request's branch on origin to push to it"
                .to_owned()
        }
        None => format!(
            "wt asks only GitHub for a pull request's branch, not {}; pass `--from origin/<branch>` to work on the pull request's own branch",
            pr.host
        ),
    };
    Notice {
        level: NoticeLevel::Warn,
        code: "PR_BRANCH_UNTRACKED".to_owned(),
        subject: Some(target.to_string()),
        message: format!("{consequence}; {action}"),
    }
}

fn empty_candidates() -> wt_core::from_ref::RefCandidates {
    wt_core::from_ref::RefCandidates {
        local: None,
        local_oid: None,
        origin: None,
        origin_oid: None,
        revision: None,
    }
}

fn ready_report(
    context: &mut Context,
    tree: TreeRec,
    created: bool,
    resumed: bool,
    sync: Option<Vec<wt_core::report::StepReport>>,
    verify: Option<NewVerifyReport>,
    notices: Vec<Notice>,
) -> Result<Output, CoreError> {
    context.reload_registry()?;
    Ok(Output::data(NewData {
        tree: list::tree_report(context, &tree, false, false, false)?,
        created,
        resumed,
        sync,
        verify,
        build: None,
    })?
    .with_notices(notices))
}

fn tree_path(context: &Context, label: &wt_core::model::LabelRec, target: &Target) -> PathBuf {
    label
        .trees_dir
        .as_ref()
        .map(|path| PathBuf::from(path.as_str()))
        .or_else(|| context.settings.trees_dir.as_ref().map(PathBuf::from))
        .unwrap_or_else(|| context.home.join("trees"))
        .join(target.label.as_str())
        .join(&target.name)
}
