use std::path::{Path, PathBuf};

use wt_core::report::WhichData;
use wt_core::CoreError;

use crate::cli::Which;

use super::{door, Context, Output};

pub(crate) fn run(context: &mut Context, args: Which) -> Result<Output, CoreError> {
    let (target, cmd) = match args.values.as_slice() {
        [cmd] => (None, cmd.clone()),
        [target, cmd] => (Some(target.as_str()), cmd.clone()),
        _ => unreachable!("clap bounds which to one or two values"),
    };
    let door = door::enter(context, target, "which")?;
    let notices = door.notices.clone();
    let path = door
        .env
        .env
        .get("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(path))
        .map(|dir| dir.join(&cmd))
        .find(|path| wt_sys::fsx::is_executable_file(path).unwrap_or(false));
    let in_bin = path.as_ref().is_some_and(|path| {
        door.config
            .root
            .bin
            .iter()
            .any(|bin| path.starts_with(Path::new(door.tree.path.as_str()).join(bin.as_str())))
    });
    Ok(Output::data(WhichData {
        target: door.target.to_string(),
        cmd,
        path: path.map(|path: PathBuf| path.to_string_lossy().into_owned()),
        in_bin,
    })?
    .with_notices(notices))
}
