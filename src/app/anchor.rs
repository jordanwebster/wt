//! The anchor refresh (SPEC §11.10): keep a label's canonical checkout at
//! the default branch's tip and built, so it is worth seeding from (§11.8).

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use wt_core::model::{Label, Target, TreeRec};
use wt_core::report::{AnchorData, MovedReport, Notice, NoticeLevel};
use wt_core::{CoreError, ExitClass};
use wt_sys::lock;

use crate::cli::{AliasRun, Anchor};

use super::{build, Context, Output};

/// How far below a foreground process a detached refresh runs.
const NICE: i32 = 10;

pub(crate) fn run(context: &mut Context, args: Anchor) -> Result<Output, CoreError> {
    let label = Label::new(&args.label)?;
    if !context.registry.labels.contains_key(&label) {
        return Err(CoreError::new(
            ExitClass::NotFound,
            "NOT_FOUND",
            format!("label `{label}` is not registered"),
            "run `wt ls` to see registered labels",
        ));
    }
    let target = Target::canonical(label.clone());
    let tree = context.tree(&target)?;
    context.require_identity(&tree)?;
    let holder = context.holder(target.to_string(), "anchor")?;
    let _anchor = match lock::anchor(&context.anchor_lock_path(&label), &holder) {
        Ok(token) => token,
        Err(error) if error.code.0 == "LOCK_HELD" => {
            return Ok(Output::data(AnchorData {
                label: label.to_string(),
                refreshed: false,
                fetched: None,
                moved: None,
                head: None,
                build: "busy".to_owned(),
                swept: None,
            })?
            .with_notices([Notice {
                level: NoticeLevel::Info,
                code: "ANCHOR_BUSY".to_owned(),
                subject: Some(label.to_string()),
                message: format!("another refresh of {label} is running; nothing was done"),
            }]));
        }
        Err(error) => return Err(error),
    };
    let mut notices = Vec::new();
    let path = Path::new(tree.path.as_str());
    let git = context.git(path)?;
    let default = match context.registry.labels[&label].default_branch.clone() {
        Some(default) => default,
        None => git.default_branch()?,
    };
    let has_origin = git.origin_url()?.is_some();
    let fetched = if args.no_fetch || !has_origin {
        None
    } else {
        let gitdir_id = context.registry.labels[&label].gitdir_id.clone();
        let _git_lock = lock::git(
            &context.git_lock_path(&gitdir_id),
            &holder,
            super::context::duration(
                context.settings.locks.repo_git.as_deref(),
                Duration::from_secs(60),
            ),
        )?;
        match git.fetch_origin_named(&BTreeSet::from([default.clone()])) {
            Ok(()) => Some(true),
            Err(error) => {
                notices.push(Notice {
                    level: NoticeLevel::Warn,
                    code: "FETCH_FAILED".to_owned(),
                    subject: Some(label.to_string()),
                    message: format!(
                        "origin/{default} was not fetched: {}; building what is here",
                        error.message
                    ),
                });
                Some(false)
            }
        }
    };
    let moved = if has_origin {
        move_canonical(context, &git, &tree, &default, &mut notices)?
    } else {
        None
    };
    let head = git.head_oid_in(path)?;
    let config = context.load_config(&tree)?;
    let has_build = context.task_catalog(&tree, &config)?.contains_key("build");
    let mut swept = None;
    let build = if !has_build {
        "none"
    } else if build::is_fresh(context, &tree, &head)? {
        "fresh"
    } else {
        let result = build::run_build(
            context,
            AliasRun {
                target: Some(target.to_string()),
                wait: None,
                timeout: None,
                dry_run: false,
                no_log: false,
                args: Vec::new(),
            },
            Some(build::Slot::for_tree(&tree)),
        );
        match result {
            Ok((output, report)) => {
                swept = report;
                notices.extend(output.notices);
                "ok"
            }
            Err(error) => {
                notices.push(Notice {
                    level: NoticeLevel::Warn,
                    code: "BUILD_FAILED".to_owned(),
                    subject: Some(target.to_string()),
                    message: format!("{}; remedy: {}", error.message, error.remedy),
                });
                "failed"
            }
        }
    };
    Ok(Output::data(AnchorData {
        label: label.to_string(),
        refreshed: true,
        fetched,
        moved,
        head: Some(head),
        build: build.to_owned(),
        swept,
    })?
    .with_notices(notices))
}

