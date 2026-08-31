use std::collections::BTreeMap;

use wt_core::report::MetaData;
use wt_core::{CoreError, ExitClass};

use crate::cli::Meta;

use super::{Context, Output};

enum Edit {
    Set(String, String),
    Unset(String),
}

pub(crate) fn parse_creation(values: &[String]) -> Result<BTreeMap<String, String>, CoreError> {
    let mut meta = BTreeMap::new();
    for value in values {
        let (key, value) = assignment(value)?;
        wt_core::model::validate_meta(key, value)?;
        meta.insert(key.to_owned(), value.to_owned());
    }
    Ok(meta)
}

pub(crate) fn run(context: &mut Context, args: Meta) -> Result<Output, CoreError> {
    let target = context.resolve(Some(&args.target))?;
    let edits = args
        .edits
        .iter()
        .map(|value| {
            let (key, value) = assignment(value)?;
            wt_core::model::validate_meta(key, value)?;
            Ok(if value.is_empty() {
                Edit::Unset(key.to_owned())
            } else {
                Edit::Set(key.to_owned(), value.to_owned())
            })
        })
        .collect::<Result<Vec<_>, CoreError>>()?;
    let meta = if edits.is_empty() {
        context.tree(&target)?.meta
    } else {
        let holder = context.holder(target.to_string(), "meta")?;
        context.mutate_registry(&holder, |registry| {
            let tree = registry
                .trees
                .iter_mut()
                .find(|tree| tree.label == target.label && tree.name == target.name)
                .ok_or_else(|| super::context::not_found(&target))?;
            for edit in edits {
                match edit {
                    Edit::Set(key, value) => {
                        tree.meta.insert(key, value);
                    }
                    Edit::Unset(key) => {
                        tree.meta.remove(&key);
                    }
                }
            }
            Ok(tree.meta.clone())
        })?
    };
    let text = meta
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");
    Output::text(
        MetaData {
            target: target.to_string(),
            meta,
        },
        text,
    )
}

fn assignment(value: &str) -> Result<(&str, &str), CoreError> {
    value.split_once('=').ok_or_else(|| {
        CoreError::new(
            ExitClass::Usage,
            "META_INVALID",
            format!("metadata assignment `{value}` is missing `=`"),
            "use k=v to set metadata or k= to unset it",
        )
    })
}
