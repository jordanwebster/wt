use wt_core::report::{ConfigData, ConfigEntryReport};
use wt_core::CoreError;

use crate::cli::Config;

use super::{Context, Output};

pub(crate) fn run(context: &mut Context, args: Config) -> Result<Output, CoreError> {
    let target = context.resolve(args.target.as_deref())?;
    let tree = context.tree(&target)?;
    let config = context.load_config(&tree)?;
    let value = serde_json::to_value(config).map_err(|error| {
        CoreError::new(
            wt_core::ExitClass::Internal,
            "SERIALIZE_FAILED",
            error.to_string(),
            "report this wt bug",
        )
    })?;
    Output::data(ConfigData {
        target: target.to_string(),
        entries: vec![ConfigEntryReport {
            key: "effective".to_owned(),
            scope: ".".to_owned(),
            layer: if args.origin { "merged" } else { "effective" }.to_owned(),
            value,
        }],
    })
}