/// Moves the canonical to `origin/<default>` when that is safe. A wt-owned
/// canonical (§11.6 `clone`) is detached: the default branch is moved as a
/// ref, fast-forward only, and the checkout re-detached onto it unless a
/// tracked file is modified. A user-owned canonical fast-forwards only
/// while the default branch is checked out in it, no tracked file is
/// modified, and origin is strictly ahead. Anything else leaves it where
/// it is and says why.
fn move_canonical(
    context: &Context,
    git: &wt_sys::git::Git,
    tree: &TreeRec,
    default: &str,
    notices: &mut Vec<Notice>,
) -> Result<Option<MovedReport>, CoreError> {
    let path = Path::new(tree.path.as_str());
    let remote = format!("refs/remotes/origin/{default}");
    if !git.ref_exists(&remote)? {
        return Ok(None);
    }
    let subject = Some(tree.label.to_string());
    let mut stay = |code: &str, message: String| {
        notices.push(Notice {
            level: NoticeLevel::Info,
            code: code.to_owned(),
            subject: subject.clone(),
            message,
        });
    };
    if context.registry.labels[&tree.label].owner == wt_core::model::Owner::Wt {
        let local = format!("refs/heads/{default}");
        let Some(to) = git.resolve_commit(&remote)? else {
            return Ok(None);
        };
        if let Some(local_commit) = git.resolve_commit(&local)? {
            if local_commit != to {
                if !git.is_ancestor(&local_commit, &to)? {
                    stay(
                        "ANCHOR_DIVERGED",
                        format!("local {default} has commits origin/{default} does not; it was not moved"),
                    );
                    return Ok(None);
                }
                if git.branch_holder(default)?.is_some() {
                    stay(
                        "ANCHOR_OFF_DEFAULT",
                        format!("{default} is checked out in a worktree, so the ref was not moved"),
                    );
                    return Ok(None);
                }
                git.update_ref(&local, &to, &local_commit)?;
            }
        }
        let from = git.head_oid_in(path)?;
        if from == to {
            return Ok(None);
        }
        if git.tracked_modified(path)? {
            stay(
                "ANCHOR_DIRTY",
                "the canonical has modified tracked files; it was not moved".to_owned(),
            );
            return Ok(None);
        }
        return if git.checkout_detach(path, &to)? {
            Ok(Some(MovedReport { from, to }))
        } else {
            notices.push(Notice {
                level: NoticeLevel::Warn,
                code: "ANCHOR_NOT_MOVED".to_owned(),
                subject,
                message: format!("git refused to move the canonical to origin/{default}"),
            });
            Ok(None)
        };
    }
    let checked_out = git.head_branch(path)?;
    if checked_out.as_deref() != Some(default) {
        stay(
            "ANCHOR_OFF_DEFAULT",
            format!(
                "the canonical has {} checked out, not {default}; it was not moved",
                checked_out.unwrap_or_else(|| "a detached commit".to_owned())
            ),
        );
        return Ok(None);
    }
    let from = git.head_oid_in(path)?;
    let Some(to) = git.resolve_commit(&remote)? else {
        return Ok(None);
    };
    if from == to {
        return Ok(None);
    }
    if !git.is_ancestor(&from, &to)? {
        stay(
            "ANCHOR_DIVERGED",
            format!(
                "the canonical's {default} has commits origin/{default} does not; it was not moved"
            ),
        );
        return Ok(None);
    }
    if git.tracked_modified(path)? {
        stay(
            "ANCHOR_DIRTY",
            "the canonical has modified tracked files; it was not moved".to_owned(),
        );
        return Ok(None);
    }
    if git.merge_ff_only(path, &remote)? {
        Ok(Some(MovedReport { from, to }))
    } else {
        notices.push(Notice {
            level: NoticeLevel::Warn,
            code: "ANCHOR_NOT_MOVED".to_owned(),
            subject,
            message: format!("git refused to fast-forward the canonical to origin/{default}"),
        });
        Ok(None)
    }
}

/// Whether a canonical that seeds trees lacks a build of its current
/// commit — the condition a refresh exists to end (§11.10). A label whose
/// adapters seed nothing, or that has no build task, is never cold.
pub(crate) fn is_cold(context: &Context, tree: &TreeRec) -> Result<bool, CoreError> {
    let config = context.load_config(tree)?;
    if config.seed.is_empty() || !context.task_catalog(tree, &config)?.contains_key("build") {
        return Ok(false);
    }
    let Some(head) = build::head_of(context, &super::context::target_of(tree)) else {
        return Ok(false);
    };
    Ok(!build::is_fresh(context, tree, &head)?)
}

/// Starts a detached refresh of `tree`'s label after a lifecycle verb, when
/// the label has something to keep warm — adapters that seed — and no
/// refresh is already running. Fire-and-forget: the verb that called has
/// finished, and a refresh that cannot start is a warning, never a failure.
pub(crate) fn spawn_after(context: &Context, tree: &TreeRec, fetch: bool) -> Option<Notice> {
    if tree.canonical {
        return None;
    }
    let label = tree.label.clone();
    let canonical = context
        .registry
        .trees
        .iter()
        .find(|candidate| candidate.canonical && candidate.label == label)?
        .clone();
    if !context.identity_ok(&canonical).unwrap_or(false) {
        return None;
    }
    let seeds = context
        .load_config(&canonical)
        .map(|config| !config.seed.is_empty())
        .unwrap_or(false);
    if !seeds {
        return None;
    }
    if lock::is_held(&context.anchor_lock_path(&label)).unwrap_or(true) {
        return None;
    }
    let mut args = vec!["anchor", label.as_str()];
    if !fetch {
        args.push("--no-fetch");
    }
    let root = Path::new(canonical.path.as_str());
    let log = root.join(".wt/logs/wt-anchor.log");
    match build::spawn_wt(context, root, &args, &[], Some(&log), Some(NICE)) {
        Ok(_) => None,
        Err(error) => Some(Notice {
            level: NoticeLevel::Warn,
            code: "ANCHOR_START_FAILED".to_owned(),
            subject: Some(label.to_string()),
            message: format!(
                "the canonical's refresh did not start: {}; run `wt anchor {label}` by hand",
                error.message
            ),
        }),
    }
}
