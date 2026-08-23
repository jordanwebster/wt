use wt_core::report::PathData;
use wt_core::CoreError;

use crate::cli::TargetArg;

use super::{Context, Output};

pub(crate) fn run(context: &mut Context, args: TargetArg) -> Result<Output, CoreError> {
    let target = context.resolve(args.target.as_deref())?;
    let tree = context.tree(&target)?;
    Output::text(
        PathData {
            target: target.to_string(),
            path: tree.path.as_str().to_owned(),
        },
        tree.path.as_str(),
    )
}
