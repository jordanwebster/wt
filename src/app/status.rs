use wt_core::report::StatusData;
use wt_core::CoreError;

use crate::cli::Status;

use super::{list, Context, Output};

pub(crate) fn run(context: &mut Context, args: Status) -> Result<Output, CoreError> {
    let target = context.resolve(args.target.as_deref())?;
    let tree = context.tree(&target)?;
    let tree = list::tree_report(context, &tree, false, false, args.probe)?;
    let source_tree = context.tree(&target)?;
    let (config, config_errors) = if tree.phase == "ready" {
        match context.load_config(&source_tree) {
            Ok(config) => (Some(config), Vec::new()),
            Err(error) if error.code.0 == "CONFIG_INVALID" => {
                (None, vec![config_error(&error.message)])
            }
            Err(error) => return Err(error),
        }
    } else {
        (None, Vec::new())
    };
    let tasks = if let Some(config) = config.as_ref().filter(|_| tree.phase == "ready") {
        context
            .task_catalog(&source_tree, config)?
            .into_values()
            .filter(|node| !node.id.starts_with('@'))
            .map(|node| wt_core::report::TaskInfoReport {
                id: node.id,
                scope: node.scope.to_string(),
                origin: format!("{:?}", node.origin).to_ascii_lowercase(),
                cwd: node.cwd.to_string(),
                needs: node.needs,
                resource: node.destroy.is_some(),
                tied_to: node
                    .tied_to
                    .map(|value| format!("{value:?}").to_ascii_lowercase()),
                lock: node.lock,
                description: node.description,
            })
            .collect()
    } else {
        Vec::new()
    };
    Output::data(StatusData {
        tree,
        tasks,
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
