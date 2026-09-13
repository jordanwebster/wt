use wt_core::report::StatusData;
use wt_core::CoreError;

use crate::cli::Status;

use super::{list, Context, Output};

pub(crate) fn run(context: &mut Context, args: Status) -> Result<Output, CoreError> {
    let target = context.resolve(args.target.as_deref())?;
    let tree = context.tree(&target)?;
    let tree = list::tree_report(context, &tree, false, false, args.probe)?;
    let source_tree = context.tree(&target)?;
    let config_errors = if tree.phase == "ready" {
        match context.load_config(&source_tree) {
            Ok(_) => Vec::new(),
            Err(error) if error.code.0 == "CONFIG_INVALID" => {
                vec![config_error(&error.message)]
            }
            Err(error) => return Err(error),
        }
    } else {
        Vec::new()
    };
    Output::data(StatusData {
        tree,
        config_errors,
    })
}

fn config_error(message: &str) -> wt_core::report::ConfigErrorReport {
    let mut parts = message.splitn(4, ':');
    let path = parts.next().unwrap_or("<config>").to_owned();
    let line = parts
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let col = parts
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let message = parts.next().unwrap_or(message).trim().to_owned();
    wt_core::report::ConfigErrorReport {
        path,
        line,
        col,
        message,
    }
}
