//! `wt clone` (SPEC §11.6): a bare hub plus a wt-owned canonical checkout,
//! detached at the default branch, which no tree ever checks out.

use wt_core::model::{Label, Owner};
use wt_core::report::CloneData;
use wt_core::{CoreError, ExitClass};

use crate::cli::CloneRepo;

use super::{register, Context, Output};

pub(crate) fn run(context: &mut Context, args: CloneRepo) -> Result<Output, CoreError> {
    let stem = args
        .url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git")
        .to_owned();
    let label = Label::new(args.label.clone().unwrap_or(stem))?;
    // Everything below runs git inside the hub, so paths the caller gave
    // relative to their own directory are made absolute here, once: the
    // hub's parent, and a repository named by a filesystem path.
    let base = match args.path.clone() {
        Some(path) if path.is_relative() => context.cwd.join(path),
        Some(path) => path,
        None => context.home.join("repos").join(label.as_str()),
    };
    let url = if is_relative_local_path(&args.url) {
        context.cwd.join(&args.url).to_string_lossy().into_owned()
    } else {
        args.url.clone()
    };
    let hub = base.join("hub.git");
    let canonical = base.join("canonical");
    let hub_exists = matches!(
        wt_sys::fsx::path_kind(&hub)?,
        wt_sys::fsx::PathKind::Directory
    );
    let cloned = !hub_exists;
    if cloned {
        if !matches!(
            wt_sys::fsx::path_kind(&canonical)?,
            wt_sys::fsx::PathKind::Missing
        ) {
            return Err(CoreError::new(
                ExitClass::State,
                "PATH_OCCUPIED",
                format!(
                    "{} exists but {} does not",
                    canonical.display(),
                    hub.display()
                ),
                "move or delete it, or choose another --path",
            ));
        }
        wt_sys::fsx::create_private_dir(&base)?;
        let deadlines = wt_sys::git::Deadlines::from_settings(&context.settings.git.timeouts)?;
        let default = wt_sys::git::clone_hub(
            "git",
            &url,
            &hub,
            deadlines.for_class(wt_sys::git::Class::Clone),
        )?;
        // The canonical: a worktree of the hub, detached at the default
        // branch's tip, so the branch itself stays a ref wt can move.
        context.git(&hub)?.worktree_add(
            &canonical,
            &wt_sys::git::AddSpec::Detached { start: default },
        )?;
    } else if !matches!(
        wt_sys::fsx::path_kind(&canonical)?,
        wt_sys::fsx::PathKind::Directory
    ) {
        return Err(CoreError::new(
            ExitClass::State,
            "PATH_OCCUPIED",
            format!(
                "{} exists but its canonical checkout {} does not",
                hub.display(),
                canonical.display()
            ),
            "delete the hub and clone again, or choose another --path",
        ));
    }
    let registered = register::perform(
        context,
        canonical,
        Some(label.to_string()),
        None,
        false,
        Owner::Wt,
    )?;
    let register: wt_core::report::RegisterData = serde_json::from_value(registered.data.clone())
        .map_err(|error| {
        CoreError::new(
            ExitClass::Internal,
            "SERIALIZE_FAILED",
            error.to_string(),
            "report this wt bug",
        )
    })?;
    Ok(Output::data(CloneData {
        url: args.url,
        cloned,
        hub: wt_sys::fsx::canonicalize(&hub)
            .unwrap_or(hub)
            .to_string_lossy()
            .into_owned(),
        register,
    })?
    .with_notices(registered.notices))
}

/// A repository named by a relative filesystem path — `./source`,
/// `../source`, `repos/source.git` — rather than a URL or an scp-style
/// `host:path`. Git would otherwise resolve it relative to the hub.
fn is_relative_local_path(url: &str) -> bool {
    !url.contains("://") && !url.contains(':') && std::path::Path::new(url).is_relative()
}
