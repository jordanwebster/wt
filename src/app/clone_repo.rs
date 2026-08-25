use wt_core::report::CloneData;
use wt_core::CoreError;

use crate::cli::CloneRepo;

use super::{register, Context, Output};

pub(crate) fn run(context: &mut Context, args: CloneRepo) -> Result<Output, CoreError> {
    let stem = args
        .url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git");
    let path = args.path.clone().unwrap_or_else(|| context.cwd.join(stem));
    let cloned = !matches!(
        wt_sys::fsx::path_kind(&path)?,
        wt_sys::fsx::PathKind::Directory
    );
    if cloned {
        let deadlines = wt_sys::git::Deadlines::from_settings(&context.settings.git.timeouts)?;
        wt_sys::git::clone(
            "git",
            &args.url,
            &path,
            deadlines.for_class(wt_sys::git::Class::Clone),
        )?;
    }
    let registered = register::perform(context, path, args.label, None, false, true)?;
    let register: wt_core::report::RegisterData = serde_json::from_value(registered.data.clone())
        .map_err(|error| {
        wt_core::CoreError::new(
            wt_core::ExitClass::Internal,
            "SERIALIZE_FAILED",
            error.to_string(),
            "report this wt bug",
        )
    })?;
    Output::data(CloneData {
        url: args.url,
        cloned,
        register,
    })
}
