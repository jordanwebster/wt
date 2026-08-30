use wt_core::config::TiedTo;
use wt_core::report::{TaskInfoReport, TasksData};
use wt_core::CoreError;

use crate::cli::Tasks;

use super::{Context, Output};

pub(crate) fn run(context: &mut Context, args: Tasks) -> Result<Output, CoreError> {
    let target = context.resolve(args.target.as_deref())?;
    let tree = context.tree(&target)?;
    let config = context.load_config(&tree)?;
    let mut tasks = context
        .task_catalog(&tree, &config)?
        .into_values()
        .filter(|node| args.private || !node.id.starts_with('@'))
        .map(|node| TaskInfoReport {
            id: node.id,
            scope: node.scope.to_string(),
            origin: format!("{:?}", node.origin).to_ascii_lowercase(),
            cwd: node.cwd.to_string(),
            needs: node.needs,
            resource: node.destroy.is_some(),
            tied_to: node.tied_to.map(|value| match value {
                TiedTo::Tree => "tree".to_owned(),
                TiedTo::Repo => "repo".to_owned(),
                TiedTo::Machine => "machine".to_owned(),
            }),
            lock: node.lock,
            description: None,
        })
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| (&left.scope, &left.id).cmp(&(&right.scope, &right.id)));
    Output::data(TasksData {
        target: target.to_string(),
        tasks,
    })
}
